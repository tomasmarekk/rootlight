#!/usr/bin/env python3
"""Tests for npm publication identity and ordering checks."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import publish_npm_packages


class PublicationTests(unittest.TestCase):
    @staticmethod
    def package_manifest() -> dict[str, object]:
        return {
            "gitHead": "0" * 40,
            "license": publish_npm_packages.PACKAGE_LICENSE,
            "name": "@tomasmarekk/rootlight",
            "version": "0.1.0",
            "scripts": publish_npm_packages.ROOT_LIFECYCLE_SCRIPTS.copy(),
            "publishConfig": {"access": "public", "provenance": True},
            "repository": {
                "url": "git+https://github.com/tomasmarekk/rootlight.git"
            },
        }

    def test_publish_order_requires_platforms_before_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            records = [
                {"name": name, "directory": name.rsplit("/", 1)[-1]}
                for name in publish_npm_packages.PACKAGE_NAMES
            ]
            (root / "publish-order.json").write_text(
                json.dumps(records), encoding="utf-8"
            )
            observed = publish_npm_packages.read_publish_order(root)
            self.assertEqual(
                [record["name"] for record in observed],
                list(publish_npm_packages.PACKAGE_NAMES),
            )

    def test_root_manifest_requires_exact_lifecycle_scripts(self) -> None:
        package = self.package_manifest()
        package["scripts"] = {"postinstall": "download-binary"}
        with self.assertRaises(publish_npm_packages.NpmPublicationError):
            publish_npm_packages.validate_package_json(
                package, "@tomasmarekk/rootlight", "0.1.0", "0" * 40
            )

    def test_platform_manifest_forbids_lifecycle_scripts(self) -> None:
        package = self.package_manifest()
        package["name"] = "@tomasmarekk/rootlight-linux-x64-gnu"
        with self.assertRaises(publish_npm_packages.NpmPublicationError):
            publish_npm_packages.validate_package_json(
                package,
                "@tomasmarekk/rootlight-linux-x64-gnu",
                "0.1.0",
                "0" * 40,
            )

    def test_package_manifest_requires_the_project_license(self) -> None:
        package = self.package_manifest()
        publish_npm_packages.validate_package_json(
            package, "@tomasmarekk/rootlight", "0.1.0", "0" * 40
        )

        package["license"] = "Apache-2.0"
        with self.assertRaises(publish_npm_packages.NpmPublicationError):
            publish_npm_packages.validate_package_json(
                package, "@tomasmarekk/rootlight", "0.1.0", "0" * 40
            )

    def test_numeric_versions_are_strict(self) -> None:
        self.assertEqual(
            publish_npm_packages.numeric_version("11.15.0"), (11, 15, 0)
        )
        for value in ("v11.15.0", "11.15", "11.15.0-beta.1"):
            with self.subTest(value=value):
                with self.assertRaises(publish_npm_packages.NpmPublicationError):
                    publish_npm_packages.numeric_version(value)


if __name__ == "__main__":
    unittest.main()
