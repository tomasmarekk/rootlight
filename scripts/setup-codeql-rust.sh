#!/usr/bin/env bash
# Acquires the authenticated scanner prerequisite, including its published query packs.
# Product compilation never uses this tool archive as a dependency source.
set -euo pipefail

destination="${1:?usage: setup-codeql-rust.sh DESTINATION}"
mkdir -p "$destination"
archive="$destination/codeql-rust.tar.zst"
curl --fail --location --retry 3 --proto '=https' --tlsv1.2 \
  'https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.27.2/codeql-bundle-rust-linux64.tar.zst' \
  --output "$archive"
printf '%s  %s\n' 'acf2050189aeede5b9aa566f8fb382b02ac1ace72f534778f68b06fce5371cea' "$archive" | sha256sum --check --strict
tar --extract --file "$archive" --directory "$destination"
"$destination/codeql/codeql" version --format=json
if [[ -n "${GITHUB_ENV:-}" ]]; then
  printf 'CODEQL=%s/codeql/codeql\nCODEQL_BUNDLE=%s\n' "$destination" "$archive" >> "$GITHUB_ENV"
fi
