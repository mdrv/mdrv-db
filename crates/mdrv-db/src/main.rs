//! mdrv-db CLI — fleet control for mdrv databases (v2).
//!
//! Layout per database: <data-dir>/live (owned engine: fjall/, app.db,
//! blobs/) + <data-dir>/recovery (backup output; rsync target).

mod config;
mod dump;
mod port_turso;

use clap::{CommandFactory, Parser, Subcommand, ValueHint};
use std::path::PathBuf;
use std::process::ExitCode;

/// Engine name: explicit flag, else fleet-config lookup by data_dir,
/// else the directory's file name.
fn resolve_name(data_dir: &std::path::Path, name: &Option<String>) -> String {
    if let Some(n) = name {
        return n.clone();
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

fn engine_op(
    data_dir: &std::path::Path,
    name: &Option<String>,
    f: impl FnOnce(&mut mdrv_db::Engine) -> Result<String, String>,
) -> ExitCode {
    let n = resolve_name(data_dir, name);
    let mut engine = match port_turso::open_engine(data_dir, &n) {
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
        return Err(format!("{backup_dir:?} has no manifest.json — not a backup"));
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
    /// Inspect the fleet config ($MDRV_CONFIG or ~/.config/mdrv-db/config.toml)
    Config {
        #[arg(long, value_hint = ValueHint::FilePath)]
        file: Option<PathBuf>,
        /// Validate entries (exit 1 on problems)
        #[arg(long)]
        check: bool,
        /// Set one property: --set 'db.app.data_dir=/x/db/app.example.id'
        #[arg(long)]
        set: Option<String>,
    },
    /// Emit a carapace completion spec:
    /// mdrv-db completion > ~/.config/carapace/specs/mdrv-db.yaml
    Completion,
}

fn not_yet(what: &str) -> ExitCode {
    eprintln!("{what} is planned for a later v2 phase.");
    eprintln!("Meanwhile, v1 tooling: /g/mdrv-db-v1-archive (packages/db-maintenance, admin RPC).");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Completion => {
            let mut cmd = Cli::command();
            clap_complete::generate(carapace_spec_clap::Spec, &mut cmd, "mdrv-db", &mut std::io::stdout());
            ExitCode::SUCCESS
        }
        Cmd::Init { data_dir } => {
            let live = data_dir.join("live");
            let recovery = data_dir.join("recovery");
            for d in [&live, &recovery] {
                if let Err(e) = std::fs::create_dir_all(d) {
                    eprintln!("cannot create {d:?}: {e}");
                    return ExitCode::from(1);
                }
            }
            println!("initialized {} (live/ + recovery/)", data_dir.display());
            ExitCode::SUCCESS
        }
        Cmd::Info { data_dir } => dump::info(&data_dir),
        Cmd::Dump { sub } => dump::run(sub),
        Cmd::Verify { data_dir, name } => engine_op(&data_dir, &name, |e| {
            let v = mdrv_db::verify::run(e).map_err(|x| x.to_string())?;
            let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
            if ok {
                Ok(format!("verify OK\n{v}"))
            } else {
                Err(format!("verify FAILED\n{v}"))
            }
        }),
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
        Cmd::Serve => not_yet("serve"),
        Cmd::Config { file, check, set } => config::run(file, check, set),
    }
}
