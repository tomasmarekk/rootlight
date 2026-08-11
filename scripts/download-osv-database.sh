#!/usr/bin/env bash
# Downloads and verifies the reviewed immutable crates.io OSV database generation.

set -euo pipefail

generation="1786447137307581"
expected_sha256="b2292c99556b9c3834e89506c378b547e722edeb51b02198f4600e0a9024eff5"
cache_root="${1:-artifacts/osv-db}"
destination="$cache_root/osv-scanner/crates.io/all.zip"
digest_path="$cache_root/osv-scanner/crates.io/all.zip.sha256"
mkdir -p "$(dirname "$destination")"
curl \
    --fail \
    --silent \
    --show-error \
    --location \
    --retry 3 \
    --retry-all-errors \
    --retry-delay 2 \
    --retry-max-time 30 \
    --proto '=https' \
    --tlsv1.2 \
    "https://storage.googleapis.com/download/storage/v1/b/osv-vulnerabilities/o/crates.io%2Fall.zip?generation=$generation&alt=media" \
    --output "$destination"
printf '%s  %s\n' "$expected_sha256" "$destination" | sha256sum --check --status
(
    cd "$(dirname "$destination")"
    sha256sum -- "$(basename "$destination")" > "$(basename "$digest_path")"
)
