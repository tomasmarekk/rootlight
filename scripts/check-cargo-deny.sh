#!/usr/bin/env bash
# Checks the complete graph using the repository's local source replacement.

set -euo pipefail

# cargo-deny 0.20.2 compares registry URLs literally. Directory sources retain
# Cargo's canonical Git crates.io identity even when the index defaults to sparse.
# This changes policy matching only; --offline and source replacement still apply.
CARGO_REGISTRIES_CRATES_IO_PROTOCOL=git \
    cargo deny --locked --offline "$@" check bans licenses sources
