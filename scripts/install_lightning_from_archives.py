#!/usr/bin/env python3
"""Stage the authenticated Lightning CSS producer and install its Yarn originals.

An offline mirror preserves the authored lock instead of rewriting its resolutions.
This is an installer boundary, not lifecycle execution or OS-denied build acceptance.
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
import sys
from typing import Any

import verify_lightning_yarn_sources as yarn


NODE_VERSION = "v24.11.1"
YARN_VERSION = "1.22.22"
YARN_CLI_SHA256 = "443ed69e76443b89afddccfc9faec1ff16eb5e500979cc079c696dec4c3d94ee"
YARN_PACKAGE_SHA256 = "9533b84eaaeea708ab99bcf92772bc81c7389f90a04f8b0188c163f9b3b621c3"
PRODUCER_DIRECTORY = "producer"
MIRROR_DIRECTORY = "offline-mirror"
MAX_LOG_BYTES = 16 * 1024 * 1024
VERSION_TIMEOUT_SECONDS = 300
DEFAULT_INSTALL_TIMEOUT_SECONDS = 1800
_MIRROR_URL = re.compile(r"/(?:(@[^/]+)(?:/|%2f))?[^/]+/(?:-|_attachments)/(?:@[^/]+/)?([^/]+)\Z")


class LightningInstallationError(ValueError):
    """A local installation input or child phase violated the immutable boundary."""


def _read(path: Path) -> bytes:
    return yarn.producer.read_bytes(path, yarn.MAX_EXPANDED)


def _digest(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _stamp(path: Path) -> tuple[int, int, int, int, int]:
    metadata = path.stat()
    return metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns, stat.S_IMODE(metadata.st_mode)


def _relative(value: str) -> PurePosixPath:
    if not isinstance(value, str) or not value or "\\" in value or ":" in value or any(ord(c) < 32 for c in value):
        raise LightningInstallationError("invalid authenticated relative path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in value.split("/")):
        raise LightningInstallationError("escaping authenticated relative path")
    return path


def _mirror_filename(pin: dict[str, Any]) -> str:
    match = _MIRROR_URL.fullmatch(pin["url_path"])
    if match is None:
        raise LightningInstallationError("unsupported offline mirror URL")
    scope, basename = match.groups()
    filename = f"{scope}-{basename}" if scope else basename
    _relative(filename)
    if "/" in filename or not re.fullmatch(r"@?[A-Za-z0-9._-]+\.tgz", filename):
        raise LightningInstallationError("unsafe offline mirror filename")
    return filename


def _directory_ids(paths: list[Path]) -> dict[Path, tuple[int, int]]:
    result = {}
    for path in paths:
        for ancestor in (path, *path.parents):
            checked = yarn.producer.safe_path(ancestor, directory=True)
            metadata = checked.stat()
            result[checked] = metadata.st_dev, metadata.st_ino
    return result


def _inventory(paths: list[Path]) -> dict[Path, tuple[str, tuple[int, int, int, int, int]]]:
    result = {}
    pending = list(paths)
    while pending:
        path = pending.pop()
        metadata = path.lstat()
        directory = stat.S_ISDIR(metadata.st_mode)
        checked = yarn.producer.safe_path(path, directory=directory)
        result[checked] = ("directory" if directory else "file", _stamp(checked))
        if directory:
            pending.extend(checked.iterdir())
    return result


def _reject_ancestor_manifests(work: Path) -> None:
    # Yarn discovers workspaces independently of --no-default-rc. Ordinary
    # declarations are also rejected instead of reproducing its JSON/glob rules.
    for ancestor in (work, *work.parents):
        for name in ("package.json", "yarn.json"):
            try:
                (ancestor / name).lstat()
            except FileNotFoundError:
                continue
            raise LightningInstallationError("ancestor package manifest declarations are prohibited")


def _environment(work: Path, node: Path) -> dict[str, str]:
    home, temporary = work / "home", work / "tmp"
    home.mkdir()
    temporary.mkdir()
    system = {key: os.environ[key] for key in ("SystemRoot", "WINDIR") if key in os.environ}
    path = [str(node.parent)]
    if os.name == "nt":
        path.append(str(Path(system.get("SystemRoot", r"C:\Windows")) / "System32"))
    return {
        **system, "PATH": os.pathsep.join(path), "HOME": str(home), "USERPROFILE": str(home),
        "XDG_CACHE_HOME": str(home / "cache"), "XDG_CONFIG_HOME": str(home / "config"),
        "TMP": str(temporary), "TEMP": str(temporary), "TMPDIR": str(temporary),
        "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "CI": "true",
        "YARN_IGNORE_PATH": "1", "DISABLE_V8_COMPILE_CACHE": "1",
    }


def install_lightning(source_root: Path, work: Path, cache: Path, node: Path, yarn_cli: Path,
                      node_sha256: str, *,
                      install_timeout_seconds: int = DEFAULT_INSTALL_TIMEOUT_SECONDS) -> dict[str, Any]:
    """Install verified published distributions with an empty external Yarn cache.

    Args:
        source_root: Project containing the strict producer, npm association and
            complete authenticated Yarn originals accepted by verify_bundle.
        work: Existing empty, unlinked output directory outside source_root, with
            no package.json or yarn.json entries in it or any ancestor directory.
        cache: Separate existing empty, unlinked dependency-cache directory.
        node: Explicit caller-reviewed direct Node executable matching NODE_VERSION.
        yarn_cli: Explicit Yarn lib/cli.js with the pinned adjacent package metadata.
        node_sha256: Independently reviewed lowercase SHA-256 for that Node binary;
            compared before any child. A filename/version alone cannot exclude shims.
        install_timeout_seconds: Positive whole-second operational watchdog for
            local archive installation. Version probes retain their separate short
            watchdog. This is not an indexing performance or correctness threshold.

    Returns:
        Input/tool hashes, graph counts and successful Yarn invocation evidence.
        The staged producer remains in work/producer and original lock bytes,
        selectors, integrity values and local root associations are unchanged.
        Installed-tree identity, preferred sources and build acceptance are false.

    Raises:
        LightningInstallationError/OSError/ValueError: Unsafe paths, overlapping or
            reused outputs, failed authentication, child failure or input drift.
        Failed outputs are retained; this never cleans up or writes versioned inputs.

    Independently deny network access before invocation for an OS-denied proof.
    Yarn's offline policy is not that proof. Ancestors must remain stable; this
    does not provide hostile-concurrency isolation or run any lifecycle scripts.
    Strict archive authentication runs at entry and final child qualification;
    frozen file identities, stamps and inventories conserve inputs between them.
    """
    if type(install_timeout_seconds) is not int or install_timeout_seconds <= 0:
        raise LightningInstallationError("installation timeout must be a positive whole number of seconds")
    source_root = yarn.producer.safe_path(source_root, directory=True)
    work = yarn.producer.safe_path(work, directory=True)
    cache = yarn.producer.safe_path(cache, directory=True)
    _reject_ancestor_manifests(work)
    for output in (work, cache):
        if output.is_relative_to(source_root) or source_root.is_relative_to(output):
            raise LightningInstallationError("outputs must be external to immutable sources")
        if any(output.iterdir()):
            raise LightningInstallationError("work and cache must initially be empty")
    if work.is_relative_to(cache) or cache.is_relative_to(work):
        raise LightningInstallationError("work and cache must be separate")
    node = yarn.producer.safe_path(node, directory=False)
    yarn_cli = yarn.producer.safe_path(yarn_cli, directory=False)
    if node.name != ("node.exe" if os.name == "nt" else "node"):
        raise LightningInstallationError("expected declared Node executable filename")
    if yarn_cli.name != "cli.js" or yarn_cli.parent.name != "lib":
        raise LightningInstallationError("explicit direct Yarn lib/cli.js required")
    yarn_package = yarn_cli.parent.parent / "package.json"
    tool_paths = (node, yarn_cli, yarn_package)
    for path in tool_paths:
        if path.is_relative_to(work) or path.is_relative_to(cache):
            raise LightningInstallationError("declared tools may not be inside outputs")
    tools = {path: (_digest(_read(path)), _stamp(path)) for path in tool_paths}
    if not isinstance(node_sha256, str) or not re.fullmatch(r"[0-9a-f]{64}", node_sha256) or tools[node][0] != node_sha256:
        raise LightningInstallationError("caller-reviewed Node SHA-256 differs or is malformed")
    metadata = json.loads(_read(yarn_package))
    if (tools[yarn_cli][0] != YARN_CLI_SHA256 or tools[yarn_package][0] != YARN_PACKAGE_SHA256
            or not isinstance(metadata, dict) or metadata.get("name") != "yarn" or metadata.get("version") != YARN_VERSION):
        raise LightningInstallationError("pinned Yarn tool bytes or metadata differ")

    verified = yarn.verify_bundle(source_root)
    if verified.get("schema") != "rootlight.lightning-yarn-source-closure/1" or not verified.get("original_archives_verified"):
        raise LightningInstallationError("unexpected authenticated Yarn evidence")
    source_tree = source_root / yarn.PRODUCER_PATH / "tree"
    report_path = source_root / yarn.PRODUCER_PATH / "source-tree.json"
    source_report_bytes = _read(report_path)
    if _digest(source_report_bytes) != verified["producer"]["source_tree_report"]["sha256"]:
        raise LightningInstallationError("authenticated producer report changed")
    source_report = json.loads(source_report_bytes)
    producer_files, producer_directories = {}, set()
    for member in source_report["source_inventory"]["members"]:
        if member["path"] == yarn.producer.ROOT:
            continue
        if not member["path"].startswith(yarn.producer.ROOT + "/"):
            raise LightningInstallationError("producer inventory has a foreign archive root")
        relative = str(_relative(member["path"][len(yarn.producer.ROOT) + 1:]))
        if member["kind"] == "directory":
            producer_directories.add(relative)
        elif member["kind"] == "file":
            producer_files[relative] = member
        else:
            raise LightningInstallationError("unsupported producer member kind")
    if not {"package.json", "yarn.lock"}.issubset(producer_files):
        raise LightningInstallationError("producer inventory omits authored install inputs")

    mirror_rows, mirror_names = [], set()
    for package in verified["packages"]:
        pin = package["pin"]
        filename = _mirror_filename(pin)
        if filename.casefold() in mirror_names:
            raise LightningInstallationError("offline mirror filename collision")
        mirror_names.add(filename.casefold())
        relative = str(_relative(package["archive_path"]))
        if relative != "archives/" + pin["archive_filename"]:
            raise LightningInstallationError("archive does not match authenticated pin")
        mirror_rows.append({"filename": filename, "archive_path": relative,
                            "sha256": verified["files"][relative]["sha256"],
                            "resolved": pin["resolved"], "integrity": pin["integrity"]})
    if not mirror_rows or len(mirror_rows) != verified["archive_count"]:
        raise LightningInstallationError("authenticated mirror inventory differs")
    expected_mirror_files = {row["filename"] for row in mirror_rows}
    parents = _directory_ids([source_root, work, cache, source_tree, source_root / yarn.BUNDLE_PATH,
                              node.parent, yarn_cli.parent])
    immutable_roots = [source_root / yarn.PRODUCER_PATH, source_root / yarn.BUNDLE_PATH,
                       source_root / "source-inputs/npm", source_root / yarn.producer.local.CANONICAL_LOCK]
    immutable_inventory = _inventory(immutable_roots)
    staged: dict[Path, tuple[str, tuple[int, int, int, int, int]]] = {}

    def unchanged() -> None:
        _reject_ancestor_manifests(work)
        for directory, identity in parents.items():
            metadata = yarn.producer.safe_path(directory, directory=True).stat()
            if (metadata.st_dev, metadata.st_ino) != identity:
                raise LightningInstallationError("input/output ancestor changed")
        for path, (digest, stamp) in tools.items():
            if _digest(_read(path)) != digest or _stamp(path) != stamp:
                raise LightningInstallationError("declared tool changed")
        for path, (digest, stamp) in staged.items():
            if _digest(_read(path)) != digest or _stamp(path) != stamp:
                raise LightningInstallationError("staged authored input or mirror changed")
        if {entry.name for entry in yarn.producer.safe_path(mirror, directory=True).iterdir()} != expected_mirror_files:
            raise LightningInstallationError("staged offline mirror inventory changed")
        if _inventory(immutable_roots) != immutable_inventory:
            raise LightningInstallationError("authenticated source inputs changed")

    def stage(path: Path, payload: bytes, digest: str, *, executable: bool = False) -> None:
        if _digest(payload) != digest:
            raise LightningInstallationError("source changed before staging")
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("xb") as stream:
            stream.write(payload)
        path.chmod(0o755 if executable else 0o644)
        staged[path] = digest, _stamp(path)

    producer = work / PRODUCER_DIRECTORY
    producer.mkdir()
    for relative in sorted(producer_directories):
        (producer / relative).mkdir(parents=True, exist_ok=True)
    for relative, member in sorted(producer_files.items()):
        stage(producer / relative, _read(source_tree / relative), member["sha256"],
              executable=bool(member["archive_mode"] & 0o111))
    mirror = work / MIRROR_DIRECTORY
    mirror.mkdir()
    for row in mirror_rows:
        stage(mirror / row["filename"], _read(source_root / yarn.BUNDLE_PATH / row["archive_path"]), row["sha256"])
    environment = _environment(work, node)
    for directory in ("global", "links"):
        (work / directory).mkdir()
    configuration = work / "controlled.yarnrc"
    config_bytes = ("yarn-offline-mirror " + json.dumps(str(mirror))
                    + "\nyarn-offline-mirror-pruning false\ndisable-self-update-check true\n").encode("utf-8")
    stage(configuration, config_bytes, _digest(config_bytes))
    logs = work / "logs"
    logs.mkdir()
    parents.update(_directory_ids([producer, mirror, logs, work / "home", work / "tmp",
                                  work / "global", work / "links"]))
    log_records = []

    def run(arguments: list[str], phase: str, *, version: str | None = None) -> None:
        unchanged()
        timeout = VERSION_TIMEOUT_SECONDS if version is not None else install_timeout_seconds
        stdout_path, stderr_path = logs / (phase + ".stdout.log"), logs / (phase + ".stderr.log")
        try:
            with stdout_path.open("xb") as stdout, stderr_path.open("xb") as stderr:
                result = subprocess.run(arguments, cwd=producer, env=environment, shell=False, check=False,
                                        stdout=stdout, stderr=stderr, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise LightningInstallationError(
                f"local Yarn subprocess failed: {phase} (timeout after {timeout} seconds)"
            ) from error
        except (OSError, subprocess.SubprocessError) as error:
            raise LightningInstallationError("local Yarn subprocess failed: " + phase) from error
        finally:
            unchanged()
        output = b""
        for path in (stdout_path, stderr_path):
            payload = yarn.producer.read_bytes(path, MAX_LOG_BYTES)
            digest = _digest(payload)
            staged[path] = digest, _stamp(path)
            log_records.append({"path": path.relative_to(work).as_posix(), "bytes": len(payload), "sha256": digest})
            if path == stdout_path:
                output = payload
        if result.returncode != 0 or version is not None and output.decode("utf-8").strip() != version:
            raise LightningInstallationError("local Yarn command/version failed: " + phase)

    try:
        run([str(node), "--version"], "node-version", version=NODE_VERSION)
        run([str(node), str(yarn_cli), "--version", "--no-default-rc", "--use-yarnrc", str(configuration)],
            "yarn-version", version=YARN_VERSION)
        if any(cache.iterdir()):
            raise LightningInstallationError("dependency cache is not empty immediately before installation")
        policy = ["--offline", "--ignore-scripts", "--frozen-lockfile", "--production=false", "--non-interactive",
                  "--disable-pnp", "--no-default-rc", "--use-yarnrc", str(configuration),
                  "--cache-folder", str(cache), "--global-folder", str(work / "global"), "--link-folder", str(work / "links")]
        run([str(node), str(yarn_cli), "install", *policy], "yarn-install")
    finally:
        try:
            if yarn.verify_bundle(source_root) != verified:
                raise LightningInstallationError("authenticated source inputs changed at final qualification")
        finally:
            unchanged()
    graph = verified["pin_graph"]
    return {
        "schema": "rootlight.lightning-archive-installation/1",
        "producer_git_head": verified["producer"]["git_head"],
        "producer_report_sha256": _digest(source_report_bytes),
        "yarn_source_report_sha256": _digest(yarn.producer.json_bytes(verified)),
        "package_json_sha256": graph["manifest_sha256"], "yarn_lock_sha256": graph["lockfile_sha256"],
        "node_version": NODE_VERSION, "node_sha256": node_sha256, "yarn_version": YARN_VERSION,
        "yarn_cli_sha256": tools[yarn_cli][0], "yarn_package_sha256": tools[yarn_package][0],
        "archive_count": len(mirror_rows), "offline_mirror_map_sha256": _digest(yarn.producer.json_bytes(mirror_rows)),
        "staged_producer_files": len(producer_files), "stanza_count": graph["stanza_count"],
        "selector_count": graph["selector_count"], "dependency_edge_count": graph["dependency_edge_count"],
        "local_root_associations": graph["local_associations"], "producer_subdirectory": PRODUCER_DIRECTORY,
        "dependency_cache_initially_empty": True, "lock_rewritten": False, "yarn_install_completed": True,
        "strict_source_reverification_completed": True, "child_logs": log_records,
        "phase_timeouts_seconds": {"version_probe": VERSION_TIMEOUT_SECONDS,
                                   "yarn_install": install_timeout_seconds},
        "lifecycle_scripts_enabled": False, "installed_tree_identity_verified": False, "installation_verified": False,
        "caller_os_network_denial_required": True, "os_denied_acceptance_verified": False,
        "preferred_source_closure_complete": False, "producer_transitive_sources_complete": False,
        "source_to_binary_rebuild_verified": False, "build_and_packaging_acceptance_verified": False,
    }


def main(arguments: list[str] | None = None) -> int:
    """Run the explicit local installer; print bounded evidence or return exit one.

    Requires source-root, work, cache, node and yarn-cli paths plus node-sha256. No acquisition,
    lifecycle execution, cleanup or OS-denied acceptance is performed.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("source-root", "work", "cache", "node", "yarn-cli"):
        parser.add_argument("--" + option, type=Path, required=True)
    parser.add_argument("--node-sha256", required=True)
    parser.add_argument("--install-timeout-seconds", type=int, default=DEFAULT_INSTALL_TIMEOUT_SECONDS,
                        help="positive local installation watchdog; unrelated to indexing performance")
    options = parser.parse_args(arguments)
    try:
        result = install_lightning(options.source_root, options.work, options.cache, options.node, options.yarn_cli,
                                   options.node_sha256, install_timeout_seconds=options.install_timeout_seconds)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print("local Lightning installation failed; no acceptance claim: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
