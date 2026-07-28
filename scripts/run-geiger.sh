#!/usr/bin/env bash
# Inventories unsafe usage for every workspace package, including disconnected members.
# Each report is validated independently so package selection cannot silently narrow coverage.

set -euo pipefail

export CARGO_BUILD_JOBS=1
python_executable="python3"
if ! command -v "$python_executable" >/dev/null 2>&1; then
    python_executable="python"
fi
if ! command -v "$python_executable" >/dev/null 2>&1; then
    printf 'Python is required to inventory unsafe usage\n' >&2
    exit 1
fi
# Embedded Cargo may execute rustc from dependency source directories that
# contain their own toolchain files. Keep every scan on the reviewed toolchain.
pinned_toolchain="$(
    "$python_executable" -c \
        'import pathlib,tomllib; print(tomllib.loads(pathlib.Path("rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"])'
)"
readonly pinned_toolchain
if [[ ! "$pinned_toolchain" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'unsupported pinned Rust toolchain: %s\n' "$pinned_toolchain" >&2
    exit 1
fi
export RUSTUP_TOOLCHAIN="$pinned_toolchain"
if (( $# < 1 || $# > 3 )); then
    printf 'usage: %s ABSOLUTE_CARGO_GEIGER [OUTPUT_ROOT] [PACKAGE]\n' "$0" >&2
    exit 2
fi
trusted_geiger="$1"
output_root="${2:-artifacts/geiger}"
package_filter="${3:-}"

rm -rf "$output_root"
mkdir -p "$output_root"
execution_identity="$output_root/cargo-geiger.execution.json"
"$python_executable" scripts/validate-geiger.py preflight \
    --trusted-cargo-geiger "$trusted_geiger" \
    --cargo-config .cargo/config.toml \
    --unsafe-policy policy/unsafe.toml \
    --toolchain-policy policy/toolchain.toml \
    --execution-identity "$execution_identity"

cargo metadata --locked --no-deps --format-version 1 > "$output_root/metadata.json"
"$python_executable" - \
    "$output_root/metadata.json" \
    "$output_root/workspace-packages.json" \
    "$output_root/workspace-packages.tsv" <<'PY'
import json
import pathlib
import sys

document = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
workspace_members = document["workspace_members"]
workspace = set(workspace_members)
if len(workspace) != len(workspace_members):
    raise SystemExit("Cargo metadata contains duplicate workspace member IDs")
packages = sorted(
    [
        {
            "cargo_id": package["id"],
            "name": package["name"],
            "version": package["version"],
            "manifest": str(
                pathlib.Path(package["manifest_path"]).resolve(strict=True)
            ),
        }
        for package in document["packages"]
        if package["id"] in workspace
    ],
    key=lambda package: package["cargo_id"],
)
observed = {package["cargo_id"] for package in packages}
if observed != workspace:
    missing = sorted(workspace - observed)
    unexpected = sorted(observed - workspace)
    raise SystemExit(
        f"Cargo metadata workspace inventory mismatch; "
        f"missing={missing}, unexpected={unexpected}"
    )
names = [package["name"] for package in packages]
if len(names) != len(set(names)):
    raise SystemExit("workspace package names must be unique for report artifacts")
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(
        {"schema_version": "1.0", "workspace_members": packages},
        indent=2,
        sort_keys=True,
    )
    + "\n",
    encoding="utf-8",
    newline="\n",
)
pathlib.Path(sys.argv[3]).write_text(
    "".join(
        f"{package['cargo_id']}\t{package['name']}\t"
        f"{package['version']}\t{package['manifest']}\n"
        for package in packages
    ),
    encoding="utf-8",
    newline="\n",
)
PY
rm "$output_root/metadata.json"

"$python_executable" scripts/test-validate-geiger.py

scanned_packages=0
while IFS=$'\t' read -r cargo_id package version manifest; do
    if [[ -n "$package_filter" && "$package" != "$package_filter" ]]; then
        continue
    fi
    scanned_packages=$((scanned_packages + 1))
    report="$output_root/$package-$version.report.json"
    log="$output_root/$package-$version.log"
    evidence="$output_root/$package-$version.evidence.json"
    "$python_executable" scripts/validate-geiger.py scan \
        --trusted-cargo-geiger "$trusted_geiger" \
        --cargo-config .cargo/config.toml \
        --unsafe-policy policy/unsafe.toml \
        --toolchain-policy policy/toolchain.toml \
        --execution-identity "$execution_identity" \
        --manifest "$manifest" \
        --report "$report" \
        --log "$log"
    "$python_executable" scripts/validate-geiger.py prepare \
        --trusted-cargo-geiger "$trusted_geiger" \
        --required-workspace-package-id "$cargo_id" \
        --workspace-inventory "$output_root/workspace-packages.json" \
        --unsafe-policy policy/unsafe.toml \
        --toolchain-policy policy/toolchain.toml \
        --cargo-lock Cargo.lock \
        --cargo-config .cargo/config.toml \
        --rust-toolchain rust-toolchain.toml \
        --execution-identity "$execution_identity" \
        --report "$report" \
        --evidence-envelope "$evidence"
    "$python_executable" scripts/validate-geiger.py validate \
        --trusted-cargo-geiger "$trusted_geiger" \
        --required-workspace-package-id "$cargo_id" \
        --workspace-inventory "$output_root/workspace-packages.json" \
        --unsafe-policy policy/unsafe.toml \
        --toolchain-policy policy/toolchain.toml \
        --cargo-lock Cargo.lock \
        --cargo-config .cargo/config.toml \
        --rust-toolchain rust-toolchain.toml \
        --execution-identity "$execution_identity" \
        --report "$report" \
        --evidence-envelope "$evidence"
done < "$output_root/workspace-packages.tsv"

if [[ -n "$package_filter" && "$scanned_packages" -ne 1 ]]; then
    printf 'package filter must match exactly one workspace package: %s\n' \
        "$package_filter" >&2
    exit 1
fi
