# mdrv-db

A multi-layer durable database system: an embeddable Rust engine that wraps a SQL data port
(Turso/SQLite) and a content-addressed blob store inside a **Fjall durability envelope** —
every mutation is journaled WAL-first with a monotonic LSN, applied in one port transaction,
and marked committed only after the port confirms. Recovery replays committed journal entries
above the data layer's watermark; backups, integrity verification, and verbose report logs are
first-class, all inspectable offline.

> This is the **v2** line (CLI-first). The crash-proven v1 engine lives on in `crates/mdrv-db/src`
> (carried over verbatim); the v1 archive — including the napi binding and the app integrations —
> is at a local v1 archive (not published).

## Status (v2)

| Piece                                                                      | State                                                                             |
| -------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Engine crate `mdrv_db` (WAL/recovery/blobs/backup/verify/report)           | ✅ ported from v1, crash-matrix-proven there                                      |
| CLI `mdrv-db` (clap + completion)                                          | ✅ `init` `info` `dump` `config` `verify` `backup` `restore` `checkpoint` `prune` |
| Fleet config (`~/.config/mdrv-db/config.toml`)                             | ✅ read/check/set via CLI                                                         |
| `@mdrv/db` npm binding (napi) v2 (x64 + arm64, streaming blobs)            | ✅ published (crates.io + npm 0.2.1)                                              |
| `@mdrv/db-events` (outbox + `/events` + consumer poller)                   | ✅ in repo (`packages/db-events`)                                                 |
| `daemon run` / `status` / `install` (scheduler: backup, verify, retention) | ✅ shipped                                                                        |
| Admin REST + SSE + embedded console (`packages/console`)                   | ✅ shipped (v1: read-mostly + backup-now/prune)                                   |

## Layout (per-database)

```
~/.local/share/mdrv-db/<slug>/
├── live/        ← owned engine: live/fjall/ + live/app.db + live/blobs/   (rsync: IGNORE)
└── recovery/    ← backup output: <ts>-offline/{manifest.json, app.db, fjall/, blobs/}  (rsync: THIS)
├── daemon-state.json ← daemon run log (last/next run, result, backup)
```

`live` is single-owner (fjall + turso file locks); `recovery` is safe to sync offsite.
Dumps/inspection never touch `live` while an app owns it — work on `recovery` copies.

## Build

```bash
cargo build --release            # add --offline if crate fetch times out (cache has everything)
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
mdrv-db dump ~/.local/share/mdrv-db/main.example.id wal \
    --since '2026-09-10 04:50' --filter 'register' --status committed --reverse
mdrv-db dump ~/.local/share/mdrv-db/main.example.id report --level 3 --limit 50
mdrv-db config --check                           # fleet sanity (missing live/fjall etc.)
mdrv-db config --set 'db.mid.durability="per-write"'
mdrv-db verify ~/.local/share/mdrv-db/main.example.id                 # WAL hashes, blob re-hash, PRAGMA integrity
mdrv-db backup ~/.local/share/mdrv-db/main.example.id                 # → recovery/<ts>-offline + manifest (blake3)
mdrv-db restore ~/.local/share/mdrv-db/main.example.id/recovery/<ts>-offline --data-dir ~/.local/share/mdrv-db/main.example.id
mdrv-db checkpoint ~/.local/share/mdrv-db/main.example.id --compact   # prune WAL ≤ applied_lsn (+ major compact)
mdrv-db daemon run --once                            # run every scheduled job now, then exit
mdrv-db daemon run                                   # scheduler + admin REST + console on 127.0.0.1:8300
mdrv-db daemon run --port 8301 --console ./dist      # flags override config/defaults
mdrv-db daemon status                                # per-slug last/next run, result, backups (add --json for scripts)
mdrv-db daemon install                               # systemd --user unit for `daemon run`
```

`dump` runs fully offline (stopped owner or `recovery` copy); `verify|backup|restore|
checkpoint|prune` open the engine — run them while the owning app is stopped (the file lock
enforces this loudly, never silently). Exit code `2` from `dump` = decode warnings.

## Fleet config

`~/.config/mdrv-db/config.toml` (override with `$MDRV_DB_CONFIG`):

```toml
[defaults]
durability = "per-write" # per-write | group-commit-10ms
verify = "weekly"

[sched]
data_root = "~/.local/share/mdrv-db"

[db.mid]
name = "Example App"
owner = "alice"
data_dir = "~/.local/share/mdrv-db/main.example.id"
rpc = "127.0.0.1:8100/mdrv/rpc" # admin RPC when the owner app is up
token_env = "MID_ADMIN_TOKEN"
[db.mid.backup]
cron = "daily"
retention_days = 30 # or keep = 5 for count-based; min_free = "10G" guards backup disk (1G default floor, 0 = off)

[db.mid.mid] # app-domain settings live under the app's section
admins = ["ua"]
```

Envelope knobs are generic fleet-wide; `[db.<slug>.<app>]` subtables belong to the owning app.

## Migrating a v1 (flat) database

v1 data dirs were flat (`fjall/`, `app.db`, `blobs/` side by side). The v2 layout nests them
under `live/`. Migration is the backup→restore drill:

```bash
mdrv-db backup /old/flat/dir --dest /tmp/mig          # v1 flat layout works (live_dir falls back)
mdrv-db restore /tmp/mig --data-dir ~/.local/share/mdrv-db/<slug>      # writes the NEW live/ layout + verifies
```

## Roadmap

- **P2 (daemon, done)** — `mdrv-db daemon run|status|install`: per-slug cron from
  `[db.<slug>.backup]`; each job = backup → verify → checkpoint → retention prune
  (newest always kept); a running owner is a clean skip, retried next occurrence.
  Server mode adds the admin REST + SSE (`/api/*`, cookie auth via `POST /login`,
  `MDRV_DB_ADMIN_TOKEN`) and serves the embedded console; host/port overridable via
  `[daemon]` in the fleet config or `--bind`/`--port` flags. `GET /healthz` is an
  unauthenticated liveness probe; `GET /api/version` (authed) reports the binary
  version. v1 console: fleet dashboard, backup browser, report viewer, backup-now +
  prune (restore stays CLI), live `job.started`/`job.finished` badges via SSE.
  Retention: `retention_days` and/or `keep = N` (newest always kept); the disk guard
  skips a job when the backup volume has less free space than `min_free` (default
  1 GiB, `0` disables) — a skip is surfaced in status/SSE, never auto-deletes.
- releases: bump manifests → commit → tag `vX.Y.Z` (CI publishes crates.io + npm + AUR).
