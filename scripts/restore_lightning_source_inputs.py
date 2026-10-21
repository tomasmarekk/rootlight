#!/usr/bin/env python3
"""Explicitly restore authenticated source-directory and file-mode metadata after Git.

Reports provide untrusted staging hints only. Strict archive/lock verification must
authenticate a temporary clone before any original metadata can be changed.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path, PurePosixPath
import stat
import sys
import tarfile
import tempfile

import verify_lightning_producer_sources as producer
import verify_lightning_yarn_sources as yarn
from verify_native_producer_sources import REVIEWED_PRODUCERS, native_source_inputs


# These preparation-only budgets bound untrusted hints before archive authentication.
# They do not change the strict verifiers' archive policies or product runtime limits.
MAX_HINT_PATH_BYTES = 4096
MAX_HINT_DIRECTORY_COUNT = yarn.MAX_TOTAL_MEMBERS + yarn.yarn.MAX_STANZAS
MAX_HINT_PATH_IDENTITIES = 2 * MAX_HINT_DIRECTORY_COUNT + 1


class SourceRestorationError(ValueError):
    """Source metadata cannot be restored without authenticated unchanged bytes."""


def _require(condition: bool, category: str) -> None:
    if not condition:
        raise SourceRestorationError(category)


def _relative(value: str, aliases: dict[str, str]) -> str:
    _require(isinstance(value, str) and len(value.encode("utf-8")) <= MAX_HINT_PATH_BYTES,
             "hint_path_byte_limit")
    producer.local.original._safe_path(value, "restoration hint")
    producer.local.original._path_aliases(value, aliases)
    _require(len(aliases) <= MAX_HINT_PATH_IDENTITIES, "hint_path_identity_limit")
    return value


def _parents(paths: set[str], existing: set[str] | None = None) -> set[str]:
    directories = set() if existing is None else set(existing)
    _require(len(directories) <= MAX_HINT_DIRECTORY_COUNT, "hint_directory_limit")
    for path in paths:
        for parent in PurePosixPath(path).parents:
            if str(parent) != ".":
                directories.add(str(parent))
                _require(len(directories) <= MAX_HINT_DIRECTORY_COUNT, "hint_directory_limit")
    return directories


def _producer_hints(report: dict) -> tuple[set[str], dict[str, int]]:
    _require(isinstance(report, dict) and report.get("schema") == "rootlight.lightning-producer-tree/1",
             "unsupported_producer_report")
    inventory = report.get("source_inventory")
    _require(isinstance(inventory, dict) and isinstance(inventory.get("members"), list)
             and len(inventory["members"]) <= producer.MAX_MEMBERS, "invalid_producer_member_hints")
    files = {name: 0o644 for name in producer.STORED_NAMES - {"tree"}}
    directories, aliases, seen = {"tree"}, {}, set()
    for row in inventory["members"]:
        _require(isinstance(row, dict), "invalid_producer_member_hint")
        path = _relative(row.get("path"), aliases)
        _require(path not in seen and (path == producer.ROOT or path.startswith(producer.ROOT + "/")),
                 "invalid_producer_member_path")
        seen.add(path)
        _require(row.get("kind") in ("file", "directory") and type(row.get("archive_mode")) is int
                 and 0 <= row["archive_mode"] <= 0o777, "invalid_producer_mode_hint")
        if path == producer.ROOT:
            _require(row["kind"] == "directory", "invalid_producer_root_hint")
            continue
        relative = "tree/" + path.removeprefix(producer.ROOT + "/")
        if row["kind"] == "directory":
            directories.add(relative)
            _require(len(directories) <= MAX_HINT_DIRECTORY_COUNT, "hint_directory_limit")
        else:
            files[relative] = 0o755 if row["archive_mode"] & 0o111 else 0o644
    directories = _parents(set(files) | directories, directories)
    _require(not directories.intersection(files), "file_is_hint_directory")
    return directories, files


def _yarn_hints(report: dict) -> tuple[set[str], dict[str, int]]:
    _require(isinstance(report, dict)
             and report.get("schema") == "rootlight.lightning-yarn-source-closure/1",
             "unsupported_yarn_report")
    rows, directory_rows = report.get("files"), report.get("directories")
    _require(isinstance(rows, dict) and isinstance(directory_rows, list)
             and len(rows) <= yarn.MAX_TOTAL_MEMBERS + yarn.yarn.MAX_STANZAS,
             "invalid_yarn_inventory_hints")
    _require(len(directory_rows) <= MAX_HINT_DIRECTORY_COUNT, "hint_directory_limit")
    aliases, directories, files = {}, set(), {}
    for path in directory_rows:
        path = _relative(path, aliases)
        _require(path not in directories, "duplicate_directory_hint")
        directories.add(path)
    for path, record in rows.items():
        path = _relative(path, aliases)
        _require(isinstance(record, dict) and type(record.get("mode")) is int
                 and record["mode"] in (0o644, 0o755), "invalid_yarn_mode_hint")
        files[path] = record["mode"]
    _require(yarn.REPORT_NAME not in files and not directories.intersection(files),
             "file_is_hint_directory")
    files[yarn.REPORT_NAME] = 0o644
    _require(_parents(set(files) | directories) <= directories, "missing_directory_hint_parent")
    return directories, files


def _namespace(project: Path, parent_relative: str, children: tuple[str, ...]) -> Path | None:
    parent = project / parent_relative
    # A missing optional parent is different from a present empty or partial tree.
    for path in reversed((parent, *parent.parents)):
        if os.path.lexists(path):
            producer.safe_path(path, directory=True)
    if not os.path.lexists(parent):
        return None
    current = parent
    for name in children:
        names = {child.name for child in current.iterdir()}
        if current == parent and parent_relative == "source-inputs/producers":
            _require(name in names and names <= {name, *REVIEWED_PRODUCERS},
                     "namespace_inventory_differs")
            native_source_inputs(project)
        else:
            _require(names == {name}, "namespace_inventory_differs")
        current = producer.safe_path(current / name, directory=True)
    return current


def _walk_error(error: OSError) -> None:
    raise SourceRestorationError("namespace_traversal_failed") from error


def _read_limit(relative: str, is_yarn: bool) -> int:
    if is_yarn:
        return (yarn.MAX_REPORT if relative == yarn.REPORT_NAME else
                yarn.MAX_COMPRESSED if relative.startswith("archives/") else yarn.MAX_EXPANDED)
    return (producer.MAX_COMPRESSED if relative == producer.ARCHIVE_NAME else
            producer.MAX_EXPANDED if relative.startswith("tree/") else producer.MAX_JSON)


def _snapshot(bundle: Path, is_yarn: bool, nodes: int, staged: Path | None = None) -> dict:
    files, directories, aliases, count, expanded_bytes, archive_count = {}, {}, {}, 1, 0, 0
    root_stat = producer.safe_path(bundle, directory=True).lstat()
    directories[""] = (stat.S_IMODE(root_stat.st_mode), root_stat.st_dev, root_stat.st_ino)
    for current, dirs, names in os.walk(bundle, followlinks=False, onerror=_walk_error):
        current = producer.safe_path(Path(current), directory=True)
        for name in sorted(dirs):
            child = producer.safe_path(current / name, directory=True)
            relative = _relative(child.relative_to(bundle).as_posix(), aliases)
            metadata = child.lstat()
            directories[relative] = (stat.S_IMODE(metadata.st_mode), metadata.st_dev, metadata.st_ino)
            count += 1
            _require(count <= nodes, "observed_node_limit")
            if staged is not None:
                staged.joinpath(*PurePosixPath(relative).parts).mkdir()
        for name in sorted(names):
            child = producer.safe_path(current / name, directory=False)
            relative = _relative(child.relative_to(bundle).as_posix(), aliases)
            count += 1
            _require(count <= nodes, "observed_node_limit")
            metadata = child.lstat()
            payload = producer.read_bytes(child, _read_limit(relative, is_yarn))
            if relative.startswith("packages/" if is_yarn else "tree/"):
                expanded_bytes += len(payload)
                _require(expanded_bytes <= (yarn.MAX_TOTAL_EXPANDED if is_yarn else producer.MAX_EXPANDED),
                         "observed_expanded_byte_limit")
            if is_yarn and relative.startswith("archives/"):
                archive_count += 1
                _require(archive_count <= yarn.yarn.MAX_STANZAS, "observed_archive_count_limit")
            files[relative] = {"bytes": len(payload), "sha256": producer.digest(payload),
                               "mode": stat.S_IMODE(metadata.st_mode),
                               "identity": producer.local.original._file_stamp(metadata)}
            if staged is not None:
                target = staged.joinpath(*PurePosixPath(relative).parts)
                with target.open("xb") as stream:
                    stream.write(payload)
                target.chmod(stat.S_IMODE(metadata.st_mode))
    return {"files": files, "directories": directories}


def _restore_namespace(project: Path, bundle: Path, *, is_yarn: bool,
                       restore_file_modes: bool) -> dict:
    report_name = yarn.REPORT_NAME if is_yarn else "source-tree.json"
    payload = producer.read_bytes(bundle / report_name, producer.MAX_JSON)
    hints = producer.local.original._parse_json(payload, "restoration report")
    _require(producer.json_bytes(hints) == payload, "noncanonical_report_hint")
    directories, modes = (_yarn_hints if is_yarn else _producer_hints)(hints)
    nodes = len(directories) + len(modes) + 1
    verify = yarn.verify_bundle if is_yarn else producer.verify_bundle
    with tempfile.TemporaryDirectory(prefix=".lightning-source-metadata-") as temporary:
        staged = Path(temporary) / "namespace"
        staged.mkdir()
        before = _snapshot(bundle, is_yarn, nodes, staged)
        for relative in sorted(directories, key=lambda value: (value.count("/"), value)):
            target = staged.joinpath(*PurePosixPath(relative).parts)
            if not os.path.lexists(target):
                target.mkdir()
        for relative, mode in modes.items():
            producer.safe_path(staged.joinpath(*PurePosixPath(relative).parts), directory=False).chmod(mode)
        evidence = verify(project, staged)
        # Hint authenticity comes only from full archive/lock/report reverification.
        _require(_snapshot(bundle, is_yarn, nodes) == before, "source_frame_changed_before_restoration")
        if os.name != "nt" and not restore_file_modes:
            _require(all(record["mode"] == modes.get(path) for path, record in before["files"].items()),
                     "file_modes_require_explicit_restoration")
        missing = directories - set(before["directories"])
        changed_modes = []
        for relative in sorted(missing, key=lambda value: (value.count("/"), value)):
            target = bundle.joinpath(*PurePosixPath(relative).parts)
            producer.safe_path(target.parent, directory=True)
            _require(not os.path.lexists(target), "missing_directory_appeared")
            target.mkdir(mode=0o755)
            target.chmod(0o755)
        if os.name != "nt" and restore_file_modes:
            for relative, mode in sorted(modes.items()):
                if before["files"][relative]["mode"] != mode:
                    target = producer.safe_path(bundle.joinpath(*PurePosixPath(relative).parts), directory=False)
                    _require(producer.digest(producer.read_bytes(target, _read_limit(relative, is_yarn)))
                             == before["files"][relative]["sha256"], "file_changed_before_mode_restoration")
                    target.chmod(mode, follow_symlinks=False)
                    changed_modes.append(relative)
        repeated = verify(project, bundle)
        _require(producer.json_bytes(repeated) == producer.json_bytes(evidence),
                 "restored_evidence_differs")
        after = _snapshot(bundle, is_yarn, nodes)
        _require(set(after["files"]) == set(before["files"])
                 and all({key: record[key] for key in ("bytes", "sha256")}
                         == {key: after["files"][path][key] for key in ("bytes", "sha256")}
                         for path, record in before["files"].items()), "source_bytes_changed")
        _require(set(after["directories"]) == set(before["directories"]) | missing
                 and all(after["directories"][path] == record
                         for path, record in before["directories"].items()), "existing_directory_changed")
    return {"path": bundle.relative_to(project).as_posix(), "restored_directories": sorted(missing),
            "restored_file_modes": changed_modes, "regular_files": len(before["files"]),
            "all_regular_file_bytes_conserved": True, "strict_original_verification_passed": True}


def restore_project(project: Path, *, restore_file_modes: bool = False) -> dict:
    """Restore only authenticated missing directories and optionally POSIX file modes.

    Args:
        project: Existing project with the fixed optional producer/Yarn namespaces.
            Shared-parent Flux/Raffia siblings must match their pinned catalogs
            and are never modified. Only complete namespace-parent absence is
            optional; partial owned trees reject.
        restore_file_modes: Permit archive-derived 755/644 normalization of existing
            regular files on POSIX. Without it, existing POSIX file modes must match.
            Windows permission/executable restoration is not claimed.

    Returns:
        Compact per-namespace restoration evidence, with source bytes conserved and
        strict original verification passed. No preferred/build acceptance is claimed.

    Raises:
        SourceRestorationError, shared input errors or OSError for unauthenticated,
        malformed, missing, extra, linked, changed or unsupported inputs. Regular
        source files are never created, replaced, written or deleted.

    The producer is restored before Yarn, whose strict verifier requires its complete
    producer association. Stable unlinked ancestors and nonhostile concurrency are
    required. This is not crash atomicity: a later I/O/input failure may leave only
    authenticated directories/modes already restored; safe reruns reverify them.
    Existing directory permissions are preserved. No acquisition, subprocess, Git,
    cache, installation or package/source execution occurs. Export/verifier defaults
    remain strict and never call this explicit preparation implicitly.
    Utility-only hint budgets limit UTF-8 paths to the Yarn preflight's 4096 bytes,
    directory identities to the existing total-member plus lock-stanza cardinality,
    and file/directory alias prefixes to twice that cardinality plus the root.
    These bound staging preparation, not product runtime or strict archive limits.
    """
    project = producer.safe_path(Path(project), directory=True)
    producer_bundle = _namespace(project, "source-inputs/producers", (producer.ROOT,))
    yarn_bundle = _namespace(project, "source-inputs/producer-dependencies", (producer.ROOT, "yarn-originals"))
    _require(yarn_bundle is None or producer_bundle is not None, "yarn_requires_producer_namespace")
    rows = []
    for bundle, is_yarn in ((producer_bundle, False), (yarn_bundle, True)):
        if bundle is not None:
            rows.append(_restore_namespace(project, bundle, is_yarn=is_yarn,
                                           restore_file_modes=restore_file_modes))
    return {"schema": "rootlight.lightning-source-metadata-restoration/1", "namespaces": rows,
            "source_file_bytes_written": False, "package_code_executed": False,
            "preferred_source_closure_complete": False, "os_denied_acceptance_verified": False,
            "build_and_packaging_acceptance_verified": False}


def main(arguments: list[str] | None = None) -> int:
    """Run explicit local metadata preparation; return zero on proof or one on error.

    Failure diagnostics contain no source contents. --restore-file-modes explicitly
    authorizes POSIX metadata normalization but never source-byte modifications.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--restore-file-modes", action="store_true")
    options = parser.parse_args(arguments)
    try:
        result = restore_project(options.project, restore_file_modes=options.restore_file_modes)
    except (OSError, ValueError, KeyError, TypeError, EOFError, RecursionError, tarfile.TarError):
        print("source metadata restoration failed; no acceptance claim", file=sys.stderr)
        return 1
    print(producer.json_bytes(result).decode("utf-8"), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
