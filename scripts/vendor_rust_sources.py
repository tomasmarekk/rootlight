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
    relative_path,
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
        raise PreparationError("source destination already exists; no inputs were replaced")


def vendor_sources(root: Path, archives: Path, *, lock_paths: tuple[str, ...] = LOCK_PATHS,
                   destination: Path | None = None) -> dict[str, Any]:
    """Publish a fresh Cargo-only tree from an exact local archive inventory.

    Args:
        root: Existing project containing the supplied lockfiles.
        archives: Non-link directory containing exactly their original .crate
            archive union, named <package>-<version>.crate. No cache is searched.
        lock_paths: Distinct portable project-relative lock paths, including the
            root Cargo.lock so verifier associations remain project-relative.
            The default retains the three canonical project locks.
        destination: New directory under an existing non-link parent; defaults
            to root/source-inputs. The output contains only cargo/, crates/ and
            their dependency report, never merges with other source inputs.

    Returns:
        Deterministic dependency-only verifier evidence with archive-derived file
        modes and explicit false frontend and build-acceptance claims. The same
        evidence is written to destination/cargo-source-closure.json. File-mode
        keys retain canonical source-inputs paths for subsequent versioning.

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
        destination = (root / "source-inputs" if destination is None else Path(destination).absolute())
        if ".." in destination.parts:
            raise PreparationError("source destination: parent traversal is prohibited")
        relative_path(destination.name)
        parent = _directory(destination.parent, "source destination parent")
        if destination.is_relative_to(archives):
            raise PreparationError("source destination must not modify the archive input directory")
        _new_destination(destination)
        inputs = registry_inputs(root, lock_paths=lock_paths)
        if "Cargo.lock" not in lock_paths:
            raise PreparationError("lock paths must include project-root Cargo.lock for exact associations")
        locks = [regular_source(root, name) for name in lock_paths]
        lock_hashes = {name: digest(path) for name, path in zip(lock_paths, locks)}
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
            acquired[filename] = (source, checksum)

        # Staging prevents a valid early archive from publishing a partial closure
        # when a later archive or final verification fails.
        with tempfile.TemporaryDirectory(prefix=".rust-source-inputs-", dir=parent) as temporary:
            staged = Path(temporary) / "source-inputs"
            vendor = staged / Path(VENDOR_PATH).name
            retained = staged / Path(ARCHIVES_PATH).name
            vendor.mkdir(parents=True)
            retained.mkdir()
            modes = {}
            for (name, version), checksum in inputs.items():
                filename = f"{name}-{version}.crate"
                target = retained / filename
                shutil.copyfile(acquired[filename][0], target)
                target.chmod(0o644)
                modes[f"{ARCHIVES_PATH}/{filename}"] = 0o644
                modes.update(materialize_archive(target, vendor, name, version, checksum))
            evidence = verify_bundle(vendor, locks, archives=retained)
            for name, path in zip(lock_paths, locks):
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
            _directory(archives, "archive root")
            if {entry.name for entry in archives.iterdir()} != filenames:
                raise PreparationError("archive input inventory changed during preparation")
            for filename, (_source, checksum) in acquired.items():
                if digest(regular_source(archives, filename)) != checksum:
                    raise PreparationError("archive inputs changed during preparation")
            _directory(parent, "source destination parent")
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
    parser.add_argument("--lock", action="append", dest="lock_paths",
                        help="project-relative lock path; repeat to supply the complete union")
    parser.add_argument("--destination", type=Path,
                        help="fresh Cargo-only output directory; existing inputs are never replaced")
    options = parser.parse_args(arguments)
    try:
        evidence = vendor_sources(options.root, options.archives,
                                  lock_paths=tuple(options.lock_paths) if options.lock_paths else LOCK_PATHS,
                                  destination=options.destination)
    except PreparationError as error:
        parser.exit(1, f"Rust source vendoring failed: {error}\n")
    print(json.dumps({"schema": evidence["schema"], "package_count": evidence["package_count"],
                      "status": "verified", "frontend_external_source_closure_verified": False},
                     sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
