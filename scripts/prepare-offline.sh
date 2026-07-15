#!/usr/bin/env bash
# Fetches all Cargo inputs needed by the subsequent no-egress verification phase.

set -euo pipefail

cargo fetch --locked --target x86_64-unknown-linux-gnu
cargo fetch --locked --target aarch64-apple-darwin
cargo fetch --locked --target x86_64-apple-darwin
cargo fetch --locked --target x86_64-pc-windows-msvc
