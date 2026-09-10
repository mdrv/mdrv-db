//! Test-only crash points (fault injection) for the crash-injection matrix.
//!
//! Set `MDRV_DB_FAULT=<point>` in the environment and the engine will
//! `std::process::abort()` at that exact point of the write pipeline — no
//! unwinding, no destructors, journals left as a power cut would leave them.
//! The parent test process SIGKILLs nothing; the child aborts on its own at
//! a deterministic instruction. Zero cost when the variable is unset.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// journal committed + fsynced, port transaction NOT started
    AfterPersist,
    /// port transaction committed + blobs promoted, mark still Pending
    AfterApply,
}

fn tag() -> Option<&'static str> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| std::env::var("MDRV_DB_FAULT").ok())
        .as_deref()
        .filter(|s| !s.is_empty())
}

/// Abort if `MDRV_DB_FAULT` names this point. Test-only; cheap no-op otherwise.
pub fn abort_if(point: Point) {
    let hit = match (tag(), point) {
        (Some("after_persist"), Point::AfterPersist) => true,
        (Some("after_apply"), Point::AfterApply) => true,
        _ => false,
    };
    if hit {
        std::process::abort();
    }
}
