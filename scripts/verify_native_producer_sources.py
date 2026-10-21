#!/usr/bin/env python3
"""Authenticate the reviewed Flux and Raffia siblings in shared source parents.

Pinned catalogs bind retained upstream bytes and reviewed local derivatives.
This proves source custody only; no package code, generator or build is executed.
"""

from __future__ import annotations

import json
import os
from pathlib import Path, PurePosixPath
import stat

from verify_cargo_source_bundle import (
    BundleVerificationError, _is_link, _read_file, _require_directory,
    _require_digest, _safe_relative_path,
)


PRODUCERS_PATH = "source-inputs/producers"
REVIEWED_PRODUCERS = {
    "flux-9eb12f7f4702875e79189789cf9b3ac560499c11": {
        "LICENSE": "e99a21149ddba71738af89c98bdfee84ddfdb55076cc16af6a5f0227d74fd453",
        "LOCAL_CHANGES.md": "b07ee9f744a003afbc3e1d92684d76c7fc2ca7decdd4a2affb8a8a93d7723f37",
        "PROVENANCE.json": "33a79733d3019d02ee40e26519c2512b5d99bfbdeb59a85e933410248bd2026b",
        "SELECTED_SOURCE_MAP.json": "8e2f78e74d29d958239617a23f322f31f5d4f412c0066d5b799e2e8f9d9c05e2",
        "SOURCE_MANIFEST.json": "ee36078aa5596a12753ae8d68393da1fe74be532a7d537d2755f72291c39f409",
        "UPSTREAM_MANIFEST.json": "604df1ac93281cbe2f141bb04d41cb388cc0dc7f608cab33ef5896f5abbe8ad5",
    },
    "raffia-0.13.0": {
        "LOCAL_CHANGES.patch": "cfdabd059a9df53bad8c514950bbc1a55dc30072a9af997de40e06e59ee48c1c",
        "SOURCES.md": "80a735afd27ade7f1bd8ce8cb2ca20683ed2b81aaa5300aeb857dd9fd31e7982",
        "SOURCE_MANIFEST.json": "1bb9c26b61c5920e3e59809474c8c25591809c86a63a62160adf87c8db387b39",
    },
}


def _records(name: str, metadata: dict[str, bytes]) -> dict[str, tuple[int, str, int]]:
    manifest = json.loads(metadata["SOURCE_MANIFEST.json"])
    records = {}

    def add(path, size, checksum, mode=0o644):
        _safe_relative_path(path, "native producer catalog")
        _require_digest(checksum, "native producer catalog")
        if path in records or type(size) is not int or size < 0:
            raise BundleVerificationError("invalid native producer catalog record")
        records[path] = (size, checksum, mode)

    if name.startswith("flux-"):
        if manifest["schema"] != "rootlight.native-producer-source-inputs/1":
            raise BundleVerificationError("unsupported Flux source catalog")
        upstream = json.loads(metadata["UPSTREAM_MANIFEST.json"])
        if len(upstream) != manifest["upstream_files"]:
            raise BundleVerificationError("Flux upstream inventory differs")
        for row in upstream:
            add("upstream/" + row["path"], row["bytes"], row["sha256"],
                0o755 if row["mode"] == "100755" else 0o644)
        for row in manifest["selected_files"]:
            add(row["path"], row["bytes"], row["sha256"])
    else:
        if manifest["schema"] != "rootlight.raffia-source-bundle/1":
            raise BundleVerificationError("unsupported Raffia source catalog")
        for row in manifest["upstream"]:
            add(row["path"], row["local_bytes"], row["local_sha256"])
        for row in manifest["added_licenses"]:
            add(row["path"], row["bytes"], row["sha256"])
    for path, raw in metadata.items():
        add(path, len(raw), REVIEWED_PRODUCERS[name][path])
    return records


def native_source_inputs(project: Path) -> tuple[dict[str, int], dict[str, str], set[str]]:
    """Verify present reviewed siblings and return exact portable export inputs.

    Missing complete siblings are permitted for older source frames. A present
    sibling must match its pinned metadata and every retained source record;
    partial, changed, extra or linked source inputs reject. Only the Flux local
    Cargo target is excluded as generated build state, never exported or claimed
    as source. Shared-parent namespace admission belongs to the caller.

    Returns file modes, SHA-256 hashes and parent directories without modifying
    any input. Raises ValueError or OSError on malformed or unavailable custody.
    """
    project = project.absolute()
    modes, hashes, directories = {}, {}, set()
    for name, pins in REVIEWED_PRODUCERS.items():
        prefix = f"{PRODUCERS_PATH}/{name}"
        root = project / prefix
        if not os.path.lexists(root):
            continue
        for ancestor in (root, *root.parents):
            _require_directory(ancestor, "native producer ancestor")
        metadata = {}
        for path, checksum in pins.items():
            raw, record = _read_file(root / path, f"{name}/{path}", collect=True)
            if record["sha256"] != checksum:
                raise BundleVerificationError("native producer metadata differs from reviewed pin")
            metadata[path] = raw
        records = _records(name, metadata)
        expected_directories = {str(parent) for path in records
                                for parent in PurePosixPath(path).parents if str(parent) != "."}
        observed, pending = set(), [root]
        while pending:
            directory = pending.pop()
            for path in directory.iterdir():
                relative = path.relative_to(root).as_posix()
                # This ignored target is outside the authored and selected catalogs.
                if name.startswith("flux-") and relative == "parser/target":
                    continue
                _safe_relative_path(relative, "native producer input")
                attributes = path.lstat()
                if _is_link(attributes):
                    raise BundleVerificationError("linked native producer input")
                if stat.S_ISDIR(attributes.st_mode):
                    if relative not in expected_directories:
                        raise BundleVerificationError("extra native producer source directory")
                    pending.append(path)
                else:
                    if relative not in records:
                        raise BundleVerificationError("extra native producer source input")
                    _, record = _read_file(path, f"{name}/{relative}")
                    size, checksum, mode = records[relative]
                    if (record["bytes"], record["sha256"]) != (size, checksum):
                        raise BundleVerificationError("native producer source differs from catalog")
                    observed.add(relative)
                    full = f"{prefix}/{relative}"
                    hashes[full], modes[full] = checksum, mode
                    directories.update(str(parent) for parent in PurePosixPath(full).parents
                                       if str(parent).startswith(PRODUCERS_PATH))
        if observed != set(records):
            raise BundleVerificationError("missing native producer source input")
    return dict(sorted(modes.items())), dict(sorted(hashes.items())), directories
