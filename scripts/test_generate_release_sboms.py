#!/usr/bin/env python3
"""Focused tests for npm and static-asset release SBOM inventory."""

from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).with_name("generate-release-sboms.py")
SPEC = importlib.util.spec_from_file_location("generate_release_sboms", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
release_sboms = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release_sboms)


class ReleaseSbomTests(unittest.TestCase):
    def test_npm_inventory_includes_production_and_build_dependencies(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            frontend = workspace / "apps" / "rootlight-web" / "frontend"
            frontend.mkdir(parents=True)
            package = {
                "name": "@rootlight/web-ui",
                "version": "0.1.0",
                "dependencies": {"runtime": "1.0.0"},
                "devDependencies": {"builder": "2.0.0"},
            }
            lock = {
                "name": "@rootlight/web-ui",
                "version": "0.1.0",
                "lockfileVersion": 3,
                "packages": {
                    "": package,
                    "node_modules/builder": {
                        "version": "2.0.0",
                        "dev": True,
                        "license": "Apache-2.0",
                        "resolved": "https://registry.npmjs.org/builder/-/builder-2.0.0.tgz",
                        "integrity": _integrity(b"builder"),
                    },
                    "node_modules/runtime": {
                        "version": "1.0.0",
                        "license": "MIT",
                        "resolved": "https://registry.npmjs.org/runtime/-/runtime-1.0.0.tgz",
                        "integrity": _integrity(b"runtime"),
                    },
                },
            }
            (frontend / "package.json").write_text(
                json.dumps(package), encoding="utf-8"
            )
            (frontend / "package-lock.json").write_text(
                json.dumps(lock), encoding="utf-8"
            )

            components, dependencies, root = release_sboms.npm_components(workspace)

            self.assertEqual(len(components), 3)
            self.assertEqual(root["name"], "@rootlight/web-ui")
            root_edges = dependencies[root["bom-ref"]]["dependsOn"]
            self.assertEqual(len(root_edges), 2)
            licenses = {
                item["licenses"][0]["expression"]
                for item in components.values()
                if "licenses" in item
            }
            self.assertEqual(licenses, {"Apache-2.0", "MIT"})

    def test_web_asset_inventory_is_exact_and_hash_bound(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "dist"
            root.mkdir()
            index = b'<!doctype html><script src="/assets/app-a1b2c3d4.js"></script>'
            script = b"export {};"
            (root / "assets").mkdir()
            (root / "assets" / "app-a1b2c3d4.js").write_bytes(script)
            (root / "index.html").write_bytes(index)
            records = [
                _asset_record("assets/app-a1b2c3d4.js", script),
                _asset_record("index.html", index),
            ]
            (root / "asset-manifest.json").write_text(
                json.dumps({"schema_version": 1, "assets": records}),
                encoding="utf-8",
            )
            notices = Path(temporary) / "notices.txt"
            notices.write_text("runtime 1.0.0 - MIT\n", encoding="utf-8")

            components = release_sboms.web_asset_components(
                root, notices, "x86_64-unknown-linux-gnu"
            )

            self.assertEqual(len(components), 4)
            self.assertEqual(
                {component["name"] for component in components},
                {
                    "licenses/rootlight-web-third-party-notices.txt",
                    "share/rootlight/web/asset-manifest.json",
                    "share/rootlight/web/assets/app-a1b2c3d4.js",
                    "share/rootlight/web/index.html",
                },
            )

            (root / "assets" / "foreign-a1b2c3d4.css").write_bytes(b"body{}")
            with self.assertRaisesRegex(ValueError, "differs from its manifest"):
                release_sboms.web_asset_components(
                    root, notices, "x86_64-unknown-linux-gnu"
                )


def _integrity(content: bytes) -> str:
    digest = hashlib.sha512(content).digest()
    return f"sha512-{base64.b64encode(digest).decode('ascii')}"


def _asset_record(path: str, content: bytes) -> dict[str, object]:
    return {
        "path": path,
        "bytes": len(content),
        "sha256": hashlib.sha256(content).hexdigest(),
    }


if __name__ == "__main__":
    unittest.main()
