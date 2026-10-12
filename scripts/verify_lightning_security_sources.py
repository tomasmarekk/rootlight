#!/usr/bin/env python3
"""Authenticate the supported Lightning native build profile and its source overlays.

The original upstream tree remains provenance. Executable preparation consumes
this explicitly declared profile, whose archive pins and member bytes are checked.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import sys
import tomllib

import verify_lightning_yarn_sources as authored

producer = authored.producer
yarn = authored.yarn
original = authored.original
PRODUCER_PATH = authored.PRODUCER_PATH
BUNDLE_PATH = f"source-inputs/producer-security/{producer.ROOT}"
REPORT_NAME = "native-source-closure.json"
PROFILE_NAME = "profile.json"
SCHEMA = "rootlight.lightning-native-source-closure/1"
NATIVE_TOOLS = {"@napi-rs/cli", "flowgen", "typescript", "uvu"}
NATIVE_SCRIPTS = {"build", "build-release", "prepublishOnly", "tsc", "test"}


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def _record(payload: bytes) -> dict:
    return {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}


def native_manifest(payload: bytes) -> bytes:
    """Derive the declared native-build tools and scripts from original source bytes.

    Website publishing, benchmark tools, WASM packaging and AST regeneration are
    preserved in provenance but are outside this profile. No package code runs.
    """
    document = original._parse_json(payload, "authored native manifest")
    document["private"] = True
    if "resolutions" in document:
        _require(document["resolutions"] == {document["name"]: "link:."},
                 "unsupported native producer resolutions")
        # The reduced native tool graph has no dependency on the producer itself.
        # Its old self-resolution would declare a local placement Yarn never installs.
        del document["resolutions"]
    document["devDependencies"] = {
        name: value for name, value in document["devDependencies"].items() if name in NATIVE_TOOLS
    }
    document["scripts"] = {
        name: value for name, value in document["scripts"].items() if name in NATIVE_SCRIPTS
    }
    _require(set(document["devDependencies"]) == NATIVE_TOOLS
             and set(document["scripts"]) == NATIVE_SCRIPTS, "native producer contract changed")
    return (json.dumps(document, indent=2) + "\n").encode()


def _inventory(bundle: Path, expected: set[str]) -> None:
    directories = {"."}
    for name in expected:
        directories.update(str(parent) for parent in PurePosixPath(name).parents)
    found = set()
    for current, children, files in os.walk(bundle, followlinks=False, onerror=authored._walk_error):
        directory = producer.safe_path(Path(current), directory=True)
        _require(directory.relative_to(bundle).as_posix() in directories, "extra native source directory")
        for name in children:
            producer.safe_path(directory / name, directory=True)
        for name in files:
            path = producer.safe_path(directory / name, directory=False)
            found.add(path.relative_to(bundle).as_posix())
    _require(found == expected, "native source file inventory differs")


def derive_bundle(project: Path, *, require_report: bool = False) -> dict:
    """Recompute the native source report without installation or network access.

    Every profile member and archive is bounded, hash checked and tied to the
    authenticated authored producer. Unsupported overlays and extra files reject.
    Full native rebuilding and complete preferred sources remain unqualified.
    """
    project = producer.safe_path(Path(project), directory=True)
    bundle = producer.safe_path(project / BUNDLE_PATH, directory=True)
    source = producer.verify_bundle(project, project / PRODUCER_PATH)
    source_bytes = producer.read_bytes(project / PRODUCER_PATH / "source-tree.json", producer.MAX_JSON)
    profile_bytes = producer.read_bytes(bundle / PROFILE_NAME, producer.MAX_JSON)
    profile = original._parse_json(profile_bytes, "native profile")
    _require(set(profile) == {"schema", "upstream_commit", "scope", "overlays"}
             and profile["schema"] == "rootlight.lightning-native-profile/1"
             and profile["upstream_commit"] == producer.COMMIT
             and profile["scope"] == "native binding build, Flow declarations, TypeScript checking and native binding tests",
             "unsupported native source profile")
    _require(isinstance(profile["overlays"], list), "invalid native source overlays")
    original_members = {
        item["path"][len(producer.ROOT) + 1:]: item
        for item in source["source_inventory"]["members"] if item["kind"] == "file"
    }
    overlays, inputs = {}, {PROFILE_NAME: _record(profile_bytes)}
    for item in profile["overlays"]:
        _require(isinstance(item, dict) and set(item) == {"path", "source", "sha256", "upstream_sha256"},
                 "invalid native overlay declaration")
        path, relative = item["path"], item["source"]
        original._safe_path(path, "native overlay path")
        original._safe_path(relative, "native overlay source")
        _require(path not in overlays and relative not in inputs
                 and (path in {"package.json", "yarn.lock", "Cargo.toml", "Cargo.lock", "c/Cargo.toml", "src/main.rs",
                                 "tests/cli_integration_tests.rs"}
                      or path.startswith("rootlight-patches/")), "unsupported native overlay target")
        _require(relative.startswith("overlays/") or relative.startswith("rust-patches/"),
                 "unsupported native overlay source")
        original_hash = original_members.get(path, {}).get("sha256")
        _require(item["upstream_sha256"] == original_hash, "native overlay upstream hash differs")
        payload = producer.read_bytes(bundle / relative, authored.MAX_EXPANDED)
        _require(_record(payload)["sha256"] == item["sha256"], "native overlay hash differs")
        inputs[relative] = _record(payload)
        overlays[path] = {"source_path": BUNDLE_PATH + "/" + relative, **_record(payload)}
    _require({"package.json", "yarn.lock", "Cargo.toml", "Cargo.lock"}.issubset(overlays),
             "native profile omits required executable locks")
    manifest = producer.read_bytes(project / overlays["package.json"]["source_path"], yarn.MAX_MANIFEST_BYTES)
    wanted = native_manifest(producer.read_bytes(project / PRODUCER_PATH / "tree/package.json", yarn.MAX_MANIFEST_BYTES))
    _require(original._parse_json(manifest, "native manifest") == original._parse_json(wanted, "native policy"),
             "native manifest differs from supported dependency and script roots")
    lock = producer.read_bytes(project / overlays["yarn.lock"]["source_path"], yarn.MAX_LOCK_BYTES)
    graph = yarn.parse_yarn_lock(lock, manifest)
    packages, files = [], {}
    total_bytes = total_members = 0
    for pin in graph["packages"]:
        relative = "archives/" + pin["archive_filename"]
        payload = producer.read_bytes(bundle / relative, authored.MAX_COMPRESSED)
        expanded, proof = authored._authenticate(payload, pin, authored.MAX_TOTAL_EXPANDED - total_bytes,
                                                authored.MAX_TOTAL_MEMBERS - total_members)
        total_bytes += expanded
        total_members += proof["member_count"]
        files[relative] = {**_record(payload), "mode": 0o644}
        inputs[relative] = _record(payload)
        packages.append({"pin": pin, "archive_path": relative,
                         "source_path": BUNDLE_PATH + "/" + relative, "published_package": proof})
    report = {"schema": SCHEMA, "producer": {"path": PRODUCER_PATH, "git_head": producer.COMMIT,
               "association": source["association"], "source_tree_report": _record(source_bytes)},
              "profile": profile, "inputs": inputs, "source_overlays": overlays,
              "pin_graph": graph, "packages": packages, "files": files,
              "archive_count": len(packages), "member_count": total_members,
              "original_archives_verified": True, "native_profile_sources_verified": True,
              "scope": profile["scope"], **{key: False for key in authored.FALSE_CLAIMS}}
    expected = set(inputs)
    if require_report or (bundle / REPORT_NAME).exists():
        expected.add(REPORT_NAME)
    _inventory(bundle, expected)
    if require_report:
        _require(producer.read_bytes(bundle / REPORT_NAME, authored.MAX_REPORT) == producer.json_bytes(report),
                 "native source report differs")
    for relative, record in inputs.items():
        _require(_record(producer.read_bytes(bundle / relative, authored.MAX_EXPANDED)) == record,
                 "native source input changed")
    return report


def verify_bundle(project: Path) -> dict:
    """Require the complete effective profile and its exact rederived report.

    Missing security inputs fail closed; historical original locks are never an
    automatic installation fallback. No files, processes or network are changed.
    """
    return derive_bundle(project, require_report=True)


def effective_producer(project: Path, report: dict, source_report: dict) -> tuple[dict, dict[str, bytes]]:
    """Project authenticated overlays onto original source inventory for staging.

    Returns a copied report and exact replacement bytes. Original source files
    are untouched. This projection also governs installed-byte observation.
    """
    effective = copy.deepcopy(source_report)
    payloads = {}
    members = {row["path"]: row for row in effective["source_inventory"]["members"]}
    for relative, record in report.get("source_overlays", {}).items():
        payload = producer.read_bytes(Path(project) / record["source_path"], authored.MAX_EXPANDED)
        _require(_record(payload) == {key: record[key] for key in ("bytes", "sha256")},
                 "effective producer input changed")
        payloads[relative] = payload
    if report.get("schema") == SCHEMA:
        # Cargo resolves configuration paths relative to the staged producer. An
        # absolute generated directory binds this invocation to authenticated
        # local inputs; no personal path is retained in versioned source data.
        configuration = members.get(producer.ROOT + "/.cargo/config.toml")
        original_config = b""
        if configuration is not None:
            original_config = producer.read_bytes(
                Path(project) / PRODUCER_PATH / "tree/.cargo/config.toml", producer.MAX_JSON)
            _require(_record(original_config)["sha256"] == configuration["sha256"],
                     "producer Cargo configuration changed")
            parsed = tomllib.loads(original_config.decode("utf-8"))
            _require("net" not in parsed and "source" not in parsed,
                     "upstream Cargo source or network policy requires review")
        directory = (Path(project) / "source-inputs/cargo").resolve().as_posix()
        payloads[".cargo/config.toml"] = original_config + b"\n" + (
            "[net]\noffline = true\n\n[source.crates-io]\nreplace-with = \"rootlight-local-sources\"\n\n"
            "[source.rootlight-local-sources]\ndirectory = " + json.dumps(directory) + "\n"
        ).encode()
    for relative, payload in payloads.items():
        path = producer.ROOT + "/" + relative
        members[path] = {"path": path, "kind": "file", "archive_mode": 0o644, **_record(payload)}
        for parent in PurePosixPath(path).parents:
            if str(parent) != ".":
                members.setdefault(str(parent), {"path": str(parent), "kind": "directory",
                                                  "archive_mode": 0o755, "bytes": 0})
    effective["source_inventory"]["members"] = [members[key] for key in sorted(members)]
    return effective, payloads


def main(arguments: list[str] | None = None) -> int:
    """Verify or print a freshly derived native profile report; never acquire inputs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--derive-report", action="store_true")
    options = parser.parse_args(arguments)
    try:
        report = derive_bundle(options.project, require_report=not options.derive_report)
        if options.derive_report:
            sys.stdout.buffer.write(producer.json_bytes(report))
        else:
            print(f"verified native profile: {report['archive_count']} archives, {report['member_count']} members")
        return 0
    except (ValueError, OSError, KeyError, TypeError):
        print("native producer source verification rejected", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
