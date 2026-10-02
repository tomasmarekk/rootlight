#!/usr/bin/env bash
# Verifies project-derived sources with fresh caches and kernel egress denial.

set -euo pipefail

export CARGO_NET_OFFLINE=true
export CARGO_HTTP_TIMEOUT=1
export CARGO_NET_RETRY=0
export PYTHONDONTWRITEBYTECODE=1

run_xtask() {
    cargo run --locked --offline --quiet --package xtask -- "$@"
}

printf 'offline cargo environment: CARGO_HOME=%s RUSTUP_HOME=%s\n' \
    "${CARGO_HOME:-unset}" "${RUSTUP_HOME:-unset}"
if [[ "${REQUIRE_BLOCKED_EGRESS:-0}" != "1" ]]; then
    printf 'the source-bundle gate requires independent egress denial\n' >&2
    exit 1
fi
for directory in "${CARGO_HOME:?CARGO_HOME is required}" \
    "${CARGO_TARGET_DIR:?CARGO_TARGET_DIR is required}"; do
    if [[ -L "$directory" || ! -d "$directory" ]]; then
        printf 'acceptance requires an existing empty directory: %s\n' "$directory" >&2
        exit 1
    fi
    if ! contents="$(find -H "$directory" -mindepth 1 -print -quit)"; then
        printf 'acceptance could not inspect the empty directory: %s\n' "$directory" >&2
        exit 1
    fi
    if [[ -n "$contents" ]]; then
        printf 'acceptance requires an existing empty directory: %s\n' "$directory" >&2
        exit 1
    fi
done
if [[ "${REQUIRE_BLOCKED_EGRESS:-0}" == "1" ]]; then
    network_output="$(bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>&1 || true)"
    if ! grep --extended-regexp --quiet \
        'Network is unreachable|Permission denied|Operation not permitted' \
        <<<"$network_output"; then
        printf 'egress sentinel did not fail closed:\n%s\n' "$network_output" >&2
        exit 1
    fi
fi
python3 scripts/prepare_rust_source_bundle.py --destination "$PWD" --verify-only
cargo metadata --locked --offline --format-version 1 > /dev/null
cargo test --workspace --all-features --locked --offline
cargo test --manifest-path tests/native-grammars/Cargo.toml --locked --offline
cargo metadata --manifest-path fuzz/Cargo.toml --locked --offline \
    --format-version 1 > /dev/null
run_xtask architecture-check
run_xtask generate --check
run_xtask compatibility-check
run_xtask policy-check
run_xtask id-vectors > "$CARGO_TARGET_DIR/id-vectors.actual.json"
cmp tests/fixtures/id-vectors.json "$CARGO_TARGET_DIR/id-vectors.actual.json"
registry="$CARGO_HOME/registry"
marker="$registry/CACHEDIR.TAG"
expected_marker=$'Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n# For information about cache directory tags see https://bford.info/cachedir/'
# Cargo initializes this administrative marker even for directory-only sources;
# no registry index, expanded source, or downloaded archive is permissible.
unexpected_registry_entries=""
if [[ -d "$registry" && ! -L "$registry" ]]; then
    if ! unexpected_registry_entries="$(find "$registry" -mindepth 1 ! -path "$marker" -print -quit)"; then
        printf 'acceptance could not inspect the registry directory\n' >&2
        exit 1
    fi
fi
if [[ -e "$CARGO_HOME/git" || -L "$CARGO_HOME/git" || -L "$registry" ]] || \
    { [[ -e "$registry" ]] && \
      { [[ ! -d "$registry" ]] || \
        [[ -n "$unexpected_registry_entries" ]] || \
        [[ -L "$marker" || ! -f "$marker" ]] || \
        ! cmp --silent -- "$marker" <(printf '%s\n' "$expected_marker"); }; }; then
    printf 'the build populated an external-source cache\n' >&2
    exit 1
fi
python3 scripts/prepare_rust_source_bundle.py --destination "$PWD" --verify-only
