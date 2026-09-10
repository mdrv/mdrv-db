//! mdrv-db CLI — fleet control for mdrv databases (v2).
//!
//! Layout per database: <data-dir>/live (owned engine: fjall/, app.db,
//! blobs/) + <data-dir>/recovery (backup output; rsync target).

mod config;
mod dump;

use clap::{CommandFactory, Parser, Subcommand, ValueHint};
use std::path::PathBuf;
use std::process::ExitCode;

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
    /// Verify envelope integrity (WAL hashes, blobs, port) — later phase
    Verify {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
    },
    /// Write a recovery backup into <dir>/recovery/<ts>/ — later phase
    Backup {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        #[arg(long, value_hint = ValueHint::DirPath)]
        dest: Option<PathBuf>,
    },
    /// Restore live/ from a recovery backup — later phase
    Restore {
        /// Backup directory (contains manifest.json)
        #[arg(value_hint = ValueHint::DirPath)]
        backup_dir: PathBuf,
        /// Database directory to restore into
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
    },
    /// Checkpoint the journal up to applied_lsn — later phase
    Checkpoint {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
        #[arg(long)]
        compact: bool,
    },
    /// Prune journaled entries at/below applied_lsn — later phase
    Prune {
        #[arg(value_hint = ValueHint::DirPath)]
        data_dir: PathBuf,
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
        Cmd::Verify { .. } => not_yet("verify"),
        Cmd::Backup { .. } => not_yet("backup"),
        Cmd::Restore { .. } => not_yet("restore"),
        Cmd::Checkpoint { .. } => not_yet("checkpoint"),
        Cmd::Prune { .. } => not_yet("prune"),
        Cmd::Serve => not_yet("serve"),
        Cmd::Config { file, check, set } => config::run(file, check, set),
    }
}
