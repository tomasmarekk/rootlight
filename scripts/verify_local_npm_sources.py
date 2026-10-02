#!/usr/bin/env python3
"""Authenticate a repository-local npm lock projection and declared fixture variants.

Original registry verification remains closed in verify_npm_source_bundle. Removed
upstream bytes are historical preparation evidence, never a current SRI proof.
"""

from __future__ import annotations

import argparse
import base64
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import re
import sys
import tarfile
from typing import Any

import patch_npm_source_fixture as fixture
import verify_npm_source_bundle as original


CANONICAL_LOCK = "apps/rootlight-web/frontend/package-lock.json"
UPSTREAM_LOCK = "source-inputs/npm/upstream-package-lock.json"
DISTRIBUTIONS_MANIFEST = "source-inputs/npm/local-distributions.json"
ORIGINAL_ARCHIVES = "source-inputs/npm/archives"
LOCAL_DISTRIBUTIONS = "source-inputs/npm/local-distributions"
_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_SHA512 = re.compile(r"[0-9a-f]{128}\Z")
_VARIANT_KEYS = {"name", "version", "archive_path", "archive_sha256", "provenance_path", "provenance_sha256"}
_PATCH_KEYS = {
    "member", "member_sha256_before", "member_sha256_after", "member_computed_sri_sha512_before",
    "member_computed_sri_sha512_after", "member_bytes_before", "member_bytes_after", "offset",
    "removed_length", "removed_sha256", "replacement_ascii", "replacement_length",
    "replacement_sha256", "matching_window_count",
}
_TRUE_PROVENANCE = {
    "locally_modified_distribution", "original_archive_unchanged", "original_locked_sri_verified",
    "patched_archive_self_integrity_verified", "synthetic_replacement_confirmed_by_caller",
    "other_members_bytes_and_metadata_preserved",
}
_FALSE_PROVENANCE = {
    "patched_archive_is_original_upstream_archive", "canonical_lock_updated", "locked_install_verified",
    "preferred_source_closure_complete", "frontend_source_complete", "os_blocked_network_verified",
    "provenance_signatures_verified",
}
_PROVENANCE_KEYS = {
    "schema", "lockfile_sha256", "original_pin", "archive_before", "archive_after", "patch",
    "archive_root", "package_json_sha256", "member_count", "member_inventory_before",
    "member_inventory_after", "member_metadata_before", "member_metadata_after",
    "unchanged_other_member_count", "license_files_preserved", "native_binary_files_preserved",
    "verification_scope", "source_scope", *_TRUE_PROVENANCE, *_FALSE_PROVENANCE,
}


class LocalBundleVerificationError(ValueError):
    """A local lock projection, retained archive, or declared provenance is invalid."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise LocalBundleVerificationError(message)


def _sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _sri(payload: bytes) -> str:
    return "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii")


def _json_bytes(document: Any) -> bytes:
    return json.dumps(document, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def _same_json(first: Any, second: Any) -> bool:
    # Python equality conflates JSON true, 1, and 1.0; lock metadata types matter.
    return _json_bytes(first) == _json_bytes(second)


def _hash(value: Any, pattern: re.Pattern[str] = _SHA256) -> bool:
    return isinstance(value, str) and bool(pattern.fullmatch(value))


def _number(value: Any, *, positive: bool = False) -> bool:
    return type(value) is int and value >= (1 if positive else 0)


def _local_path(value: Any, suffix: str) -> str:
    original._safe_path(value, "local distribution path")
    # npm parses file: as a URL; escapes/fragments must not alias another file.
    _require(value.startswith(LOCAL_DISTRIBUTIONS + "/") and value.endswith(suffix)
             and not any(character in value for character in ("%", "#")), "invalid local distribution path")
    return value


class _Inputs:
    def __init__(self, root: Path):
        self.root = root
        self.files: dict[str, tuple[str, os.stat_result]] = {}
        self.directories: dict[Path, tuple[set[str], dict[str, os.stat_result]]] = {}

    def read(self, name: str) -> bytes:
        payload, stamp = original._read_checked_file(self.root / name, "local npm input")
        _require(name not in self.files, "duplicate npm input path")
        self.files[name] = _sha256(payload), stamp
        return payload

    def directory(self, name: str, expected: set[str]) -> None:
        root = self.root / name
        allowed_directories = {""}
        for path in expected:
            parts = path.split("/")
            allowed_directories.update("/".join(parts[:end]) for end in range(1, len(parts)))
        pending = [(root, "")]
        observed = set()
        stamps = {}
        aliases: dict[str, str] = {}
        while pending:
            directory, relative = pending.pop()
            stamps[relative] = original._check_path(directory, directory=True, label="npm input directory")
            for entry in directory.iterdir():
                child = entry.name if not relative else relative + "/" + entry.name
                original._safe_path(child, "npm directory entry")
                original._path_aliases(child, aliases)
                if child in expected:
                    original._check_path(entry, directory=False, label="npm input file")
                    observed.add(child)
                elif child in allowed_directories:
                    pending.append((entry, child))
                else:
                    raise LocalBundleVerificationError("npm input directory has an undeclared entry")
        _require(observed == expected, "npm input directory is missing declared files")
        self.directories[root] = expected, stamps

    def unchanged(self) -> None:
        for root, (expected, stamps) in self.directories.items():
            for relative, stamp in stamps.items():
                current = original._check_path(root / relative, directory=True, label="npm input directory")
                _require(original._same_path_stamp(stamp, current), "npm input directory changed")
                entries = {entry.name for entry in (root / relative).iterdir()}
                wanted = {path[len(relative) + 1:].split("/", 1)[0] if relative else path.split("/", 1)[0]
                          for path in expected if not relative or path.startswith(relative + "/")}
                _require(entries == wanted, "npm input directory changed")
        for name, (digest, stamp) in self.files.items():
            payload, current = original._read_checked_file(self.root / name, "local npm input")
            _require(_sha256(payload) == digest and original._same_path_stamp(stamp, current), "npm input changed")


def _member_inventory(items: Any) -> dict[str, dict[str, Any]]:
    _require(isinstance(items, list), "invalid provenance member inventory")
    result = {}
    aliases: dict[str, str] = {}
    for item in items:
        _require(isinstance(item, dict), "invalid provenance member")
        kind = item.get("kind")
        keys = {"path", "kind", "bytes", "archive_mode"} | ({"sha256"} if kind == "file" else set())
        _require(kind in ("file", "directory") and set(item) == keys, "invalid provenance member fields")
        original._safe_path(item["path"], "provenance member path")
        original._path_aliases(item["path"], aliases)
        _require(item["path"] not in result and _number(item["bytes"]) and _number(item["archive_mode"]), "invalid provenance member identity")
        _require(_hash(item.get("sha256")) if kind == "file" else item["bytes"] == 0, "invalid provenance member digest")
        result[item["path"]] = item
    _require([item["path"] for item in items] == sorted(result), "unordered provenance member inventory")
    return result


def _before_archive(provenance: dict[str, Any], identity: dict[str, Any]) -> None:
    archive = provenance["archive_before"]
    _require(isinstance(archive, dict) and set(archive) == {"bytes", "sha256", "sha512", "sri_verified_algorithm", "computed_sri_sha512", "locked_integrity"}, "invalid upstream archive evidence")
    _require(_number(archive["bytes"], positive=True) and _hash(archive["sha256"]) and _hash(archive["sha512"], _SHA512), "invalid upstream archive hashes")
    sri = "sha512-" + base64.b64encode(bytes.fromhex(archive["sha512"])).decode("ascii")
    _require(archive["computed_sri_sha512"] == sri and archive["locked_integrity"] == identity["integrity"], "upstream archive pin evidence differs")
    hashes = original._integrity(identity["integrity"], "upstream integrity")
    strongest = max((algorithm for algorithm, _digest in hashes.values()), key=original._HASH_STRENGTH.__getitem__)
    _require(archive["sri_verified_algorithm"] == strongest, "upstream SRI evidence algorithm differs")
    if strongest in ("sha256", "sha512"):
        _require(any(algorithm == strongest and digest.hex() == archive[strongest] for algorithm, digest in hashes.values()), "upstream SRI evidence digest differs")


def _provenance(payload: bytes, document: Any, identity: dict[str, Any], after: dict[str, Any], upstream_hash: str) -> None:
    _require(isinstance(document, dict) and set(document) == _PROVENANCE_KEYS and document["schema"] == "rootlight.npm-local-fixture-patch/1", "invalid fixture provenance schema")
    _require(all(document[key] is True for key in _TRUE_PROVENANCE) and all(document[key] is False for key in _FALSE_PROVENANCE), "invalid fixture provenance claims")
    _require(document["lockfile_sha256"] == upstream_hash and _same_json(document["original_pin"], identity), "fixture provenance upstream pin differs")
    _require(document["verification_scope"] == "one exact caller-approved fixture window; all other logical tar members unchanged" and document["source_scope"] == "locally modified npm distribution only; no installation, preferred-source, or authorship claim", "invalid fixture provenance scope")
    _before_archive(document, identity)
    _require(_same_json(document["archive_after"], {**after["archive"], "computed_sri_sha512": _sri(payload), "filename": "patched.tgz"}), "patched archive evidence differs")
    _require(document["archive_before"]["sha256"] != after["archive"]["sha256"], "fixture archive did not change")
    for key, field in (("archive_root", "archive_root"), ("package_json_sha256", "package_json_sha256"), ("member_count", "member_count"), ("license_files_preserved", "license_files"), ("native_binary_files_preserved", "native_binary_files"), ("member_inventory_after", "members")):
        _require(_same_json(document[key], after[field]), "patched protected inventory differs")
    _require(_number(document["member_count"], positive=True) and _number(document["unchanged_other_member_count"]) and document["unchanged_other_member_count"] == after["member_count"] - 1, "invalid fixture preservation count")
    before_members = _member_inventory(document["member_inventory_before"])
    after_members = _member_inventory(document["member_inventory_after"])
    patch = document["patch"]
    _require(isinstance(patch, dict) and set(patch) == _PATCH_KEYS, "invalid fixture patch fields")
    for key in ("member_sha256_before", "member_sha256_after", "removed_sha256", "replacement_sha256"):
        _require(_hash(patch[key]), "invalid fixture patch digest")
    _require(all(_number(patch[key], positive=True) for key in ("removed_length", "replacement_length")) and all(_number(patch[key]) for key in ("offset", "member_bytes_before", "member_bytes_after")) and type(patch["matching_window_count"]) is int and patch["matching_window_count"] == 1, "invalid fixture patch coordinates")
    replacement = patch["replacement_ascii"]
    _require(isinstance(replacement, str) and replacement and all(32 <= ord(character) <= 126 for character in replacement), "invalid synthetic replacement")
    replacement_bytes = replacement.encode("ascii")
    _require(len(replacement_bytes) == patch["replacement_length"] and _sha256(replacement_bytes) == patch["replacement_sha256"] and patch["removed_sha256"] != patch["replacement_sha256"], "synthetic replacement evidence differs")
    original._safe_path(patch["member"], "fixture member")
    _require(patch["member"] in before_members and patch["member"] in after_members, "missing fixture member evidence")
    before_member, after_member = before_members[patch["member"]], after_members[patch["member"]]
    _require(before_member["kind"] == "file" and after_member["kind"] == "file" and before_member["sha256"] != after_member["sha256"], "fixture member did not change")
    for suffix, member in (("before", before_member), ("after", after_member)):
        _require(patch["member_sha256_" + suffix] == member["sha256"] and patch["member_bytes_" + suffix] == member["bytes"], "fixture member evidence differs")
        sri_hashes = original._integrity(patch["member_computed_sri_sha512_" + suffix], "fixture member SRI")
        _require(len(sri_hashes) == 1 and next(iter(sri_hashes.values()))[0] == "sha512", "invalid fixture member SRI")
    _require(patch["offset"] + patch["removed_length"] <= before_member["bytes"] and before_member["bytes"] - patch["removed_length"] + patch["replacement_length"] == after_member["bytes"], "fixture member size delta differs")
    before = {**after, "members": document["member_inventory_before"]}
    fixture._fixture_target(before, patch["member"])
    actual_metadata = fixture._metadata_inventory(payload)
    _require(_same_json(document["member_metadata_after"], actual_metadata), "patched tar metadata differs")
    metadata_before = document["member_metadata_before"]
    _require(isinstance(metadata_before, list) and len(metadata_before) == len(actual_metadata), "invalid original tar metadata evidence")
    for old, current in zip(metadata_before, actual_metadata):
        _require(isinstance(old, dict) and set(old) == set(current) and
                 all(type(old[key]) is type(current[key]) for key in current), "invalid original tar metadata types")
    fixture._preservation(before, after, metadata_before, actual_metadata, patch["member"], patch["replacement_length"] - patch["removed_length"])
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
        stream = archive.extractfile(patch["member"])
        _require(stream is not None, "fixture member is unreadable")
        with stream:
            contents = stream.read()
    _require(contents[patch["offset"]:patch["offset"] + patch["replacement_length"]] == replacement_bytes and _sri(contents) == patch["member_computed_sri_sha512_after"], "actual synthetic fixture window differs")


def _verify(source_root: Path) -> dict[str, Any]:
    _require(".." not in source_root.parts, "project root must not contain parent traversal")
    root = Path(os.path.abspath(source_root))
    original._check_path(root, directory=True, label="source root")
    inputs = _Inputs(root)
    upstream_bytes = inputs.read(UPSTREAM_LOCK)
    canonical_bytes = inputs.read(CANONICAL_LOCK)
    manifest_bytes = inputs.read(DISTRIBUTIONS_MANIFEST)
    upstream = original._parse_json(upstream_bytes, "upstream lock")
    identities = original._locked_archives(upstream)
    manifest = original._parse_json(manifest_bytes, "local distribution manifest")
    _require(isinstance(manifest, dict) and set(manifest) == {"schema", "upstream_lock_sha256", "variants"} and manifest["schema"] == "rootlight.npm-local-distributions/1", "invalid local distribution manifest")
    upstream_hash = _sha256(upstream_bytes)
    _require(manifest["upstream_lock_sha256"] == upstream_hash and isinstance(manifest["variants"], list), "local distribution manifest upstream pin differs")
    by_identity = {(identity["name"], identity["version"]): identity for identity in identities}
    variants = {}
    local_files: set[str] = set()
    aliases: dict[str, str] = {}
    for variant in manifest["variants"]:
        _require(isinstance(variant, dict) and set(variant) == _VARIANT_KEYS, "invalid local distribution fields")
        key = original._package_name(variant["name"], "variant name"), original._version(variant["version"], "variant version")
        _require(key in by_identity and key not in variants and _hash(variant["archive_sha256"]) and _hash(variant["provenance_sha256"]), "invalid or repeated local distribution identity")
        for field, suffix in (("archive_path", ".tgz"), ("provenance_path", ".json")):
            path = _local_path(variant[field], suffix)
            original._path_aliases(path, aliases)
            _require(path not in local_files, "repeated local distribution input")
            local_files.add(path)
        variants[key] = variant
    retained_originals = {identity["archive_filename"] for key, identity in by_identity.items() if key not in variants}
    inputs.directory(ORIGINAL_ARCHIVES, retained_originals)
    inputs.directory(LOCAL_DISTRIBUTIONS, {name[len(LOCAL_DISTRIBUTIONS) + 1:] for name in local_files})
    projection = copy.deepcopy(upstream)
    packages = []
    provenance_paths = []
    for identity in identities:
        key = identity["name"], identity["version"]
        variant = variants.get(key)
        path = variant["archive_path"] if variant else ORIGINAL_ARCHIVES + "/" + identity["archive_filename"]
        payload = inputs.read(path)
        current_identity = {**identity, "archive_filename": path.rsplit("/", 1)[-1], "resolved": "file:../../../" + path}
        if variant:
            _require(_sha256(payload) == variant["archive_sha256"], "local distribution archive digest differs")
            current_identity["integrity"] = _sri(payload)
        record = original._verify_archive(payload, current_identity)
        if variant:
            provenance_bytes = inputs.read(variant["provenance_path"])
            _require(_sha256(provenance_bytes) == variant["provenance_sha256"], "local distribution provenance digest differs")
            provenance = original._parse_json(provenance_bytes, "local distribution provenance")
            _provenance(payload, provenance, identity, record, upstream_hash)
            provenance_paths.append(variant["provenance_path"])
            record["fixture_patch"] = {field: provenance["patch"][field] for field in ("member", "offset", "removed_length", "removed_sha256", "replacement_length", "replacement_sha256")}
            record["provenance_path"] = variant["provenance_path"]
            record["provenance_sha256"] = variant["provenance_sha256"]
            record["original_sri_verified_during_preparation"] = True
        record.update(archive_path=path, upstream_pin=identity, distribution_kind="locally_modified_distribution" if variant else "original_registry_archive", upstream_original_sri_recomputed=variant is None)
        packages.append(record)
        for lock_path in identity["lock_paths"]:
            projection["packages"][lock_path]["resolved"] = current_identity["resolved"]
            if variant:
                projection["packages"][lock_path]["integrity"] = current_identity["integrity"]
    canonical = original._parse_json(canonical_bytes, "canonical lock")
    _require(_same_json(canonical, projection), "canonical lock differs from the exact local projection")
    inputs.unchanged()
    return {
        "schema": "rootlight.npm-local-archive-bundle/1",
        "lockfile_sha256": _sha256(canonical_bytes), "upstream_lockfile_sha256": upstream_hash,
        "local_distributions_sha256": _sha256(manifest_bytes),
        "lock_record_count": sum(len(record["lock_paths"]) for record in packages),
        "archive_count": len(packages), "original_archive_count": len(retained_originals),
        "local_distribution_count": len(variants), "archives_verified": True,
        "packages": packages, "archive_paths": sorted(record["archive_path"] for record in packages),
        "provenance_paths": sorted(provenance_paths),
        "input_files": [{"path": name, "sha256": digest} for name, (digest, _stamp) in sorted(inputs.files.items())],
        "upstream_original_sri_recomputed": not bool(variants),
        "installation_verified": False, "build_and_packaging_acceptance_verified": False,
        "os_denied_acceptance_verified": False, "preferred_source_closure_complete": False,
        "frontend_source_complete": False, "provenance_signatures_verified": False,
        "verification_scope": "exact local lock projection and every retained original or declared modified distribution",
        "historical_preparation_scope": "removed upstream archive SRI, before-member hashes and unique removed window are hash-bound preparation evidence, not current byte recomputation",
        "source_scope": "retained npm distributions only; preferred native, WASI and bundled transitive sources remain unproven",
        "licensing_scope": "supplied metadata and preserved files; not legal completeness or authorship evidence",
    }


def verify_local_bundle(source_root: Path) -> dict[str, Any]:
    """Verify fixed project inputs and return deterministic, root-path-free evidence.

    Args:
        source_root: Unlinked project directory containing both pinned locks,
            the declared variant manifest and exact retained archive directories.

    Returns:
        Current archive/member/SRI evidence, installer-relative input paths and
        explicit false installation, preferred-source and OS-denial claims.
        Absent upstream variant bytes are historical preparation evidence only.

    Raises:
        LocalBundleVerificationError: Unsafe, missing, extra, changed, malformed
            or inconsistent input. Diagnostics never relay arbitrary source text.

    Performs no writes, extraction, installation, source execution or network I/O.
    Input/stamp checks require stable ancestors, not hostile-concurrency isolation.
    """
    try:
        return _verify(Path(source_root))
    except (original.BundleVerificationError, fixture.FixturePatchError, OSError, UnicodeError, tarfile.TarError, EOFError, KeyError, TypeError) as error:
        del error
        raise LocalBundleVerificationError("local npm source input validation failed") from None


class _ArgumentParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        del message
        self.print_usage(sys.stderr)
        self.exit(2, "error: invalid local npm verification arguments\n")


def main(arguments: list[str] | None = None) -> int:
    """Print the report or exclusively create --output; never overwrite inputs.

    Accepts --project-root and optional --output. Argument errors exit 2; rejected
    inputs/output creation return 1 without source contents or exception chains.
    Report creation is the sole write; failed new outputs are not deleted.
    """
    parser = _ArgumentParser(description=__doc__)
    parser.add_argument("--project-root", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    options = parser.parse_args(arguments)
    try:
        report = verify_local_bundle(options.project_root)
        encoded = (json.dumps(report, sort_keys=True, indent=2, allow_nan=False) + "\n").encode("utf-8")
        if options.output is None:
            sys.stdout.write(encoded.decode("utf-8"))
        else:
            _require(".." not in options.output.parts, "output must not contain parent traversal")
            output = Path(os.path.abspath(options.output))
            root = Path(os.path.abspath(options.project_root))
            _require(not any(output.is_relative_to(root / directory) for directory in (ORIGINAL_ARCHIVES, LOCAL_DISTRIBUTIONS)), "output must be outside exact archive inventories")
            original._check_path(output.parent, directory=True, label="report output parent")
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
            descriptor = os.open(output, flags, 0o644)
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(encoded)
    except (LocalBundleVerificationError, original.BundleVerificationError, OSError, ValueError) as error:
        del error
        print("error: local npm source verification rejected", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
