//! Backup = checkpoint cycle: persist → VACUUM INTO → copy blobs → copy
//! fjall dir → blake3 manifest. A backup is only *valid* if all three
//! artifacts + manifest landed; checkpoint/prune happens separately after.

use crate::blob::{hex, BlobStore};
use crate::engine::Engine;
use crate::MdrvError;
use fjall::PersistMode;
use serde_json::json;
use std::fs;
use std::io;
use std::path::Path;

pub struct BackupOutcome {
    pub manifest: serde_json::Value,
}

pub fn run(engine: &Engine, dest: impl AsRef<Path>) -> Result<BackupOutcome, MdrvError> {
    let dest = dest.as_ref();
    fs::create_dir_all(dest)?;

    // 1. flush + fsync the envelope
    engine.db.persist(PersistMode::SyncAll)?;

    // 2. turso snapshot (single quotes doubled — VACUUM INTO takes a literal)
    let sql_path = dest.join("app.db");
    let lit = sql_path.display().to_string().replace('\'', "''");
    engine
        .port
        .exec(&format!("VACUUM INTO '{lit}'"))
        .map_err(MdrvError::Port)?;

    // 3. blob tree + fjall dir copies
    copy_dir(engine.root.join("blobs"), dest.join("blobs"))?;
    copy_dir(engine.root.join("fjall"), dest.join("fjall"))?;

    // 4. manifest
    let mut files = Vec::new();
    collect_manifest(dest, dest, &mut files)?;
    let manifest = json!({
        "name": engine.name,
        "applied_lsn": engine.applied.load(std::sync::atomic::Ordering::SeqCst),
        "created_at": crate::now_ms(),
        "entry_schema": crate::ENTRY_SCHEMA,
        "files": files,
    });
    fs::write(dest.join("manifest.json"), manifest.to_string())?;
    engine.report(
        1,
        "backup.run",
        Some(json!({ "dest": dest.display().to_string() })),
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
            let bytes = fs::read(e.path())?;
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap_or(&e.path())
                .display()
                .to_string();
            out.push(json!({ "path": rel, "bytes": bytes.len(), "blake3": hex(&blake3::hash(&bytes).as_bytes()[..16]) }));
        }
    }
    Ok(())
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
