#!/usr/bin/env python3
"""Acquire locked npm distribution inputs before compilation or installation.

This controlled preparation authenticates all original archives, including
optional platforms. It does not prove the preferred-source closure of native,
WASI, or bundled implementations, and never invokes npm or project code.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import shutil
import stat
from urllib.request import HTTPRedirectHandler, Request, build_opener

from verify_npm_source_bundle import archive_filename, locked_archives, verify_bundle


class PreparationError(ValueError):
    """A controlled preparation input or acquisition violates the bundle contract."""


def regular_path(path: Path, *, directory: bool = False) -> Path:
    """Require an existing regular input without symlink or reparse ancestors.

    Raises PreparationError for a linked or wrongly typed path. Performs no writes.
    """
    path = path.absolute()
    for candidate in (path, *path.parents):
        metadata = candidate.lstat()
        if stat.S_ISLNK(metadata.st_mode) or bool(
            getattr(metadata, "st_file_attributes", 0)
            & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
        ):
            raise PreparationError("linked acquisition input or output ancestor")
    valid = stat.S_ISDIR(path.stat().st_mode) if directory else stat.S_ISREG(path.stat().st_mode)
    if not valid:
        raise PreparationError("acquisition input has the wrong file type")
    return path


class RegistryRedirectHandler(HTTPRedirectHandler):
    """Keep controlled acquisition redirects inside approved registry archive URLs."""

    def redirect_request(self, request, response, code, message, headers, new_url):
        """Reject a non-registry redirect before issuing its request."""
        archive_filename(new_url)
        return super().redirect_request(request, response, code, message, headers, new_url)


def download_archive(resolved: str, destination: Path) -> None:
    """Acquire one approved archive without extracting or executing its contents.

    The caller must subsequently authenticate every acquired byte against its
    original lock integrity. Network errors and unsafe redirects fail preparation.
    """
    archive_filename(resolved)
    opener = build_opener(RegistryRedirectHandler())
    request = Request(resolved, headers={"User-Agent": "rootlight-source-preparation/1"})
    with opener.open(request, timeout=60) as source, destination.open("xb") as output:
        archive_filename(source.geturl())
        shutil.copyfileobj(source, output, length=1024 * 1024)


def prepare_bundle(
    lock: Path,
    destination: Path,
    *,
    acquired_archives: Path | None = None,
    allow_network: bool = False,
) -> dict:
    """Prepare a fresh exact archive directory, then independently verify it.

    Args:
        lock: Canonical npm v3 lockfile; never modified.
        destination: New archive directory, never an existing output.
        acquired_archives: Optional directory of previously acquired original
            archives using archive_filename names, not an npm install/cache.
        allow_network: Explicit authorization for this pre-build acquisition only.

    Returns:
        Deterministic original-archive verification evidence. Preferred-source and
        full frontend closure remain unproven, exactly as the verifier records.

    Raises:
        PreparationError: Inputs, mode or output are invalid, or the lock changes.
        ValueError/OSError: Acquisition or locked archive verification fails.
        Failed outputs are never reported as verified; choose a fresh destination
        after correcting their cause. No npm, subprocess or project execution occurs.
    """
    if (acquired_archives is None) == (not allow_network):
        raise PreparationError("choose acquired original archives or explicit --allow-network")
    lock = regular_path(Path(lock))
    initial_lock_hash = hashlib.sha256(lock.read_bytes()).hexdigest()
    identities = locked_archives(lock)
    if not identities:
        raise PreparationError("locked archive inventory is empty")
    destination = Path(destination).absolute()
    if destination.exists() or destination.is_symlink():
        raise PreparationError("archive destination already exists; choose a fresh directory")
    for ancestor in destination.parents:
        if ancestor.exists() or ancestor.is_symlink():
            regular_path(ancestor, directory=True)
    source_root = (regular_path(Path(acquired_archives), directory=True)
                   if acquired_archives is not None else None)
    if source_root is not None and destination.resolve().is_relative_to(source_root.resolve()):
        raise PreparationError("archive output must not be inside the acquired input directory")
    destination.mkdir(parents=True)

    def acquire(identity: dict) -> None:
        target = destination / identity["archive_filename"]
        if source_root is not None:
            source = regular_path(source_root / identity["archive_filename"])
            with source.open("rb") as input_stream, target.open("xb") as output_stream:
                shutil.copyfileobj(input_stream, output_stream, length=1024 * 1024)
        else:
            download_archive(identity["resolved"], target)

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(acquire, identities))
    evidence = verify_bundle(lock, destination)
    if evidence["lockfile_sha256"] != initial_lock_hash:
        raise PreparationError("canonical lockfile changed during preparation")
    return evidence


def main() -> None:
    """Prepare authenticated archive inputs through an explicit, separate CLI phase."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lock", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--from-archives", type=Path)
    mode.add_argument("--allow-network", action="store_true")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    try:
        output = arguments.output.absolute() if arguments.output else None
        if output and output.resolve().is_relative_to(arguments.destination.resolve()):
            raise PreparationError("evidence output must be outside the exact archive inventory")
        evidence = prepare_bundle(arguments.lock, arguments.destination,
                                  acquired_archives=arguments.from_archives,
                                  allow_network=arguments.allow_network)
        encoded = json.dumps(evidence, sort_keys=True, indent=2) + "\n"
        if output:
            with output.open("x", encoding="utf-8", newline="\n") as stream:
                stream.write(encoded)
        else:
            print(encoded, end="")
    except (OSError, ValueError) as error:
        parser.exit(1, f"npm original archive preparation failed: {error}\n")


if __name__ == "__main__":
    main()
