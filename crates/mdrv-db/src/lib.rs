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
pub mod verify;

pub use blob::BlobStore;
pub use engine::{Engine, EngineConfig, ExecuteOutcome, MdrvError, MutateRequest, RecoveryReport};
pub use entry::{Op, PortValue, SqlKind, TxEntry, ENTRY_SCHEMA};
pub use port::{DataPort, Stmt};

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
