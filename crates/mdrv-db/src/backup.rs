//! Backup = checkpoint cycle: persist → VACUUM INTO → copy blobs → copy
//! fjall dir → blake3 manifest. A backup is only *valid* if all three
//! artifacts + manifest landed; checkpoint/prune happens separately after.

use crate::blob::{hex, BlobStore};
use crate::engine::Engine;
use crate::MdrvDbError;
use fjall::PersistMode;
use serde_json::json;
use std::fs;
use std::io;
use std::path::Path;

pub struct BackupOutcome {
    pub manifest: serde_json::Value,
}

pub fn run(engine: &Engine, dest: impl AsRef<Path>) -> Result<BackupOutcome, MdrvDbError> {
    let dest = dest.as_ref();
    fs::create_dir_all(dest)?;

    // Serialize against the write pipeline (in-process concurrent writers).
    // Cross-process writers are already excluded by the engine locks: a
    // daemon-side backup only runs when the owner is stopped.
    let _write = engine
        .write_mu
        .lock()
        .map_err(|_| MdrvDbError::Usage("engine write lock poisoned".into()))?;

    // 1. flush + fsync the envelope
    engine.db.persist(PersistMode::SyncAll)?;

    // 2. capture the ceiling BEFORE the snapshot: everything above it is
    // not in the backup and must stay replay-able in the primary envelope
    let ceiling = engine.applied.load(std::sync::atomic::Ordering::SeqCst);

    // 3. turso snapshot (single quotes doubled — VACUUM INTO takes a literal)
    let sql_path = dest.join("app.db");
    let lit = sql_path.display().to_string().replace('\'', "''");
    engine
        .port
        .exec(&format!("VACUUM INTO '{lit}'"))
        .map_err(MdrvDbError::Port)?;

    // 4. blob tree + fjall dir copies
    copy_dir(engine.root.join("blobs"), dest.join("blobs"))?;
    copy_dir(engine.root.join("fjall"), dest.join("fjall"))?;

    // 5. manifest (streamed hashing; blobs may be far larger than RAM)
    let mut files = Vec::new();
    collect_manifest(dest, dest, &mut files)?;
    let manifest = json!({
        "name": engine.name,
        "applied_lsn": ceiling,
        "created_at": crate::now_ms(),
        "entry_schema": crate::ENTRY_SCHEMA,
        "engine_version": env!("CARGO_PKG_VERSION"),
        "files": files,
    });
    fs::write(dest.join("manifest.json"), manifest.to_string())?;

    // 6. checkpoint to the ceiling — part of the backup cycle so no window
    // exists where a newer write could be pruned before any artifact
    // captured it. Only reached once every artifact + manifest landed.
    let pruned = engine.checkpoint_upto(ceiling, false)?;

    engine.report(
        1,
        "backup.run",
        Some(json!({ "dest": dest.display().to_string(), "ceiling": ceiling, "checkpointed": pruned })),
    )?;
    Ok(BackupOutcome { manifest })
}

fn copy_dir(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> io::Result<()> {
    let src = src.as_ref();
    let dst = dst.as_ref();
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let ft = e.file_type()?;
        if ft.is_dir() {
            copy_dir(e.path(), dst.join(e.file_name()))?;
        } else {
            fs::copy(e.path(), dst.join(e.file_name()))?;
        }
    }
    Ok(())
}

fn collect_manifest(root: &Path, dir: &Path, out: &mut Vec<serde_json::Value>) -> io::Result<()> {
    for e in fs::read_dir(dir)? {
        let e = e?;
        if ft_is_dir(&e)? {
            collect_manifest(root, &e.path(), out)?;
        } else {
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap_or(&e.path())
                .display()
                .to_string();
            let (blake, bytes) = blob_digest(&e.path())?;
            out.push(json!({ "path": rel, "bytes": bytes, "blake3": blake }));
        }
    }
    Ok(())
}

/// Stream a 1 MiB-at-a-time blake3 so manifest cost is I/O-bound, not
/// memory-bound (blobs may be far larger than RAM).
fn blob_digest(path: &Path) -> io::Result<(String, u64)> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((hex(&h.finalize().as_bytes()[..16]), n))
}

fn ft_is_dir(e: &fs::DirEntry) -> io::Result<bool> {
    Ok(e.file_type()?.is_dir())
}

/// Restore is intentionally manual: stop the owning process, replace the
/// data dir, start. Fjall replays its journal tail at open; mdrv recovery
/// replays committed WAL above the restored port watermark.
pub fn blob_hash_of(bytes: &[u8]) -> String {
    BlobStore::hash(bytes)
}
