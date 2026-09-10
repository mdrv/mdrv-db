//! Integrity sweep: envelope hashes (wal vs marks), blob tree re-hash,
//! port integrity_check. Runs after any restore, on a schedule, on demand.

use crate::engine::Engine;
use crate::entry::{TxEntry, MARK_ABORTED, MARK_COMMITTED, MARK_PENDING};
use crate::MdrvError;
use serde_json::json;

pub fn run(engine: &Engine) -> Result<serde_json::Value, MdrvError> {
    let mut anomalies: Vec<String> = Vec::new();
    let mut wal_entries = 0u64;
    let mut checked = 0u64;

    // 1. envelope: every wal entry's blake3 must match its mark
    for kv in engine
        .wal
        .range::<[u8; 8], _>(0u64.to_be_bytes()..=u64::MAX.to_be_bytes())
    {
        let (k, v) = kv.into_inner()?;
        let lsn = u64::from_be_bytes(k[..8].try_into().unwrap_or([0; 8]));
        wal_entries += 1;
        let mark = engine.marks.get(&k)?;
        let Some(mark) = mark else {
            anomalies.push(format!("lsn {lsn}: missing mark"));
            continue;
        };
        if mark.len() != 9 {
            anomalies.push(format!("lsn {lsn}: mark malformed ({} bytes)", mark.len()));
            continue;
        }
        let h = blake3::hash(&v);
        if mark[1..] != h.as_bytes()[..8] {
            anomalies.push(format!("lsn {lsn}: entry hash mismatch (tamper/rot)"));
            continue;
        }
        // decode sanity for committed entries
        if mark[0] == MARK_COMMITTED {
            if postcard::from_bytes::<TxEntry>(&v).is_err() {
                anomalies.push(format!("lsn {lsn}: committed entry undecodable"));
            }
        }
        checked += 1;
    }

    // 2. blobs: sha256(file) == filename; orphans reported, not deleted
    let mut blob_files = 0u64;
    let mut blob_bytes = 0u64;
    for (hash_hex, path) in engine.blobs.walk_final()? {
        let bytes = std::fs::read(&path)?;
        blob_files += 1;
        blob_bytes += bytes.len() as u64;
        if crate::blob::BlobStore::hash(&bytes) != hash_hex {
            anomalies.push(format!("blob {hash_hex}: content hash mismatch"));
        }
    }

    // 3. port opinion
    let turso = match engine.port.integrity_check() {
        Ok(s) if s.eq_ignore_ascii_case("ok") => "ok".to_string(),
        Ok(s) => {
            anomalies.push(format!("port integrity: {s}"));
            s
        }
        Err(e) => {
            anomalies.push(format!("port integrity error: {e}"));
            e
        }
    };

    let ok = anomalies.is_empty();
    let report = json!({
        "ok": ok,
        "name": engine.name,
        "wal": { "entries": wal_entries, "checked": checked },
        "blobs": { "files": blob_files, "bytes": blob_bytes },
        "port_integrity": turso,
        "anomalies": anomalies,
    });
    engine.report(if ok { 1 } else { 3 }, "verify.run", Some(report.clone()))?;
    Ok(report)
}

/// Pending entries at or below the port watermark rolled forward (crash
/// between apply and mark) — they are fine; above it they are pending
/// work that recovery skipped (also fine). Anything else is an anomaly.
pub fn pending_state(engine: &Engine, lsn: u64) -> Result<&'static str, MdrvError> {
    let m = engine.marks.get(lsn.to_be_bytes())?;
    let applied = engine.applied.load(std::sync::atomic::Ordering::SeqCst);
    Ok(match m.as_ref().and_then(|x| x.first()) {
        Some(&MARK_PENDING) if lsn <= applied => "rolled-forward",
        Some(&MARK_PENDING) => "pending",
        Some(&MARK_COMMITTED) => "committed",
        Some(&MARK_ABORTED) => "aborted",
        _ => "unknown",
    })
}
