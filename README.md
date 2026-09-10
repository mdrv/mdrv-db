# mdrv-db

A multi-layer durable database system: an embeddable Rust engine that wraps a SQL data port
(Turso/SQLite) and a content-addressed blob store inside a **Fjall durability envelope** —
every mutation is journaled WAL-first with a monotonic LSN, applied in one port transaction,
and marked committed only after the port confirms. Recovery replays committed journal entries
above the data layer's watermark; backups, integrity verification, and verbose report logs are
first-class, all inspectable offline.

> This is the **v2** line (CLI-first). The crash-proven v1 engine lives on in `crates/mdrv-db/src`
> (carried over verbatim); the v1 archive — including the napi binding and the app integrations —
> is at `/g/mdrv-db-v1-archive`.

## Status (v2)

| Piece                                                            | State                                                                             |
| ---------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Engine crate `mdrv_db` (WAL/recovery/blobs/backup/verify/report) | ✅ ported from v1, crash-matrix-proven there                                      |
| CLI `mdrv-db` (clap + carapace completion)                       | ✅ `init` `info` `dump` `config` `verify` `backup` `restore` `checkpoint` `prune` |
| Fleet config (`~/.config/mdrv-db/config.toml`)                   | ✅ read/check/set via CLI                                                         |
| `serve` (daemon: scheduler, admin RPC relay, website)            | ⏳ P2                                                                             |
| Website (fleet dashboard, log viewer, backup browser)            | ⏳ P3                                                                             |
| `@mdrv/db` npm binding (napi) v2                                 | ⏳ P4 (consumers still build against v1)                                          |

## Layout (per-database)

```
/x/db/<slug>/
├── live/        ← owned engine: live/fjall/ + live/app.db + live/blobs/   (rsync: IGNORE)
└── recovery/    ← backup output: <ts>-offline/{manifest.json, app.db, fjall/, blobs/}  (rsync: THIS)
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
mdrv-db completion > ~/.config/carapace/specs/mdrv-db.yaml
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
```

`dump` runs fully offline (stopped owner or `recovery` copy); `verify|backup|restore|
checkpoint|prune` open the engine — run them while the owning app is stopped (the file lock
enforces this loudly, never silently). Exit code `2` from `dump` = decode warnings.

## Fleet config

`~/.config/mdrv-db/config.toml` (override with `$MDRV_CONFIG`):

```toml
[defaults]
durability = "per-write" # per-write | group-commit-10ms
verify = "weekly"

[sched]
data_root = "/x/db"
backup_root = "/x/db-backups"

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

- **P2** — `mdrv-db serve`: fleet daemon (scheduler: backup/verify/retention incl. restore
  verification; relays admin RPC to up owners, offline-copies down ones).
- **P3** — website (Svelte 5 + vanilla-extract + LogTape): fleet dashboard, log viewer,
  backup browser, restore wizard. Read-mostly by design.
- **P4** — napi binding v2 (`@mdrv/db`), migrate consumers (mid, app) via backup→restore.

See `PLAN.md` for decisions (D1–D6) and the full phase plan. Guides: `/guides/mdrv-db/`.
