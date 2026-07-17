#!/usr/bin/env python3
"""Validate cargo-geiger output against the exact Cargo workspace inventory."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sys
import tomllib
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class WorkspacePackage:
    """One package identity emitted by the same Cargo metadata invocation."""

    cargo_id: str
    name: str
    version: str
    manifest: pathlib.Path

    @property
    def root(self) -> pathlib.Path:
        return self.manifest.parent


def fail(message: str) -> ValueError:
    return ValueError(message)


def require_object(value: Any, description: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise fail(f"{description} must be an object")
    return value


def require_string(value: Any, description: str) -> str:
    if not isinstance(value, str) or not value:
        raise fail(f"{description} must be a non-empty string")
    return value


def unsafe_count(unsafety: dict[str, Any]) -> int:
    total = 0
    for usage in ("used", "unused"):
        metrics = require_object(unsafety.get(usage), f"{usage} metrics")
        for counts in metrics.values():
            count_object = require_object(counts, "unsafe count")
            count = count_object.get("unsafe_", 0)
            if not isinstance(count, int) or isinstance(count, bool) or count < 0:
                raise fail("unsafe count must be a non-negative integer")
            total += count
    return total


def canonical_file(path: pathlib.Path, description: str) -> pathlib.Path:
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise fail(f"{description} cannot be canonicalized: {error}") from error
    if not resolved.is_file():
        raise fail(f"{description} is not a file")
    return resolved


def load_inventory(path: pathlib.Path) -> dict[str, WorkspacePackage]:
    document = require_object(
        json.loads(path.read_text(encoding="utf-8")), "workspace inventory"
    )
    if document.get("schema_version") != "1.0":
        raise fail("workspace inventory has an unsupported schema version")
    members = document.get("workspace_members")
    if not isinstance(members, list) or not members:
        raise fail("workspace inventory must contain workspace_members")

    packages: dict[str, WorkspacePackage] = {}
    identities: set[tuple[str, str, pathlib.Path]] = set()
    for raw_member in members:
        member = require_object(raw_member, "workspace inventory member")
        if set(member) != {"cargo_id", "name", "version", "manifest"}:
            raise fail("workspace inventory member has missing or unknown fields")
        package = WorkspacePackage(
            cargo_id=require_string(member["cargo_id"], "Cargo package ID"),
            name=require_string(member["name"], "Cargo package name"),
            version=require_string(member["version"], "Cargo package version"),
            manifest=canonical_file(
                pathlib.Path(require_string(member["manifest"], "Cargo manifest path")),
                "Cargo manifest",
            ),
        )
        identity = (package.name, package.version, package.manifest)
        if package.cargo_id in packages or identity in identities:
            raise fail(f"duplicate workspace package identity {package.cargo_id}")
        packages[package.cargo_id] = package
        identities.add(identity)
    return packages


def load_approved_counts(
    policy_path: pathlib.Path,
    inventory: dict[str, WorkspacePackage],
) -> dict[str, int]:
    policy = require_object(
        tomllib.loads(policy_path.read_text(encoding="utf-8")),
        "unsafe inventory policy",
    )
    if policy.get("schema_version") != "1.0":
        raise fail("unsafe inventory policy has an unsupported version")
    boundaries = policy.get("boundaries")
    if not isinstance(boundaries, list):
        raise fail("unsafe inventory policy boundaries must be an array")

    policy_root = canonical_file(policy_path, "unsafe inventory policy").parent.parent
    by_identity = {
        (package.name, package.version, package.manifest): cargo_id
        for cargo_id, package in inventory.items()
    }
    approved: dict[str, int] = {}
    for boundary_value in boundaries:
        boundary = require_object(boundary_value, "unsafe boundary")
        package_name = require_string(boundary.get("package"), "boundary package")
        package_version = require_string(
            boundary.get("package_version"), "boundary package version"
        )
        manifest_value = require_string(
            boundary.get("manifest"), "boundary package manifest"
        )
        manifest_relative = pathlib.PurePosixPath(manifest_value)
        if (
            manifest_relative.is_absolute()
            or ".." in manifest_relative.parts
            or "\\" in manifest_value
        ):
            raise fail(f"boundary manifest is not a safe relative path: {manifest_value}")
        manifest = canonical_file(
            policy_root.joinpath(*manifest_relative.parts), "boundary package manifest"
        )
        cargo_id = by_identity.get((package_name, package_version, manifest))
        if cargo_id is None:
            raise fail(
                "unsafe boundary does not identify an exact workspace package: "
                f"{package_name}@{package_version} ({manifest_value})"
            )

        status = boundary.get("status")
        count = boundary.get("expected_geiger_count")
        if (
            status not in {"proposed", "accepted"}
            or not isinstance(count, int)
            or isinstance(count, bool)
            or count < 0
            or (status == "proposed" and count != 0)
            or (status == "accepted" and count == 0)
        ):
            raise fail("unsafe inventory policy contains an invalid boundary")
        if status == "accepted":
            approved[cargo_id] = approved.get(cargo_id, 0) + count
    return approved


def package_root_from_uri(value: Any) -> pathlib.Path:
    uri = require_string(value, "cargo-geiger path source")
    # cargo-geiger appends an encoded `#version` suffix to Path source URLs.
    package_uri = uri.split("%23", 1)[0].split("#", 1)[0]
    parsed = urllib.parse.urlsplit(package_uri)
    if parsed.scheme != "file" or parsed.netloc not in {"", "localhost"}:
        raise fail(f"cargo-geiger path source is not a local file URI: {uri}")
    decoded = urllib.request.url2pathname(urllib.parse.unquote(parsed.path))
    if os.name == "nt" and len(decoded) >= 3 and decoded[0] in "/\\" and decoded[2] == ":":
        decoded = decoded[1:]
    try:
        root = pathlib.Path(decoded).resolve(strict=True)
    except OSError as error:
        raise fail(f"cargo-geiger package root cannot be canonicalized: {error}") from error
    if not root.is_dir():
        raise fail("cargo-geiger package root is not a directory")
    return root


def validate_report(
    report: dict[str, Any],
    required_cargo_id: str,
    inventory: dict[str, WorkspacePackage],
    approved_counts: dict[str, int],
) -> int:
    omitted = report.get("used_but_not_scanned_files", [])
    if not isinstance(omitted, list) or omitted:
        raise fail("unsafe inventory contains used but unscanned compiler inputs")
    packages_without_metrics = report.get("packages_without_metrics", [])
    if not isinstance(packages_without_metrics, list):
        raise fail("packages_without_metrics must be an array")
    for missing_value in packages_without_metrics:
        missing = require_object(missing_value, "package without metrics")
        require_string(missing.get("name"), "package without metrics name")
        require_string(missing.get("version"), "package without metrics version")
        source = require_object(
            missing.get("source"), "package without metrics source"
        )
        # Registry parser gaps cannot substitute for an exact workspace Path
        # package: every Path/Git/unknown source remains fatal below.
        if set(source) != {"Registry"}:
            raise fail("unsafe inventory omitted workspace or non-registry package metrics")
        registry = require_object(source["Registry"], "registry package source")
        require_string(registry.get("name"), "registry name")
        require_string(registry.get("url"), "registry URL")
    package_entries = report.get("packages")
    if not isinstance(package_entries, list):
        raise fail("unsafe inventory packages must be an array")

    inventory_by_identity = {
        (package.name, package.version, package.root): cargo_id
        for cargo_id, package in inventory.items()
    }
    observed_ids: set[str] = set()
    for raw_entry in package_entries:
        entry = require_object(raw_entry, "cargo-geiger package entry")
        package = require_object(entry.get("package"), "cargo-geiger package")
        identifier = require_object(package.get("id"), "cargo-geiger package ID")
        source = require_object(identifier.get("source"), "cargo-geiger package source")
        if "Path" not in source:
            continue
        if set(source) != {"Path"}:
            raise fail("cargo-geiger path source has unknown fields")
        name = require_string(identifier.get("name"), "cargo-geiger package name")
        version = require_string(identifier.get("version"), "cargo-geiger package version")
        root = package_root_from_uri(source["Path"])
        cargo_id = inventory_by_identity.get((name, version, root))
        if cargo_id is None:
            raise fail(
                "cargo-geiger reported a path package outside the exact workspace "
                f"inventory: {name}@{version} ({root})"
            )
        if cargo_id in observed_ids:
            raise fail(f"cargo-geiger duplicated workspace package {cargo_id}")
        observed_ids.add(cargo_id)

        raw_unsafety = entry.get("unsafety")
        if raw_unsafety is None:
            observed_count: int | None = None
            forbids_unsafe = entry.get("forbids_unsafe")
        else:
            unsafety = require_object(raw_unsafety, "cargo-geiger unsafety metrics")
            observed_count = unsafe_count(unsafety)
            forbids_unsafe = unsafety.get(
                "forbids_unsafe", entry.get("forbids_unsafe")
            )

        approved_count = approved_counts.get(cargo_id)
        if approved_count is not None:
            if observed_count is None:
                raise fail(
                    f"accepted workspace package {cargo_id} lacks full unsafe metrics"
                )
            if observed_count != approved_count:
                raise fail(
                    f"workspace package {cargo_id} expected {approved_count} unsafe "
                    f"uses, observed {observed_count}"
                )
        elif forbids_unsafe is not True or (
            observed_count is not None and observed_count != 0
        ):
            raise fail(f"workspace package {cargo_id} permits or uses unsafe code")

    if required_cargo_id not in inventory:
        raise fail(
            f"required Cargo package ID is absent from workspace inventory: "
            f"{required_cargo_id}"
        )
    if required_cargo_id not in observed_ids:
        raise fail(
            f"unsafe inventory omitted required workspace package {required_cargo_id}; "
            f"observed {sorted(observed_ids)}"
        )
    return len(package_entries)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--required-workspace-package-id", required=True)
    parser.add_argument("--workspace-inventory", required=True)
    parser.add_argument("--unsafe-policy", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        inventory = load_inventory(pathlib.Path(args.workspace_inventory))
        approved = load_approved_counts(pathlib.Path(args.unsafe_policy), inventory)
        report = require_object(json.load(sys.stdin), "cargo-geiger report")
        package_count = validate_report(
            report,
            args.required_workspace_package_id,
            inventory,
            approved,
        )
    except (OSError, ValueError, json.JSONDecodeError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1

    print(
        f"unsafe inventory verified {args.required_workspace_package_id} across "
        f"{package_count} resolved packages"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
