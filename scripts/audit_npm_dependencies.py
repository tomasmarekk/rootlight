#!/usr/bin/env python3
"""Audit every executable npm pin against the public npm bulk advisory endpoint.

The frontend's registry witness and the native producer's effective Yarn graph
include development and optional packages. Published nested example manifests
are provenance, not additional installation roots. No package code is executed,
installed or downloaded. A malformed or unavailable advisory response fails.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from urllib.request import Request, urlopen

import verify_local_npm_sources as frontend
import verify_lightning_security_sources as native

ENDPOINT = "https://registry.npmjs.org/-/npm/v1/security/advisories/bulk"
MAX_RESPONSE = 16 * 1024 * 1024


def package_inventory(project: Path) -> dict[str, list[str]]:
    """Collect pinned versions without omitting development or platform packages.

    Canonical frontend local paths are checked against the registry witness by
    the existing verifier. The native verifier reauthenticates profile, overlays
    and archives before its effective graph can enter this advisory request.
    """
    frontend.verify_local_bundle(project)
    upstream = json.loads((project / "source-inputs/npm/upstream-package-lock.json").read_bytes())
    versions: dict[str, set[str]] = {}
    for location, row in upstream["packages"].items():
        if not location:
            continue
        name = row.get("name") or location.rsplit("node_modules/", 1)[-1]
        versions.setdefault(name, set()).add(row["version"])
    graph = native.verify_bundle(project)["pin_graph"]
    for row in graph["packages"]:
        versions.setdefault(row["name"], set()).add(row["version"])
    if not versions or any(not name or any(not version for version in pins) for name, pins in versions.items()):
        raise ValueError("empty npm advisory package identity")
    return {name: sorted(pins) for name, pins in sorted(versions.items())}


def query_advisories(inventory: dict[str, list[str]]) -> dict:
    """Query only known registry package names and exact locked versions over TLS.

    The response map supplies each package identity; an optional record name
    must agree with it. Every severity fails, with no suppression list.
    HTTP failures and bounded JSON validation failures propagate to the caller.
    """
    payload = json.dumps(inventory, sort_keys=True).encode()
    request = Request(ENDPOINT, data=payload, headers={"Content-Type": "application/json"}, method="POST")
    with urlopen(request, timeout=60) as response:
        if response.geturl() != ENDPOINT:
            raise ValueError("npm advisory endpoint redirected")
        contents = response.read(MAX_RESPONSE + 1)
    if len(contents) > MAX_RESPONSE:
        raise ValueError("npm advisory response exceeds limit")
    result = json.loads(contents)
    if not isinstance(result, dict):
        raise ValueError("invalid npm advisory response")
    for name, advisories in result.items():
        if name not in inventory or not isinstance(advisories, list) or not advisories:
            raise ValueError("invalid npm advisory package")
        for row in advisories:
            if (not isinstance(row, dict) or ("name" in row and row["name"] != name)
                or not isinstance(row.get("id"), int)
                or not isinstance(row.get("vulnerable_versions"), str)):
                raise ValueError("invalid npm advisory record")
    return result


def main(arguments: list[str] | None = None) -> int:
    """Print the full advisory result and fail on any finding or incomplete audit."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, default=Path(__file__).resolve().parents[1])
    options = parser.parse_args(arguments)
    try:
        inventory = package_inventory(options.project.resolve())
        advisories = query_advisories(inventory)
        print(json.dumps({"package_names": len(inventory), "pinned_versions": sum(map(len, inventory.values())),
                          "development_and_optional_packages_included": True, "advisories": advisories},
                         sort_keys=True, indent=2))
        return 1 if advisories else 0
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"npm advisory audit failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
