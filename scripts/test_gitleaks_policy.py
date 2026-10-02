#!/usr/bin/env python3
"""Exercise secret policy against authenticated upstream data and new credentials.

These tests run the installed policy tool, not a mock. Exact upstream exceptions
must leave unrelated values and paths detectable, including within vendored files.
"""

from __future__ import annotations

import base64
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / ".gitleaks.toml"
CHECKSUM = "source-inputs/cargo/aho-corasick-1.1.4/.cargo-checksum.json"
PEM = "source-inputs/cargo/ed25519-compact-2.3.1/src/pem.rs"
WEBSOCKET = "source-inputs/cargo/axum-0.8.9/src/extract/ws.rs"
SQLITE = "source-inputs/cargo/libsqlite3-sys-0.37.0/sqlite3/sqlite3ext.h"
LINUX = "source-inputs/cargo/linux-raw-sys-0.12.1/src/x86_64/netlink.rs"
WINDOWS = "source-inputs/cargo/windows-0.61.3/src/Windows/Win32"


class GitleaksPolicyTests(unittest.TestCase):
    """Require actual scanner success and fail-closed exception boundaries."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.scanner = shutil.which("gitleaks")
        if cls.scanner is None:
            raise RuntimeError("the installed policy tool is required; tests cannot skip")

    def scan(self, files: dict[str, bytes], mode: str = "dir") -> list[dict]:
        """Scan isolated files or their Git additions and return a redacted report."""
        if mode not in {"dir", "git"}:
            raise ValueError("unsupported scanner mode")
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            source = base / "source"
            source.mkdir()
            for name, content in files.items():
                destination = source / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(content)
            if mode == "git":
                # Diff scanning has separate chunk boundaries from directory scanning.
                # Both must exercise exact exceptions with no inherited hook execution.
                def run_git(*arguments: str) -> None:
                    result = subprocess.run(
                        ["git", "-c", "core.hooksPath=" + str(base / "no-hooks"),
                         "-c", "core.autocrlf=false", "-c", "commit.gpgSign=false",
                         "-c", "user.name=rootlight-policy-fixture",
                         "-c", "user.email=rootlight-policy-fixture@example.invalid",
                         *arguments],
                        cwd=source, stdout=subprocess.DEVNULL,
                        stderr=subprocess.PIPE, check=False,
                    )
                    self.assertEqual(result.returncode, 0, "isolated Git fixture failed")

                run_git("init", "--quiet")
                run_git("add", "--", *files)
                run_git("commit", "--quiet", "-m", "fixture")
            report = base / "redacted.json"
            result = subprocess.run(
                [self.scanner, mode, "--config", str(CONFIG),
                 "--redact=100", "--no-banner", "--no-color", "--log-level", "error",
                 "--report-format", "json", "--report-path", str(report), "."],
                cwd=source,
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=False,
            )
            self.assertIn(result.returncode, (0, 1), "scanner execution failed")
            self.assertTrue(report.is_file(), "scanner did not write a report")
            findings = json.loads(report.read_bytes())
            self.assertIsInstance(findings, list)
            for finding in findings:
                self.assertEqual(finding.get("Secret"), "REDACTED")
            self.assertEqual(result.returncode, int(bool(findings)))
            return findings

    def assert_rule_detected(self, name: str, content: bytes, rule: str) -> None:
        """Require detection without exposing synthetic scanner payloads."""
        findings = self.scan({name: content})
        self.assertIn(rule, {finding["RuleID"] for finding in findings})

    def test_authenticated_upstream_samples_remain_unchanged_and_scan_clean(self) -> None:
        paths = [CHECKSUM, PEM, WEBSOCKET, SQLITE, LINUX,
                 f"{WINDOWS}/Graphics/Direct3D12/mod.rs",
                 f"{WINDOWS}/NetworkManagement/WiFi/mod.rs",
                 f"{WINDOWS}/Security/Cryptography/Certificates/mod.rs"]
        self.assertFalse(self.scan({name: (ROOT / name).read_bytes() for name in paths}),
                         "authenticated upstream samples require exact exceptions")

    def test_unknown_digest_value_in_known_checksum_path_is_detected(self) -> None:
        synthetic = hashlib.sha256(b"rootlight synthetic checksum credential").hexdigest()
        self.assert_rule_detected(CHECKSUM, f'api_key = "{synthetic}"\n'.encode(),
                                  "generic-api-key")

    def test_known_digest_outside_checksum_path_is_detected(self) -> None:
        values = json.loads((ROOT / CHECKSUM).read_bytes())["files"]
        digest = next(iter(values.values()))
        self.assert_rule_detected("src/credentials.rs", f'api_key = "{digest}"\n'.encode(),
                                  "generic-api-key")

    def test_new_generic_value_in_identifier_exception_path_is_detected(self) -> None:
        synthetic = hashlib.sha256(b"rootlight synthetic unrelated api credential").hexdigest()
        self.assert_rule_detected(SQLITE, f'api_key = "{synthetic}"\n'.encode(),
                                  "generic-api-key")

    def test_known_identifier_outside_its_upstream_path_is_detected(self) -> None:
        identifier = b"soft_heap_" + b"limit64"
        content = b'api_' + b'key = "' + identifier + b'"\n'
        self.assert_rule_detected("src/credentials.rs", content, "generic-api-key")

    def test_fixed_websocket_nonce_outside_upstream_path_is_detected(self) -> None:
        self.assert_rule_detected("src/websocket.rs", (ROOT / WEBSOCKET).read_bytes(),
                                  "generic-api-key")

    def test_unrelated_private_key_in_upstream_pem_path_is_detected(self) -> None:
        seed = hashlib.sha256(b"rootlight synthetic nonproduction private key").digest()
        encoded = base64.b64encode(bytes.fromhex("302e020100300506032b657004220420") + seed)
        content = b"-----BEGIN " + b"PRIVATE KEY-----\n" + encoded
        content += b"\n-----END " + b"PRIVATE KEY-----\n"
        self.assert_rule_detected(PEM, content, "private-key")

    def test_unrelated_typed_credentials_in_every_exception_category_are_detected(self) -> None:
        credential = b"token = 'glpat-" + b"0123456789" + b"abcdefghijkl'\n"
        for name in (CHECKSUM, SQLITE, LINUX, PEM, WEBSOCKET):
            with self.subTest(path=name):
                self.assert_rule_detected(name, credential, "gitlab-pat")

    def test_authenticated_upstream_git_additions_scan_clean(self) -> None:
        paths = [CHECKSUM, PEM, WEBSOCKET, SQLITE, LINUX,
                 "source-inputs/cargo/winapi-x86_64-pc-windows-gnu-0.4.0/.cargo-checksum.json",
                 "source-inputs/cargo/winapi-i686-pc-windows-gnu-0.4.0/.cargo-checksum.json"]
        self.assertFalse(self.scan({name: (ROOT / name).read_bytes() for name in paths}, "git"),
                         "authenticated Git additions require exact exceptions")

    def test_original_negative_control_match_is_the_only_policy_script_exception(self) -> None:
        identifier = b"soft_heap_" + b"limit64"
        known = b'api_' + b'key = "' + identifier + b'"\n'
        name = "scripts/test_gitleaks_policy.py"
        self.assertFalse(self.scan({name: known}, "git"), "known inert history must scan clean")
        synthetic = hashlib.sha256(b"rootlight synthetic policy-script credential").hexdigest()
        self.assert_rule_detected(name, f'api_key = "{synthetic}"\n'.encode(),
                                  "generic-api-key")


if __name__ == "__main__":
    unittest.main()
