#!/usr/bin/env python3
"""Validates cargo-geiger JSON without publishing host-specific scan paths."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import tomllib


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
    parser.add_argument("--unsafe-policy", required=True)
    args = parser.parse_args()

    policy_path = pathlib.Path(args.unsafe_policy)
    policy = tomllib.loads(policy_path.read_text(encoding="utf-8"))
    if policy.get("schema_version") != "1.0":
        print("unsafe inventory policy has an unsupported version", file=sys.stderr)
        return 1
    approved_counts: dict[str, int] = {}
    for boundary in policy.get("boundaries", []):
        package = boundary.get("package")
        status = boundary.get("status")
        count = boundary.get("expected_geiger_count")
        if (
            not isinstance(package, str)
            or status not in {"proposed", "accepted"}
            or not isinstance(count, int)
            or count < 0
            or (status == "proposed" and count != 0)
            or (status == "accepted" and count == 0)
        ):
            print("unsafe inventory policy contains an invalid boundary", file=sys.stderr)
            return 1
        if status == "accepted":
            approved_counts[package] = approved_counts.get(package, 0) + count

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
        observed_count = unsafe_count(unsafety)
        if name in approved_counts:
            if observed_count != approved_counts[name]:
                print(
                    f"workspace package {name} expected "
                    f"{approved_counts[name]} unsafe uses, observed {observed_count}",
                    file=sys.stderr,
                )
                return 1
        elif not unsafety.get("forbids_unsafe") or observed_count != 0:
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
