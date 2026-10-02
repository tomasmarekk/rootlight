#!/usr/bin/env python3
"""Install a canonical frontend lock from authenticated versioned npm archives.

The all-local canonical lock retains authored relative archive paths. The caller
must deny network access; derived cache output is not OS-denied acceptance.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
from typing import Any

from verify_local_npm_sources import verify_local_bundle


FRONTEND = "apps/rootlight-web/frontend/"
MANIFEST = "rust-source-bundle.json"
REPORT = "source-inputs/npm/npm-source-closure.json"
ARCHIVES = "source-inputs/npm/archives"
UPSTREAM_LOCK = "source-inputs/npm/upstream-package-lock.json"
LOCAL_MANIFEST = "source-inputs/npm/local-distributions.json"
LOCAL_DISTRIBUTIONS = "source-inputs/npm/local-distributions"
NODE_VERSION = "v24.11.1"
NPM_VERSION = "11.6.2"
_FALSE_REPORT_FIELDS = (
    "installation_verified", "build_and_packaging_acceptance_verified",
    "os_denied_acceptance_verified",
)
_HIDDEN_FILES = {".gitignore", ".npmrc", ".prettierignore", ".prettierrc.json"}
_GENERATED = {"node_modules", "dist", "coverage", "test-results", "playwright-report"}
_SOURCE_SUFFIXES = {".ts", ".tsx", ".js", ".mjs", ".cjs", ".json", ".html", ".css", ".svg", ".md"}


class FrontendInstallationError(ValueError):
    """Installation failed without an acquisition fallback or acceptance claim."""


def _pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise FrontendInstallationError("duplicate JSON key")
        result[key] = value
    return result


def _json(payload: bytes) -> Any:
    return json.loads(payload, object_pairs_hook=_pairs)


def _regular(path: Path, *, directory: bool = False) -> Path:
    path = Path(path).absolute()
    if ".." in path.parts:
        raise FrontendInstallationError("parent traversal is prohibited")
    for ancestor in (path, *path.parents):
        metadata = ancestor.lstat()
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0):
            raise FrontendInstallationError("linked or reparse input/output ancestor")
    metadata = path.stat()
    if not (stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode)):
        raise FrontendInstallationError("input/output has the wrong file type")
    return path


def _stamp(path: Path) -> tuple[int, int, int, int, int]:
    metadata = path.stat()
    return metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns, stat.S_IMODE(metadata.st_mode)


def _read(path: Path) -> bytes:
    path = _regular(path)
    before = _stamp(path)
    payload = path.read_bytes()
    if _stamp(_regular(path)) != before or len(payload) != before[2]:
        raise FrontendInstallationError("input changed while reading")
    return payload


def _relative(value: str) -> PurePosixPath:
    if not isinstance(value, str) or not value or "\\" in value or ":" in value or any(ord(character) < 32 for character in value):
        raise FrontendInstallationError("invalid manifest source path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in value.split("/")):
        raise FrontendInstallationError("escaping or noncanonical manifest source path")
    return path


def _frontend_source(name: str) -> bool:
    if not name.startswith(FRONTEND):
        return False
    relative = _relative(name[len(FRONTEND):])
    if any(part in _GENERATED for part in relative.parts) or relative.name == "npm-shrinkwrap.json":
        raise FrontendInstallationError("generated frontend file is in the source manifest")
    if any(part.startswith(".") and part not in _HIDDEN_FILES for part in relative.parts):
        raise FrontendInstallationError("private or unsupported hidden frontend source")
    if relative.name not in _HIDDEN_FILES and relative.suffix not in _SOURCE_SUFFIXES:
        raise FrontendInstallationError("unsupported frontend source file type")
    return True


def _directory_ids(paths: list[Path]) -> dict[Path, tuple[int, int]]:
    result = {}
    for path in paths:
        for directory in (path, *path.parents):
            _regular(directory, directory=True)
            metadata = directory.stat()
            result[directory] = metadata.st_dev, metadata.st_ino
    return result


def _npm_inventory(source_root: Path) -> dict[str, tuple[str, int, int]]:
    observed = {}
    pending = [source_root / ARCHIVES, source_root / LOCAL_DISTRIBUTIONS]
    while pending:
        directory = _regular(pending.pop(), directory=True)
        metadata = directory.stat()
        observed[directory.relative_to(source_root).as_posix()] = ("directory", metadata.st_dev, metadata.st_ino)
        for entry in directory.iterdir():
            metadata = entry.lstat()
            if stat.S_ISDIR(metadata.st_mode):
                _regular(entry, directory=True)
                pending.append(entry)
            else:
                _regular(entry)
                observed[entry.relative_to(source_root).as_posix()] = ("file", metadata.st_dev, metadata.st_ino)
    return observed


def _environment(work: Path, cache: Path, node: Path) -> dict[str, str]:
    home, temporary = work / "home", work / "tmp"
    home.mkdir()
    temporary.mkdir()
    user, global_config = work / "user.npmrc", work / "global.npmrc"
    user.touch(exist_ok=False)
    global_config.touch(exist_ok=False)
    system = {key: os.environ[key] for key in ("SystemRoot", "WINDIR") if key in os.environ}
    path = [str(node.parent)]
    if os.name == "nt":
        path.append(str(Path(system.get("SystemRoot", r"C:\Windows")) / "System32"))
    else:
        path.extend(("/usr/bin", "/bin"))
    return {
        **system, "PATH": os.pathsep.join(path), "HOME": str(home), "USERPROFILE": str(home),
        "TMP": str(temporary), "TEMP": str(temporary), "TMPDIR": str(temporary),
        "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "CI": "true",
        "NPM_CONFIG_CACHE": str(cache), "NPM_CONFIG_USERCONFIG": str(user),
        "NPM_CONFIG_GLOBALCONFIG": str(global_config), "NPM_CONFIG_OFFLINE": "true",
        "NPM_CONFIG_IGNORE_SCRIPTS": "true", "NPM_CONFIG_AUDIT": "false",
        "NPM_CONFIG_FUND": "false", "NPM_CONFIG_UPDATE_NOTIFIER": "false",
        "NPM_CONFIG_PREFER_ONLINE": "false", "NPM_CONFIG_FETCH_RETRIES": "0",
    }


def install_frontend(source_root: Path, work: Path, cache: Path, node: Path, npm_cli: Path) -> dict[str, Any]:
    """Stage authenticated frontend files and perform an archive-only npm install.

    Args:
        source_root: Immutable exported project with its source manifest, canonical
            frontend inputs, local archive projection, upstream lock, modification
            provenance, and exact versioned npm report.
        work: Existing empty non-linked directory outside the exported sources.
        cache: Separate existing empty non-linked external npm cache directory.
        node: Explicit declared Node executable; only its exact version is accepted.
        npm_cli: Explicit declared npm CLI JavaScript entry point, run through node.

    Returns:
        Bounded input hashes/counts and npm-ci completion evidence. Frontend files
        remain at ``work/apps/rootlight-web/frontend`` so canonical relative file
        resolutions reach the staged archives without rewriting the lock. Installation
        identity, preferred sources, OS denial and packaging acceptance remain false.

    Raises:
        FrontendInstallationError/OSError: Inputs, paths, version probes, subprocess
            execution or immutable rechecks fail. No existing outputs are deleted.
        Failed fresh outputs are retained; use new empty outputs for another attempt.

    The caller must establish OS-denied networking before calling this function.
    This does not implement that boundary, run install hooks or build the frontend.
    Stable input/output ancestors are required, not hostile-concurrency protection.
    """
    source_root = _regular(source_root, directory=True)
    work = _regular(work, directory=True)
    cache = _regular(cache, directory=True)
    for output in (work, cache):
        if output.is_relative_to(source_root) or source_root.is_relative_to(output):
            raise FrontendInstallationError("outputs must be external to immutable sources")
        if any(output.iterdir()):
            raise FrontendInstallationError("work and cache must initially be empty")
    if work.is_relative_to(cache) or cache.is_relative_to(work):
        raise FrontendInstallationError("work and cache must be separate")
    node, npm_cli = _regular(node), _regular(npm_cli)
    parents = _directory_ids([source_root, work, cache, node.parent, npm_cli.parent])
    manifest_path = source_root / MANIFEST
    manifest_bytes = _read(manifest_path)
    manifest = _json(manifest_bytes)
    if not isinstance(manifest, dict) or manifest.get("schema") != "rootlight.rust-source-bundle/1" or not isinstance(manifest.get("project_sources"), list):
        raise FrontendInstallationError("expected an exported project source manifest")
    records = {}
    for record in manifest["project_sources"]:
        if not isinstance(record, dict):
            raise FrontendInstallationError("invalid source manifest record")
        name = record.get("path")
        _relative(name)
        if name in records or not isinstance(record.get("sha256"), str) or not re.fullmatch(r"[0-9a-f]{64}", record["sha256"]) or type(record.get("executable")) is not bool:
            raise FrontendInstallationError("invalid or duplicate source manifest identity")
        records[name] = record
    selected = sorted(name for name in records if _frontend_source(name))
    required = {FRONTEND + name for name in ("package.json", "package-lock.json", ".npmrc")}
    if not required.issubset(selected) or not {REPORT, UPSTREAM_LOCK, LOCAL_MANIFEST}.issubset(records):
        raise FrontendInstallationError("source manifest omits required frontend inputs")
    original = {}
    stamps = {}

    def retain(name: str) -> bytes:
        path = source_root / name
        value = _read(path)
        digest = hashlib.sha256(value).hexdigest()
        if name != MANIFEST and (name not in records or digest != records[name]["sha256"]):
            raise FrontendInstallationError("source differs from manifest: " + name)
        # Windows cannot attest POSIX execute bits; the export carries that intent.
        if os.name != "nt" and name != MANIFEST and bool(path.stat().st_mode & 0o111) != records[name]["executable"]:
            raise FrontendInstallationError("source executable mode differs from manifest: " + name)
        original[name], stamps[name] = digest, _stamp(path)
        return value

    original[MANIFEST], stamps[MANIFEST] = hashlib.sha256(manifest_bytes).hexdigest(), _stamp(manifest_path)
    staged_bytes = {name: retain(name) for name in selected}
    package = _json(staged_bytes[FRONTEND + "package.json"])
    lock = _json(staged_bytes[FRONTEND + "package-lock.json"])
    packages = lock.get("packages") if isinstance(lock, dict) else None
    root_record = packages.get("") if isinstance(packages, dict) else None
    if not isinstance(package, dict) or not isinstance(root_record, dict) or any(package.get(key) != root_record.get(key) for key in ("name", "version", "dependencies", "devDependencies", "engines")) or package.get("engines") != {"node": NODE_VERSION[1:], "npm": NPM_VERSION} or package.get("packageManager") != "npm@" + NPM_VERSION:
        raise FrontendInstallationError("canonical package/lock root or pinned toolchain differs")
    npmrc = staged_bytes[FRONTEND + ".npmrc"].decode("utf-8")
    allowed_config = {"audit-level=high", "engine-strict=true", "fund=false", "package-lock=true", "save-exact=true"}
    if any(line.strip() not in allowed_config for line in npmrc.splitlines() if line.strip()):
        raise FrontendInstallationError("unsupported or credential-bearing project npm configuration")
    report_bytes = retain(REPORT)
    verified = verify_local_bundle(source_root)
    if verified.get("schema") != "rootlight.npm-local-archive-bundle/1":
        raise FrontendInstallationError("unexpected local npm verification schema")
    expected_report = {**verified, **dict.fromkeys(_FALSE_REPORT_FIELDS, False)}
    if report_bytes != (json.dumps(expected_report, sort_keys=True, indent=2, allow_nan=False) + "\n").encode("utf-8"):
        raise FrontendInstallationError("versioned npm report differs from verified archive inputs")
    archive_paths = verified["archive_paths"]
    provenance_paths = verified["provenance_paths"]
    if not archive_paths or archive_paths != sorted(set(archive_paths)) or provenance_paths != sorted(set(provenance_paths)):
        raise FrontendInstallationError("locked archive inventory is empty")
    verified_inputs = {}
    for item in verified["input_files"]:
        name = item["path"]
        _relative(name)
        if name in verified_inputs or not (name == FRONTEND + "package-lock.json" or name.startswith("source-inputs/npm/")):
            raise FrontendInstallationError("unexpected local npm input identity")
        value = retain(name)
        if hashlib.sha256(value).hexdigest() != item["sha256"]:
            raise FrontendInstallationError("npm verifier input differs from source manifest")
        verified_inputs[name] = item["sha256"]
        staged_bytes[name] = value
    required_npm = {FRONTEND + "package-lock.json", UPSTREAM_LOCK, LOCAL_MANIFEST, *archive_paths, *provenance_paths}
    if set(verified_inputs) != required_npm:
        raise FrontendInstallationError("local npm verifier omits required authenticated inputs")
    staged_bytes[REPORT] = report_bytes
    inventory = _npm_inventory(source_root)
    if {name for name, identity in inventory.items() if identity[0] == "file"} != set(archive_paths) | set(provenance_paths):
        raise FrontendInstallationError("immutable archive inventory differs from authenticated paths")
    tools = {path: (hashlib.sha256(_read(path)).hexdigest(), _stamp(path)) for path in (node, npm_cli)}
    frontend = work / FRONTEND
    staged = {}
    staged_modes = {}
    controlled_configs = ()

    def unchanged() -> None:
        for directory, identity in parents.items():
            _regular(directory, directory=True)
            metadata = directory.stat()
            if (metadata.st_dev, metadata.st_ino) != identity:
                raise FrontendInstallationError("input/output ancestor changed")
        for name, digest in original.items():
            path = source_root / name
            if hashlib.sha256(_read(path)).hexdigest() != digest or _stamp(path) != stamps[name]:
                raise FrontendInstallationError("immutable source changed: " + name)
        if _npm_inventory(source_root) != inventory:
            raise FrontendInstallationError("immutable archive inventory changed")
        for path, (digest, stamp) in tools.items():
            if hashlib.sha256(_read(path)).hexdigest() != digest or _stamp(path) != stamp:
                raise FrontendInstallationError("declared tool changed")
        for path, digest in staged.items():
            if hashlib.sha256(_read(path)).hexdigest() != digest or stat.S_IMODE(path.stat().st_mode) != staged_modes[path]:
                raise FrontendInstallationError("staged authored source changed")
        for path in controlled_configs:
            if _read(path) != b"":
                raise FrontendInstallationError("controlled npm configuration changed")

    unchanged()
    frontend.mkdir(parents=True)
    for directory in (ARCHIVES, LOCAL_DISTRIBUTIONS):
        (work / directory).mkdir(parents=True, exist_ok=True)
    for name, value in staged_bytes.items():
        path = work / name
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("xb") as stream:
            stream.write(value)
        path.chmod(0o755 if records[name]["executable"] else 0o644)
        staged[path] = original[name]
        staged_modes[path] = stat.S_IMODE(path.stat().st_mode)
    environment = _environment(work, cache, node)
    controlled_configs = (work / "user.npmrc", work / "global.npmrc")
    unchanged()

    def run(arguments: list[str], phase: str, *, expected_version: str | None = None) -> None:
        unchanged()
        try:
            result = subprocess.run(arguments, cwd=frontend, env=environment, shell=False, check=False,
                                    stdout=subprocess.PIPE if expected_version else subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL, text=True, timeout=300)
        except (OSError, subprocess.SubprocessError) as error:
            raise FrontendInstallationError("frontend subprocess failed: " + phase) from error
        finally:
            unchanged()
        if result.returncode != 0 or expected_version is not None and result.stdout.strip() != expected_version:
            raise FrontendInstallationError("frontend command/version failed: " + phase)

    run([str(node), "--version"], "node-version", expected_version=NODE_VERSION)
    run([str(node), str(npm_cli), "--version"], "npm-version", expected_version=NPM_VERSION)
    policy = ["--cache", str(cache), "--offline", "--ignore-scripts", "--audit=false", "--fund=false"]
    run([str(node), str(npm_cli), "ci", *policy, "--include=dev", "--include=optional", "--include=peer"], "npm-ci")
    return {
        "schema": "rootlight.frontend-archive-installation/1",
        "source_manifest_sha256": original[MANIFEST],
        "package_json_sha256": original[FRONTEND + "package.json"],
        "package_lock_sha256": original[FRONTEND + "package-lock.json"],
        "npm_source_report_sha256": original[REPORT],
        "upstream_lock_sha256": original[UPSTREAM_LOCK],
        "local_distributions_manifest_sha256": original[LOCAL_MANIFEST],
        "archive_count": verified["archive_count"], "lock_record_count": verified["lock_record_count"],
        "original_archive_count": verified["original_archive_count"],
        "local_distribution_count": verified["local_distribution_count"],
        "staged_authored_files": len(selected), "staged_archive_files": len(archive_paths),
        "staged_provenance_files": len(provenance_paths), "staged_files": len(staged),
        "cache_add_batches": 0,
        "frontend_subdirectory": FRONTEND.rstrip("/"), "npm_ci_completed": True,
        "installed_tree_identity_verified": False, "caller_os_network_denial_required": True,
        "os_denied_acceptance_verified": False, "build_and_packaging_acceptance_verified": False,
        "preferred_source_closure_complete": False, "frontend_source_complete": False,
    }


def main(arguments: list[str] | None = None) -> int:
    """Install through explicit local inputs; return one on any failed phase.

    Requires --source-root, --work, --cache, --node and --npm-cli. Success prints
    bounded hashes/counts, never claims caller-established OS denial as verified.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("source-root", "work", "cache", "node", "npm-cli"):
        parser.add_argument("--" + option, type=Path, required=True)
    options = parser.parse_args(arguments)
    try:
        result = install_frontend(options.source_root, options.work, options.cache, options.node, options.npm_cli)
    except (OSError, ValueError) as error:
        parser.exit(1, f"local frontend installation failed: {error}\n")
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
