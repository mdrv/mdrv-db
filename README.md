# mdrv-db

> [!CAUTION]
> **Personal software, maintained for the author only.** The repo is public so
> the CI and install pipeline have a home, not because it is a supported
> product. Development is heavily LLM-assisted: most commits are written with
> AI agents and verified only against the author's own apps. Expect breaking
> changes at any minor bump, no support, no stability guarantee, no release
> schedule. Read it as a reference for your own durable-engine design; if you
> want a supported embedded database, use SQLite or Turso directly.

A multi-layer durable database system: an embeddable Rust engine that wraps a SQL data port
(Turso/SQLite) and a content-addressed blob store inside a **Fjall durability envelope**.
Every mutation is journaled WAL-first with a monotonic LSN, applied in one port transaction,
and marked committed only after the port confirms. Recovery replays committed journal entries
above the data layer's watermark. Backups, integrity verification, and verbose report logs are
first-class, all inspectable offline.

## Status

Current release: **v0.6.1**. The data port targets turso 0.8 (feature-gated).

| Piece                                                                      | State                                                                                                       |
| -------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| Engine crate `mdrv_db` (WAL/recovery/blobs/backup/verify/report/rebuild)   | ✅ crash-matrix-proven (`mdrv-db selftest`, 8 fault-injected scenarios)                                     |
| CLI `mdrv-db` (clap + carapace completion)                                 | ✅ `init` `info` `dump` `config` `verify` `backup` `restore` `rebuild` `checkpoint` `prune` `blob` `daemon` |
| Fleet config (`~/.config/mdrv-db/config.toml`)                             | ✅ read/check/set via CLI                                                                                   |
| `@mdrv/db` npm binding (napi; streaming blobs, deleteBlob)                 | ✅ published: linux x64/arm64 (gnu+musl), darwin x64/arm64, win32-x64                                      |
| `@mdrv/db-config` + `@mdrv/db-events` (outbox + `/events` + consumer)      | ✅ published                                                                                                |
| `daemon run` / `status` / `install` (scheduler: backup, verify, retention) | ✅ shipped                                                                                                  |
| Admin REST + SSE + embedded console (`packages/console`)                   | ✅ shipped (read-mostly + backup-now/prune; restore stays CLI)                                              |
| Platforms                                                                  | ✅ linux (battle-tested) · ✅ darwin + windows (verified 0.6.1) · 🧪 android aarch64 (compile-verified)     |

## Layout (per-database)

```
~/.local/share/mdrv-db/<slug>/
├── live/                ← owned engine: live/fjall/ + live/app.db + live/blobs/   (rsync: IGNORE)
├── recovery/            ← backup output: <ts>-offline/{manifest.json, app.db, fjall/, blobs/}  (rsync: THIS)
└── daemon-state.json    ← daemon run log (last/next run, result, backup)
```

`live` is single-owner (fjall + turso file locks); `recovery` is safe to sync offsite.
Dumps/inspection never touch `live` while an app owns it; work on `recovery` copies.

## Install (prebuilt)

macOS & Linux (x86_64/aarch64):

```sh
curl -fsSL https://github.com/mdrv/mdrv-db/releases/latest/download/install.sh | sh
```

Windows (x64, PowerShell):

```powershell
irm https://github.com/mdrv/mdrv-db/releases/latest/download/install.ps1 | iex
```

macOS & Windows targets shipped in 0.6.1 (verified on real hardware); Linux
remains the longest-tested platform.

## Build

```bash
cargo build --release -p mdrv-db --features turso
#                ^ the CLI bin requires the `turso` feature; without it only
#                  the lib + napi cdylib are built
# add --offline if crate fetch times out (cache has everything)
```

Binary: `target/release/mdrv-db`.

## Shell completion (carapace)

```bash
mdrv-db completion install
```

## CLI

```bash
mdrv-db init ~/.local/share/mdrv-db/app.example.id            # create live/ + recovery/
mdrv-db info ~/.local/share/mdrv-db/main.example.id                   # keyspace sizes, applied_lsn, blobs
mdrv-db dump wal ~/.local/share/mdrv-db/main.example.id \
    --since '2026-09-10 04:50' --filter 'register' --status committed --reverse
mdrv-db dump report ~/.local/share/mdrv-db/main.example.id --level 3 --limit 50
mdrv-db config --check                           # fleet sanity (missing live/fjall etc.)
mdrv-db config --set 'db.main.durability="per-write"'
mdrv-db verify ~/.local/share/mdrv-db/main.example.id                 # WAL hashes, blob re-hash, PRAGMA integrity
mdrv-db backup ~/.local/share/mdrv-db/main.example.id                 # → recovery/<ts>-offline + manifest (blake3)
mdrv-db restore ~/.local/share/mdrv-db/main.example.id/recovery/<ts>-offline ~/.local/share/mdrv-db/main.example.id
mdrv-db rebuild ~/.local/share/mdrv-db/main.example.id                # swap a corrupt app.db for the newest backup, keep the journal
mdrv-db checkpoint ~/.local/share/mdrv-db/main.example.id --compact   # prune WAL ≤ applied_lsn (+ major compact)
mdrv-db blob put ~/.local/share/mdrv-db/main.example.id big.bin       # streaming CAS upload (dedup by sha256)
mdrv-db daemon run --once                            # run every scheduled job now, then exit
mdrv-db daemon run                                   # scheduler + admin REST + console on 127.0.0.1:8300
mdrv-db daemon run --port 8301 --console ./dist      # flags override config/defaults
mdrv-db daemon status                                # per-slug last/next run, result, backups (add --json for scripts)
mdrv-db daemon install                               # systemd --user unit for `daemon run`
```

`dump` runs fully offline (stopped owner or `recovery` copy); `verify|backup|restore|
rebuild|checkpoint|prune` open the engine; run them while the owning app is stopped (the file lock
enforces this loudly, never silently). Exit code `2` from `dump` = decode warnings.

## Fleet config

`mdrv-db/config.toml` (override with `$MDRV_DB_CONFIG`): `~/.config/` on
Linux, `%APPDATA%\mdrv-db\config.toml` on Windows, `~/Library/Application
Support/mdrv-db/config.toml` on macOS.

```toml
[defaults]
durability = "per-write" # per-write | group-commit-10ms
verify = "weekly"

[sched]
data_root = "~/.local/share/mdrv-db"

[db.main]
owner = "alice" # engine name is NOT set here: it lives in live/ meta (stamped
# by `init`), and a mismatched `name =` field makes the daemon
# skip the slug (name-guard refuses the open)
data_dir = "~/.local/share/mdrv-db/main.example.id"
rpc = "127.0.0.1:8100/mdrv/rpc" # admin RPC when the owner app is up
token_env = "MAIN_ADMIN_TOKEN"
[db.main.backup]
cron = "daily"
retention_days = 30 # or keep = 5 for count-based; min_free = "10G" guards backup disk (1G default floor, 0 = off)

[db.main.app] # app-domain settings live under the app's section
admins = ["alice"]
```

Envelope knobs are generic fleet-wide; `[db.<slug>.<app>]` subtables belong to the owning app.

## Migrating a legacy (flat) layout

Old data dirs were flat (`fjall/`, `app.db`, `blobs/` side by side). The current layout nests them
under `live/`. Migration is the backup→restore drill; the engine still opens flat dirs read/write
(`live_dir()` falls back to the root when `live/` is absent):

```bash
mdrv-db backup /old/flat/dir --dest /tmp/mig           # flat layout works
mdrv-db restore /tmp/mig ~/.local/share/mdrv-db/<slug> # writes the nested layout + verifies
```

## Roadmap

- **Daemon (done)**: `mdrv-db daemon run|status|install`: per-slug cron from
  `[db.<slug>.backup]`; each job = backup → verify → checkpoint → retention prune
  (newest always kept); a running owner is a clean skip, retried next occurrence.
  Server mode adds the admin REST + SSE (`/api/*`, cookie auth via `POST /login`,
  `MDRV_DB_ADMIN_TOKEN`) and serves the embedded console; host/port overridable via
  `[daemon]` in the fleet config or `--bind`/`--port` flags. `GET /healthz` is an
  unauthenticated liveness probe; `GET /api/version` (authed) reports the binary
  version. Console scope: fleet dashboard, backup browser, report viewer, backup-now +
  prune (restore stays CLI), live `job.started`/`job.finished` badges via SSE.
  Retention: `retention_days` and/or `keep = N` (newest always kept); the disk guard
  skips a job when the backup volume has less free space than `min_free` (default
  1 GiB, `0` disables). A skip is surfaced in status/SSE; nothing is ever auto-deleted.
- releases: bump manifests, commit, tag `vX.Y.Z` (CI publishes crates.io + npm + AUR).
