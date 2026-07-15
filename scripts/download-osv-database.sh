#!/usr/bin/env bash
# Downloads and verifies the reviewed immutable crates.io OSV database generation.

set -euo pipefail

generation="1784055228546157"
expected_sha256="d53bbd8a1a90fb78a803ad2877b7295dc175e6f121e5970e5646cd7a3f7e9d90"
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
