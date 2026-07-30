#!/usr/bin/env python3
"""Regression tests for installed package lifecycle validation."""

from __future__ import annotations

import copy
import importlib.util
import unittest
from pathlib import Path
from types import ModuleType


def _load_validator() -> ModuleType:
    path = Path(__file__).with_name("validate-package-lifecycle.py")
    specification = importlib.util.spec_from_file_location(
        "validate_package_lifecycle",
        path,
    )
    if specification is None or specification.loader is None:
        raise RuntimeError("package lifecycle validator could not be loaded")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


VALIDATOR = _load_validator()
REVISION = "a" * 40
SHA256 = "b" * 64
VERSION = "0.1.0"


def _mcp(cleanup: int | None) -> dict[str, object]:
    measured = list(range(1_000, 101_000, 1_000))
    return {
        "warmup_samples_micros": [1_000] * 5,
        "measured_samples_micros": measured,
        "p50_micros": 50_000,
        "p95_micros": 95_000,
        "p99_micros": 99_000,
        "p50_limit_micros": 80_000,
        "p95_limit_micros": 150_000,
        "p99_limit_micros": 300_000,
        "successful_warmups": 5,
        "successful_measurements": 100,
        "launcher_exit_count": 105,
        "daemon_exit_count": 1,
        "steady_state_active_processes": 2 if cleanup is not None else None,
        "post_cleanup_active_processes": cleanup,
    }


def _document(target: str) -> dict[str, object]:
    windows = target == "x86_64-pc-windows-msvc"
    health: dict[str, object] | None = None
    if windows:
        health = {
            "limit_micros": 3_000_000,
            "samples_micros": [1_000_000] * 10,
            "successful_attempts": 10,
            "launcher_exit_count": 10,
            "stdout_eof_count": 10,
            "stderr_eof_count": 10,
            "pre_cleanup_active_processes": [2] * 10,
            "post_cleanup_active_processes": 0,
        }
    return {
        "schema": "rootlight.package-lifecycle/4",
        "source_revision": REVISION,
        "candidate_target": target,
        "candidate_version": VERSION,
        "baseline_version": "0.0.0",
        "candidate_archive": f"rootlight-{VERSION}-{target}.zip",
        "candidate_sha256": SHA256,
        "baseline_archive": f"rootlight-0.0.0-{target}.zip",
        "baseline_sha256": "c" * 64,
        "bootstrap_owned_files": 5,
        "committed_active_version": VERSION,
        "committed_last_good_version": "0.0.0",
        "rollback_active_version": "0.0.0",
        "uninstall_removed_versions": 2,
        "installed_release": {
            "windows_first_health": health,
            "mcp_initialize": _mcp(0 if windows else None),
            "lazy_payload_handoff_observed": True if windows else None,
            "mcp_vertical": {
                "malformed_partial_result_observed": True,
                "malformed_incomplete_coverage_observed": True,
                "syntax_recovery_diagnostic_observed": True,
                "incremental_lineage_observed": True,
            },
        },
        "installed_command_uninstall_observed": True,
        "launcher_probe_observed": True,
        "candidate_health_observed": True,
        "failed_health_rollback_observed": True,
        "clean_recovery_observed": True,
        "user_data_preserved": True,
        "unowned_data_preserved": True,
    }


def _validate(document: dict[str, object], target: str) -> None:
    VALIDATOR.validate_document(
        document,
        target=target,
        candidate_version=VERSION,
        source_revision=REVISION,
        candidate_archive=f"rootlight-{VERSION}-{target}.zip",
        candidate_sha256=SHA256,
    )


class PackageLifecycleValidatorTests(unittest.TestCase):
    def test_accepts_windows_and_non_windows_release_evidence(self) -> None:
        for target in (
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
        ):
            with self.subTest(target=target):
                _validate(_document(target), target)

    def test_rejects_latency_summary_not_derived_from_samples(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        document = _document(target)
        document["installed_release"]["mcp_initialize"]["p99_micros"] = 1
        with self.assertRaises(VALIDATOR.LifecycleValidationError):
            _validate(document, target)

    def test_rejects_missing_measurements(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        document = _document(target)
        document["installed_release"]["mcp_initialize"][
            "measured_samples_micros"
        ].pop()
        with self.assertRaises(VALIDATOR.LifecycleValidationError):
            _validate(document, target)

    def test_rejects_windows_health_without_eof_proof(self) -> None:
        target = "x86_64-pc-windows-msvc"
        document = _document(target)
        document["installed_release"]["windows_first_health"]["stdout_eof_count"] = 9
        with self.assertRaises(VALIDATOR.LifecycleValidationError):
            _validate(document, target)

    def test_rejects_non_windows_job_accounting(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        document = _document(target)
        document["installed_release"]["mcp_initialize"][
            "post_cleanup_active_processes"
        ] = 0
        with self.assertRaises(VALIDATOR.LifecycleValidationError):
            _validate(document, target)

    def test_rejects_unknown_nested_fields(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        document = _document(target)
        document["installed_release"]["mcp_initialize"]["summary_only"] = True
        with self.assertRaises(VALIDATOR.LifecycleValidationError):
            _validate(document, target)

    def test_rejects_unobserved_installed_vertical_behavior(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        document = _document(target)
        document["installed_release"]["mcp_vertical"][
            "malformed_partial_result_observed"
        ] = False
        with self.assertRaises(VALIDATOR.LifecycleValidationError):
            _validate(document, target)

    def test_rejects_boolean_numeric_evidence(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        document = _document(target)
        document["installed_release"]["mcp_initialize"]["daemon_exit_count"] = True
        with self.assertRaises(VALIDATOR.LifecycleValidationError):
            _validate(document, target)

    def test_rejects_stale_schema(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        document = copy.deepcopy(_document(target))
        document["schema"] = "rootlight.package-lifecycle/3"
        with self.assertRaises(VALIDATOR.LifecycleValidationError):
            _validate(document, target)


if __name__ == "__main__":
    unittest.main()
