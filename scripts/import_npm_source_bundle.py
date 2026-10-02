#!/usr/bin/env python3
"""Import authenticated local npm archives for subsequent versioned source review.

The existing preparer copies originals and the verifier derives their inventory.
Published distributions alone do not prove preferred sources or build acceptance.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import tempfile
from typing import Any

import prepare_npm_source_bundle as prepare
from verify_npm_source_bundle import verify_bundle


ARCHIVES_NAME = "archives"
REPORT_NAME = "npm-source-closure.json"


class ImportPreparationError(ValueError):
    """Local validation or publication failed without executing an installer."""


def _path(path: Path, label: str) -> Path:
    path = Path(path).absolute()
    if ".." in path.parts:
        raise ImportPreparationError(f"{label}: parent traversal is prohibited")
    return path


def _new_destination(destination: Path) -> None:
    try:
        destination.lstat()
    except FileNotFoundError:
        return
    raise ImportPreparationError("npm source destination already exists; no inputs were replaced")


def import_bundle(lock: Path, archives: Path, destination: Path) -> dict[str, Any]:
    """Publish a fresh local archive layout only after exact verification succeeds.

    Args:
        lock: Authoritative npm v3 lockfile, never modified.
        archives: Exact previously acquired archive_filename inventory, including
            every optional, dev, and nested record. No cache or network is used.
        destination: New explicit output, normally source-inputs/npm. Its parent
            must exist without linked or reparse ancestors; output cannot be
            inside archives. Parent traversal and existing outputs are rejected.

    Returns:
        Independently derived verifier evidence, also written to
        npm-source-closure.json beside archives/. Original archive bytes, member
        modes, lock placements, supplied licenses and provenance are retained.
        Installation, preferred-source closure, frontend closure and OS-denied
        build/packaging acceptance remain explicitly unverified.

    Raises:
        ImportPreparationError: Missing, changed, tampered or unsafe inputs, or
            invalid output. Staging is removed on failure and no input is edited.

    This is preparation, not versioning, installation or compilation. Callers must
    keep the destination parent stable during publication; no claim is made about
    protecting against concurrent hostile filesystem ancestor replacement.
    """
    try:
        lock = prepare.regular_path(_path(lock, "lockfile"))
        archives = prepare.regular_path(_path(archives, "archive root"), directory=True)
        destination = _path(destination, "destination")
        parent = prepare.regular_path(destination.parent, directory=True)
        _new_destination(destination)
        if destination.is_relative_to(archives):
            raise ImportPreparationError("npm output must not be inside the acquired archive directory")
        original = verify_bundle(lock, archives)
        if not original["archive_count"]:
            raise ImportPreparationError("locked archive inventory is empty")

        # Complete prevalidation and staging keep a late optional-archive failure
        # from publishing an incomplete source closure.
        with tempfile.TemporaryDirectory(prefix=".npm-source-inputs-", dir=parent) as temporary:
            staged = Path(temporary) / "npm"
            staged.mkdir()
            copied = prepare.prepare_bundle(
                lock, staged / ARCHIVES_NAME, acquired_archives=archives, allow_network=False
            )
            verified = verify_bundle(lock, staged / ARCHIVES_NAME)
            if copied != original or verified != original or verify_bundle(lock, archives) != original:
                raise ImportPreparationError("canonical npm inputs changed during import")
            evidence = dict(verified)
            evidence["installation_verified"] = False
            evidence["build_and_packaging_acceptance_verified"] = False
            evidence["os_denied_acceptance_verified"] = False
            report = staged / REPORT_NAME
            with report.open("x", encoding="utf-8", newline="\n") as stream:
                stream.write(json.dumps(evidence, sort_keys=True, indent=2) + "\n")
            report.chmod(0o644)
            for record in evidence["packages"]:
                (staged / ARCHIVES_NAME / record["archive_filename"]).chmod(0o644)
            prepare.regular_path(parent, directory=True)
            _new_destination(destination)
            os.rename(staged, destination)
            return evidence
    except ImportPreparationError:
        raise
    except (OSError, ValueError) as error:
        raise ImportPreparationError(str(error)) from error


def main(arguments: list[str] | None = None) -> int:
    """Run local-only preparation and print a bounded success summary.

    Accepts --lock, --archives and --destination paths. Invalid input is diagnosed
    on stderr with exit status one. No npm, subprocess, network or Git is invoked.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lock", type=Path, required=True)
    parser.add_argument("--archives", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    options = parser.parse_args(arguments)
    try:
        evidence = import_bundle(options.lock, options.archives, options.destination)
    except ImportPreparationError as error:
        parser.exit(1, f"npm source import failed: {error}\n")
    print(json.dumps({
        "schema": evidence["schema"],
        "archive_count": evidence["archive_count"],
        "lock_record_count": evidence["lock_record_count"],
        "archives_verified": True,
        "preferred_source_closure_complete": False,
        "frontend_source_complete": False,
        "build_and_packaging_acceptance_verified": False,
        "os_denied_acceptance_verified": False,
    }, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
