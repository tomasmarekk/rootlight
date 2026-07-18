#!/usr/bin/env bash
# Installs the pinned Linux policy toolchain after verifying every release digest.
# Network access is intentionally confined to this acquisition phase.

set -euo pipefail

install_root="${1:-${HOME}/.local/bin}"
mkdir -p "$install_root"
temporary_root="$(mktemp -d)"
trap 'rm -rf "$temporary_root"' EXIT

install_archive() {
    local name="$1"
    local url="$2"
    local sha256="$3"
    local archive_type="$4"
    local binary_path="$5"
    local archive="$temporary_root/${name}.archive"
    local unpacked="$temporary_root/${name}"

    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 "$url" --output "$archive"
    printf '%s  %s\n' "$sha256" "$archive" | sha256sum --check --status
    mkdir -p "$unpacked"
    case "$archive_type" in
        tar.gz) tar -xzf "$archive" -C "$unpacked" ;;
        tar.xz) tar -xJf "$archive" -C "$unpacked" ;;
        *) printf 'unsupported archive type: %s\n' "$archive_type" >&2; return 2 ;;
    esac
    install -m 0755 "$unpacked/$binary_path" "$install_root/$name"
}

install_binary() {
    local name="$1"
    local url="$2"
    local sha256="$3"
    local binary="$temporary_root/$name"

    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 "$url" --output "$binary"
    printf '%s  %s\n' "$sha256" "$binary" | sha256sum --check --status
    install -m 0755 "$binary" "$install_root/$name"
}

install_archive \
    cargo-deny \
    https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz \
    9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f \
    tar.gz \
    cargo-deny-0.20.2-x86_64-unknown-linux-musl/cargo-deny
install_archive \
    cargo-audit \
    https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-unknown-linux-musl-v0.22.2.tgz \
    7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428 \
    tar.gz \
    cargo-audit-x86_64-unknown-linux-musl-v0.22.2/cargo-audit
install_archive \
    cargo-cyclonedx \
    https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/download/cargo-cyclonedx-0.5.9/cargo-cyclonedx-x86_64-unknown-linux-gnu.tar.xz \
    fb8dbee9f182173e062a64a387b21a0badc6fab8b2abf9294973f012972bf6d8 \
    tar.xz \
    cargo-cyclonedx-x86_64-unknown-linux-gnu/cargo-cyclonedx
install_archive \
    gitleaks \
    https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_x64.tar.gz \
    551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb \
    tar.gz \
    gitleaks
install_binary \
    osv-scanner \
    https://github.com/google/osv-scanner/releases/download/v2.4.0/osv-scanner_linux_amd64 \
    15314940c10d26af9c6649f150b8a47c1262e8fc7e17b1d1029b0e479e8ed8a0

geiger_archive="$temporary_root/cargo-geiger.crate"
geiger_source="$temporary_root/cargo-geiger"
curl \
    --fail \
    --silent \
    --show-error \
    --location \
    --proto '=https' \
    --tlsv1.2 \
    https://static.crates.io/crates/cargo-geiger/cargo-geiger-0.13.0.crate \
    --output "$geiger_archive"
printf '%s  %s\n' \
    f36131e0c6e5b9464ca742a88c697b07b3a387e72fc05ff50850279ba52d8879 \
    "$geiger_archive" | sha256sum --check --status
mkdir -p "$geiger_source"
tar -xzf "$geiger_archive" -C "$geiger_source" --strip-components=1
printf '%s  %s\n' \
    e87104c9738f274e7f20e294027c863556bc9e41a4f60044f8b68898ba97a477 \
    scripts/cargo-geiger-0.13.0.lock | sha256sum --check --status
cp scripts/cargo-geiger-0.13.0.lock "$geiger_source/Cargo.lock"
cargo install \
    --locked \
    --path "$geiger_source" \
    --root "$(dirname "$install_root")"

geiger_binary="$install_root/cargo-geiger"
geiger_version="$("$geiger_binary" --version)"
if [[ "$geiger_version" != "cargo-geiger 0.13.0" ]]; then
    printf 'unsupported installed cargo-geiger version: %s\n' "$geiger_version" >&2
    exit 1
fi
python - \
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
digest = hashlib.sha256(binary.read_bytes()).hexdigest()
identity = {
    "schema_version": "1.0",
    "tool": "cargo-geiger",
    "version": sys.argv[3],
    "executable_sha256": digest,
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

if [[ -n "${GITHUB_PATH:-}" ]]; then
    printf '%s\n' "$install_root" >> "$GITHUB_PATH"
fi
