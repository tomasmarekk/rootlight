#!/usr/bin/env bash
# Generates normalized CycloneDX inputs for every Cargo workspace package.
# Absolute checkout paths and wall-clock timestamps are removed before comparison.

set -euo pipefail

output_root="${1:-artifacts/sbom}"
workspace_root="$(pwd -P)"
rm -rf "$output_root"
mkdir -p "$output_root"

metadata="$(mktemp)"
manifest_list="$(mktemp)"
cleanup() {
    rm -f "$metadata" "$manifest_list"
}
trap cleanup EXIT

cargo metadata --locked --no-deps --format-version 1 > "$metadata"
python - "$metadata" "$manifest_list" <<'PY'
import json
import pathlib
import sys

document = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
workspace = set(document["workspace_members"])
rows = sorted(
    (package["name"], package["manifest_path"])
    for package in document["packages"]
    if package["id"] in workspace
)
pathlib.Path(sys.argv[2]).write_text(
    "".join(f"{name}\t{manifest}\n" for name, manifest in rows),
    encoding="utf-8",
    newline="\n",
)
PY

export SOURCE_DATE_EPOCH=0
while IFS=$'\t' read -r package_name manifest; do
    package_dir="$(dirname "$manifest")"
    generated="$package_dir/${package_name}_all.cdx.json"
    destination="$output_root/${package_name}.cdx.json"
    cargo cyclonedx \
        --manifest-path "$manifest" \
        --all-features \
        --target all \
        --target-in-filename \
        --format json \
        --spec-version 1.5
    python - "$generated" "$destination" "$workspace_root" <<'PY'
import json
import pathlib
import re
import sys

source = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
workspace = sys.argv[3].replace("\\", "/")
document = json.loads(source.read_text(encoding="utf-8"))
absolute_windows = re.compile(r"^(?:file:///)?[A-Za-z]:/")

def normalize(value):
    if isinstance(value, dict):
        return {key: normalize(item) for key, item in value.items()}
    if isinstance(value, list):
        return [normalize(item) for item in value]
    if isinstance(value, str):
        normalized = value.replace("\\", "/").replace(workspace, "${WORKSPACE}")
        if normalized.startswith("/") or absolute_windows.match(normalized):
            raise ValueError(f"SBOM contains an absolute path: {normalized}")
        return normalized
    return value

destination.write_text(
    json.dumps(normalize(document), indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
    newline="\n",
)
source.unlink()
PY
done < "$manifest_list"

(
    cd "$output_root"
    sha256sum -- *.cdx.json > SHA256SUMS
)
