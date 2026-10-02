#!/usr/bin/env python3
"""Reverify the pinned Lightning CSS authored tree and its local npm association.

Archive, source-tree and caller-observed publisher evidence are separate inputs.
This read-only verifier proves neither producer dependency closure nor rebuilding.
"""

from __future__ import annotations

import argparse
from datetime import datetime
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import tarfile
import tomllib

import verify_local_npm_sources as local


COMMIT = "1d680fa14e9a089c92dc0929f869d6757ae91c30"
PACKAGE = "lightningcss"
VERSION = "1.33.0"
ROOT = f"lightningcss-{COMMIT}"
ARCHIVE_NAME = ROOT + ".tar.gz"
ARCHIVE_SHA256 = "ab377f42f5aa2f7f85058408ab45dbd03caa65fe6c00f4fdc32b9611c4e51bb4"
URL = f"https://codeload.github.com/parcel-bundler/lightningcss/tar.gz/{COMMIT}"
METADATA_URL = f"https://registry.npmjs.org/{PACKAGE}/{VERSION}"
NPM_REPORT = "source-inputs/npm/npm-source-closure.json"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
PREPARED_NAMES = {ARCHIVE_NAME, "publisher-metadata.json", "producer-inputs.json"}
STORED_NAMES = PREPARED_NAMES | {"publisher-capture.json", "source-tree.json", "tree"}
MAX_COMPRESSED = 64 * 1024 * 1024
MAX_EXPANDED = 256 * 1024 * 1024
MAX_MEMBERS = 20000
MAX_JSON = 32 * 1024 * 1024
MAX_MANIFEST = 8 * 1024 * 1024
CHUNK = 64 * 1024
REQUIRED = {"Cargo.toml", "Cargo.lock", "package.json", "yarn.lock", "LICENSE",
            "node/Cargo.toml", "napi/Cargo.toml", "scripts/build.js"}
FALSE_CLAIMS = ("upstream_commit_content_independently_verified", "archive_signatures_verified",
                "links_extracted_or_followed", "producer_lock_edge_inventory_complete",
                "producer_transitive_sources_complete", "preferred_source_closure_complete",
                "source_to_binary_rebuild_verified", "frontend_source_complete",
                "os_denied_acceptance_verified", "build_and_packaging_acceptance_verified")
PATH_SCOPE = "unlinked stable ancestors; not hostile-concurrency isolation"
LICENSE_SCOPE = "retained supplied source license bytes and metadata; not legal completeness or authorship"
_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_LICENSE = re.compile(r"(?:license|licence|copying|notice|patents)(?:[._-].*)?\Z", re.I)


class ProducerVerificationError(ValueError):
    """A retained producer input differs from the pinned read-only contract."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ProducerVerificationError(message)


def json_bytes(document: object) -> bytes:
    """Return canonical report bytes; unsupported/nonfinite values raise ValueError."""
    return (json.dumps(document, sort_keys=True, indent=2, allow_nan=False) + "\n").encode()


def digest(payload: bytes) -> str:
    """Return the SHA-256 of supplied immutable bytes without I/O."""
    return hashlib.sha256(payload).hexdigest()


def safe_path(path: Path, *, directory: bool) -> Path:
    """Check stable nonlinked ancestors and input kind; reject parent traversal.

    Raises ProducerVerificationError or the shared typed npm path error. This
    does not isolate concurrent hostile ancestor replacement.
    """
    _require(".." not in Path(path).parts, "parent traversal is prohibited")
    absolute = Path(os.path.abspath(path))
    local.original._check_path(absolute, directory=directory, label="producer input")
    return absolute


def read_bytes(path: Path, maximum: int) -> bytes:
    """Read a bounded nonlink input with shared same-file mutation guards.

    Raises ProducerVerificationError or shared input errors, and performs no writes.
    Callers must retain the stated stable-ancestor boundary.
    """
    path = safe_path(path, directory=False)
    before = local.original._check_path(path, directory=False, label="producer input")
    _require(before.st_size <= maximum, "producer input exceeds byte limit")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    with os.fdopen(os.open(path, flags), "rb") as stream:
        opened = os.fstat(stream.fileno())
        _require(stat.S_ISREG(opened.st_mode) and not local.original._is_link(opened)
                 and local.original._file_stamp(opened) == local.original._file_stamp(before),
                 "producer input changed while opening")
        payload = stream.read(maximum + 1)
        _require(len(payload) <= maximum, "producer input exceeds byte limit")
        after = os.fstat(stream.fileno())
    current = local.original._check_path(path, directory=False, label="producer input")
    _require(all(local.original._file_stamp(item) == local.original._file_stamp(before)
                 for item in (opened, after, current))
             and local.original._stable_change_times(before, opened, after, current, windows=os.name == "nt")
             and len(payload) == before.st_size, "producer input changed while reading")
    return payload


def _cargo_lock(payload: bytes) -> dict:
    document = tomllib.loads(payload.decode("utf-8"))
    _require(set(document) == {"version", "package"}
             and type(document["version"]) is int and document["version"] in (3, 4)
             and isinstance(document["package"], list), "unsupported producer Cargo lock")
    packages, seen = [], set()
    for package in document["package"]:
        _require(isinstance(package, dict) and set(package) <= {
            "name", "version", "source", "checksum", "dependencies"}, "unsupported Cargo package fields")
        _require(all(isinstance(package.get(key), str) and package[key] for key in ("name", "version")),
                 "invalid Cargo locked identity")
        source = package.get("source")
        _require(source is None or source == CRATES_IO, "unsupported producer Cargo source")
        if source is not None:
            _require(isinstance(package.get("checksum"), str)
                     and bool(_SHA256.fullmatch(package["checksum"])), "registry checksum is absent")
        else:
            _require("checksum" not in package, "local Cargo package has a checksum")
        dependencies = package.get("dependencies", [])
        _require(isinstance(dependencies, list) and all(isinstance(item, str) and item for item in dependencies),
                 "invalid Cargo dependency edges")
        key = (package["name"], package["version"], source or "")
        _require(key not in seen, "duplicate Cargo locked identity")
        seen.add(key)
        packages.append(package)
    return {"format_version": document["version"],
            "packages": sorted(packages, key=lambda item: (item["name"], item["version"], item.get("source", ""))),
            "external_package_count": sum("source" in item for item in packages),
            "extra_tables": {}, "edge_inventory_complete": True}


def _inspect_archive(payload: bytes) -> tuple[dict, dict[str, bytes]]:
    _require(0 < len(payload) <= MAX_COMPRESSED, "compressed source exceeds limit")
    chunks, total = [], 0
    with gzip.GzipFile(fileobj=io.BytesIO(payload)) as stream:
        while chunk := stream.read(CHUNK):
            total += len(chunk)
            _require(total <= MAX_EXPANDED, "expanded source exceeds limit")
            chunks.append(chunk)
    expanded = b"".join(chunks)
    _require(len(expanded) % 512 == 0, "unaligned source tar framing")
    members, seen, aliases, files, binaries, tail = [], set(), {}, {}, [], 0
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as archive:
        for member in archive:
            _require(len(members) < MAX_MEMBERS, "source member count exceeds limit")
            path = member.name.rstrip("/") if member.isdir() else member.name
            local.original._safe_path(path, "source archive member")
            _require(path == ROOT or path.startswith(ROOT + "/"), "source archive root differs")
            _require(path not in seen, "duplicate source member")
            seen.add(path)
            local.original._path_aliases(path, aliases)
            _require(0 <= member.mode <= 0o777 and member.size >= 0 and not member.issparse(),
                     "unsupported source mode or sparse member")
            record = {"path": path, "archive_mode": member.mode, "bytes": member.size}
            relative = path.removeprefix(ROOT + "/")
            if member.isfile():
                _require(path != ROOT and member.size <= MAX_EXPANDED, "invalid regular source member")
                content = archive.extractfile(member).read(member.size + 1)
                _require(len(content) == member.size, "source member length differs")
                record.update(kind="file", sha256=digest(content))
                files[relative] = content
                formats = local.original._native_formats(path, content[:16])
                if formats:
                    binaries.append({**record, "formats": formats})
            elif member.isdir():
                _require(member.size == 0, "directory contains source data")
                record["kind"] = "directory"
            else:
                raise ProducerVerificationError("linked or special source member is not permitted")
            tail = max(tail, member.offset_data + (member.size + 511) // 512 * 512)
            members.append(record)
    _require(len(expanded) - tail >= 1024 and not any(expanded[tail:]), "source tar trailer differs")
    by_path = {record["path"]: record for record in members}
    for record in members:
        parts = record["path"].split("/")
        for end in range(1, len(parts)):
            ancestor = "/".join(parts[:end])
            _require(ancestor not in by_path or by_path[ancestor]["kind"] == "directory",
                     "source member is below a non-directory")
    _require(REQUIRED <= files.keys(), "required authored producer inputs are missing")
    inputs = {path: content for path, content in files.items()
              if path in REQUIRED or path.endswith("/Cargo.toml")}
    _require(all(len(content) <= MAX_MANIFEST for content in inputs.values()), "producer manifest exceeds limit")
    package = local.original._parse_json(inputs["package.json"], "producer package")
    _require(isinstance(package, dict) and package.get("name") == PACKAGE and package.get("version") == VERSION,
             "authored producer package differs")
    manifests = [{"path": path, "sha256": digest(content),
                  "document": tomllib.loads(content.decode("utf-8"))}
                 for path, content in sorted(inputs.items()) if path.endswith("Cargo.toml")]
    ordered = sorted(members, key=lambda item: item["path"])
    preferred = [item for item in ordered if item["kind"] == "file" and (
        item["path"].endswith((".rs", ".js", ".ts", ".c", ".h", ".toml", ".lock"))
        or "/patches/" in item["path"] or _LICENSE.fullmatch(PurePosixPath(item["path"]).name))]
    inventory = {"archive_root": ROOT, "archive": {"bytes": len(payload), "sha256": digest(payload)},
                 "expanded_bytes": len(expanded), "member_count": len(ordered), "members": ordered,
                 "member_inventory_sha256": digest(json_bytes(ordered)), "authored_input_inventory": preferred,
                 "native_binary_files": sorted(binaries, key=lambda item: item["path"]),
                 "license_input_inventory": [item for item in ordered if item["kind"] == "file"
                                             and _LICENSE.fullmatch(PurePosixPath(item["path"]).name)],
                 "cargo_manifests": manifests, "cargo_lock": _cargo_lock(inputs["Cargo.lock"]),
                 "yarn_lock": {"sha256": digest(inputs["yarn.lock"]), "bytes": len(inputs["yarn.lock"]),
                               "edge_inventory_complete": False, "reason": "raw lock retained; external edges not enumerated"}}
    return inventory, files


def inspect_archive(payload: bytes) -> dict:
    """Derive bounded inert inventory using the acquisition report's exact schema.

    Rejects links, special/aliased members and unsupported Cargo lock fields.
    No members are extracted or executed. Yarn is retained, not interpreted.
    This function is suitable for the separate acquisition driver to share after
    it is moved to tracked code; callers independently authenticate archive bytes.
    """
    return _inspect_archive(payload)[0]


def publisher_association(project: Path, publisher: bytes) -> dict:
    """Bind retained publisher metadata to independently verified local npm pins.

    Reads the canonical npm inputs without installation and raises typed input
    errors on mismatch. It does not fetch or authenticate publisher transport.
    """
    verified = local.verify_local_bundle(project)
    report_bytes = read_bytes(project / NPM_REPORT, MAX_JSON)
    _require(report_bytes == json_bytes(verified), "retained npm report differs from authenticated inputs")
    selected = [record for record in verified["packages"]
                if record["name"] == PACKAGE and record["version"] == VERSION]
    _require(len(selected) == 1 and selected[0]["distribution_kind"] == "original_registry_archive",
             "publisher package is absent, ambiguous or not original")
    package = selected[0]
    metadata = local.original._parse_json(publisher, "publisher metadata")
    _require(isinstance(metadata, dict) and metadata.get("name") == PACKAGE
             and metadata.get("version") == VERSION and metadata.get("gitHead") == COMMIT,
             "publisher association differs")
    repository, dist = metadata.get("repository"), metadata.get("dist")
    _require(isinstance(repository, dict) and repository.get("url") in {
        "git+https://github.com/parcel-bundler/lightningcss.git",
        "https://github.com/parcel-bundler/lightningcss.git"}, "publisher repository differs")
    pin = package["upstream_pin"]
    _require(isinstance(dist, dict) and dist.get("tarball") == pin["resolved"]
             and dist.get("integrity") == pin["integrity"], "publisher distribution pin differs")
    recomputed = local.original._verify_archive(read_bytes(project / package["archive_path"], MAX_COMPRESSED), pin)
    _require(recomputed["archive"] == package["archive"]
             and recomputed["package_json_sha256"] == package["package_json_sha256"], "publisher archive differs")
    keys = ("name", "version", "archive_path", "archive", "lock_paths", "upstream_pin")
    related = [{key: record[key] for key in keys} for record in verified["packages"]
               if record["version"] == VERSION and record["name"].startswith(PACKAGE + "-")]
    return {"kind": "caller-supplied publisher metadata matching authenticated local npm pins",
            "expected_metadata_url": METADATA_URL, "publisher_metadata_sha256": digest(publisher),
            "npm_report_sha256": digest(report_bytes), "canonical_lock_sha256": verified["lockfile_sha256"],
            "upstream_lock_sha256": verified["upstream_lockfile_sha256"], "git_head": COMMIT,
            "published_main_distribution": {key: package[key] for key in keys},
            "related_locked_platform_distributions": sorted(related, key=lambda item: item["name"]),
            "related_platform_producer_association_verified": False, "publisher_metadata_transport_verified": False,
            "publisher_metadata_signatures_verified": False, "source_to_binary_association_attested": False}


def prepared_report(association: dict, inventory: dict) -> dict:
    """Derive the supported acquisition evidence; no historical assertion is trusted."""
    return {"schema": "rootlight.lightning-producer-inputs/1", "association": association,
            "source_archive_url": URL, "source_inventory": inventory, "archive_sha256_preverified": False,
            **{key: False for key in FALSE_CLAIMS}, "path_safety_scope": PATH_SCOPE, "licensing_scope": LICENSE_SCOPE}


def _capture(payload: bytes, publisher: bytes) -> dict:
    capture = local.original._parse_json(payload, "publisher capture")
    expected_keys = {"schema", "requested_url", "status_code", "content_type", "payload_bytes", "payload_sha256",
                     "git_head", "observed_by_utc", "scope", "registry_signatures_verified", "source_to_binary_rebuild_verified"}
    _require(isinstance(capture, dict) and set(capture) == expected_keys, "unsupported publisher capture schema")
    _require(capture["schema"] == "rootlight.publisher-response-capture/1"
             and capture["requested_url"] == METADATA_URL and type(capture["status_code"]) is int
             and capture["status_code"] == 200 and capture["content_type"] == "application/json"
             and type(capture["payload_bytes"]) is int and capture["payload_bytes"] == len(publisher)
             and capture["payload_sha256"] == digest(publisher) and capture["git_head"] == COMMIT
             and capture["registry_signatures_verified"] is False
             and capture["source_to_binary_rebuild_verified"] is False, "publisher capture differs")
    _require(isinstance(capture["scope"], str) and capture["scope"] ==
             "direct HTTPS response; proxy disabled and redirects denied; identity checked against exact locked package",
             "unsupported publisher observation scope")
    date = capture["observed_by_utc"]
    _require(isinstance(date, str) and bool(re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", date)),
             "invalid publisher observation time")
    datetime.fromisoformat(date.replace("Z", "+00:00"))
    return {"sha256": digest(payload), "payload_sha256": digest(publisher), "observed_by_utc": date,
            "scope": "retained caller observation; transport not independently verified by this verifier",
            "publisher_metadata_transport_verified": False, "registry_signatures_verified": False}


def validate_prepared(project: Path, prepared: Path, capture: Path) -> tuple[dict, dict[str, bytes], dict[str, bytes]]:
    """Recompute prepared evidence and return report, retained bytes and tree files.

    Requires exact three-file preparation inventory, the reviewed archive SHA-256
    pin, canonical npm inputs and separate caller capture. Performs no writes.
    Unsupported schemas, mutation, links or changed claims raise typed errors.
    """
    project, prepared = safe_path(project, directory=True), safe_path(prepared, directory=True)
    _require({entry.name for entry in prepared.iterdir()} == PREPARED_NAMES, "prepared inventory differs")
    blobs = {name: read_bytes(prepared / name, MAX_COMPRESSED if name == ARCHIVE_NAME else MAX_JSON)
             for name in sorted(PREPARED_NAMES)}
    blobs["publisher-capture.json"] = read_bytes(capture, MAX_JSON)
    _require(digest(blobs[ARCHIVE_NAME]) == ARCHIVE_SHA256, "source archive differs from reviewed pin")
    inventory, files = _inspect_archive(blobs[ARCHIVE_NAME])
    association = publisher_association(project, blobs["publisher-metadata.json"])
    derived = prepared_report(association, inventory)
    _require(blobs["producer-inputs.json"] == json_bytes(derived), "prepared report differs from rederived inputs")
    evidence = _tree_report(association, inventory, blobs)
    for name in PREPARED_NAMES:
        _require(read_bytes(prepared / name, MAX_COMPRESSED if name == ARCHIVE_NAME else MAX_JSON) == blobs[name],
                 "prepared input changed during verification")
    _require(read_bytes(capture, MAX_JSON) == blobs["publisher-capture.json"], "publisher capture changed")
    _require(publisher_association(project, blobs["publisher-metadata.json"]) == association, "npm association changed")
    _require({entry.name for entry in prepared.iterdir()} == PREPARED_NAMES, "prepared inventory changed")
    return evidence, blobs, files


def verify_bundle(project: Path, bundle: Path) -> dict:
    """Check the exact stored authored tree, original evidence and current npm pins.

    Returns deterministic JSON evidence. Rejects extra/missing files/directories,
    traversal failures, mutation and POSIX file-mode differences. Raw directory
    modes are retained archive metadata, not a restored-permission guarantee;
    Git preserves file executable intent, not directory permissions. No writes occur.
    """
    bundle = safe_path(bundle, directory=True)
    _require({entry.name for entry in bundle.iterdir()} == STORED_NAMES, "stored producer inventory differs")
    evidence, blobs, files = _stored_inputs(project, bundle)
    _require(read_bytes(bundle / "source-tree.json", MAX_JSON) == json_bytes(evidence), "source-tree report differs")
    tree = safe_path(bundle / "tree", directory=True)
    directories = set()
    modes = {}
    for record in evidence["source_inventory"]["members"]:
        rel = record["path"].removeprefix(ROOT + "/")
        if record["path"] != ROOT and record["kind"] == "directory":
            directories.add(rel)
        if record["kind"] == "file":
            modes[rel] = 0o755 if record["archive_mode"] & 0o111 else 0o644
    for path in set(files) | directories:
        directories.update(str(parent) for parent in PurePosixPath(path).parents if str(parent) != ".")
    _check_tree(tree, files, directories, modes)
    if os.name != "nt":
        for name in STORED_NAMES - {"tree"}:
            _require(stat.S_IMODE((bundle / name).stat().st_mode) == 0o644, "stored evidence mode differs")
    for name, content in blobs.items():
        _require(read_bytes(bundle / name, MAX_COMPRESSED if name == ARCHIVE_NAME else MAX_JSON) == content,
                 "retained input changed while checking tree")
    _require(read_bytes(bundle / "source-tree.json", MAX_JSON) == json_bytes(evidence), "source-tree report changed")
    _require(publisher_association(project, blobs["publisher-metadata.json"]) == evidence["association"],
             "npm association changed while checking tree")
    _check_tree(tree, files, directories, modes)
    _require({entry.name for entry in bundle.iterdir()} == STORED_NAMES, "stored producer inventory changed")
    return evidence


def _check_tree(tree: Path, files: dict[str, bytes], directories: set[str], modes: dict[str, int]) -> None:
    observed_files, observed_dirs = set(), set()
    for current, dirs, names in os.walk(tree, followlinks=False, onerror=_walk_error):
        current = safe_path(Path(current), directory=True)
        for name in dirs:
            child = safe_path(current / name, directory=True)
            observed_dirs.add(child.relative_to(tree).as_posix())
        for name in names:
            child = safe_path(current / name, directory=False)
            rel = child.relative_to(tree).as_posix()
            _require(rel in files and read_bytes(child, MAX_EXPANDED) == files[rel], "stored source bytes differ")
            if os.name != "nt":
                _require(stat.S_IMODE(child.stat().st_mode) == modes[rel], "stored source mode differs")
            observed_files.add(rel)
    _require(observed_files == set(files) and observed_dirs == directories, "stored source path inventory differs")


def _walk_error(error: OSError) -> None:
    raise ProducerVerificationError("source tree traversal failed") from error


def _tree_report(association: dict, inventory: dict, blobs: dict[str, bytes]) -> dict:
    return {"schema": "rootlight.lightning-producer-tree/1", "git_head": COMMIT,
            "source_archive_url": URL, "source_inventory": inventory, "association": association,
            "publisher_capture": _capture(blobs["publisher-capture.json"], blobs["publisher-metadata.json"]),
            "retained_inputs": {name: {"bytes": len(content), "sha256": digest(content)}
                                for name, content in sorted(blobs.items())},
            **{key: False for key in FALSE_CLAIMS}, "path_safety_scope": PATH_SCOPE, "licensing_scope": LICENSE_SCOPE}


def _stored_inputs(project: Path, bundle: Path) -> tuple[dict, dict[str, bytes], dict[str, bytes]]:
    blobs = {name: read_bytes(bundle / name, MAX_COMPRESSED if name == ARCHIVE_NAME else MAX_JSON)
             for name in sorted(PREPARED_NAMES | {"publisher-capture.json"})}
    _require(digest(blobs[ARCHIVE_NAME]) == ARCHIVE_SHA256, "source archive differs from reviewed pin")
    inventory, files = _inspect_archive(blobs[ARCHIVE_NAME])
    association = publisher_association(safe_path(project, directory=True), blobs["publisher-metadata.json"])
    _require(blobs["producer-inputs.json"] == json_bytes(prepared_report(association, inventory)),
             "prepared report differs from rederived inputs")
    evidence = _tree_report(association, inventory, blobs)
    return evidence, blobs, files


def main(arguments: list[str] | None = None) -> int:
    """Verify --project/--bundle offline, returning one on any input failure."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--bundle", type=Path, required=True)
    options = parser.parse_args(arguments)
    try:
        result = verify_bundle(options.project, options.bundle)
    except (OSError, ValueError, KeyError, TypeError, EOFError, RecursionError, tarfile.TarError):
        print("producer verification failed; no acceptance claim", file=sys.stderr)
        return 1
    print(json.dumps({"member_count": result["source_inventory"]["member_count"],
                      "preferred_source_closure_complete": False}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
