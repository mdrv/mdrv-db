//! mdrv-db CLI — fleet control for mdrv databases (v2).
//!
//! Layout per database: <data-dir>/live (owned engine: fjall/, app.db,
//! blobs/) + <data-dir>/recovery (backup output; rsync target).

mod config;
mod dump;
mod port_turso;
mod selftest;

use clap::{CommandFactory, Parser, Subcommand, ValueHint};
use std::path::PathBuf;
use std::process::ExitCode;

/// Engine name: explicit flag, else fleet-config lookup by data_dir,
/// else the engine's own meta.db_name, else the directory's file name.
fn resolve_name(data_dir: &std::path::Path, name: &Option<String>) -> String {
    if let Some(n) = name {
        return n.clone();
    }
    // The envelope's own db_name is authoritative when present (the name
    // guard compares against it); config lookup only fills in fresh dirs.
    if let Some(n) = read_meta_name(data_dir) {
        return n;
    }
    if let Some(p) = config::default_path() {
        if let Some(n) = config::lookup_name(&p, data_dir) {
            return n;
        }
    }
    data_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".into())
}

/// Read the engine's recorded db_name straight from the fjall envelope
/// (read-only, no port). Returns None when there is no envelope yet.
fn read_meta_name(data_dir: &std::path::Path) -> Option<String> {
    let root = port_turso::live_dir(data_dir);
    let db = fjall::Database::builder(root.join("fjall")).open().ok()?;
    let meta = db
        .keyspace("meta", fjall::KeyspaceCreateOptions::default)
        .ok()?;
    let v = meta.get("db_name").ok()??;
    String::from_utf8(v.to_vec()).ok()
}

fn engine_op(
    data_dir: &std::path::Path,
    name: &Option<String>,
    f: impl FnOnce(&mut mdrv_db::Engine) -> Result<String, String>,
) -> ExitCode {
    let n = resolve_name(data_dir, name);
    let mut engine = match port_turso::open_engine(data_dir, &n, true, false) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let out = f(&mut engine);
    let _ = engine.close();
    match out {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<u64, String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let mut n = 0u64;
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let p = e.path();
        if p.is_dir() {
            n += copy_dir(&p, &dst.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, dst.join(e.file_name())).map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    Ok(n)
}

/// Copy a backup (manifest.json + app.db + fjall/ + blobs/) into <data-dir>/live.
fn restore_copy(backup_dir: &std::path::Path, data_dir: &std::path::Path) -> Result<(), String> {
    if !backup_dir.join("manifest.json").is_file() {
        return Err(format!(
            "{backup_dir:?} has no manifest.json — not a backup"
        ));
    }
    if data_dir.join("live").join("fjall").exists() {
        return Err(format!(
            "{:?} already has live/fjall — refusing to overwrite; clear it first",
            data_dir
        ));
    }
    let live = data_dir.join("live");
    std::fs::create_dir_all(&live).map_err(|e| e.to_string())?;
    std::fs::copy(backup_dir.join("app.db"), live.join("app.db")).map_err(|e| e.to_string())?;
    let fj = copy_dir(&backup_dir.join("fjall"), &live.join("fjall"))?;
    let blobs = if backup_dir.join("blobs").is_dir() {
        copy_dir(&backup_dir.join("blobs"), &live.join("blobs"))?
    } else {
        0
    };
    println!(
        "copied app.db + {fj} fjall files + {blobs} blobs into {}",
        live.display()
    );
    Ok(())
}

#[derive(Parser)]
#[command(
    name = "mdrv-db",
    version,
    about = "mdrv-db fleet CLI — offline inspection and lifecycle ops for Fjall-enveloped databases"
)]
struct Cli {
    /// Fleet config path (default: $MDRV_DB_CONFIG or ~/.config/mdrv-db/config.toml)
    #[arg(long, global = true, value_hint = ValueHint::FilePath)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a data-dir skeleton: <dir>/live + <dir>/recovery
    Init {
        /// Database directory, e.g. /x/db/app.example.id
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
    },
    /// Summarize an mdrv database (keyspace sizes, meta, blobs) — offline
    Info {
        /// Database directory (contains live/)
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
    },
    /// Offline viewer for the Fjall envelope (run on a stopped owner or a copy)
    Dump {
        #[command(subcommand)]
        sub: dump::DumpCmd,
    },
    /// Verify envelope integrity (WAL hashes, blobs, port) — owner must be stopped
    Verify {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Write a recovery backup (persist + VACUUM INTO + copies + manifest) — owner must be stopped
    Backup {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        /// Destination (default: <data-dir>/recovery/<ts>-offline)
        #[arg(long, value_hint = ValueHint::DirPath)]
        dest: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Restore live/ from a recovery backup, then verify — owner must be stopped
    Restore {
        /// Backup directory (contains manifest.json)
        #[arg(value_hint = ValueHint::DirPath)]
        backup_dir: PathBuf,
        /// Database directory to restore into
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Checkpoint the journal up to applied_lsn (prunes wal/marks below it) — owner must be stopped
    Checkpoint {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        #[arg(long)]
        compact: bool,
        #[arg(long)]
        name: Option<String>,
    },
    /// Alias of checkpoint (without compaction) — owner must be stopped
    Prune {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Start the fleet daemon (admin REST, scheduler, website) — later phase
    Serve,
    /// Inspect the fleet config ($MDRV_DB_CONFIG or ~/.config/mdrv-db/config.toml)
    Config {
        #[arg(long, value_hint = ValueHint::FilePath)]
        file: Option<PathBuf>,
        /// Validate entries (exit 1 on problems)
        #[arg(long)]
        check: bool,
        /// Set one property: --set 'db.myapp.data_dir=/x/db/myapp'
        #[arg(long)]
        set: Option<String>,
    },
    /// Blob store operations (content-addressed, streamed)
    Blob {
        #[command(subcommand)]
        op: BlobOp,
    },
    /// Crash-injection matrix: children write acked entries and abort at
    /// pipeline points; the parent asserts durability + verify
    Selftest {
        /// Scratch root for the scenario databases
        #[arg(default_value = "/tmp/mdrv-db-selftest", value_hint = ValueHint::DirPath)]
        root: PathBuf,
    },
    /// Internal: selftest child (hidden)
    #[command(hide = true)]
    SelftestChild {
        dir: PathBuf,
        /// fault point: - | after_persist | after_apply
        fault: String,
        /// fsync each write (1/0)
        fsync: String,
        /// abort after this many acked writes (0 = never)
        abort_after: u64,
        progress: PathBuf,
    },
    /// Completion helpers (bare = print the carapace spec, pipe-friendly)
    Completion {
        #[command(subcommand)]
        cmd: Option<CompletionCmd>,
    },
}

#[derive(Subcommand)]
enum CompletionCmd {
    /// Print a completion spec/script to stdout (carapace YAML by default)
    Print {
        /// Emit a native completion script for this shell instead of the carapace YAML
        #[arg(long)]
        shell: Option<clap_complete::shells::Shell>,
    },
    /// Write the carapace spec to <user config>/carapace/specs/mdrv-db.yaml
    Install,
}

#[derive(Subcommand)]
enum BlobOp {
    /// Stream a file into the content-addressed blob store (no DB write;
    /// reference it with a BlobPut op to make it durable)
    Put {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        /// File to ingest (streamed in chunks, never fully buffered)
        #[arg(long, value_hint = ValueHint::FilePath)]
        file: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Print the store path of a committed blob by hash
    Get {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        hash: String,
        #[arg(long)]
        name: Option<String>,
    },
}

/// Write the generated spec into the user's carapace specs dir — the only
/// location carapace-bin loads user specs from (UserConfigDir).
fn completion_install() -> Result<std::path::PathBuf, String> {
    let mut buf = Vec::new();
    let mut cmd = Cli::command();
    clap_complete::generate(carapace_spec_clap::Spec, &mut cmd, "mdrv-db", &mut buf);
    let config_dir = match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => std::path::PathBuf::from(
            std::env::var("HOME").map_err(|_| "cannot resolve config dir: no XDG_CONFIG_HOME or HOME")?,
        )
        .join(".config"),
    };
    let dir = config_dir.join("carapace").join("specs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
    let path = dir.join("mdrv-db.yaml");
    std::fs::write(&path, buf).map_err(|e| format!("cannot write {path:?}: {e}"))?;
    Ok(path)
}

fn not_yet(what: &str) -> ExitCode {
    eprintln!("{what} is planned for a later v2 phase.");
    eprintln!("Meanwhile, v1 tooling (maintenance daemon, admin RPC) lives in the private v1 archive.");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    config::set_override(cli.config.clone());
    match cli.cmd {
        Cmd::Completion { cmd } => match cmd {
            None => {
                let mut command = Cli::command();
                clap_complete::generate(
                    carapace_spec_clap::Spec,
                    &mut command,
                    "mdrv-db",
                    &mut std::io::stdout(),
                );
                ExitCode::SUCCESS
            }
            Some(CompletionCmd::Print { shell }) => {
                let mut command = Cli::command();
                match shell {
                    Some(s) => {
                        clap_complete::generate(s, &mut command, "mdrv-db", &mut std::io::stdout())
                    }
                    None => clap_complete::generate(
                        carapace_spec_clap::Spec,
                        &mut command,
                        "mdrv-db",
                        &mut std::io::stdout(),
                    ),
                }
                ExitCode::SUCCESS
            }
            Some(CompletionCmd::Install) => match completion_install() {
                Ok(path) => {
                    println!("spec installed: {}", path.display());
                    println!("carapace-bin loads user specs ONLY from this dir");
                    println!("(system-wide /usr/share/carapace/specs is ignored as of 1.7.3).");
                    println!("Completions also need the carapace shell hook (carapace <shell> init in");
                    println!("your shell rc). Test: carapace mdrv-db nushell mdrv-db ''");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("completion install failed: {e}");
                    ExitCode::from(1)
                }
            }
        },
        Cmd::Init { data_dir } => {
            let live = data_dir.join("live");
            let recovery = data_dir.join("recovery");
            for d in [&live, &recovery] {
                if let Err(e) = std::fs::create_dir_all(d) {
                    eprintln!("cannot create {d:?}: {e}");
                    return ExitCode::from(1);
                }
            }
            let n = resolve_name(&data_dir, &None);
            match port_turso::open_engine(&data_dir, &n, true, true) {
                Ok(mut e) => {
                    let applied = e
                        .status()
                        .get("applied_lsn")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    e.close().ok();
                    println!(
                        "initialized {} (live/ + recovery/, name '{n}', applied_lsn {applied})",
                        data_dir.display()
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::from(1)
                }
            }
        }
        Cmd::Info { data_dir } => dump::info(&data_dir),
        Cmd::Dump { sub } => dump::run(sub),
        Cmd::Verify { data_dir, name } => {
            if data_dir.join("manifest.json").is_file() {
                return match mdrv_db::verify::run_backup(&data_dir) {
                    Ok(v) => {
                        let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
                        if ok {
                            println!("backup verify OK\n{v}");
                            ExitCode::SUCCESS
                        } else {
                            eprintln!("backup verify FAILED\n{v}");
                            ExitCode::from(1)
                        }
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::from(1)
                    }
                };
            }
            engine_op(&data_dir, &name, |e| {
                let v = mdrv_db::verify::run(e).map_err(|x| x.to_string())?;
                let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
                if ok {
                    Ok(format!("verify OK\n{v}"))
                } else {
                    Err(format!("verify FAILED\n{v}"))
                }
            })
        }
        Cmd::Backup {
            data_dir,
            dest,
            name,
        } => {
            let n = resolve_name(&data_dir, &name);
            let d = dest.unwrap_or_else(|| {
                data_dir
                    .join("recovery")
                    .join(format!("{}-offline", mdrv_db::now_ms()))
            });
            engine_op(&data_dir, &Some(n.clone()), |e| {
                let out = mdrv_db::backup::run(e, &d).map_err(|x| x.to_string())?;
                Ok(format!(
                    "backup → {}\napplied_lsn = {}",
                    d.display(),
                    out.manifest
                        .get("applied_lsn")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "?".into())
                ))
            })
        }
        Cmd::Restore {
            backup_dir,
            data_dir,
            name,
        } => {
            if let Err(e) = restore_copy(&backup_dir, &data_dir) {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
            engine_op(&data_dir, &name, |e| {
                let v = mdrv_db::verify::run(e).map_err(|x| x.to_string())?;
                Ok(format!("restore + verify: {v}"))
            })
        }
        Cmd::Checkpoint {
            data_dir,
            compact,
            name,
        } => engine_op(&data_dir, &name, |e| {
            let n = e.checkpoint(compact).map_err(|x| x.to_string())?;
            Ok(format!("checkpoint ok ({n})"))
        }),
        Cmd::Prune { data_dir, name } => engine_op(&data_dir, &name, |e| {
            let n = e.checkpoint(false).map_err(|x| x.to_string())?;
            Ok(format!("prune ok ({n})"))
        }),
        Cmd::Blob { op } => match op {
            BlobOp::Put {
                data_dir,
                file,
                name,
            } => engine_op(&data_dir, &name, |e| {
                let mut up = e.blob_upload_begin().map_err(|x| x.to_string())?;
                let mut f = std::fs::File::open(&file).map_err(|x| x.to_string())?;
                let mut buf = vec![0u8; 1024 * 1024];
                loop {
                    use std::io::Read;
                    let n = f.read(&mut buf).map_err(|x| x.to_string())?;
                    if n == 0 {
                        break;
                    }
                    up.write(&buf[..n]).map_err(|x| x.to_string())?;
                }
                let (hash, bytes) = e.blob_upload_finish(up).map_err(|x| x.to_string())?;
                Ok(format!("{hash} {bytes}"))
            }),
            BlobOp::Get {
                data_dir,
                hash,
                name,
            } => engine_op(&data_dir, &name, |e| match e.blob_path(&hash) {
                Some(p) => Ok(p.display().to_string()),
                None => Err(format!("no blob {hash}")),
            }),
        },
        Cmd::Selftest { root } => selftest::run(&root),
        Cmd::SelftestChild {
            dir,
            fault,
            fsync,
            abort_after,
            progress,
        } => selftest::child(&dir, &fault, fsync == "1", abort_after, &progress),
        Cmd::Serve => not_yet("serve"),
        Cmd::Config { file, check, set } => config::run(file, check, set),
    }
}
