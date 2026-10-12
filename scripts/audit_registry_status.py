#!/usr/bin/env python3
"""Acquire registry metadata separately, then verify locked withdrawal status offline.

No package archives or build inputs are downloaded. The sparse-index snapshot is
an audit input: every active registry version must have one checksum-matching,
non-yanked record. Missing lookup results never count as a successful audit.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import time
import tomllib
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


LOCKFILES = (
    "Cargo.lock",
    "tests/native-grammars/Cargo.lock",
    "fuzz/Cargo.lock",
    "source-inputs/producer-security/lightningcss-1d680fa14e9a089c92dc0929f869d6757ae91c30/overlays/Cargo.lock",
)
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
SCHEMA = "rootlight.registry-status-snapshot/1"
MAX_BYTES = 16 * 1024 * 1024
MAX_TOTAL_BYTES = 256 * 1024 * 1024
MAX_AGE = timedelta(hours=24)
NAME = re.compile(r"[a-z0-9][a-z0-9_-]*\Z")
DIGEST = re.compile(r"[0-9a-f]{64}\Z")


def read_file(path: Path) -> bytes:
    """Bound reads from ordinary local audit files."""
    if path.is_symlink():
        raise ValueError(f"symbolic-link audit input: {path.name}")
    with path.open("rb") as source:
        metadata = os.fstat(source.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_BYTES:
            raise ValueError(f"invalid audit input size or type: {path.name}")
        payload = source.read(MAX_BYTES + 1)
    if len(payload) > MAX_BYTES:
        raise ValueError("audit input exceeds the byte limit")
    return payload


def lock_inventory(project: Path) -> tuple[list[dict], list[dict]]:
    """Bind every registry package to exact lock bytes and publication checksum."""
    locks, packages = [], {}
    for relative in LOCKFILES:
        payload = read_file(project / relative)
        locks.append({"path": relative, "sha256": hashlib.sha256(payload).hexdigest()})
        seen = set()
        for package in tomllib.loads(payload.decode("utf-8"))["package"]:
            if "source" not in package:
                continue
            if package["source"] != REGISTRY:
                raise ValueError("unsupported registry or Git source in an active lock")
            name, version, checksum = package["name"], package["version"], package.get("checksum", "")
            if not NAME.fullmatch(name) or not isinstance(version, str) or not DIGEST.fullmatch(checksum):
                raise ValueError("invalid registry package identity or checksum")
            key = (name, version)
            if key in seen:
                raise ValueError("duplicate registry identity in a lock")
            seen.add(key)
            record = packages.setdefault(key, {"name": name, "version": version, "checksum": checksum})
            if record["checksum"] != checksum:
                raise ValueError("conflicting checksums across active locks")
    if not packages:
        raise ValueError("active lock inventory has no registry packages")
    return locks, [packages[key] for key in sorted(packages)]


def index_url(name: str) -> str:
    if not NAME.fullmatch(name):
        raise ValueError("invalid sparse-index package name")
    if len(name) < 3:
        relative = f"{len(name)}/{name}"
    elif len(name) == 3:
        relative = f"3/{name[0]}/{name}"
    else:
        relative = f"{name[:2]}/{name[2:4]}/{name}"
    return "https://index.crates.io/" + relative


def selected_versions(payload: bytes, name: str, required: list[dict]) -> list[dict]:
    """Reject incomplete or ambiguous index records before reading yank status."""
    if len(payload) > MAX_BYTES:
        raise ValueError("sparse-index response exceeds the byte limit")
    versions = {}
    for line in payload.decode("utf-8").splitlines():
        row = json.loads(line)
        if not isinstance(row, dict) or row.get("name") != name or not isinstance(row.get("vers"), str):
            raise ValueError(f"malformed sparse-index record for {name}")
        version = row["vers"]
        if version in versions:
            raise ValueError(f"duplicate sparse-index version for {name}")
        versions[version] = row
    selected = []
    for package in required:
        row = versions.get(package["version"])
        if row is None:
            raise ValueError(f"missing locked sparse-index version: {name}@{package['version']}")
        if row.get("cksum") != package["checksum"]:
            raise ValueError(f"registry checksum differs from active lock: {name}@{package['version']}")
        if type(row.get("yanked")) is not bool:
            raise ValueError(f"missing or malformed yanked status: {name}@{package['version']}")
        selected.append({**package, "yanked": row["yanked"]})
    return selected


def fetch_index(name: str) -> bytes:
    """Fetch only canonical official metadata, with bounded reads and retries."""
    url = index_url(name)
    request = Request(url, headers={"User-Agent": "rootlight-dependency-audit/1", "Accept": "text/plain"})
    for attempt in range(3):
        try:
            with urlopen(request, timeout=30) as response:
                if response.status != 200 or response.geturl() != url:
                    raise ValueError("unexpected sparse-index endpoint or response")
                length = response.headers.get("Content-Length")
                if length is not None and (int(length) < 0 or int(length) > MAX_BYTES):
                    raise ValueError("sparse-index response exceeds the byte limit")
                payload = response.read(MAX_BYTES + 1)
                if len(payload) > MAX_BYTES:
                    raise ValueError("sparse-index response exceeds the byte limit")
                return payload
        except HTTPError as error:
            if error.code not in {429, 500, 502, 503, 504} or attempt == 2:
                raise ValueError(f"registry metadata request failed for {name}: HTTP {error.code}") from error
        except (URLError, TimeoutError) as error:
            if attempt == 2:
                raise ValueError(f"registry metadata request failed for {name}") from error
        time.sleep(attempt + 1)
    raise AssertionError("unreachable retry state")


def acquire(project: Path, output: Path) -> dict:
    """Capture a fresh metadata snapshot without changing Cargo inputs or caches."""
    captured_at = datetime.now(timezone.utc).isoformat()
    locks, packages = lock_inventory(project)
    names = sorted({package["name"] for package in packages})
    output.mkdir(parents=True, exist_ok=False)
    (output / "entries").mkdir()
    entries = []
    received_bytes = 0
    with ThreadPoolExecutor(max_workers=6) as executor:
        pending = {executor.submit(fetch_index, name): name for name in names}
        for future in as_completed(pending):
            # Release completed futures so the acquisition does not retain every
            # response body in memory after writing its authenticated snapshot.
            name, payload = pending.pop(future), future.result()
            received_bytes += len(payload)
            if received_bytes > MAX_TOTAL_BYTES:
                for queued in pending:
                    queued.cancel()
                raise ValueError("registry metadata exceeds the total snapshot byte limit")
            selected_versions(payload, name, [package for package in packages if package["name"] == name])
            relative = f"entries/{name}.jsonl"
            (output / relative).write_bytes(payload)
            entries.append({"name": name, "path": relative, "url": index_url(name),
                            "sha256": hashlib.sha256(payload).hexdigest(), "bytes": len(payload)})
            if len(entries) % 50 == 0:
                print(f"captured {len(entries)}/{len(names)} registry metadata entries", flush=True)
    if lock_inventory(project) != (locks, packages):
        raise ValueError("active lock inputs changed during metadata acquisition")
    report = {"schema": SCHEMA, "captured_at": captured_at,
              "locks": locks, "packages": packages, "entries": sorted(entries, key=lambda entry: entry["name"])}
    (output / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return report


def verify(project: Path, directory: Path, *, now: datetime | None = None) -> dict:
    """Verify complete, recent status from local files; this function never fetches."""
    if directory.is_symlink() or (directory / "entries").is_symlink():
        raise ValueError("symbolic-link audit snapshot directory")
    report = json.loads(read_file(directory / "report.json"))
    if report.get("schema") != SCHEMA:
        raise ValueError("unsupported registry status snapshot")
    captured = datetime.fromisoformat(report["captured_at"])
    current = now or datetime.now(timezone.utc)
    if captured.tzinfo is None or not -timedelta(minutes=5) <= current - captured <= MAX_AGE:
        raise ValueError("registry metadata snapshot is stale or has an invalid timestamp")
    locks, packages = lock_inventory(project)
    if report["locks"] != locks or report["packages"] != packages:
        raise ValueError("registry metadata snapshot does not match the active locks")
    expected_names = {package["name"] for package in packages}
    entries = report["entries"]
    if len(entries) != len(expected_names) or {entry["name"] for entry in entries} != expected_names:
        raise ValueError("registry metadata inventory is incomplete or duplicated")
    expected_files = {"report.json"} | {f"entries/{name}.jsonl" for name in expected_names}
    observed_files = set()
    for path in directory.rglob("*"):
        if path.is_symlink():
            raise ValueError("symbolic-link audit snapshot member")
        if path.is_file():
            observed_files.add(path.relative_to(directory).as_posix())
        elif path != directory / "entries":
            raise ValueError("unexpected audit snapshot directory or special file")
    if observed_files != expected_files:
        raise ValueError("registry metadata file inventory differs from its complete report")
    checked = []
    for entry in entries:
        name = entry["name"]
        if entry["path"] != f"entries/{name}.jsonl" or entry["url"] != index_url(name):
            raise ValueError("noncanonical registry metadata source")
        payload = read_file(directory / entry["path"])
        if len(payload) != entry["bytes"] or hashlib.sha256(payload).hexdigest() != entry["sha256"]:
            raise ValueError("registry metadata payload changed")
        checked.extend(selected_versions(payload, name, [package for package in packages if package["name"] == name]))
    yanked = [f"{package['name']}@{package['version']}" for package in checked if package["yanked"]]
    if yanked:
        raise ValueError("locked registry versions are yanked: " + ", ".join(yanked))
    return {"locks": len(locks), "packages": len(checked), "entries": len(entries), "yanked": 0,
            "captured_at": report["captured_at"]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, default=Path(__file__).resolve().parents[1])
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("acquire", help="fetch registry metadata only into a new directory")
    prepare.add_argument("--output", type=Path, required=True)
    validate = commands.add_parser("verify", help="verify complete snapshot without network access")
    validate.add_argument("--input", type=Path, required=True)
    options = parser.parse_args()
    try:
        if options.command == "acquire":
            report = acquire(options.project, options.output)
            print(f"captured {len(report['packages'])} locked versions from {len(report['entries'])} registry entries")
        else:
            print(json.dumps(verify(options.project, options.input), sort_keys=True))
        return 0
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"registry status audit rejected: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
