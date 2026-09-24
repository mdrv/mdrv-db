//! mdrv-db — multi-layer durable recovery vault (v2).
//!
//! Fjall is the outermost layer: every mutation is journaled WAL-first
//! (`wal` + `marks` keyspaces), applied to the data layer through a
//! [`DataPort`] (one transaction per LSN), and marked Committed only
//! after the port confirms. Recovery replays Committed entries above the
//! port's `applied_lsn` watermark. Blobs live in a content-addressed
//! store on disk; reports age out of a Fifo keyspace.

pub mod backup;
pub mod blob;
pub mod engine;
pub mod entry;
pub mod fault;
pub mod port;
#[cfg(feature = "turso")]
pub mod port_turso;
pub mod verify;

pub use blob::BlobStore;
pub use engine::{
    Engine, EngineConfig, ExecuteOutcome, MdrvDbError, MutateRequest, RecoveryReport,
};
pub use entry::{Op, PortValue, SqlKind, TxEntry, ENTRY_SCHEMA};
pub use port::{DataPort, Stmt};
#[cfg(feature = "turso")]
pub use port_turso::TursoPort;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Data dir root for an engine: `<root>/live` when the v2 fleet layout
/// exists, otherwise the root itself (flat layout, v1-compatible).
pub fn live_dir(root: &std::path::Path) -> std::path::PathBuf {
    let live = root.join("live");
    if live.is_dir() {
        live
    } else {
        root.to_path_buf()
    }
}
