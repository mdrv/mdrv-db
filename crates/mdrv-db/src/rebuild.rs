//! Offline port rebuild (owner must be stopped): quarantine a damaged or
//! missing live/app.db, copy the snapshot from a recovery backup over it,
//! then open the engine so recovery replays the committed WAL tail above
//! the backup's watermark. The journal (fjall) is never replaced — unlike
//! `restore`, which resets the whole live/ tree from the backup, rebuild
//! preserves every WAL entry written after the backup.

use serde_json::json;
use std::path::{Path, PathBuf};

use mdrv_db::{DataPort, Engine, EngineConfig, TursoPort};

/// Explicit --backup dir, else the newest manifest-bearing dir under
/// <data_dir>/recovery (backup dir names sort by their ms timestamp).
fn pick_backup(data_dir: &Path, backup: Option<&Path>) -> Result<PathBuf, String> {
    match backup {
        Some(b) => Ok(b.to_path_buf()),
        None => {
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(data_dir.join("recovery"))
                .map_err(|e| format!("cannot list recovery/: {e}"))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.join("manifest.json").is_file())
                .collect();
            dirs.sort();
            dirs.pop().ok_or_else(|| {
                "no backups under recovery/ — pass --backup or run `mdrv-db backup` first"
                    .to_string()
            })
        }
    }
}

pub fn run(data_dir: &Path, backup: Option<&Path>) -> Result<serde_json::Value, String> {
    let live = data_dir.join("live");
    if !live.is_dir() {
        return Err(format!("{live:?} is not an mdrv database (no live/)"));
    }
    let backup = pick_backup(data_dir, backup)?;
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(backup.join("manifest.json"))
            .map_err(|e| format!("cannot read manifest: {e}"))?,
    )
    .map_err(|e| format!("bad manifest.json: {e}"))?;
    let name = manifest
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("manifest missing \"name\"")?
        .to_string();

    // quarantine the damaged port, then copy the backup snapshot in
    let app = live.join("app.db");
    if app.exists() {
        let q = live.join("quarantine");
        std::fs::create_dir_all(&q).map_err(|e| format!("cannot create quarantine/: {e}"))?;
        let dest = q.join(format!("app.db.{}", mdrv_db::now_ms()));
        std::fs::rename(&app, &dest).map_err(|e| format!("cannot quarantine app.db: {e}"))?;
        println!("quarantined damaged port → {}", dest.display());
    }
    std::fs::copy(backup.join("app.db"), &app)
        .map_err(|e| format!("cannot copy backup snapshot: {e}"))?;

    // open: recovery replays committed WAL above the backup watermark
    let port: Box<dyn DataPort> =
        Box::new(TursoPort::open(app.clone()).map_err(|e| format!("port open: {e}"))?);
    let mut eng = Engine::open(&live, &name, port, EngineConfig::default())
        .map_err(|e| format!("engine open (recovery replay): {e}"))?;
    let applied = eng
        .status()
        .get("applied_lsn")
        .cloned()
        .unwrap_or(json!(null));
    let v = mdrv_db::verify::run(&eng).map_err(|e| format!("verify after rebuild: {e}"))?;
    eng.close().map_err(|e| format!("close: {e}"))?;
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    Ok(json!({
        "ok": ok,
        "backup": backup.display().to_string(),
        "applied_lsn": applied,
        "verify": v,
    }))
}
