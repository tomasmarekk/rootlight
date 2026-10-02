#!/usr/bin/env bash
# Exports verified versioned Rust sources without acquisition or compilation.

set -euo pipefail

python3 scripts/prepare_rust_source_bundle.py \
    --destination target/rust-source-bundle/rootlight \
    --archive target/rust-source-bundle/rootlight-rust-sources.tar.gz
