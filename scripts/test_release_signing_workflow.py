#!/usr/bin/env python3
"""Static security contract tests for protected update signing."""

from __future__ import annotations

from pathlib import Path
import re
import tomllib
import unittest


REPOSITORY = Path(__file__).parent.parent
RELEASE_WORKFLOW = REPOSITORY / ".github/workflows/release.yml"
ACTION_POLICY = REPOSITORY / "policy/github-actions.toml"


def workflow_job(workflow: str, name: str) -> str:
    start_marker = f"  {name}:\n"
    start = workflow.find(start_marker)
    if start < 0:
        raise AssertionError(f"workflow job is missing: {name}")
    remainder = workflow[start + len(start_marker) :]
    next_job = re.search(r"(?m)^  [a-z0-9][a-z0-9-]*:\n", remainder)
    end = len(workflow) if next_job is None else start + len(start_marker) + next_job.start()
    return workflow[start:end]


class ReleaseSigningWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = RELEASE_WORKFLOW.read_text(encoding="utf-8")
        self.signing_job = workflow_job(self.workflow, "sign-update-metadata")

    def test_signing_job_uses_only_the_protected_environment_secret(self) -> None:
        self.assertIn("    environment: release-signing\n", self.signing_job)
        self.assertEqual(
            self.signing_job.count("${{ secrets.ROOTLIGHT_UPDATE_PRIVATE_SEED_HEX }}"),
            1,
        )
        self.assertNotIn("ROOTLIGHT_UPDATE_SIGNER_", self.signing_job)
        self.assertNotIn("ACTIONS_ID_TOKEN_", self.signing_job)
        self.assertNotIn("actions/checkout", self.signing_job)
        for repository_command in ("cargo ", "npm ", "target/", "xtask"):
            with self.subTest(repository_command=repository_command):
                self.assertNotIn(repository_command, self.signing_job)

    def test_signing_job_has_exact_minimal_permissions(self) -> None:
        self.assertIn(
            "    permissions:\n"
            "      actions: read\n"
            "      contents: none\n",
            self.signing_job,
        )
        self.assertNotIn("id-token:", self.signing_job)

    def test_private_seed_is_validated_restricted_and_removed_from_environment(
        self,
    ) -> None:
        required = (
            '[[ "$ROOTLIGHT_UPDATE_PRIVATE_SEED_HEX" =~ ^[0-9a-f]{64}$ ]]',
            "umask 077",
            '"$ROOTLIGHT_UPDATE_PRIVATE_SEED_HEX"',
            "unset ROOTLIGHT_UPDATE_PRIVATE_SEED_HEX",
            '"$temporary/private-key.der"',
            '"$temporary/private-key.pem"',
            "trap 'rm -rf \"$temporary\"' EXIT",
        )
        for marker in required:
            with self.subTest(marker=marker):
                self.assertIn(marker, self.signing_job)
        self.assertLess(
            self.signing_job.index("unset ROOTLIGHT_UPDATE_PRIVATE_SEED_HEX"),
            self.signing_job.index("sign_payload()"),
        )

    def test_every_signature_is_canonical_and_verified_locally(self) -> None:
        required = (
            "openssl pkeyutl \\\n              -sign",
            '-inkey "$temporary/private-key.pem"',
            'test "$(wc -c < "$signature_binary")" -eq 64',
            'xxd -p -c 256 "$signature_binary" > "$signature"',
            '[[ "$(tr -d \'\\n\' < "$signature")" =~ ^[0-9a-f]{128}$ ]]',
            "openssl pkeyutl \\\n              -verify",
            '-inkey "$temporary/public-key.pem"',
            '-sigfile "$signature_binary"',
        )
        for field in required:
            with self.subTest(field=field):
                self.assertIn(field, self.signing_job)

    def test_policy_has_no_write_permission_exception_for_signing_job(self) -> None:
        policy = tomllib.loads(ACTION_POLICY.read_text(encoding="utf-8"))
        matching = [
            entry
            for entry in policy["write_permission_jobs"]
            if entry["workflow"] == ".github/workflows/release.yml"
            and entry["job"] == "sign-update-metadata"
        ]
        self.assertEqual(matching, [])


if __name__ == "__main__":
    unittest.main()
