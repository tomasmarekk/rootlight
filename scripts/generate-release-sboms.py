#!/usr/bin/env python3
"""Generate deterministic target-specific CycloneDX release SBOMs."""

from __future__ import annotations

import argparse
import base64
import binascii
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
from urllib.parse import quote
import uuid


PACKAGES = (
    ("rootlight", Path("apps/rootlight/Cargo.toml")),
    ("rootlight-adapter-host", Path("apps/rootlight-adapter-host/Cargo.toml")),
    ("rootlight-daemon", Path("apps/rootlight-daemon/Cargo.toml")),
    ("rootlight-launcher", Path("apps/rootlight-launcher/Cargo.toml")),
    ("rootlight-mcp", Path("apps/rootlight-mcp/Cargo.toml")),
    ("rootlight-semantic-host", Path("apps/rootlight-semantic-host/Cargo.toml")),
    ("rootlight-web", Path("apps/rootlight-web/Cargo.toml")),
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
ASSET_MANIFEST_SCHEMA = 1
MAX_WEB_ASSETS = 1_024
MAX_WEB_ASSET_BYTES = 16 * 1024 * 1024
MAX_WEB_ASSET_TOTAL_BYTES = 64 * 1024 * 1024
NPM_LOCK_PATH = Path("apps/rootlight-web/frontend/package-lock.json")
NPM_PACKAGE_PATH = Path("apps/rootlight-web/frontend/package.json")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--web-assets-dir", required=True, type=Path)
    parser.add_argument("--web-notices", required=True, type=Path)
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
        normalized = value.replace("\\", "/").replace(workspace_text, "${WORKSPACE}")
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


def read_bounded_regular(path: Path, maximum_bytes: int) -> bytes:
    metadata = path.lstat()
    if (
        is_link_or_reparse(path, metadata)
        or not stat.S_ISREG(metadata.st_mode)
        or metadata.st_size > maximum_bytes
    ):
        raise ValueError(f"release input is not a bounded regular file: {path}")
    content = path.read_bytes()
    if len(content) != metadata.st_size:
        raise ValueError(f"release input changed while it was read: {path}")
    return content


def is_link_or_reparse(path: Path, metadata: os.stat_result | None = None) -> bool:
    attributes = getattr(
        metadata if metadata is not None else path.lstat(), "st_file_attributes", 0
    )
    return (
        path.is_symlink()
        or (hasattr(path, "is_junction") and path.is_junction())
        or attributes & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0) != 0
    )


def read_web_asset(root: Path, relative: str) -> bytes:
    current = root
    components = relative.split("/")
    for index, component in enumerate(components):
        current /= component
        metadata = current.lstat()
        if is_link_or_reparse(current, metadata):
            raise ValueError(f"web asset path contains a link: {relative}")
        if index < len(components) - 1 and not stat.S_ISDIR(metadata.st_mode):
            raise ValueError(f"web asset parent is not a directory: {relative}")
    return read_bounded_regular(current, MAX_WEB_ASSET_BYTES)


def observed_web_assets(root: Path) -> set[str]:
    observed: set[str] = set()
    pending = [root]
    while pending:
        directory = pending.pop()
        with os.scandir(directory) as entries:
            children = sorted(entries, key=lambda entry: entry.name)
        for entry in children:
            path = Path(entry.path)
            metadata = path.lstat()
            if is_link_or_reparse(path, metadata):
                raise ValueError("web asset tree contains a link")
            if stat.S_ISDIR(metadata.st_mode):
                pending.append(path)
            elif stat.S_ISREG(metadata.st_mode):
                relative = path.relative_to(root).as_posix()
                if relative != "asset-manifest.json":
                    observed.add(relative)
            else:
                raise ValueError("web asset tree contains an unsupported file type")
    return observed


def web_asset_path_valid(path: str) -> bool:
    if (
        not path
        or len(path.encode("utf-8")) > 512
        or path.startswith("/")
        or path.endswith("/")
        or "\\" in path
        or "\0" in path
        or path.endswith(".map")
        or any(component in ("", ".", "..") for component in path.split("/"))
    ):
        return False
    if path == "index.html":
        return True
    if not path.startswith("assets/") or "/" in path.removeprefix("assets/"):
        return False
    name = path.removeprefix("assets/")
    stem = name.rsplit(".", 1)[0]
    content_hash = stem.rsplit("-", 1)[-1]
    return len(content_hash) >= 8 and content_hash.isascii() and content_hash.isalnum()


def web_asset_components(
    asset_root: Path, notices: Path, target: str
) -> list[dict[str, object]]:
    root_metadata = asset_root.lstat()
    if is_link_or_reparse(asset_root, root_metadata) or not stat.S_ISDIR(
        root_metadata.st_mode
    ):
        raise ValueError("web asset root must be a non-link directory")
    manifest_path = asset_root / "asset-manifest.json"
    manifest_bytes = read_bounded_regular(manifest_path, 1024 * 1024)
    try:
        manifest = json.loads(manifest_bytes)
    except json.JSONDecodeError as error:
        raise ValueError("web asset manifest is not valid JSON") from error
    if (
        not isinstance(manifest, dict)
        or set(manifest) != {"schema_version", "assets"}
        or manifest.get("schema_version") != ASSET_MANIFEST_SCHEMA
        or not isinstance(manifest.get("assets"), list)
        or not 0 < len(manifest["assets"]) <= MAX_WEB_ASSETS
    ):
        raise ValueError("web asset manifest identity or count is invalid")

    components: list[dict[str, object]] = []
    declared: set[str] = set()
    previous = ""
    total_bytes = 0
    for record in manifest["assets"]:
        if (
            not isinstance(record, dict)
            or set(record) != {"path", "bytes", "sha256"}
            or not isinstance(record.get("path"), str)
            or not isinstance(record.get("bytes"), int)
            or isinstance(record.get("bytes"), bool)
            or not isinstance(record.get("sha256"), str)
        ):
            raise ValueError("web asset manifest record is invalid")
        path = record["path"]
        size = record["bytes"]
        sha256 = record["sha256"]
        if (
            path <= previous
            or path in declared
            or not web_asset_path_valid(path)
            or not 0 < size <= MAX_WEB_ASSET_BYTES
            or not re.fullmatch(r"[0-9a-f]{64}", sha256)
        ):
            raise ValueError("web asset manifest record violates the package contract")
        previous = path
        declared.add(path)
        total_bytes += size
        if total_bytes > MAX_WEB_ASSET_TOTAL_BYTES:
            raise ValueError("web asset manifest exceeds its total-byte limit")
        content = read_web_asset(asset_root, path)
        if len(content) != size or hashlib.sha256(content).hexdigest() != sha256:
            raise ValueError(f"web asset content differs from its manifest: {path}")
        package_path = f"share/rootlight/web/{path}"
        components.append(
            {
                "bom-ref": f"urn:rootlight:asset:{target}:{package_path}",
                "hashes": [{"alg": "SHA-256", "content": sha256}],
                "name": package_path,
                "properties": [
                    {"name": "rootlight:asset:bytes", "value": str(size)},
                    {"name": "rootlight:package:path", "value": package_path},
                ],
                "type": "file",
            }
        )

    observed = observed_web_assets(asset_root)
    if observed != declared or "index.html" not in declared:
        raise ValueError("web asset tree differs from its manifest")

    components.append(
        {
            "bom-ref": (
                f"urn:rootlight:asset:{target}:share/rootlight/web/asset-manifest.json"
            ),
            "hashes": [
                {
                    "alg": "SHA-256",
                    "content": hashlib.sha256(manifest_bytes).hexdigest(),
                }
            ],
            "name": "share/rootlight/web/asset-manifest.json",
            "properties": [
                {
                    "name": "rootlight:package:path",
                    "value": "share/rootlight/web/asset-manifest.json",
                }
            ],
            "type": "file",
        }
    )
    notice_bytes = read_bounded_regular(notices, 8 * 1024 * 1024)
    if not notice_bytes:
        raise ValueError("web third-party notice inventory is empty")
    components.append(
        {
            "bom-ref": (
                f"urn:rootlight:asset:{target}:"
                "licenses/rootlight-web-third-party-notices.txt"
            ),
            "hashes": [
                {
                    "alg": "SHA-256",
                    "content": hashlib.sha256(notice_bytes).hexdigest(),
                }
            ],
            "name": "licenses/rootlight-web-third-party-notices.txt",
            "properties": [
                {
                    "name": "rootlight:package:path",
                    "value": "licenses/rootlight-web-third-party-notices.txt",
                }
            ],
            "type": "file",
        }
    )
    components.sort(key=component_ref)
    return components


def npm_name_from_lock_path(path: str) -> str:
    marker = "node_modules/"
    if marker not in path:
        raise ValueError(f"npm lock path is not a package path: {path}")
    name = path.rsplit(marker, 1)[1]
    if not name or (name.startswith("@") and name.count("/") != 1):
        raise ValueError(f"npm lock package name is invalid: {path}")
    return name


def npm_purl(name: str, version: str) -> str:
    if name.startswith("@"):
        namespace, package = name.split("/", 1)
        qualified = f"{quote(namespace, safe='')}/{quote(package, safe='')}"
    else:
        qualified = quote(name, safe="")
    return f"pkg:npm/{qualified}@{quote(version, safe='')}"


def npm_integrity_hash(integrity: str) -> dict[str, str]:
    algorithm, separator, encoded = integrity.partition("-")
    algorithms = {
        "sha256": ("SHA-256", 32),
        "sha384": ("SHA-384", 48),
        "sha512": ("SHA-512", 64),
    }
    if not separator or algorithm not in algorithms:
        raise ValueError("npm package integrity uses an unsupported hash")
    try:
        digest = base64.b64decode(encoded, validate=True)
    except (binascii.Error, ValueError) as error:
        raise ValueError("npm package integrity is not canonical base64") from error
    cyclonedx_name, expected_bytes = algorithms[algorithm]
    if len(digest) != expected_bytes:
        raise ValueError("npm package integrity digest has the wrong length")
    return {"alg": cyclonedx_name, "content": digest.hex()}


def resolve_npm_dependency(
    packages: dict[str, object], package_path: str, dependency_name: str
) -> str | None:
    current = f"/{package_path}" if package_path else ""
    while True:
        candidate = f"{current}/node_modules/{dependency_name}".lstrip("/")
        if candidate in packages:
            return candidate
        if "/node_modules/" not in current:
            return None
        current = current.rsplit("/node_modules/", 1)[0]


def npm_components(
    workspace: Path,
) -> tuple[
    dict[str, dict[str, object]],
    dict[str, dict[str, object]],
    dict[str, object],
]:
    lock_bytes = read_bounded_regular(workspace / NPM_LOCK_PATH, 16 * 1024 * 1024)
    package_bytes = read_bounded_regular(workspace / NPM_PACKAGE_PATH, 1024 * 1024)
    try:
        lock = json.loads(lock_bytes)
        package_document = json.loads(package_bytes)
    except json.JSONDecodeError as error:
        raise ValueError("npm package metadata is not valid JSON") from error
    packages = lock.get("packages") if isinstance(lock, dict) else None
    root = packages.get("") if isinstance(packages, dict) else None
    if (
        lock.get("lockfileVersion") != 3
        or not isinstance(packages, dict)
        or not isinstance(root, dict)
        or package_document.get("name") != "@rootlight/web-ui"
        or package_document.get("version") != root.get("version")
    ):
        raise ValueError("npm package metadata identity is invalid")

    components: dict[str, dict[str, object]] = {}
    path_to_ref: dict[str, str] = {}
    for path in sorted(item for item in packages if item):
        package = packages[path]
        if not isinstance(package, dict):
            raise ValueError(f"npm package record is invalid: {path}")
        name = npm_name_from_lock_path(path)
        version = package.get("version")
        license_expression = package.get("license")
        if (
            not isinstance(version, str)
            or not version
            or not isinstance(license_expression, str)
            or not license_expression
        ):
            raise ValueError(f"npm package identity or license is missing: {path}")
        identity = hashlib.sha256(path.encode("utf-8")).hexdigest()
        reference = f"urn:rootlight:npm-lock:{identity}"
        path_to_ref[path] = reference
        component: dict[str, object] = {
            "bom-ref": reference,
            "licenses": [{"expression": license_expression}],
            "name": name,
            "properties": [
                {
                    "name": "rootlight:npm:development",
                    "value": str(package.get("dev") is True).lower(),
                },
                {"name": "rootlight:npm:lock-path", "value": path},
                {
                    "name": "rootlight:npm:optional",
                    "value": str(package.get("optional") is True).lower(),
                },
            ],
            "purl": npm_purl(name, version),
            "type": "library",
            "version": version,
        }
        bundled_dependencies = package.get("bundleDependencies", [])
        if not isinstance(bundled_dependencies, list) or not all(
            isinstance(item, str) for item in bundled_dependencies
        ):
            raise ValueError(f"npm bundled dependency field is invalid: {path}")
        for dependency_name in sorted(bundled_dependencies):
            properties = component["properties"]
            if not isinstance(properties, list):
                raise AssertionError("npm component properties changed type")
            properties.append(
                {
                    "name": "rootlight:npm:bundled-dependency",
                    "value": dependency_name,
                }
            )
        integrity = package.get("integrity")
        if isinstance(integrity, str):
            component["hashes"] = [npm_integrity_hash(integrity)]
        elif package.get("inBundle") is not True:
            raise ValueError(f"npm package lacks integrity and is not bundled: {path}")
        components[reference] = component

    root_reference = "urn:rootlight:npm-application:web-ui"
    root_component: dict[str, object] = {
        "bom-ref": root_reference,
        "name": package_document["name"],
        "properties": [
            {
                "name": "rootlight:npm:package-json-sha256",
                "value": hashlib.sha256(package_bytes).hexdigest(),
            },
            {
                "name": "rootlight:npm:package-lock-sha256",
                "value": hashlib.sha256(lock_bytes).hexdigest(),
            },
        ],
        "type": "application",
        "version": package_document["version"],
    }
    components[root_reference] = root_component

    dependencies: dict[str, dict[str, object]] = {}
    for path, package in packages.items():
        if not isinstance(package, dict):
            raise ValueError(f"npm package record is invalid: {path}")
        reference = root_reference if path == "" else path_to_ref[path]
        edges: set[str] = set()
        for field in (
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
        ):
            names = package.get(field, {})
            if not isinstance(names, dict):
                raise ValueError(f"npm dependency field is invalid: {path}:{field}")
            for name in names:
                resolved = resolve_npm_dependency(packages, path, name)
                if resolved is None:
                    # Optional and peer requirements may legitimately be absent
                    # from the concrete lock graph for this platform.
                    bundled = package.get("bundleDependencies", [])
                    if (
                        field in ("optionalDependencies", "peerDependencies")
                        or isinstance(bundled, list)
                        and name in bundled
                    ):
                        continue
                    raise ValueError(
                        f"npm dependency is not represented in the lock graph: {path}:{name}"
                    )
                edges.add(path_to_ref[resolved])
        dependencies[reference] = {
            "ref": reference,
            "dependsOn": sorted(edges),
        }
    return components, dependencies, root_component


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
    web_assets_dir: Path,
    web_notices: Path,
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

    npm_inventory, npm_dependencies, npm_root = npm_components(workspace)
    for identity, component in npm_inventory.items():
        previous = components.setdefault(identity, component)
        if previous != component:
            raise ValueError(f"conflicting npm CycloneDX component: {identity}")
    for identity, dependency in npm_dependencies.items():
        previous = dependencies.setdefault(identity, dependency)
        if previous != dependency:
            raise ValueError(f"conflicting npm CycloneDX dependency: {identity}")
    assets = [
        asset_component(workspace, target, package_path, source)
        for package_path, source in (*COMMON_ASSETS, autostart_asset(target))
    ]
    assets.extend(web_asset_components(web_assets_dir, web_notices, target))
    assets.sort(key=component_ref)
    distribution_ref = f"urn:rootlight:distribution:{version}:{target}"
    child_refs = [component_ref(component) for component in (*roots, npm_root, *assets)]
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
                    "version": "2",
                },
                {
                    "name": "node",
                    "vendor": "OpenJS Foundation",
                    "version": "24.11.1",
                },
                {
                    "name": "npm",
                    "vendor": "npm",
                    "version": "11.6.2",
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
        raise ValueError(
            "release SBOM output must stay inside the workspace"
        ) from error
    if resolved == workspace:
        raise ValueError("release SBOM output cannot be the workspace root")
    return resolved


def require_workspace_input(workspace: Path, path: Path, name: str) -> Path:
    candidate = Path(os.path.abspath(path))
    try:
        relative = candidate.relative_to(workspace)
    except ValueError as error:
        raise ValueError(f"{name} must stay inside the workspace") from error
    current = workspace
    for component in relative.parts:
        current /= component
        metadata = current.lstat()
        if is_link_or_reparse(current, metadata):
            raise ValueError(f"{name} path contains a link or reparse point")
    resolved = candidate.resolve(strict=True)
    if os.path.normcase(resolved) != os.path.normcase(candidate):
        raise ValueError(f"{name} path does not resolve to its lexical location")
    return resolved


def main() -> None:
    args = parse_args()
    if not SOURCE_REVISION.fullmatch(args.source_revision):
        raise ValueError("source revision must be 40 lowercase hexadecimal characters")
    workspace = Path.cwd().resolve(strict=True)
    if not (workspace / "Cargo.toml").is_file():
        raise ValueError("run release SBOM generation from the workspace root")
    output = require_safe_output(workspace, args.output)
    web_assets_dir = require_workspace_input(
        workspace, args.web_assets_dir, "web asset directory"
    )
    web_notices = require_workspace_input(workspace, args.web_notices, "web notices")
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
                web_assets_dir,
                web_notices,
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
        checksum_lines.append(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {relative}\n"
        )
    if len(checksum_lines) != len(TARGETS):
        raise ValueError("release SBOM output set differs from target contract")
    (output / "SHA256SUMS").write_text(
        "".join(checksum_lines),
        encoding="utf-8",
        newline="\n",
    )


if __name__ == "__main__":
    main()
