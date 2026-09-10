#!/usr/bin/env bash
# Comprehensive CLI test for mdrv-db (v2). Run from repo root:
#   bash scripts/cli-test.sh
set -u
BIN=target/release/mdrv-db
T=/tmp/opencode/cli-test
S=/tmp/mdrv-db-selftest
PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "ok   - $1"; }
bad() { FAIL=$((FAIL+1)); echo "FAIL - $1"; }
check() { # desc, expected_exit, actual_exit
  if [ "$2" = "$3" ]; then ok "$1 (exit $3)"; else bad "$1 (expected exit $2, got $3)"; fi
}
expect_contains() { # desc, haystack, needle
  if printf '%s' "$2" | grep -q "$3"; then ok "$1"; else bad "$1 (missing: $3)"; fi
}

rm -rf "$T" "$S"; mkdir -p "$T"

# --- 1. completion + help navigation
OUT=$($BIN completion 2>&1); [ $? -eq 0 ] && expect_contains "completion spec has name" "$OUT" "name: mdrv-db" || bad "completion failed"
OUT=$($BIN --help 2>&1)
for sub in init info dump verify backup restore checkpoint prune config completion blob selftest; do
  expect_contains "help lists '$sub'" "$OUT" "$sub"
done

# --- 2. config with temp config (never touches real one)
export MDRV_DB_CONFIG=$T/config.toml
cat > "$T/config.toml" << 'EOF'
[defaults]
durability = "per-write"
[sched]
data_root = "/x/db"
backup_root = "/x/db-backups"
[db.t1]
name = "Test One"
data_dir = "$T/t1"
EOF
# init t1 via CLI (config data_dir uses literal $T for substitution check)
sed -i "s|\$T|$T|g" "$T/config.toml"
OUT=$($BIN config 2>&1); check "config render" 0 $?
expect_contains "config lists t1" "$OUT" "t1"
OUT=$($BIN config --check 2>&1); check "config --check before init" 1 $?
OUT=$($BIN --config "$T/config.toml" config 2>&1); check "global --config flag" 0 $?
expect_contains "--config override renders" "$OUT" "Test One"

# --- 3. init (skeleton + engine bootstrap + name from config)
OUT=$($BIN init "$T/t1" 2>&1); check "init t1" 0 $?
expect_contains "init prints name from config" "$OUT" "name 't1'"
[ -d "$T/t1/live/fjall" ] && ok "init created live/fjall" || bad "init missing live/fjall"
[ -d "$T/t1/recovery" ] && ok "init created recovery/" || bad "init missing recovery/"
OUT=$($BIN config --check 2>&1); check "config --check after init" 0 $?

# --- 4. selftest (crash matrix: 8 scenarios)
OUT=$($BIN selftest "$S" 2>&1); RC=$?
check "selftest all scenarios" 0 $RC
printf '%s\n' "$OUT" | grep -c '^PASS' | grep -q 8 && ok "8 PASS lines" || bad "expected 8 PASS lines, got: $(printf '%s\n' "$OUT" | grep -c '^PASS')"
printf '%s\n' "$OUT" | grep -q '^FAIL' && bad "selftest had FAIL lines" || ok "no FAIL lines"

# --- 5. info + dump on a populated selftest db
DB=$S/fault_none_fsync_true
[ -d "$DB/live/fjall" ] || DB=$S/random-abort-137_fsync_1
OUT=$($BIN info "$DB" 2>&1); check "info on populated db" 0 $?
expect_contains "info shows applied_lsn" "$OUT" "applied_lsn"
expect_contains "info shows wal keyspace" "$OUT" "wal"
OUT=$($BIN dump wal "$DB" --limit 3 2>&1); check "dump wal" 0 $?
OUT=$($BIN dump wal "$DB" --filter 'v2' --json --limit 2 2>&1); check "dump wal --filter --json" 0 $?
expect_contains "json line has lsn" "$OUT" '"lsn"'
OUT=$($BIN dump wal "$DB" --actor selftest --status committed --reverse --limit 2 2>&1); check "dump wal --actor --status --reverse" 0 $?
OUT=$($BIN dump report "$DB" --limit 3 2>&1); check "dump report" 0 $?
OUT=$($BIN dump marks "$DB" --status pending --limit 3 2>&1); check "dump marks --status" 0 $?
OUT=$($BIN dump idem "$DB" --limit 3 2>&1); check "dump idem" 0 $?
OUT=$($BIN dump meta "$DB" 2>&1); check "dump meta" 0 $?
expect_contains "meta has db_name" "$OUT" "db_name"
OUT=$($BIN dump raw "$DB" meta 2>&1); check "dump raw meta" 0 $?
$BIN dump wal "$DB" --bogus >/dev/null 2>&1; check "dump bad flag rejected" 2 $?
$BIN dump wal "$T/nope" >/dev/null 2>&1; check "dump guard: missing dir" 1 $?
$BIN info "$T/nope" >/dev/null 2>&1; check "info guard: missing dir" 1 $?

# --- 6. verify live + fence
OUT=$($BIN verify "$DB" 2>&1); check "verify live" 0 $?
expect_contains "verify reports fence" "$OUT" '"fence"'

# --- 7. backup → verify backup → tamper → restore
OUT=$($BIN backup "$DB" 2>&1); check "backup default dest" 0 $?
BK=$(printf '%s\n' "$OUT" | grep -o '/tmp/mdrv-db-selftest/[^ ]*recovery/[0-9]*-offline' | head -1)
[ -n "$BK" ] && ok "backup dest resolved: $BK" || bad "no backup dest parsed"
[ -f "$BK/manifest.json" ] && ok "manifest.json exists" || bad "missing manifest.json"
OUT=$($BIN verify "$BK" 2>&1); check "verify backup dir" 0 $?
expect_contains "backup verify checked files" "$OUT" '"files"'
# tamper
TARGET=$(find "$BK" -name '*.db' | head -1)
printf 'X' | dd of="$TARGET" bs=1 seek=100 conv=notrunc 2>/dev/null
$BIN verify "$BK" >/dev/null 2>&1; check "tampered backup detected" 1 $?
# fresh backup for restore
rm -rf "$BK"; $BIN backup "$DB" >/dev/null 2>&1
BK=$(printf '%s\n' "$($BIN backup "$DB" 2>&1)" | grep -o '/tmp/mdrv-db-selftest/[^ ]*recovery/[0-9]*-offline' | head -1)
REST=$T/restored
OUT=$($BIN restore "$BK" "$REST" 2>&1); check "restore to new dir" 0 $?
expect_contains "restore verifies" "$OUT" 'verify'
[ -d "$REST/live/fjall" ] && ok "restored into live/ layout" || bad "restore missing live/fjall"
OUT=$($BIN info "$REST" 2>&1); check "info on restored" 0 $?
$BIN restore "$BK" "$REST" >/dev/null 2>&1; check "restore refuses overwrite" 1 $?

# --- 8. checkpoint / prune
OUT=$($BIN checkpoint "$DB" --compact 2>&1); check "checkpoint --compact" 0 $?
OUT=$($BIN prune "$DB" 2>&1); check "prune" 0 $?
OUT=$($BIN verify "$DB" 2>&1); check "verify after checkpoint" 0 $?

# --- 9. streaming blob put: 120 MiB random file, hash must match sha256sum
HEAD=$(dirname "$DB")
BIG=$T/big.bin
dd if=/dev/urandom of="$BIG" bs=1M count=120 2>/dev/null
WANT=$(sha256sum "$BIG" | cut -d' ' -f1)
OUT=$($BIN blob put "$DB" --file "$BIG" 2>&1); check "blob put 120MiB" 0 $?
GOT=$(printf '%s\n' "$OUT" | cut -d' ' -f1)
SIZE=$(printf '%s\n' "$OUT" | cut -d' ' -f2)
[ "$GOT" = "$WANT" ] && ok "streaming hash == sha256sum" || bad "hash mismatch: $GOT vs $WANT"
[ "$SIZE" = "125829120" ] && ok "byte count exact" || bad "size $SIZE"
OUT=$($BIN blob get "$DB" "$WANT" 2>&1); check "blob get path" 0 $?
P=$(printf '%s\n' "$OUT" | head -1)
cmp -s "$P" "$BIG" && ok "blob content identical" || bad "blob content differs"
# dedup: put again, disk usage must not double
BLOBS="$DB/live/blobs"; [ -d "$BLOBS" ] || BLOBS="$DB/blobs"
DU1=$(du -sk "$BLOBS" | cut -f1)
$BIN blob put "$DB" --file "$BIG" >/dev/null 2>&1
DU2=$(du -sk "$BLOBS" | cut -f1)
[ "$DU1" = "$DU2" ] && ok "dedup: no double storage" || bad "dedup failed ($DU1 -> $DU2 KiB)"
# leftover staging swept?
[ -z "$(ls -A "$BLOBS/.staging" 2>/dev/null)" ] && ok "staging empty after finish" || bad "staging has leftovers"

echo
# --- carapace install (user spec dir; system-wide is ignored by carapace-bin) ---
export XDG_CONFIG_HOME="$T/xdgconf"
if OUT=$($BIN carapace install 2>&1) && [ -s "$T/xdgconf/carapace/specs/mdrv-db.yaml" ]; then
  ok "carapace install writes user spec"
else
  bad "carapace install writes user spec ($OUT)"
fi
unset XDG_CONFIG_HOME

echo "cli-test: $PASS passed, $FAIL failed"
[ $FAIL -eq 0 ]
