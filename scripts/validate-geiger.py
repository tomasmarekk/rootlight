#!/usr/bin/env python3
"""Create and validate fail-closed cargo-geiger evidence.

The evidence contract binds each QuickSafetyReport to the installed scanner,
the Rust toolchain, and every repository input that the current gate can name.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import tomllib
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any, Sequence


SCHEMA_VERSION = "1.0"
UNSAFE_POLICY_SCHEMA_VERSION = "2.0"
SUPPORTED_CARGO_GEIGER_VERSION = "cargo-geiger 0.13.0"
SUPPORTED_CARGO_GEIGER_POLICY_VERSION = "0.13.0"
ENABLED_UNSAFE_EVIDENCE_UNIMPLEMENTED = (
    "Enabled unsafe boundary evidence requires compiler-derived expanded input "
    "inventory and the full cargo-geiger SafetyReport; this evidence is not implemented"
)
SOURCE_INPUT_MODE = "workspace-rust-source-placeholder-v1"
REPORT_FORMAT = "cargo-geiger QuickSafetyReport"
CARGO_GEIGER_REPORT_ARGUMENTS = (
    "--all-features",
    "--all-targets",
    "--all-dependencies",
    "--forbid-only",
    "--locked",
    "--offline",
    "--output-format",
    "Json",
)

INVENTORY_ROOT_KEYS = {"schema_version", "workspace_members"}
INVENTORY_MEMBER_KEYS = {"cargo_id", "name", "version", "manifest"}
UNSAFE_POLICY_ROOT_KEYS = {"schema_version", "boundaries"}
UNSAFE_BOUNDARY_KEYS = {
    "package",
    "package_version",
    "manifest",
    "module",
    "source",
    "status",
    "owner",
    "reason",
    "expected_source_tokens",
    "expected_geiger_count",
}
TOOLCHAIN_POLICY_ROOT_KEYS = {"schema_version", "inputs", "tools"}
CARGO_GEIGER_POLICY_KEYS = {
    "name",
    "version",
    "url",
    "sha256",
    "lockfile",
    "lockfile_sha256",
    "install",
}
CARGO_GEIGER_INSTALL_KEYS = {
    "schema_version",
    "tool",
    "version",
    "executable_sha256",
    "source_url",
    "source_sha256",
    "lockfile",
    "lockfile_sha256",
}
CARGO_GEIGER_EXECUTION_KEYS = {
    "schema_version",
    "executable_sha256",
    "install_identity_sha256",
    "device",
    "inode",
    "size",
    "mtime_ns",
    "ctime_ns",
}
EVIDENCE_ROOT_KEYS = {
    "schema_version",
    "workspace_inventory_sha256",
    "cargo_lock_sha256",
    "cargo_config_sha256",
    "unsafe_policy_sha256",
    "toolchain_policy_sha256",
    "rust_toolchain_file_sha256",
    "workspace_manifests",
    "source_inputs",
    "cargo_geiger",
    "scanner_execution",
    "rust_toolchain",
    "report",
}
EVIDENCE_MANIFEST_KEYS = {"path", "sha256"}
EVIDENCE_SOURCE_KEYS = {
    "mode",
    "compiler_expanded",
    "authoritative_for_enabled_boundary",
    "file_count",
    "sha256",
}
EVIDENCE_RUST_TOOLCHAIN_KEYS = {"cargo_verbose_version", "rustc_verbose_version"}
EVIDENCE_REPORT_KEYS = {
    "format",
    "authoritative_for_enabled_boundary",
    "required_workspace_package_id",
    "sha256",
}
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


def require_nonnegative_integer(value: Any, description: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise fail(f"{description} must be a non-negative integer")
    return value


def require_sha256(value: Any, description: str) -> str:
    digest = require_string(value, description)
    if len(digest) != 64 or any(
        character not in "0123456789abcdef" for character in digest
    ):
        raise fail(f"{description} must be a lowercase SHA-256 digest")
    return digest


def require_exact_keys(
    value: dict[str, Any], expected: set[str], description: str
) -> None:
    observed = set(value)
    if observed != expected:
        raise fail(
            f"{description} has missing or unknown fields; "
            f"expected {sorted(expected)}, observed {sorted(observed)}"
        )


def require_safe_relative_path(value: Any, description: str) -> pathlib.PurePosixPath:
    raw_path = require_string(value, description)
    relative = pathlib.PurePosixPath(raw_path)
    if relative.is_absolute() or ".." in relative.parts or "\\" in raw_path:
        raise fail(f"{description} is not a safe relative path: {raw_path}")
    return relative


def validate_tool_version(value: Any) -> None:
    version = require_string(value, "cargo-geiger version")
    if version != SUPPORTED_CARGO_GEIGER_VERSION:
        raise fail(
            f"unsupported cargo-geiger version {version!r}; "
            f"expected {SUPPORTED_CARGO_GEIGER_VERSION!r}"
        )


def is_reparse_point(metadata: os.stat_result) -> bool:
    attribute = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    attributes = getattr(metadata, "st_file_attributes", 0)
    return bool(attribute and attributes & attribute)


def canonical_file(path: pathlib.Path, description: str) -> pathlib.Path:
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise fail(f"{description} cannot be canonicalized: {error}") from error
    if not resolved.is_file():
        raise fail(f"{description} is not a file")
    return resolved


def canonical_non_alias_file(
    path: pathlib.Path,
    description: str,
    *,
    require_absolute: bool = False,
) -> pathlib.Path:
    if require_absolute and not path.is_absolute():
        raise fail(f"{description} must be an absolute path")
    try:
        metadata = path.lstat()
    except OSError as error:
        raise fail(f"{description} cannot be inspected: {error}") from error
    if stat.S_ISLNK(metadata.st_mode) or is_reparse_point(metadata):
        raise fail(f"{description} must not be a symlink or reparse point")
    if not stat.S_ISREG(metadata.st_mode):
        raise fail(f"{description} is not a regular file")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise fail(f"{description} cannot be canonicalized: {error}") from error
    if require_absolute:
        lexical = pathlib.Path(os.path.abspath(path))
        if os.path.normcase(str(lexical)) != os.path.normcase(str(resolved)):
            raise fail(f"{description} must already be a canonical absolute path")
    return resolved


def require_bound_file(
    supplied: pathlib.Path,
    expected: pathlib.Path,
    description: str,
) -> pathlib.Path:
    actual = canonical_non_alias_file(supplied, description)
    expected_canonical = canonical_non_alias_file(expected, description)
    if os.path.normcase(str(actual)) != os.path.normcase(str(expected_canonical)):
        raise fail(f"{description} does not identify the repository contract path")
    return actual


def sha256_file(path: pathlib.Path, description: str) -> str:
    canonical = canonical_non_alias_file(path, description)
    digest = hashlib.sha256()
    try:
        with canonical.open("rb") as file:
            for chunk in iter(lambda: file.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as error:
        raise fail(f"{description} cannot be hashed: {error}") from error
    return digest.hexdigest()


def workspace_root_from_unsafe_policy(policy_path: pathlib.Path) -> pathlib.Path:
    policy = canonical_non_alias_file(policy_path, "unsafe inventory policy")
    root = policy.parent.parent
    require_bound_file(
        policy, root / "policy" / "unsafe.toml", "unsafe inventory policy"
    )
    return root


def load_inventory(path: pathlib.Path) -> dict[str, WorkspacePackage]:
    document = require_object(
        json.loads(path.read_text(encoding="utf-8")), "workspace inventory"
    )
    require_exact_keys(document, INVENTORY_ROOT_KEYS, "workspace inventory")
    if document["schema_version"] != SCHEMA_VERSION:
        raise fail("workspace inventory has an unsupported schema version")
    members = require_array(document["workspace_members"], "workspace_members")
    if not members:
        raise fail("workspace inventory must contain workspace_members")

    packages: dict[str, WorkspacePackage] = {}
    identities: set[tuple[str, str, pathlib.Path]] = set()
    for raw_member in members:
        member = require_object(raw_member, "workspace inventory member")
        require_exact_keys(member, INVENTORY_MEMBER_KEYS, "workspace inventory member")
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
    require_exact_keys(policy, UNSAFE_POLICY_ROOT_KEYS, "unsafe inventory policy")
    if policy["schema_version"] != UNSAFE_POLICY_SCHEMA_VERSION:
        raise fail("unsafe inventory policy has an unsupported version")
    boundaries = require_array(
        policy["boundaries"], "unsafe inventory policy boundaries"
    )
    normalized_boundaries: list[dict[str, Any]] = []
    for boundary_value in boundaries:
        boundary = require_object(boundary_value, "unsafe boundary")
        require_exact_keys(boundary, UNSAFE_BOUNDARY_KEYS, "unsafe boundary")
        normalized_boundaries.append(boundary)
        if boundary["status"] == "enabled":
            raise fail(ENABLED_UNSAFE_EVIDENCE_UNIMPLEMENTED)

    policy_root = workspace_root_from_unsafe_policy(policy_path)
    by_identity = {
        (package.name, package.version, package.manifest): cargo_id
        for cargo_id, package in inventory.items()
    }
    for boundary in normalized_boundaries:
        package_name = require_string(boundary["package"], "boundary package")
        package_version = require_string(
            boundary["package_version"], "boundary package version"
        )
        manifest_relative = require_safe_relative_path(
            boundary["manifest"], "boundary package manifest"
        )
        manifest = canonical_file(
            policy_root.joinpath(*manifest_relative.parts), "boundary package manifest"
        )
        cargo_id = by_identity.get((package_name, package_version, manifest))
        if cargo_id is None:
            raise fail(
                "unsafe boundary does not identify an exact workspace package: "
                f"{package_name}@{package_version} ({manifest_relative})"
            )

        for key, description in (
            ("module", "boundary module"),
            ("owner", "boundary owner"),
            ("reason", "boundary reason"),
        ):
            require_string(boundary[key], description)
        require_safe_relative_path(boundary["source"], "boundary source")
        if boundary["status"] != "disabled":
            raise fail("unsafe inventory policy contains an invalid boundary status")
        if (
            require_nonnegative_integer(
                boundary["expected_source_tokens"], "expected source token count"
            )
            != 0
            or require_nonnegative_integer(
                boundary["expected_geiger_count"], "expected cargo-geiger count"
            )
            != 0
        ):
            raise fail("disabled unsafe boundaries must retain zero evidence counts")

    # QuickSafetyReport is deliberately non-authoritative for enabled boundaries.
    return {}


def package_root_from_uri(value: Any) -> pathlib.Path:
    uri = require_string(value, "cargo-geiger path source")
    # cargo-geiger appends an encoded `#version` suffix to Path source URLs.
    package_uri = uri.split("%23", 1)[0].split("#", 1)[0]
    parsed = urllib.parse.urlsplit(package_uri)
    if parsed.scheme != "file" or parsed.netloc not in {"", "localhost"}:
        raise fail(f"cargo-geiger path source is not a local file URI: {uri}")
    decoded = urllib.request.url2pathname(urllib.parse.unquote(parsed.path))
    if (
        os.name == "nt"
        and len(decoded) >= 3
        and decoded[0] in "/\\"
        and decoded[2] == ":"
    ):
        decoded = decoded[1:]
    try:
        root = pathlib.Path(decoded).resolve(strict=True)
    except OSError as error:
        raise fail(
            f"cargo-geiger package root cannot be canonicalized: {error}"
        ) from error
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
        raise fail(ENABLED_UNSAFE_EVIDENCE_UNIMPLEMENTED)
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
            raise fail(
                "unsafe inventory omitted workspace or non-registry package metrics"
            )
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


def validate_environment_aliases() -> None:
    for name in os.environ:
        if name.casefold() == "cargo_alias_geiger":
            raise fail("CARGO_ALIAS_GEIGER is forbidden for cargo-geiger evidence")


def validate_repository_cargo_config(
    cargo_config_path: pathlib.Path,
    workspace_root: pathlib.Path,
) -> pathlib.Path:
    config = require_bound_file(
        cargo_config_path,
        workspace_root / ".cargo" / "config.toml",
        "repository Cargo config",
    )
    legacy_config = workspace_root / ".cargo" / "config"
    if os.path.lexists(legacy_config):
        raise fail(
            "legacy repository .cargo/config is forbidden by the evidence contract"
        )
    document = require_object(
        tomllib.loads(config.read_text(encoding="utf-8")), "repository Cargo config"
    )
    aliases = document.get("alias")
    if aliases is not None:
        alias_table = require_object(aliases, "repository Cargo aliases")
        if any(name.casefold() == "geiger" for name in alias_table):
            raise fail("repository Cargo alias 'geiger' is forbidden")
    return config


def capture_command(
    arguments: Sequence[str],
    description: str,
    workspace_root: pathlib.Path,
) -> str:
    try:
        completed = subprocess.run(
            list(arguments),
            cwd=workspace_root,
            check=False,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
    except OSError as error:
        raise fail(f"{description} could not execute: {error}") from error
    if completed.returncode != 0:
        diagnostic = completed.stderr.strip() or completed.stdout.strip()
        raise fail(f"{description} failed: {diagnostic}")
    return require_string(completed.stdout.strip(), description)


def load_cargo_geiger_policy(
    policy_path: pathlib.Path,
    workspace_root: pathlib.Path,
) -> dict[str, Any]:
    policy_file = require_bound_file(
        policy_path,
        workspace_root / "policy" / "toolchain.toml",
        "toolchain policy",
    )
    document = require_object(
        tomllib.loads(policy_file.read_text(encoding="utf-8")), "toolchain policy"
    )
    require_exact_keys(document, TOOLCHAIN_POLICY_ROOT_KEYS, "toolchain policy")
    if document["schema_version"] != SCHEMA_VERSION:
        raise fail("toolchain policy has an unsupported schema version")
    require_array(document["inputs"], "toolchain policy inputs")
    tools = require_array(document["tools"], "toolchain policy tools")
    matches: list[dict[str, Any]] = []
    for value in tools:
        tool = require_object(value, "toolchain policy tool")
        if tool.get("name") == "cargo-geiger":
            require_exact_keys(
                tool, CARGO_GEIGER_POLICY_KEYS, "cargo-geiger tool policy"
            )
            matches.append(tool)
    if len(matches) != 1:
        raise fail("toolchain policy must contain exactly one cargo-geiger tool")
    tool = matches[0]
    if require_string(tool["version"], "cargo-geiger policy version") != (
        SUPPORTED_CARGO_GEIGER_POLICY_VERSION
    ):
        raise fail("toolchain policy cargo-geiger version is unsupported")
    source_url = require_string(tool["url"], "cargo-geiger source URL")
    source_sha256 = require_sha256(tool["sha256"], "cargo-geiger source SHA-256")
    lockfile_relative = require_safe_relative_path(
        tool["lockfile"], "cargo-geiger lockfile"
    )
    lockfile_sha256 = require_sha256(
        tool["lockfile_sha256"], "cargo-geiger lockfile SHA-256"
    )
    require_string(tool["install"], "cargo-geiger install contract")
    lockfile = canonical_non_alias_file(
        workspace_root.joinpath(*lockfile_relative.parts), "cargo-geiger lockfile"
    )
    if sha256_file(lockfile, "cargo-geiger lockfile") != lockfile_sha256:
        raise fail("cargo-geiger lockfile digest does not match toolchain policy")
    return {
        "source_url": source_url,
        "source_sha256": source_sha256,
        "lockfile": str(lockfile_relative),
        "lockfile_sha256": lockfile_sha256,
    }


def trusted_cargo_geiger_binary(path: pathlib.Path) -> pathlib.Path:
    binary = canonical_non_alias_file(
        path, "trusted cargo-geiger executable", require_absolute=True
    )
    expected_names = {"cargo-geiger", "cargo-geiger.exe"}
    if binary.name.casefold() not in expected_names:
        raise fail("trusted cargo-geiger executable has an unexpected file name")
    return binary


def executable_file_identity(binary_path: pathlib.Path) -> dict[str, Any]:
    binary = trusted_cargo_geiger_binary(binary_path)
    digest = hashlib.sha256()
    try:
        with binary.open("rb") as file:
            before = os.fstat(file.fileno())
            for chunk in iter(lambda: file.read(1024 * 1024), b""):
                digest.update(chunk)
            after = os.fstat(file.fileno())
        path_metadata = binary.lstat()
    except OSError as error:
        raise fail(
            f"trusted cargo-geiger executable cannot be measured: {error}"
        ) from error

    measured_fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    path_fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns")
    if any(
        getattr(before, field) != getattr(after, field) for field in measured_fields
    ) or any(
        getattr(after, field) != getattr(path_metadata, field) for field in path_fields
    ):
        raise fail("trusted cargo-geiger executable changed while it was measured")
    if stat.S_ISLNK(path_metadata.st_mode) or is_reparse_point(path_metadata):
        raise fail("trusted cargo-geiger executable became a filesystem alias")
    return {
        "schema_version": SCHEMA_VERSION,
        "executable_sha256": digest.hexdigest(),
        "device": before.st_dev,
        "inode": before.st_ino,
        "size": before.st_size,
        "mtime_ns": before.st_mtime_ns,
        "ctime_ns": before.st_ctime_ns,
    }


def current_cargo_geiger_execution_identity(
    binary_path: pathlib.Path,
) -> dict[str, Any]:
    binary = trusted_cargo_geiger_binary(binary_path)
    identity = executable_file_identity(binary)
    receipt_path = binary.with_name("cargo-geiger.identity.json")
    identity["install_identity_sha256"] = sha256_file(
        receipt_path, "cargo-geiger install identity"
    )
    return identity


def validate_cargo_geiger_execution_identity(document: dict[str, Any]) -> None:
    require_exact_keys(
        document, CARGO_GEIGER_EXECUTION_KEYS, "cargo-geiger execution identity"
    )
    if document["schema_version"] != SCHEMA_VERSION:
        raise fail("cargo-geiger execution identity has an unsupported schema version")
    require_sha256(
        document["executable_sha256"], "cargo-geiger execution executable SHA-256"
    )
    require_sha256(
        document["install_identity_sha256"],
        "cargo-geiger execution install identity SHA-256",
    )
    for key, description in (
        ("device", "cargo-geiger executable device"),
        ("inode", "cargo-geiger executable inode"),
        ("size", "cargo-geiger executable size"),
        ("mtime_ns", "cargo-geiger executable modification time"),
        ("ctime_ns", "cargo-geiger executable change time"),
    ):
        require_nonnegative_integer(document[key], description)


def load_cargo_geiger_execution_identity(path: pathlib.Path) -> dict[str, Any]:
    identity_file = canonical_non_alias_file(path, "cargo-geiger execution identity")
    document = require_object(
        json.loads(identity_file.read_text(encoding="utf-8")),
        "cargo-geiger execution identity",
    )
    validate_cargo_geiger_execution_identity(document)
    return document


def load_cargo_geiger_install_identity(
    binary_path: pathlib.Path,
    toolchain_policy_path: pathlib.Path,
    workspace_root: pathlib.Path,
) -> dict[str, Any]:
    binary = trusted_cargo_geiger_binary(binary_path)
    policy_identity = load_cargo_geiger_policy(toolchain_policy_path, workspace_root)
    receipt_path = binary.with_name("cargo-geiger.identity.json")
    receipt_file = canonical_non_alias_file(
        receipt_path, "cargo-geiger install identity"
    )
    receipt = require_object(
        json.loads(receipt_file.read_text(encoding="utf-8")),
        "cargo-geiger install identity",
    )
    require_exact_keys(
        receipt, CARGO_GEIGER_INSTALL_KEYS, "cargo-geiger install identity"
    )
    if receipt["schema_version"] != SCHEMA_VERSION:
        raise fail("cargo-geiger install identity has an unsupported schema version")
    if receipt["tool"] != "cargo-geiger":
        raise fail("cargo-geiger install identity names an unexpected tool")
    validate_tool_version(receipt["version"])
    executable_sha256 = require_sha256(
        receipt["executable_sha256"], "cargo-geiger executable SHA-256"
    )
    if sha256_file(binary, "trusted cargo-geiger executable") != executable_sha256:
        raise fail(
            "trusted cargo-geiger executable digest does not match install identity"
        )
    observed_version = capture_command(
        [str(binary), "--version"], "cargo-geiger version", workspace_root
    )
    validate_tool_version(observed_version)
    if observed_version != receipt["version"]:
        raise fail("cargo-geiger version does not match install identity")
    for key in ("source_url", "source_sha256", "lockfile", "lockfile_sha256"):
        if receipt[key] != policy_identity[key]:
            raise fail(
                f"cargo-geiger install identity does not match policy field {key}"
            )
    require_string(receipt["source_url"], "cargo-geiger install source URL")
    require_sha256(receipt["source_sha256"], "cargo-geiger install source SHA-256")
    require_safe_relative_path(receipt["lockfile"], "cargo-geiger install lockfile")
    require_sha256(receipt["lockfile_sha256"], "cargo-geiger install lockfile SHA-256")
    return receipt


def write_json_contract(
    path: pathlib.Path,
    document: dict[str, Any],
    description: str,
) -> None:
    parent = path.parent.resolve(strict=True)
    output = parent / path.name
    if os.path.lexists(output):
        metadata = output.lstat()
        if stat.S_ISLNK(metadata.st_mode) or is_reparse_point(metadata):
            raise fail(f"{description} output must not be an alias")
    output.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def prepare_cargo_geiger_execution_identity(
    trusted_binary: pathlib.Path,
    cargo_config_path: pathlib.Path,
    unsafe_policy_path: pathlib.Path,
    toolchain_policy_path: pathlib.Path,
    execution_identity_path: pathlib.Path,
) -> None:
    workspace_root = workspace_root_from_unsafe_policy(unsafe_policy_path)
    validate_environment_aliases()
    validate_repository_cargo_config(cargo_config_path, workspace_root)
    before = current_cargo_geiger_execution_identity(trusted_binary)
    install_identity = load_cargo_geiger_install_identity(
        trusted_binary, toolchain_policy_path, workspace_root
    )
    after = current_cargo_geiger_execution_identity(trusted_binary)
    if before != after:
        raise fail("trusted cargo-geiger executable changed during preflight")
    if after["executable_sha256"] != install_identity["executable_sha256"]:
        raise fail("cargo-geiger execution identity does not match install identity")
    validate_cargo_geiger_execution_identity(after)
    write_json_contract(
        execution_identity_path,
        after,
        "cargo-geiger execution identity",
    )


def verify_cargo_geiger_execution_identity(
    trusted_binary: pathlib.Path,
    cargo_config_path: pathlib.Path,
    unsafe_policy_path: pathlib.Path,
    toolchain_policy_path: pathlib.Path,
    execution_identity_path: pathlib.Path,
) -> tuple[dict[str, Any], dict[str, Any]]:
    workspace_root = workspace_root_from_unsafe_policy(unsafe_policy_path)
    validate_environment_aliases()
    validate_repository_cargo_config(cargo_config_path, workspace_root)
    expected = load_cargo_geiger_execution_identity(execution_identity_path)
    before = current_cargo_geiger_execution_identity(trusted_binary)
    if before != expected:
        raise fail("trusted cargo-geiger executable differs from preflight identity")
    install_identity = load_cargo_geiger_install_identity(
        trusted_binary, toolchain_policy_path, workspace_root
    )
    after = current_cargo_geiger_execution_identity(trusted_binary)
    if after != expected:
        raise fail("trusted cargo-geiger executable changed during identity check")
    if after["executable_sha256"] != install_identity["executable_sha256"]:
        raise fail("cargo-geiger execution identity does not match install identity")
    return expected, install_identity


def cargo_geiger_report_argv(
    binary_path: pathlib.Path,
    manifest_path: pathlib.Path,
) -> list[str]:
    binary = trusted_cargo_geiger_binary(binary_path)
    manifest = canonical_non_alias_file(
        manifest_path, "cargo-geiger package manifest", require_absolute=True
    )
    return [
        str(binary),
        "--manifest-path",
        str(manifest),
        *CARGO_GEIGER_REPORT_ARGUMENTS,
    ]


def publication_path(path: pathlib.Path, description: str) -> pathlib.Path:
    parent = path.parent.resolve(strict=True)
    output = parent / path.name
    if os.path.lexists(output):
        metadata = output.lstat()
        if stat.S_ISLNK(metadata.st_mode) or is_reparse_point(metadata):
            raise fail(f"{description} output must not be an alias")
        if not stat.S_ISREG(metadata.st_mode):
            raise fail(f"{description} output must be a regular file")
    return output


def scan_with_trusted_cargo_geiger(
    *,
    trusted_binary_path: pathlib.Path,
    manifest_path: pathlib.Path,
    cargo_config_path: pathlib.Path,
    unsafe_policy_path: pathlib.Path,
    toolchain_policy_path: pathlib.Path,
    execution_identity_path: pathlib.Path,
    report_path: pathlib.Path,
    log_path: pathlib.Path,
) -> None:
    workspace_root = workspace_root_from_unsafe_policy(unsafe_policy_path)
    before, _ = verify_cargo_geiger_execution_identity(
        trusted_binary_path,
        cargo_config_path,
        unsafe_policy_path,
        toolchain_policy_path,
        execution_identity_path,
    )
    arguments = cargo_geiger_report_argv(trusted_binary_path, manifest_path)
    report_output = publication_path(report_path, "cargo-geiger report")
    log_output = publication_path(log_path, "cargo-geiger log")
    if os.path.normcase(str(report_output)) == os.path.normcase(str(log_output)):
        raise fail("cargo-geiger report and log outputs must be distinct")

    report_temporary: pathlib.Path | None = None
    log_temporary: pathlib.Path | None = None
    completed: subprocess.CompletedProcess[bytes] | None = None
    execution_error: OSError | None = None
    try:
        with (
            tempfile.NamedTemporaryFile(
                mode="w+b",
                prefix=f".{report_output.name}.",
                suffix=".tmp",
                dir=report_output.parent,
                delete=False,
            ) as report_file,
            tempfile.NamedTemporaryFile(
                mode="w+b",
                prefix=f".{log_output.name}.",
                suffix=".tmp",
                dir=log_output.parent,
                delete=False,
            ) as log_file,
        ):
            report_temporary = pathlib.Path(report_file.name)
            log_temporary = pathlib.Path(log_file.name)
            try:
                completed = subprocess.run(
                    arguments,
                    cwd=workspace_root,
                    check=False,
                    stdout=report_file,
                    stderr=log_file,
                )
            except OSError as error:
                execution_error = error
            report_file.flush()
            log_file.flush()
            os.fsync(report_file.fileno())
            os.fsync(log_file.fileno())

        after, _ = verify_cargo_geiger_execution_identity(
            trusted_binary_path,
            cargo_config_path,
            unsafe_policy_path,
            toolchain_policy_path,
            execution_identity_path,
        )
        if before != after:
            raise fail(
                "trusted cargo-geiger executable changed across report execution"
            )
        if execution_error is not None:
            raise fail(
                f"trusted cargo-geiger executable could not run: {execution_error}"
            )
        if completed is None:
            raise fail("trusted cargo-geiger executable produced no process result")
        if completed.returncode != 0:
            diagnostic = ""
            if log_temporary is not None:
                with log_temporary.open(
                    "r", encoding="utf-8", errors="replace"
                ) as log_file:
                    diagnostic = log_file.read(8192).strip()
            raise fail(
                "trusted cargo-geiger executable failed"
                + (f": {diagnostic}" if diagnostic else "")
            )

        if report_temporary is None or log_temporary is None:
            raise fail("cargo-geiger temporary outputs were not created")
        os.replace(report_temporary, report_output)
        report_temporary = None
        os.replace(log_temporary, log_output)
        log_temporary = None
    finally:
        for temporary in (report_temporary, log_temporary):
            if temporary is not None:
                temporary.unlink(missing_ok=True)


def workspace_manifest_evidence(
    inventory: dict[str, WorkspacePackage],
    workspace_root: pathlib.Path,
) -> list[dict[str, str]]:
    manifests = {
        canonical_non_alias_file(workspace_root / "Cargo.toml", "workspace manifest")
    }
    for package in inventory.values():
        manifests.add(canonical_non_alias_file(package.manifest, "workspace manifest"))

    evidence: list[dict[str, str]] = []
    for manifest in manifests:
        try:
            relative = manifest.relative_to(workspace_root)
        except ValueError as error:
            raise fail(
                f"workspace manifest is outside the workspace: {manifest}"
            ) from error
        evidence.append(
            {
                "path": relative.as_posix(),
                "sha256": sha256_file(manifest, "workspace manifest"),
            }
        )
    return sorted(evidence, key=lambda item: item["path"])


def workspace_source_evidence(
    inventory: dict[str, WorkspacePackage],
    workspace_root: pathlib.Path,
) -> dict[str, Any]:
    source_files: set[pathlib.Path] = set()
    ignored_directories = {".git", "target"}
    for package in inventory.values():
        for directory, child_directories, files in os.walk(
            package.root, followlinks=False
        ):
            directory_path = pathlib.Path(directory)
            retained: list[str] = []
            for name in child_directories:
                if name in ignored_directories:
                    continue
                child = directory_path / name
                metadata = child.lstat()
                if stat.S_ISLNK(metadata.st_mode) or is_reparse_point(metadata):
                    raise fail(f"workspace source directory is an alias: {child}")
                retained.append(name)
            child_directories[:] = retained
            for name in files:
                if pathlib.Path(name).suffix != ".rs":
                    continue
                source = canonical_non_alias_file(
                    directory_path / name, "workspace Rust source"
                )
                try:
                    source.relative_to(workspace_root)
                except ValueError as error:
                    raise fail(
                        f"workspace Rust source is outside the workspace: {source}"
                    ) from error
                source_files.add(source)

    digest = hashlib.sha256()
    for source in sorted(source_files, key=lambda path: path.as_posix()):
        relative = source.relative_to(workspace_root).as_posix()
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(bytes.fromhex(sha256_file(source, "workspace Rust source")))
        digest.update(b"\0")
    return {
        "mode": SOURCE_INPUT_MODE,
        "compiler_expanded": False,
        "authoritative_for_enabled_boundary": False,
        "file_count": len(source_files),
        "sha256": digest.hexdigest(),
    }


def build_evidence_envelope(
    *,
    trusted_binary_path: pathlib.Path,
    required_cargo_id: str,
    workspace_inventory_path: pathlib.Path,
    unsafe_policy_path: pathlib.Path,
    toolchain_policy_path: pathlib.Path,
    cargo_lock_path: pathlib.Path,
    cargo_config_path: pathlib.Path,
    rust_toolchain_path: pathlib.Path,
    execution_identity_path: pathlib.Path,
    report_path: pathlib.Path,
) -> dict[str, Any]:
    workspace_root = workspace_root_from_unsafe_policy(unsafe_policy_path)
    unsafe_policy = require_bound_file(
        unsafe_policy_path,
        workspace_root / "policy" / "unsafe.toml",
        "unsafe inventory policy",
    )
    toolchain_policy = require_bound_file(
        toolchain_policy_path,
        workspace_root / "policy" / "toolchain.toml",
        "toolchain policy",
    )
    cargo_lock = require_bound_file(
        cargo_lock_path, workspace_root / "Cargo.lock", "Cargo lockfile"
    )
    cargo_config = validate_repository_cargo_config(cargo_config_path, workspace_root)
    rust_toolchain = require_bound_file(
        rust_toolchain_path,
        workspace_root / "rust-toolchain.toml",
        "Rust toolchain file",
    )
    inventory_file = canonical_non_alias_file(
        workspace_inventory_path, "workspace inventory"
    )
    report_file = canonical_non_alias_file(report_path, "cargo-geiger report")

    inventory = load_inventory(inventory_file)
    approved = load_approved_counts(unsafe_policy, inventory)
    required_id = require_string(required_cargo_id, "required Cargo package ID")
    if required_id not in inventory:
        raise fail(
            f"required Cargo package ID is absent from workspace inventory: {required_id}"
        )
    execution_identity, install_identity = verify_cargo_geiger_execution_identity(
        trusted_binary_path,
        cargo_config,
        unsafe_policy,
        toolchain_policy,
        execution_identity_path,
    )
    report = require_object(
        json.loads(report_file.read_text(encoding="utf-8")), "cargo-geiger report"
    )
    validate_report(
        report,
        required_id,
        inventory,
        approved,
        install_identity["version"],
    )

    cargo_verbose_version = capture_command(
        ["cargo", "-vV"], "Cargo verbose version", workspace_root
    )
    rustc_verbose_version = capture_command(
        ["rustc", "-vV"], "rustc verbose version", workspace_root
    )
    return {
        "schema_version": SCHEMA_VERSION,
        "workspace_inventory_sha256": sha256_file(
            inventory_file, "workspace inventory"
        ),
        "cargo_lock_sha256": sha256_file(cargo_lock, "Cargo lockfile"),
        "cargo_config_sha256": sha256_file(cargo_config, "repository Cargo config"),
        "unsafe_policy_sha256": sha256_file(unsafe_policy, "unsafe inventory policy"),
        "toolchain_policy_sha256": sha256_file(toolchain_policy, "toolchain policy"),
        "rust_toolchain_file_sha256": sha256_file(
            rust_toolchain, "Rust toolchain file"
        ),
        "workspace_manifests": workspace_manifest_evidence(inventory, workspace_root),
        "source_inputs": workspace_source_evidence(inventory, workspace_root),
        "cargo_geiger": install_identity,
        "scanner_execution": execution_identity,
        "rust_toolchain": {
            "cargo_verbose_version": cargo_verbose_version,
            "rustc_verbose_version": rustc_verbose_version,
        },
        "report": {
            "format": REPORT_FORMAT,
            "authoritative_for_enabled_boundary": False,
            "required_workspace_package_id": required_id,
            "sha256": sha256_file(report_file, "cargo-geiger report"),
        },
    }


def validate_evidence_envelope(document: dict[str, Any]) -> None:
    require_exact_keys(document, EVIDENCE_ROOT_KEYS, "cargo-geiger evidence envelope")
    if document["schema_version"] != SCHEMA_VERSION:
        raise fail("cargo-geiger evidence envelope has an unsupported schema version")
    for key, description in (
        ("workspace_inventory_sha256", "workspace inventory SHA-256"),
        ("cargo_lock_sha256", "Cargo lockfile SHA-256"),
        ("cargo_config_sha256", "Cargo config SHA-256"),
        ("unsafe_policy_sha256", "unsafe policy SHA-256"),
        ("toolchain_policy_sha256", "toolchain policy SHA-256"),
        ("rust_toolchain_file_sha256", "Rust toolchain file SHA-256"),
    ):
        require_sha256(document[key], description)

    manifests = require_array(
        document["workspace_manifests"], "workspace manifest evidence"
    )
    if not manifests:
        raise fail("workspace manifest evidence must not be empty")
    observed_manifest_paths: set[str] = set()
    for value in manifests:
        manifest = require_object(value, "workspace manifest evidence entry")
        require_exact_keys(
            manifest, EVIDENCE_MANIFEST_KEYS, "workspace manifest evidence entry"
        )
        path = str(
            require_safe_relative_path(
                manifest["path"], "workspace manifest evidence path"
            )
        )
        if path in observed_manifest_paths:
            raise fail(f"duplicate workspace manifest evidence path {path}")
        observed_manifest_paths.add(path)
        require_sha256(manifest["sha256"], "workspace manifest SHA-256")

    source_inputs = require_object(
        document["source_inputs"], "workspace source input evidence"
    )
    require_exact_keys(
        source_inputs, EVIDENCE_SOURCE_KEYS, "workspace source input evidence"
    )
    if source_inputs["mode"] != SOURCE_INPUT_MODE:
        raise fail("workspace source input evidence uses an unsupported mode")
    if require_bool(
        source_inputs["compiler_expanded"], "compiler-expanded input state"
    ):
        raise fail("compiler-expanded input evidence is not implemented")
    if require_bool(
        source_inputs["authoritative_for_enabled_boundary"],
        "enabled source evidence authority",
    ):
        raise fail(ENABLED_UNSAFE_EVIDENCE_UNIMPLEMENTED)
    require_nonnegative_integer(source_inputs["file_count"], "source input file count")
    require_sha256(source_inputs["sha256"], "source input SHA-256")

    cargo_geiger = require_object(
        document["cargo_geiger"], "cargo-geiger evidence identity"
    )
    require_exact_keys(
        cargo_geiger, CARGO_GEIGER_INSTALL_KEYS, "cargo-geiger evidence identity"
    )
    if cargo_geiger["schema_version"] != SCHEMA_VERSION:
        raise fail("cargo-geiger evidence identity has an unsupported schema version")
    if cargo_geiger["tool"] != "cargo-geiger":
        raise fail("cargo-geiger evidence identity names an unexpected tool")
    validate_tool_version(cargo_geiger["version"])
    require_sha256(cargo_geiger["executable_sha256"], "cargo-geiger executable SHA-256")
    require_string(cargo_geiger["source_url"], "cargo-geiger source URL")
    require_sha256(cargo_geiger["source_sha256"], "cargo-geiger source SHA-256")
    require_safe_relative_path(cargo_geiger["lockfile"], "cargo-geiger lockfile")
    require_sha256(cargo_geiger["lockfile_sha256"], "cargo-geiger lockfile SHA-256")

    scanner_execution = require_object(
        document["scanner_execution"], "cargo-geiger scanner execution identity"
    )
    validate_cargo_geiger_execution_identity(scanner_execution)
    if scanner_execution["executable_sha256"] != cargo_geiger["executable_sha256"]:
        raise fail("scanner execution digest does not match cargo-geiger identity")

    rust_identity = require_object(
        document["rust_toolchain"], "Rust toolchain evidence identity"
    )
    require_exact_keys(
        rust_identity,
        EVIDENCE_RUST_TOOLCHAIN_KEYS,
        "Rust toolchain evidence identity",
    )
    require_string(
        rust_identity["cargo_verbose_version"], "Cargo verbose version identity"
    )
    require_string(
        rust_identity["rustc_verbose_version"], "rustc verbose version identity"
    )

    report = require_object(document["report"], "cargo-geiger report evidence")
    require_exact_keys(report, EVIDENCE_REPORT_KEYS, "cargo-geiger report evidence")
    if report["format"] != REPORT_FORMAT:
        raise fail("cargo-geiger report evidence uses an unsupported format")
    if require_bool(
        report["authoritative_for_enabled_boundary"], "enabled report evidence authority"
    ):
        raise fail(ENABLED_UNSAFE_EVIDENCE_UNIMPLEMENTED)
    require_string(report["required_workspace_package_id"], "required Cargo package ID")
    require_sha256(report["sha256"], "cargo-geiger report SHA-256")


def first_evidence_difference(
    observed: Any,
    expected: Any,
    path: str = "evidence",
) -> str | None:
    if type(observed) is not type(expected):
        return path
    if isinstance(observed, dict):
        if set(observed) != set(expected):
            return path
        for key in sorted(observed):
            difference = first_evidence_difference(
                observed[key], expected[key], f"{path}.{key}"
            )
            if difference is not None:
                return difference
        return None
    if isinstance(observed, list):
        if len(observed) != len(expected):
            return path
        for index, (observed_item, expected_item) in enumerate(
            zip(observed, expected, strict=True)
        ):
            difference = first_evidence_difference(
                observed_item, expected_item, f"{path}[{index}]"
            )
            if difference is not None:
                return difference
        return None
    return None if observed == expected else path


def verify_evidence_envelope(
    observed: dict[str, Any],
    expected: dict[str, Any],
) -> None:
    validate_evidence_envelope(observed)
    validate_evidence_envelope(expected)
    difference = first_evidence_difference(observed, expected)
    if difference is not None:
        raise fail(f"cargo-geiger evidence envelope mismatch at {difference}")


def load_evidence_envelope(path: pathlib.Path) -> dict[str, Any]:
    evidence_file = canonical_non_alias_file(path, "cargo-geiger evidence envelope")
    document = require_object(
        json.loads(evidence_file.read_text(encoding="utf-8")),
        "cargo-geiger evidence envelope",
    )
    validate_evidence_envelope(document)
    return document


def write_evidence_envelope(path: pathlib.Path, document: dict[str, Any]) -> None:
    validate_evidence_envelope(document)
    parent = path.parent.resolve(strict=True)
    output = parent / path.name
    if os.path.lexists(output):
        metadata = output.lstat()
        if stat.S_ISLNK(metadata.st_mode) or is_reparse_point(metadata):
            raise fail("cargo-geiger evidence output must not be an alias")
    output.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def add_preflight_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--trusted-cargo-geiger", required=True)
    parser.add_argument("--cargo-config", required=True)
    parser.add_argument("--unsafe-policy", required=True)
    parser.add_argument("--toolchain-policy", required=True)
    parser.add_argument("--execution-identity", required=True)


def add_evidence_arguments(parser: argparse.ArgumentParser) -> None:
    add_preflight_arguments(parser)
    parser.add_argument("--required-workspace-package-id", required=True)
    parser.add_argument("--workspace-inventory", required=True)
    parser.add_argument("--cargo-lock", required=True)
    parser.add_argument("--rust-toolchain", required=True)
    parser.add_argument("--report", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    preflight = subparsers.add_parser("preflight")
    add_preflight_arguments(preflight)

    scan = subparsers.add_parser("scan")
    add_preflight_arguments(scan)
    scan.add_argument("--manifest", required=True)
    scan.add_argument("--report", required=True)
    scan.add_argument("--log", required=True)

    prepare = subparsers.add_parser("prepare")
    add_evidence_arguments(prepare)
    prepare.add_argument("--evidence-envelope", required=True)

    validate = subparsers.add_parser("validate")
    add_evidence_arguments(validate)
    validate.add_argument("--evidence-envelope", required=True)
    return parser.parse_args()


def evidence_from_args(args: argparse.Namespace) -> dict[str, Any]:
    return build_evidence_envelope(
        trusted_binary_path=pathlib.Path(args.trusted_cargo_geiger),
        required_cargo_id=args.required_workspace_package_id,
        workspace_inventory_path=pathlib.Path(args.workspace_inventory),
        unsafe_policy_path=pathlib.Path(args.unsafe_policy),
        toolchain_policy_path=pathlib.Path(args.toolchain_policy),
        cargo_lock_path=pathlib.Path(args.cargo_lock),
        cargo_config_path=pathlib.Path(args.cargo_config),
        rust_toolchain_path=pathlib.Path(args.rust_toolchain),
        execution_identity_path=pathlib.Path(args.execution_identity),
        report_path=pathlib.Path(args.report),
    )


def main() -> int:
    args = parse_args()
    try:
        if args.command == "preflight":
            prepare_cargo_geiger_execution_identity(
                pathlib.Path(args.trusted_cargo_geiger),
                pathlib.Path(args.cargo_config),
                pathlib.Path(args.unsafe_policy),
                pathlib.Path(args.toolchain_policy),
                pathlib.Path(args.execution_identity),
            )
            print("trusted cargo-geiger install identity verified")
            return 0

        if args.command == "scan":
            scan_with_trusted_cargo_geiger(
                trusted_binary_path=pathlib.Path(args.trusted_cargo_geiger),
                manifest_path=pathlib.Path(args.manifest),
                cargo_config_path=pathlib.Path(args.cargo_config),
                unsafe_policy_path=pathlib.Path(args.unsafe_policy),
                toolchain_policy_path=pathlib.Path(args.toolchain_policy),
                execution_identity_path=pathlib.Path(args.execution_identity),
                report_path=pathlib.Path(args.report),
                log_path=pathlib.Path(args.log),
            )
            print(f"trusted cargo-geiger report published for {args.manifest}")
            return 0

        expected = evidence_from_args(args)
        if args.command == "prepare":
            write_evidence_envelope(pathlib.Path(args.evidence_envelope), expected)
            print(
                "cargo-geiger QuickSafetyReport evidence prepared for "
                f"{args.required_workspace_package_id}"
            )
            return 0

        observed = load_evidence_envelope(pathlib.Path(args.evidence_envelope))
        verify_evidence_envelope(observed, expected)
        report = require_object(
            json.loads(pathlib.Path(args.report).read_text(encoding="utf-8")),
            "cargo-geiger report",
        )
        print(
            f"unsafe inventory verified {args.required_workspace_package_id} across "
            f"{len(require_array(report['packages'], 'unsafe inventory packages'))} "
            "resolved packages"
        )
        return 0
    except (
        OSError,
        ValueError,
        json.JSONDecodeError,
        tomllib.TOMLDecodeError,
    ) as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
