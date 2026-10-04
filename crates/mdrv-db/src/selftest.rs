//! Built-in crash-injection matrix (`mdrv-db selftest`).
//!
//! Spawns child copies of this same binary that write acknowledged entries
//! and abort at deterministic pipeline points (fault.rs) or after N writes.
//! The parent then reopens each database and asserts: every acked write is
//! durable, no acked write is lost (rolled-forward extras are allowed), and verify() is clean.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, ExitCode};

use mdrv_db::entry::{Op, PortValue, SqlKind};
use mdrv_db::MutateRequest;

const WRITES: u64 = 300;
const BLOB_EVERY: u64 = 17;
const MULTI_EVERY: u64 = 5;

pub fn run(root: &Path) -> ExitCode {
    let _ = std::fs::remove_dir_all(root);
    if let Err(e) = std::fs::create_dir_all(root) {
        eprintln!("cannot create {}: {e}", root.display());
        return ExitCode::from(1);
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cannot locate own binary: {e}");
            return ExitCode::from(1);
        }
    };
    let mut scenarios: Vec<(String, String, bool, u64)> = Vec::new();
    for fault in ["-", "after_persist", "after_apply"] {
        for fsync in [true, false] {
            scenarios.push((
                format!(
                    "fault={} fsync={}",
                    if fault == "-" { "none" } else { fault },
                    fsync
                ),
                fault.into(),
                fsync,
                0,
            ));
        }
    }
    scenarios.push(("random-abort-137 fsync=1".into(), "-".into(), true, 137));
    scenarios.push(("random-abort-233 fsync=0".into(), "-".into(), false, 233));

    let mut failed = 0usize;
    for (name, fault, fsync, abort_after) in &scenarios {
        let dir = root.join(name.replace(['=', ' '], "_"));
        let progress = dir.join("progress.log");
        let status = Command::new(&exe)
            .args([
                "selftest-child",
                &dir.to_string_lossy(),
                fault,
                if *fsync { "1" } else { "0" },
                &abort_after.to_string(),
                &progress.to_string_lossy(),
            ])
            .status();
        let crashed = match status {
            Ok(s) => !s.success(), // abort() = signal → non-success expected
            Err(e) => {
                println!("FAIL {name}: spawn error: {e}");
                failed += 1;
                continue;
            }
        };
        match judge(&dir, &progress) {
            Ok((acked, rows)) => {
                println!("PASS {name} (acked {acked}, rows {rows}, child crashed: {crashed})");
            }
            Err(e) => {
                println!("FAIL {name}: {e}");
                failed += 1;
            }
        }
    }
    if failed == 0 {
        println!("selftest: all {} scenarios passed", scenarios.len());
        ExitCode::SUCCESS
    } else {
        eprintln!("selftest: {failed} scenario(s) FAILED");
        ExitCode::from(1)
    }
}

/// Reopen the child's database and check durability invariants.
fn judge(dir: &Path, progress: &Path) -> Result<(usize, i64), String> {
    // A crash before the first ack (e.g. fault=after_persist) legitimately
    // leaves no progress log — that means zero acked writes, not an error.
    let acked = std::fs::read_to_string(progress)
        .map(|s| {
            s.lines()
                .filter_map(|l| l.trim().parse::<i64>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut engine =
        crate::engine_open::open_engine(dir, "selftest", true, false).map_err(|e| e.to_string())?;
    // query() returns a JSON array of named-row objects: [{col: value}, ...]
    let rows = engine
        .query("SELECT COUNT(*) AS n FROM kv WHERE id < 1000000", vec![])
        .map_err(|e| e.to_string())?;
    let n = rows
        .get(0)
        .and_then(|r| r.get("n"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    for id in &acked {
        let hit = engine
            .query(
                "SELECT COUNT(*) AS n FROM kv WHERE id = ?",
                vec![PortValue::Int(*id)],
            )
            .map_err(|e| e.to_string())?;
        let count = hit
            .get(0)
            .and_then(|r| r.get("n"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if count != 1 {
            engine.close().ok();
            return Err(format!("acked write {id} is NOT durable"));
        }
    }
    // Rolled-forward writes (committed+applied but unacked due to a crash
    // between apply and ack) are allowed: rows >= acked, never behind.
    let complete = n >= acked.len() as i64;
    let v = mdrv_db::verify::run(&engine).map_err(|e| e.to_string());
    engine.close().ok();
    let v = v?;
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    if !complete {
        return Err(format!(
            "primary rows {n} < acked {} (LOST writes)",
            acked.len()
        ));
    }
    if !ok {
        return Err(format!("verify anomalies: {:?}", v.get("anomalies")));
    }
    Ok((acked.len(), n))
}

/// Child process: writes WRITES acked entries, aborting per scenario.
pub fn child(dir: &Path, fault: &str, fsync: bool, abort_after: u64, progress: &Path) -> ExitCode {
    if fault != "-" {
        std::env::set_var("MDRV_DB_FAULT", fault);
    }
    let mut engine = match crate::engine_open::open_engine(dir, "selftest", fsync, true) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("child open: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) =
        engine.bootstrap(&["CREATE TABLE IF NOT EXISTS kv (id INTEGER PRIMARY KEY, v TEXT)".into()])
    {
        eprintln!("child bootstrap: {e}");
        return ExitCode::from(1);
    }
    for i in 0..WRITES {
        let mut ops = vec![Op::Sql {
            kind: SqlKind::Insert,
            table: "kv".into(),
            pk_col: "id".into(),
            columns: vec!["id".into(), "v".into()],
            values: vec![PortValue::Int(i as i64), PortValue::Text(format!("v{i}"))],
            pk: PortValue::Int(i as i64),
        }];
        if i % MULTI_EVERY == 0 {
            ops.push(Op::Sql {
                kind: SqlKind::Insert,
                table: "kv".into(),
                pk_col: "id".into(),
                columns: vec!["id".into(), "v".into()],
                values: vec![
                    PortValue::Int(1_000_000 + i as i64),
                    PortValue::Text("multi".into()),
                ],
                pk: PortValue::Int(1_000_000 + i as i64),
            });
        }
        if i % BLOB_EVERY == 0 {
            let bytes = format!("blob-{i}-{}", "x".repeat(64)).into_bytes();
            let (hash, _) = match engine.put_blob(&bytes) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("child blob: {e}");
                    return ExitCode::from(1);
                }
            };
            ops.push(Op::BlobPut { hash_hex: hash });
        }
        let req = MutateRequest {
            actor: "selftest".into(),
            ops,
            idem_key: None,
            response: None,
        };
        if let Err(e) = engine.execute(req) {
            eprintln!("child execute at {i}: {e}");
            return ExitCode::from(1);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(progress)
        {
            let _ = writeln!(f, "{i}");
            let _ = f.sync_data();
        }
        if abort_after > 0 && i + 1 == abort_after {
            std::process::abort();
        }
    }
    let _ = engine.close();
    ExitCode::SUCCESS
}
