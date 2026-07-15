#!/usr/bin/env bash
# Inventories unsafe usage for every workspace package, including disconnected members.
# Each report is validated independently so package selection cannot silently narrow coverage.

set -euo pipefail

export CARGO_BUILD_JOBS=1
output_root="${1:-artifacts/geiger}"
rm -rf "$output_root"
mkdir -p "$output_root"

cargo metadata --locked --no-deps --format-version 1 > "$output_root/metadata.json"
python - "$output_root/metadata.json" "$output_root/workspace-packages.tsv" <<'PY'
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
rm "$output_root/metadata.json"

while IFS=$'\t' read -r package manifest; do
    cargo geiger \
        --manifest-path "$manifest" \
        --all-features \
        --all-targets \
        --all-dependencies \
        --locked \
        --offline \
        --output-format Json \
        2> "$output_root/$package.log" \
        | python scripts/validate-geiger.py \
            --required-workspace-package "$package"
done < "$output_root/workspace-packages.tsv"
