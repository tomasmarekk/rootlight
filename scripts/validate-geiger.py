#!/usr/bin/env python3
"""Validates cargo-geiger JSON without publishing host-specific scan paths."""

from __future__ import annotations

import argparse
import json
import sys


def unsafe_count(unsafety: dict[str, object]) -> int:
    total = 0
    for usage in ("used", "unused"):
        metrics = unsafety[usage]
        if not isinstance(metrics, dict):
            raise ValueError(f"invalid {usage} metrics")
        for counts in metrics.values():
            if not isinstance(counts, dict):
                raise ValueError("invalid unsafe count object")
            total += int(counts.get("unsafe_", 0))
    return total


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--required-workspace-package", required=True)
    args = parser.parse_args()

    report = json.load(sys.stdin)
    if report.get("packages_without_metrics"):
        print("unsafe inventory omitted package metrics", file=sys.stderr)
        return 1

    workspace_packages: set[str] = set()
    for entry in report.get("packages", []):
        package = entry.get("package", {})
        identifier = package.get("id", {})
        source = identifier.get("source")
        if not isinstance(source, dict) or "Path" not in source:
            continue
        name = identifier.get("name")
        if not isinstance(name, str):
            print("unsafe inventory contains a path package without a name", file=sys.stderr)
            return 1
        workspace_packages.add(name)
        unsafety = entry.get("unsafety", {})
        if not unsafety.get("forbids_unsafe") or unsafe_count(unsafety) != 0:
            print(f"workspace package {name} permits or uses unsafe code", file=sys.stderr)
            return 1

    required = args.required_workspace_package
    if required not in workspace_packages:
        print(
            f"unsafe inventory omitted required workspace package {required}; "
            f"observed {sorted(workspace_packages)}",
            file=sys.stderr,
        )
        return 1

    print(
        f"unsafe inventory verified {required} across "
        f"{len(report.get('packages', []))} resolved packages"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
