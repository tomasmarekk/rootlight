#!/usr/bin/env python3
"""Authorize and run exact-revision GitHub release gates."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

SOURCE_REVISION_PATTERN = re.compile(r"^[0-9a-f]{40}$")
VERSION_PATTERN = re.compile(
    r"^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-alpha\.(?:0|[1-9][0-9]*))?$"
)
REQUEST_PATTERN = re.compile(r"^[A-Za-z0-9._-]{1,64}$")
REPOSITORY_PATTERN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
DISCOVERY_TIMEOUT_SECONDS = 5 * 60
GATE_TIMEOUT_SECONDS = 5 * 60 * 60
POLL_SECONDS = 20


class ReleaseGateError(RuntimeError):
    """An exact-revision GitHub gate failed or became ambiguous."""


def parse_args(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--release-version", required=True)
    parser.add_argument("--request-id", required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args(arguments)


def main(arguments: list[str]) -> int:
    options = parse_args(arguments)
    try:
        result = orchestrate(
            repository=os.environ.get("GITHUB_REPOSITORY", ""),
            source_revision=options.source_revision,
            release_version=options.release_version,
            request_id=options.request_id,
        )
        write_json_new(options.output, result)
    except (OSError, ReleaseGateError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


def orchestrate(
    *,
    repository: str,
    source_revision: str,
    release_version: str,
    request_id: str,
) -> dict[str, int]:
    validate_inputs(repository, source_revision, release_version, request_id)

    ci_run = latest_successful_run(
        repository, "ci.yml", source_revision, allowed_events={"push"}
    )
    if ci_run is None:
        raise ReleaseGateError("no successful exact-revision CI run is available")
    require_run(
        repository,
        ci_run,
        source_revision=source_revision,
        workflow_path=".github/workflows/ci.yml",
        aggregate_name="ci-required",
        allowed_events={"push"},
    )

    candidate_title = f"release candidate / v{release_version} / {request_id}"
    dispatch_workflow(
        "release-candidate.yml",
        {
            "release_version": release_version,
            "source_sha": source_revision,
            "ci_run_id": str(ci_run["id"]),
            "ci_run_attempt": str(ci_run["run_attempt"]),
            "request_id": request_id,
        },
    )
    candidate_run = discover_run(
        repository, "release-candidate.yml", candidate_title, source_revision
    )
    candidate_run = wait_for_completion(repository, candidate_run)
    require_run(
        repository,
        candidate_run,
        source_revision=source_revision,
        workflow_path=".github/workflows/release-candidate.yml",
        aggregate_name="release-candidate-required",
        allowed_events={"workflow_dispatch"},
    )

    return {
        "ci_run_id": int(ci_run["id"]),
        "ci_run_attempt": int(ci_run["run_attempt"]),
        "candidate_run_id": int(candidate_run["id"]),
        "candidate_run_attempt": int(candidate_run["run_attempt"]),
    }


def validate_inputs(
    repository: str, source_revision: str, release_version: str, request_id: str
) -> None:
    if REPOSITORY_PATTERN.fullmatch(repository) is None:
        raise ReleaseGateError("GitHub repository identity is invalid")
    if SOURCE_REVISION_PATTERN.fullmatch(source_revision) is None:
        raise ReleaseGateError("source revision is invalid")
    if VERSION_PATTERN.fullmatch(release_version) is None:
        raise ReleaseGateError("release version is invalid")
    if REQUEST_PATTERN.fullmatch(request_id) is None:
        raise ReleaseGateError("release request identifier is invalid")


def latest_successful_run(
    repository: str,
    workflow: str,
    source_revision: str,
    *,
    allowed_events: set[str],
) -> dict[str, Any] | None:
    response = gh_json(
        f"/repos/{repository}/actions/workflows/{workflow}/runs"
        "?status=success&per_page=100"
    )
    matches = [
        run
        for run in response.get("workflow_runs", [])
        if run.get("head_sha") == source_revision
        and run.get("event") in allowed_events
        and run.get("conclusion") == "success"
    ]
    if not matches:
        return None
    return max(matches, key=lambda run: (run.get("created_at", ""), int(run["id"])))


def discover_run(
    repository: str, workflow: str, title: str, source_revision: str
) -> dict[str, Any]:
    deadline = time.monotonic() + DISCOVERY_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        response = gh_json(
            f"/repos/{repository}/actions/workflows/{workflow}/runs"
            "?event=workflow_dispatch&per_page=50"
        )
        matches = [
            run
            for run in response.get("workflow_runs", [])
            if run.get("display_title") == title
            and run.get("head_sha") == source_revision
        ]
        if len(matches) == 1:
            return matches[0]
        if len(matches) > 1:
            raise ReleaseGateError("workflow dispatch correlation is ambiguous")
        time.sleep(POLL_SECONDS)
    raise ReleaseGateError("workflow dispatch did not become visible before the deadline")


def wait_for_completion(repository: str, run: dict[str, Any]) -> dict[str, Any]:
    deadline = time.monotonic() + GATE_TIMEOUT_SECONDS
    run_id = validated_run_id(str(run.get("id", "")))
    while time.monotonic() < deadline:
        current = gh_json(f"/repos/{repository}/actions/runs/{run_id}")
        if current.get("status") == "completed":
            return current
        time.sleep(POLL_SECONDS)
    raise ReleaseGateError("workflow gate did not complete before the deadline")


def require_run(
    repository: str,
    run: dict[str, Any],
    *,
    source_revision: str,
    workflow_path: str,
    aggregate_name: str,
    allowed_events: set[str],
) -> None:
    expected = {
        "conclusion": "success",
        "head_sha": source_revision,
        "path": workflow_path,
        "status": "completed",
    }
    observed = {key: run.get(key) for key in expected}
    if observed != expected or run.get("event") not in allowed_events:
        raise ReleaseGateError(f"{aggregate_name} workflow run does not authorize release")

    attempt = run.get("run_attempt")
    if not isinstance(attempt, int) or isinstance(attempt, bool) or attempt < 1:
        raise ReleaseGateError(f"{aggregate_name} run attempt is invalid")
    jobs = gh_json(
        f"/repos/{repository}/actions/runs/{validated_run_id(str(run.get('id', '')))}"
        "/jobs?filter=latest&per_page=100"
    )
    if jobs.get("total_count", 0) > 100:
        raise ReleaseGateError(f"{aggregate_name} job set exceeds the audit bound")
    aggregates = [
        job
        for job in jobs.get("jobs", [])
        if job.get("name") == aggregate_name and job.get("run_attempt") == attempt
    ]
    if len(aggregates) != 1 or aggregates[0].get("conclusion") != "success":
        raise ReleaseGateError(f"{aggregate_name} did not succeed")


def dispatch_workflow(workflow: str, inputs: dict[str, str]) -> None:
    command = ["gh", "workflow", "run", workflow, "--ref", "main"]
    for name, value in inputs.items():
        command.extend(("--field", f"{name}={value}"))
    completed = subprocess.run(
        command,
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if completed.returncode != 0:
        raise ReleaseGateError("failed to dispatch a required GitHub workflow")


def gh_json(endpoint: str) -> dict[str, Any]:
    completed = subprocess.run(
        ["gh", "api", endpoint],
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
    )
    if completed.returncode != 0 or len(completed.stdout) > 16 * 1024 * 1024:
        raise ReleaseGateError("GitHub API request failed")
    try:
        value = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise ReleaseGateError("GitHub API returned invalid JSON") from error
    if not isinstance(value, dict):
        raise ReleaseGateError("GitHub API returned an unexpected document")
    return value


def validated_run_id(value: str) -> int:
    if not value.isascii() or not value.isdigit():
        raise ReleaseGateError("GitHub run identifier is invalid")
    run_id = int(value)
    if run_id < 1:
        raise ReleaseGateError("GitHub run identifier is invalid")
    return run_id


def write_json_new(path: Path, value: object) -> None:
    encoded = json.dumps(value, indent=2, sort_keys=True) + "\n"
    with path.open("x", encoding="utf-8", newline="\n") as output:
        output.write(encoded)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
