#!/usr/bin/env bash
# Runs the complete current core suite with Cargo network access disabled.
# Linux CI additionally executes this script inside a network namespace.

set -euo pipefail

export CARGO_NET_OFFLINE=true
export CARGO_HTTP_TIMEOUT=1
export CARGO_NET_RETRY=0

run_xtask() {
    cargo run --locked --offline --quiet --package xtask -- "$@"
}

printf 'offline cargo environment: CARGO_HOME=%s RUSTUP_HOME=%s\n' \
    "${CARGO_HOME:-unset}" "${RUSTUP_HOME:-unset}"
cargo metadata --locked --offline --format-version 1 > /dev/null
cargo test --workspace --all-features --locked --offline
run_xtask architecture-check
run_xtask generate --check
run_xtask compatibility-check
run_xtask policy-check
run_xtask id-vectors > target/id-vectors.actual.json
cmp tests/fixtures/id-vectors.json target/id-vectors.actual.json
