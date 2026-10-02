#!/usr/bin/env python3
"""Replace one digest-identified synthetic fixture window in a local npm archive.

The upstream lock and archive remain immutable. The output is a locally modified
distribution, not locked-install, preferred-source, or network-isolation evidence.
"""

from __future__ import annotations

import argparse
import base64
import copy
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import sys
import tarfile
from typing import Any

import verify_npm_source_bundle as verifier


_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_FIXTURE_DIRECTORIES = {"test", "tests", "__tests__", "fixture", "fixtures"}


class FixturePatchError(ValueError):
    """An input, patch assertion, or fresh-output condition failed closed."""


class _RedactedArgumentParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        # argparse otherwise repeats arbitrary invalid option values verbatim.
        del message
        self.print_usage(sys.stderr)
        self.exit(2, "error: invalid fixture patch arguments\n")


def patch_fixture_archive(
    lock: Path,
    archives: Path,
    package_name: str,
    version: str,
    member_path: str,
    expected_member_sha256: str,
    removed_length: int,
    removed_sha256: str,
    replacement: bytes,
    output: Path,
    *,
    synthetic_replacement_confirmed: bool,
    offset: int | None = None,
) -> dict[str, Any]:
    """Verify and exclusively publish a deterministic, redacted fixture patch.

    ``replacement`` must be caller-confirmed synthetic, nonempty printable ASCII.
    The removed bytes are never an input or an evidence field: a whole-member
    digest and exactly one length-sized digest window identify them. An optional
    offset must agree with that unique window; it cannot bypass ambiguity checks.

    ``output`` must already be an empty, unlinked directory outside the original
    lock/archive inputs. No files are extracted, installed, executed, downloaded,
    deleted, or overwritten. Errors raise FixturePatchError (or OSError). A failed
    publication may leave a newly created partial output; it is never accepted or
    cleaned up automatically. Path/stamp checks do not claim hostile-concurrent-
    ancestor protection or OS-blocked-network acceptance.
    """
    if synthetic_replacement_confirmed is not True:
        raise FixturePatchError("replacement requires explicit synthetic-content confirmation")
    if not isinstance(replacement, bytes) or not replacement or any(byte < 32 or byte > 126 for byte in replacement):
        raise FixturePatchError("replacement must be nonempty printable ASCII bytes")
    for digest in (expected_member_sha256, removed_sha256):
        if not isinstance(digest, str) or not _SHA256.fullmatch(digest):
            raise FixturePatchError("patch digests must be lowercase SHA-256 hex")
    if type(removed_length) is not int or removed_length <= 0:
        raise FixturePatchError("removed length must be a positive integer")
    if offset is not None and (type(offset) is not int or offset < 0):
        raise FixturePatchError("offset must be a nonnegative integer")

    lock, archives, output = (_absolute_path(path) for path in (lock, archives, output))
    try:
        verifier._safe_path(member_path, "fixture member")
        verifier._package_name(package_name, "selected package")
        verifier._version(version, "selected version")
        output_stamp = _empty_output(output, lock, archives)
        lock_bytes, lock_stamp = verifier._read_checked_file(lock, "original lockfile")
        identities = verifier._locked_archives(verifier._parse_json(lock_bytes, "original lockfile"))
        matches = [item for item in identities if (item["name"], item["version"]) == (package_name, version)]
        if len(matches) != 1:
            raise FixturePatchError("selected package/version is not uniquely present in the original lock")
        identity = matches[0]
        directory_stamp = verifier._check_path(archives, directory=True, label="original archive directory")
        archive_path = archives / identity["archive_filename"]
        original, archive_stamp = verifier._read_checked_file(archive_path, "original npm archive")
        before = verifier._verify_archive(original, identity)
        target = next((item for item in before["members"] if item["path"] == member_path), None)
        if target is None or target["kind"] != "file":
            raise FixturePatchError("exact fixture member is missing or is not a regular file")
        if target["sha256"] != expected_member_sha256:
            raise FixturePatchError("fixture member differs from its expected whole-member SHA-256")
        _fixture_target(before, member_path)
        patched, actual_offset, metadata_before, member_sri_before, member_sri_after = _repack(
            original, member_path, removed_length, removed_sha256, replacement, offset
        )
        patched_sri = _sri(patched)
        after = verifier._verify_archive(patched, {**identity, "integrity": patched_sri})
        metadata_after = _metadata_inventory(patched)
        _preservation(before, after, metadata_before, metadata_after, member_path, len(replacement) - removed_length)
        if after["archive"]["sha256"] == before["archive"]["sha256"]:
            raise FixturePatchError("patch must actually change the fixture archive")
        evidence = {
            "schema": "rootlight.npm-local-fixture-patch/1",
            "locally_modified_distribution": True,
            "original_archive_unchanged": True,
            "original_locked_sri_verified": True,
            "patched_archive_self_integrity_verified": True,
            "patched_archive_is_original_upstream_archive": False,
            "canonical_lock_updated": False,
            "locked_install_verified": False,
            "preferred_source_closure_complete": False,
            "frontend_source_complete": False,
            "os_blocked_network_verified": False,
            "provenance_signatures_verified": False,
            "synthetic_replacement_confirmed_by_caller": True,
            "lockfile_sha256": _sha256(lock_bytes),
            "original_pin": identity,
            "archive_before": {**before["archive"], "computed_sri_sha512": _sri(original), "locked_integrity": identity["integrity"]},
            "archive_after": {**after["archive"], "computed_sri_sha512": patched_sri, "filename": "patched.tgz"},
            "patch": {
                "member": member_path,
                "member_sha256_before": target["sha256"],
                "member_sha256_after": next(item["sha256"] for item in after["members"] if item["path"] == member_path),
                "member_computed_sri_sha512_before": member_sri_before,
                "member_computed_sri_sha512_after": member_sri_after,
                "member_bytes_before": target["bytes"],
                "member_bytes_after": target["bytes"] - removed_length + len(replacement),
                "offset": actual_offset,
                "removed_length": removed_length,
                "removed_sha256": removed_sha256,
                "replacement_ascii": replacement.decode("ascii"),
                "replacement_length": len(replacement),
                "replacement_sha256": _sha256(replacement),
                "matching_window_count": 1,
            },
            "archive_root": before["archive_root"],
            "package_json_sha256": before["package_json_sha256"],
            "member_count": before["member_count"],
            "member_inventory_before": before["members"],
            "member_inventory_after": after["members"],
            "member_metadata_before": metadata_before,
            "member_metadata_after": metadata_after,
            "unchanged_other_member_count": before["member_count"] - 1,
            "other_members_bytes_and_metadata_preserved": True,
            "license_files_preserved": before["license_files"],
            "native_binary_files_preserved": before["native_binary_files"],
            "verification_scope": "one exact caller-approved fixture window; all other logical tar members unchanged",
            "source_scope": "locally modified npm distribution only; no installation, preferred-source, or authorship claim",
        }
        encoded = (json.dumps(evidence, indent=2, sort_keys=True, allow_nan=False) + "\n").encode("utf-8")
        _stable_inputs(lock, lock_bytes, lock_stamp, archives, directory_stamp, archive_path, original, archive_stamp)
        current_output = _empty_output(output, lock, archives)
        if not verifier._same_path_stamp(output_stamp, current_output):
            raise FixturePatchError("output directory changed while preparing patch")
        _exclusive_write(output / "patched.tgz", patched)
        _exclusive_write(output / "patch-provenance.json", encoded)
        _stable_inputs(lock, lock_bytes, lock_stamp, archives, directory_stamp, archive_path, original, archive_stamp)
        for path, expected in ((output / "patched.tgz", patched), (output / "patch-provenance.json", encoded)):
            if verifier._read_checked_file(path, "published patch output")[0] != expected:
                raise FixturePatchError("published patch output changed")
        if {entry.name for entry in output.iterdir()} != {"patched.tgz", "patch-provenance.json"}:
            raise FixturePatchError("fresh output directory gained unrelated entries")
        return evidence
    except verifier.BundleVerificationError as error:
        # Verifier JSON errors can contain source text; never relay their details.
        raise FixturePatchError("original or patched input violates npm archive verification") from error


def _absolute_path(path: Path) -> Path:
    path = Path(path)
    if ".." in path.parts:
        raise FixturePatchError("input and output paths must not contain parent traversal")
    return Path(os.path.abspath(path))


def _empty_output(output: Path, lock: Path, archives: Path) -> os.stat_result:
    if output.is_relative_to(archives) or archives.is_relative_to(output) or lock.is_relative_to(output):
        raise FixturePatchError("output must be outside and separate from original inputs")
    stamp = verifier._check_path(output, directory=True, label="fresh output directory")
    if any(output.iterdir()):
        raise FixturePatchError("output directory must already exist and be empty")
    return stamp


def _fixture_target(record: dict[str, Any], path: str) -> None:
    parts = path.split("/")
    if not any(part.casefold() in _FIXTURE_DIRECTORIES for part in parts[1:-1]):
        raise FixturePatchError("target must be inside an authored test or fixture directory")
    protected = {item["path"] for item in record["license_files"] + record["native_binary_files"]}
    if path == f"{record['archive_root']}/package.json" or path in protected:
        raise FixturePatchError("manifest, license, and native runtime members cannot be patched")


def _sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _sri(payload: bytes) -> str:
    return "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii")


def _window(contents: bytes, length: int, digest: str, offset: int | None) -> int:
    match = None
    for start in range(len(contents) - length + 1):
        if _sha256(contents[start:start + length]) == digest:
            if match is not None:
                raise FixturePatchError("removed-window digest has multiple matching locations")
            match = start
    if match is None:
        raise FixturePatchError("removed-window digest has no matching location")
    if offset is not None and offset != match:
        raise FixturePatchError("explicit offset differs from the unique verified window")
    return match


def _member_metadata(member: tarfile.TarInfo) -> dict[str, Any]:
    # Hash free-form tar metadata rather than copying opaque source strings.
    return {
        "path": member.name,
        "type_hex": member.type.hex(),
        "mode": member.mode,
        "bytes": member.size,
        "uid": member.uid,
        "gid": member.gid,
        "mtime": member.mtime,
        "uname_sha256": _sha256(member.uname.encode("utf-8", "surrogatepass")),
        "gname_sha256": _sha256(member.gname.encode("utf-8", "surrogatepass")),
        "linkname_sha256": _sha256(member.linkname.encode("utf-8", "surrogatepass")),
        "pax_headers_sha256": _sha256(json.dumps(member.pax_headers, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")),
        "pax_non_size_headers_sha256": _sha256(json.dumps({key: value for key, value in member.pax_headers.items() if key != "size"}, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")),
        "pax_size": int(member.pax_headers["size"]) if "size" in member.pax_headers else None,
    }


def _metadata_inventory(payload: bytes) -> list[dict[str, Any]]:
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
        return [_member_metadata(member) for member in archive]


def _repack(
    original: bytes, path: str, length: int, digest: str, replacement: bytes, offset: int | None
) -> tuple[bytes, int, list[dict[str, Any]], str, str]:
    tar_output = io.BytesIO()
    metadata = []
    found_offset = None
    member_sri_before = None
    member_sri_after = None
    with tarfile.open(fileobj=io.BytesIO(original), mode="r:gz") as source:
        with tarfile.open(fileobj=tar_output, mode="w:", format=tarfile.PAX_FORMAT) as destination:
            for member in source:
                metadata.append(_member_metadata(member))
                info = copy.copy(member)
                info.pax_headers = dict(member.pax_headers)
                data = None
                if member.isfile():
                    stream = source.extractfile(member)
                    if stream is None:
                        raise FixturePatchError("verified regular archive member is unreadable")
                    with stream:
                        data = stream.read()
                    if member.name == path:
                        member_sri_before = _sri(data)
                        found_offset = _window(data, length, digest, offset)
                        if data[found_offset:found_offset + length] == replacement:
                            raise FixturePatchError("replacement must differ from the verified window")
                        data = data[:found_offset] + replacement + data[found_offset + length:]
                        member_sri_after = _sri(data)
                        info.size = len(data)
                        if "size" in info.pax_headers:
                            info.pax_headers["size"] = str(info.size)
                destination.addfile(info, io.BytesIO(data) if data is not None else None)
    if found_offset is None or member_sri_before is None or member_sri_after is None:
        raise FixturePatchError("verified fixture member was not repacked")
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", filename="", mtime=0, compresslevel=9) as stream:
        stream.write(tar_output.getvalue())
    return compressed.getvalue(), found_offset, metadata, member_sri_before, member_sri_after


def _preservation(
    before: dict[str, Any], after: dict[str, Any], metadata_before: list[dict[str, Any]],
    metadata_after: list[dict[str, Any]], path: str, size_delta: int,
) -> None:
    for field in ("archive_root", "package_json_sha256", "member_count", "license_metadata", "provenance_metadata", "license_files", "native_binary_files"):
        if before[field] != after[field]:
            raise FixturePatchError("protected archive identity or license/runtime inventory changed")
    expected_members = [dict(item) for item in before["members"]]
    for item in expected_members:
        if item["path"] == path:
            item["bytes"] += size_delta
            item["sha256"] = next(member["sha256"] for member in after["members"] if member["path"] == path)
    if expected_members != after["members"]:
        raise FixturePatchError("unapproved archive member content, mode, path, or type changed")
    if len(metadata_before) != len(metadata_after):
        raise FixturePatchError("archive member metadata count changed")
    for original, patched in zip(metadata_before, metadata_after):
        expected = dict(original)
        if expected["path"] == path:
            expected["bytes"] += size_delta
            if size_delta and expected["pax_size"] is not None:
                # A PAX size override, if present, is the target's size metadata.
                expected["pax_size"] += size_delta
                expected["pax_headers_sha256"] = patched["pax_headers_sha256"]
        if expected != patched:
            raise FixturePatchError("unapproved archive member metadata or order changed")


def _stable_inputs(
    lock: Path, lock_bytes: bytes, lock_stamp: os.stat_result,
    archives: Path, directory_stamp: os.stat_result,
    archive_path: Path, archive_bytes: bytes, archive_stamp: os.stat_result,
) -> None:
    if not verifier._same_path_stamp(directory_stamp, verifier._check_path(archives, directory=True, label="original archive directory")):
        raise FixturePatchError("original archive directory changed")
    for path, contents, stamp, label in (
        (lock, lock_bytes, lock_stamp, "original lockfile"),
        (archive_path, archive_bytes, archive_stamp, "original npm archive"),
    ):
        current, current_stamp = verifier._read_checked_file(path, label)
        if contents != current or not verifier._same_path_stamp(stamp, current_stamp):
            raise FixturePatchError("original patch input changed")


def _exclusive_write(path: Path, contents: bytes) -> None:
    verifier._check_path(path.parent, directory=True, label="patch output directory")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(contents)
        stream.flush()
        os.fsync(stream.fileno())


def main(arguments: list[str] | None = None) -> int:
    """Publish two new output files; print only a generic completion or error."""
    parser = _RedactedArgumentParser(description=__doc__)
    parser.add_argument("--lock", type=Path, required=True)
    parser.add_argument("--archives", type=Path, required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--member", required=True)
    parser.add_argument("--member-sha256", required=True)
    parser.add_argument("--removed-length", type=int, required=True)
    parser.add_argument("--removed-sha256", required=True)
    parser.add_argument("--replacement-ascii", required=True)
    parser.add_argument("--confirm-synthetic-replacement", action="store_true", required=True)
    parser.add_argument("--offset", type=int)
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args(arguments)
    try:
        replacement = options.replacement_ascii.encode("ascii")
        patch_fixture_archive(
            options.lock, options.archives, options.package, options.version,
            options.member, options.member_sha256, options.removed_length,
            options.removed_sha256, replacement, options.output,
            synthetic_replacement_confirmed=options.confirm_synthetic_replacement,
            offset=options.offset,
        )
    except (FixturePatchError, OSError, ValueError, tarfile.TarError, EOFError) as error:
        # No source excerpts, unknown contents, or exception chain on the CLI.
        del error
        print("error: fixture patch rejected; no verified patch result", file=sys.stderr)
        return 1
    print("Locally modified fixture archive and redacted provenance created.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
