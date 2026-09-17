//! @mdrv/db napi surface. JSON-string in/out for structured payloads
//! (Bun's JSON.parse beats deep marshalling), Uint8Array for blobs.
//! All engine work runs on spawn_blocking — never block the JS thread.
//!
//! v2 additions over v1: live/ fleet layout resolution and a streaming
//! blob upload API (begin/chunk/finish/abort) for large files.

use mdrv_db::blob::BlobUpload;
use mdrv_db::engine::{Engine, EngineConfig, MutateRequest};
use mdrv_db::port::{DataPort, QueryOut, Stmt};
use mdrv_db::PortValue;
use napi::bindgen_prelude::{Error, Result, Uint8Array};
use napi_derive::napi;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

// ---- Turso data port --------------------------------------------------------

struct TursoPort {
    rt: tokio::runtime::Runtime,
    conn: std::sync::Mutex<turso::Connection>,
}

impl TursoPort {
    fn open(db_path: PathBuf) -> std::result::Result<Self, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let conn = rt
            .block_on(async {
                let path = db_path.display().to_string();
                let db = turso::Builder::new_local(&path).build().await?;
                let mut conn: turso::Connection = db.connect()?;
                use turso::transaction::TransactionBehavior;
                conn.set_transaction_behavior(TransactionBehavior::Immediate);
                // pragma returns a row; consume via query (default journal
                // mode is fine — transactions run Immediate)
                let _ = conn.query("PRAGMA journal_mode", ()).await?;
                Ok::<turso::Connection, turso::Error>(conn)
            })
            .map_err(|e: turso::Error| e.to_string())?;
        Ok(TursoPort {
            rt,
            conn: std::sync::Mutex::new(conn),
        })
    }
}

fn to_turso_params(params: &[PortValue]) -> std::result::Result<Vec<turso::Value>, String> {
    params
        .iter()
        .map(|v| match v {
            PortValue::Null => Ok(turso::Value::Null),
            PortValue::Int(i) => Ok(turso::Value::Integer(*i)),
            PortValue::Real(f) => Ok(turso::Value::Real(*f)),
            PortValue::Text(s) => Ok(turso::Value::Text(s.clone())),
            PortValue::Blob(b) => Ok(turso::Value::Blob(b.clone())),
            // the engine resolves Lsn at stmt-build time; reaching the port
            // unresolved is an internal invariant violation
            PortValue::Lsn => Err("unresolved PortValue::Lsn reached the port".into()),
        })
        .collect()
}

fn from_turso_value(v: turso::Value) -> PortValue {
    match v {
        turso::Value::Null => PortValue::Null,
        turso::Value::Integer(i) => PortValue::Int(i),
        turso::Value::Real(f) => PortValue::Real(f),
        turso::Value::Text(s) => PortValue::Text(s),
        turso::Value::Blob(b) => PortValue::Blob(b),
    }
}

impl DataPort for TursoPort {
    fn exec_tx(&self, stmts: &[Stmt]) -> std::result::Result<u64, String> {
        let mut conn = self.conn.lock().unwrap();
        let res: std::result::Result<u64, String> = self.rt.block_on(async {
            let tx = conn.transaction().await.map_err(|e| e.to_string())?;
            let mut total = 0u64;
            for s in stmts {
                let params = to_turso_params(&s.params).map_err(|e| e.to_string())?;
                total += tx
                    .execute(&s.sql, params)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            tx.commit().await.map_err(|e| e.to_string())?;
            Ok(total)
        });
        res
    }

    fn exec(&self, sql: &str) -> std::result::Result<u64, String> {
        let conn = self.conn.lock().unwrap();
        self.rt
            .block_on(async { conn.execute(sql, ()).await.map_err(|e| e.to_string()) })
    }

    fn query(&self, sql: &str, params: &[PortValue]) -> std::result::Result<QueryOut, String> {
        let conn = self.conn.lock().unwrap();
        let res: std::result::Result<QueryOut, String> = self.rt.block_on(async {
            let params = to_turso_params(params).map_err(|e| e.to_string())?;
            let mut rows = conn.query(sql, params).await.map_err(|e| e.to_string())?;
            // Rows knows its columns from the prepared statement, so names
            // are available even when the result set is empty.
            let names = rows.column_names();
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(|e| e.to_string())? {
                let n = row.column_count();
                let mut r = Vec::with_capacity(n);
                for i in 0..n {
                    r.push(from_turso_value(
                        row.get_value(i).map_err(|e| e.to_string())?,
                    ));
                }
                out.push(r);
            }
            Ok(QueryOut { names, rows: out })
        });
        res
    }

    fn integrity_check(&self) -> std::result::Result<String, String> {
        let conn = self.conn.lock().unwrap();
        self.rt.block_on(async {
            let mut rows = conn
                .query("PRAGMA integrity_check", ())
                .await
                .map_err(|e| e.to_string())?;
            if let Some(row) = rows.next().await.map_err(|e| e.to_string())? {
                if let Ok(turso::Value::Text(s)) = row.get_value(0) {
                    return Ok(s);
                }
            }
            Ok::<String, String>("unknown".into())
        })
    }
}

// ---- napi surface -------------------------------------------------------------

/// The engine lives behind Arc<Mutex<Option<Engine>>>: `close()` takes the
/// Option to None, dropping the Engine (and with it fjall's file lock).
/// In-flight streaming uploads live in their own map; leftovers on disk are
/// swept by the engine's staging sweep on next open.
struct Inner {
    engine: std::sync::Mutex<Option<Engine>>,
    uploads: std::sync::Mutex<HashMap<String, BlobUpload>>,
}

static UPLOAD_SEQ: AtomicU64 = AtomicU64::new(0);

impl Inner {
    fn with<T>(&self, f: impl FnOnce(&Engine) -> Result<T>) -> Result<T> {
        let guard = self.engine.lock().unwrap();
        match guard.as_ref() {
            Some(e) => f(e),
            None => Err(Error::from_reason("engine is closed")),
        }
    }
}

/// BLAKE3 of arbitrary bytes, hex — used by TS-side backup verification
/// (Bun's CryptoHasher has no blake3). Pure function, opens nothing.
#[napi]
pub fn blake3_hex(bytes: Uint8Array) -> String {
    blake3::hash(bytes.as_ref()).to_hex().to_string()
}

#[napi]
pub struct MdrvDb {
    inner: std::sync::Arc<Inner>,
}

#[napi]
impl MdrvDb {
    /// Open (or create) a data dir and run recovery-on-open. Uses the v2
    /// live/ layout when present (data dir = <root>/live), else flat.
    #[napi(constructor)]
    pub fn new(data_dir: String, name: String, fsync_each_write: Option<bool>) -> Result<Self> {
        let root = PathBuf::from(&data_dir);
        std::fs::create_dir_all(&root)
            .map_err(|e| Error::from_reason(format!("mkdir {data_dir}: {e}")))?;
        let live = mdrv_db::live_dir(&root);
        let port = TursoPort::open(live.join("app.db")).map_err(Error::from_reason)?;
        let cfg = EngineConfig {
            fsync_each_write: fsync_each_write.unwrap_or(true),
        };
        let engine = Engine::open(&live, &name, Box::new(port), cfg)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(MdrvDb {
            inner: std::sync::Arc::new(Inner {
                engine: std::sync::Mutex::new(Some(engine)),
                uploads: std::sync::Mutex::new(HashMap::new()),
            }),
        })
    }

    /// WAL-first mutation. `request_json` = MutateRequest (see index.d.ts).
    #[napi]
    pub async fn execute(&self, request_json: String) -> Result<String> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                let req: MutateRequest = serde_json::from_str(&request_json)
                    .map_err(|e| Error::from_reason(format!("bad request json: {e}")))?;
                let out = e
                    .execute(req)
                    .map_err(|e| Error::from_reason(e.to_string()))?;
                Ok(serde_json::to_string(&out).unwrap())
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Read-only SELECT. Returns rows as JSON array of named-row objects.
    #[napi]
    pub async fn query(&self, sql: String, params_json: Option<String>) -> Result<String> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                let params: Vec<PortValue> = match params_json {
                    Some(s) if !s.is_empty() => serde_json::from_str(&s)
                        .map_err(|e| Error::from_reason(format!("bad params json: {e}")))?,
                    _ => Vec::new(),
                };
                let rows = e
                    .query(&sql, params)
                    .map_err(|e| Error::from_reason(e.to_string()))?;
                Ok(serde_json::to_string(&rows).unwrap())
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Trusted DDL/migrations for the owning app (boot time only).
    #[napi]
    pub async fn bootstrap(&self, statements_json: String) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                let stmts: Vec<String> = serde_json::from_str(&statements_json)
                    .map_err(|e| Error::from_reason(format!("bad statements json: {e}")))?;
                e.bootstrap(&stmts)
                    .map_err(|e| Error::from_reason(e.to_string()))
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Stage bytes (fsync) for a follow-up execute() with Op.BlobPut.
    /// Whole-buffer convenience path — stream large files instead.
    #[napi]
    pub async fn put_blob(&self, bytes: Uint8Array) -> Result<String> {
        let inner = self.inner.clone();
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                let (hash, size) = e
                    .put_blob(&bytes)
                    .map_err(|e| Error::from_reason(e.to_string()))?;
                Ok(serde_json::to_string(&json!({ "hash": hash, "size": size })).unwrap())
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Begin a streaming blob upload; returns an upload id.
    #[napi]
    pub fn blob_put_begin(&self) -> Result<String> {
        let id = format!(
            "up-{}-{}",
            mdrv_db::now_ms(),
            UPLOAD_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        self.inner.with(|e| {
            let up = e
                .blob_upload_begin()
                .map_err(|err| Error::from_reason(err.to_string()))?;
            self.inner.uploads.lock().unwrap().insert(id.clone(), up);
            Ok(id)
        })
    }

    /// Append a chunk to a streaming upload.
    #[napi]
    pub fn blob_put_chunk(&self, id: String, bytes: Uint8Array) -> Result<()> {
        let mut ups = self.inner.uploads.lock().unwrap();
        let up = ups
            .get_mut(&id)
            .ok_or_else(|| Error::from_reason(format!("unknown upload id {id}")))?;
        up.write(bytes.as_ref())
            .map_err(|e| Error::from_reason(format!("chunk write: {e}")))
    }

    /// Finish a streaming upload: fsync, move into the CAS store, return
    /// JSON {hash,size}. Dedup: identical content keeps one file.
    #[napi]
    pub async fn blob_put_finish(&self, id: String) -> Result<String> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let up = inner
                .uploads
                .lock()
                .unwrap()
                .remove(&id)
                .ok_or_else(|| Error::from_reason(format!("unknown upload id {id}")))?;
            inner.with(|e| {
                let (hash, size) = e
                    .blob_upload_finish(up)
                    .map_err(|err| Error::from_reason(err.to_string()))?;
                Ok(serde_json::to_string(&json!({ "hash": hash, "size": size })).unwrap())
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Abort a streaming upload (temp file removed or left for the sweep).
    #[napi]
    pub fn blob_put_abort(&self, id: String) -> Result<()> {
        if let Some(up) = self.inner.uploads.lock().unwrap().remove(&id) {
            up.abort();
        }
        Ok(())
    }

    /// Absolute path of a stored blob (for Bun.file() serving). Read-only.
    #[napi]
    pub fn get_blob_path(&self, hash_hex: String) -> Result<Option<String>> {
        self.inner.with(|e| {
            let p = e.blobs.final_path(&hash_hex);
            Ok(if p.is_file() {
                Some(p.display().to_string())
            } else {
                None
            })
        })
    }

    /// Dumb delete of a blob (committed file + any staged copy). Returns
    /// true when a blob existed. No journal entry — journaled removal is
    /// Op.BlobDrop via execute().
    #[napi]
    pub async fn delete_blob(&self, hash_hex: String) -> Result<bool> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                e.delete_blob(&hash_hex)
                    .map_err(|e| Error::from_reason(e.to_string()))
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Append to the verbose ops log (Fifo keyspace, ages out).
    #[napi]
    pub fn report(&self, level: u8, event: String, data_json: Option<String>) -> Result<()> {
        self.inner.with(|e| {
            let data = data_json
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .unwrap_or(serde_json::Value::Null);
            e.report(level, &event, Some(data))
                .map_err(|e| Error::from_reason(e.to_string()))
        })
    }

    /// Export report entries since ts_ms as JSON array.
    #[napi]
    pub async fn report_export(&self, since_ms: i64, limit: Option<u32>) -> Result<String> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                let out = e
                    .report_export(since_ms, limit.unwrap_or(1000) as usize)
                    .map_err(|e| Error::from_reason(e.to_string()))?;
                Ok(serde_json::to_string(&out).unwrap())
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Full backup into dest dir (VACUUM INTO + blobs + fjall + manifest).
    #[napi]
    pub async fn backup(&self, dest_dir: String) -> Result<String> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                let out = mdrv_db::backup::run(e, &dest_dir)
                    .map_err(|e| Error::from_reason(e.to_string()))?;
                Ok(serde_json::to_string(&out.manifest).unwrap())
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Integrity sweep (wal hashes, blob re-hash, port check, fence).
    #[napi]
    pub async fn verify(&self) -> Result<String> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                let out = mdrv_db::verify::run(e).map_err(|e| Error::from_reason(e.to_string()))?;
                Ok(serde_json::to_string(&out).unwrap())
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Prune WAL+marks ≤ applied watermark (call after successful backup).
    #[napi]
    pub async fn checkpoint(&self, compact: Option<bool>) -> Result<String> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.with(|e| {
                let lsn = e
                    .checkpoint(compact.unwrap_or(false))
                    .map_err(|e| Error::from_reason(e.to_string()))?;
                Ok(serde_json::to_string(&json!({ "pruned_to": lsn })).unwrap())
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    #[napi(getter)]
    pub fn status(&self) -> String {
        self.inner
            .with(|e| Ok(serde_json::to_string(&e.status()).unwrap()))
            .unwrap_or_else(|_| r#"{"open":false}"#.into())
    }

    /// Persist (SyncAll) and drop the engine — releases fjall's file lock.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.engine.lock().unwrap();
            if let Some(e) = guard.as_ref() {
                e.persist_sync_all()
                    .map_err(|e| Error::from_reason(e.to_string()))?;
            }
            *guard = None; // drop Engine → release lock
            Ok(())
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }
}
