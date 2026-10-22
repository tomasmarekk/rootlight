#!/usr/bin/env bash
# Exports verified versioned Rust sources without acquisition or compilation.

set -euo pipefail

if [[ $# -gt 1 || ($# -eq 1 && $1 != --remove-staging-after-archive) ]]; then
    printf '%s\n' 'usage: prepare-offline.sh [--remove-staging-after-archive]' >&2
    exit 2
fi

python3 scripts/prepare_rust_source_bundle.py \
    --derive-cargo-members-from-archives \
    --destination target/rust-source-bundle/rootlight \
    --archive target/rust-source-bundle/rootlight-rust-sources.tar.gz "$@"
