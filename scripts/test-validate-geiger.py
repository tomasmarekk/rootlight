#!/usr/bin/env python3
"""Negative controls for cargo-geiger identity and evidence validation."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


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
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/rootlight-vfs"]\n',
            encoding="utf-8",
        )
        self.cargo_lock = self.root / "Cargo.lock"
        self.cargo_lock.write_text("version = 4\n", encoding="utf-8")
        self.rust_toolchain = self.root / "rust-toolchain.toml"
        self.rust_toolchain.write_text(
            '[toolchain]\nchannel = "1.97.0"\n', encoding="utf-8"
        )
        self.cargo_config = self.root / ".cargo" / "config.toml"
        self.cargo_config.parent.mkdir()
        self.cargo_config.write_text(
            '[alias]\nxtask = "run --package xtask --"\n',
            encoding="utf-8",
        )
        self.source_path = self.package_root / "src" / "lib.rs"
        self.source_path.parent.mkdir()
        self.source_path.write_text("#![forbid(unsafe_code)]\n", encoding="utf-8")

        self.policy_path = self.root / "policy" / "unsafe.toml"
        self.policy_path.parent.mkdir()
        self.policy_path.write_text(
            "\n".join(
                (
                    'schema_version = "4.0"',
                    "",
                    "[[boundaries]]",
                    'package = "rootlight-vfs"',
                    'package_version = "0.1.0"',
                    'manifest = "crates/rootlight-vfs/Cargo.toml"',
                    'module = "rootlight_vfs::platform::os"',
                    'source = "crates/rootlight-vfs/src/lib.rs"',
                    'status = "disabled"',
                    'owner = "@tomasmarekk"',
                    'reason = "native handle APIs require a reviewed boundary"',
                    "expected_source_tokens = 0",
                    "expected_geiger_count = 0",
                    'geiger_host_os = "all"',
                    "",
                )
            ),
            encoding="utf-8",
        )
        self.geiger_lock = self.root / "scripts" / "cargo-geiger-0.13.0.lock"
        self.geiger_lock.parent.mkdir()
        self.geiger_lock.write_text("version = 4\n", encoding="utf-8")
        self.geiger_lock_sha256 = hashlib.sha256(
            self.geiger_lock.read_bytes()
        ).hexdigest()
        self.geiger_patch = (
            self.root / "scripts" / "cargo-geiger-0.13.0-package-id.patch"
        )
        self.geiger_patch.write_text("reviewed package ID patch\n", encoding="utf-8")
        self.geiger_patch_sha256 = hashlib.sha256(
            self.geiger_patch.read_bytes()
        ).hexdigest()
        self.source_sha256 = "a" * 64
        self.toolchain_policy = self.root / "policy" / "toolchain.toml"
        self.toolchain_policy.write_text(
            "\n".join(
                (
                    'schema_version = "1.0"',
                    "inputs = []",
                    "",
                    "[[tools]]",
                    'name = "cargo-geiger"',
                    'version = "0.13.0"',
                    'url = "https://example.invalid/cargo-geiger-0.13.0.crate"',
                    f'sha256 = "{self.source_sha256}"',
                    'lockfile = "scripts/cargo-geiger-0.13.0.lock"',
                    f'lockfile_sha256 = "{self.geiger_lock_sha256}"',
                    'patch = "scripts/cargo-geiger-0.13.0-package-id.patch"',
                    f'patch_sha256 = "{self.geiger_patch_sha256}"',
                    'install = "verified source install"',
                    "",
                )
            ),
            encoding="utf-8",
        )

        self.binary = (self.root / "trusted" / "bin" / "cargo-geiger.exe").resolve()
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b"trusted cargo-geiger executable")
        self.receipt_path = self.binary.with_name("cargo-geiger.identity.json")
        self.receipt_path.write_text(
            json.dumps(
                {
                    "schema_version": "1.0",
                    "tool": "cargo-geiger",
                    "version": VALIDATOR.SUPPORTED_CARGO_GEIGER_VERSION,
                    "executable_sha256": hashlib.sha256(
                        self.binary.read_bytes()
                    ).hexdigest(),
                    "source_url": ("https://example.invalid/cargo-geiger-0.13.0.crate"),
                    "source_sha256": self.source_sha256,
                    "lockfile": "scripts/cargo-geiger-0.13.0.lock",
                    "lockfile_sha256": self.geiger_lock_sha256,
                    "patch": "scripts/cargo-geiger-0.13.0-package-id.patch",
                    "patch_sha256": self.geiger_patch_sha256,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )

        self.cargo_id = f"path+{self.package_root.as_uri()}#rootlight-vfs@0.1.0"
        self.inventory_path = self.root / "inventory.json"
        self.inventory_path.write_text(
            json.dumps(
                {
                    "approved_path_dependencies": [],
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
        self.inventory = VALIDATOR.load_inventory(self.inventory_path)
        self.approved = VALIDATOR.load_approved_counts(
            self.policy_path, self.inventory, "windows"
        )
        self.report_path = self.root / "report.json"
        self.write_report()
        self.execution_identity = self.root / "cargo-geiger.execution.json"
        with mock.patch.object(
            VALIDATOR, "capture_command", side_effect=self.capture_command
        ):
            VALIDATOR.prepare_cargo_geiger_execution_identity(
                self.binary,
                self.cargo_config,
                self.policy_path,
                self.toolchain_policy,
                self.execution_identity,
            )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def capture_command(
        self,
        arguments: list[str],
        description: str,
        workspace_root: pathlib.Path,
    ) -> str:
        del description, workspace_root
        if list(arguments) == [str(self.binary), "--version"]:
            return VALIDATOR.SUPPORTED_CARGO_GEIGER_VERSION
        if list(arguments) == ["cargo", "-vV"]:
            return "cargo 1.97.0 (test)\nrelease: 1.97.0"
        if list(arguments) == ["rustc", "-vV"]:
            return (
                "rustc 1.97.0 (test)\n"
                "release: 1.97.0\n"
                "host: x86_64-pc-windows-msvc"
            )
        raise AssertionError(f"unexpected command: {arguments}")

    def report(
        self,
        *,
        root: pathlib.Path | None = None,
        version: str = "0.1.0",
    ) -> dict[str, object]:
        package_root = root or self.package_root
        zero_counts = {"safe": 0, "unsafe_": 0}
        scope = {
            name: dict(zero_counts)
            for name in ("functions", "exprs", "item_impls", "item_traits", "methods")
        }
        return {
            "packages": [
                {
                    "package": {
                        "id": {
                            "name": "rootlight-vfs",
                            "version": version,
                            "source": {"Path": f"{package_root.as_uri()}%23{version}"},
                        },
                        "dependencies": [],
                        "dev_dependencies": [],
                        "build_dependencies": [],
                    },
                    "unsafety": {
                        "used": copy.deepcopy(scope),
                        "unused": copy.deepcopy(scope),
                        "forbids_unsafe": True,
                    },
                }
            ],
            "packages_without_metrics": [],
            "used_but_not_scanned_files": [],
        }

    def write_report(self, report: dict[str, object] | None = None) -> None:
        self.report_path.write_text(
            json.dumps(self.report() if report is None else report) + "\n",
            encoding="utf-8",
        )

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

    def build_envelope(self) -> dict[str, object]:
        with mock.patch.object(
            VALIDATOR, "capture_command", side_effect=self.capture_command
        ):
            return VALIDATOR.build_evidence_envelope(
                trusted_binary_path=self.binary,
                required_cargo_id=self.cargo_id,
                workspace_inventory_path=self.inventory_path,
                unsafe_policy_path=self.policy_path,
                toolchain_policy_path=self.toolchain_policy,
                cargo_lock_path=self.cargo_lock,
                cargo_config_path=self.cargo_config,
                rust_toolchain_path=self.rust_toolchain,
                execution_identity_path=self.execution_identity,
                report_path=self.report_path,
            )

    def test_exact_workspace_identity_passes(self) -> None:
        self.assertEqual(self.validate(), 1)

    def test_enabled_boundary_returns_the_exact_approved_count(self) -> None:
        self.policy_path.write_text(
            self.policy_path.read_text(encoding="utf-8")
            .replace('status = "disabled"', 'status = "enabled"')
            .replace("expected_source_tokens = 0", "expected_source_tokens = 1")
            .replace("expected_geiger_count = 0", "expected_geiger_count = 1"),
            encoding="utf-8",
        )
        self.assertEqual(
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "windows"
            ),
            {self.cargo_id: 1},
        )

    def test_multiple_enabled_boundaries_sum_for_the_current_host(self) -> None:
        second_source = self.package_root / "src" / "macos.rs"
        second_source.write_text(
            "#![allow(unsafe_code, reason = \"reviewed macOS boundary\")]\n",
            encoding="utf-8",
        )
        enabled = (
            self.policy_path.read_text(encoding="utf-8")
            .replace('status = "disabled"', 'status = "enabled"')
            .replace("expected_source_tokens = 0", "expected_source_tokens = 1")
            .replace("expected_geiger_count = 0", "expected_geiger_count = 2")
        )
        self.policy_path.write_text(
            enabled
            + "\n".join(
                (
                    "[[boundaries]]",
                    'package = "rootlight-vfs"',
                    'package_version = "0.1.0"',
                    'manifest = "crates/rootlight-vfs/Cargo.toml"',
                    'module = "rootlight_vfs::macos"',
                    'source = "crates/rootlight-vfs/src/macos.rs"',
                    'status = "enabled"',
                    'owner = "@tomasmarekk"',
                    'reason = "Darwin APIs require a reviewed boundary"',
                    "expected_source_tokens = 1",
                    "expected_geiger_count = 3",
                    'geiger_host_os = "linux"',
                    "",
                )
            ),
            encoding="utf-8",
        )

        self.assertEqual(
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "windows"
            ),
            {self.cargo_id: 2},
        )
        self.assertEqual(
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "macos"
            ),
            {self.cargo_id: 2},
        )
        self.assertEqual(
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "linux"
            ),
            {self.cargo_id: 5},
        )

    def test_uncovered_native_boundary_is_rejected(self) -> None:
        self.policy_path.write_text(
            self.policy_path.read_text(encoding="utf-8")
            .replace('status = "disabled"', 'status = "enabled"')
            .replace("expected_source_tokens = 0", "expected_source_tokens = 1")
            .replace("expected_geiger_count = 0", "expected_geiger_count = 1")
            .replace('geiger_host_os = "all"', 'geiger_host_os = "windows"'),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "lacks native CI coverage"):
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "linux"
            )

    def test_safe_package_still_requires_an_unconditional_forbid_lint(self) -> None:
        report = self.report()
        report["packages"][0]["unsafety"]["forbids_unsafe"] = False

        with self.assertRaisesRegex(ValueError, "inconsistent unsafe lint state"):
            VALIDATOR.validate_report(
                report,
                self.cargo_id,
                self.inventory,
                {},
                VALIDATOR.SUPPORTED_CARGO_GEIGER_VERSION,
            )

    def test_rustc_host_operating_system_is_exact(self) -> None:
        for host, expected in (
            ("x86_64-unknown-linux-gnu", "linux"),
            ("aarch64-apple-darwin", "macos"),
            ("x86_64-pc-windows-msvc", "windows"),
        ):
            self.assertEqual(
                VALIDATOR.rustc_host_operating_system(
                    f"rustc 1.97.0\nhost: {host}"
                ),
                expected,
            )
        for invalid in (
            "rustc 1.97.0",
            "host: x86_64-unknown-freebsd",
            "host: x86_64-unknown-linux-gnu\nhost: x86_64-pc-windows-msvc",
        ):
            with self.assertRaises(ValueError):
                VALIDATOR.rustc_host_operating_system(invalid)

    def test_enabled_boundary_requires_the_exact_full_report_count(self) -> None:
        report = self.report()
        entry = report["packages"][0]
        entry["unsafety"]["forbids_unsafe"] = False
        entry["unsafety"]["used"]["exprs"]["unsafe_"] = 1

        self.assertEqual(
            VALIDATOR.validate_report(
                report,
                self.cargo_id,
                self.inventory,
                {self.cargo_id: 1},
                VALIDATOR.SUPPORTED_CARGO_GEIGER_VERSION,
            ),
            1,
        )
        entry["unsafety"]["used"]["exprs"]["unsafe_"] = 2
        with self.assertRaisesRegex(ValueError, "expected 1 used unsafe items"):
            VALIDATOR.validate_report(
                report,
                self.cargo_id,
                self.inventory,
                {self.cargo_id: 1},
                VALIDATOR.SUPPORTED_CARGO_GEIGER_VERSION,
            )

    def test_dependency_counts_are_deferred_to_their_authoritative_report(self) -> None:
        dependency_root = self.root / "dependency"
        dependency_root.mkdir()
        dependency_root = dependency_root.resolve(strict=True)
        dependency_id = f"path+{dependency_root.as_uri()}#dependency@1.0.0"
        dependency = VALIDATOR.WorkspacePackage(
            cargo_id=dependency_id,
            name="dependency",
            version="1.0.0",
            manifest=dependency_root / "Cargo.toml",
        )
        inventory = {**self.inventory, dependency_id: dependency}
        report = self.report()
        dependency_entry = copy.deepcopy(report["packages"][0])
        dependency_entry["package"]["id"] = {
            "name": "dependency",
            "version": "1.0.0",
            "source": {"Path": f"{dependency_root.as_uri()}%231.0.0"},
        }
        report["packages"].append(dependency_entry)
        approved = {dependency_id: 7}

        self.assertEqual(
            VALIDATOR.validate_report(
                report,
                self.cargo_id,
                inventory,
                approved,
                VALIDATOR.SUPPORTED_CARGO_GEIGER_VERSION,
            ),
            2,
        )
        with self.assertRaisesRegex(ValueError, "expected 7 used unsafe items"):
            VALIDATOR.validate_report(
                report,
                dependency_id,
                inventory,
                approved,
                VALIDATOR.SUPPORTED_CARGO_GEIGER_VERSION,
            )

    def test_approved_path_dependency_sources_are_bound(self) -> None:
        dependency_root = self.root / "third_party" / "dependency"
        source_root = dependency_root / "src"
        source_root.mkdir(parents=True)
        manifest = dependency_root / "Cargo.toml"
        manifest.write_text(
            '[package]\nname = "dependency"\nversion = "1.0.0"\n',
            encoding="utf-8",
        )
        source = source_root / "lib.rs"
        source.write_text("pub fn dependency() {}\n", encoding="utf-8")
        dependency_id = f"path+{dependency_root.as_uri()}#dependency@1.0.0"
        inventory_document = json.loads(
            self.inventory_path.read_text(encoding="utf-8")
        )
        inventory_document["approved_path_dependencies"] = [
            {
                "cargo_id": dependency_id,
                "manifest": str(manifest),
                "name": "dependency",
                "version": "1.0.0",
            }
        ]
        self.inventory_path.write_text(
            json.dumps(inventory_document),
            encoding="utf-8",
        )
        inventory = VALIDATOR.load_inventory(self.inventory_path)

        manifests = VALIDATOR.workspace_manifest_evidence(inventory, self.root)
        manifest_paths = {entry["path"] for entry in manifests}
        self.assertIn("third_party/dependency/Cargo.toml", manifest_paths)
        before = VALIDATOR.workspace_source_evidence(inventory, self.root)
        source.write_text("pub fn changed_dependency() {}\n", encoding="utf-8")
        after = VALIDATOR.workspace_source_evidence(inventory, self.root)
        self.assertNotEqual(before["sha256"], after["sha256"])

    def test_same_name_outside_workspace_is_rejected(self) -> None:
        outside = self.root / "outside"
        outside.mkdir()
        with self.assertRaisesRegex(ValueError, "outside the exact workspace"):
            self.validate(self.report(root=outside, version="999.0.0"))

    def test_safety_report_rejects_unknown_fields(self) -> None:
        report = self.report()
        report["unknown"] = []
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

    def test_registry_parser_gap_cannot_replace_required_workspace_metrics(
        self,
    ) -> None:
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
            self.validate(required_cargo_id=f"{self.cargo_id}-substitute")

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
                lambda report: report.update(package_entries=report.pop("packages")),
            ),
            (
                "missing packages_without_metrics",
                lambda report: report.pop("packages_without_metrics"),
            ),
            (
                "null packages_without_metrics",
                lambda report: report.update(packages_without_metrics=None),
            ),
            (
                "missing used_but_not_scanned_files",
                lambda report: report.pop("used_but_not_scanned_files"),
            ),
            (
                "null used_but_not_scanned_files",
                lambda report: report.update(used_but_not_scanned_files=None),
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
                lambda entry: entry["unsafety"].pop("forbids_unsafe"),
            ),
            (
                "null forbids",
                lambda entry: entry["unsafety"].update(forbids_unsafe=None),
            ),
            (
                "string forbids",
                lambda entry: entry["unsafety"].update(forbids_unsafe="true"),
            ),
            (
                "renamed forbids",
                lambda entry: entry["unsafety"].update(
                    forbidsUnsafe=entry["unsafety"].pop("forbids_unsafe")
                ),
            ),
            ("missing unsafety", lambda entry: entry.pop("unsafety")),
            ("null unsafety", lambda entry: entry.update(unsafety=None)),
            ("unknown entry", lambda entry: entry.update(unknown={})),
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
                lambda package: package.update(deps=package.pop("dependencies")),
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
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "windows"
            )

    def test_inventory_root_rejects_unknown_fields(self) -> None:
        document = json.loads(self.inventory_path.read_text(encoding="utf-8"))
        document["unexpected"] = True
        self.inventory_path.write_text(json.dumps(document), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "missing or unknown fields"):
            VALIDATOR.load_inventory(self.inventory_path)

    def test_policy_root_rejects_unknown_fields(self) -> None:
        self.policy_path.write_text(
            self.policy_path.read_text(encoding="utf-8").replace(
                'schema_version = "4.0"\n',
                'schema_version = "4.0"\nunexpected = true\n',
                1,
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "missing or unknown fields"):
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "windows"
            )

    def test_boundary_rejects_unknown_fields(self) -> None:
        self.policy_path.write_text(
            self.policy_path.read_text(encoding="utf-8") + "unexpected = true\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "missing or unknown fields"):
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "windows"
            )

    def test_boundary_rejects_unknown_geiger_host(self) -> None:
        self.policy_path.write_text(
            self.policy_path.read_text(encoding="utf-8").replace(
                'geiger_host_os = "all"', 'geiger_host_os = "freebsd"'
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "invalid cargo-geiger host"):
            VALIDATOR.load_approved_counts(
                self.policy_path, self.inventory, "windows"
            )

    def test_evidence_root_and_nested_objects_reject_unknown_fields(self) -> None:
        envelope = self.build_envelope()
        mutations = (
            lambda document: document.update(unexpected=True),
            lambda document: document["source_inputs"].update(unexpected=True),
            lambda document: document["cargo_geiger"].update(unexpected=True),
            lambda document: document["scanner_execution"].update(unexpected=True),
            lambda document: document["rust_toolchain"].update(unexpected=True),
            lambda document: document["report"].update(unexpected=True),
            lambda document: document["workspace_manifests"][0].update(unexpected=True),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                candidate = copy.deepcopy(envelope)
                mutate(candidate)
                with self.assertRaisesRegex(ValueError, "missing or unknown fields"):
                    VALIDATOR.validate_evidence_envelope(candidate)

    def test_full_report_evidence_is_authoritative_for_enabled_boundary(self) -> None:
        envelope = self.build_envelope()
        self.assertTrue(envelope["report"]["authoritative_for_enabled_boundary"])
        self.assertFalse(envelope["source_inputs"]["authoritative_for_enabled_boundary"])
        self.assertFalse(envelope["source_inputs"]["compiler_expanded"])

    def test_repository_cargo_alias_is_rejected_before_any_tool_executes(self) -> None:
        self.cargo_config.write_text(
            '[alias]\ngeiger = "run --package fake-scanner --"\n',
            encoding="utf-8",
        )
        with mock.patch.object(VALIDATOR.subprocess, "run") as runner:
            with self.assertRaisesRegex(ValueError, "alias 'geiger' is forbidden"):
                VALIDATOR.prepare_cargo_geiger_execution_identity(
                    self.binary,
                    self.cargo_config,
                    self.policy_path,
                    self.toolchain_policy,
                    self.execution_identity,
                )
            runner.assert_not_called()

    def test_environment_cargo_alias_is_rejected_before_any_tool_executes(self) -> None:
        with (
            mock.patch.dict(
                os.environ, {"CARGO_ALIAS_GEIGER": "run --package fake-scanner --"}
            ),
            mock.patch.object(VALIDATOR.subprocess, "run") as runner,
        ):
            with self.assertRaisesRegex(ValueError, "CARGO_ALIAS_GEIGER is forbidden"):
                VALIDATOR.prepare_cargo_geiger_execution_identity(
                    self.binary,
                    self.cargo_config,
                    self.policy_path,
                    self.toolchain_policy,
                    self.execution_identity,
                )
            runner.assert_not_called()

    def test_trusted_tool_path_must_be_canonical_absolute_regular_file(self) -> None:
        with self.assertRaisesRegex(ValueError, "absolute path"):
            VALIDATOR.trusted_cargo_geiger_binary(pathlib.Path("cargo-geiger.exe"))
        with self.assertRaisesRegex(ValueError, "regular file"):
            VALIDATOR.trusted_cargo_geiger_binary(self.binary.parent)

    def test_trusted_tool_symlink_is_rejected(self) -> None:
        alias = self.root / "alias" / "cargo-geiger.exe"
        alias.parent.mkdir()
        try:
            alias.symlink_to(self.binary)
        except OSError:
            self.skipTest("filesystem does not permit symlink creation")
        with self.assertRaisesRegex(ValueError, "symlink or reparse"):
            VALIDATOR.trusted_cargo_geiger_binary(alias)

    def test_install_identity_symlink_is_rejected(self) -> None:
        real_receipt = self.receipt_path.with_suffix(".real.json")
        self.receipt_path.replace(real_receipt)
        try:
            self.receipt_path.symlink_to(real_receipt)
        except OSError:
            real_receipt.replace(self.receipt_path)
            self.skipTest("filesystem does not permit symlink creation")
        with self.assertRaisesRegex(ValueError, "symlink or reparse"):
            VALIDATOR.current_cargo_geiger_execution_identity(self.binary)

    def test_tool_path_substitution_cannot_use_fake_version_or_report(self) -> None:
        substitute = (self.root / "substitute" / "cargo-geiger.exe").resolve()
        substitute.parent.mkdir()
        substitute.write_bytes(b"fake scanner")
        substitute.with_name("cargo-geiger.identity.json").write_text(
            self.receipt_path.read_text(encoding="utf-8"), encoding="utf-8"
        )
        with mock.patch.object(VALIDATOR.subprocess, "run") as runner:
            with self.assertRaisesRegex(ValueError, "differs from preflight identity"):
                VALIDATOR.verify_cargo_geiger_execution_identity(
                    substitute,
                    self.cargo_config,
                    self.policy_path,
                    self.toolchain_policy,
                    self.execution_identity,
                )
            runner.assert_not_called()

    def test_executable_digest_mutation_fails_closed(self) -> None:
        self.binary.write_bytes(b"mutated scanner")
        with self.assertRaisesRegex(ValueError, "differs from preflight identity"):
            self.build_envelope()

    def test_bound_input_mutations_invalidate_the_evidence_envelope(self) -> None:
        mutations = (
            ("Cargo.lock", self.cargo_lock, "version = 4\n# mutated\n"),
            (
                "Cargo config",
                self.cargo_config,
                '[alias]\nxtask = "run --package xtask --"\n# mutated\n',
            ),
            (
                "unsafe policy",
                self.policy_path,
                self.policy_path.read_text(encoding="utf-8") + "# mutated\n",
            ),
            (
                "toolchain policy",
                self.toolchain_policy,
                self.toolchain_policy.read_text(encoding="utf-8") + "# mutated\n",
            ),
            (
                "Rust toolchain file",
                self.rust_toolchain,
                '[toolchain]\nchannel = "1.97.0"\n# mutated\n',
            ),
            (
                "workspace inventory",
                self.inventory_path,
                json.dumps(
                    json.loads(self.inventory_path.read_text(encoding="utf-8")),
                    indent=2,
                )
                + "\n",
            ),
            (
                "root workspace manifest",
                self.root / "Cargo.toml",
                '[workspace]\nmembers = ["crates/rootlight-vfs"]\n# mutated\n',
            ),
            (
                "workspace manifest",
                self.manifest,
                '[package]\nname = "rootlight-vfs"\nversion = "0.1.0"\n# mutated\n',
            ),
            (
                "workspace source",
                self.source_path,
                "#![forbid(unsafe_code)]\n// mutation\n",
            ),
            (
                "report",
                self.report_path,
                json.dumps(self.report(), indent=2) + "\n",
            ),
        )
        for name, path, contents in mutations:
            with self.subTest(name=name):
                original_contents = path.read_text(encoding="utf-8")
                observed = self.build_envelope()
                path.write_text(contents, encoding="utf-8")
                expected = self.build_envelope()
                with self.assertRaisesRegex(ValueError, "evidence envelope mismatch"):
                    VALIDATOR.verify_evidence_envelope(observed, expected)
                path.write_text(original_contents, encoding="utf-8")

    def test_tool_lock_mutation_fails_closed(self) -> None:
        self.geiger_lock.write_text("version = 4\n# mutated\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "digest does not match"):
            self.build_envelope()

    def test_tool_patch_mutation_fails_closed(self) -> None:
        self.geiger_patch.write_text("mutated package ID patch\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "digest does not match"):
            self.build_envelope()

    def test_toolchain_identity_mutation_invalidates_envelope(self) -> None:
        observed = self.build_envelope()

        def changed_capture(
            arguments: list[str],
            description: str,
            workspace_root: pathlib.Path,
        ) -> str:
            if list(arguments) == ["cargo", "-vV"]:
                return "cargo 1.98.0 (substitute)\nrelease: 1.98.0"
            return self.capture_command(arguments, description, workspace_root)

        with mock.patch.object(
            VALIDATOR, "capture_command", side_effect=changed_capture
        ):
            expected = VALIDATOR.build_evidence_envelope(
                trusted_binary_path=self.binary,
                required_cargo_id=self.cargo_id,
                workspace_inventory_path=self.inventory_path,
                unsafe_policy_path=self.policy_path,
                toolchain_policy_path=self.toolchain_policy,
                cargo_lock_path=self.cargo_lock,
                cargo_config_path=self.cargo_config,
                rust_toolchain_path=self.rust_toolchain,
                execution_identity_path=self.execution_identity,
                report_path=self.report_path,
            )
        with self.assertRaisesRegex(ValueError, "rust_toolchain"):
            VALIDATOR.verify_evidence_envelope(observed, expected)

    def test_direct_scanner_argv_is_frozen_without_cargo_subcommand(self) -> None:
        expected = [
            str(self.binary),
            "--manifest-path",
            str(self.manifest.resolve()),
            "--all-features",
            "--all-targets",
            "--all-dependencies",
            "--locked",
            "--offline",
            "--output-format",
            "Json",
        ]
        observed = VALIDATOR.cargo_geiger_report_argv(
            self.binary, self.manifest.resolve()
        )
        self.assertEqual(observed, expected)
        self.assertNotIn("geiger", observed[1:])

    def test_report_is_published_only_after_successful_postcheck(self) -> None:
        output = self.root / "output"
        output.mkdir()
        report_output = output / "report.json"
        log_output = output / "report.log"

        def run_scanner(
            arguments: list[str], **kwargs: object
        ) -> subprocess.CompletedProcess:
            kwargs["stdout"].write(json.dumps(self.report()).encode("utf-8"))
            kwargs["stderr"].write(b"scanner log")
            return subprocess.CompletedProcess(arguments, 0)

        with (
            mock.patch.object(
                VALIDATOR, "capture_command", side_effect=self.capture_command
            ),
            mock.patch.object(VALIDATOR.subprocess, "run", side_effect=run_scanner),
        ):
            VALIDATOR.scan_with_trusted_cargo_geiger(
                trusted_binary_path=self.binary,
                manifest_path=self.manifest.resolve(),
                cargo_config_path=self.cargo_config,
                unsafe_policy_path=self.policy_path,
                toolchain_policy_path=self.toolchain_policy,
                execution_identity_path=self.execution_identity,
                report_path=report_output,
                log_path=log_output,
            )
        self.assertEqual(json.loads(report_output.read_text()), self.report())
        self.assertEqual(log_output.read_text(), "scanner log")

    def test_postcheck_rejects_executable_swap_without_publishing_output(self) -> None:
        output = self.root / "output"
        output.mkdir()
        report_output = output / "report.json"
        log_output = output / "report.log"

        def swap_scanner(
            arguments: list[str], **kwargs: object
        ) -> subprocess.CompletedProcess:
            kwargs["stdout"].write(json.dumps(self.report()).encode("utf-8"))
            self.binary.write_bytes(b"scanner swapped during execution")
            return subprocess.CompletedProcess(arguments, 0)

        with (
            mock.patch.object(
                VALIDATOR, "capture_command", side_effect=self.capture_command
            ),
            mock.patch.object(VALIDATOR.subprocess, "run", side_effect=swap_scanner),
        ):
            with self.assertRaisesRegex(ValueError, "differs from preflight identity"):
                VALIDATOR.scan_with_trusted_cargo_geiger(
                    trusted_binary_path=self.binary,
                    manifest_path=self.manifest.resolve(),
                    cargo_config_path=self.cargo_config,
                    unsafe_policy_path=self.policy_path,
                    toolchain_policy_path=self.toolchain_policy,
                    execution_identity_path=self.execution_identity,
                    report_path=report_output,
                    log_path=log_output,
                )
        self.assertFalse(report_output.exists())
        self.assertFalse(log_output.exists())


if __name__ == "__main__":
    unittest.main()
