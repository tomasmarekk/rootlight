#!/usr/bin/env bash
# Stages the reviewed local crates.io OSV snapshot without publishing partial data.

set -euo pipefail

generation="1791796673905677"
expected_sha256="be70cb6ddcf93003efa65fff3196f342c3fc5cbb2779f67404de8ae688c7a330"
expected_bytes=3560422
cache_root="${1:-artifacts/osv-db}"
project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source_archive="$project_root/source-inputs/advisory-databases/osv-crates-io-$generation.zip"

# GCS generations expire; cold CI uses the exact reviewed versioned snapshot.
[[ -f "$source_archive" && ! -L "$source_archive" ]]
[[ "$(wc -c < "$source_archive")" -eq "$expected_bytes" ]]
printf '%s  %s\n' "$expected_sha256" "$source_archive" | sha256sum --check --status
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

if [[ ! -f "$destination" || -L "$destination" ]] ||
    ! printf '%s  %s\n' "$expected_sha256" "$destination" | sha256sum --check --status; then
    cp -- "$source_archive" "$archive"
    printf '%s  %s\n' "$expected_sha256" "$archive" | sha256sum --check --status
    mv -fT -- "$archive" "$destination"
fi
printf '%s  all.zip\n' "$expected_sha256" > "$digest"
mv -fT -- "$digest" "$digest_path"
