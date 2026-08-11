#!/usr/bin/env python3
"""Static security contract tests for protected update signing.

Normal releases must use the external OIDC signer, while private-seed signing
remains available only through the explicit offline xtask interface.
"""

from __future__ import annotations

from pathlib import Path
import re
import tomllib
import unittest


REPOSITORY = Path(__file__).parent.parent
RELEASE_WORKFLOW = REPOSITORY / ".github/workflows/release.yml"
ACTION_POLICY = REPOSITORY / "policy/github-actions.toml"
OFFLINE_SIGNER = REPOSITORY / "xtask/src/update_release.rs"


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

    def test_normal_release_has_no_private_key_or_local_signing_path(self) -> None:
        forbidden = (
            "ROOTLIGHT_UPDATE_PRIVATE_SEED_HEX",
            "private-key.der",
            "private-key.pem",
            "--private-seed",
        )
        for marker in forbidden:
            with self.subTest(marker=marker):
                self.assertNotIn(marker, self.workflow)
        self.assertNotIn("${{ secrets.", self.signing_job)
        self.assertNotRegex(self.signing_job, r"(?<![A-Za-z0-9_-])-sign(?![A-Za-z0-9_-])")
        self.assertNotIn("actions/checkout", self.signing_job)
        for repository_command in ("cargo ", "npm ", "target/", "xtask"):
            with self.subTest(repository_command=repository_command):
                self.assertNotIn(repository_command, self.signing_job)
        self.assertIn(
            '"--private-seed"',
            OFFLINE_SIGNER.read_text(encoding="utf-8"),
        )

    def test_signing_job_has_exact_oidc_permission_and_configuration(self) -> None:
        self.assertIn(
            "    permissions:\n"
            "      actions: read\n"
            "      contents: none\n"
            "      id-token: write\n",
            self.signing_job,
        )
        required = (
            "ROOTLIGHT_UPDATE_SIGNER_AUDIENCE: ${{ vars.ROOTLIGHT_UPDATE_SIGNER_AUDIENCE }}",
            "ROOTLIGHT_UPDATE_SIGNER_URL: ${{ vars.ROOTLIGHT_UPDATE_SIGNER_URL }}",
            "SOURCE_REVISION: ${{ github.sha }}",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "unset ACTIONS_ID_TOKEN_REQUEST_TOKEN ACTIONS_ID_TOKEN_REQUEST_URL",
            'parsed.scheme != "https"',
            'parsed.username is not None',
            'parsed.password is not None',
            "--data-urlencode \"audience=$ROOTLIGHT_UPDATE_SIGNER_AUDIENCE\"",
        )
        for marker in required:
            with self.subTest(marker=marker):
                self.assertIn(marker, self.signing_job)

    def test_request_response_and_signature_bind_every_identity(self) -> None:
        request_headers = (
            "X-Rootlight-Signature-Schema",
            "X-Rootlight-Source-Revision",
            "X-Rootlight-Key-Id",
            "X-Rootlight-Public-Key-Hex",
            "X-Rootlight-Payload-Sha256",
        )
        for header in request_headers:
            with self.subTest(header=header):
                self.assertEqual(self.signing_job.count(header), 1)
        response_fields = (
            '"schema": os.environ["SIGNATURE_SCHEMA"]',
            '"source_revision": os.environ["SOURCE_REVISION"]',
            '"key_id": os.environ["ROOTLIGHT_UPDATE_KEY_ID"]',
            '"public_key_hex": os.environ["ROOTLIGHT_UPDATE_PUBLIC_KEY_HEX"]',
            '"payload_sha256": os.environ["PAYLOAD_SHA256"]',
            '"signature_hex"',
        )
        for field in response_fields:
            with self.subTest(field=field):
                self.assertIn(field, self.signing_job)
        self.assertIn("1 <= len(response_bytes) <= 4096", self.signing_job)
        self.assertIn("len(signature) != 128", self.signing_job)
        self.assertIn('"0123456789abcdef"', self.signing_job)
        self.assertIn("openssl pkeyutl \\\n              -verify", self.signing_job)
        self.assertIn('-inkey "$temporary/public-key.pem"', self.signing_job)
        self.assertIn('-sigfile "$signature_binary"', self.signing_job)

    def test_policy_allows_only_the_exact_signing_job_permissions(self) -> None:
        policy = tomllib.loads(ACTION_POLICY.read_text(encoding="utf-8"))
        matching = [
            entry
            for entry in policy["write_permission_jobs"]
            if entry["workflow"] == ".github/workflows/release.yml"
            and entry["job"] == "sign-update-metadata"
        ]
        self.assertEqual(
            matching,
            [
                {
                    "workflow": ".github/workflows/release.yml",
                    "job": "sign-update-metadata",
                    "condition": (
                        "github.event_name == 'workflow_dispatch' "
                        "&& github.ref == 'refs/heads/main'"
                    ),
                    "permissions": [
                        "actions=read",
                        "contents=none",
                        "id-token=write",
                    ],
                }
            ],
        )


if __name__ == "__main__":
    unittest.main()
