# mdrv-db task runner — `just --list` shows everything.
# Recipes are thin wrappers around the gates documented in DEVELOPMENT.md.
set shell := ["bash", "-cu"]

# list available recipes
default:
    @just --list

# full dev gate: fmt + clippy (default and all-features) + tests
gate:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy --workspace --all-features --all-targets -- -D warnings
    cargo test --workspace --all-features

# build the admin console bundle (packages/console/dist is the embed source)
console:
    cd packages/console && bun install && bun run build

# release binary with the turso port (build.rs syncs console-dist automatically)
build:
    cargo build --release -p mdrv-db --features turso

# release binary + the full CLI test suite
citest: build
    bash scripts/cli-test.sh

# run the admin daemon + console from the local release binary (needs MDRV_DB_ADMIN_TOKEN)
run config port="8300" host="127.0.0.1":
    MDRV_DB_ADMIN_TOKEN="${MDRV_DB_ADMIN_TOKEN:?set MDRV_DB_ADMIN_TOKEN}" \
      ./target/release/mdrv-db --config {{config}} \
      daemon run --bind {{host}} --port {{port}} --console packages/console/dist

# release readiness: manifests in sync + crates.io packaging dry run
publish-dry:
    scripts/version-check.sh
    cargo publish --dry-run --locked -p mdrv-db
