//! Synchronous Turso data port (optional `turso` feature).
//!
//! Ported from the v1 napi crate (same logic, no napi). Gated behind the
//! optional `turso` feature so embedders that bring their own
//! [`DataPort`] never compile a SQL engine. Holding the port takes the
//! turso file lock — the owning app must not have the DB open elsewhere.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::port::{DataPort, QueryOut, Stmt};
use crate::PortValue;

pub struct TursoPort {
    rt: tokio::runtime::Runtime,
    conn: Mutex<turso::Connection>,
}

impl TursoPort {
    pub fn open(db_path: PathBuf) -> Result<Self, String> {
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
            conn: Mutex::new(conn),
        })
    }
}

fn to_turso_params(params: &[PortValue]) -> Result<Vec<turso::Value>, String> {
    params
        .iter()
        .map(|v| match v {
            PortValue::Null => Ok(turso::Value::Null),
            PortValue::Int(i) => Ok(turso::Value::Integer(*i)),
            PortValue::Real(f) => Ok(turso::Value::Real(*f)),
            PortValue::Text(s) => Ok(turso::Value::Text(s.clone())),
            PortValue::Blob(b) => Ok(turso::Value::Blob(b.clone())),
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

impl TursoPort {
    /// Lock the connection. A poisoned mutex (a panic in a previous holder)
    /// surfaces as an error instead of cascading a panic through the daemon.
    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, turso::Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "port connection lock poisoned".to_string())
    }
}

impl DataPort for TursoPort {
    fn exec_tx(&self, stmts: &[Stmt]) -> Result<u64, String> {
        let mut conn = self.lock_conn()?;
        self.rt.block_on(async {
            let tx = conn.transaction().await.map_err(|e| e.to_string())?;
            let mut total = 0u64;
            for (i, s) in stmts.iter().enumerate() {
                let params = to_turso_params(&s.params)?;
                match tx.execute(&s.sql, params).await {
                    Ok(n) => total += n,
                    Err(e) => {
                        // roll back explicitly so the connection never holds
                        // a dangling tx (turso defers it to the NEXT
                        // transaction() call, which would silently swallow
                        // a later commit)
                        let _ = tx.rollback().await;
                        return Err(format!(
                            "statement {}/{} failed: {e}; sql: {}",
                            i + 1,
                            stmts.len(),
                            crate::port::sql_head(&s.sql)
                        ));
                    }
                }
            }
            tx.commit().await.map_err(|e| e.to_string())?;
            Ok(total)
        })
    }

    fn exec(&self, sql: &str) -> Result<u64, String> {
        let conn = self.lock_conn()?;
        self.rt
            .block_on(async { conn.execute(sql, ()).await.map_err(|e| e.to_string()) })
    }

    fn query(&self, sql: &str, params: &[PortValue]) -> Result<QueryOut, String> {
        let conn = self.lock_conn()?;
        self.rt.block_on(async {
            let params = to_turso_params(params)?;
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
        })
    }

    fn integrity_check(&self) -> Result<String, String> {
        let conn = self.lock_conn()?;
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

#[cfg(test)]
mod tests {
    use super::TursoPort;
    use crate::port::DataPort as _;
    use crate::{Engine, EngineConfig, MutateRequest, Op, PortValue, SqlKind};
    use std::sync::Arc;

    #[test]
    fn turso_port_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mdrv-db-turso-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let port = TursoPort::open(dir.join("app.db")).expect("open port");
        let mut e = Engine::open(
            &dir,
            "upperadd-test",
            Box::new(port),
            EngineConfig::default(),
        )
        .expect("open engine");
        e.bootstrap(&["CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT NOT NULL)".into()])
            .expect("bootstrap");
        let out = e
            .execute(MutateRequest {
                actor: "test".into(),
                ops: vec![Op::Sql {
                    kind: SqlKind::Upsert,
                    table: "t".into(),
                    pk_col: "id".into(),
                    columns: vec!["id".into(), "v".into()],
                    values: vec![PortValue::Int(1), PortValue::Text("hello".into())],
                    pk: PortValue::Int(1),
                }],
                idem_key: None,
                response: None,
            })
            .expect("execute");
        assert_eq!(out.lsn, 1);
        // turso counts INSERT OR REPLACE as 2 (delete + insert)
        assert!(out.rows_changed >= 1);
        let rows = e
            .query("SELECT v FROM t WHERE id = ?", vec![PortValue::Int(1)])
            .expect("query");
        assert_eq!(rows[0]["v"], serde_json::json!("hello"));
        e.close().expect("close");
        // reopen fresh: integrity_check lives on the port, not the engine
        let p = TursoPort::open(dir.join("app.db")).expect("reopen port");
        assert_eq!(p.integrity_check().expect("integrity_check"), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bootstrap_error_context() {
        let dir = std::env::temp_dir().join(format!("mdrv-db-boot-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let port = TursoPort::open(dir.join("app.db")).expect("open port");
        let mut e = Engine::open(&dir, "boot-ctx", Box::new(port), EngineConfig::default())
            .expect("open engine");
        let err = e
            .bootstrap(&[
                "CREATE TABLE ok (id INTEGER PRIMARY KEY)".into(),
                "CREATE TABLE broken (id TEXT".into(),
            ])
            .expect_err("bad sql must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("bootstrap statement 2/2"),
            "no statement index: {msg}"
        );
        assert!(msg.contains("CREATE TABLE broken"), "no sql head: {msg}");
        e.close().expect("close");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fresh(name: &str) -> (std::path::PathBuf, Engine) {
        let dir = std::env::temp_dir().join(format!("mdrv-db-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let port = TursoPort::open(dir.join("app.db")).expect("open port");
        let e =
            Engine::open(&dir, name, Box::new(port), EngineConfig::default()).expect("open engine");
        e.bootstrap(&["CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT NOT NULL)".into()])
            .expect("bootstrap");
        (dir, e)
    }

    fn sql_upsert(id: i64) -> Op {
        Op::Sql {
            kind: SqlKind::Upsert,
            table: "t".into(),
            pk_col: "id".into(),
            columns: vec!["id".into(), "v".into()],
            values: vec![PortValue::Int(id), PortValue::Text("x".into())],
            pk: PortValue::Int(id),
        }
    }

    /// Regression (audit A-F1): concurrent execute() callers share one
    /// &self Engine; LSN allocation must not hand out duplicates.
    #[test]
    fn concurrent_execute_allocates_unique_lsns() {
        let (dir, e) = fresh("lsn-race");
        let e = std::sync::Arc::new(e);
        let mut handles = Vec::new();
        for t in 0..8 {
            let e = e.clone();
            handles.push(std::thread::spawn(move || {
                let mut lsns = Vec::new();
                for i in 0..8 {
                    let out = e
                        .execute(MutateRequest {
                            actor: format!("t{t}"),
                            ops: vec![sql_upsert(t * 100 + i)],
                            idem_key: None,
                            response: None,
                        })
                        .expect("execute");
                    lsns.push(out.lsn);
                }
                lsns
            }));
        }
        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().expect("join"));
        }
        all.sort_unstable();
        let n = all.len();
        all.dedup();
        assert_eq!(all.len(), n, "duplicate LSNs allocated: {all:?}");
        Arc::into_inner(e)
            .expect("sole owner")
            .close()
            .expect("close");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression (audit A-F2): an aborted transaction must not destroy
    /// blobs; a committed BlobDrop must.
    #[test]
    fn blobdrop_aborts_preserve_and_commits_delete() {
        let (dir, mut e) = fresh("blobdrop");
        let bytes = b"payload-for-audit".to_vec();
        let (hash, _) = e.put_blob(&bytes).expect("stage blob");
        assert!(e.blob_path(&hash).is_none(), "staged only, not final");
        let staging = e.blobs.staging_path(&hash);
        assert!(staging.is_file(), "staging copy must exist");

        // BlobDrop + failing SQL in one tx -> abort; blob must survive
        let res = e.execute(MutateRequest {
            actor: "test".into(),
            ops: vec![
                Op::BlobDrop {
                    hash_hex: hash.clone(),
                },
                Op::Sql {
                    kind: SqlKind::Upsert,
                    table: "no_such_table".into(),
                    pk_col: "id".into(),
                    columns: vec!["id".into()],
                    values: vec![PortValue::Int(1)],
                    pk: PortValue::Int(1),
                },
            ],
            idem_key: None,
            response: None,
        });
        assert!(res.is_err(), "tx must fail");
        assert!(staging.is_file(), "abort must not delete the staged blob");

        // committed BlobDrop deletes
        let res = e
            .execute(MutateRequest {
                actor: "test".into(),
                ops: vec![Op::BlobDrop {
                    hash_hex: hash.clone(),
                }],
                idem_key: None,
                response: None,
            })
            .expect("drop tx");
        let _ = res.lsn;
        assert!(!staging.is_file(), "commit must delete the staged blob");
        e.close().expect("close");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression (audit A-H2): malformed blob hashes are rejected with a
    /// Usage error instead of slicing into a panic.
    #[test]
    fn invalid_blob_hash_is_usage_error() {
        let (dir, mut e) = fresh("badhash");
        for bad in ["z", "ZZ", "a", "64-chars-but-not-hex!!"] {
            let res = e.execute(MutateRequest {
                actor: "test".into(),
                ops: vec![Op::BlobDrop {
                    hash_hex: bad.into(),
                }],
                idem_key: None,
                response: None,
            });
            let err = res.expect_err("must reject");
            assert!(
                err.to_string().contains("invalid blob hash"),
                "{bad}: {err}"
            );
        }
        assert!(e.blob_path("short").is_none());
        e.close().expect("close");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression (audit A-F4): backup captures the ceiling BEFORE the
    /// snapshot and checkpoints only up to it; manifest applied_lsn never
    /// exceeds what the artifacts contain.
    #[test]
    fn backup_ceiling_and_auto_checkpoint() {
        let (dir, mut e) = fresh("backup-ceiling");
        for i in 0..3 {
            e.execute(MutateRequest {
                actor: "test".into(),
                ops: vec![sql_upsert(i)],
                idem_key: None,
                response: None,
            })
            .expect("execute");
        }
        let applied = e.status()["applied_lsn"].as_u64().expect("lsn num");
        let bdir = dir.join("bk");
        let out = crate::backup::run(&e, &bdir).expect("backup");
        assert_eq!(out.manifest["applied_lsn"], serde_json::json!(applied));
        assert_eq!(e.status()["checkpoint_lsn"], serde_json::json!(applied));
        e.close().expect("close");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression (audit A-L2): a tampered/NULL applied_lsn must fail the
    /// open loudly instead of silently replaying from zero.
    #[test]
    fn applied_lsn_tamper_is_corrupt() {
        let (dir, mut e) = fresh("tamper-lsn");
        e.execute(MutateRequest {
            actor: "test".into(),
            ops: vec![sql_upsert(1)],
            idem_key: None,
            response: None,
        })
        .expect("execute");
        let lsn = e.status()["applied_lsn"].as_u64().expect("lsn num");
        e.close().expect("close");
        drop(e); // release the fjall flock before reopening

        // tamper the watermark directly in the port (value is NOT NULL, so
        // the realistic tamper is non-numeric text)
        let port = TursoPort::open(dir.join("app.db")).expect("reopen port");
        port.exec("UPDATE _mdrv SET value = 'garbage' WHERE key = 'applied_lsn'")
            .expect("tamper");
        drop(port);

        let port = TursoPort::open(dir.join("app.db")).expect("reopen port");
        let err = match Engine::open(&dir, "tamper-lsn", Box::new(port), EngineConfig::default()) {
            Ok(_) => panic!("tampered watermark must refuse to open"),
            Err(e) => e,
        };
        assert!(
            err.to_string().to_lowercase().contains("applied_lsn"),
            "{err}"
        );
        let _ = lsn;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
