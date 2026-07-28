#!/usr/bin/env python3
"""Validate installed-package lifecycle evidence without trusting its summaries."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any

SCHEMA = "rootlight.package-lifecycle/2"
TARGETS = {
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
}
TOP_LEVEL_FIELDS = {
    "schema",
    "source_revision",
    "candidate_target",
    "candidate_version",
    "baseline_version",
    "candidate_archive",
    "candidate_sha256",
    "baseline_archive",
    "baseline_sha256",
    "bootstrap_owned_files",
    "committed_active_version",
    "committed_last_good_version",
    "rollback_active_version",
    "uninstall_removed_versions",
    "installed_release",
    "installed_command_uninstall_observed",
    "launcher_probe_observed",
    "candidate_health_observed",
    "failed_health_rollback_observed",
    "clean_recovery_observed",
    "user_data_preserved",
    "unowned_data_preserved",
}
OBSERVATIONS = {
    "installed_command_uninstall_observed",
    "launcher_probe_observed",
    "candidate_health_observed",
    "failed_health_rollback_observed",
    "clean_recovery_observed",
    "user_data_preserved",
    "unowned_data_preserved",
}
HEALTH_FIELDS = {
    "limit_micros",
    "samples_micros",
    "successful_attempts",
    "launcher_exit_count",
    "stdout_eof_count",
    "stderr_eof_count",
    "pre_cleanup_active_processes",
    "post_cleanup_active_processes",
}
MCP_FIELDS = {
    "warmup_samples_micros",
    "measured_samples_micros",
    "p50_micros",
    "p95_micros",
    "p99_micros",
    "p50_limit_micros",
    "p95_limit_micros",
    "p99_limit_micros",
    "successful_warmups",
    "successful_measurements",
    "launcher_exit_count",
    "daemon_exit_count",
    "steady_state_active_processes",
    "post_cleanup_active_processes",
}
HEX_256 = re.compile(r"[0-9a-f]{64}")
MAX_EVIDENCE_BYTES = 64 * 1024


class LifecycleValidationError(ValueError):
    """A lifecycle document failed a release-authority invariant."""


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    document: dict[str, Any] = {}
    for key, value in pairs:
        if key in document:
            raise LifecycleValidationError(f"duplicate JSON field: {key}")
        document[key] = value
    return document


def _integer(value: Any, field: str, *, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise LifecycleValidationError(
            f"{field} must be an integer greater than or equal to {minimum}"
        )
    return value


def _samples(value: Any, field: str, expected: int) -> list[int]:
    if not isinstance(value, list) or len(value) != expected:
        raise LifecycleValidationError(f"{field} must contain exactly {expected} samples")
    return [
        _integer(sample, f"{field}[{index}]", minimum=1)
        for index, sample in enumerate(value)
    ]


def _nearest_rank(samples: list[int], percentile: int) -> int:
    ordered = sorted(samples)
    rank = (len(ordered) * percentile + 99) // 100
    return ordered[rank - 1]


def _exact_fields(document: Any, fields: set[str], name: str) -> dict[str, Any]:
    if not isinstance(document, dict):
        raise LifecycleValidationError(f"{name} must be a JSON object")
    observed = set(document)
    if observed != fields:
        raise LifecycleValidationError(
            f"{name} fields differ: "
            f"missing={sorted(fields - observed)!r}, "
            f"extra={sorted(observed - fields)!r}"
        )
    return document


def _validate_health(document: Any, target: str) -> None:
    if target != "x86_64-pc-windows-msvc":
        if document is not None:
            raise LifecycleValidationError(
                "Windows cold-health evidence must be null on non-Windows targets"
            )
        return
    health = _exact_fields(document, HEALTH_FIELDS, "Windows cold-health evidence")
    samples = _samples(health["samples_micros"], "health samples", 10)
    limit = _integer(health["limit_micros"], "health limit", minimum=1)
    if limit != 3_000_000 or any(sample > limit for sample in samples):
        raise LifecycleValidationError("Windows cold-health samples exceed the 3 second limit")
    for field in (
        "successful_attempts",
        "launcher_exit_count",
        "stdout_eof_count",
        "stderr_eof_count",
    ):
        if _integer(health[field], field) != 10:
            raise LifecycleValidationError(f"{field} must prove all 10 attempts")
    active = _samples(
        health["pre_cleanup_active_processes"],
        "health pre-cleanup process counts",
        10,
    )
    if any(count > 2 for count in active):
        raise LifecycleValidationError("Windows cold-health process tree is unbounded")
    if _integer(
        health["post_cleanup_active_processes"],
        "health post-cleanup process count",
    ) != 0:
        raise LifecycleValidationError("Windows cold-health cleanup retained a process")


def _validate_mcp(document: Any, target: str) -> None:
    mcp = _exact_fields(document, MCP_FIELDS, "installed MCP evidence")
    warmups = _samples(mcp["warmup_samples_micros"], "MCP warmups", 5)
    measured = _samples(mcp["measured_samples_micros"], "MCP measurements", 100)
    expected_percentiles = {
        "p50_micros": _nearest_rank(measured, 50),
        "p95_micros": _nearest_rank(measured, 95),
        "p99_micros": _nearest_rank(measured, 99),
    }
    for field, expected in expected_percentiles.items():
        if _integer(mcp[field], field, minimum=1) != expected:
            raise LifecycleValidationError(f"{field} was not recomputed from raw samples")
    limits = {
        "p50_limit_micros": 80_000,
        "p95_limit_micros": 150_000,
        "p99_limit_micros": 300_000,
    }
    for field, expected in limits.items():
        if _integer(mcp[field], field, minimum=1) != expected:
            raise LifecycleValidationError(f"{field} differs from the release limit")
    if (
        expected_percentiles["p50_micros"] > limits["p50_limit_micros"]
        or expected_percentiles["p95_micros"] > limits["p95_limit_micros"]
        or expected_percentiles["p99_micros"] > limits["p99_limit_micros"]
    ):
        raise LifecycleValidationError("installed MCP latency exceeds a release percentile")
    expected_counts = {
        "successful_warmups": len(warmups),
        "successful_measurements": len(measured),
        "launcher_exit_count": len(warmups) + len(measured),
        "daemon_exit_count": 1,
    }
    for field, expected in expected_counts.items():
        if _integer(mcp[field], field) != expected:
            raise LifecycleValidationError(f"{field} differs from raw execution evidence")
    cleanup = mcp["post_cleanup_active_processes"]
    if target == "x86_64-pc-windows-msvc":
        steady = _integer(
            mcp["steady_state_active_processes"],
            "MCP steady-state process count",
            minimum=1,
        )
        if steady > 2:
            raise LifecycleValidationError("installed MCP process tree is unbounded")
        if _integer(cleanup, "MCP post-cleanup process count") != 0:
            raise LifecycleValidationError("installed MCP cleanup retained a Windows process")
    elif cleanup is not None or mcp["steady_state_active_processes"] is not None:
        raise LifecycleValidationError(
            "MCP process accounting must be null where Job Objects are unavailable"
        )


def validate_document(
    document: Any,
    *,
    target: str,
    candidate_version: str,
    source_revision: str,
    candidate_archive: str,
    candidate_sha256: str,
) -> None:
    """Validate one parsed lifecycle document against independently supplied identity."""

    if target not in TARGETS:
        raise LifecycleValidationError(f"unsupported release target: {target}")
    lifecycle = _exact_fields(document, TOP_LEVEL_FIELDS, "package lifecycle")
    expected = {
        "schema": SCHEMA,
        "source_revision": source_revision,
        "candidate_target": target,
        "candidate_version": candidate_version,
        "baseline_version": "0.0.0",
        "candidate_archive": candidate_archive,
        "candidate_sha256": candidate_sha256,
        "baseline_archive": f"rootlight-0.0.0-{target}.zip",
        "committed_active_version": candidate_version,
        "committed_last_good_version": "0.0.0",
        "rollback_active_version": "0.0.0",
    }
    for field, value in expected.items():
        if lifecycle[field] != value:
            raise LifecycleValidationError(f"{field} differs from release identity")
    if not HEX_256.fullmatch(str(lifecycle["baseline_sha256"])):
        raise LifecycleValidationError("baseline_sha256 is not canonical SHA-256")
    if not HEX_256.fullmatch(candidate_sha256):
        raise LifecycleValidationError("candidate SHA-256 input is not canonical")
    if any(lifecycle[field] is not True for field in OBSERVATIONS):
        raise LifecycleValidationError("package lifecycle observations are incomplete")
    if _integer(lifecycle["bootstrap_owned_files"], "bootstrap_owned_files", minimum=1) < 1:
        raise LifecycleValidationError("package bootstrap owned no files")
    if _integer(lifecycle["uninstall_removed_versions"], "uninstall_removed_versions") != 2:
        raise LifecycleValidationError("package uninstall did not remove both versions")
    installed = _exact_fields(
        lifecycle["installed_release"],
        {"windows_first_health", "mcp_initialize"},
        "installed release evidence",
    )
    _validate_health(installed["windows_first_health"], target)
    _validate_mcp(installed["mcp_initialize"], target)


def validate_path(
    path: Path,
    *,
    target: str,
    candidate_version: str,
    source_revision: str,
    candidate_archive: str,
    candidate_sha256: str,
) -> None:
    """Read and validate one bounded, duplicate-free lifecycle artifact."""

    if not path.is_file():
        raise LifecycleValidationError(f"lifecycle evidence is not a regular file: {path}")
    if path.stat().st_size > MAX_EVIDENCE_BYTES:
        raise LifecycleValidationError("lifecycle evidence exceeds its byte ceiling")
    try:
        document = json.loads(
            path.read_bytes(),
            object_pairs_hook=_reject_duplicate_keys,
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise LifecycleValidationError("lifecycle evidence is not valid bounded JSON") from error
    validate_document(
        document,
        target=target,
        candidate_version=candidate_version,
        source_revision=source_revision,
        candidate_archive=candidate_archive,
        candidate_sha256=candidate_sha256,
    )


def _arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--path", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--candidate-version", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--candidate-archive", required=True)
    parser.add_argument("--candidate-sha256", required=True)
    return parser.parse_args()


def main() -> int:
    arguments = _arguments()
    try:
        validate_path(
            arguments.path,
            target=arguments.target,
            candidate_version=arguments.candidate_version,
            source_revision=arguments.source_revision,
            candidate_archive=arguments.candidate_archive,
            candidate_sha256=arguments.candidate_sha256,
        )
    except LifecycleValidationError as error:
        raise SystemExit(f"package lifecycle validation failed: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
