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
/// Single-key edit that preserves comments and formatting (toml_edit).
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
    let out = match apply_set(&text, key, val) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = std::fs::write(path, out) {
        eprintln!("cannot write {}: {e}", path.display());
        return ExitCode::from(1);
    }
    println!("{key} = {val}");
    ExitCode::SUCCESS
}

/// Pure core of `--set`: parse `text`, set `key` (a dot path) to `val`,
/// and return the serialized document. Missing intermediate tables are
/// created; the value is parsed as bool/int/`[..]` string array, else
/// stored verbatim as a string.
fn apply_set(text: &str, key: &str, val: &str) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut =
        text.parse().map_err(|e| format!("TOML parse error: {e}"))?;
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err("--set expects KEY=VALUE with a non-empty dot path".into());
    }
    let mut cur = doc.as_table_mut();
    for p in &parts[..parts.len() - 1] {
        let slot = cur
            .entry(p)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        cur = match slot.as_table_mut() {
            Some(t) => t,
            None => return Err(format!("{p} is not a table")),
        };
    }
    cur.insert(parts[parts.len() - 1], parse_value(val));
    Ok(doc.to_string())
}

/// Values for `--set`: `true`/`false`, integers, `["a", "b"]` string
/// arrays, anything else verbatim as a string.
fn parse_value(val: &str) -> toml_edit::Item {
    let v = val.trim();
    if v == "true" || v == "false" {
        return toml_edit::Item::Value((v == "true").into());
    }
    if let Ok(i) = v.parse::<i64>() {
        return toml_edit::Item::Value(i.into());
    }
    if v.starts_with('[') && v.ends_with(']') {
        let mut arr = toml_edit::Array::new();
        for part in v[1..v.len() - 1].split(',') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            arr.push(p.trim_matches('"').trim_matches('\''));
        }
        return toml_edit::Item::Value(arr.into());
    }
    toml_edit::Item::Value(val.into())
}

/// Rewrite the per-slug display rank (`order = N`) for a whole fleet.
/// `ranks` maps slug -> position; missing slug tables are created.
pub fn set_slug_orders(path: &Path, ranks: &[(String, i64)]) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("TOML parse error in {}: {e}", path.display()))?;
    let db = doc
        .as_table_mut()
        .entry("db")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(db) = db.as_table_mut() else {
        return Err("[db] is not a table".into());
    };
    for (slug, rank) in ranks {
        let item = db
            .entry(slug.as_str())
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        let Some(t) = item.as_table_mut() else {
            return Err(format!("[db.{slug}] is not a table"));
        };
        t.insert("order", toml_edit::Item::Value((*rank).into()));
    }
    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Set or remove display metadata for one fleet slug, preserving
/// comments and formatting. `None` removes the key; an empty labels
/// slice removes the key.
pub fn set_slug_meta(
    path: &Path,
    slug: &str,
    title: Option<&str>,
    description: Option<&str>,
    labels: Option<&[String]>,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("TOML parse error in {}: {e}", path.display()))?;
    let db = doc
        .as_table_mut()
        .entry("db")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(db) = db.as_table_mut() else {
        return Err("[db] is not a table".into());
    };
    let slug_item = db
        .entry(slug)
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(slug_t) = slug_item.as_table_mut() else {
        return Err(format!("[db.{slug}] is not a table"));
    };
    match title {
        Some(v) => {
            slug_t.insert("title", toml_edit::Item::Value(v.into()));
        }
        None => {
            slug_t.remove("title");
        }
    }
    match description {
        Some(v) => {
            slug_t.insert("description", toml_edit::Item::Value(v.into()));
        }
        None => {
            slug_t.remove("description");
        }
    }
    match labels {
        Some(list) if !list.is_empty() => {
            let mut arr = toml_edit::Array::new();
            for l in list {
                arr.push(l.as_str());
            }
            slug_t.insert("labels", toml_edit::Item::Value(arr.into()));
        }
        _ => {
            slug_t.remove("labels");
        }
    }
    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "# fleet header\n",
        "[fleet]\n",
        "data_dir = \"/db\"\n",
        "\n",
        "# ap26 is the busiest\n",
        "[db.ap26]\n",
        "name = \"ap26\"\n",
    );

    #[test]
    fn set_preserves_comments_and_formatting() {
        let out = apply_set(SAMPLE, "db.ap26.title", "AP tracker").expect("set");
        assert!(out.contains("# fleet header"));
        assert!(out.contains("# ap26 is the busiest"));
        assert!(out.contains("title = \"AP tracker\""));
        assert!(out.contains("name = \"ap26\""));
    }

    #[test]
    fn set_creates_nested_tables_and_arrays() {
        let out = apply_set(SAMPLE, "db.ko26.labels", "[active, students]").expect("set");
        assert!(out.contains("labels = [\"active\", \"students\"]"));
        let out = apply_set(&out, "db.ko26.retention_days", "14").expect("set2");
        assert!(out.contains("retention_days = 14"));
    }

    #[test]
    fn slug_meta_set_and_remove() {
        let dir = std::env::temp_dir().join("mdrv-config-test");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let p = dir.join("meta.toml");
        std::fs::write(&p, SAMPLE).expect("write");
        set_slug_meta(
            &p,
            "ap26",
            Some("AP tracker"),
            Some("weekly"),
            Some(&["s".into()]),
        )
        .expect("set meta");
        let text = std::fs::read_to_string(&p).expect("read");
        assert!(text.contains("# ap26 is the busiest"));
        assert!(text.contains("title = \"AP tracker\""));
        set_slug_meta(&p, "ap26", None, None, None).expect("clear meta");
        let text = std::fs::read_to_string(&p).expect("read2");
        assert!(!text.contains("title"));
    }

    #[test]
    fn slug_orders_dense_rewrite() {
        let dir = std::env::temp_dir().join("mdrv-config-test");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let p = dir.join("orders.toml");
        std::fs::write(&p, SAMPLE).expect("write");
        set_slug_orders(&p, &[("wa26".into(), 0), ("ap26".into(), 1)]).expect("set orders");
        let text = std::fs::read_to_string(&p).expect("read");
        assert!(text.contains("# ap26 is the busiest"));
        assert!(text.contains("order = 1"));
        assert!(text.contains("order = 0"));
    }
}
