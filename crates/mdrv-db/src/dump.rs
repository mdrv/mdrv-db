//! Offline viewer for the Fjall envelope (ported from v1's mdrv-db-dump bin).
//!
//! Opens <data-dir>/live/fjall DIRECTLY (no engine, no port) — run against
//! a stopped owner or a backup copy (Fjall's file lock still applies).
//! Exit 0 = clean; 1 = usage/IO error; 2 = completed WITH decode warnings.

use std::path::Path;
use std::process::ExitCode;

use clap::{Args, Subcommand, ValueHint};
use serde_json::Value;

use mdrv_db::entry::{Op, ReportEntry, SqlKind, TxEntry};

const MARK_NAMES: [&str; 3] = ["pending", "committed", "aborted"];

pub struct Open {
    pub db: fjall::Database,
    pub wal: fjall::Keyspace,
    pub marks: fjall::Keyspace,
    pub report: fjall::Keyspace,
    pub idem: fjall::Keyspace,
    pub meta: fjall::Keyspace,
}

pub fn open(dir: &Path) -> Result<Open, String> {
    let live = if dir.join("live").is_dir() {
        dir.join("live")
    } else {
        dir.to_path_buf()
    };
    let fjall_dir = live.join("fjall");
    if !fjall_dir.is_dir() {
        return Err(format!("{fjall_dir:?} does not exist (not initialized or wrong dir)"));
    }
    let db = fjall::Database::builder(&fjall_dir)
        .open()
        .map_err(|e| format!("cannot open {fjall_dir:?}: {e} (owner running? wrong dir?)"))?;
    let open = |name| db.keyspace(name, fjall::KeyspaceCreateOptions::default);
    let (Ok(wal), Ok(marks), Ok(report), Ok(idem), Ok(meta)) = (
        open("wal"),
        open("marks"),
        open("report"),
        open("idem"),
        open("meta"),
    ) else {
        return Err(format!(
            "keyspaces missing under {fjall_dir:?} — not an mdrv database?"
        ));
    };
    Ok(Open {
        db,
        wal,
        marks,
        report,
        idem,
        meta,
    })
}

// ------------------------------------------------------------------ args

#[derive(Args)]
pub struct TimeFilter {
    /// Epoch ms, or "YYYY-MM-DD[ HH:MM[:SS]]" (UTC)
    #[arg(long)]
    pub since: Option<String>,
    /// Epoch ms, or "YYYY-MM-DD[ HH:MM[:SS]]" (UTC)
    #[arg(long)]
    pub until: Option<String>,
}

#[derive(Args)]
pub struct Common {
    /// Rust regex matched against the decoded line (prefix (?i) for case-insensitive)
    #[arg(long)]
    pub filter: Option<String>,
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
    /// Newest first
    #[arg(long)]
    pub reverse: bool,
    /// One JSON object per line
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum DumpCmd {
    /// Transaction entries (joined with marks)
    Wal {
        /// Database directory (contains live/)
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: std::path::PathBuf,
        #[command(flatten)]
        time: TimeFilter,
        /// Rust regex on the actor field
        #[arg(long)]
        actor: Option<String>,
        #[arg(long, value_parser = ["pending", "committed", "aborted"])]
        status: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Report log (engine events)
    Report {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: std::path::PathBuf,
        #[command(flatten)]
        time: TimeFilter,
        /// Rust regex on the event name
        #[arg(long)]
        event: Option<String>,
        /// 0=debug 1=info 2=warn 3=error
        #[arg(long)]
        level: Option<u8>,
        #[command(flatten)]
        common: Common,
    },
    /// Commit marks
    Marks {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: std::path::PathBuf,
        #[arg(long, value_parser = ["pending", "committed", "aborted"])]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Idempotency records
    Idem {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: std::path::PathBuf,
        #[command(flatten)]
        time: TimeFilter,
        #[command(flatten)]
        common: Common,
    },
    /// Meta keyspace
    Meta {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: std::path::PathBuf,
    },
    /// Raw keyspace dump
    Raw {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: std::path::PathBuf,
        /// Keyspace name
        keyspace: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        filter: Option<String>,
    },
}

pub fn run(cmd: DumpCmd) -> ExitCode {
    match cmd {
        DumpCmd::Wal {
            data_dir,
            time,
            actor,
            status,
            common,
        } => {
            let o = match open(&data_dir) {
                Ok(o) => o,
                Err(e) => return usage(e),
            };
            let f = match to_f(&time, &actor, &None, status.as_deref(), None, &common) {
                Ok(f) => f,
                Err(e) => return usage(e),
            };
            wal(&o, &data_dir, &f)
        }
        DumpCmd::Report {
            data_dir,
            time,
            event,
            level,
            common,
        } => {
            let o = match open(&data_dir) {
                Ok(o) => o,
                Err(e) => return usage(e),
            };
            let f = match to_f(&time, &None, &event, None, level, &common) {
                Ok(f) => f,
                Err(e) => return usage(e),
            };
            report(&o, &data_dir, &f)
        }
        DumpCmd::Marks {
            data_dir,
            status,
            limit,
            json,
        } => {
            let o = match open(&data_dir) {
                Ok(o) => o,
                Err(e) => return usage(e),
            };
            let f = F {
                status: status.map(|s| mark_byte(&s)),
                limit,
                json,
                ..F::default()
            };
            marks(&o, &f)
        }
        DumpCmd::Idem {
            data_dir,
            time,
            common,
        } => {
            let o = match open(&data_dir) {
                Ok(o) => o,
                Err(e) => return usage(e),
            };
            let f = match to_f(&time, &None, &None, None, None, &common) {
                Ok(f) => f,
                Err(e) => return usage(e),
            };
            idem(&o, &f)
        }
        DumpCmd::Meta { data_dir } => {
            let o = match open(&data_dir) {
                Ok(o) => o,
                Err(e) => return usage(e),
            };
            meta(&o)
        }
        DumpCmd::Raw {
            data_dir,
            keyspace,
            limit,
            filter,
        } => {
            let o = match open(&data_dir) {
                Ok(o) => o,
                Err(e) => return usage(e),
            };
            let re = match filter.map(|s| regex::Regex::new(&s)).transpose() {
                Ok(r) => r,
                Err(e) => return usage(e.to_string()),
            };
            raw(&o, &keyspace, limit, re.as_ref())
        }
    }
}

// --------------------------------------------------------------- filters

#[derive(Default)]
pub struct F {
    since: Option<i64>,
    until: Option<i64>,
    filter: Option<regex::Regex>,
    actor: Option<regex::Regex>,
    event: Option<regex::Regex>,
    status: Option<u8>,
    level: Option<u8>,
    limit: usize,
    reverse: bool,
    json: bool,
}

#[allow(clippy::too_many_arguments)]
fn to_f(
    time: &TimeFilter,
    actor: &Option<String>,
    event: &Option<String>,
    status: Option<&str>,
    level: Option<u8>,
    common: &Common,
) -> Result<F, String> {
    let since = match &time.since {
        Some(s) => parse_time(s).ok_or("bad --since")?,
        None => 0,
    };
    let since = (time.since.is_some()).then_some(since);
    let until = match &time.until {
        Some(s) => parse_time(s).ok_or("bad --until")?,
        None => 0,
    };
    let until = (time.until.is_some()).then_some(until);
    Ok(F {
        since,
        until,
        filter: common
            .filter
            .as_deref()
            .map(regex::Regex::new)
            .transpose()
            .map_err(|e| e.to_string())?,
        actor: actor
            .as_deref()
            .map(regex::Regex::new)
            .transpose()
            .map_err(|e| e.to_string())?,
        event: event
            .as_deref()
            .map(regex::Regex::new)
            .transpose()
            .map_err(|e| e.to_string())?,
        status: status.map(mark_byte),
        level,
        limit: common.limit,
        reverse: common.reverse,
        json: common.json,
    })
}

fn mark_byte(s: &str) -> u8 {
    match s {
        "pending" => 1,
        "committed" => 2,
        _ => 3,
    }
}

/// Epoch milliseconds, or "YYYY-MM-DD[ HH:MM[:SS]]" (UTC).
fn parse_time(s: &str) -> Option<i64> {
    if let Ok(ms) = s.parse::<i64>() {
        return Some(ms);
    }
    let b: Vec<i64> = s
        .split(['-', ' ', 'T', ':'])
        .filter_map(|p| p.parse::<i64>().ok())
        .collect();
    let (y, m, d) = (*b.first()?, *b.get(1)?, *b.get(2)?);
    // days from civil (Hinnant), UTC
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let hh = b.get(3).copied().unwrap_or(0);
    let mi = b.get(4).copied().unwrap_or(0);
    let ss = b.get(5).copied().unwrap_or(0);
    Some((days * 86_400 + hh * 3_600 + mi * 60 + ss) * 1_000)
}

fn in_window(t: i64, f: &F) -> bool {
    f.since.is_none_or(|s| t >= s) && f.until.is_none_or(|u| t <= u)
}

fn keep(line: &str, f: &F) -> bool {
    f.filter.as_ref().is_none_or(|re| re.is_match(line))
}

// ------------------------------------------------------------------ cmds

pub fn info(dir: &Path) -> ExitCode {
    let o = match open(dir) {
        Ok(o) => o,
        Err(e) => return usage(e),
    };
    println!("database: {}", dir.display());
    for name in o.db.list_keyspace_names() {
        if let Ok(ks) = o.db.keyspace(&name, fjall::KeyspaceCreateOptions::default) {
            println!(
                "  {name:<10} entries≈{:<9} disk={:<9} tables={}",
                ks.approximate_len(),
                format_bytes(ks.disk_space()),
                ks.table_count(),
            );
        }
    }
    for k in ["applied_lsn", "checkpoint_lsn", "db_name", "entry_schema"] {
        if let Ok(Some(v)) = o.meta.get(k.as_bytes()) {
            println!("meta: {k} = {}", decode_meta(&v));
        }
    }
    let (files, bytes) = walk_blobs(&dir.join("live").join("blobs"));
    println!("blobs: {files} file(s), {}", format_bytes(bytes));
    ExitCode::SUCCESS
}

fn wal(o: &Open, dir: &Path, f: &F) -> ExitCode {
    let _ = dir;
    let mut shown = 0usize;
    let mut warn = 0usize;
    let iter: Box<dyn Iterator<Item = fjall::Guard>> = if f.reverse {
        Box::new(o.wal.iter().rev())
    } else {
        Box::new(o.wal.iter())
    };
    for item in iter {
        let (k, v) = match item.into_inner() {
            Ok(kv) => kv,
            Err(e) => {
                eprintln!("read error: {e}");
                warn += 1;
                continue;
            }
        };
        let entry: TxEntry = match postcard::from_bytes(&v) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("lsn {}: UNDECODABLE ({e})", be_u64(&k));
                warn += 1;
                continue;
            }
        };
        if !in_window(entry.issued_at, f)
            || !f.actor.as_ref().is_none_or(|re| re.is_match(&entry.actor))
        {
            continue;
        }
        let st = o
            .marks
            .get(k.as_slice())
            .ok()
            .flatten()
            .map(|m| m[0])
            .unwrap_or(0);
        if f.status.is_some_and(|s| s != st) {
            continue;
        }
        let name = mark_name(st);
        let (line, line_str) = if f.json {
            let line = serde_json::json!({
                "lsn": entry.lsn, "status": name, "issued_at": entry.issued_at,
                "actor": entry.actor, "ops": serde_json::to_value(&entry.ops).unwrap_or(Value::Null),
            });
            (line.to_string(), line)
        } else {
            let ops: Vec<String> = entry.ops.iter().map(fmt_op).collect();
            let line = format!(
                "#{:<6} {:<9} {} {:<14} {}",
                entry.lsn,
                name,
                fmt_time(entry.issued_at),
                truncate(&entry.actor, 14),
                if ops.is_empty() {
                    "(empty)".into()
                } else {
                    ops.join(" | ")
                },
            );
            (line.clone(), Value::String(line))
        };
        let _ = line_str;
        if !keep(&line, f) {
            continue;
        }
        println!("{line}");
        shown += 1;
        if shown >= f.limit {
            break;
        }
    }
    finish(warn)
}

fn fmt_op(op: &Op) -> String {
    match op {
        Op::Sql {
            kind,
            table,
            values,
            ..
        } => {
            let k = match kind {
                SqlKind::Insert => "ins",
                SqlKind::Upsert => "upsert",
                SqlKind::Update => "update",
                SqlKind::Delete => "del",
            };
            format!("{k} {table}({})", values.len())
        }
        Op::BlobPut { hash_hex } => format!("blob+ {}", &hash_hex[..hash_hex.len().min(12)]),
        Op::BlobDrop { hash_hex } => format!("blob- {}", &hash_hex[..hash_hex.len().min(12)]),
    }
}

fn report(o: &Open, dir: &Path, f: &F) -> ExitCode {
    let _ = dir;
    let lvl = |l: u8| {
        ["debug", "info", "warn", "error"]
            .get(l as usize)
            .copied()
            .unwrap_or("?")
    };
    let mut shown = 0usize;
    let mut warn = 0usize;
    let iter: Box<dyn Iterator<Item = fjall::Guard>> = if f.reverse {
        Box::new(o.report.iter().rev())
    } else {
        Box::new(o.report.iter())
    };
    for item in iter {
        let (k, v) = match item.into_inner() {
            Ok(kv) => kv,
            Err(e) => {
                eprintln!("read error: {e}");
                warn += 1;
                continue;
            }
        };
        if k.len() < 8 || !in_window(be_u64(&k[..8]) as i64, f) {
            continue;
        }
        let entry: ReportEntry = match postcard::from_bytes(&v) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("UNDECODABLE report row ({e})");
                warn += 1;
                continue;
            }
        };
        if !in_window(entry.ts_ms, f)
            || f.level.is_some_and(|l| l != entry.level)
            || !f.event.as_ref().is_none_or(|re| re.is_match(&entry.event))
        {
            continue;
        }
        let line = if f.json {
            serde_json::json!({
                "ts_ms": entry.ts_ms, "level": entry.level,
                "event": entry.event, "data": entry.data_json,
            })
            .to_string()
        } else {
            format!(
                "{} [{:<5}] {:<22} {}",
                fmt_time(entry.ts_ms),
                lvl(entry.level),
                entry.event,
                entry.data_json
            )
        };
        if !keep(&line, f) {
            continue;
        }
        println!("{line}");
        shown += 1;
        if shown >= f.limit {
            break;
        }
    }
    finish(warn)
}

fn marks(o: &Open, f: &F) -> ExitCode {
    let mut shown = 0usize;
    for item in o.marks.iter() {
        let (k, v) = match item.into_inner() {
            Ok(kv) => kv,
            Err(_) => continue,
        };
        let st = v.first().copied().unwrap_or(0);
        if f.status.is_some_and(|s| s != st) {
            continue;
        }
        let name = mark_name(st);
        let hash = hex_head(v.get(1..9).unwrap_or(&[]));
        if f.json {
            println!(
                "{}",
                serde_json::json!({"lsn": be_u64(&k), "status": name, "hash": hash})
            );
        } else {
            println!("{:<20} {name:<9} hash={hash}", be_u64(&k));
        }
        shown += 1;
        if shown >= f.limit {
            break;
        }
    }
    ExitCode::SUCCESS
}

fn idem(o: &Open, f: &F) -> ExitCode {
    let mut shown = 0usize;
    let mut warn = 0usize;
    for item in o.idem.iter() {
        let (k, v) = match item.into_inner() {
            Ok(kv) => kv,
            Err(_) => {
                warn += 1;
                continue;
            }
        };
        let state = v.first().copied().unwrap_or(0);
        let (lsn, ts) = if v.len() >= 17 {
            (be_u64(&v[1..9]), Some(be_u64(&v[9..17]) as i64))
        } else {
            (0, None)
        };
        if ts.is_some_and(|t| !in_window(t, f)) {
            continue;
        }
        let key = String::from_utf8_lossy(&k).into_owned();
        let name = match state {
            1 => "processing",
            2 => "completed",
            _ => "?",
        };
        let line = if f.json {
            serde_json::json!({"key": key, "state": name, "lsn": lsn, "ts_ms": ts}).to_string()
        } else {
            format!(
                "{key} {name:<10} lsn={lsn} {}",
                ts.map(fmt_time).unwrap_or_default()
            )
        };
        if !keep(&line, f) {
            continue;
        }
        println!("{line}");
        shown += 1;
        if shown >= f.limit {
            break;
        }
    }
    finish(warn)
}

fn meta(o: &Open) -> ExitCode {
    for item in o.meta.iter() {
        let Ok((k, v)) = item.into_inner() else {
            continue;
        };
        println!("{:<16} {}", String::from_utf8_lossy(&k), decode_meta(&v));
    }
    ExitCode::SUCCESS
}

fn raw(o: &Open, name: &str, limit: usize, re: Option<&regex::Regex>) -> ExitCode {
    let Ok(ks) = o.db.keyspace(name, fjall::KeyspaceCreateOptions::default) else {
        eprintln!("no keyspace {name:?}");
        return ExitCode::from(1);
    };
    let mut shown = 0usize;
    for item in ks.iter() {
        let Ok((k, v)) = item.into_inner() else {
            continue;
        };
        let key = String::from_utf8_lossy(&k);
        let val = String::from_utf8_lossy(&v);
        let line = format!("{key} = {val}");
        if re.is_some_and(|r| !r.is_match(&line)) {
            continue;
        }
        println!("{line}");
        shown += 1;
        if shown >= limit {
            break;
        }
    }
    ExitCode::SUCCESS
}

fn usage(msg: String) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

fn finish(warn: usize) -> ExitCode {
    if warn > 0 {
        eprintln!(
            "{warn} decode warning(s) — treat with suspicion, run the owner's admin-RPC `verify`"
        );
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

// -------------------------------------------------------------- helpers

fn mark_name(st: u8) -> &'static str {
    MARK_NAMES
        .get((st.saturating_sub(1)) as usize)
        .copied()
        .unwrap_or("orphan")
}

fn be_u64(b: &[u8]) -> u64 {
    let mut arr = [0u8; 8];
    arr[..b.len().min(8)].copy_from_slice(&b[..b.len().min(8)]);
    u64::from_be_bytes(arr)
}

fn hex_head(b: &[u8]) -> String {
    b.iter().take(8).map(|x| format!("{x:02x}")).collect()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn fmt_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        sod / 3_600,
        sod % 3_600 / 60,
        sod % 60
    )
}

fn format_bytes(n: u64) -> String {
    for (div, unit) in [(1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")] {
        if n >= div {
            return format!("{:.1}{unit}", n as f64 / div as f64);
        }
    }
    format!("{n}B")
}

fn decode_meta(v: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(v) {
        return s.to_owned();
    }
    if let Ok(s) = postcard::from_bytes::<String>(v) {
        return s;
    }
    if v.len() == 8 {
        return be_u64(v).to_string();
    }
    hex_head(v)
}

fn walk_blobs(root: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let (f, b) = walk_blobs(&p);
                files += f;
                bytes += b;
            } else {
                files += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (files, bytes)
}
