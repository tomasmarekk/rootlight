#!/usr/bin/env python3
"""Publish an idempotent, provenance-bearing Rootlight npm package set."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

PACKAGE_NAMES = (
    "@tomasmarekk/rootlight-darwin-arm64",
    "@tomasmarekk/rootlight-linux-arm64-gnu",
    "@tomasmarekk/rootlight-darwin-x64",
    "@tomasmarekk/rootlight-linux-x64-gnu",
    "@tomasmarekk/rootlight-win32-x64-msvc",
    "@tomasmarekk/rootlight",
)
ROOT_PACKAGE = "@tomasmarekk/rootlight"
ROOT_LIFECYCLE_SCRIPTS = {
    "postinstall": "node ./bin/postinstall.mjs",
    "preuninstall": "node ./bin/preuninstall.mjs",
}
PACKAGE_LICENSE = "AGPL-3.0-only"
VERSION_PATTERN = re.compile(
    r"^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-alpha\.(?:0|[1-9][0-9]*))?$"
)
SOURCE_REVISION_PATTERN = re.compile(r"^[0-9a-f]{40}$")
INTEGRITY_PATTERN = re.compile(r"^sha512-[A-Za-z0-9+/]+={0,2}$")
SHASUM_PATTERN = re.compile(r"^[0-9a-f]{40}$")
MAX_COMMAND_OUTPUT_BYTES = 1024 * 1024
REGISTRY_PROPAGATION_SECONDS = 120


class NpmPublicationError(RuntimeError):
    """An npm package identity, registry, or publication invariant failed."""


def parse_args(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--packages-dir", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", choices=("alpha", "latest"), required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args(arguments)


def main(arguments: list[str]) -> int:
    options = parse_args(arguments)
    try:
        publish_packages(
            options.packages_dir,
            options.version,
            options.tag,
            options.source_revision,
            options.output,
        )
    except (OSError, NpmPublicationError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


def publish_packages(
    packages_dir: Path,
    version: str,
    tag: str,
    source_revision: str,
    output: Path,
) -> None:
    validate_runtime()
    if VERSION_PATTERN.fullmatch(version) is None:
        raise NpmPublicationError("npm release version is invalid")
    if SOURCE_REVISION_PATTERN.fullmatch(source_revision) is None:
        raise NpmPublicationError("npm source revision is invalid")
    expected_tag = "alpha" if "-alpha." in version else "latest"
    if tag != expected_tag:
        raise NpmPublicationError("npm distribution tag differs from release channel")

    packages_dir = packages_dir.resolve()
    if not packages_dir.is_dir() or packages_dir.is_symlink():
        raise NpmPublicationError("npm package directory is invalid")
    order = read_publish_order(packages_dir)
    tarballs = packages_dir / "tarballs"
    tarballs.mkdir()
    audit_packages = []

    for expected_name, record in zip(PACKAGE_NAMES, order, strict=True):
        if record["name"] != expected_name:
            raise NpmPublicationError("npm publication order differs")
        package_dir = (packages_dir / record["directory"]).resolve()
        if (
            package_dir.parent != packages_dir
            or not package_dir.is_dir()
            or package_dir.is_symlink()
        ):
            raise NpmPublicationError("npm package path escapes the prepared set")
        package = read_json_regular(package_dir / "package.json", 128 * 1024)
        validate_package_json(package, expected_name, version, source_revision)
        packed = pack_package(package_dir, tarballs)
        package_spec = f"{expected_name}@{version}"
        registry = registry_dist(package_spec)
        status = "existing"
        if registry is None:
            publish_tarball(packed["path"], tag)
            registry = wait_for_registry(package_spec)
            status = "published"
        require_matching_dist(registry, packed)
        require_distribution_tag(expected_name, tag, version)
        audit_packages.append(
            {
                "integrity": packed["integrity"],
                "name": expected_name,
                "registry_url": f"https://www.npmjs.com/package/{expected_name}/v/{version}",
                "shasum": packed["shasum"],
                "status": status,
                "tarball_bytes": packed["size"],
                "version": version,
            }
        )

    write_json_new(
        output,
        {
            "channel_tag": tag,
            "packages": audit_packages,
            "schema": "rootlight.npm-publication/1",
            "source_revision": source_revision,
            "version": version,
        },
    )


def validate_runtime() -> None:
    node = command_output(["node", "--version"]).removeprefix("v").strip()
    npm = command_output(["npm", "--version"]).strip()
    if numeric_version(node) < (22, 14, 0) or numeric_version(npm) < (11, 5, 1):
        raise NpmPublicationError("Node.js or npm is too old for trusted publishing")
    if os.environ.get("NODE_AUTH_TOKEN"):
        raise NpmPublicationError("long-lived npm tokens are forbidden in release publishing")


def numeric_version(value: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"([0-9]+)\.([0-9]+)\.([0-9]+)", value)
    if match is None:
        raise NpmPublicationError("tool version is not canonical")
    return tuple(int(component) for component in match.groups())


def read_publish_order(packages_dir: Path) -> list[dict[str, str]]:
    value = read_json_regular(packages_dir / "publish-order.json", 64 * 1024)
    if not isinstance(value, list) or len(value) != len(PACKAGE_NAMES):
        raise NpmPublicationError("npm publication order is invalid")
    records = []
    for item in value:
        if (
            not isinstance(item, dict)
            or set(item) != {"directory", "name"}
            or not isinstance(item["directory"], str)
            or not isinstance(item["name"], str)
            or not re.fullmatch(r"[a-z0-9-]{1,80}", item["directory"])
        ):
            raise NpmPublicationError("npm publication record is invalid")
        records.append(item)
    return records


def validate_package_json(
    package: Any, name: str, version: str, source_revision: str
) -> None:
    if not isinstance(package, dict):
        raise NpmPublicationError("npm package manifest is invalid")
    expected = {
        "gitHead": source_revision,
        "license": PACKAGE_LICENSE,
        "name": name,
        "version": version,
    }
    if {key: package.get(key) for key in expected} != expected:
        raise NpmPublicationError("npm package identity differs")
    publish = package.get("publishConfig")
    if publish != {"access": "public", "provenance": True}:
        raise NpmPublicationError("npm package publication policy differs")
    repository = package.get("repository")
    if not isinstance(repository, dict) or repository.get("url") != (
        "git+https://github.com/tomasmarekk/rootlight.git"
    ):
        raise NpmPublicationError("npm package repository identity differs")
    expected_scripts = ROOT_LIFECYCLE_SCRIPTS if name == ROOT_PACKAGE else None
    if package.get("scripts") != expected_scripts:
        raise NpmPublicationError("npm package lifecycle policy differs")


def pack_package(package_dir: Path, tarballs: Path) -> dict[str, Any]:
    value = command_json(
        [
            "npm",
            "pack",
            str(package_dir),
            "--json",
            "--ignore-scripts",
            "--pack-destination",
            str(tarballs),
        ]
    )
    if not isinstance(value, list) or len(value) != 1 or not isinstance(value[0], dict):
        raise NpmPublicationError("npm pack returned an unexpected document")
    record = value[0]
    required = ("filename", "integrity", "shasum", "size")
    if any(key not in record for key in required):
        raise NpmPublicationError("npm pack omitted package identity")
    filename = record["filename"]
    integrity = record["integrity"]
    shasum = record["shasum"]
    size = record["size"]
    if (
        not isinstance(filename, str)
        or not re.fullmatch(r"[A-Za-z0-9_.-]+\.tgz", filename)
        or not isinstance(integrity, str)
        or INTEGRITY_PATTERN.fullmatch(integrity) is None
        or not isinstance(shasum, str)
        or SHASUM_PATTERN.fullmatch(shasum) is None
        or not isinstance(size, int)
        or isinstance(size, bool)
        or not 0 < size <= 1024 * 1024 * 1024
    ):
        raise NpmPublicationError("npm pack returned invalid package identity")
    path = (tarballs / filename).resolve()
    if path.parent != tarballs.resolve() or not path.is_file() or path.is_symlink():
        raise NpmPublicationError("npm pack tarball path is invalid")
    return {
        "integrity": integrity,
        "path": path,
        "shasum": shasum,
        "size": size,
    }


def registry_dist(package_spec: str) -> dict[str, Any] | None:
    completed = run_command(["npm", "view", package_spec, "dist", "--json"])
    if completed.returncode == 0:
        try:
            value = json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise NpmPublicationError("npm registry returned invalid JSON") from error
        if not isinstance(value, dict):
            raise NpmPublicationError("npm registry returned an unexpected dist document")
        return value
    if "E404" in completed.stderr or "404 Not Found" in completed.stderr:
        return None
    raise NpmPublicationError("npm registry lookup failed")


def publish_tarball(path: Path, tag: str) -> None:
    completed = run_command(
        [
            "npm",
            "publish",
            str(path),
            "--access",
            "public",
            "--tag",
            tag,
            "--provenance",
            "--ignore-scripts",
        ]
    )
    if completed.returncode != 0:
        raise NpmPublicationError("npm trusted publication failed")


def wait_for_registry(package_spec: str) -> dict[str, Any]:
    deadline = time.monotonic() + REGISTRY_PROPAGATION_SECONDS
    while time.monotonic() < deadline:
        dist = registry_dist(package_spec)
        if dist is not None:
            return dist
        time.sleep(5)
    raise NpmPublicationError("published npm package did not become visible")


def require_matching_dist(registry: dict[str, Any], packed: dict[str, Any]) -> None:
    if (
        registry.get("integrity") != packed["integrity"]
        or registry.get("shasum") != packed["shasum"]
    ):
        raise NpmPublicationError("npm registry tarball identity differs")


def require_distribution_tag(name: str, tag: str, version: str) -> None:
    value = command_json(["npm", "view", name, "dist-tags", "--json"])
    if not isinstance(value, dict) or value.get(tag) != version:
        raise NpmPublicationError("npm distribution tag differs after publication")


def command_output(command: list[str]) -> str:
    completed = run_command(command)
    if completed.returncode != 0:
        raise NpmPublicationError("required publication tool failed")
    return completed.stdout


def command_json(command: list[str]) -> Any:
    output = command_output(command)
    try:
        return json.loads(output)
    except json.JSONDecodeError as error:
        raise NpmPublicationError("publication tool returned invalid JSON") from error


def run_command(command: list[str]) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        command,
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if (
        len(completed.stdout.encode("utf-8")) > MAX_COMMAND_OUTPUT_BYTES
        or len(completed.stderr.encode("utf-8")) > MAX_COMMAND_OUTPUT_BYTES
    ):
        raise NpmPublicationError("publication tool output exceeded the audit bound")
    return completed


def read_json_regular(path: Path, maximum: int) -> Any:
    if not path.is_file() or path.is_symlink() or path.stat().st_size > maximum:
        raise NpmPublicationError("npm publication input is invalid")
    try:
        return json.loads(path.read_bytes())
    except json.JSONDecodeError as error:
        raise NpmPublicationError("npm publication input is invalid JSON") from error


def write_json_new(path: Path, value: object) -> None:
    encoded = json.dumps(value, indent=2, sort_keys=True) + "\n"
    with path.open("x", encoding="utf-8", newline="\n") as output:
        output.write(encoded)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
