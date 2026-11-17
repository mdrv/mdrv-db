//! @mdrv/db napi surface. JSON-string in/out for structured payloads
//! (Bun's JSON.parse beats deep marshalling), Uint8Array for blobs.
//! All engine work runs on spawn_blocking — never block the JS thread.
//!
//! v2 additions over v1: live/ fleet layout resolution and a streaming
//! blob upload API (begin/chunk/finish/abort) for large files.

use mdrv_db::blob::BlobUpload;
use mdrv_db::engine::{Engine, EngineConfig, MutateRequest};
use mdrv_db::PortValue;
use napi::bindgen_prelude::{Error, Result, Uint8Array};
use napi_derive::napi;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Inner {
    engine: std::sync::Mutex<Option<Engine>>,
    uploads: std::sync::Mutex<HashMap<String, BlobUpload>>,
}

static UPLOAD_SEQ: AtomicU64 = AtomicU64::new(0);

impl Inner {
    fn with<T>(&self, f: impl FnOnce(&Engine) -> Result<T>) -> Result<T> {
        // a panic inside an earlier call would poison this mutex; surface
        // that as a JS error instead of aborting on the next call
        let guard = self
            .engine
            .lock()
            .map_err(|_| Error::from_reason("engine lock poisoned by a failed call"))?;
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
        let port = mdrv_db::TursoPort::open(live.join("app.db")).map_err(Error::from_reason)?;
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
                serde_json::to_string(&out)
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
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
                serde_json::to_string(&rows)
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
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
                serde_json::to_string(&json!({ "hash": hash, "size": size }))
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
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
            self.inner
                .uploads
                .lock()
                .map_err(|_| Error::from_reason("upload lock poisoned"))?
                .insert(id.clone(), up);
            Ok(id)
        })
    }

    /// Append a chunk to a streaming upload.
    #[napi]
    pub fn blob_put_chunk(&self, id: String, bytes: Uint8Array) -> Result<()> {
        let mut ups = self
            .inner
            .uploads
            .lock()
            .map_err(|_| Error::from_reason("upload lock poisoned"))?;
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
                .map_err(|_| Error::from_reason("upload lock poisoned"))?
                .remove(&id)
                .ok_or_else(|| Error::from_reason(format!("unknown upload id {id}")))?;
            inner.with(|e| {
                let (hash, size) = e
                    .blob_upload_finish(up)
                    .map_err(|err| Error::from_reason(err.to_string()))?;
                serde_json::to_string(&json!({ "hash": hash, "size": size }))
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    /// Abort a streaming upload (temp file removed or left for the sweep).
    #[napi]
    pub fn blob_put_abort(&self, id: String) -> Result<()> {
        if let Some(up) = self
            .inner
            .uploads
            .lock()
            .map_err(|_| Error::from_reason("upload lock poisoned"))?
            .remove(&id)
        {
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
                serde_json::to_string(&out)
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
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
                serde_json::to_string(&out.manifest)
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
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
                serde_json::to_string(&out)
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
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
                serde_json::to_string(&json!({ "pruned_to": lsn }))
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
            })
        })
        .await
        .map_err(|e| Error::from_reason(format!("join error: {e}")))?
    }

    #[napi(getter)]
    pub fn status(&self) -> String {
        self.inner
            .with(|e| {
                serde_json::to_string(&e.status())
                    .map_err(|e| Error::from_reason(format!("serialize: {e}")))
            })
            .unwrap_or_else(|_| r#"{"open":false}"#.to_string())
    }

    /// Persist (SyncAll) and drop the engine — releases fjall's file lock.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner
                .engine
                .lock()
                .map_err(|_| Error::from_reason("engine lock poisoned by a failed call"))?;
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
