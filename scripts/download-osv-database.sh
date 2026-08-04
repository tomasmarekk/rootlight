#!/usr/bin/env bash
# Downloads and verifies the reviewed immutable crates.io OSV database generation.

set -euo pipefail

generation="1785774003284492"
expected_sha256="e864f02179aeb85e5e884bc9411f1647e3d60087d248b8d3776f2ddcd906ed0e"
cache_root="${1:-artifacts/osv-db}"
destination="$cache_root/osv-scanner/crates.io/all.zip"
digest_path="$cache_root/osv-scanner/crates.io/all.zip.sha256"
mkdir -p "$(dirname "$destination")"
curl \
    --fail \
    --silent \
    --show-error \
    --location \
    --proto '=https' \
    --tlsv1.2 \
    "https://storage.googleapis.com/download/storage/v1/b/osv-vulnerabilities/o/crates.io%2Fall.zip?generation=$generation&alt=media" \
    --output "$destination"
printf '%s  %s\n' "$expected_sha256" "$destination" | sha256sum --check --status
(
    cd "$(dirname "$destination")"
    sha256sum -- "$(basename "$destination")" > "$(basename "$digest_path")"
)
