#!/usr/bin/env bash
# Builds the reviewed cargo-geiger source on the current host and records its identity.

set -euo pipefail

if (( $# > 1 )); then
    printf 'usage: %s [INSTALL_ROOT]\n' "$0" >&2
    exit 2
fi

install_root="${1:-${HOME}/.local/bin}"
mkdir -p "$install_root"
temporary_root="$(mktemp -d)"
trap 'rm -rf "$temporary_root"' EXIT

case "$(uname -s)" in
    CYGWIN*|MINGW*|MSYS*) executable_suffix=".exe" ;;
    *) executable_suffix="" ;;
esac

python_executable="python3"
if ! command -v "$python_executable" >/dev/null 2>&1; then
    python_executable="python"
fi
if ! command -v "$python_executable" >/dev/null 2>&1; then
    printf 'Python is required to install cargo-geiger\n' >&2
    exit 1
fi

verify_sha256() {
    local expected="$1"
    local path="$2"
    "$python_executable" - "$expected" "$path" <<'PY'
import hashlib
import hmac
import pathlib
import sys

expected = sys.argv[1]
path = pathlib.Path(sys.argv[2])
digest = hashlib.sha256()
with path.open("rb") as source:
    for chunk in iter(lambda: source.read(1024 * 1024), b""):
        digest.update(chunk)
if not hmac.compare_digest(digest.hexdigest(), expected):
    raise SystemExit("cargo-geiger input SHA-256 differs")
PY
}

geiger_archive="$temporary_root/cargo-geiger.crate"
geiger_source="$temporary_root/cargo-geiger"
geiger_patch="$(pwd -P)/scripts/cargo-geiger-0.13.0-package-id.patch"
curl \
    --fail \
    --silent \
    --show-error \
    --location \
    --proto '=https' \
    --tlsv1.2 \
    https://static.crates.io/crates/cargo-geiger/cargo-geiger-0.13.0.crate \
    --output "$geiger_archive"
verify_sha256 \
    f36131e0c6e5b9464ca742a88c697b07b3a387e72fc05ff50850279ba52d8879 \
    "$geiger_archive"
mkdir -p "$geiger_source"
tar -xzf "$geiger_archive" -C "$geiger_source" --strip-components=1
verify_sha256 \
    e87104c9738f274e7f20e294027c863556bc9e41a4f60044f8b68898ba97a477 \
    scripts/cargo-geiger-0.13.0.lock
verify_sha256 \
    0e32a439da0c2bf2954f43a061771dd9d21cd9c11edd37695b57f5055f28f9fb \
    "$geiger_patch"
cp scripts/cargo-geiger-0.13.0.lock "$geiger_source/Cargo.lock"
git -C "$geiger_source" apply --check --unidiff-zero --whitespace=error-all "$geiger_patch"
git -C "$geiger_source" apply --unidiff-zero --whitespace=error-all "$geiger_patch"
cargo install \
    --locked \
    --path "$geiger_source" \
    --root "$temporary_root/cargo-geiger-install"
cp \
    "$temporary_root/cargo-geiger-install/bin/cargo-geiger$executable_suffix" \
    "$install_root/cargo-geiger$executable_suffix"
chmod 0755 "$install_root/cargo-geiger$executable_suffix"

geiger_binary="$install_root/cargo-geiger$executable_suffix"
geiger_version="$("$geiger_binary" --version)"
if [[ "$geiger_version" != "cargo-geiger 0.13.0" ]]; then
    printf 'unsupported installed cargo-geiger version: %s\n' "$geiger_version" >&2
    exit 1
fi
"$python_executable" - \
    "$geiger_binary" \
    "$install_root/cargo-geiger.identity.json" \
    "$geiger_version" <<'PY'
import hashlib
import json
import os
import pathlib
import stat
import sys

binary = pathlib.Path(sys.argv[1]).resolve(strict=True)
identity = {
    "schema_version": "1.0",
    "tool": "cargo-geiger",
    "version": sys.argv[3],
    "executable_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    "source_url": (
        "https://static.crates.io/crates/cargo-geiger/"
        "cargo-geiger-0.13.0.crate"
    ),
    "source_sha256": (
        "f36131e0c6e5b9464ca742a88c697b07b3a387e72fc05ff50850279ba52d8879"
    ),
    "lockfile": "scripts/cargo-geiger-0.13.0.lock",
    "lockfile_sha256": (
        "e87104c9738f274e7f20e294027c863556bc9e41a4f60044f8b68898ba97a477"
    ),
    "patch": "scripts/cargo-geiger-0.13.0-package-id.patch",
    "patch_sha256": (
        "0e32a439da0c2bf2954f43a061771dd9d21cd9c11edd37695b57f5055f28f9fb"
    ),
}
identity_path = pathlib.Path(sys.argv[2])
if os.path.lexists(identity_path):
    metadata = identity_path.lstat()
    reparse = getattr(metadata, "st_file_attributes", 0) & getattr(
        stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0
    )
    if stat.S_ISLNK(metadata.st_mode) or reparse:
        raise SystemExit("cargo-geiger install identity must not be an alias")
identity_path.write_text(
    json.dumps(identity, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
    newline="\n",
)
PY
