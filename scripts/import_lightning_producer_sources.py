#!/usr/bin/env python3
"""Publish a complete local Lightning CSS authored tree after reverification.

This separate preparation preserves original archives and caller provenance.
It never acquires dependencies, installs packages or executes producer sources.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import sys
import tarfile
import tempfile

import verify_lightning_producer_sources as verifier


class ProducerImportError(ValueError):
    """Local preparation cannot publish an authenticated fresh producer namespace."""


def _fresh(project: Path, destination: Path) -> Path:
    if ".." in Path(destination).parts:
        raise ProducerImportError("destination parent traversal is prohibited")
    destination = Path(os.path.abspath(destination))
    parent = verifier.safe_path(destination.parent, directory=True)
    expected = project / "source-inputs" / "producers" / (verifier.PACKAGE + "-" + verifier.COMMIT)
    if destination != expected or parent != expected.parent:
        raise ProducerImportError("destination must be the declared project producer namespace")
    if os.path.lexists(destination):
        raise ProducerImportError("producer destination already exists; no inputs were replaced")
    return destination


def import_bundle(project: Path, prepared: Path, capture: Path, destination: Path) -> dict:
    """Atomically publish a new declared producer namespace from local inputs.

    Args:
        project: Existing project with independently verified all-local npm inputs.
        prepared: Exact original three-file output of separate source acquisition.
        capture: Caller-observed publisher capture matching retained metadata.
        destination: New source-inputs/producers/lightningcss-<commit> directory;
            its nonlinked parent must already exist. No other output is accepted.

    Returns:
        Independently derived source-tree evidence with all transitive, preferred,
        binary reproduction and build/packaging acceptance claims false.

    Raises:
        ProducerImportError or shared typed input errors for malformed, changed,
        unsafe or unauthenticated inputs. Temporary staging is removed on failure.
        Existing project inputs and source destinations are never replaced.

    No network, subprocess, package installation, source execution or Git operation
    occurs. Callers must retain stable ancestors; hostile concurrency is not isolated.
    """
    project = verifier.safe_path(project, directory=True)
    destination = _fresh(project, destination)
    evidence, blobs, files = verifier.validate_prepared(project, prepared, capture)
    with tempfile.TemporaryDirectory(prefix=".producer-source-inputs-", dir=destination.parent) as temporary:
        staged = Path(temporary) / destination.name
        staged.mkdir()
        tree = staged / "tree"
        tree.mkdir()
        modes = {}
        for record in evidence["source_inventory"]["members"]:
            relative = record["path"].removeprefix(verifier.ROOT + "/")
            if record["path"] == verifier.ROOT:
                continue
            target = tree.joinpath(*PurePosixPath(relative).parts)
            if record["kind"] == "directory":
                target.mkdir(parents=True, exist_ok=True)
                target.chmod(0o755)
            else:
                modes[relative] = 0o755 if record["archive_mode"] & 0o111 else 0o644
        for relative, payload in files.items():
            target = tree.joinpath(*PurePosixPath(relative).parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open("xb") as stream:
                stream.write(payload)
            # Git carries executable intent, not group write bits; raw archive
            # modes remain verbatim in the independently derived source inventory.
            target.chmod(modes[relative])
        for name, payload in {**blobs, "source-tree.json": verifier.json_bytes(evidence)}.items():
            with (staged / name).open("xb") as stream:
                stream.write(payload)
            (staged / name).chmod(0o644)
        verified = verifier.verify_bundle(project, staged)
        repeated, repeated_blobs, repeated_files = verifier.validate_prepared(project, prepared, capture)
        if (verifier.json_bytes(verified) != verifier.json_bytes(evidence)
                or verifier.json_bytes(repeated) != verifier.json_bytes(evidence)
                or repeated_blobs != blobs or repeated_files != files):
            raise ProducerImportError("producer inputs changed during import")
        _fresh(project, destination)
        os.rename(staged, destination)
    return evidence


def main(arguments: list[str] | None = None) -> int:
    """Run local-only --project/--prepared/--capture/--destination publication."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("project", "prepared", "capture", "destination"):
        parser.add_argument("--" + name, type=Path, required=True)
    options = parser.parse_args(arguments)
    try:
        report = import_bundle(options.project, options.prepared, options.capture, options.destination)
    except (OSError, ValueError, KeyError, TypeError, EOFError, RecursionError, tarfile.TarError):
        print("producer import failed; no acceptance claim", file=sys.stderr)
        return 1
    print(json.dumps({"member_count": report["source_inventory"]["member_count"],
                      "preferred_source_closure_complete": False}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
