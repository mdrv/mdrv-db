#!/usr/bin/env bash
# All manifests must equal the workspace version in root Cargo.toml.
set -euo pipefail
cd "$(dirname "$0")/.."
V=$(grep -m1 '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/')
fail=0
for f in packages/db/package.json packages/db-config/package.json packages/db-events/package.json; do
  if ! grep -q "\"version\": \"$V\"" "$f"; then
    echo "version mismatch in $f (expected $V)" >&2
    fail=1
  fi
done
[ "$fail" -eq 0 ] && echo "versions in sync: $V"
exit $fail
