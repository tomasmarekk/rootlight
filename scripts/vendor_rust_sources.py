#!/usr/bin/env python3
"""Import pinned local Rust archives for subsequent versioned source review.

The existing bundle materializer and verifier authenticate every registry input.
This preparation performs no acquisition or build and proves no frontend closure.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import stat
import tarfile
import tempfile
from typing import Any

from prepare_rust_source_bundle import (
    ARCHIVES_PATH,
    LOCK_PATHS,
    VENDOR_PATH,
    digest,
    linked_path,
    materialize_archive,
    registry_inputs,
    regular_source,
)
from verify_cargo_source_bundle import verify_bundle


REPORT_NAME = "cargo-source-closure.json"


class PreparationError(ValueError):
    """Local input validation or source publication failed without acquisition."""


def _directory(path: Path, label: str) -> Path:
    path = Path(path).absolute()
    if ".." in path.parts:
        raise PreparationError(f"{label}: parent traversal is prohibited")
    for candidate in (path, *path.parents):
        if linked_path(candidate) or not stat.S_ISDIR(candidate.lstat().st_mode):
            raise PreparationError(f"{label}: expected non-link directory ancestors")
    return path


def _new_destination(path: Path) -> None:
    if path.exists() or linked_path(path):
        raise PreparationError("source-inputs already exists; no inputs were replaced")


def vendor_sources(root: Path, archives: Path) -> dict[str, Any]:
    """Publish a new source-inputs tree from an exact local archive inventory.

    Args:
        root: Existing project containing the three canonical lockfiles.
        archives: Non-link directory containing exactly their original .crate
            archive union, named <package>-<version>.crate. No cache is searched.

    Returns:
        Deterministic dependency-only verifier evidence with archive-derived file
        modes and explicit false frontend and build-acceptance claims. The same
        evidence is written to source-inputs/cargo-source-closure.json.

    Raises:
        PreparationError: An input is unsafe, missing, changed, malformed or
            unauthenticated, or source-inputs already exists. Temporary staging
            is removed on failure; existing destination contents are never edited.

    Sources and archives are published only after existing verification succeeds.
    This neither executes project code nor changes locks, Cargo configuration,
    Git state or licensing metadata. Versioning and review remain separate work.
    """
    try:
        root = _directory(root, "project root")
        archives = _directory(archives, "archive root")
        destination = root / "source-inputs"
        _new_destination(destination)
        locks = [regular_source(root, name) for name in LOCK_PATHS]
        lock_hashes = {name: digest(path) for name, path in zip(LOCK_PATHS, locks)}
        inputs = registry_inputs(root)
        filenames = {f"{name}-{version}.crate" for name, version in inputs}
        observed = {entry.name for entry in archives.iterdir()}
        if filenames != observed:
            missing, extra = sorted(filenames - observed), sorted(observed - filenames)
            raise PreparationError(f"archive inventory differs: missing={missing}; extra={extra}")
        acquired = {}
        for (name, version), checksum in inputs.items():
            filename = f"{name}-{version}.crate"
            source = regular_source(archives, filename)
            if digest(source) != checksum:
                raise PreparationError(f"archive checksum differs from lockfiles: {filename}")
            acquired[filename] = source

        # Staging prevents a valid early archive from publishing a partial closure
        # when a later archive or final verification fails.
        with tempfile.TemporaryDirectory(prefix=".rust-source-inputs-", dir=root) as temporary:
            staged = Path(temporary) / "source-inputs"
            vendor = staged / Path(VENDOR_PATH).name
            retained = staged / Path(ARCHIVES_PATH).name
            vendor.mkdir(parents=True)
            retained.mkdir()
            modes = {}
            for (name, version), checksum in inputs.items():
                filename = f"{name}-{version}.crate"
                target = retained / filename
                shutil.copyfile(acquired[filename], target)
                target.chmod(0o644)
                modes[f"{ARCHIVES_PATH}/{filename}"] = 0o644
                modes.update(materialize_archive(target, vendor, name, version, checksum))
            evidence = verify_bundle(vendor, locks, archives=retained)
            for name, path in zip(LOCK_PATHS, locks):
                if digest(regular_source(root, name)) != lock_hashes[name]:
                    raise PreparationError("project lockfiles changed during preparation")
            if {record["path"]: record["sha256"] for record in evidence["lockfiles"]} != lock_hashes:
                raise PreparationError("verified lockfiles differ from initial project inputs")
            evidence["file_modes"] = dict(sorted(modes.items()))
            evidence["frontend_external_source_closure_verified"] = False
            evidence["build_and_packaging_acceptance_verified"] = False
            report = staged / REPORT_NAME
            report.write_text(json.dumps(evidence, sort_keys=True, indent=2) + "\n",
                              encoding="utf-8", newline="\n")
            report.chmod(0o644)
            _directory(root, "project root")
            _new_destination(destination)
            os.rename(staged, destination)
            return evidence
    except PreparationError:
        raise
    except (OSError, ValueError, KeyError, TypeError, tarfile.TarError) as error:
        raise PreparationError(str(error)) from error


def main(arguments: list[str] | None = None) -> int:
    """Run explicit local preparation; return zero only after verified publication.

    Invalid local inputs are diagnosed on stderr with exit status one. No Cargo,
    Git, network, installation, or compilation command is invoked.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--archives", type=Path, required=True)
    options = parser.parse_args(arguments)
    try:
        evidence = vendor_sources(options.root, options.archives)
    except PreparationError as error:
        parser.exit(1, f"Rust source vendoring failed: {error}\n")
    print(json.dumps({"schema": evidence["schema"], "package_count": evidence["package_count"],
                      "status": "verified", "frontend_external_source_closure_verified": False},
                     sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
