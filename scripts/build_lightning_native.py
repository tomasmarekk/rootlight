#!/usr/bin/env python3
"""Build the authenticated Lightning native cdylib with an explicit Cargo child.

This bypasses the authored JavaScript wrapper's lost child exit status. A copied
.node artifact is retained evidence, not admission by the frontend archive guard.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tomllib

import install_lightning_from_archives as install
import prepare_rust_source_bundle as sources


SCHEMA = "rootlight.lightning-native-build/1"
TARGETS = {
    "x86_64-pc-windows-msvc": ("lightningcss_node.dll", "lightningcss.win32-x64-msvc.node"),
    "x86_64-unknown-linux-gnu": ("liblightningcss_node.so", "lightningcss.linux-x64-gnu.node"),
    "aarch64-unknown-linux-gnu": ("liblightningcss_node.so", "lightningcss.linux-arm64-gnu.node"),
    "x86_64-apple-darwin": ("liblightningcss_node.dylib", "lightningcss.darwin-x64.node"),
    "aarch64-apple-darwin": ("liblightningcss_node.dylib", "lightningcss.darwin-arm64.node"),
}
ENVIRONMENT_KEYS = {"PATH", "INCLUDE", "LIB", "LIBPATH", "SDKROOT", "SystemRoot", "WINDIR",
                    "MACOSX_DEPLOYMENT_TARGET", "CC", "CXX", "AR", "LD"}
FALSE_CLAIMS = ("os_denied_acceptance_verified", "preferred_source_closure_complete",
                "frontend_source_complete", "frontend_rebuilt_artifact_selected",
                "build_and_packaging_acceptance_verified", "whole_native_profile_verified")


class NativeBuildError(ValueError):
    """Admission, child execution or artifact binding failed; outputs stay retained."""


def _require(condition: bool, detail: str) -> None:
    if not condition:
        raise NativeBuildError(detail)


def _record(path: Path) -> dict:
    checked = install.yarn.producer.safe_path(path, directory=False)
    digest = hashlib.sha256()
    before = install._stamp(checked)
    with checked.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    _require(install._stamp(checked) == before, "file changed while hashing")
    return {"path": str(checked), "bytes": checked.stat().st_size,
            "mode": stat.S_IMODE(checked.stat().st_mode), "sha256": digest.hexdigest()}


def _save(path: Path, document: dict) -> None:
    with path.open("xb") as stream:
        stream.write((json.dumps(document, sort_keys=True, indent=2) + "\n").encode())


def _ancestor_configs(work: Path) -> None:
    for parent in (work, *work.parents):
        for name in (".cargo/config", ".cargo/config.toml"):
            _require(not (parent / name).exists() and not (parent / name).is_symlink(),
                     "ambient ancestor Cargo configuration requires an isolated work root")


def _toolchain(path: Path) -> tuple[dict, list[dict]]:
    document = json.loads(install._read(path))
    _require(set(document) == {"schema", "target", "cargo", "rustc", "inputs", "environment"}
             and document["schema"] == "rootlight.lightning-native-toolchain/1", "unsupported toolchain manifest")
    _require(document["target"] in TARGETS, "unsupported native target")
    environment = document["environment"]
    _require(isinstance(environment, dict) and set(environment) <= ENVIRONMENT_KEYS
             and "PATH" in environment and all(isinstance(v, str) and "\0" not in v for v in environment.values()),
             "toolchain environment is not a reviewed explicit compiler environment")
    records = [_record(path)]
    seen = {str(path)}
    for name in ("cargo", "rustc"):
        item = document[name]
        _require(isinstance(item, dict) and set(item) == {"path", "sha256", "version"}, "invalid compiler declaration")
        expected_name = name + (".exe" if os.name == "nt" else "")
        _require(Path(item["path"]).name == expected_name and isinstance(item["version"], str), "direct compiler required")
    _require(isinstance(document["inputs"], list), "toolchain input inventory must be explicit")
    for item in [document["cargo"], document["rustc"], *document["inputs"]]:
        _require(isinstance(item, dict) and isinstance(item.get("path"), str)
                 and Path(item["path"]).is_absolute()
                 and isinstance(item.get("sha256"), str), "invalid declared tool input")
        actual = _record(Path(item["path"]))
        _require(actual["path"] not in seen and actual["sha256"] == item["sha256"], "duplicate or changed tool input")
        seen.add(actual["path"])
        records.append(actual)
    return document, records


def _authenticate(source_root: Path) -> tuple[dict, dict]:
    security = install.security.verify_bundle(source_root)
    locks = (*sources.LOCK_PATHS, sources.LIGHTNING_LOCK_PATH, sources.LIGHTNING_NATIVE_LOCK_PATH)
    cargo, _modes = sources._versioned_closure(source_root, lock_paths=locks)
    return security, cargo


def _helper_inputs(source_root: Path) -> list[dict]:
    names = ("install_lightning_from_archives", "prepare_rust_source_bundle", "verify_cargo_source_bundle",
             "verify_lightning_producer_sources", "verify_lightning_yarn_sources", "verify_lightning_security_sources",
             "verify_local_npm_sources", "verify_npm_source_bundle", "lightning_yarn_pins", "patch_npm_source_fixture")
    rows = [_record(Path(__file__).resolve())]
    for name in names:
        path = Path(sys.modules[name].__file__).resolve()
        _require(path == source_root / "scripts" / (name + ".py"), "helper resolved outside immutable source root")
        rows.append(_record(path))
    return rows


def _stage(source_root: Path, producer: Path, verified: dict) -> dict[str, dict]:
    original = source_root / install.yarn.PRODUCER_PATH
    proof = json.loads(install._read(original / "source-tree.json"))
    expected = verified["producer"]["source_tree_report"]
    _require(_record(original / "source-tree.json")["sha256"] == expected["sha256"], "producer report changed")
    effective, overlays = install.security.effective_producer(source_root, verified, proof)
    producer.mkdir()
    files = {}
    for member in effective["source_inventory"]["members"]:
        if member["path"] == install.yarn.producer.ROOT:
            continue
        prefix = install.yarn.producer.ROOT + "/"
        _require(member["path"].startswith(prefix), "foreign authored archive root")
        relative = str(install._relative(member["path"][len(prefix):]))
        destination = producer / relative
        if member["kind"] == "directory":
            destination.mkdir(parents=True, exist_ok=True)
        else:
            _require(member["kind"] == "file", "unsupported authored member")
            payload = overlays.get(relative)
            if payload is None:
                payload = install._read(original / "tree" / relative)
            _require(hashlib.sha256(payload).hexdigest() == member["sha256"], "authored member changed before staging")
            destination.parent.mkdir(parents=True, exist_ok=True)
            with destination.open("xb") as stream:
                stream.write(payload)
            destination.chmod(0o755 if member["archive_mode"] & 0o111 else 0o644)
            files[relative] = _record(destination)
    manifest = tomllib.loads(install._read(producer / "node/Cargo.toml").decode())
    _require(manifest["package"]["name"] == "lightningcss_node"
             and manifest["lib"]["crate-type"] == ["cdylib"], "native crate contract differs")
    return files


def _run(command: list[str], cwd: Path, environment: dict, logs: Path, phase: str, processes: list[dict]) -> dict:
    record = {"phase": phase, "command": command, "cwd": str(cwd), "spawn_attempted": False,
              "pid": None, "writer_closed": False, "exit_code": None}
    processes.append(record)
    stdout, stderr = logs / (phase + ".stdout"), logs / (phase + ".stderr")
    child = None
    interruption = None
    try:
        with stdout.open("xb") as out, stderr.open("xb") as err:
            record["spawn_attempted"] = True
            child = subprocess.Popen(command, cwd=cwd, env=environment, shell=False, stdout=out, stderr=err)
            record["pid"] = child.pid
            record["exit_code"] = child.wait()
            record["writer_closed"] = True
    except BaseException as error:
        interruption = error
        if child is not None:
            try:
                record["exit_code"] = child.poll()
                record["writer_closed"] = record["exit_code"] is not None
            except BaseException as observation_error:
                record["state_observation_error"] = type(observation_error).__name__
        record["error"] = type(error).__name__
        raise
    finally:
        if record["writer_closed"]:
            try:
                record["stdout"], record["stderr"] = _record(stdout), _record(stderr)
            except BaseException as reporting_error:
                if interruption is None:
                    raise
                record["reporting_error"] = type(reporting_error).__name__
                interruption.add_note("Closed-child log reporting also failed: " + str(reporting_error))
    _require(record["exit_code"] == 0, "native child failed: " + phase)
    return record


def _artifacts(events: Path, producer: Path, target: Path, target_name: str, retained: Path) -> dict:
    records, outputs, out_dirs, native, finished = [], set(), set(), [], []
    native_path = target / target_name / "release" / TARGETS[target_name][0]
    for line in events.open("rb"):
        event = json.loads(line)
        reason = event.get("reason")
        if reason == "compiler-artifact":
            records.append(event)
            outputs.update(Path(value) for value in event.get("filenames", []))
            if event.get("target", {}).get("name") == "lightningcss_node":
                _require(event["target"].get("crate_types") == ["cdylib"]
                         and event["target"].get("kind") == ["cdylib"]
                         and Path(event.get("manifest_path", "")) == producer / "node/Cargo.toml"
                         and event.get("profile", {}).get("test") is False
                         and event["profile"].get("opt_level") == "3"
                         and event.get("fresh") is False, "native artifact event is not this fresh release cdylib")
                native.append(event)
        elif reason == "build-script-executed":
            records.append(event)
            out_dirs.add(Path(event["out_dir"]))
        elif reason == "build-finished":
            finished.append(event.get("success"))
    _require(finished == [True] and len(native) == 1
             and native_path in {Path(value) for value in native[0]["filenames"]}, "missing or duplicate native build completion")
    for directory in out_dirs:
        directory = install.yarn.producer.safe_path(directory, directory=True)
        _require(directory.is_relative_to(target), "native OUT_DIR escaped its source-owned target")
        pending = [directory]
        while pending:
            for path in pending.pop().iterdir():
                metadata = path.lstat()
                _require(not install.yarn.producer.local.original._is_link(metadata), "linked native artifact")
                if stat.S_ISDIR(metadata.st_mode):
                    pending.append(path)
                else:
                    outputs.add(path)
    bindings = []
    for path in sorted(outputs):
        path = install.yarn.producer.safe_path(path, directory=False)
        _require(path.is_relative_to(target), "compiler artifact escaped its source-owned target")
        before = _record(path)
        copy = retained / path.relative_to(target)
        copy.parent.mkdir(parents=True, exist_ok=True)
        with path.open("rb") as source, copy.open("xb") as destination:
            shutil.copyfileobj(source, destination, 1024 * 1024)
        copied = _record(copy)
        _require(before == _record(path) and all(before[key] == copied[key] for key in ("bytes", "sha256")),
                 "compiler artifact changed during retention")
        bindings.append({"original": before, "retained": copied})
    selected = next((row for row in bindings if row["original"]["path"] == str(native_path)), None)
    _require(selected is not None and selected["original"]["bytes"] > 0, "native artifact is empty or unbound")
    return {"cargo_events": records, "bindings": bindings, "native": selected}


def _staged_inventory(producer: Path, target: Path) -> dict[str, dict]:
    files, pending = {}, [producer]
    while pending:
        for path in pending.pop().iterdir():
            if path == target:
                continue
            metadata = path.lstat()
            _require(not install.yarn.producer.local.original._is_link(metadata), "linked staged source")
            if stat.S_ISDIR(metadata.st_mode):
                pending.append(path)
            else:
                files[path.relative_to(producer).as_posix()] = _record(path)
    return files


def _cache_inventory(home: Path) -> list[dict]:
    _require(not (home / "git").exists() and not (home / "git").is_symlink(), "external Git cache was populated")
    registry = home / "registry"
    if registry.exists() or registry.is_symlink():
        install.yarn.producer.safe_path(registry, directory=True)
        marker = registry / "CACHEDIR.TAG"
        expected = (b"Signature: 8a477f597d28d172789f06886806bc55\n"
                    b"# This file is a cache directory tag created by cargo.\n"
                    b"# For information about cache directory tags see https://bford.info/cachedir/\n")
        _require({path.name for path in registry.iterdir()} == {"CACHEDIR.TAG"}
                 and install._read(marker) == expected, "external registry cache was populated")
    rows, pending = [], [home]
    while pending:
        for path in pending.pop().iterdir():
            metadata = path.lstat()
            _require(not install.yarn.producer.local.original._is_link(metadata), "linked Cargo cache output")
            if stat.S_ISDIR(metadata.st_mode):
                pending.append(path)
            else:
                rows.append(_record(path))
    return sorted(rows, key=lambda row: row["path"])


def build_lightning_native(source_root: Path, work: Path, toolchain_path: Path) -> dict:
    """Stage verified native sources and retain an actual Cargo artifact-to-node join.

    Args:
        source_root: Immutable reviewed local source inputs, including all five
            Cargo lock inventories and the effective native security profile.
        work: Existing empty unlinked directory outside source_root, with no
            ancestor Cargo configuration. Failed output is never removed.
        toolchain_path: Reviewed direct compiler paths, expected hashes/versions,
            native target, supporting tool input pins and explicit environment.

    Returns:
        Closed child logs, exact effective source and dependency evidence, all
        emitted artifact bindings and a byte-identical platform .node copy.
        Broader preferred-source, OS-denied and frontend acceptance remain false.

    Raises:
        NativeBuildError/OSError/ValueError: Reused or unsafe paths, changed
            source/tool bytes, child failure or malformed artifact evidence.
        BaseException: Interruption propagates; an unknown child is neither
            killed nor treated as closed and no later phase is dispatched.

    The caller independently denies network access before invocation. No Node,
    Yarn, npm, authored build.js, frontend integration or elapsed watchdog runs.
    Stable source/output ancestors and nonhostile concurrency are required.
    """
    source_root = install.yarn.producer.safe_path(source_root, directory=True)
    work = install.yarn.producer.safe_path(work, directory=True)
    toolchain_path = install.yarn.producer.safe_path(toolchain_path, directory=False)
    _require(not work.is_relative_to(source_root) and not source_root.is_relative_to(work)
             and not any(work.iterdir()), "work must be a fresh external directory")
    _ancestor_configs(work)
    toolchain, tools = _toolchain(toolchain_path)
    helpers = _helper_inputs(source_root)
    _require(all(not Path(row["path"]).is_relative_to(work) for row in tools), "tools overlap build output")
    authenticated = _authenticate(source_root)
    producer, home, target, logs = work / "producer", work / "cargo-home", work / "producer/target", work / "logs"
    files = _stage(source_root, producer, authenticated[0])
    _require("target" not in {Path(name).parts[0] for name in files}, "authored target conflicts with build output")
    home.mkdir()
    target.mkdir()
    logs.mkdir()
    private_home, temporary = work / "home", work / "tmp"
    private_home.mkdir()
    temporary.mkdir()
    environment = {**toolchain["environment"], "CARGO_HOME": str(home), "CARGO_TARGET_DIR": str(target),
                   "RUSTC": toolchain["rustc"]["path"], "CARGO_NET_OFFLINE": "true", "CARGO_NET_RETRY": "0",
                   "HOME": str(private_home), "USERPROFILE": str(private_home), "TMP": str(temporary),
                   "TEMP": str(temporary), "TMPDIR": str(temporary), "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"}
    report = {"schema": SCHEMA, "completed": False, "processes": [], "target": toolchain["target"],
              "toolchain": toolchain, "tools": tools, "effective_sources": files,
              "helper_inputs": helpers,
              "authenticated_source_reports": authenticated, "source_root": str(source_root),
              "caller_os_network_denial_required": True, **{key: False for key in FALSE_CLAIMS}}

    def unchanged() -> None:
        _require(all(_record(Path(row["path"])) == row for row in (*tools, *helpers)), "declared compiler/helper input changed")
        _require(_staged_inventory(producer, target) == files, "effective source inventory changed")
        _require(_authenticate(source_root) == authenticated, "authenticated source inputs changed")

    failure = None
    try:
        for name, arguments in (("cargo-version", [toolchain["cargo"]["path"], "--version"]),
                                ("rustc-version", [toolchain["rustc"]["path"], "-vV"])):
            process = _run(arguments, producer, environment, logs, name, report["processes"])
            output = Path(process["stdout"]["path"]).read_text(encoding="utf-8").strip()
            component = name.split("-")[0]
            _require(output.splitlines()[0] == toolchain[component]["version"], "compiler version differs")
            if component == "rustc":
                _require([line for line in output.splitlines() if line.startswith("host: ")]
                         == ["host: " + toolchain["target"]], "cross-compilation is not admitted")
        _require(not any(home.iterdir()) and not any(target.iterdir()), "cache populated before build")
        unchanged()
        command = [toolchain["cargo"]["path"], "build", "--manifest-path", str(producer / "node/Cargo.toml"),
                   "--package", "lightningcss_node", "--lib", "--release", "--locked", "--offline",
                   "--target", toolchain["target"], "--message-format=json"]
        child = _run(command, producer, environment, logs, "cargo-build", report["processes"])
        unchanged()
        artifacts = _artifacts(Path(child["stdout"]["path"]), producer, target, toolchain["target"], work / "artifacts")
        original = Path(artifacts["native"]["original"]["path"])
        node = work / TARGETS[toolchain["target"]][1]
        with original.open("rb") as source, node.open("xb") as destination:
            shutil.copyfileobj(source, destination, 1024 * 1024)
        node_pin = _record(node)
        _require(all(node_pin[key] == artifacts["native"]["original"][key] for key in ("bytes", "sha256"))
                 and _record(original) == artifacts["native"]["original"], "node copy differs from compiled cdylib")
        unchanged()
        cache = _cache_inventory(home)
        report.update({"completed": True, "source_and_tools_conserved": True,
                       "artifact_evidence": artifacts, "node_artifact": node_pin,
                       "cargo_cache_after": cache,
                       "native_cargo_build_observed": True, "node_copy_matches_cargo_artifact": True})
        return report
    except BaseException as error:
        failure = error
        report["error"] = type(error).__name__ + ": " + str(error)
        raise
    finally:
        try:
            _save(work / "native-build.json", report)
        except BaseException as reporting_error:
            if failure is None:
                raise
            failure.add_note("Build receipt reporting also failed: " + str(reporting_error))


def main(arguments: list[str] | None = None) -> int:
    """Run one explicitly declared native build, retaining all outputs on failure."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source-root", "work", "toolchain"):
        parser.add_argument("--" + name, type=Path, required=True)
    options = parser.parse_args(arguments)
    try:
        result = build_lightning_native(options.source_root, options.work, options.toolchain)
    except (OSError, ValueError, KeyError) as error:
        print("native build failed; no acceptance claim: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps({"schema": SCHEMA, "native_cargo_build_observed": result["native_cargo_build_observed"],
                      "node_artifact": result["node_artifact"], **{key: False for key in FALSE_CLAIMS}}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
