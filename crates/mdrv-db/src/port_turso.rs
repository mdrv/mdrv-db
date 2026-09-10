//! Synchronous Turso data port for CLI operations + engine-open helper.
//!
//! Ported from the v1 napi crate (same logic, no napi). Used by
//! verify/backup/restore/checkpoint/prune, which all need the real data
//! layer — so they REQUIRE the owning app to be stopped (fjall + turso
//! file locks).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mdrv_db::port::{DataPort, QueryOut, Stmt};
use mdrv_db::{Engine, EngineConfig, PortValue};

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

impl DataPort for TursoPort {
    fn exec_tx(&self, stmts: &[Stmt]) -> Result<u64, String> {
        let mut conn = self.conn.lock().unwrap();
        self.rt.block_on(async {
            let tx = conn.transaction().await.map_err(|e| e.to_string())?;
            let mut total = 0u64;
            for s in stmts {
                let params = to_turso_params(&s.params)?;
                total += tx
                    .execute(&s.sql, params)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            tx.commit().await.map_err(|e| e.to_string())?;
            Ok(total)
        })
    }

    fn exec(&self, sql: &str) -> Result<u64, String> {
        let conn = self.conn.lock().unwrap();
        self.rt
            .block_on(async { conn.execute(sql, ()).await.map_err(|e| e.to_string()) })
    }

    fn query(&self, sql: &str, params: &[PortValue]) -> Result<QueryOut, String> {
        let conn = self.conn.lock().unwrap();
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

/// Resolve the live dir: `<dir>/live` when the fleet layout is used,
/// otherwise the dir itself (flat layout, v1-compatible).
pub fn live_dir(root: &Path) -> PathBuf {
    let live = root.join("live");
    if live.is_dir() {
        live
    } else {
        root.to_path_buf()
    }
}

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
