#!/usr/bin/env bash
# Downloads and verifies the reviewed RustSec advisory database snapshot.

set -euo pipefail

snapshot="25a288f68f2d5ebe658b243a334110c7217a2569"
expected_sha256="050966b5085577e5cc1d0f534c6054d2ed596bd950b338cece9f793880d673de"
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
    --retry 3 \
    --retry-all-errors \
    --retry-delay 2 \
    --retry-max-time 30 \
    --proto '=https' \
    --tlsv1.2 \
    "https://github.com/RustSec/advisory-db/archive/$snapshot.tar.gz" \
    --output "$archive"
printf '%s  %s\n' "$expected_sha256" "$archive" | sha256sum --check --status
tar -xzf "$archive" -C "$destination" --strip-components=1
printf '%s\n' "$snapshot" > "$destination/ROOTLIGHT_SNAPSHOT"
