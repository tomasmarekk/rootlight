#!/usr/bin/env python3
"""Authenticate the exact original npm archive closure without installing it.

Original registry archives preserve distribution evidence, not necessarily the
preferred sources of bundled JavaScript, native libraries, or WASI implementations.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import gzip
import hashlib
import io
import json
import math
import os
from pathlib import Path
import re
import stat
import sys
import tarfile
from typing import Any
import unicodedata
from urllib.parse import urlsplit
import zlib


REGISTRY_URL = "https://registry.npmjs.org/"
_NAME_PART = re.compile(r"[a-z0-9][a-z0-9._-]*\Z")
_VERSION = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z"
)
_HASH_STRENGTH = {"sha1": 1, "sha256": 2, "sha384": 3, "sha512": 4}
_LICENSE_NAME = re.compile(r"(?:licen[cs]es?|copying|notice)(?:[._-].*)?\Z", re.I)
_WINDOWS_DEVICES = {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"} | {
    f"{prefix}{number}"
    for prefix in ("COM", "LPT")
    for number in (*range(1, 10), "¹", "²", "³")
}
_NATIVE_SUFFIX = re.compile(r"\.(?:node|wasm|dll|dylib|a|lib|exe|o|obj|so(?:\.[0-9.]+)?)\Z", re.I)


class BundleVerificationError(ValueError):
    """A lockfile or original archive violates the authenticated bundle contract."""


def archive_filename(resolved: str) -> str:
    """Return SHA-256 of an approved registry URL, followed by ``.tgz``.

    This stable, package-independent name avoids scoped-name and version aliases.
    Raises BundleVerificationError for noncanonical or non-registry URLs; no I/O
    occurs. Package-specific URL identity is additionally checked by locked_archives.
    """
    if not isinstance(resolved, str):
        raise BundleVerificationError("resolved archive URL must be a string")
    try:
        parsed = urlsplit(resolved)
    except ValueError as error:
        raise BundleVerificationError("invalid resolved archive URL") from error
    if (
        not resolved.startswith(REGISTRY_URL)
        or parsed.scheme != "https"
        or parsed.netloc != "registry.npmjs.org"
        or resolved != f"https://registry.npmjs.org{parsed.path}"
        or parsed.query or parsed.fragment
        or any(character in resolved for character in ("%", "\\"))
        or any(ord(character) < 33 or ord(character) == 127 for character in resolved)
        or not parsed.path.endswith(".tgz")
    ):
        raise BundleVerificationError("unsupported resolved archive URL")
    _safe_path(parsed.path.removeprefix("/"), "resolved archive path")
    return hashlib.sha256(resolved.encode("utf-8")).hexdigest() + ".tgz"


def locked_archives(lock: Path) -> list[dict[str, Any]]:
    """Read every npm v3 registry record and return deduplicated archive identities.

    Required, dev, optional, platform-specific, and nested records are all retained.
    Identical URL/SRI identities share one archive but retain every lock path and
    selected supplied metadata. Local links, aliases, Git sources, missing pins,
    conflicting identities, and duplicate JSON keys raise BundleVerificationError.
    Performs no writes, dependency resolution, or network requests.
    """
    contents = _read_file(Path(lock), "lockfile")
    return _locked_archives(_parse_json(contents, "lockfile"))


def verify_bundle(lock: Path, archives: Path) -> dict[str, Any]:
    """Validate the exact original archive directory against an npm v3 lockfile.

    Args:
        lock: Canonical npm v3 package-lock.json with pinned registry records.
        archives: Directory containing only the archive_filename outputs for all
            non-root lock records, including optional packages on other platforms.

    Returns:
        Deterministic, JSON-ready lock/archive/member hashes, supplied license
        metadata, retained license files, and native binary-file observations.
        Each archive has one safe top-level directory, recorded as archive_root;
        member paths retain that authored root rather than being renamed.
        Preferred-source and full frontend closure claims are explicitly false.

    Raises:
        BundleVerificationError: Missing, extra, tampered, unsafe, linked, special,
            duplicate, or identity-mismatched input. No extraction, installation,
            source execution, downloads, or input writes occur.
    """
    lock = Path(lock)
    archives = Path(archives)
    lock_bytes, lock_stamp = _read_checked_file(lock, "lockfile")
    expected = _locked_archives(_parse_json(lock_bytes, "lockfile"))
    directory_stamp = _check_path(archives, directory=True, label="archive directory")
    try:
        observed = {entry.name: entry for entry in archives.iterdir()}
    except OSError as error:
        raise BundleVerificationError("archive directory is unreadable") from error
    filenames = {record["archive_filename"] for record in expected}
    missing = sorted(filenames - observed.keys())
    extra = sorted(observed.keys() - filenames)
    if missing or extra:
        detail = "; ".join(
            item for item in (
                "missing archives: " + ", ".join(missing) if missing else "",
                "extra archives: " + ", ".join(extra) if extra else "",
            ) if item
        )
        raise BundleVerificationError(detail)
    records = []
    archive_stamps = {}
    for identity in expected:
        filename = identity["archive_filename"]
        payload, archive_stamps[filename] = _read_checked_file(observed[filename], "original npm archive")
        records.append(_verify_archive(payload, identity))
    final_directory_stamp = _check_path(archives, directory=True, label="archive directory")
    try:
        final_names = {entry.name for entry in archives.iterdir()}
    except OSError as error:
        raise BundleVerificationError("archive directory is unreadable") from error
    if final_names != filenames or not _same_path_stamp(directory_stamp, final_directory_stamp):
        raise BundleVerificationError("archive directory changed while verifying")
    for filename, initial in archive_stamps.items():
        current = _check_path(archives / filename, directory=False, label="original npm archive")
        if not _same_path_stamp(initial, current):
            raise BundleVerificationError("original npm archive changed while verifying")
    if not _same_path_stamp(lock_stamp, _check_path(lock, directory=False, label="lockfile")):
        raise BundleVerificationError("lockfile changed while verifying")
    return {
        "schema": "rootlight.npm-original-archive-bundle/1",
        "lockfile_sha256": hashlib.sha256(lock_bytes).hexdigest(),
        "lock_record_count": sum(len(record["lock_paths"]) for record in records),
        "archive_count": len(records),
        "archives_verified": True,
        "packages": records,
        "preferred_source_closure_complete": False,
        "frontend_source_complete": False,
        "provenance_signatures_verified": False,
        "verification_scope": "all non-root registry records in the supplied npm v3 lockfile",
        "source_scope": "original npm distributions; preferred native, WASI, and bundled transitive sources remain unproven",
        "licensing_scope": "supplied metadata and present files; not a legal completeness or authorship assessment",
    }


def _locked_archives(document: Any) -> list[dict[str, Any]]:
    if (
        not isinstance(document, dict)
        or type(document.get("lockfileVersion")) is not int
        or document["lockfileVersion"] != 3
        or not isinstance(document.get("packages"), dict)
        or not isinstance(document["packages"].get(""), dict)
    ):
        raise BundleVerificationError("unsupported lockfile format: expected npm v3 packages")
    root = document["packages"][""]
    _package_name(root.get("name"), "root package name")
    _version(root.get("version"), "root package version")
    if root["name"] != document.get("name") or root["version"] != document.get("version"):
        raise BundleVerificationError("root package identity differs from lockfile identity")
    identities: dict[tuple[str, str], dict[str, Any]] = {}
    urls: dict[str, tuple[str, str]] = {}
    path_aliases: dict[str, str] = {}
    for path, package in sorted(document["packages"].items()):
        if path == "":
            continue
        _safe_path(path, "lock package path")
        _path_aliases(path, path_aliases)
        name = _name_from_lock_path(path)
        if not isinstance(package, dict) or any(key in package for key in ("link", "inBundle")):
            raise BundleVerificationError(f"{path}: unsupported linked or bundled package")
        version = _version(package.get("version"), f"{path}: version")
        if "name" in package and package["name"] != name:
            raise BundleVerificationError(f"{path}: declared name differs from lock path")
        resolved = package.get("resolved")
        filename = archive_filename(resolved)
        expected_url = f"{REGISTRY_URL}{name}/-/{name.rsplit('/', 1)[-1]}-{version}.tgz"
        if resolved != expected_url:
            raise BundleVerificationError(f"{path}: resolved URL differs from pinned package identity")
        hashes = _integrity(package.get("integrity"), f"{path}: integrity")
        integrity = " ".join(sorted(hashes))
        key = (name, version)
        record = {
            "name": name,
            "version": version,
            "resolved": resolved,
            "integrity": integrity,
            "archive_filename": filename,
            "lock_paths": [],
            "lock_records": [],
        }
        if resolved in urls and urls[resolved] != key:
            raise BundleVerificationError(f"{path}: conflicting resolved archive identities")
        urls[resolved] = key
        if key in identities:
            previous = identities[key]
            if (previous["resolved"], previous["integrity"]) != (resolved, integrity):
                raise BundleVerificationError(f"{path}: conflicting package archive identities")
            record = previous
        else:
            identities[key] = record
        metadata: dict[str, Any] = {"path": path}
        for field in ("dev", "optional", "devOptional", "hasInstallScript"):
            if field in package:
                if type(package[field]) is not bool:
                    raise BundleVerificationError(f"{path}: invalid {field} metadata")
                metadata[field] = package[field]
        for field in ("license", "os", "cpu", "libc"):
            if field in package:
                metadata[field] = package[field]
        record["lock_paths"].append(path)
        record["lock_records"].append(metadata)
    return [identities[key] for key in sorted(identities)]


def _package_name(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise BundleVerificationError(f"{label}: invalid package name")
    parts = value.split("/")
    if value.startswith("@"):
        valid = len(parts) == 2 and bool(_NAME_PART.fullmatch(parts[0][1:])) and bool(_NAME_PART.fullmatch(parts[1]))
    else:
        valid = len(parts) == 1 and bool(_NAME_PART.fullmatch(value))
    if not valid:
        raise BundleVerificationError(f"{label}: invalid package name")
    return value


def _version(value: Any, label: str) -> str:
    if not isinstance(value, str) or not _VERSION.fullmatch(value):
        raise BundleVerificationError(f"{label}: expected an exact version")
    return value


def _name_from_lock_path(path: str) -> str:
    parts = path.split("/")
    position = 0
    name = ""
    while position < len(parts):
        if parts[position] != "node_modules" or position + 1 >= len(parts):
            raise BundleVerificationError(f"{path}: unsupported package path")
        position += 1
        name = parts[position]
        position += 1
        if name.startswith("@"):
            if position >= len(parts):
                raise BundleVerificationError(f"{path}: incomplete scoped package path")
            name += "/" + parts[position]
            position += 1
        _package_name(name, f"{path}: package name")
    return name


def _integrity(value: Any, label: str) -> dict[str, tuple[str, bytes]]:
    if not isinstance(value, str) or not value.strip():
        raise BundleVerificationError(f"{label}: missing locked SRI")
    result = {}
    for token in value.split():
        algorithm, separator, encoded = token.partition("-")
        if not separator or algorithm not in _HASH_STRENGTH:
            raise BundleVerificationError(f"{label}: unsupported SRI algorithm or syntax")
        try:
            digest = base64.b64decode(encoded, validate=True)
        except (ValueError, binascii.Error) as error:
            raise BundleVerificationError(f"{label}: invalid SRI digest") from error
        if (
            len(digest) != hashlib.new(algorithm).digest_size
            or base64.b64encode(digest).decode("ascii") != encoded
        ):
            raise BundleVerificationError(f"{label}: invalid SRI digest length or encoding")
        result[token] = (algorithm, digest)
    if max(_HASH_STRENGTH[item[0]] for item in result.values()) < _HASH_STRENGTH["sha256"]:
        raise BundleVerificationError(f"{label}: SHA-1-only SRI cannot authenticate source archives")
    return result


def _verify_archive(payload: bytes, identity: dict[str, Any]) -> dict[str, Any]:
    hashes = _integrity(identity["integrity"], "archive integrity")
    strongest = max((item[0] for item in hashes.values()), key=_HASH_STRENGTH.__getitem__)
    actual = hashlib.new(strongest, payload).digest()
    # SSRI selects the strongest declared algorithm; a weaker match cannot rescue it.
    if not any(algorithm == strongest and digest == actual for algorithm, digest in hashes.values()):
        raise BundleVerificationError(f"{identity['name']}: archive differs from locked SRI")
    members = []
    names: dict[str, str] = {}
    aliases: dict[str, str] = {}
    manifest = None
    archive_root = None
    native = []
    try:
        # Reading the complete gzip stream authenticates CRCs and concatenated frames,
        # even when a tar reader would stop at its first end-of-archive block.
        tar_bytes = gzip.decompress(payload)
        with tarfile.open(fileobj=io.BytesIO(tar_bytes), mode="r:") as archive:
            for member in archive:
                path = member.name.removesuffix("/") if member.isdir() else member.name
                _safe_path(path, "archive member")
                _path_aliases(path, aliases)
                # npm strips one directory layer; its name is not necessarily
                # package: https://docs.npmjs.com/cli/v11/commands/npm-install/
                root = path.split("/", 1)[0]
                if archive_root is None:
                    archive_root = root
                elif root != archive_root:
                    raise BundleVerificationError("archive members have multiple top-level roots")
                manifest_path = f"{archive_root}/package.json"
                if path in names:
                    raise BundleVerificationError(f"duplicate archive member: {path}")
                if member.isdir():
                    kind = "directory"
                    allowed_mode = 0o7777 | stat.S_IFDIR
                    if member.size != 0:
                        raise BundleVerificationError(f"nonempty archive directory: {path}")
                elif member.type in (tarfile.REGTYPE, tarfile.AREGTYPE) and member.sparse is None:
                    kind = "file"
                    allowed_mode = 0o7777 | stat.S_IFREG
                    if path == archive_root or member.size < 0:
                        raise BundleVerificationError(f"invalid archive file: {path}")
                else:
                    raise BundleVerificationError(f"linked or special archive member: {path}")
                if member.mode < 0 or member.mode & ~allowed_mode:
                    raise BundleVerificationError(f"invalid archive member mode: {path}")
                names[path] = kind
                record: dict[str, Any] = {"path": path, "kind": kind, "bytes": member.size, "archive_mode": member.mode}
                if kind == "file":
                    stream = archive.extractfile(member)
                    if stream is None:
                        raise BundleVerificationError(f"unreadable archive member: {path}")
                    with stream:
                        digest = hashlib.sha256()
                        length = 0
                        chunks = []
                        header = b""
                        while chunk := stream.read(1024 * 1024):
                            if not header:
                                header = chunk[:8]
                            digest.update(chunk)
                            length += len(chunk)
                            if path == manifest_path:
                                chunks.append(chunk)
                    if length != member.size:
                        raise BundleVerificationError(f"truncated archive member: {path}")
                    record["sha256"] = digest.hexdigest()
                    formats = _native_formats(path, header)
                    if formats:
                        native.append({**record, "detected_formats": formats})
                    if path == manifest_path:
                        manifest = _parse_json(b"".join(chunks), "archive package.json")
                members.append(record)
            remainder = tar_bytes[archive.offset:]
            if len(remainder) < 2 * tarfile.BLOCKSIZE or any(remainder):
                raise BundleVerificationError("nonzero or missing tar end-of-archive padding")
    except (tarfile.TarError, OSError, EOFError, ValueError, zlib.error) as error:
        if isinstance(error, BundleVerificationError):
            raise
        raise BundleVerificationError(f"{identity['name']}: invalid gzip tar archive") from error
    for path in names:
        parts = path.split("/")
        if any(names.get("/".join(parts[:end])) == "file" for end in range(1, len(parts))):
            raise BundleVerificationError(f"archive file is an ancestor of another member: {path}")
    if not isinstance(manifest, dict) or (manifest.get("name"), manifest.get("version")) != (identity["name"], identity["version"]):
        raise BundleVerificationError(f"{identity['name']}: package.json identity differs from lock path/version")
    members.sort(key=lambda record: record["path"])
    return {
        **identity,
        "archive": {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest(), "sha512": hashlib.sha512(payload).hexdigest(), "sri_verified_algorithm": strongest},
        "archive_root": archive_root,
        "package_json_sha256": next(record["sha256"] for record in members if record["path"] == f"{archive_root}/package.json"),
        "member_count": len(members),
        "members": members,
        "members_sha256": hashlib.sha256(json.dumps(members, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest(),
        "license_metadata": {field: manifest[field] for field in ("license", "licenses") if field in manifest},
        "provenance_metadata": {field: manifest[field] for field in ("repository", "homepage", "gitHead") if field in manifest},
        "license_files": [record for record in members if record["kind"] == "file" and any(_LICENSE_NAME.fullmatch(part) for part in record["path"].split("/"))],
        "native_binary_files": sorted(native, key=lambda record: record["path"]),
    }


def _native_formats(path: str, header: bytes) -> list[str]:
    formats = []
    if _NATIVE_SUFFIX.search(path):
        formats.append("native-file-extension")
    for signature, name in ((b"\x00asm", "wasm"), (b"\x7fELF", "elf"), (b"MZ", "pe"), (b"!<arch>\n", "static-archive")):
        if header.startswith(signature):
            formats.append(name)
    if header[:4] in (b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca"):
        formats.append("mach-o-or-fat-binary")
    return formats


def _safe_path(value: Any, label: str) -> None:
    if (
        not isinstance(value, str) or not value
        or any(character in value for character in '<>:"\\|?*')
        or any(ord(character) < 32 or ord(character) == 127 or 0xD800 <= ord(character) <= 0xDFFF for character in value)
        or any(part in ("", ".", "..") or part.endswith((".", " ")) or part.split(".")[0].upper() in _WINDOWS_DEVICES for part in value.split("/"))
    ):
        raise BundleVerificationError(f"{label}: unsafe portable path {value!r}")


def _path_aliases(path: str, aliases: dict[str, str]) -> None:
    parts = path.split("/")
    for end in range(1, len(parts) + 1):
        prefix = "/".join(parts[:end])
        folded = unicodedata.normalize("NFC", prefix).casefold()
        previous = aliases.setdefault(folded, prefix)
        if previous != prefix:
            raise BundleVerificationError(f"case or Unicode aliased path: {path}")


def _is_link(metadata: os.stat_result) -> bool:
    return stat.S_ISLNK(metadata.st_mode) or bool(getattr(metadata, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0))


def _check_path(path: Path, *, directory: bool, label: str) -> os.stat_result:
    absolute = Path(os.path.abspath(path))
    try:
        for ancestor in reversed(absolute.parents):
            metadata = ancestor.lstat()
            if _is_link(metadata) or not stat.S_ISDIR(metadata.st_mode):
                raise BundleVerificationError(f"{label}: linked or non-directory ancestor")
        metadata = absolute.lstat()
        correct_type = stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode)
        if _is_link(metadata) or not correct_type:
            raise BundleVerificationError(f"{label}: linked or incorrect input type")
        return metadata
    except OSError as error:
        raise BundleVerificationError(f"{label}: missing or unreadable input") from error


def _read_file(path: Path, label: str) -> bytes:
    return _read_checked_file(path, label)[0]


def _read_checked_file(path: Path, label: str) -> tuple[bytes, os.stat_result]:
    before = _check_path(path, directory=False, label=label)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            if not stat.S_ISREG(opened.st_mode) or _is_link(opened) or _file_stamp(opened) != _file_stamp(before):
                raise BundleVerificationError(f"{label}: input changed while opening")
            chunks = []
            while chunk := stream.read(1024 * 1024):
                chunks.append(chunk)
            after = os.fstat(stream.fileno())
        current = _check_path(path, directory=False, label=label)
    except OSError as error:
        raise BundleVerificationError(f"{label}: unreadable input") from error
    if any(not stat.S_ISREG(item.st_mode) or _is_link(item) or _file_stamp(item) != _file_stamp(before) for item in (opened, after, current)):
        raise BundleVerificationError(f"{label}: input changed while reading")
    if not _stable_change_times(before, opened, after, current, windows=os.name == "nt"):
        raise BundleVerificationError(f"{label}: input changed while reading")
    contents = b"".join(chunks)
    if len(contents) != before.st_size:
        raise BundleVerificationError(f"{label}: truncated input")
    return contents, current


def _file_stamp(metadata: os.stat_result) -> tuple[int, int, int, int]:
    return (metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns)


def _same_path_stamp(first: os.stat_result, second: os.stat_result) -> bool:
    if stat.S_ISDIR(first.st_mode) and stat.S_ISDIR(second.st_mode):
        # Windows may expose a directory allocation size as 0 or 4096 after
        # enumeration; identity/timestamps/mode and callers' exact names remain strict.
        return ((first.st_dev, first.st_ino, first.st_mode, first.st_mtime_ns, first.st_ctime_ns)
                == (second.st_dev, second.st_ino, second.st_mode, second.st_mtime_ns, second.st_ctime_ns))
    return _file_stamp(first) == _file_stamp(second) and first.st_ctime_ns == second.st_ctime_ns


def _stable_change_times(
    before: os.stat_result,
    opened: os.stat_result,
    after: os.stat_result,
    current: os.stat_result,
    *,
    windows: bool,
) -> bool:
    # Windows lstat/fstat expose different ctime meanings; retain same-API guards.
    return (
        before.st_ctime_ns == current.st_ctime_ns
        and opened.st_ctime_ns == after.st_ctime_ns
        and (windows or before.st_ctime_ns == opened.st_ctime_ns)
    )


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result = {}
    for key, value in pairs:
        if key in result:
            raise BundleVerificationError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def _parse_json(contents: bytes, label: str) -> Any:
    try:
        return json.loads(
            contents.decode("utf-8"),
            object_pairs_hook=_unique_object,
            parse_constant=_reject_json_constant,
            parse_float=_finite_json_float,
        )
    except (ValueError, UnicodeError) as error:
        raise BundleVerificationError(f"{label}: invalid JSON: {error}") from error


def _reject_json_constant(value: str) -> None:
    raise BundleVerificationError(f"non-finite JSON number: {value}")


def _finite_json_float(value: str) -> float:
    number = float(value)
    if not math.isfinite(number):
        raise BundleVerificationError(f"non-finite JSON number: {value}")
    return number


def main(arguments: list[str] | None = None) -> int:
    """Print deterministic evidence or exclusively create --output; return 1 on error.

    Inputs remain read-only. Output creation is the CLI's sole write; no existing
    file is overwritten. Argument errors use argparse's usual exit status.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lock", type=Path, required=True)
    parser.add_argument("--archives", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    options = parser.parse_args(arguments)
    try:
        evidence = verify_bundle(options.lock, options.archives)
        encoded = json.dumps(evidence, indent=2, sort_keys=True, allow_nan=False) + "\n"
        if options.output is None:
            print(encoded, end="")
        else:
            output_path = Path(os.path.abspath(options.output))
            archive_path = Path(os.path.abspath(options.archives))
            if output_path.is_relative_to(archive_path):
                raise BundleVerificationError("evidence output must be outside the exact archive directory")
            _check_path(options.output.parent, directory=True, label="output parent")
            with options.output.open("x", encoding="utf-8", newline="\n") as output:
                output.write(encoded)
    except (BundleVerificationError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
