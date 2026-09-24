//! Fleet daemon: scheduled backups + retention for every `[db.*]` entry
//! in the fleet config (`mdrv-db daemon`).
//!
//! Increment 2 adds the admin REST + SSE + embedded console:
//! Each tick the run loop checks every job's next-due time (cron under
//! `[db.<slug>.backup]`); when due, one job runs:
//!
//!   try open engine → backup → verify backup → checkpoint → prune
//!   recovery dirs older than retention_days (newest always kept) →
//!   write daemon-state.json
//!
//! A held fjall lock (owning app process running) is a clean *skip*,
//! not an error — the daemon shares the box with app processes and
//! simply retries at the next cron occurrence.

use crate::engine_open::open_engine;
use crate::server;
use chrono::Local;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Clone)]
pub(crate) struct Job {
    pub(crate) slug: String,
    pub(crate) data_dir: PathBuf,
    pub(crate) name: Option<String>,
    pub(crate) cron: Option<String>,
    pub(crate) retention_days: Option<i64>,
    pub(crate) keep: Option<usize>,
    pub(crate) min_free: Option<u64>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct State {
    pub(crate) slug: String,
    pub(crate) last_run_ms: i64,
    pub(crate) last_result: String,
    pub(crate) detail: String,
    pub(crate) next_run_ms: i64,
    pub(crate) last_backup: Option<String>,
    pub(crate) applied_lsn: Option<i64>,
}

fn load_fleet() -> Result<(PathBuf, toml::Value), String> {
    let path = crate::config::default_path()
        .ok_or_else(|| "no config path (set $MDRV_DB_CONFIG or $HOME)".to_string())?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let cfg: toml::Value = toml::from_str(&text)
        .map_err(|e| format!("TOML parse error in {}: {e}", path.display()))?;
    Ok((path, cfg))
}

fn jobs(cfg: &toml::Value) -> Vec<Job> {
    let mut out = Vec::new();
    let Some(dbs) = cfg.get("db").and_then(|v| v.as_table()) else {
        return out;
    };
    let data_root = cfg
        .get("sched")
        .and_then(|s| s.get("data_root"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    for (slug, t) in dbs {
        let Some(t) = t.as_table() else { continue };
        let data_dir = t
            .get("data_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .or_else(|| data_root.as_ref().map(|r| r.join(slug)))
            .unwrap_or_else(|| PathBuf::from("/x/db").join(slug));
        let name = t.get("name").and_then(|v| v.as_str()).map(String::from);
        let (cron, retention_days, keep, min_free) = match t.get("backup") {
            Some(b) => (
                b.get("cron").and_then(|v| v.as_str()).map(String::from),
                b.get("retention_days").and_then(|v| v.as_integer()),
                b.get("keep")
                    .and_then(|v| v.as_integer())
                    .map(|v| v.max(0) as usize),
                b.get("min_free").and_then(|v| {
                    v.as_str()
                        .and_then(parse_size)
                        .or_else(|| v.as_integer().map(|i| i.max(0) as u64))
                }),
            ),
            None => (None, None, None, None),
        };
        out.push(Job {
            slug: slug.clone(),
            data_dir,
            name,
            cron,
            retention_days,
            keep,
            min_free,
        });
    }
    out
}

/// Accepts the common aliases (hourly/daily/weekly/monthly), standard
/// 5-field cron (minutes first), or the crate's 6/7-field form.
fn parse_sched(expr: &str) -> Result<cron::Schedule, String> {
    let lower = expr.trim().to_ascii_lowercase();
    let norm = match lower.as_str() {
        "hourly" => "0 0 * * * *".to_string(),
        "daily" => "0 0 3 * * *".to_string(),
        "weekly" => "0 0 3 * * sun".to_string(),
        "monthly" => "0 0 3 1 * *".to_string(),
        e if e.split_whitespace().count() == 5 => format!("0 {e}"),
        e => e.to_string(),
    };
    norm.parse().map_err(|e| format!("bad cron '{expr}': {e}"))
}

fn next_after(expr: &str, from: chrono::DateTime<Local>) -> Result<i64, String> {
    let sched = parse_sched(expr)?;
    sched
        .after(&from)
        .next()
        .map(|dt| dt.timestamp_millis())
        .ok_or_else(|| format!("cron '{expr}' never fires"))
}

pub(crate) fn ts(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "?".into())
}

/// Backup dirs are `<ms>-offline` / `<ms>-daemon`; the leading digits are
/// the creation timestamp.
pub(crate) fn backup_ts(name: &str) -> Option<i64> {
    let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() == name.len() {
        return None;
    }
    if !name[digits.len()..].starts_with('-') {
        return None;
    }
    digits.parse().ok()
}

/// Default minimum free space on the backup volume (1 GiB);
/// `min_free = 0` disables the guard.
const DEFAULT_MIN_FREE: u64 = 1 << 30;

/// Parse `"10G"`, `"500M"`, `"16K"`, `"64B"` or a bare byte count.
fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim();
    let (num, mult) = match t.chars().last()? {
        'K' | 'k' => (&t[..t.len() - 1], 1024u64),
        'M' | 'm' => (&t[..t.len() - 1], 1024 * 1024),
        'G' | 'g' => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        'B' | 'b' => (&t[..t.len() - 1], 1),
        _ => (t, 1),
    };
    num.trim()
        .parse::<u64>()
        .ok()
        .map(|n| n.saturating_mul(mult))
}

fn fmt_bytes(n: u64) -> String {
    let g = n as f64 / (1 << 30) as f64;
    if g >= 1.0 {
        return format!("{g:.1}G");
    }
    format!("{:.0}M", n as f64 / (1 << 20) as f64)
}

/// Pre-flight disk guard (daemon-managed jobs only): free space on the
/// volume holding `recovery/` must be >= floor, else the job is skipped.
fn disk_guard(j: &Job) -> Option<String> {
    let floor = j.min_free.unwrap_or(DEFAULT_MIN_FREE);
    if floor == 0 {
        return None;
    }
    let target = j.data_dir.join("recovery");
    let probe = if target.is_dir() {
        target
    } else {
        j.data_dir.clone()
    };
    let avail = fs2::available_space(&probe).ok()?;
    if avail < floor {
        Some(format!(
            "insufficient free: {} < {} (min_free)",
            fmt_bytes(avail),
            fmt_bytes(floor)
        ))
    } else {
        None
    }
}

/// Delete expired backup dirs, always keeping the newest. Returns
/// (kept, removed).
pub(crate) fn prune_backups(
    recovery: &Path,
    retention_days: Option<i64>,
    keep: Option<usize>,
) -> Result<(usize, usize), String> {
    let mut dirs: Vec<(i64, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(recovery)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let p = e.path();
        if p.is_dir() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if let Some(ts) = backup_ts(name) {
                    dirs.push((ts, p));
                }
            }
        }
    }
    if dirs.is_empty() {
        return Ok((0, 0));
    }
    dirs.sort_by_key(|(ts, _)| *ts);
    let cutoff = retention_days.map(|d| mdrv_db::now_ms() - d.saturating_mul(86_400_000));
    // keep=N caps the number of retained backups; without it, retention_days
    // alone governs (everything unexpired stays).
    let deletable_keep = match keep {
        Some(k) => dirs.len().saturating_sub(k.max(1)),
        None => 0,
    };
    let mut removed = 0usize;
    for (i, (ts, p)) in dirs.iter().enumerate() {
        // Union of the two deletion rules; the newest dir is always exempt.
        let expired = cutoff.is_some_and(|c| *ts < c) && i + 1 < dirs.len();
        let excess = i < deletable_keep;
        if expired || excess {
            std::fs::remove_dir_all(p).map_err(|e| format!("prune {}: {e}", p.display()))?;
            removed += 1;
        }
    }
    Ok((dirs.len() - removed, removed))
}

pub(crate) fn count_backups(recovery: &Path) -> usize {
    std::fs::read_dir(recovery)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.path().is_dir() && e.file_name().to_str().and_then(backup_ts).is_some()
                })
                .count()
        })
        .unwrap_or(0)
}

type JobOut = (String, String, Option<i64>, Option<String>);

/// (result, detail, applied_lsn, backup dir) — result is
/// ok | skipped | error | not_initialized.
fn run_job(j: &Job) -> JobOut {
    if !j.data_dir.join("live/fjall").is_dir() {
        return (
            "not_initialized".into(),
            "no live/fjall — run `mdrv-db init` first".into(),
            None,
            None,
        );
    }
    let name = crate::resolve_name(&j.data_dir, &j.name);
    let mut engine = match open_engine(&j.data_dir, &name, true, false) {
        Ok(e) => e,
        Err(e) => return ("skipped".into(), e, None, None),
    };
    let dest = j
        .data_dir
        .join("recovery")
        .join(format!("{}-daemon", mdrv_db::now_ms()));
    let dest_str = dest.display().to_string();
    let mut lsn = None;
    let step = mdrv_db::backup::run(&engine, &dest)
        .map_err(|e| e.to_string())
        .and_then(|b| {
            lsn = b.manifest.get("applied_lsn").and_then(|v| v.as_i64());
            mdrv_db::verify::run_backup(&dest).map_err(|e| e.to_string())
        })
        .and_then(|v| {
            if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                let l = lsn.map(|x| x.to_string()).unwrap_or_else(|| "?".into());
                engine
                    .checkpoint(false)
                    .map_err(|e| e.to_string())
                    .map(|n| format!("backup → {dest_str} (lsn {l}, verified, checkpoint {n})"))
            } else {
                Err(format!("backup verify FAILED: {v}"))
            }
        });
    let close_err = engine.close().err();
    match (step, close_err) {
        (Ok(d), None) => ("ok".into(), d, lsn, Some(dest_str)),
        (Ok(d), Some(e)) => (
            "error".into(),
            format!("{d}; close: {e}"),
            lsn,
            Some(dest_str),
        ),
        (Err(e), _) => ("error".into(), e, None, None),
    }
}

fn save_state(j: &Job, st: &State) {
    if let Ok(text) = serde_json::to_string_pretty(st) {
        let _ = std::fs::write(j.data_dir.join("daemon-state.json"), text);
    }
}

pub(crate) fn load_state(j: &Job) -> Option<State> {
    let text = std::fs::read_to_string(j.data_dir.join("daemon-state.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Run one job now (used by --once and by the loop when due), then
/// retention-prune and persist the state file.
pub(crate) fn execute(j: &Job) -> State {
    let started = mdrv_db::now_ms();
    let (result, detail, lsn, backup) = match disk_guard(j) {
        Some(msg) => ("skipped".into(), msg, None, None),
        None => run_job(j),
    };
    let mut detail = detail;
    if j.retention_days.is_some() || j.keep.is_some() {
        if let Ok((kept, removed)) =
            prune_backups(&j.data_dir.join("recovery"), j.retention_days, j.keep)
        {
            if removed > 0 {
                detail = format!("{detail}; pruned {removed} old backup(s), kept {kept}");
            }
        }
    }
    let next = j
        .cron
        .as_deref()
        .and_then(|c| next_after(c, Local::now()).ok())
        .unwrap_or(started + 86_400_000);
    let st = State {
        slug: j.slug.clone(),
        last_run_ms: started,
        last_result: result,
        detail,
        next_run_ms: next,
        last_backup: backup,
        applied_lsn: lsn,
    };
    save_state(j, &st);
    st
}

fn validate(js: &[Job]) -> Result<(), String> {
    for j in js {
        if let Some(c) = &j.cron {
            parse_sched(c).map_err(|e| format!("[{}] {e}", j.slug))?;
        }
    }
    Ok(())
}

pub fn run(
    slug: Option<&str>,
    once: bool,
    interval_secs: u64,
    bind: Option<String>,
    port: Option<u16>,
    console: Option<PathBuf>,
) -> ExitCode {
    let (cfg_path, cfg) = match load_fleet() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let mut js: Vec<Job> = jobs(&cfg);
    if let Some(f) = slug {
        js.retain(|j| j.slug == f);
        if js.is_empty() {
            eprintln!("no fleet entry '{f}' in {}", cfg_path.display());
            return ExitCode::from(1);
        }
    }
    if let Err(e) = validate(&js) {
        eprintln!("{e}");
        return ExitCode::from(1);
    }
    let sched: Vec<&Job> = js.iter().filter(|j| j.cron.is_some()).collect();
    if sched.is_empty() {
        println!("no [db.*] entries with backup.cron — nothing to schedule");
        return ExitCode::SUCCESS;
    }
    if once {
        let mut errors = 0usize;
        for j in &sched {
            let st = execute(j);
            println!("[{}] {} {}", j.slug, st.last_result, st.detail);
            if st.last_result == "error" {
                errors += 1;
            }
        }
        return if errors > 0 {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }
    // Server mode: scheduler loop + admin REST + SSE + console.
    let data_root = cfg
        .get("sched")
        .and_then(|s| s.get("data_root"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/x/db"));
    // [daemon] host/port in the fleet config are the defaults; flags win.
    let daemon_cfg = cfg.get("daemon");
    let bind = bind.or_else(|| {
        daemon_cfg
            .and_then(|d| d.get("host"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    });
    let port = port.or_else(|| {
        daemon_cfg
            .and_then(|d| d.get("port"))
            .and_then(|v| v.as_integer())
            .map(|p| p as u16)
    });
    match server::serve(server::Opts {
        bind,
        port,
        console,
        interval_secs,
        jobs: sched.into_iter().cloned().collect(),
        data_root,
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

pub fn status(slug: Option<&str>, json: bool) -> ExitCode {
    let (cfg_path, cfg) = match load_fleet() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let mut js: Vec<Job> = jobs(&cfg);
    if let Some(f) = slug {
        js.retain(|j| j.slug == f);
        if js.is_empty() {
            eprintln!("no fleet entry '{f}' in {}", cfg_path.display());
            return ExitCode::from(1);
        }
    }
    if let Err(e) = validate(&js) {
        eprintln!("{e}");
        return ExitCode::from(1);
    }
    if js.is_empty() {
        println!("(no [db.*] entries) [{cfg_path:?}]");
        return ExitCode::SUCCESS;
    }
    if json {
        let rows: Vec<serde_json::Value> = js
            .iter()
            .map(|j| {
                let st = load_state(j);
                serde_json::json!({
                    "slug": j.slug,
                    "name": j.name,
                    "data_dir": j.data_dir.display().to_string(),
                    "cron": j.cron,
                    "initialized": j.data_dir.join("live/fjall").is_dir(),
                    "backups": count_backups(&j.data_dir.join("recovery")),
                    "last_run_ms": st.as_ref().map(|s| s.last_run_ms),
                    "last_result": st
                        .as_ref()
                        .map(|s| s.last_result.clone())
                        .unwrap_or_else(|| "never".into()),
                    "detail": st.as_ref().map(|s| s.detail.clone()).unwrap_or_default(),
                    "next_run_ms": st.as_ref().map(|s| s.next_run_ms),
                    "applied_lsn": st.as_ref().and_then(|s| s.applied_lsn),
                    "last_backup": st.as_ref().and_then(|s| s.last_backup.clone()),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into())
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "{:<18} {:<16} {:<20} {:<20} {:>7}  detail",
        "slug", "result", "last", "next", "backups"
    );
    for j in &js {
        let (result, last, next, detail) = match load_state(j) {
            Some(s) => {
                let mut d = s.detail;
                if let Some(b) = s.last_backup {
                    let base = Path::new(&b)
                        .file_name()
                        .map(|x| x.to_string_lossy().into_owned())
                        .unwrap_or(b);
                    d = format!("{d} [{base}]");
                }
                if let Some(l) = s.applied_lsn {
                    d = format!("{d} lsn={l}");
                }
                (s.last_result, ts(s.last_run_ms), ts(s.next_run_ms), d)
            }
            None => ("never".into(), "-".into(), "-".into(), String::new()),
        };
        let backups = count_backups(&j.data_dir.join("recovery"));
        println!(
            "{:<18} {result:<16} {last:<20} {next:<20} {backups:>7}  {detail}",
            j.slug
        );
    }
    ExitCode::SUCCESS
}

pub fn install() -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cannot resolve executable path: {e}");
            return ExitCode::from(1);
        }
    };
    let config_dir = match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => match std::env::var("HOME") {
            Ok(h) => PathBuf::from(h).join(".config"),
            Err(_) => {
                eprintln!("cannot resolve config dir: no XDG_CONFIG_HOME or HOME");
                return ExitCode::from(1);
            }
        },
    };
    let dir = config_dir.join("systemd/user");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create {dir:?}: {e}");
        return ExitCode::from(1);
    }
    let unit = format!(
        "[Unit]\nDescription=mdrv-db fleet daemon (scheduled backups)\nAfter=local-fs.target\n\n\
         [Service]\nExecStart={} daemon run\nRestart=on-failure\nRestartSec=10\n\n\
         [Install]\nWantedBy=default.target\n",
        exe.display()
    );
    let path = dir.join("mdrv-db.service");
    match std::fs::write(&path, unit) {
        Ok(_) => {
            println!("unit written: {}", path.display());
            println!("enable with:  systemctl --user daemon-reload");
            println!("              systemctl --user enable --now mdrv-db");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot write {path:?}: {e}");
            ExitCode::from(1)
        }
    }
}
