//! Journal entry formats. Postcard-encoded, schema-versioned (`v` first).
//! Replayer rule: code must read v1 forever; the WAL is never rewritten.

use serde::{Deserialize, Serialize};

pub const ENTRY_SCHEMA: u16 = 1;

/// Marks are single bytes + a blake3 prefix of the entry bytes (integrity).
pub const MARK_PENDING: u8 = 1;
pub const MARK_COMMITTED: u8 = 2;
pub const MARK_ABORTED: u8 = 3;

pub const MARK_LEN: usize = 9; // 1 status byte + 8-byte blake3 prefix

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TxEntry {
    pub v: u16,
    pub lsn: u64,
    pub issued_at: i64,
    pub actor: String,
    pub ops: Vec<Op>,
}

/// Row-based CDC only — never raw SQL. Deterministic replay: every
/// non-deterministic value (ids, timestamps) was computed once by the
/// caller and is replayed as data.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Op {
    Sql {
        kind: SqlKind,
        table: String,
        pk_col: String,
        columns: Vec<String>,
        values: Vec<PortValue>,
        pk: PortValue,
    },
    /// Blob was staged (fsynced) before this entry was journaled; promote
    /// (atomic rename) happens after the SQL transaction commits.
    BlobPut {
        hash_hex: String,
    },
    BlobDrop {
        hash_hex: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlKind {
    Insert,
    Upsert,
    Delete,
    /// Partial-row update: `UPDATE t SET cols WHERE pk = ?` — replay-
    /// idempotent (re-applying the same SET is a no-op), and unlike
    /// Upsert it cannot wipe columns that are not mentioned.
    Update,
}

/// Portability layer between the envelope and the SQL engine (Turso).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PortValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    /// Placeholder resolved at apply time to the entry's LSN — lets
    /// callers use the LSN as a deterministic, replay-safe row id.
    Lsn,
}

impl PortValue {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            PortValue::Null => serde_json::Value::Null,
            PortValue::Int(i) => (*i).into(),
            PortValue::Real(f) => (*f).into(),
            PortValue::Text(s) => s.clone().into(),
            PortValue::Blob(b) => {
                use base64_encode as _b64;
                serde_json::Value::String(_b64(b))
            }
            PortValue::Lsn => serde_json::Value::Null,
        }
    }
}

// tiny inline base64 (standard alphabet) — avoids pulling a base64 crate
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReportEntry {
    pub v: u16,
    pub ts_ms: i64,
    pub level: u8, // 0=debug 1=info 2=warn 3=error
    pub event: String,
    pub data_json: String,
}
