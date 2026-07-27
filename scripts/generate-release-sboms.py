#!/usr/bin/env python3
"""Generate deterministic target-specific CycloneDX release SBOMs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import uuid


PACKAGES = (
    ("rootlight", Path("apps/rootlight/Cargo.toml")),
    ("rootlight-adapter-host", Path("apps/rootlight-adapter-host/Cargo.toml")),
    ("rootlight-daemon", Path("apps/rootlight-daemon/Cargo.toml")),
    ("rootlight-launcher", Path("apps/rootlight-launcher/Cargo.toml")),
    ("rootlight-mcp", Path("apps/rootlight-mcp/Cargo.toml")),
)
TARGETS = (
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
)
COMMON_ASSETS = (
    ("LICENSE", Path("LICENSE")),
    ("NOTICE", Path("NOTICE")),
    (
        "licenses/tree-sitter-cpp-0.23.4-LICENSE",
        Path("adapters/licenses/tree-sitter-cpp-0.23.4-LICENSE"),
    ),
    (
        "licenses/tree-sitter-java-0.23.5-LICENSE",
        Path("adapters/licenses/tree-sitter-java-0.23.5-LICENSE"),
    ),
    (
        "licenses/tree-sitter-kotlin-ng-1.1.0-LICENSE",
        Path("adapters/licenses/tree-sitter-kotlin-ng-1.1.0-LICENSE"),
    ),
    (
        "licenses/tree-sitter-typescript-0.23.2-LICENSE",
        Path("adapters/licenses/tree-sitter-typescript-0.23.2-LICENSE"),
    ),
)
AUTOSTART_ASSETS = {
    "apple-darwin": (
        "autostart/com.rootlight.daemon.plist",
        Path("packaging/autostart/macos/com.rootlight.daemon.plist"),
    ),
    "pc-windows-msvc": (
        "autostart/rootlight-daemon.xml",
        Path("packaging/autostart/windows/rootlight-daemon.xml"),
    ),
    "unknown-linux-gnu": (
        "autostart/rootlight-daemon.service",
        Path("packaging/autostart/linux/rootlight-daemon.service"),
    ),
}
SOURCE_REVISION = re.compile(r"^[0-9a-f]{40}$")
ABSOLUTE_WINDOWS = re.compile(r"^(?:file:///)?[A-Za-z]:/")
TARGET_PROPERTY = "cdx:rustc:sbom:target:triple"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--source-revision", required=True)
    return parser.parse_args()


def workspace_version(workspace: Path) -> str:
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=workspace,
        check=True,
        capture_output=True,
        text=True,
    )
    metadata = json.loads(result.stdout)
    versions = {
        package["version"]
        for package in metadata["packages"]
        if package["name"] == "rootlight"
    }
    if len(versions) != 1:
        raise ValueError(f"expected one rootlight version, found {sorted(versions)!r}")
    return versions.pop()


def normalize(value: object, workspace: Path) -> object:
    workspace_text = workspace.as_posix()
    if isinstance(value, dict):
        return {key: normalize(item, workspace) for key, item in value.items()}
    if isinstance(value, list):
        return [normalize(item, workspace) for item in value]
    if isinstance(value, str):
        normalized = value.replace("\\", "/").replace(
            workspace_text, "${WORKSPACE}"
        )
        if normalized.startswith("/") or ABSOLUTE_WINDOWS.match(normalized):
            raise ValueError(f"SBOM contains an absolute path: {normalized}")
        return normalized
    return value


def target_property(document: dict[str, object]) -> str | None:
    metadata = document.get("metadata")
    if not isinstance(metadata, dict):
        return None
    properties = metadata.get("properties")
    if not isinstance(properties, list):
        return None
    for item in properties:
        if isinstance(item, dict) and item.get("name") == TARGET_PROPERTY:
            value = item.get("value")
            return value if isinstance(value, str) else None
    return None


def generate_package_sbom(
    workspace: Path,
    package_name: str,
    manifest: Path,
    target: str,
) -> dict[str, object]:
    manifest_path = workspace / manifest
    generated = manifest_path.parent / f"{package_name}_{target}.cdx.json"
    if generated.exists() or generated.is_symlink():
        generated.unlink()
    environment = os.environ.copy()
    environment["SOURCE_DATE_EPOCH"] = "0"
    subprocess.run(
        [
            "cargo",
            "cyclonedx",
            "--manifest-path",
            str(manifest_path),
            "--all-features",
            "--target",
            target,
            "--target-in-filename",
            "--format",
            "json",
            "--spec-version",
            "1.5",
        ],
        cwd=workspace,
        env=environment,
        check=True,
    )
    try:
        document = normalize(json.loads(generated.read_bytes()), workspace)
    finally:
        if generated.exists() or generated.is_symlink():
            generated.unlink()
    if not isinstance(document, dict):
        raise ValueError(f"{package_name} SBOM is not a JSON object")
    if document.get("bomFormat") != "CycloneDX":
        raise ValueError(f"{package_name} SBOM is not CycloneDX")
    if document.get("specVersion") != "1.5":
        raise ValueError(f"{package_name} SBOM is not CycloneDX 1.5")
    if target_property(document) != target:
        raise ValueError(f"{package_name} SBOM target does not match {target}")
    return document


def keyed_items(
    documents: list[dict[str, object]], field: str, key: str
) -> dict[str, dict[str, object]]:
    merged: dict[str, dict[str, object]] = {}
    for document in documents:
        items = document.get(field, [])
        if not isinstance(items, list):
            raise ValueError(f"CycloneDX {field} must be an array")
        for item in items:
            if not isinstance(item, dict) or not isinstance(item.get(key), str):
                raise ValueError(f"CycloneDX {field} entry lacks {key}")
            identity = item[key]
            previous = merged.setdefault(identity, item)
            if previous != item:
                raise ValueError(f"conflicting CycloneDX {field} entry: {identity}")
    return merged


def merged_dependencies(
    documents: list[dict[str, object]],
) -> dict[str, dict[str, object]]:
    merged: dict[str, dict[str, object]] = {}
    for document in documents:
        dependencies = document.get("dependencies", [])
        if not isinstance(dependencies, list):
            raise ValueError("CycloneDX dependencies must be an array")
        for dependency in dependencies:
            if not isinstance(dependency, dict) or not isinstance(
                dependency.get("ref"), str
            ):
                raise ValueError("CycloneDX dependency lacks ref")
            identity = dependency["ref"]
            depends_on = dependency.get("dependsOn", [])
            if not isinstance(depends_on, list) or not all(
                isinstance(item, str) for item in depends_on
            ):
                raise ValueError(f"CycloneDX dependency {identity} has invalid edges")
            unsupported = set(dependency) - {"ref", "dependsOn"}
            if unsupported:
                raise ValueError(
                    f"CycloneDX dependency {identity} has unsupported fields: "
                    f"{sorted(unsupported)!r}"
                )
            current = merged.setdefault(identity, {"ref": identity, "dependsOn": []})
            current_edges = current["dependsOn"]
            if not isinstance(current_edges, list):
                raise AssertionError("merged dependency edges changed type")
            current["dependsOn"] = sorted(set(current_edges) | set(depends_on))
    return merged


def component_ref(component: dict[str, object]) -> str:
    identity = component.get("bom-ref")
    if not isinstance(identity, str):
        raise ValueError("CycloneDX root component lacks bom-ref")
    return identity


def asset_component(
    workspace: Path, target: str, package_path: str, source: Path
) -> dict[str, object]:
    content = (workspace / source).read_bytes()
    return {
        "bom-ref": f"urn:rootlight:asset:{target}:{package_path}",
        "hashes": [
            {
                "alg": "SHA-256",
                "content": hashlib.sha256(content).hexdigest(),
            }
        ],
        "name": package_path,
        "properties": [
            {"name": "rootlight:package:path", "value": package_path},
        ],
        "type": "file",
    }


def autostart_asset(target: str) -> tuple[str, Path]:
    for suffix, asset in AUTOSTART_ASSETS.items():
        if target.endswith(suffix):
            return asset
    raise ValueError(f"target has no autostart asset: {target}")


def merge_release_sbom(
    workspace: Path,
    documents: list[dict[str, object]],
    target: str,
    version: str,
    source_revision: str,
) -> dict[str, object]:
    components = keyed_items(documents, "components", "bom-ref")
    dependencies = merged_dependencies(documents)
    roots: list[dict[str, object]] = []
    for document in documents:
        metadata = document.get("metadata")
        if not isinstance(metadata, dict):
            raise ValueError("CycloneDX metadata must be an object")
        root = metadata.get("component")
        if not isinstance(root, dict):
            raise ValueError("CycloneDX metadata lacks root component")
        roots.append(root)
    roots.sort(key=component_ref)
    if [root.get("name") for root in roots] != sorted(name for name, _ in PACKAGES):
        raise ValueError("release SBOM package roots differ from package contract")

    assets = [
        asset_component(workspace, target, package_path, source)
        for package_path, source in (*COMMON_ASSETS, autostart_asset(target))
    ]
    assets.sort(key=component_ref)
    distribution_ref = f"urn:rootlight:distribution:{version}:{target}"
    child_refs = [component_ref(component) for component in (*roots, *assets)]
    dependencies[distribution_ref] = {
        "ref": distribution_ref,
        "dependsOn": sorted(child_refs),
    }
    serial = uuid.uuid5(
        uuid.NAMESPACE_URL,
        f"https://github.com/tomasmarekk/rootlight/{source_revision}/{target}",
    )
    return {
        "bomFormat": "CycloneDX",
        "components": [components[key] for key in sorted(components)],
        "dependencies": [dependencies[key] for key in sorted(dependencies)],
        "metadata": {
            "component": {
                "bom-ref": distribution_ref,
                "components": [*roots, *assets],
                "externalReferences": [
                    {
                        "type": "vcs",
                        "url": "https://github.com/tomasmarekk/rootlight",
                    }
                ],
                "name": "rootlight-distribution",
                "properties": [
                    {"name": "rootlight:build:profile", "value": "release"},
                    {
                        "name": "rootlight:source:revision",
                        "value": source_revision,
                    },
                    {"name": "rootlight:target:triple", "value": target},
                ],
                "type": "application",
                "version": version,
            },
            "properties": [
                {"name": TARGET_PROPERTY, "value": target},
                {"name": "rootlight:build:profile", "value": "release"},
                {"name": "rootlight:source:revision", "value": source_revision},
            ],
            "timestamp": "1970-01-01T00:00:00Z",
            "tools": [
                {
                    "name": "cargo-cyclonedx",
                    "vendor": "CycloneDX",
                    "version": "0.5.9",
                },
                {
                    "name": "generate-release-sboms.py",
                    "vendor": "Rootlight",
                    "version": "1",
                },
            ],
        },
        "serialNumber": f"urn:uuid:{serial}",
        "specVersion": "1.5",
        "version": 1,
    }


def require_safe_output(workspace: Path, output: Path) -> Path:
    resolved = output.resolve(strict=False)
    try:
        resolved.relative_to(workspace)
    except ValueError as error:
        raise ValueError("release SBOM output must stay inside the workspace") from error
    if resolved == workspace:
        raise ValueError("release SBOM output cannot be the workspace root")
    return resolved


def main() -> None:
    args = parse_args()
    if not SOURCE_REVISION.fullmatch(args.source_revision):
        raise ValueError("source revision must be 40 lowercase hexadecimal characters")
    workspace = Path.cwd().resolve(strict=True)
    if not (workspace / "Cargo.toml").is_file():
        raise ValueError("run release SBOM generation from the workspace root")
    output = require_safe_output(workspace, args.output)
    if output.exists() or output.is_symlink():
        if output.is_symlink() or not output.is_dir():
            raise ValueError("release SBOM output must be a non-symlink directory")
        shutil.rmtree(output)
    output.mkdir(parents=True)

    version_output = subprocess.run(
        ["cargo", "cyclonedx", "--version"],
        cwd=workspace,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if not version_output.endswith(" 0.5.9"):
        raise ValueError(f"unsupported cargo-cyclonedx version: {version_output}")
    version = workspace_version(workspace)
    for target in TARGETS:
        documents = [
            generate_package_sbom(workspace, package_name, manifest, target)
            for package_name, manifest in PACKAGES
        ]
        release_sbom = normalize(
            merge_release_sbom(
                workspace,
                documents,
                target,
                version,
                args.source_revision,
            ),
            workspace,
        )
        target_output = output / target
        target_output.mkdir()
        (target_output / "rootlight-distribution.cdx.json").write_text(
            json.dumps(release_sbom, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
            newline="\n",
        )

    checksum_lines = []
    for path in sorted(output.glob("*/rootlight-distribution.cdx.json")):
        relative = path.relative_to(output).as_posix()
        checksum_lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {relative}\n")
    if len(checksum_lines) != len(TARGETS):
        raise ValueError("release SBOM output set differs from target contract")
    (output / "SHA256SUMS").write_text(
        "".join(checksum_lines),
        encoding="utf-8",
        newline="\n",
    )


if __name__ == "__main__":
    main()
