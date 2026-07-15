#!/usr/bin/env bash
# Fetches all Cargo inputs needed by the subsequent no-egress verification phase.

set -euo pipefail

# Cargo metadata resolves the complete lockfile graph, including dependencies that
# are inactive on the runner target, so target-filtered fetches are insufficient.
cargo fetch --locked
