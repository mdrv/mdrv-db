//! The engine: WAL-first write path, status lifecycle, recovery-on-open,
//! checkpoint/prune. One engine owns one data dir (fjall + blobs + port).

use crate::blob::{BlobStore, BlobUpload};
use crate::entry::{
    Op, PortValue, ReportEntry, SqlKind, TxEntry, ENTRY_SCHEMA, MARK_ABORTED, MARK_COMMITTED,
    MARK_LEN, MARK_PENDING,
};
use crate::now_ms;
use crate::port::{safe_ident, sql_head, DataPort, Stmt};
use fjall::compaction::Fifo;
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MdrvDbError {
    #[error("fjall: {0}")]
    Fjall(#[from] fjall::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("codec: {0}")]
    Codec(#[from] postcard::Error),
    #[error("data port: {0}")]
    Port(String),
    #[error("usage: {0}")]
    Usage(String),
    #[error("corrupt: {0}")]
    Corrupt(String),
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EngineConfig {
    /// fsync the journal between intent and apply (WAL-first, full strength).
    /// false = group-commit style durability (OS buffers; still crash-safe,
    /// not power-loss-safe). Configurable per DB — default true.
    pub fsync_each_write: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            fsync_each_write: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MutateRequest {
    pub actor: String,
    pub ops: Vec<Op>,
    #[serde(default)]
    pub idem_key: Option<String>,
    /// Optional response digest cached for idempotent replays.
    #[serde(default)]
    pub response: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecuteOutcome {
    pub lsn: u64,
    pub rows_changed: u64,
    pub replayed_from_cache: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryReport {
    pub applied_before: u64,
    pub replayed: u64,
    pub skipped_pending: u64,
    pub skipped_aborted: u64,
    pub wall_ms: i64,
}

pub struct Engine {
    pub(crate) db: Database,
    pub(crate) wal: Keyspace,
    pub(crate) marks: Keyspace,
    pub(crate) idem: Keyspace,
    pub(crate) report: Keyspace,
    pub(crate) meta: Keyspace,
    pub(crate) port: Box<dyn DataPort>,
    pub blobs: BlobStore,
    pub(crate) root: PathBuf,
    pub(crate) name: String,
    pub(crate) cfg: EngineConfig,
    pub(crate) applied: AtomicU64,
    pub(crate) report_seq: AtomicU64,
    idem_sweeps: AtomicU64,
    pub(crate) open: bool,
}

const META_APPLIED: &str = "applied_lsn"; // mirror of port watermark (info only)
const META_CHECKPOINT: &str = "checkpoint_lsn";
const META_NAME: &str = "db_name";
// idempotency-cache records older than this are pruneable garbage (weak delete)
const IDEM_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const IDEM_SWEEP_MASK: u64 = 255; // every 256 executes
const IDEM_SWEEP_BUDGET: usize = 256;

impl Engine {
    /// Open (or create) a data dir, then run recovery-on-open.
    /// `name` guards against path mix-ups (meta.db_name must match).
    pub fn open(
        root: impl AsRef<Path>,
        name: &str,
        port: Box<dyn DataPort>,
        cfg: EngineConfig,
    ) -> Result<Engine, MdrvDbError> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let fjall_dir = root.join("fjall");

        let db = Database::builder(&fjall_dir).open()?;
        let wal = db.keyspace("wal", KeyspaceCreateOptions::default)?;
        let marks = db.keyspace("marks", KeyspaceCreateOptions::default)?;
        let idem = db.keyspace("idem", KeyspaceCreateOptions::default)?;
        let report = db.keyspace("report", || {
            KeyspaceCreateOptions::default()
                .compaction_strategy(std::sync::Arc::new(Fifo::new(64_000_000, Some(2_592_000))))
        })?;
        let meta = db.keyspace("meta", KeyspaceCreateOptions::default)?;

        // name guard
        match meta.get(META_NAME)? {
            Some(prev) => {
                let s = String::from_utf8_lossy(&prev).into_owned();
                if s != name {
                    return Err(MdrvDbError::Usage(format!(
                        "data dir belongs to {s:?}, refusing to open as {name:?}"
                    )));
                }
            }
            None => {
                let mut b = db.batch();
                b.insert(&meta, META_NAME, name);
                b.insert(&meta, "entry_schema", ENTRY_SCHEMA.to_string());
                b.commit()?;
            }
        }

        // port housekeeping: fence table + watermark
        port.exec("CREATE TABLE IF NOT EXISTS _mdrv (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .map_err(MdrvDbError::Port)?;
        port.exec("INSERT OR IGNORE INTO _mdrv (key, value) VALUES ('applied_lsn', '0')")
            .map_err(MdrvDbError::Port)?;
        let applied = read_applied(port.as_ref())?;

        let blobs = BlobStore::open(root.join("blobs"))?;

        let engine = Engine {
            db,
            wal,
            marks,
            idem,
            report,
            meta,
            port,
            blobs,
            root,
            name: name.to_string(),
            cfg,
            applied: AtomicU64::new(applied),
            report_seq: AtomicU64::new(0),
            idem_sweeps: AtomicU64::new(0),
            open: true,
        };

        engine.blobs.sweep_staging();
        let rec = engine.recover()?;
        engine.report(
            1,
            "engine.open",
            Some(json!({ "name": engine.name, "recovery": rec })),
        )?;
        Ok(engine)
    }

    // -- write path ----------------------------------------------------------

    /// WAL-first mutation: journal intent (+fsync) → apply (one tx) → mark.
    pub fn execute(&self, req: MutateRequest) -> Result<ExecuteOutcome, MdrvDbError> {
        if req.ops.is_empty() {
            return Err(MdrvDbError::Usage("empty ops".into()));
        }

        // idempotency replay
        if let Some(key) = &req.idem_key {
            if let Some(v) = self.idem.get(key.as_bytes())? {
                if v.first() == Some(&2u8) {
                    // Completed: {2, ts_ms be u64, response str}
                    let resp = String::from_utf8_lossy(&v[17..]).into_owned();
                    return Ok(ExecuteOutcome {
                        lsn: u64::from_be_bytes(v[1..9].try_into().unwrap_or([0; 8])),
                        rows_changed: 0,
                        replayed_from_cache: true,
                        response: Some(resp),
                    });
                }
            }
        }

        // stage blobs (fsync) BEFORE journaling the intent
        for op in &req.ops {
            if let Op::BlobPut { hash_hex } = op {
                // bytes were staged via put_blob(); nothing to do here —
                // the staging file IS the durable pre-commit copy.
                if !self.blobs.staging_path(hash_hex).is_file()
                    && !self.blobs.final_path(hash_hex).is_file()
                {
                    return Err(MdrvDbError::Usage(format!(
                        "blob {hash_hex} was never staged"
                    )));
                }
            }
        }

        let lsn = self.next_lsn();
        let entry = TxEntry {
            v: ENTRY_SCHEMA,
            lsn,
            issued_at: now_ms(),
            actor: req.actor,
            ops: req.ops,
        };
        let entry_bytes = postcard::to_allocvec(&entry)?;
        let hash8 = blake3::hash(&entry_bytes);

        // 1. journal intent (atomic across keyspaces)
        let mut b = self.db.batch();
        b.insert(&self.wal, lsn.to_be_bytes(), entry_bytes.as_slice());
        let mut mark_pending = Vec::with_capacity(MARK_LEN);
        mark_pending.push(MARK_PENDING);
        mark_pending.extend_from_slice(&hash8.as_bytes()[..8]);
        b.insert(&self.marks, lsn.to_be_bytes(), mark_pending.as_slice());
        if let Some(key) = &req.idem_key {
            let mut rec = vec![1u8]; // Processing
            rec.extend_from_slice(&lsn.to_be_bytes());
            rec.extend_from_slice(&now_ms().to_be_bytes());
            b.insert(&self.idem, key.as_bytes(), rec);
        }
        b.commit()?;

        // 2. make the intent power-loss durable BEFORE touching data
        if self.cfg.fsync_each_write {
            self.db.persist(PersistMode::SyncData)?;
        }
        crate::fault::abort_if(crate::fault::Point::AfterPersist);

        // 3. apply (single port transaction; watermark bumps inside it)
        match self.apply(&entry) {
            Ok(rows) => {
                // 4. promote blobs (post-commit; missing files fail verify)
                for op in &entry.ops {
                    if let Op::BlobPut { hash_hex } = op {
                        self.blobs.promote(hash_hex)?;
                    }
                }
                crate::fault::abort_if(crate::fault::Point::AfterApply);
                // 5. mark committed + complete idem
                let mut b = self.db.batch();
                let mut mark_committed = Vec::with_capacity(MARK_LEN);
                mark_committed.push(MARK_COMMITTED);
                mark_committed.extend_from_slice(&hash8.as_bytes()[..8]);
                b.insert(&self.marks, lsn.to_be_bytes(), mark_committed.as_slice());
                if let Some(key) = &req.idem_key {
                    let mut rec = vec![2u8];
                    rec.extend_from_slice(&lsn.to_be_bytes());
                    rec.extend_from_slice(&now_ms().to_be_bytes());
                    rec.extend_from_slice(req.response.unwrap_or_default().as_bytes());
                    b.insert(&self.idem, key.as_bytes(), rec);
                }
                b.commit()?;
                self.applied.store(lsn, Ordering::SeqCst);
                // 6. opportunistic idempotency-cache expiry (bounded, sampled)
                if self.idem_sweeps.fetch_add(1, Ordering::SeqCst) & IDEM_SWEEP_MASK == 0 {
                    self.idem_sweep(now_ms(), lsn);
                }
                Ok(ExecuteOutcome {
                    lsn,
                    rows_changed: rows,
                    replayed_from_cache: false,
                    response: None,
                })
            }
            Err(e) => {
                // aborted ops never replay
                let mut b = self.db.batch();
                let mut mark_aborted = Vec::with_capacity(MARK_LEN);
                mark_aborted.push(MARK_ABORTED);
                mark_aborted.extend_from_slice(&hash8.as_bytes()[..8]);
                b.insert(&self.marks, lsn.to_be_bytes(), mark_aborted.as_slice());
                b.commit()?;
                for op in &entry.ops {
                    if let Op::BlobDrop { hash_hex } = op {
                        let _ = self.blobs.drop_blob(hash_hex);
                    }
                }
                Err(e)
            }
        }
    }

    fn apply(&self, entry: &TxEntry) -> Result<u64, MdrvDbError> {
        let mut stmts: Vec<Stmt> = Vec::with_capacity(entry.ops.len() + 1);
        for op in &entry.ops {
            match op {
                Op::Sql {
                    kind,
                    table,
                    pk_col,
                    columns,
                    values,
                    pk,
                } => {
                    stmts.push(op_to_stmt(
                        *kind, table, pk_col, columns, values, pk, entry.lsn,
                    )?);
                }
                Op::BlobPut { .. } => {} // handled post-commit
                Op::BlobDrop { hash_hex } => {
                    // tombstone the blob reference; sweep happens in verify()
                    let _ = hash_hex;
                }
            }
        }
        stmts.push(Stmt {
            sql: "UPDATE _mdrv SET value = ? WHERE key = 'applied_lsn'".into(),
            params: vec![PortValue::Text(entry.lsn.to_string())],
        });
        let n = self.port.exec_tx(&stmts).map_err(MdrvDbError::Port)?;
        Ok(n)
    }

    /// Stage bytes for a future execute() carrying Op::BlobPut{hash}.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<(String, u64), MdrvDbError> {
        let hash = BlobStore::hash(bytes);
        self.blobs.stage(&hash, bytes)?;
        Ok((hash, bytes.len() as u64))
    }

    /// Begin a streaming blob upload (arbitrary size; chunks never fully
    /// buffered). Feed the returned handle, then `blob_upload_finish`.
    /// The resulting blob is unreferenced until an `execute` carries its
    /// `BlobPut` op — unreferenced blobs are reported (not deleted) by
    /// verify, and removed with `BlobDrop`.
    pub fn blob_upload_begin(&self) -> Result<BlobUpload, MdrvDbError> {
        let id = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        );
        self.blobs.upload_begin(&id).map_err(MdrvDbError::from)
    }

    /// Finish a streaming upload: returns (hash_hex, bytes_written).
    pub fn blob_upload_finish(&self, up: BlobUpload) -> Result<(String, u64), MdrvDbError> {
        up.finish(&self.blobs).map_err(MdrvDbError::from)
    }

    /// Final (committed) blob path by hash, if present.
    pub fn blob_path(&self, hash_hex: &str) -> Option<std::path::PathBuf> {
        let p = self.blobs.final_path(hash_hex);
        p.is_file().then_some(p)
    }

    /// Dumb delete of a blob: remove the committed file (and any staged
    /// copy). Sidecar method — no journal entry, no tombstone; journaled
    /// removal is `Op::BlobDrop` via execute(). Returns true when a blob
    /// existed (committed or staged), false when absent.
    pub fn delete_blob(&self, hash_hex: &str) -> Result<bool, MdrvDbError> {
        if !BlobStore::valid_hash(hash_hex) {
            return Err(MdrvDbError::Usage(format!(
                "invalid blob hash {hash_hex:?}"
            )));
        }
        let existed = self.blobs.final_path(hash_hex).is_file()
            || self.blobs.staging_path(hash_hex).is_file();
        self.blobs.drop_blob(hash_hex)?;
        Ok(existed)
    }

    /// Bounded TTL sweep of the idempotency cache. Samples one 1/256 slice
    /// of the keyspace (random-ish prefix from time^lsn) so cost is O(budget)
    /// regardless of cache size; weak deletes — consumed-once records.
    fn idem_sweep(&self, now_ms: i64, lsn: u64) {
        let cutoff = now_ms - IDEM_TTL_MS;
        let salt = (now_ms as u64 ^ lsn) as u8;
        let lo = vec![salt];
        let hi = if salt == 255 {
            vec![salt, 0]
        } else {
            vec![salt + 1]
        };
        let mut b = self.db.batch();
        let mut pruned = 0usize;
        for kv in self.idem.range(lo..hi) {
            if pruned >= IDEM_SWEEP_BUDGET {
                break;
            }
            let Ok((k, v)) = kv.into_inner() else {
                continue;
            };
            // rec = [state][lsn be8][ts be8][response...]; short = malformed
            let expired = v.len() < 17
                || i64::from_be_bytes(v[9..17].try_into().expect("8-byte ts")) < cutoff;
            if expired {
                b.remove_weak(&self.idem, k.as_slice());
                pruned += 1;
            }
        }
        if pruned > 0 && b.commit().is_ok() {
            let _ = self.report(
                20,
                "idem.prune",
                Some(serde_json::json!({"pruned": pruned})),
            );
        }
    }

    /// Read-only SELECT path. Rejects anything that is not a SELECT.
    pub fn query(
        &self,
        sql: &str,
        params: Vec<PortValue>,
    ) -> Result<serde_json::Value, MdrvDbError> {
        let t = sql.trim_start();
        let head = t
            .as_bytes()
            .first()
            .map(|b| b.to_ascii_lowercase())
            .unwrap_or(0);
        if head != b's' || !t.to_ascii_lowercase().starts_with("select") || sql.contains(';') {
            return Err(MdrvDbError::Usage("query() is SELECT-only".into()));
        }
        let out = self.port.query(sql, &params).map_err(MdrvDbError::Port)?;
        // Named-row objects: {col: value}. Zero rows => zero objects
        // (names are unknowable then; callers treat [] as "no rows").
        let rows: Vec<serde_json::Value> = out
            .rows
            .iter()
            .map(|r| {
                let mut obj = serde_json::Map::new();
                for (i, v) in r.iter().enumerate() {
                    if let Some(name) = out.names.get(i) {
                        obj.insert(name.clone(), v.to_json());
                    }
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        Ok(serde_json::Value::Array(rows))
    }

    /// Trusted DDL/migrations (called by the owning app at boot, not replayed).
    pub fn bootstrap(&self, statements: &[String]) -> Result<(), MdrvDbError> {
        let total = statements.len();
        for (i, s) in statements.iter().enumerate() {
            self.port.exec(s).map_err(|e| {
                // Terse parser errors ("incomplete input", no offset) force
                // callers with a multi-statement schema batch to bisect by
                // hand — tag the failing statement and an SQL head instead.
                MdrvDbError::Usage(format!(
                    "bootstrap statement {}/{} failed: {e}; sql: {}",
                    i + 1,
                    total,
                    sql_head(s)
                ))
            })?;
        }
        Ok(())
    }

    // -- recovery ------------------------------------------------------------

    fn recover(&self) -> Result<RecoveryReport, MdrvDbError> {
        let started = std::time::Instant::now();
        let applied_before = self.applied.load(Ordering::SeqCst);
        let from = applied_before.saturating_add(1);
        let (mut replayed, mut pending, mut aborted) = (0u64, 0u64, 0u64);

        // collect committed entries above the watermark
        let mut batch: Vec<TxEntry> = Vec::new();
        for kv in self.wal.range(from.to_be_bytes()..=u64::MAX.to_be_bytes()) {
            let (k, v) = kv.into_inner()?;
            let lsn = u64::from_be_bytes(k[..8].try_into().expect("8-byte key"));
            let mark = self.marks.get(&k)?;
            let status = mark.as_ref().and_then(|m| m.first().copied()).unwrap_or(0);
            match status {
                MARK_COMMITTED => {
                    let entry: TxEntry = postcard::from_bytes(&v)?;
                    batch.push(entry);
                }
                MARK_PENDING => pending += 1, // applied-or-not; port watermark decides
                MARK_ABORTED => aborted += 1,
                _ => {
                    return Err(MdrvDbError::Corrupt(format!(
                        "lsn {lsn}: unknown mark {status}"
                    )));
                }
            }
        }

        // replay in LSN order, grouped transactions
        const GROUP: usize = 500;
        for group in batch.chunks(GROUP) {
            self.apply_group(group)?;
            let last = group.last().expect("non-empty group").lsn;
            self.applied.store(last, Ordering::SeqCst);
            replayed += group.len() as u64;
        }
        // keep fjall mirror in sync (info only)
        let mut b = self.db.batch();
        b.insert(
            &self.meta,
            META_APPLIED,
            self.applied.load(Ordering::SeqCst).to_string(),
        );
        b.commit()?;

        Ok(RecoveryReport {
            applied_before,
            replayed,
            skipped_pending: pending,
            skipped_aborted: aborted,
            wall_ms: started.elapsed().as_millis() as i64,
        })
    }

    fn apply_group(&self, group: &[TxEntry]) -> Result<(), MdrvDbError> {
        let mut stmts = Vec::new();
        for entry in group {
            for op in &entry.ops {
                if let Op::Sql {
                    kind,
                    table,
                    pk_col,
                    columns,
                    values,
                    pk,
                } = op
                {
                    stmts.push(op_to_stmt(
                        *kind, table, pk_col, columns, values, pk, entry.lsn,
                    )?);
                }
            }
        }
        let last_lsn = group.last().expect("non-empty group").lsn;
        stmts.push(Stmt {
            sql: "UPDATE _mdrv SET value = ? WHERE key = 'applied_lsn'".into(),
            params: vec![PortValue::Text(last_lsn.to_string())],
        });
        self.port.exec_tx(&stmts).map_err(MdrvDbError::Port)?;
        // re-promote any blobs the entries reference
        for entry in group {
            for op in &entry.ops {
                if let Op::BlobPut { hash_hex } = op {
                    self.blobs.promote(hash_hex)?;
                }
            }
        }
        Ok(())
    }

    // -- checkpoint / prune ----------------------------------------------------

    /// Prune WAL+marks up to the applied watermark. Call only after a
    /// successful backup of turso+blobs (see backup::run).
    pub fn checkpoint(&self, compact: bool) -> Result<u64, MdrvDbError> {
        let applied = self.applied.load(Ordering::SeqCst);
        if applied == 0 {
            return Ok(0);
        }
        let mut b = self.db.batch();
        for kv in self.wal.range(..=applied.to_be_bytes()) {
            let (k, _) = kv.into_inner()?;
            b.remove_weak(&self.wal, k.as_slice());
            b.remove_weak(&self.marks, k.as_slice());
        }
        b.insert(&self.meta, META_CHECKPOINT, applied.to_string());
        b.commit()?;
        if compact {
            self.wal.major_compact()?;
        }
        Ok(applied)
    }

    // -- report ---------------------------------------------------------------

    pub fn report(
        &self,
        level: u8,
        event: &str,
        data: Option<serde_json::Value>,
    ) -> Result<(), MdrvDbError> {
        let e = ReportEntry {
            v: 1,
            ts_ms: now_ms(),
            level,
            event: event.into(),
            data_json: data.unwrap_or(serde_json::Value::Null).to_string(),
        };
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&e.ts_ms.to_be_bytes());
        key[8..].copy_from_slice(&self.report_seq.fetch_add(1, Ordering::SeqCst).to_be_bytes());
        let bytes = postcard::to_allocvec(&e)?;
        self.report.insert(key, bytes.as_slice())?;
        Ok(())
    }

    /// Export report entries (after `since_ms`) as JSON lines.
    pub fn report_export(
        &self,
        since_ms: i64,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, MdrvDbError> {
        let mut from = [0u8; 16];
        from[..8].copy_from_slice(&since_ms.to_be_bytes());
        let mut out = Vec::new();
        for kv in self.report.range(from..) {
            if out.len() >= limit {
                break;
            }
            let (_, v) = kv.into_inner()?;
            let e: ReportEntry = postcard::from_bytes(&v)?;
            out.push(json!({ "ts_ms": e.ts_ms, "level": e.level, "event": e.event, "data": serde_json::from_str::<serde_json::Value>(&e.data_json).unwrap_or(serde_json::Value::Null) }));
        }
        Ok(out)
    }

    // -- status / close ---------------------------------------------------------

    pub fn status(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "open": self.open,
            "applied_lsn": self.applied.load(Ordering::SeqCst),
            "checkpoint_lsn": self.meta.get(META_CHECKPOINT).ok().flatten()
                .map(|v| String::from_utf8_lossy(&v).parse::<u64>().unwrap_or(0)).unwrap_or(0),
            "entry_schema": ENTRY_SCHEMA,
            "wal_disk_space": self.wal.disk_space(),
            "fsync_each_write": self.cfg.fsync_each_write,
            "root": self.root.display().to_string(),
        })
    }

    /// Public flush+fsync (close-equivalent; Drop also attempts this).
    pub fn persist_sync_all(&self) -> Result<(), MdrvDbError> {
        self.db.persist(PersistMode::SyncAll)?;
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), MdrvDbError> {
        if self.open {
            self.db.persist(PersistMode::SyncAll)?;
            self.open = false;
        }
        Ok(())
    }

    // -- helpers -----------------------------------------------------------------

    fn next_lsn(&self) -> u64 {
        let floor = self.applied.load(Ordering::SeqCst);
        let last = self
            .wal
            .last_key_value()
            .map(|g| match g.into_inner() {
                Ok((k, _)) => u64::from_be_bytes(k[..8].try_into().unwrap_or([0; 8])),
                Err(_) => 0,
            })
            .unwrap_or(0);
        last.max(floor).saturating_add(1)
    }
}

fn op_to_stmt(
    kind: SqlKind,
    table: &str,
    pk_col: &str,
    columns: &[String],
    values: &[PortValue],
    pk: &PortValue,
    lsn: u64,
) -> Result<Stmt, MdrvDbError> {
    let table = safe_ident(table).map_err(MdrvDbError::Usage)?;
    let pk_col = safe_ident(pk_col).map_err(MdrvDbError::Usage)?;
    for c in columns {
        safe_ident(c).map_err(MdrvDbError::Usage)?;
    }
    if columns.len() != values.len() {
        return Err(MdrvDbError::Usage("columns/values length mismatch".into()));
    }
    // resolve the Lsn placeholder to the entry's LSN (deterministic replay:
    // journaled value is the marker, resolution happens at stmt-build time)
    let resolve = |v: &PortValue| match v {
        PortValue::Lsn => PortValue::Int(lsn as i64),
        other => other.clone(),
    };
    let (sql, params): (String, Vec<PortValue>);
    match kind {
        SqlKind::Insert => {
            let cols = columns
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(",");
            let qs = vec!["?"; columns.len()].join(",");
            sql = format!("INSERT OR IGNORE INTO {table} ({cols}) VALUES ({qs})");
            params = values.iter().map(resolve).collect();
        }
        SqlKind::Upsert => {
            let cols = columns
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(",");
            let qs = vec!["?"; columns.len()].join(",");
            sql = format!("INSERT OR REPLACE INTO {table} ({cols}) VALUES ({qs})");
            params = values.iter().map(resolve).collect();
        }
        SqlKind::Update => {
            if columns.is_empty() {
                return Err(MdrvDbError::Usage("Update with no columns".into()));
            }
            let sets = columns
                .iter()
                .map(|c| format!("\"{c}\" = ?"))
                .collect::<Vec<_>>()
                .join(",");
            let mut p: Vec<PortValue> = values.iter().map(resolve).collect();
            p.push(resolve(pk));
            sql = format!("UPDATE {table} SET {sets} WHERE \"{pk_col}\" = ?");
            params = p;
        }
        SqlKind::Delete => {
            sql = format!("DELETE FROM {table} WHERE \"{pk_col}\" = ?");
            params = vec![resolve(pk)];
        }
    }
    Ok(Stmt { sql, params })
}

fn read_applied(port: &dyn DataPort) -> Result<u64, MdrvDbError> {
    let out = port
        .query("SELECT value FROM _mdrv WHERE key = 'applied_lsn'", &[])
        .map_err(MdrvDbError::Port)?;
    match out.rows.first().and_then(|r| r.first()) {
        Some(PortValue::Text(s)) => s
            .parse()
            .map_err(|_| MdrvDbError::Corrupt("bad applied_lsn".into())),
        _ => Ok(0),
    }
}
