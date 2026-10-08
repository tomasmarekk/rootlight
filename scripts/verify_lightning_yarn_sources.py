#!/usr/bin/env python3
"""Rederive retained Lightning producer Yarn distributions without installation.

This namespace is independent of consumer npm-v3 policy and the authored producer
tree. Published distributions and expanded bytes do not prove preferred sources.
"""

from __future__ import annotations

import argparse
import base64
import gzip
import hashlib
import io
import os
from pathlib import Path, PurePosixPath
import stat
import sys
import tarfile
import zlib

import lightning_yarn_pins as yarn
import verify_lightning_producer_sources as producer
import verify_npm_source_bundle as original


PRODUCER_PATH = f"source-inputs/producers/{producer.ROOT}"
BUNDLE_PATH = f"source-inputs/producer-dependencies/{producer.ROOT}/yarn-originals"
REPORT_NAME = "yarn-source-closure.json"
MAX_COMPRESSED = 128 * 1024 * 1024
MAX_EXPANDED = 256 * 1024 * 1024
MAX_TOTAL_EXPANDED = 2 * 1024 * 1024 * 1024
MAX_MEMBERS = 20000
MAX_TOTAL_MEMBERS = 200000
MAX_MANIFEST = 1024 * 1024
MAX_REPORT = producer.MAX_JSON
FALSE_CLAIMS = (
    "consumer_install_policy_verified", "installation_verified",
    "producer_transitive_sources_complete", "preferred_source_closure_complete",
    "native_producers_reviewed", "source_to_binary_rebuild_verified",
    "build_and_packaging_acceptance_verified", "frontend_source_complete",
    "os_denied_acceptance_verified", "provenance_signatures_verified",
    "license_completeness_verified", "package_code_executed",
)


class YarnSourceVerificationError(ValueError):
    """Retained producer dependencies violate a source-free verification contract."""


def _require(condition: bool, category: str) -> None:
    if not condition:
        raise YarnSourceVerificationError(category)


def _record(payload: bytes) -> dict:
    return {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}


def _mode(path: Path, expected: int) -> None:
    if os.name != "nt":
        _require(stat.S_IMODE(path.lstat().st_mode) == expected, "file_execute_intent_differs")


def _bounded_members(payload: bytes, expanded_left: int, members_left: int) -> tuple[int, list[dict]]:
    _require(0 < len(payload) <= MAX_COMPRESSED, "compressed_byte_limit")
    parts, total = [], 0
    maximum = min(MAX_EXPANDED, expanded_left)
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(payload), mode="rb") as compressed:
            while part := compressed.read(min(65536, maximum - total + 1)):
                total += len(part)
                _require(total <= maximum, "expanded_byte_limit")
                parts.append(part)
        expanded = b"".join(parts)
        members, paths, aliases = [], {}, {}
        with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as archive:
            for member in archive:
                _require(len(members) < min(MAX_MEMBERS, members_left), "member_count_limit")
                path = member.name.removesuffix("/") if member.isdir() else member.name
                original._safe_path(path, "Yarn member")
                original._path_aliases(path, aliases)
                _require(path not in paths and len(path.encode("utf-8")) <= 4096,
                         "duplicate_or_long_member")
                _require(0 <= member.mode <= 0o7777 and member.sparse is None,
                         "unsupported_member_metadata")
                if member.isdir():
                    _require(member.size == 0, "nonempty_directory")
                    record = {"path": path, "kind": "directory", "bytes": 0,
                              "archive_mode": member.mode}
                else:
                    _require(member.type in (tarfile.REGTYPE, tarfile.AREGTYPE)
                             and 0 <= member.size <= MAX_EXPANDED, "linked_or_special_member")
                    if path.split("/", 1)[-1] == "package.json":
                        _require(member.size <= MAX_MANIFEST, "manifest_byte_limit")
                    stream = archive.extractfile(member)
                    _require(stream is not None, "unreadable_member")
                    hasher, count = hashlib.sha256(), 0
                    with stream:
                        while part := stream.read(65536):
                            count += len(part)
                            hasher.update(part)
                    _require(count == member.size, "truncated_member")
                    record = {"path": path, "kind": "file", "bytes": count,
                              "archive_mode": member.mode, "sha256": hasher.hexdigest()}
                paths[path] = record["kind"]
                members.append(record)
            remainder = expanded[archive.offset:]
            _require(len(remainder) >= 2 * tarfile.BLOCKSIZE and not any(remainder),
                     "nonzero_or_missing_tar_padding")
        for path in paths:
            _require(all(paths.get(str(parent)) != "file" for parent in PurePosixPath(path).parents),
                     "file_is_member_ancestor")
        return total, sorted(members, key=lambda record: record["path"])
    except (OSError, EOFError, tarfile.TarError, zlib.error) as error:
        raise YarnSourceVerificationError("invalid_compressed_archive") from error


def _authenticate(payload: bytes, pin: dict, expanded_left: int, members_left: int) -> tuple[int, dict]:
    _require("sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii")
             == pin["integrity"] and hashlib.sha1(payload).hexdigest() == pin["url_fragment_sha1"],
             "original_archive_pin_mismatch")
    expanded_bytes, members = _bounded_members(payload, expanded_left, members_left)
    # Preflight bounds the second decompression in the established archive verifier.
    proof = original._verify_archive(payload, pin)
    _require(proof["members"] == members, "canonical_member_projection_differs")
    return expanded_bytes, proof


def _walk_error(error: OSError) -> None:
    raise YarnSourceVerificationError("namespace_traversal_failed") from error


def _check_inventory(bundle: Path, files: dict, directories: set[str]) -> None:
    seen_files, seen_directories = set(), set()
    aliases = {}
    for current, dirs, names in os.walk(bundle, followlinks=False, onerror=_walk_error):
        current = producer.safe_path(Path(current), directory=True)
        for name in dirs:
            child = producer.safe_path(current / name, directory=True)
            relative = child.relative_to(bundle).as_posix()
            original._safe_path(relative, "stored Yarn directory")
            original._path_aliases(relative, aliases)
            _require(relative in directories, "extra_stored_directory")
            seen_directories.add(relative)
        for name in names:
            child = producer.safe_path(current / name, directory=False)
            relative = child.relative_to(bundle).as_posix()
            original._safe_path(relative, "stored Yarn file")
            original._path_aliases(relative, aliases)
            _require(relative in files, "extra_stored_file")
            record = files[relative]
            payload = producer.read_bytes(child, record["bytes"])
            _require(_record(payload) == {key: record[key] for key in ("bytes", "sha256")},
                     "stored_file_bytes_differ")
            _mode(child, record["mode"])
            seen_files.add(relative)
    _require(seen_files == set(files) and seen_directories == directories,
             "stored_namespace_inventory_differs")


def _inputs(project: Path) -> tuple[dict, bytes, bytes, bytes]:
    bundle = project / PRODUCER_PATH
    evidence = producer.verify_bundle(project, bundle)
    manifest = producer.read_bytes(bundle / "tree/package.json", yarn.MAX_MANIFEST_BYTES)
    lock = producer.read_bytes(bundle / "tree/yarn.lock", yarn.MAX_LOCK_BYTES)
    source_report = producer.read_bytes(bundle / "source-tree.json", MAX_REPORT)
    return evidence, manifest, lock, source_report


def _derive(project: Path, bundle: Path, *, require_report: bool) -> dict:
    project = producer.safe_path(project, directory=True)
    bundle = producer.safe_path(bundle, directory=True)
    names = {entry.name for entry in bundle.iterdir()}
    expected_names = {"archives", "packages"}
    if require_report:
        expected_names.add(REPORT_NAME)
    _require(names in (expected_names, expected_names | {REPORT_NAME}), "namespace_root_inventory_differs")
    producer.safe_path(bundle / "archives", directory=True)
    producer.safe_path(bundle / "packages", directory=True)
    evidence, manifest, lock, source_report = _inputs(project)
    graph = yarn.parse_yarn_lock(lock, manifest)
    pins = graph["packages"]
    filenames = {pin["archive_filename"] for pin in pins}
    _require({entry.name for entry in (bundle / "archives").iterdir()} == filenames,
             "archive_inventory_differs")
    stems = {Path(name).stem for name in filenames}
    _require({entry.name for entry in (bundle / "packages").iterdir()} == stems,
             "package_inventory_differs")
    files, directories, packages = {}, {"archives", "packages"}, []
    expanded_total = member_total = 0
    for pin in pins:
        archive_path = "archives/" + pin["archive_filename"]
        payload = producer.read_bytes(bundle / archive_path, MAX_COMPRESSED)
        expanded_bytes, proof = _authenticate(payload, pin, MAX_TOTAL_EXPANDED - expanded_total,
                                             MAX_TOTAL_MEMBERS - member_total)
        expanded_total += expanded_bytes
        member_total += proof["member_count"]
        files[archive_path] = {**_record(payload), "mode": 0o644}
        expanded_path = "packages/" + Path(pin["archive_filename"]).stem
        directories.add(expanded_path)
        for member in proof["members"]:
            path = expanded_path + "/" + member["path"]
            directories.update(str(parent) for parent in PurePosixPath(path).parents if str(parent) != ".")
            if member["kind"] == "directory":
                directories.add(path)
            else:
                files[path] = {"bytes": member["bytes"], "sha256": member["sha256"],
                               "mode": 0o755 if member["archive_mode"] & 0o111 else 0o644}
        packages.append({"pin": pin, "archive_path": archive_path, "expanded_path": expanded_path,
                         "published_package": proof})
    report = {
        "schema": "rootlight.lightning-yarn-source-closure/1",
        "producer": {"path": PRODUCER_PATH, "git_head": producer.COMMIT,
                     "association": evidence["association"], "source_tree_report": _record(source_report)},
        "inputs": {f"{PRODUCER_PATH}/tree/package.json": _record(manifest),
                   f"{PRODUCER_PATH}/tree/yarn.lock": _record(lock)},
        "pin_graph": graph, "packages": packages, "archive_count": len(packages),
        "expanded_tar_bytes": expanded_total, "member_count": member_total,
        "files": dict(sorted(files.items())), "directories": sorted(directories),
        "original_archives_verified": True, "expanded_distribution_bytes_verified": True,
        "source_scope": "published Yarn distributions, not complete implementation preferred sources",
        "mode_scope": "archive modes retained; file executable intent755/644; Git does not retain directory modes",
        "path_safety_scope": producer.PATH_SCOPE, "licensing_scope": producer.LICENSE_SCOPE,
        "limits": {"compressed_per_archive": MAX_COMPRESSED, "expanded_per_archive": MAX_EXPANDED,
                   "expanded_total": MAX_TOTAL_EXPANDED, "members_per_archive": MAX_MEMBERS,
                   "members_total": MAX_TOTAL_MEMBERS, "package_json_bytes": MAX_MANIFEST},
        **{key: False for key in FALSE_CLAIMS},
    }
    canonical = producer.json_bytes(report)
    _require(len(canonical) <= MAX_REPORT, "report_byte_limit")
    if REPORT_NAME in names:
        report_bytes = producer.read_bytes(bundle / REPORT_NAME, MAX_REPORT)
        if require_report:
            _require(report_bytes == canonical, "canonical_report_differs")
        files = {**files, REPORT_NAME: {**_record(report_bytes), "mode": 0o644}}
    _check_inventory(bundle, files, directories)
    _require(_inputs(project) == (evidence, manifest, lock, source_report), "producer_inputs_changed")
    _check_inventory(bundle, files, directories)
    _require({entry.name for entry in bundle.iterdir()} == names, "namespace_changed")
    return report


def derive_bundle(project: Path, bundle: Path) -> dict:
    """Derive canonical evidence from complete originals and inert expanded bytes.

    The authored producer is independently verified under project. The supplied
    fresh namespace contains archives/ and packages/, optionally its report.
    An optional report is not accepted as evidence by this derivation. Returns
    JSON-ready proof with all build/preferred-source claims false. No writes or
    dependency execution occur; path/archive/input errors raise ValueError/OSError.
    Stable unlinked ancestors are required, not hostile-concurrency isolation.
    """
    return _derive(Path(project), Path(bundle), require_report=False)


def verify_bundle(project: Path, bundle: Path | None = None) -> dict:
    """Reverify the exact stored namespace, canonical report, and producer association.

    Default bundle is the fixed producer-dependencies Yarn namespace in project.
    Returns rederived evidence, not receipt assertions. Missing/extra/mutated or
    unsafe inputs raise ValueError/OSError. Optional and local-lock placements
    remain in the graph; local placeholders never fabricate registry archives.
    No network, installer, package source execution, or input writes occur.
    """
    project = Path(project)
    return _derive(project, Path(bundle) if bundle is not None else project / BUNDLE_PATH,
                   require_report=True)


def main(arguments: list[str] | None = None) -> int:
    """Print derived JSON or compact verification counts; reject unsafe inputs with exit1."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--derive-report", action="store_true")
    options = parser.parse_args(arguments)
    try:
        bundle = options.bundle if options.bundle is not None else options.project / BUNDLE_PATH
        if options.derive_report:
            sys.stdout.buffer.write(producer.json_bytes(derive_bundle(options.project, bundle)))
        else:
            report = verify_bundle(options.project, bundle)
            print(f"verified {report['archive_count']} original Yarn archives and {report['member_count']} members")
        return 0
    except (ValueError, OSError, RecursionError):
        print("producer Yarn source verification failed", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
