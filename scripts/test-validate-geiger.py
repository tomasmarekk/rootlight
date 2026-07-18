#!/usr/bin/env python3
"""Negative controls for cargo-geiger evidence validation."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import re
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
                        },
                        "dependencies": [],
                        "dev_dependencies": [],
                        "build_dependencies": [],
                    },
                    "forbids_unsafe": True,
                }
            ],
            "packages_without_metrics": [],
        }

    def validate(
        self,
        report: dict[str, object] | None = None,
        *,
        required_cargo_id: str | None = None,
        cargo_geiger_version: object = VALIDATOR.SUPPORTED_CARGO_GEIGER_VERSION,
    ) -> int:
        return VALIDATOR.validate_report(
            self.report() if report is None else report,
            required_cargo_id or self.cargo_id,
            self.inventory,
            self.approved,
            cargo_geiger_version,
        )

    def test_exact_workspace_identity_passes(self) -> None:
        self.assertEqual(self.validate(), 1)

    def test_accepted_boundary_requires_unimplemented_authoritative_evidence(
        self,
    ) -> None:
        self.policy_path.write_text(
            self.policy_path.read_text(encoding="utf-8")
            .replace('status = "proposed"', 'status = "accepted"')
            .replace("expected_geiger_count = 0", "expected_geiger_count = 1"),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            ValueError,
            "^" + re.escape(VALIDATOR.ACCEPTED_UNSAFE_EVIDENCE_UNIMPLEMENTED) + "$",
        ):
            VALIDATOR.load_approved_counts(self.policy_path, self.inventory)

    def test_same_name_outside_workspace_is_rejected(self) -> None:
        outside = self.root / "outside"
        outside.mkdir()
        with self.assertRaisesRegex(ValueError, "outside the exact workspace"):
            self.validate(self.report(root=outside, version="999.0.0"))

    def test_quick_report_rejects_full_report_only_and_unknown_fields(self) -> None:
        report = self.report()
        report["used_but_not_scanned_files"] = []
        with self.assertRaisesRegex(ValueError, "missing or unknown fields"):
            self.validate(report)

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
            self.validate(report)

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
            self.validate(report)

    def test_required_cargo_id_must_be_exact(self) -> None:
        with self.assertRaisesRegex(ValueError, "absent from workspace inventory"):
            self.validate(
                required_cargo_id=f"{self.cargo_id}-substitute",
            )

    def test_tool_version_must_be_present_and_supported(self) -> None:
        for version in (None, "", "cargo-geiger 0.12.0", 13):
            with self.subTest(version=version):
                with self.assertRaises(ValueError):
                    self.validate(cargo_geiger_version=version)

    def test_report_security_keys_must_be_present_typed_and_exact(self) -> None:
        mutations = (
            ("missing packages", lambda report: report.pop("packages")),
            ("null packages", lambda report: report.update(packages=None)),
            (
                "renamed packages",
                lambda report: report.update(
                    package_entries=report.pop("packages")
                ),
            ),
            (
                "missing packages_without_metrics",
                lambda report: report.pop("packages_without_metrics"),
            ),
            (
                "null packages_without_metrics",
                lambda report: report.update(packages_without_metrics=None),
            ),
            ("unknown top-level", lambda report: report.update(safe=True)),
        )
        for name, mutate in mutations:
            with self.subTest(name=name):
                report = self.report()
                mutate(report)
                with self.assertRaises(ValueError):
                    self.validate(report)

    def test_entry_security_keys_must_be_present_typed_and_exact(self) -> None:
        mutations = (
            (
                "missing forbids",
                lambda entry: entry.pop("forbids_unsafe"),
            ),
            (
                "null forbids",
                lambda entry: entry.update(forbids_unsafe=None),
            ),
            (
                "string forbids",
                lambda entry: entry.update(forbids_unsafe="true"),
            ),
            (
                "renamed forbids",
                lambda entry: entry.update(
                    forbidsUnsafe=entry.pop("forbids_unsafe")
                ),
            ),
            ("unknown entry", lambda entry: entry.update(unsafety={})),
        )
        for name, mutate in mutations:
            with self.subTest(name=name):
                report = self.report()
                entry = report["packages"][0]
                mutate(entry)
                with self.assertRaises(ValueError):
                    self.validate(report)

    def test_package_schema_rejects_missing_null_renamed_and_unknown_keys(self) -> None:
        mutations = (
            ("missing id", lambda package: package.pop("id")),
            ("null id", lambda package: package.update(id=None)),
            (
                "renamed dependencies",
                lambda package: package.update(
                    deps=package.pop("dependencies")
                ),
            ),
            ("unknown package", lambda package: package.update(features=[])),
        )
        for name, mutate in mutations:
            with self.subTest(name=name):
                report = self.report()
                package = report["packages"][0]["package"]
                mutate(package)
                with self.assertRaises(ValueError):
                    self.validate(report)

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
