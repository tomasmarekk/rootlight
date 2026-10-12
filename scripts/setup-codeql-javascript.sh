#!/usr/bin/env bash
# Acquires the authenticated scanner prerequisite, including its published query packs.
# Product compilation never uses this tool archive as a dependency source.
set -euo pipefail

destination="${1:?usage: setup-codeql-javascript.sh DESTINATION}"
mkdir -p "$destination"
archive="$destination/codeql-javascript.tar.zst"
curl --fail --location --retry 3 --proto '=https' --tlsv1.2 \
  'https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.27.2/codeql-bundle-javascript-linux64.tar.zst' \
  --output "$archive"
printf '%s  %s\n' 'c85010379fa10172b7a59c6689171cbfe692bcd1a2ab92cdc902e83c375deab5' "$archive" | sha256sum --check --strict
tar --extract --file "$archive" --directory "$destination"
"$destination/codeql/codeql" version --format=json
if [[ -n "${GITHUB_ENV:-}" ]]; then
  printf 'CODEQL=%s/codeql/codeql\nCODEQL_BUNDLE=%s\n' "$destination" "$archive" >> "$GITHUB_ENV"
fi
