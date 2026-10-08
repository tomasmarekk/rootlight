#!/usr/bin/env python3
"""Publish authenticated local Yarn distributions into the producer namespace.

The pure verifier rederives originals, expanded members and producer association.
Publication is preparation, not installation or complete preferred-source closure.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path, PurePosixPath
import sys
import tempfile

import verify_lightning_yarn_sources as verifier


class YarnImportError(ValueError):
    """Local verification or publication rejected an incomplete or unsafe input."""


def _fresh(project: Path, prepared: Path, destination: Path) -> Path:
    if ".." in Path(destination).parts:
        raise YarnImportError("destination parent traversal is prohibited")
    destination = Path(os.path.abspath(destination))
    expected = project / verifier.BUNDLE_PATH
    if destination != expected:
        raise YarnImportError("destination must be the declared producer Yarn namespace")
    verifier.producer.safe_path(destination.parent, directory=True)
    if os.path.lexists(destination):
        raise YarnImportError("Yarn source destination already exists; no inputs were replaced")
    if destination.is_relative_to(prepared) or prepared.is_relative_to(destination):
        raise YarnImportError("prepared inputs and output namespace must not overlap")
    return destination


def _copy_inventory(prepared: Path, staged: Path, evidence: dict) -> None:
    for relative in sorted(evidence["directories"], key=lambda value: (value.count("/"), value)):
        target = staged.joinpath(*PurePosixPath(relative).parts)
        target.mkdir()
        target.chmod(0o755)
    for relative, record in evidence["files"].items():
        parts = PurePosixPath(relative).parts
        payload = verifier.producer.read_bytes(prepared.joinpath(*parts), record["bytes"])
        if verifier._record(payload) != {key: record[key] for key in ("bytes", "sha256")}:
            raise YarnImportError("prepared Yarn bytes changed during copy")
        target = staged.joinpath(*parts)
        with target.open("xb") as stream:
            stream.write(payload)
        target.chmod(record["mode"])
    report = staged / verifier.REPORT_NAME
    with report.open("xb") as stream:
        stream.write(verifier.producer.json_bytes(evidence))
    report.chmod(0o644)


def import_bundle(project: Path, prepared: Path, destination: Path) -> dict:
    """Atomically publish a fresh fixed namespace after full local reverification.

    Args:
        project: Existing project containing the independently authenticated authored
            producer tree and its unchanged package manifest and Yarn-v1 lock.
        prepared: Complete local archives/, packages/ and canonical report inventory.
            Archives, raw member modes, supplied licenses and provenance are retained.
        destination: New exact project/source-inputs/producer-dependencies/
            lightningcss-<pinned-commit>/yarn-originals directory. Its existing parent
            and all input ancestors must be stable and nonlinked; traversal, overlap,
            reparse inputs and any existing output are rejected.

    Returns:
        Independently rederived verifier evidence, also retained byte-exact in the
        destination. All installation, preferred-source, producer rebuild, signature,
        licensing-completeness and build/packaging acceptance claims remain false.

    Raises:
        YarnImportError, shared typed verification errors or OSError on missing,
        extra, unsafe or changed inputs. Temporary staging is removed on failure.
        Existing source inputs and outputs are never intentionally replaced.

    No network, dependency cache, package execution, subprocess or Git operation is
    used. Callers retain stable ancestors during the final absence check and rename;
    this is not hostile-concurrency isolation or a crash-durability guarantee.
    """
    project = verifier.producer.safe_path(project, directory=True)
    prepared = verifier.producer.safe_path(prepared, directory=True)
    destination = _fresh(project, prepared, destination)
    original = verifier.verify_bundle(project, prepared)
    if not original["archive_count"]:
        raise YarnImportError("locked Yarn archive inventory is empty")
    with tempfile.TemporaryDirectory(prefix=".yarn-source-inputs-", dir=destination.parent) as temporary:
        staged = Path(temporary) / destination.name
        staged.mkdir()
        _copy_inventory(prepared, staged, original)
        verified = verifier.verify_bundle(project, staged)
        repeated = verifier.verify_bundle(project, prepared)
        if (verifier.producer.json_bytes(verified) != verifier.producer.json_bytes(original)
                or verifier.producer.json_bytes(repeated) != verifier.producer.json_bytes(original)):
            raise YarnImportError("canonical Yarn inputs changed during import")
        _fresh(project, prepared, destination)
        os.rename(staged, destination)
    return original


def main(arguments: list[str] | None = None) -> int:
    """Run local-only --project/--prepared/--destination preparation.

    Returns zero and compact independently derived counts on success, or one with
    a source-free stderr diagnostic on input/publication failure. No installer runs.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("project", "prepared", "destination"):
        parser.add_argument("--" + name, type=Path, required=True)
    options = parser.parse_args(arguments)
    try:
        evidence = import_bundle(options.project, options.prepared, options.destination)
    except (ValueError, OSError, RecursionError):
        print("producer Yarn source import failed; no acceptance claim", file=sys.stderr)
        return 1
    print(verifier.producer.json_bytes({
        "schema": evidence["schema"],
        "archive_count": evidence["archive_count"],
        "member_count": evidence["member_count"],
        "original_archives_verified": True,
        **{key: False for key in verifier.FALSE_CLAIMS},
    }).decode("utf-8"), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
