#!/usr/bin/env bash
# Downloads and verifies the reviewed RustSec advisory database snapshot.

set -euo pipefail

snapshot="9f3e138091487e69144f536d36976e427a7a3307"
expected_sha256="b0eb048042adf7d7c06ad426195ebd7a8c2dd946eb53fde658cb45a3bc82f265"
destination="${1:-artifacts/rustsec-db}"
archive="$(mktemp)"
cleanup() {
    rm -f "$archive"
}
trap cleanup EXIT

rm -rf "$destination"
mkdir -p "$destination"
curl \
    --fail \
    --silent \
    --show-error \
    --location \
    --proto '=https' \
    --tlsv1.2 \
    "https://github.com/RustSec/advisory-db/archive/$snapshot.tar.gz" \
    --output "$archive"
printf '%s  %s\n' "$expected_sha256" "$archive" | sha256sum --check --status
tar -xzf "$archive" -C "$destination" --strip-components=1
printf '%s\n' "$snapshot" > "$destination/ROOTLIGHT_SNAPSHOT"
