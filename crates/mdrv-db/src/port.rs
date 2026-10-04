//! DataPort — the seam between the Fjall envelope and the SQL engine.
//!
//! Implementations: Turso (in `mdrv_napi`). The trait is sync; async
//! engines are driven via a private runtime inside the port. All
//! statements inside `exec_tx` run in ONE transaction (all-or-nothing).

use crate::entry::PortValue;

/// SELECT result: column names (from the first row) + row values.
/// Zero rows => empty names (ports cannot know them then).
pub struct QueryOut {
    pub names: Vec<String>,
    pub rows: Vec<Vec<PortValue>>,
}

pub trait DataPort: Send + Sync {
    /// Execute statements as a single transaction. Returns total rows changed.
    fn exec_tx(&self, stmts: &[Stmt]) -> Result<u64, String>;
    /// Autocommit single statement (DDL, VACUUM INTO, PRAGMA...).
    fn exec(&self, sql: &str) -> Result<u64, String>;
    /// SELECT-only read path (enforced by callers; ports may also check).
    fn query(&self, sql: &str, params: &[PortValue]) -> Result<QueryOut, String>;
    /// Engine integrity opinion ("ok" or diagnostics).
    fn integrity_check(&self) -> Result<String, String>;
}

pub struct Stmt {
    pub sql: String,
    pub params: Vec<PortValue>,
}

/// Identifiers arrive from callers and are spliced into SQL text (values
/// are always bound params) — so gate them to [A-Za-z0-9_].
pub fn safe_ident(s: &str) -> Result<&str, String> {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(s)
    } else {
        Err(format!("unsafe identifier: {s:?}"))
    }
}

/// One-line SQL excerpt for error messages: whitespace collapsed, at most
/// 100 chars plus an ellipsis. Parser errors ("incomplete input") carry no
/// offset, so a multi-statement batch is otherwise undiagnosable.
pub(crate) fn sql_head(s: &str) -> String {
    let mut head: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if head.chars().count() > 100 {
        head = head.chars().take(100).collect();
        head.push_str("...");
    }
    head
}
