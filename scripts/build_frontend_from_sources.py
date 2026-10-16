#!/usr/bin/env python3
"""Build the frontend with an explicitly selected fresh source-built native binding.

Canonical npm archives and installed package bytes remain unchanged. Compilation
authority is the builder invoked here, never a deserialized artifact receipt.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

import build_lightning_native as native
import install_frontend_from_archives as frontend


SCHEMA = "rootlight.frontend-source-build/1"
SELECTION_ENV = "ROOTLIGHT_REBUILT_LIGHTNING_STDIN"
PLATFORMS = {
    "x86_64-pc-windows-msvc": ("win32", "x64"),
    "x86_64-unknown-linux-gnu": ("linux", "x64"),
    "aarch64-unknown-linux-gnu": ("linux", "arm64"),
    "x86_64-apple-darwin": ("darwin", "x64"),
    "aarch64-apple-darwin": ("darwin", "arm64"),
}
FALSE_CLAIMS = ("preferred_source_closure_complete", "whole_native_profile_verified", "frontend_source_complete",
                "os_denied_acceptance_verified", "reproducible_package_verified", "installed_package_verified")
AUTHORED_BUILD = "tsc -b && node --require ../../../scripts/check_lightning_frontend_boundary.cjs node_modules/vite/bin/vite.js build && node scripts/write-asset-manifest.mjs"


def _require(condition, detail):
    if not condition:
        raise ValueError(detail)


def _digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()).hexdigest()


def _load(path):
    return frontend._json(frontend._read(path))


def _helpers(source_root):
    names = [(native, "build_lightning_native.py"), (frontend, "install_frontend_from_archives.py")]
    rows = native._helper_inputs(source_root)
    for module, name in names:
        path = Path(module.__file__).resolve()
        _require(path == source_root / "scripts" / name, "build helper resolved outside immutable source root")
        rows.append(native._record(path))
    _require(Path(__file__).resolve() == source_root / "scripts/build_frontend_from_sources.py",
             "frontend builder resolved outside immutable source root")
    rows.append(native._record(Path(__file__).resolve()))
    return rows


def _capsule(source_root, native_work, built, runtime):
    _require(built["completed"] is True and built["source_and_tools_conserved"] is True
             and built["native_cargo_build_observed"] is True and built["node_copy_matches_cargo_artifact"] is True
             and Path(built["source_root"]) == source_root, "fresh native builder did not complete this source frame")
    _require([row["phase"] for row in built["processes"]] == ["cargo-version", "rustc-version", "cargo-build"]
             and all(row["writer_closed"] is True and row["exit_code"] == 0 for row in built["processes"]),
             "native builder child state is not successful and closed")
    target = built["target"]
    _require(target in PLATFORMS and (runtime["platform"], runtime["arch"]) == PLATFORMS[target]
             and runtime["version"] == frontend.NODE_VERSION and isinstance(runtime["napi"], str) and runtime["napi"].isdigit()
             and isinstance(runtime["modules"], str) and runtime["modules"].isdigit(), "Node/native target or declared ABI metadata differs")
    artifact_path = native_work / native.TARGETS[target][1]
    artifact = native._record(artifact_path)
    join = built["artifact_evidence"]["native"]
    compiled_relative = Path(target) / "release" / native.TARGETS[target][0]
    _require(Path(built["node_artifact"]["path"]) == artifact_path
             and Path(join["original"]["path"]) == native_work / "producer/target" / compiled_relative
             and Path(join["retained"]["path"]) == native_work / "artifacts" / compiled_relative
             and all(artifact[key] == row[key] for row in (built["node_artifact"], join["original"], join["retained"])
                     for key in ("bytes", "sha256")), "fresh native artifact/copy join changed")
    expected_manifest = native_work / "producer/node/Cargo.toml"
    events = [row for row in built["artifact_evidence"]["cargo_events"]
              if row.get("reason") == "compiler-artifact" and row.get("target", {}).get("name") == "lightningcss_node"]
    _require(len(events) == 1 and Path(events[0]["manifest_path"]) == expected_manifest
             and events[0]["target"]["kind"] == events[0]["target"]["crate_types"] == ["cdylib"]
             and events[0]["fresh"] is False and events[0]["profile"]["test"] is False
             and events[0]["profile"]["opt_level"] == "3"
             and join["original"]["path"] in events[0]["filenames"], "native Cargo event/source binding differs")
    _require(native._record(Path(join["original"]["path"])) == join["original"]
             and native._record(Path(join["retained"]["path"])) == join["retained"], "actual dependency artifact changed")
    authenticated = native._authenticate(source_root)
    _require(authenticated == built["authenticated_source_reports"], "effective producer/five-lock inputs changed")
    report = _load(source_root / frontend.REPORT)
    package_name = "lightningcss-" + native.TARGETS[target][1][len("lightningcss."):-len(".node")]
    packages = [row for row in report["packages"] if row["name"] == package_name]
    _require(len(packages) == 1 and packages[0]["lock_paths"], "derived target lacks one canonical package association")
    platform_package = packages[0]
    source = dict(npm_report_sha256=native._record(source_root / frontend.REPORT)["sha256"],
                  producer_report_sha256=authenticated[0]["producer"]["source_tree_report"]["sha256"],
                  cargo_report_sha256=native._record(source_root / "source-inputs/cargo-source-closure.json")["sha256"],
                  effective_native_report_sha256=_digest(authenticated[0]),
                  effective_sources_sha256=_digest(built["effective_sources"]),
                  five_lockfiles_sha256=_digest(authenticated[1]["lockfiles"]),
                  native_build_evidence_sha256=_digest(built))
    return dict(schema="rootlight.lightning-derived-selection/1", target=target,
        package=dict(name=platform_package["name"], version=platform_package["version"],
                     lock_paths=platform_package["lock_paths"]),
        artifact={key: artifact[key] for key in ("path", "bytes", "sha256")}, runtime=runtime, source=source,
        compiler_versions=dict(cargo=built["toolchain"]["cargo"]["version"], rustc=built["toolchain"]["rustc"]["version"]),
        authorization_scope="one child selection only; compilation proof belongs to the fresh in-process builder")


def _run(command, cwd, environment, logs, name, processes, payload=None):
    record = dict(phase=name, command=command, spawn_attempted=False, pid=None, writer_closed=False, exit_code=None)
    processes.append(record)
    child, failure = None, None
    try:
        with (logs / (name + ".stdout")).open("xb") as out, (logs / (name + ".stderr")).open("xb") as err:
            record["spawn_attempted"] = True
            child = subprocess.Popen(command, cwd=cwd, env=environment, shell=False,
                stdin=subprocess.PIPE if payload is not None else subprocess.DEVNULL, stdout=out, stderr=err)
            record["pid"] = child.pid
            child.communicate(input=payload)
            record["exit_code"], record["writer_closed"] = child.returncode, True
    except BaseException as error:
        failure = error
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
                record["stdout"] = native._record(logs / (name + ".stdout"))
                record["stderr"] = native._record(logs / (name + ".stderr"))
            except BaseException as reporting_error:
                if failure is None:
                    raise
                failure.add_note("closed frontend log reporting failed: " + str(reporting_error))
    _require(record["exit_code"] == 0, "frontend child failed: " + name)
    return record


def build_frontend_from_sources(source_root, frontend_work, native_work, cache, toolchain, node, npm_cli):
    """Install exact archives, invoke a fresh native build, and run the authored frontend stages.

    All paths name explicit local inputs or fresh external outputs. The caller must
    independently deny network and retain final process/source evidence. No receipt
    or prebuilt-artifact input is accepted. The existing installer validates exact
    Node/npm versions and canonical archive/source bytes before any build stage.
    Failed outputs and unknown writers remain retained; no elapsed cutoff or kill
    is introduced here. Source ancestors and concurrency must remain stable.
    """
    source_root = frontend._regular(source_root, directory=True)
    node, npm_cli = frontend._regular(node), frontend._regular(npm_cli)
    frontend_work = frontend._regular(frontend_work, directory=True)
    native_work = frontend._regular(native_work, directory=True)
    cache = frontend._regular(cache, directory=True)
    outputs = (frontend_work, native_work, cache)
    _require(all(not any(path.iterdir()) and not path.is_relative_to(source_root) and not source_root.is_relative_to(path)
                 for path in outputs), "all outputs must be fresh and external to immutable sources")
    _require(all(left == right or not left.is_relative_to(right) and not right.is_relative_to(left)
                 for left in outputs for right in outputs) and len(set(outputs)) == 3, "build outputs overlap")
    helpers = _helpers(source_root)
    tools = [native._record(path) for path in (node, npm_cli)]
    installed = frontend.install_frontend(source_root, frontend_work, cache, node, npm_cli)
    built = native.build_lightning_native(source_root, native_work, toolchain)
    frontend_path = frontend_work / frontend.FRONTEND
    _require(_load(frontend_path / "package.json")["scripts"]["build"] == AUTHORED_BUILD,
             "authored frontend build stages require review")
    guard = frontend_work / frontend.BUILD_GUARD
    _require(frontend._read(guard) == frontend._read(source_root / frontend.BUILD_GUARD), "staged frontend guard changed")
    logs = frontend_work / "source-build-logs"
    logs.mkdir()
    system = {key: os.environ[key] for key in ("SystemRoot", "WINDIR") if key in os.environ}
    environment = dict(system, PATH=os.pathsep.join((str(node.parent), "/usr/bin", "/bin")) if os.name != "nt"
                       else os.pathsep.join((str(node.parent), str(Path(system["SystemRoot"]) / "System32"))),
        HOME=str(frontend_work / "home"), USERPROFILE=str(frontend_work / "home"), TMP=str(frontend_work / "tmp"),
        TEMP=str(frontend_work / "tmp"), TMPDIR=str(frontend_work / "tmp"), LANG="C.UTF-8", LC_ALL="C.UTF-8", CI="true")
    report = dict(schema=SCHEMA, completed=False, installation=installed, native_build=built, frontend_tools=tools, processes=[],
                  **{name: False for name in FALSE_CLAIMS})
    failure = None
    try:
        probe = "process.stdout.write(JSON.stringify({version:process.version,platform:process.platform,arch:process.arch,napi:process.versions.napi,modules:process.versions.modules})+'\\n')"
        runtime_child = _run([str(node), "--eval", probe], frontend_path, environment, logs, "node-runtime", report["processes"])
        runtime = _load(Path(runtime_child["stdout"]["path"]))
        capsule = _capsule(source_root, native_work, built, runtime)
        _require(capsule["source"]["npm_report_sha256"] == installed["npm_source_report_sha256"],
                 "frontend installation and native build use different npm source frames")
        payload = (json.dumps(capsule, sort_keys=True, separators=(",", ":")) + "\n").encode()
        _require(len(payload) <= 65536, "selection capsule exceeds its explicit protocol bound")
        native._save(frontend_work / "derived-native-selection.json", capsule)
        _run([str(node), "node_modules/typescript/bin/tsc", "-b"], frontend_path, environment, logs, "typescript", report["processes"])
        vite = _run([str(node), "--require", str(guard), "node_modules/vite/bin/vite.js", "build"], frontend_path,
                    {**environment, SELECTION_ENV: "1"}, logs, "vite", report["processes"], payload)
        observations = []
        for line in frontend._read(Path(vite["stdout"]["path"])).splitlines():
            if line.startswith(b"{"):
                try:
                    value = frontend._json(line)
                except (ValueError, UnicodeError):
                    continue
                if isinstance(value, dict) and value.get("schema") == "rootlight.lightning-frontend-boundary/2":
                    observations.append(value)
        _require(len(observations) == 1 and observations[0]["transform_calls"] >= 1
                 and observations[0]["derived_selection_observed"] is True
                 and observations[0]["selection_capsule_sha256"] == hashlib.sha256(payload).hexdigest(),
                 "production minifier did not observe this one-use derived selection")
        selected = observations[0]["native_artifacts"]
        _require(len(selected) == 1 and selected[0]["selected_native"]["sha256"] == capsule["artifact"]["sha256"]
                 and selected[0]["selected_native"]["path"] == capsule["artifact"]["path"], "actual minifier selected another native artifact")
        _run([str(node), "scripts/write-asset-manifest.mjs"], frontend_path, environment, logs, "asset-manifest", report["processes"])
        _require(_helpers(source_root) == helpers and native._authenticate(source_root) == built["authenticated_source_reports"]
                 and [native._record(path) for path in (node, npm_cli)] == tools
                 and frontend._read(guard) == frontend._read(source_root / frontend.BUILD_GUARD)
                 and all(native._record(Path(built["node_artifact"]["path"]))[key] == capsule["artifact"][key]
                         for key in ("bytes", "sha256")), "source/helper/guard/derived artifact changed")
        report.update(completed=True, fresh_in_process_native_build_observed=True,
                      derived_selection=capsule, frontend_boundary=observations[0],
                      real_frontend_selected_rebuilt_native=True, authored_build_stages_completed=True)
        return report
    except BaseException as error:
        failure = error
        report["error"] = type(error).__name__ + ": " + str(error)
        raise
    finally:
        try:
            native._save(frontend_work / "source-build.json", report)
        except BaseException as reporting_error:
            if failure is None:
                raise
            failure.add_note("frontend source-build reporting failed: " + str(reporting_error))


def main():
    """Run a fresh source build with explicit roots; accept no persisted compilation proof."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source-root", "frontend-work", "native-work", "cache", "toolchain", "node", "npm-cli"):
        parser.add_argument("--" + name, type=Path, required=True)
    options = parser.parse_args()
    try:
        result = build_frontend_from_sources(options.source_root, options.frontend_work, options.native_work,
            options.cache, options.toolchain, options.node, options.npm_cli)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, "frontend source build failed: " + str(error) + "\n")
    print(json.dumps(dict(schema=SCHEMA, completed=result["completed"],
        real_frontend_selected_rebuilt_native=result["real_frontend_selected_rebuilt_native"],
        **{name: False for name in FALSE_CLAIMS}), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
