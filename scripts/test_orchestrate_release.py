#!/usr/bin/env python3
"""Tests for exact-revision GitHub release gate selection."""

from __future__ import annotations

import unittest
from unittest.mock import patch

import orchestrate_release

REVISION = "0123456789abcdef0123456789abcdef01234567"


class OrchestrationTests(unittest.TestCase):
    def test_orchestration_uses_one_exhaustive_ci_gate(self) -> None:
        ci_run = {"id": 11, "run_attempt": 2}
        candidate_run = {"id": 22, "run_attempt": 3}
        with (
            patch(
                "orchestrate_release.latest_successful_run", return_value=ci_run
            ) as latest,
            patch("orchestrate_release.require_run") as require,
            patch("orchestrate_release.dispatch_workflow") as dispatch,
            patch("orchestrate_release.discover_run", return_value=candidate_run),
            patch(
                "orchestrate_release.wait_for_completion",
                return_value=candidate_run,
            ),
        ):
            result = orchestrate_release.orchestrate(
                repository="tomasmarekk/rootlight",
                source_revision=REVISION,
                release_version="0.1.0-alpha.1",
                request_id="release-1-1",
            )

        self.assertEqual(
            result,
            {
                "candidate_run_attempt": 3,
                "candidate_run_id": 22,
                "ci_run_attempt": 2,
                "ci_run_id": 11,
            },
        )
        latest.assert_called_once_with(
            "tomasmarekk/rootlight",
            "ci.yml",
            REVISION,
            allowed_events={"push"},
        )
        self.assertEqual(require.call_count, 2)
        self.assertEqual(
            require.call_args_list[0].kwargs,
            {
                "source_revision": REVISION,
                "workflow_path": ".github/workflows/ci.yml",
                "aggregate_name": "ci-required",
                "allowed_events": {"push"},
            },
        )
        dispatch.assert_called_once_with(
            "release-candidate.yml",
            {
                "release_version": "0.1.0-alpha.1",
                "source_sha": REVISION,
                "ci_run_id": "11",
                "ci_run_attempt": "2",
                "request_id": "release-1-1",
            },
        )

    @patch("orchestrate_release.gh_json")
    def test_latest_successful_run_is_exact_and_newest(self, gh_json) -> None:
        gh_json.return_value = {
            "workflow_runs": [
                {
                    "id": 1,
                    "created_at": "2026-01-01T00:00:00Z",
                    "head_sha": REVISION,
                    "event": "push",
                    "conclusion": "success",
                },
                {
                    "id": 2,
                    "created_at": "2026-01-02T00:00:00Z",
                    "head_sha": REVISION,
                    "event": "push",
                    "conclusion": "success",
                },
                {
                    "id": 3,
                    "created_at": "2026-01-03T00:00:00Z",
                    "head_sha": "f" * 40,
                    "event": "push",
                    "conclusion": "success",
                },
            ]
        }
        selected = orchestrate_release.latest_successful_run(
            "tomasmarekk/rootlight", "ci.yml", REVISION, allowed_events={"push"}
        )
        self.assertIsNotNone(selected)
        self.assertEqual(selected["id"], 2)

    def test_inputs_reject_noncanonical_identity(self) -> None:
        orchestrate_release.validate_inputs(
            "tomasmarekk/rootlight", REVISION, "0.1.0-alpha.1", "release-1-1"
        )
        invalid = (
            ("bad repository", REVISION, "0.1.0", "release"),
            ("tomasmarekk/rootlight", "A" * 40, "0.1.0", "release"),
            ("tomasmarekk/rootlight", REVISION, "v0.1.0", "release"),
            ("tomasmarekk/rootlight", REVISION, "0.1.0", "release id"),
        )
        for arguments in invalid:
            with self.subTest(arguments=arguments):
                with self.assertRaises(orchestrate_release.ReleaseGateError):
                    orchestrate_release.validate_inputs(*arguments)

    def test_run_identifier_is_positive_decimal(self) -> None:
        self.assertEqual(orchestrate_release.validated_run_id("30427191953"), 30427191953)
        for value in ("", "0", "-1", "1.0", "１２"):
            with self.subTest(value=value):
                with self.assertRaises(orchestrate_release.ReleaseGateError):
                    orchestrate_release.validated_run_id(value)


if __name__ == "__main__":
    unittest.main()
