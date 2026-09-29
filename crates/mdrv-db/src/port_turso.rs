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

#[cfg(test)]
mod tests {
    use super::TursoPort;
    use crate::port::DataPort as _;
    use crate::{Engine, EngineConfig, MutateRequest, Op, PortValue, SqlKind};

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
}
