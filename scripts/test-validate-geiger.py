#!/usr/bin/env python3
"""Negative controls for cargo-geiger evidence validation."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("validate-geiger.py")
SPEC = importlib.util.spec_from_file_location("validate_geiger", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load validate-geiger.py")
VALIDATOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = VALIDATOR
SPEC.loader.exec_module(VALIDATOR)


class GeigerValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.package_root = self.root / "crates" / "rootlight-vfs"
        self.package_root.mkdir(parents=True)
        self.manifest = self.package_root / "Cargo.toml"
        self.manifest.write_text(
            '[package]\nname = "rootlight-vfs"\nversion = "0.1.0"\n',
            encoding="utf-8",
        )
        self.policy_path = self.root / "policy" / "unsafe.toml"
        self.policy_path.parent.mkdir()
        self.policy_path.write_text(
            "\n".join(
                (
                    'schema_version = "1.0"',
                    "",
                    "[[boundaries]]",
                    'package = "rootlight-vfs"',
                    'package_version = "0.1.0"',
                    'manifest = "crates/rootlight-vfs/Cargo.toml"',
                    'status = "proposed"',
                    "expected_geiger_count = 0",
                    "",
                )
            ),
            encoding="utf-8",
        )
        self.cargo_id = f"path+{self.package_root.as_uri()}#rootlight-vfs@0.1.0"
        inventory_path = self.root / "inventory.json"
        inventory_path.write_text(
            json.dumps(
                {
                    "schema_version": "1.0",
                    "workspace_members": [
                        {
                            "cargo_id": self.cargo_id,
                            "name": "rootlight-vfs",
                            "version": "0.1.0",
                            "manifest": str(self.manifest),
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.inventory = VALIDATOR.load_inventory(inventory_path)
        self.approved = VALIDATOR.load_approved_counts(
            self.policy_path, self.inventory
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def report(
        self,
        *,
        root: pathlib.Path | None = None,
        version: str = "0.1.0",
        omitted: list[str] | None = None,
    ) -> dict[str, object]:
        package_root = root or self.package_root
        return {
            "packages": [
                {
                    "package": {
                        "id": {
                            "name": "rootlight-vfs",
                            "version": version,
                            "source": {
                                "Path": f"{package_root.as_uri()}%23{version}"
                            },
                        }
                    },
                    "forbids_unsafe": True,
                }
            ],
            "packages_without_metrics": [],
            "used_but_not_scanned_files": omitted or [],
        }

    def test_exact_workspace_identity_passes(self) -> None:
        self.assertEqual(
            VALIDATOR.validate_report(
                self.report(), self.cargo_id, self.inventory, self.approved
            ),
            1,
        )

    def test_same_name_outside_workspace_is_rejected(self) -> None:
        outside = self.root / "outside"
        outside.mkdir()
        with self.assertRaisesRegex(ValueError, "outside the exact workspace"):
            VALIDATOR.validate_report(
                self.report(root=outside, version="999.0.0"),
                self.cargo_id,
                self.inventory,
                self.approved,
            )

    def test_used_but_not_scanned_files_are_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "unscanned compiler inputs"):
            VALIDATOR.validate_report(
                self.report(omitted=["/omitted/unsafe.rs"]),
                self.cargo_id,
                self.inventory,
                self.approved,
            )

    def test_workspace_package_without_metrics_is_rejected(self) -> None:
        report = self.report()
        report["packages_without_metrics"] = [
            {
                "name": "rootlight-vfs",
                "version": "0.1.0",
                "source": {"Path": self.package_root.as_uri()},
            }
        ]
        with self.assertRaisesRegex(ValueError, "omitted workspace"):
            VALIDATOR.validate_report(
                report, self.cargo_id, self.inventory, self.approved
            )

    def test_registry_parser_gap_cannot_replace_required_workspace_metrics(self) -> None:
        report = self.report()
        report["packages_without_metrics"] = [
            {
                "name": "registry-dependency",
                "version": "1.0.0",
                "source": {
                    "Registry": {
                        "name": "crates.io",
                        "url": "https://github.com/rust-lang/crates.io-index",
                    }
                },
            }
        ]
        report["packages"] = []
        with self.assertRaisesRegex(ValueError, "omitted required workspace"):
            VALIDATOR.validate_report(
                report, self.cargo_id, self.inventory, self.approved
            )

    def test_required_cargo_id_must_be_exact(self) -> None:
        with self.assertRaisesRegex(ValueError, "absent from workspace inventory"):
            VALIDATOR.validate_report(
                self.report(),
                f"{self.cargo_id}-substitute",
                self.inventory,
                self.approved,
            )

    def test_policy_manifest_must_bind_to_inventory(self) -> None:
        self.policy_path.write_text(
            self.policy_path.read_text(encoding="utf-8").replace(
                "rootlight-vfs/Cargo.toml", "substitute/Cargo.toml"
            ),
            encoding="utf-8",
        )
        substitute = self.root / "crates" / "substitute"
        substitute.mkdir()
        (substitute / "Cargo.toml").write_text("[package]\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "exact workspace package"):
            VALIDATOR.load_approved_counts(self.policy_path, self.inventory)


if __name__ == "__main__":
    unittest.main()
