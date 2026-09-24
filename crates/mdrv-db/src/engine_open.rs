//! CLI engine-open helper. The TursoPort implementation itself lives in
//! the lib behind the optional `turso` feature (the bin target requires
//! that feature); only the CLI-specific open-with-hints wrapper is here.

use std::path::Path;

use mdrv_db::{live_dir, Engine, EngineConfig, TursoPort};

/// Open the engine for CLI ops. Fails with a hint when the fjall lock is
/// held (owner running) or the dir is not an mdrv database.
pub fn open_engine(
    root: &Path,
    name: &str,
    fsync_each_write: bool,
    allow_create: bool,
) -> Result<Engine, String> {
    let live = live_dir(root);
    let fjall_dir = live.join("fjall");
    if !fjall_dir.is_dir() {
        if allow_create {
            std::fs::create_dir_all(&fjall_dir)
                .map_err(|e| format!("cannot create {fjall_dir:?}: {e}"))?;
        } else {
            return Err(format!(
                "{:?} does not exist — not an mdrv database (or never initialized)",
                fjall_dir
            ));
        }
    }
    let port = Box::new(TursoPort::open(live.join("app.db"))?);
    Engine::open(&live, name, port, EngineConfig { fsync_each_write })
        .map_err(|e| format!("engine open failed: {e} (owner running? wrong --name?)"))
}
