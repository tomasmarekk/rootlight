#!/usr/bin/env python3
"""Tests for exact-revision GitHub release gate selection."""

from __future__ import annotations

import unittest
from unittest.mock import patch

import orchestrate_release

REVISION = "0123456789abcdef0123456789abcdef01234567"


class OrchestrationTests(unittest.TestCase):
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
