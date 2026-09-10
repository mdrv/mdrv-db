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

| Piece                                                            | State                                                                             |
| ---------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Engine crate `mdrv_db` (WAL/recovery/blobs/backup/verify/report) | ✅ ported from v1, crash-matrix-proven there                                      |
| CLI `mdrv-db` (clap + completion)                                | ✅ `init` `info` `dump` `config` `verify` `backup` `restore` `checkpoint` `prune` |
| Fleet config (`~/.config/mdrv-db/config.toml`)                   | ✅ read/check/set via CLI                                                         |
| `@mdrv/db` npm binding (napi) v2 (x64 + arm64, streaming blobs)  | ✅ published (crates.io + npm 0.2.1)                                              |
| `@mdrv/db-events` (outbox + `/events` + consumer poller)         | ✅ in repo (`packages/db-events`)                                                  |
| `daemon run` / `status` / `install` (scheduler: backup, verify, retention) | ✅ shipped (admin REST still ⏳)                                        |
| Console (fleet dashboard, log viewer, backup browser)            | ⏳ after daemon (`packages/console`, read-mostly)                                   |

## Layout (per-database)

```
/x/db/<slug>/
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
mdrv-db init /x/db/app.example.id            # create live/ + recovery/
mdrv-db info /x/db/main.example.id                   # keyspace sizes, applied_lsn, blobs
mdrv-db dump /x/db/main.example.id wal \
    --since '2026-09-10 04:50' --filter 'register' --status committed --reverse
mdrv-db dump /x/db/main.example.id report --level 3 --limit 50
mdrv-db config --check                           # fleet sanity (missing live/fjall etc.)
mdrv-db config --set 'db.mid.durability="per-write"'
mdrv-db verify /x/db/main.example.id                 # WAL hashes, blob re-hash, PRAGMA integrity
mdrv-db backup /x/db/main.example.id                 # → recovery/<ts>-offline + manifest (blake3)
mdrv-db restore /x/db/main.example.id/recovery/<ts>-offline --data-dir /x/db/main.example.id
mdrv-db checkpoint /x/db/main.example.id --compact   # prune WAL ≤ applied_lsn (+ major compact)
mdrv-db daemon run --once                            # run every scheduled job now, then exit
mdrv-db daemon run                                   # scheduler loop (cron per [db.*.backup])
mdrv-db daemon status                                # per-slug last/next run, result, backups
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
data_root = "/x/db"

[db.mid]
name = "Example App"
owner = "alice"
data_dir = "/x/db/main.example.id"
rpc = "127.0.0.1:8100/mdrv/rpc" # admin RPC when the owner app is up
token_env = "MID_ADMIN_TOKEN"
backup = "daily"
retention_days = 30

[db.mid.mid] # app-domain settings live under the app's section
admins = ["ua"]
```

Envelope knobs are generic fleet-wide; `[db.<slug>.<app>]` subtables belong to the owning app.

## Migrating a v1 (flat) database

v1 data dirs were flat (`fjall/`, `app.db`, `blobs/` side by side). The v2 layout nests them
under `live/`. Migration is the backup→restore drill:

```bash
mdrv-db backup /old/flat/dir --dest /tmp/mig          # v1 flat layout works (live_dir falls back)
mdrv-db restore /tmp/mig --data-dir /x/db/<slug>      # writes the NEW live/ layout + verifies
```

## Roadmap

- **P2 (scheduler, done)** — `mdrv-db daemon run|status|install`: per-slug cron from
  `[db.<slug>.backup]`; each job = backup → verify → checkpoint → retention prune
  (newest always kept); a running owner is a clean skip, retried next occurrence.
- **then** — console (`packages/console`, Svelte 5 + vanilla-extract + LogTape), read-mostly.
- releases: bump manifests → commit → tag `vX.Y.Z` (CI publishes crates.io + npm + AUR).

