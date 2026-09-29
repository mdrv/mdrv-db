//! Fleet config inspection (`mdrv-db config`).
//!
//! Reads the same TOML registry the daemon and TS packages use
//! (`$MDRV_DB_CONFIG` or ~/.config/mdrv-db/config.toml) and renders or
//! checks it. The config is the single source of truth: every tunable
//! (paths, cadences, retention, RPC endpoints, token env names,
//! per-DB settings) is a property here.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

static OVERRIDE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// `--config <path>` global flag; takes precedence over $MDRV_DB_CONFIG.
pub fn set_override(p: Option<PathBuf>) {
    let _ = OVERRIDE.set(p);
}

pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = OVERRIDE.get().and_then(|o| o.as_ref()) {
        return Some(p.clone());
    }
    if let Ok(p) = std::env::var("MDRV_DB_CONFIG") {
        return Some(PathBuf::from(p));
    }
    config_home().map(|d| d.join("mdrv-db/config.toml"))
}

/// Per-OS data home: `$XDG_DATA_HOME`/`~/.local/share` (linux & friends),
/// `~/Library/Application Support` (macOS), `%LOCALAPPDATA%` (Windows).
fn data_home() -> Option<PathBuf> {
    match std::env::consts::OS {
        "windows" => std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        "macos" => {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
        }
        _ => std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))),
    }
}

/// Fleet data root used when nothing is configured — the per-OS standard
/// location, never a hardcoded path. Falls back to a relative dir when no
/// home is resolvable (headless/CI).
pub fn default_data_root() -> PathBuf {
    data_home().unwrap_or_default().join("mdrv-db")
}

/// Slug data dir used when neither `data_dir` nor `sched.data_root` is set.
pub fn default_data_dir(slug: &str) -> PathBuf {
    default_data_root().join(slug)
}

/// Per-OS config home: `$XDG_CONFIG_HOME`/`~/.config`,
/// `~/Library/Application Support` (macOS), `%APPDATA%` (Windows).
pub fn config_home() -> Option<PathBuf> {
    match std::env::consts::OS {
        "windows" => std::env::var_os("APPDATA").map(PathBuf::from),
        "macos" => {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
        }
        _ => std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))),
    }
}

fn load(file: &Path) -> Result<toml::Value, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    toml::from_str(&text).map_err(|e| format!("TOML parse error in {}: {e}", file.display()))
}

/// Find the engine name (fleet slug) whose data_dir matches, e.g.
/// [db.myapp] data_dir = ~/.local/share/mdrv-db/myapp -> "myapp". Comparison is
/// canonicalized when possible, string-based otherwise.
pub fn lookup_name(file: &Path, data_dir: &Path) -> Option<String> {
    let cfg = load(file).ok()?;
    let dbs = cfg.get("db")?.as_table()?;
    let want = std::fs::canonicalize(data_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| data_dir.display().to_string());
    for (slug, v) in dbs {
        let Some(t) = v.as_table() else { continue };
        let Some(dir) = t.get("data_dir").and_then(|d| d.as_str()) else {
            continue;
        };
        let have = std::fs::canonicalize(dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| dir.to_string());
        if have == want {
            return Some(slug.clone());
        }
    }
    None
}

pub fn run(file: Option<PathBuf>, check: bool, set: Option<String>) -> ExitCode {
    let path = file.or_else(default_path);
    let Some(path) = path else {
        eprintln!("no config path (set $MDRV_DB_CONFIG or $HOME)");
        return ExitCode::from(1);
    };
    if let Some(expr) = set {
        return set_prop(&path, &expr);
    }
    let cfg = match load(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let mut problems = 0usize;
    println!("config: {}", path.display());
    if let Some(sched) = cfg.get("sched") {
        println!(
            "sched:  data_root={}",
            sched
                .get("data_root")
                .and_then(|v| v.as_str())
                .unwrap_or("-"),
        );
    }
    if let Some(dbs) = cfg.get("db").and_then(|v| v.as_table()) {
        for (slug, t) in dbs {
            let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("-");
            let root = cfg
                .get("sched")
                .and_then(|s| s.get("data_root"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            let data_dir: PathBuf = t
                .get("data_dir")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .or_else(|| root.map(|r| r.join(slug)))
                .unwrap_or_else(|| default_data_dir(slug));
            let live_ok = data_dir.join("live/fjall").is_dir();
            let dur = t
                .get("durability")
                .and_then(|v| v.as_str())
                .unwrap_or("per-write");
            let backup = fmt_cadence(t.get("backup"));
            let rpc = t.get("rpc");
            let (url, token_env) = (
                rpc.and_then(|r| r.get("url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
                rpc.and_then(|r| r.get("token_env"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
            );
            let token_set = std::env::var_os(token_env).is_some_and(|v| !v.is_empty());
            println!(
                "{slug:<18} {name:<24} data={}{} dur={dur} backup={backup} rpc={url} token_env={token_env}({})",
                data_dir.display(),
                if live_ok { "" } else { " [no live/fjall]" },
                if token_set { "set" } else { "unset" },
            );
            if !live_ok {
                problems += 1;
            }
        }
    } else if !check {
        println!("(no [db.*] entries)");
    }
    if check {
        if problems == 0 {
            println!("check: OK");
            ExitCode::SUCCESS
        } else {
            eprintln!("check: {problems} problem(s)");
            ExitCode::from(1)
        }
    } else {
        ExitCode::SUCCESS
    }
}

fn fmt_cadence(v: Option<&toml::Value>) -> String {
    match v {
        Some(t) if t.is_table() => format!(
            "{}/{}d",
            t.get("cron").and_then(|x| x.as_str()).unwrap_or("?"),
            t.get("retention_days")
                .and_then(|x| x.as_integer())
                .unwrap_or(0)
        ),
        _ => "-".into(),
    }
}

/// `mdrv-db config --set db.myapp.data_dir=~/.local/share/mdrv-db/myapp`
/// Writes the TOML back preserving comments where possible (single-key edit).
fn set_prop(path: &Path, expr: &str) -> ExitCode {
    let (key, val) = match expr.split_once('=') {
        Some(kv) => kv,
        None => {
            eprintln!("--set expects KEY=VALUE");
            return ExitCode::from(1);
        }
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    let mut cfg: toml::Value = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("TOML parse error: {e}");
            return ExitCode::from(1);
        }
    };
    let parts: Vec<&str> = key.split('.').collect();
    let Some(table) = cfg.as_table_mut() else {
        return ExitCode::from(1);
    };
    let mut cur = table;
    for p in &parts[..parts.len() - 1] {
        cur = match cur
            .entry(p.to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()))
        {
            toml::Value::Table(t) => t,
            _ => {
                eprintln!("{p} is not a table");
                return ExitCode::from(1);
            }
        };
    }
    let last = parts[parts.len() - 1];
    let parsed: toml::Value = match val.parse::<i64>() {
        Ok(n) => toml::Value::Integer(n),
        Err(_) => match val.parse::<bool>() {
            Ok(b) => toml::Value::Boolean(b),
            Err(_) => toml::Value::String(val.to_string()),
        },
    };
    cur.insert(last.to_string(), parsed);
    match toml::to_string_pretty(&cfg) {
        Ok(out) => {
            if let Err(e) = std::fs::write(path, out) {
                eprintln!("cannot write {}: {e}", path.display());
                return ExitCode::from(1);
            }
            println!("{key} = {val}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("TOML serialize error: {e}");
            ExitCode::from(1)
        }
    }
}
