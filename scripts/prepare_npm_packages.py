#!/usr/bin/env python3
"""Build bounded npm packages from an exact native release candidate set."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import sys
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

PACKAGE_SCOPE = "@tomasmarekk"
ROOT_PACKAGE = f"{PACKAGE_SCOPE}/rootlight"
BOOTSTRAP_VERSION = "0.0.0-security-bootstrap.0"
MAX_ARCHIVE_BYTES = 1024 * 1024 * 1024
MAX_EXTRACTED_BYTES = 2 * 1024 * 1024 * 1024
MAX_ARCHIVE_ENTRIES = 128
MAX_PATH_BYTES = 240
REPOSITORY_URL = "git+https://github.com/tomasmarekk/rootlight.git"
SEMVER_PATTERN = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-((?:0|[1-9A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9A-Za-z-][0-9A-Za-z-]*))*))?$"
)
RELEASE_VERSION_PATTERN = re.compile(
    r"^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-alpha\.(?:0|[1-9][0-9]*))?$"
)
SOURCE_REVISION_PATTERN = re.compile(r"^[0-9a-f]{40}$")
PUBLIC_EXECUTABLES = ("rootlight", "rootlight-mcp")
NPM_RUNTIME_FILES = (
    "packaging/npm/rootlight.mjs",
    "packaging/npm/rootlight-mcp.mjs",
    "packaging/npm/run-native.mjs",
)


@dataclass(frozen=True)
class NativeTarget:
    triple: str
    package_suffix: str
    os_name: str
    cpu: str
    libc: str | None = None

    @property
    def package_name(self) -> str:
        return f"{PACKAGE_SCOPE}/rootlight-{self.package_suffix}"

    @property
    def directory_name(self) -> str:
        return self.package_name.removeprefix(f"{PACKAGE_SCOPE}/")


TARGETS = (
    NativeTarget("aarch64-apple-darwin", "darwin-arm64", "darwin", "arm64"),
    NativeTarget(
        "aarch64-unknown-linux-gnu", "linux-arm64-gnu", "linux", "arm64", "glibc"
    ),
    NativeTarget("x86_64-apple-darwin", "darwin-x64", "darwin", "x64"),
    NativeTarget(
        "x86_64-unknown-linux-gnu", "linux-x64-gnu", "linux", "x64", "glibc"
    ),
    NativeTarget("x86_64-pc-windows-msvc", "win32-x64-msvc", "win32", "x64"),
)


class PackagePreparationError(RuntimeError):
    """A source-redacted npm package preparation failure."""


def parse_args(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="mode", required=True)

    release = subparsers.add_parser("release")
    release.add_argument("--candidates-dir", type=Path, required=True)
    release.add_argument("--version", required=True)
    release.add_argument("--source-revision", required=True)
    release.add_argument("--output-dir", type=Path, required=True)
    release.add_argument("--workspace", type=Path, default=Path.cwd())

    bootstrap = subparsers.add_parser("bootstrap")
    bootstrap.add_argument("--version", default=BOOTSTRAP_VERSION)
    bootstrap.add_argument("--output-dir", type=Path, required=True)
    bootstrap.add_argument("--workspace", type=Path, default=Path.cwd())
    return parser.parse_args(arguments)


def main(arguments: list[str]) -> int:
    options = parse_args(arguments)
    try:
        if options.mode == "release":
            prepare_release(
                options.workspace,
                options.candidates_dir,
                options.version,
                options.source_revision,
                options.output_dir,
            )
        else:
            prepare_bootstrap(options.workspace, options.version, options.output_dir)
    except (OSError, PackagePreparationError, zipfile.BadZipFile) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


def prepare_release(
    workspace: Path,
    candidates_dir: Path,
    version: str,
    source_revision: str,
    output_dir: Path,
) -> None:
    validate_release_version(version)
    validate_source_revision(source_revision)
    workspace = validate_workspace(workspace)
    candidates_dir = validate_directory(candidates_dir, "candidate directory")
    create_output_dir(output_dir)

    published = []
    for target in TARGETS:
        package_dir = output_dir / target.directory_name
        package_dir.mkdir()
        archive = candidates_dir / f"rootlight-{version}-{target.triple}.zip"
        verify_archive_digest(archive)
        extract_candidate(archive, package_dir, target, version, source_revision)
        write_json_new(
            package_dir / "package.json",
            platform_package_json(target, version, source_revision),
        )
        published.append(publication_record(target.package_name, package_dir.name))

    root_dir = output_dir / "rootlight"
    root_dir.mkdir()
    write_root_package(
        workspace,
        root_dir,
        version,
        source_revision,
        include_runtime=True,
    )
    published.append(publication_record(ROOT_PACKAGE, root_dir.name))
    write_json_new(output_dir / "publish-order.json", published)


def prepare_bootstrap(workspace: Path, version: str, output_dir: Path) -> None:
    if version != BOOTSTRAP_VERSION or SEMVER_PATTERN.fullmatch(version) is None:
        raise PackagePreparationError("bootstrap version is invalid")
    workspace = validate_workspace(workspace)
    create_output_dir(output_dir)

    published = []
    for target in TARGETS:
        package_dir = output_dir / target.directory_name
        package_dir.mkdir()
        package = platform_package_json(target, version, None)
        package["description"] = "Rootlight npm trusted-publisher security bootstrap"
        write_json_new(package_dir / "package.json", package)
        write_text_new(
            package_dir / "SECURITY-BOOTSTRAP.txt",
            "This deprecated version exists only to establish npm trusted publishing.\n",
        )
        copy_regular(workspace / "LICENSE", package_dir / "LICENSE", 1024 * 1024)
        published.append(publication_record(target.package_name, package_dir.name))

    root_dir = output_dir / "rootlight"
    root_dir.mkdir()
    write_root_package(workspace, root_dir, version, None, include_runtime=False)
    published.append(publication_record(ROOT_PACKAGE, root_dir.name))
    write_json_new(output_dir / "publish-order.json", published)


def validate_workspace(path: Path) -> Path:
    resolved = validate_directory(path, "workspace")
    for relative in ("LICENSE", "packaging/npm/README.md", *NPM_RUNTIME_FILES):
        candidate = resolved / relative
        if not candidate.is_file() or candidate.is_symlink():
            raise PackagePreparationError("workspace npm packaging inputs are incomplete")
    return resolved


def validate_directory(path: Path, label: str) -> Path:
    resolved = path.resolve()
    if not resolved.is_dir() or resolved.is_symlink():
        raise PackagePreparationError(f"{label} must be a regular directory")
    return resolved


def create_output_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=False)


def validate_release_version(version: str) -> None:
    if RELEASE_VERSION_PATTERN.fullmatch(version) is None:
        raise PackagePreparationError("release version must be stable or alpha SemVer")


def validate_source_revision(source_revision: str) -> None:
    if SOURCE_REVISION_PATTERN.fullmatch(source_revision) is None:
        raise PackagePreparationError("source revision is invalid")


def verify_archive_digest(archive: Path) -> None:
    if not archive.is_file() or archive.is_symlink():
        raise PackagePreparationError("native candidate archive is missing")
    size = archive.stat().st_size
    if size <= 0 or size > MAX_ARCHIVE_BYTES:
        raise PackagePreparationError("native candidate archive size is invalid")
    sidecar = archive.with_name(f"{archive.name}.sha256")
    if not sidecar.is_file() or sidecar.is_symlink() or sidecar.stat().st_size > 512:
        raise PackagePreparationError("native candidate checksum is missing")
    expected_line = sidecar.read_text(encoding="ascii")
    match = re.fullmatch(r"([0-9a-f]{64})  ([^\r\n]+)\n", expected_line)
    if match is None or match.group(2) != archive.name:
        raise PackagePreparationError("native candidate checksum format is invalid")
    observed = hashlib.sha256(archive.read_bytes()).hexdigest()
    if observed != match.group(1):
        raise PackagePreparationError("native candidate checksum differs")


def extract_candidate(
    archive: Path,
    output_dir: Path,
    target: NativeTarget,
    version: str,
    source_revision: str,
) -> None:
    with zipfile.ZipFile(archive) as bundle:
        infos = bundle.infolist()
        if not infos or len(infos) > MAX_ARCHIVE_ENTRIES:
            raise PackagePreparationError("native candidate entry count is invalid")
        names = [validate_archive_entry(info) for info in infos]
        if len(names) != len(set(names)) or "package-manifest.json" not in names:
            raise PackagePreparationError("native candidate paths are invalid")
        executable_suffix = ".exe" if target.os_name == "win32" else ""
        if any(
            f"bin/{executable}{executable_suffix}" not in names
            for executable in PUBLIC_EXECUTABLES
        ):
            raise PackagePreparationError("native candidate public executable is missing")
        total = sum(info.file_size for info in infos)
        if total > MAX_EXTRACTED_BYTES:
            raise PackagePreparationError("native candidate expands beyond the package limit")

        manifest_info = infos[names.index("package-manifest.json")]
        manifest = json.loads(read_bounded(bundle, manifest_info, 1024 * 1024))
        expected_manifest = {
            "schema": "rootlight.package-manifest/1",
            "source_revision": source_revision,
            "target": target.triple,
            "version": version,
        }
        if {key: manifest.get(key) for key in expected_manifest} != expected_manifest:
            raise PackagePreparationError("native candidate manifest identity differs")

        for info, name in zip(infos, names, strict=True):
            destination = output_dir.joinpath(*PurePosixPath(name).parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            data = read_bounded(bundle, info, MAX_ARCHIVE_BYTES)
            with destination.open("xb") as output:
                output.write(data)
            mode = (info.external_attr >> 16) & 0o777
            if mode:
                destination.chmod(mode)


def validate_archive_entry(info: zipfile.ZipInfo) -> str:
    name = info.filename
    encoded = name.encode("utf-8")
    path = PurePosixPath(name)
    file_type = (info.external_attr >> 16) & 0o170000
    if (
        not name
        or len(encoded) > MAX_PATH_BYTES
        or "\\" in name
        or path.is_absolute()
        or ".." in path.parts
        or "." in path.parts
        or info.is_dir()
        or file_type == stat.S_IFLNK
        or any(ord(character) < 0x20 or ord(character) == 0x7F for character in name)
        or info.file_size < 0
        or info.file_size > MAX_ARCHIVE_BYTES
        or info.compress_size > MAX_ARCHIVE_BYTES
    ):
        raise PackagePreparationError("native candidate archive entry is invalid")
    return name


def read_bounded(
    bundle: zipfile.ZipFile, info: zipfile.ZipInfo, maximum: int
) -> bytes:
    if info.file_size > maximum:
        raise PackagePreparationError("native candidate entry is too large")
    with bundle.open(info) as source:
        data = source.read(maximum + 1)
    if len(data) != info.file_size or len(data) > maximum:
        raise PackagePreparationError("native candidate entry size differs")
    return data


def platform_package_json(
    target: NativeTarget, version: str, source_revision: str | None
) -> dict[str, object]:
    package: dict[str, object] = common_package_json(
        target.package_name,
        version,
        f"Native Rootlight distribution for {target.triple}",
        source_revision,
    )
    package["os"] = [target.os_name]
    package["cpu"] = [target.cpu]
    if target.libc is not None:
        package["libc"] = [target.libc]
    package["files"] = [
        "autostart",
        "bin",
        "launcher",
        "licenses",
        "LICENSE",
        "NOTICE",
        "package-manifest.json",
        "SECURITY-BOOTSTRAP.txt",
    ]
    return package


def write_root_package(
    workspace: Path,
    output_dir: Path,
    version: str,
    source_revision: str | None,
    *,
    include_runtime: bool,
) -> None:
    package = common_package_json(
        ROOT_PACKAGE,
        version,
        "Official native Rootlight CLI and MCP distribution",
        source_revision,
    )
    if include_runtime:
        package["bin"] = {
            "rootlight": "bin/rootlight.mjs",
            "rootlight-mcp": "bin/rootlight-mcp.mjs",
        }
        package["optionalDependencies"] = {
            target.package_name: version for target in TARGETS
        }
        package["files"] = ["bin", "LICENSE", "README.md"]
        bin_dir = output_dir / "bin"
        bin_dir.mkdir()
        for runtime_file in NPM_RUNTIME_FILES:
            source = workspace / runtime_file
            destination = bin_dir / source.name
            copy_regular(source, destination, 64 * 1024)
            destination.chmod(0o755)
    else:
        package["files"] = ["LICENSE", "README.md", "SECURITY-BOOTSTRAP.txt"]
        write_text_new(
            output_dir / "SECURITY-BOOTSTRAP.txt",
            "This deprecated version exists only to establish npm trusted publishing.\n",
        )
    write_json_new(output_dir / "package.json", package)
    copy_regular(workspace / "LICENSE", output_dir / "LICENSE", 1024 * 1024)
    copy_regular(
        workspace / "packaging/npm/README.md", output_dir / "README.md", 256 * 1024
    )


def common_package_json(
    name: str,
    version: str,
    description: str,
    source_revision: str | None,
) -> dict[str, object]:
    tag = f"v{version}"
    package: dict[str, object] = {
        "name": name,
        "version": version,
        "description": description,
        "license": "Apache-2.0",
        "type": "module",
        "engines": {"node": ">=22.14.0"},
        "repository": {
            "type": "git",
            "url": REPOSITORY_URL,
            "directory": "packaging/npm",
        },
        "homepage": f"https://github.com/tomasmarekk/rootlight/releases/tag/{tag}",
        "bugs": {"url": "https://github.com/tomasmarekk/rootlight/issues"},
        "publishConfig": {"access": "public", "provenance": True},
    }
    if source_revision is not None:
        package["gitHead"] = source_revision
    return package


def publication_record(name: str, directory: str) -> dict[str, str]:
    return {"name": name, "directory": directory}


def write_json_new(path: Path, value: object) -> None:
    encoded = json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
    write_text_new(path, encoded)


def write_text_new(path: Path, value: str) -> None:
    with path.open("x", encoding="utf-8", newline="\n") as output:
        output.write(value)


def copy_regular(source: Path, destination: Path, maximum: int) -> None:
    if not source.is_file() or source.is_symlink():
        raise PackagePreparationError("npm package source is not a regular file")
    size = source.stat().st_size
    if size > maximum:
        raise PackagePreparationError("npm package source is too large")
    with source.open("rb") as input_file, destination.open("xb") as output_file:
        remaining = maximum + 1
        while remaining > 0:
            chunk = input_file.read(min(64 * 1024, remaining))
            if not chunk:
                break
            output_file.write(chunk)
            remaining -= len(chunk)
        if remaining == 0 and input_file.read(1):
            raise PackagePreparationError("npm package source is too large")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
