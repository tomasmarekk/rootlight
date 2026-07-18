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


SUPPORTED_CARGO_GEIGER_VERSION = "cargo-geiger 0.13.0"
ACCEPTED_UNSAFE_EVIDENCE_UNIMPLEMENTED = (
    "Accepted unsafe boundary evidence requires compiler-derived expanded input "
    "inventory and the full cargo-geiger SafetyReport; this evidence is not implemented"
)
QUICK_REPORT_KEYS = {"packages", "packages_without_metrics"}
QUICK_ENTRY_KEYS = {"package", "forbids_unsafe"}
PACKAGE_INFO_KEYS = {
    "id",
    "dependencies",
    "dev_dependencies",
    "build_dependencies",
}
PACKAGE_ID_KEYS = {"name", "version", "source"}


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


def require_array(value: Any, description: str) -> list[Any]:
    if not isinstance(value, list):
        raise fail(f"{description} must be an array")
    return value


def require_bool(value: Any, description: str) -> bool:
    if not isinstance(value, bool):
        raise fail(f"{description} must be a boolean")
    return value


def require_exact_keys(
    value: dict[str, Any], expected: set[str], description: str
) -> None:
    observed = set(value)
    if observed != expected:
        raise fail(
            f"{description} has missing or unknown fields; "
            f"expected {sorted(expected)}, observed {sorted(observed)}"
        )


def validate_tool_version(value: Any) -> None:
    version = require_string(value, "cargo-geiger version")
    if version != SUPPORTED_CARGO_GEIGER_VERSION:
        raise fail(
            f"unsupported cargo-geiger version {version!r}; "
            f"expected {SUPPORTED_CARGO_GEIGER_VERSION!r}"
        )


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
    for boundary_value in boundaries:
        boundary = require_object(boundary_value, "unsafe boundary")
        if boundary.get("status") == "accepted":
            raise fail(ACCEPTED_UNSAFE_EVIDENCE_UNIMPLEMENTED)

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
            status != "proposed"
            or not isinstance(count, int)
            or isinstance(count, bool)
            or count < 0
            or count != 0
        ):
            raise fail("unsafe inventory policy contains an invalid boundary")
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


def validate_package_source(value: Any) -> tuple[str, Any]:
    source = require_object(value, "cargo-geiger package source")
    if len(source) != 1:
        raise fail("cargo-geiger package source must contain one known variant")
    variant, details = next(iter(source.items()))
    if variant == "Path":
        require_string(details, "cargo-geiger Path source")
    elif variant == "Registry":
        registry = require_object(details, "cargo-geiger Registry source")
        require_exact_keys(registry, {"name", "url"}, "cargo-geiger Registry source")
        require_string(registry["name"], "cargo-geiger registry name")
        require_string(registry["url"], "cargo-geiger registry URL")
    elif variant == "Git":
        git = require_object(details, "cargo-geiger Git source")
        require_exact_keys(git, {"url", "rev"}, "cargo-geiger Git source")
        require_string(git["url"], "cargo-geiger Git URL")
        require_string(git["rev"], "cargo-geiger Git revision")
    else:
        raise fail(f"cargo-geiger package source uses unknown variant {variant!r}")
    return variant, details


def validate_package_id(value: Any, description: str) -> dict[str, Any]:
    identifier = require_object(value, description)
    require_exact_keys(identifier, PACKAGE_ID_KEYS, description)
    require_string(identifier["name"], f"{description} name")
    require_string(identifier["version"], f"{description} version")
    validate_package_source(identifier["source"])
    return identifier


def validate_package_info(value: Any) -> dict[str, Any]:
    package = require_object(value, "cargo-geiger package")
    require_exact_keys(package, PACKAGE_INFO_KEYS, "cargo-geiger package")
    validate_package_id(package["id"], "cargo-geiger package ID")
    for key in ("dependencies", "dev_dependencies", "build_dependencies"):
        dependencies = require_array(package[key], f"cargo-geiger {key}")
        for dependency in dependencies:
            validate_package_id(dependency, f"cargo-geiger {key} ID")
    return package


def validate_report(
    report: dict[str, Any],
    required_cargo_id: str,
    inventory: dict[str, WorkspacePackage],
    approved_counts: dict[str, int],
    cargo_geiger_version: Any,
) -> int:
    validate_tool_version(cargo_geiger_version)
    if approved_counts:
        raise fail(ACCEPTED_UNSAFE_EVIDENCE_UNIMPLEMENTED)
    require_exact_keys(report, QUICK_REPORT_KEYS, "cargo-geiger QuickSafetyReport")
    packages_without_metrics = require_array(
        report["packages_without_metrics"], "packages_without_metrics"
    )
    for missing_value in packages_without_metrics:
        missing = validate_package_id(missing_value, "package without metrics")
        source = missing["source"]
        # Registry parser gaps cannot substitute for an exact workspace Path
        # package: every Path/Git/unknown source remains fatal below.
        if set(source) != {"Registry"}:
            raise fail("unsafe inventory omitted workspace or non-registry package metrics")
    package_entries = require_array(report["packages"], "unsafe inventory packages")

    inventory_by_identity = {
        (package.name, package.version, package.root): cargo_id
        for cargo_id, package in inventory.items()
    }
    observed_ids: set[str] = set()
    for raw_entry in package_entries:
        entry = require_object(raw_entry, "cargo-geiger package entry")
        require_exact_keys(entry, QUICK_ENTRY_KEYS, "cargo-geiger package entry")
        forbids_unsafe = require_bool(
            entry["forbids_unsafe"], "cargo-geiger forbids_unsafe"
        )
        package = validate_package_info(entry["package"])
        identifier = package["id"]
        source = identifier["source"]
        if "Path" not in source:
            continue
        name = identifier["name"]
        version = identifier["version"]
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

        if not forbids_unsafe:
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
    parser.add_argument("--cargo-geiger-version", required=True)
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
            args.cargo_geiger_version,
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
