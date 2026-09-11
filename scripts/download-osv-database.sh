#!/usr/bin/env bash
# Acquires the reviewed crates.io OSV snapshot without publishing partial downloads.

set -euo pipefail

generation="1789077802747690"
expected_sha256="7d998e8daced17be350fda073c0ee2133c64940ff3676c15e9426a536a54f0dc"
expected_bytes=3434677
cache_root="${1:-artifacts/osv-db}"
destination="$cache_root/osv-scanner/crates.io/all.zip"
digest_path="$cache_root/osv-scanner/crates.io/all.zip.sha256"
mkdir -p "$(dirname "$destination")"
archive="$(mktemp "$(dirname "$destination")/.all.zip.XXXXXX")"
digest=""
cleanup() {
    rm -f -- "$archive" "$digest"
}
trap cleanup EXIT
digest="$(mktemp "$(dirname "$destination")/.all.zip.sha256.XXXXXX")"

# A pinned generation can disappear upstream; a matching local snapshot remains valid.
if [[ ! -f "$destination" || -L "$destination" ]] ||
    ! printf '%s  %s\n' "$expected_sha256" "$destination" | sha256sum --check --status; then
    curl \
        --fail \
        --silent \
        --show-error \
        --location \
        --retry 3 \
        --retry-all-errors \
        --retry-delay 2 \
        --retry-max-time 30 \
        --connect-timeout 15 \
        --max-time 60 \
        --max-filesize "$expected_bytes" \
        --proto '=https' \
        --tlsv1.2 \
        "https://storage.googleapis.com/download/storage/v1/b/osv-vulnerabilities/o/crates.io%2Fall.zip?generation=$generation&alt=media" \
        --output "$archive"
    printf '%s  %s\n' "$expected_sha256" "$archive" | sha256sum --check --status
    mv -fT -- "$archive" "$destination"
fi
printf '%s  all.zip\n' "$expected_sha256" > "$digest"
mv -fT -- "$digest" "$digest_path"
