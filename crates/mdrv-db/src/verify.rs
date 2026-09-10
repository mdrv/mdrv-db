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

    // 4. fence: committed WAL above the port watermark means recovery is
    // pending or was halted (apply lag). The inverse (watermark above the
    // envelope tip) is normal after a full checkpoint prune, so it is
    // reported as a number, not flagged.
    let applied = engine.applied.load(std::sync::atomic::Ordering::SeqCst);
    let mut max_wal = 0u64;
    let mut max_committed_above = 0u64;
    for kv in engine
        .marks
        .range::<[u8; 8], _>(0u64.to_be_bytes()..=u64::MAX.to_be_bytes())
    {
        let (k, v) = kv.into_inner()?;
        let lsn = u64::from_be_bytes(k[..8].try_into().unwrap_or([0; 8]));
        if lsn > max_wal {
            max_wal = lsn;
        }
        if v.first() == Some(&MARK_COMMITTED) && lsn > applied && lsn > max_committed_above {
            max_committed_above = lsn;
        }
    }
    if max_committed_above > 0 {
        anomalies.push(format!(
            "apply lag: committed entries up to lsn {max_committed_above} sit above the port watermark {applied} (recovery pending or halted)"
        ));
    }

    let ok = anomalies.is_empty();
    let report = json!({
        "ok": ok,
        "name": engine.name,
        "wal": { "entries": wal_entries, "checked": checked },
        "blobs": { "files": blob_files, "bytes": blob_bytes },
        "port_integrity": turso,
        "fence": {
            "applied_lsn": applied,
            "max_wal_lsn": max_wal,
            "max_committed_above_watermark": max_committed_above,
        },
        "anomalies": anomalies,
    });
    engine.report(if ok { 1 } else { 3 }, "verify.run", Some(report.clone()))?;
    Ok(report)
}

/// Verify a recovery backup directory (manifest.json + app.db + fjall/ +
/// blobs/): re-hashes every manifest-listed file with blake3 and compares
/// sizes. No engine, no lock — safe next to a running owner.
pub fn run_backup(backup_dir: &std::path::Path) -> Result<serde_json::Value, MdrvError> {
    let mf = backup_dir.join("manifest.json");
    let text = std::fs::read_to_string(&mf)?;
    let manifest: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| MdrvError::Corrupt(format!("manifest.json: {e}")))?;
    let mut anomalies: Vec<String> = Vec::new();
    let mut files = 0u64;
    let mut bytes_total = 0u64;
    if let Some(list) = manifest.get("files").and_then(|f| f.as_array()) {
        for f in list {
            let rel = f.get("path").and_then(|p| p.as_str()).unwrap_or_default();
            let want = f.get("blake3").and_then(|b| b.as_str()).unwrap_or_default();
            let want_bytes = f.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0);
            let p = backup_dir.join(rel);
            match std::fs::read(&p) {
                Ok(bytes) => {
                    files += 1;
                    bytes_total += bytes.len() as u64;
                    let got = crate::blob::hex(&blake3::hash(&bytes).as_bytes()[..16]);
                    if got != want {
                        anomalies.push(format!("{rel}: checksum mismatch"));
                    }
                    if bytes.len() as u64 != want_bytes {
                        anomalies.push(format!("{rel}: size mismatch"));
                    }
                }
                Err(e) => anomalies.push(format!("{rel}: unreadable ({e})")),
            }
        }
    } else {
        anomalies.push("manifest has no files[]".into());
    }
    let ok = anomalies.is_empty();
    Ok(json!({
        "ok": ok,
        "name": manifest.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "applied_lsn": manifest.get("applied_lsn").cloned().unwrap_or(serde_json::Value::Null),
        "files": { "checked": files, "bytes": bytes_total },
        "anomalies": anomalies,
    }))
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
