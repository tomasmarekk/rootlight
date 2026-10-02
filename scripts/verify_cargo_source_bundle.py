#!/usr/bin/env python3
"""Verify the exact crates.io source closure of a Cargo vendor directory.

This offline check binds local source files to lockfile package identities; its
license evidence records supplied metadata, not legal completeness or authorship.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import sys
import tarfile
import tomllib
from typing import Any


CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
_CHECKSUM_FILE = ".cargo-checksum.json"
_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_NAME = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_-]*\Z")
_VERSION = re.compile(
    r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\Z"
)
_LICENSE_NAME = re.compile(r"(?:licen[cs]es?|copying|notice)(?:[._-].*)?\Z", re.I)
_WINDOWS_DEVICES = {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"} | {
    f"{prefix}{number}" for prefix in ("COM", "LPT") for number in range(1, 10)
}


class BundleVerificationError(ValueError):
    """A lockfile or local source input violates the exact bundle contract."""


def verify_bundle(
    vendor: Path, locks: list[Path], *, archives: Path | None = None
) -> dict[str, Any]:
    """Validate a read-only vendor tree against the union of supplied lockfiles.

    Args:
        vendor: A Cargo vendor directory containing only package directories.
        locks: Nonempty, distinct Cargo lockfile paths. Overlap is allowed only
            when crates.io package checksums agree.
        archives: Optional directory containing the exact original .crate
            archives. When supplied, verify their lockfile hashes and bind every
            regular archive member to the vendor bytes, retaining archive modes.

    Returns:
        Deterministic JSON-ready hashes, package identities, and supplied license
        metadata. Local/path packages are counted but their sources are not
        verified by this registry-source check. No absolute paths are emitted.

    Raises:
        BundleVerificationError: An input is malformed, unsafe, missing, extra,
            or differs from the lock/checksum inventories. Performs no writes,
            dependency resolution, downloads, or project execution.
    """
    lock_paths, labels = _lock_paths(locks)
    expected: dict[tuple[str, str], str] = {}
    local_packages: set[tuple[str, str]] = set()
    lock_records = []
    for lock_path, label in zip(lock_paths, labels):
        contents, file_record = _read_file(lock_path, f"lockfile {label}", collect=True)
        document = _parse_toml(contents, f"lockfile {label}")
        packages = document.get("package")
        if document.get("version") not in (3, 4) or not isinstance(packages, list):
            raise BundleVerificationError(f"lockfile {label} has an unsupported format")
        seen = set()
        registry_count = local_count = 0
        for package in packages:
            identity = _identity(package, f"lockfile {label}")
            source = package.get("source")
            if source is not None and source != CRATES_IO_SOURCE:
                raise BundleVerificationError(
                    f"lockfile {label}: unsupported source for {_display(identity)}"
                )
            key = (*identity, source)
            if key in seen:
                raise BundleVerificationError(
                    f"lockfile {label}: duplicate package {_display(identity)}"
                )
            seen.add(key)
            if source is None:
                if "checksum" in package:
                    raise BundleVerificationError(
                        f"lockfile {label}: local package has a checksum: {_display(identity)}"
                    )
                local_packages.add(identity)
                local_count += 1
                continue
            checksum = package.get("checksum")
            _require_digest(checksum, f"lockfile {label}: {_display(identity)} checksum")
            if identity in expected and expected[identity] != checksum:
                raise BundleVerificationError(
                    f"conflicting lock checksums for {_display(identity)}"
                )
            expected[identity] = checksum
            registry_count += 1
        lock_records.append(
            {
                "path": label,
                "sha256": file_record["sha256"],
                "registry_package_count": registry_count,
                "local_package_count": local_count,
            }
        )

    _require_directory(vendor, "vendor root")
    observed = {}
    for directory in _entries(vendor, "vendor root"):
        _safe_relative_path(directory.name, "vendor package directory")
        _require_directory(directory, f"vendor package {directory.name}")
        manifest_bytes, manifest_record = _read_file(
            directory / "Cargo.toml", f"{directory.name}/Cargo.toml", collect=True
        )
        manifest = _parse_toml(manifest_bytes, f"{directory.name}/Cargo.toml")
        identity = _identity(manifest.get("package"), f"{directory.name}/Cargo.toml")
        if identity in observed:
            raise BundleVerificationError(f"duplicate vendor package {_display(identity)}")
        if identity not in expected:
            raise BundleVerificationError(f"extra vendor package {_display(identity)}")
        observed[identity] = _verify_package(
            directory, manifest["package"], expected[identity], manifest_record["sha256"]
        )
    missing = sorted(set(expected) - set(observed))
    if missing:
        raise BundleVerificationError(
            "missing vendor packages: " + ", ".join(map(_display, missing))
        )
    package_records = [observed[identity] for identity in sorted(observed)]
    if archives is not None:
        _verify_archives(archives, package_records)
    return {
        "schema": "rootlight.cargo-source-bundle/1",
        "lockfiles": sorted(lock_records, key=lambda record: record["path"]),
        "package_count": len(package_records),
        "packages": package_records,
        "archives_verified": archives is not None,
        "local_package_count": len(local_packages),
        "local_packages": [
            {"name": name, "version": version} for name, version in sorted(local_packages)
        ],
        "licensing_scope": "supplied metadata and present files; not a completeness or authorship assessment",
    }


def _verify_package(
    directory: Path, package: dict[str, Any], checksum: str, manifest_digest: str
) -> dict[str, Any]:
    label = directory.name
    checksum_bytes, checksum_record = _read_file(
        directory / _CHECKSUM_FILE, f"{label}/{_CHECKSUM_FILE}", collect=True
    )
    try:
        checksums = json.loads(checksum_bytes, object_pairs_hook=_unique_json_object)
    except (ValueError, UnicodeError) as error:
        raise BundleVerificationError(f"{label}: invalid checksum manifest: {error}") from error
    if not isinstance(checksums, dict) or set(checksums) != {"files", "package"}:
        raise BundleVerificationError(f"{label}: checksum manifest must contain files and package")
    if checksums["package"] != checksum:
        raise BundleVerificationError(f"{label}: package checksum differs from lockfiles")
    expected_files = checksums["files"]
    if not isinstance(expected_files, dict) or "Cargo.toml" not in expected_files:
        raise BundleVerificationError(f"{label}: checksum file inventory is invalid")
    for path, digest in expected_files.items():
        _safe_relative_path(path, f"{label}: checksum path")
        if path == _CHECKSUM_FILE:
            raise BundleVerificationError(f"{label}: checksum manifest cannot inventory itself")
        _require_digest(digest, f"{label}/{path}: file checksum")
    observed = _collect_files(directory)
    # Cargo adds this manifest after unpacking; no other file is exempt from hashing.
    observed_checksum = observed.pop(_CHECKSUM_FILE, None)
    if (
        observed_checksum != checksum_record
        or observed.get("Cargo.toml", {}).get("sha256") != manifest_digest
    ):
        raise BundleVerificationError(f"{label}: manifest changed during verification")
    missing = sorted(set(expected_files) - set(observed))
    extra = sorted(set(observed) - set(expected_files))
    if missing or extra:
        detail = "; ".join(
            text for text in (
                "missing files: " + ", ".join(missing) if missing else "",
                "extra files: " + ", ".join(extra) if extra else "",
            ) if text
        )
        raise BundleVerificationError(f"{label}: {detail}")
    for path, digest in expected_files.items():
        if observed[path]["sha256"] != digest:
            raise BundleVerificationError(f"{label}/{path}: source file checksum differs")
    license_expression = package.get("license")
    license_file = package.get("license-file")
    for key, value in (("license", license_expression), ("license-file", license_file)):
        if value is not None and (not isinstance(value, str) or not value.strip()):
            raise BundleVerificationError(f"{label}: invalid package {key} metadata")
    if license_file is not None:
        _safe_relative_path(license_file, f"{label}: license-file")
        if license_file not in observed:
            raise BundleVerificationError(f"{label}: declared license-file is missing")
    file_records = [{"path": path, **observed[path]} for path in sorted(observed)]
    license_records = [
        record for record in file_records
        if record["path"] == license_file
        or any(_LICENSE_NAME.fullmatch(part) for part in record["path"].split("/"))
    ]
    return {
        "name": package["name"],
        "version": package["version"],
        "source": CRATES_IO_SOURCE,
        "package_checksum": checksum,
        "vendor_path": label,
        "cargo_toml_sha256": observed["Cargo.toml"]["sha256"],
        "checksum_manifest_sha256": checksum_record["sha256"],
        "file_count": len(file_records),
        "files": file_records,
        "source_files_sha256": hashlib.sha256(
            json.dumps(file_records, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ).hexdigest(),
        "license": license_expression,
        "license_file": license_file,
        "license_files": license_records,
    }


def _verify_archives(archives: Path, packages: list[dict[str, Any]]) -> None:
    _require_directory(archives, "archive root")
    expected = {f"{package['name']}-{package['version']}.crate": package for package in packages}
    entries = {path.name: path for path in _entries(archives, "archive root")}
    missing = sorted(set(expected) - set(entries))
    extra = sorted(set(entries) - set(expected))
    if missing or extra:
        detail = "; ".join(
            text for text in (
                "missing archives: " + ", ".join(missing) if missing else "",
                "extra archives: " + ", ".join(extra) if extra else "",
            ) if text
        )
        raise BundleVerificationError(detail)
    for filename, package in sorted(expected.items()):
        contents, record = _read_file(entries[filename], f"archive {filename}", collect=True)
        if record["sha256"] != package["package_checksum"]:
            raise BundleVerificationError(f"archive {filename}: checksum differs from lockfiles")
        members = _archive_files(contents, filename, filename.removesuffix(".crate"))
        vendor_files = {record["path"]: record for record in package["files"]}
        if set(members) != set(vendor_files):
            raise BundleVerificationError(f"archive {filename}: file set differs from vendor package")
        for path, member in sorted(members.items()):
            if any(member[key] != vendor_files[path][key] for key in ("bytes", "sha256")):
                raise BundleVerificationError(f"archive {filename}/{path}: content differs from vendor")
            # Archive modes remain authoritative across Windows and Unix hosts.
            vendor_files[path]["archive_mode"] = member["mode"]
        package["archive"] = {"path": filename, **record}
        package["archive_executable_files"] = sorted(
            path for path, member in members.items() if member["mode"] & 0o111
        )


def _archive_files(contents: bytes, filename: str, prefix: str) -> dict[str, dict[str, Any]]:
    files = {}
    seen = set()
    try:
        with tarfile.open(fileobj=io.BytesIO(contents), mode="r:*") as archive:
            for member in archive:
                path = member.name.removesuffix("/") if member.isdir() else member.name
                _safe_relative_path(path, f"archive {filename}: member")
                if path in seen:
                    raise BundleVerificationError(f"archive {filename}: duplicate member {path}")
                seen.add(path)
                if path != prefix and not path.startswith(prefix + "/"):
                    raise BundleVerificationError(f"archive {filename}: member is outside package prefix")
                if not member.isdir() and member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE):
                    raise BundleVerificationError(f"archive {filename}: nonregular member {path}")
                # Older published crates include stat type bits in the mode field;
                # only the type declared by TarInfo is valid alongside permissions.
                allowed_type = stat.S_IFDIR if member.isdir() else stat.S_IFREG
                if member.mode < 0 or member.mode & ~(0o7777 | allowed_type):
                    raise BundleVerificationError(f"archive {filename}: invalid member mode {path}")
                if member.isdir():
                    continue
                relative = path.removeprefix(prefix + "/")
                if relative == prefix:
                    raise BundleVerificationError(f"archive {filename}: package prefix is not a directory")
                stream = archive.extractfile(member)
                if stream is None:
                    raise BundleVerificationError(f"archive {filename}: unreadable member {path}")
                digest = hashlib.sha256()
                size = 0
                with stream:
                    while chunk := stream.read(1024 * 1024):
                        digest.update(chunk)
                        size += len(chunk)
                if size != member.size:
                    raise BundleVerificationError(f"archive {filename}: truncated member {path}")
                files[relative] = {"bytes": size, "sha256": digest.hexdigest(), "mode": member.mode}
    except (tarfile.TarError, OSError, EOFError) as error:
        raise BundleVerificationError(f"archive {filename}: invalid tar archive") from error
    return files


def _lock_paths(locks: list[Path]) -> tuple[list[Path], list[str]]:
    if not locks:
        raise BundleVerificationError("at least one lockfile is required")
    paths = [Path(os.path.abspath(path)) for path in locks]
    if len(set(paths)) != len(paths):
        raise BundleVerificationError("duplicate lockfile input")
    try:
        parent = Path(os.path.commonpath([path.parent for path in paths]))
    except ValueError as error:
        raise BundleVerificationError("lockfiles must share a filesystem root") from error
    labels = [path.relative_to(parent).as_posix() for path in paths]
    for label in labels:
        _safe_relative_path(label, "lockfile label")
    return paths, labels


def _identity(package: Any, label: str) -> tuple[str, str]:
    if (
        not isinstance(package, dict)
        or not isinstance(package.get("name"), str)
        or not _NAME.fullmatch(package["name"])
        or not isinstance(package.get("version"), str)
        or not _VERSION.fullmatch(package["version"])
    ):
        raise BundleVerificationError(f"{label}: invalid package name or version")
    return package["name"], package["version"]


def _display(identity: tuple[str, str]) -> str:
    return "@".join(identity)


def _safe_relative_path(value: Any, label: str) -> None:
    if (
        not isinstance(value, str) or not value or "\\" in value or ":" in value
        or any(ord(character) < 32 or ord(character) == 127 for character in value)
        or any(
            part in ("", ".", "..") or part.endswith((".", " "))
            or part.split(".")[0].upper() in _WINDOWS_DEVICES
            for part in value.split("/")
        )
    ):
        raise BundleVerificationError(f"{label}: unsafe relative path {value!r}")


def _require_digest(value: Any, label: str) -> None:
    if not isinstance(value, str) or not _SHA256.fullmatch(value):
        raise BundleVerificationError(f"{label}: expected lowercase SHA-256")


def _is_link(metadata: os.stat_result) -> bool:
    return stat.S_ISLNK(metadata.st_mode) or bool(
        getattr(metadata, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    )


def _require_directory(path: Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise BundleVerificationError(f"{label}: directory is missing or unreadable") from error
    if _is_link(metadata) or not stat.S_ISDIR(metadata.st_mode):
        raise BundleVerificationError(f"{label}: expected a non-link directory")


def _entries(directory: Path, label: str) -> list[Path]:
    try:
        return sorted(directory.iterdir(), key=lambda path: path.name)
    except OSError as error:
        raise BundleVerificationError(f"{label}: cannot read directory") from error


def _collect_files(root: Path) -> dict[str, dict[str, Any]]:
    pending = [root]
    files = {}
    while pending:
        directory = pending.pop()
        for path in _entries(directory, root.name):
            relative = path.relative_to(root).as_posix()
            _safe_relative_path(relative, f"{root.name}: source path")
            try:
                metadata = path.lstat()
            except OSError as error:
                raise BundleVerificationError(f"{root.name}/{relative}: cannot inspect input") from error
            if _is_link(metadata):
                raise BundleVerificationError(f"{root.name}/{relative}: links are prohibited")
            if stat.S_ISDIR(metadata.st_mode):
                pending.append(path)
            else:
                _, files[relative] = _read_file(path, f"{root.name}/{relative}")
    return files


def _read_file(path: Path, label: str, *, collect: bool = False) -> tuple[bytes, dict[str, Any]]:
    try:
        before = path.lstat()
        if _is_link(before) or not stat.S_ISREG(before.st_mode):
            raise BundleVerificationError(f"{label}: expected a non-link regular file")
        digest = hashlib.sha256()
        chunks = []
        size = 0
        with path.open("rb") as stream:
            opened = os.fstat(stream.fileno())
            if (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino):
                raise BundleVerificationError(f"{label}: input changed while opening")
            while chunk := stream.read(1024 * 1024):
                digest.update(chunk)
                size += len(chunk)
                if collect:
                    chunks.append(chunk)
            after = os.fstat(stream.fileno())
        current = path.lstat()
        if _is_link(current) or any(
            (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns)
            != (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
            for item in (opened, after, current)
        ) or size != before.st_size:
            raise BundleVerificationError(f"{label}: input changed while reading")
        return b"".join(chunks), {"bytes": size, "sha256": digest.hexdigest()}
    except OSError as error:
        raise BundleVerificationError(f"{label}: file is missing or unreadable") from error


def _parse_toml(contents: bytes, label: str) -> dict[str, Any]:
    try:
        return tomllib.loads(contents.decode("utf-8"))
    except (UnicodeError, tomllib.TOMLDecodeError) as error:
        raise BundleVerificationError(f"{label}: invalid TOML") from error


def _unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result = {}
    for key, value in pairs:
        if key in result:
            raise BundleVerificationError(f"duplicate checksum key {key!r}")
        result[key] = value
    return result


def main(arguments: list[str] | None = None) -> int:
    """Verify CLI inputs and print or write JSON evidence; return nonzero on failure."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vendor", type=Path, required=True)
    parser.add_argument("--lock", type=Path, action="append", required=True)
    parser.add_argument("--archives", type=Path)
    parser.add_argument("--output", type=Path)
    options = parser.parse_args(arguments)
    try:
        evidence = verify_bundle(options.vendor, options.lock, archives=options.archives)
        encoded = json.dumps(evidence, indent=2, sort_keys=True) + "\n"
        if options.output is None:
            print(encoded, end="")
        else:
            with options.output.open("x", encoding="utf-8", newline="\n") as output:
                output.write(encoded)
    except (BundleVerificationError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
