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
import re
import shutil
import subprocess
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / ".gitleaks.toml"
CHECKSUM = "source-inputs/cargo/aho-corasick-1.1.4/.cargo-checksum.json"
PEM = "source-inputs/cargo/ed25519-compact-2.3.1/src/pem.rs"
WEBSOCKET = "source-inputs/cargo/axum-0.8.9/src/extract/ws.rs"
SQLITE = "source-inputs/cargo/libsqlite3-sys-0.37.0/sqlite3/sqlite3ext.h"
LINUX = "source-inputs/cargo/linux-raw-sys-0.12.1/src/x86_64/netlink.rs"
WINDOWS = "source-inputs/cargo/windows-0.61.3/src/Windows/Win32"
HISTORICAL = "source-inputs/historical-cargo-checksums.json"
NATIVE_CLOSURE = (
    "source-inputs/producer-security/"
    "lightningcss-1d680fa14e9a089c92dc0929f869d6757ae91c30/native-source-closure.json"
)
GRAMMAR_LOCK = "adapters/grammars.lock"
YARN_NONASCII_DOC = (
    "source-inputs/producer-dependencies/"
    "lightningcss-1d680fa14e9a089c92dc0929f869d6757ae91c30/yarn-originals/packages/"
    "b1ff0c2a3ae1c98752b511a82deb3a7a0b0e98f42f12dc2d147405dffa124ffc/package/README-es.md"
)


def _historical_cargo_checksum(root: Path, name: str) -> dict:
    evidence = json.loads((root / HISTORICAL).read_bytes())
    if (evidence.get("schema") != "rootlight.historical-cargo-checksums/1"
            or not re.fullmatch(r"[0-9a-f]{40}", evidence.get("retired_from_commit", ""))):
        raise ValueError("invalid historical Cargo checksum provenance")
    record = evidence.get("manifests", {}).get(name)
    if not isinstance(record, dict):
        raise ValueError("historical Cargo checksum evidence is missing")
    expected = record.get("original_manifest_sha256", "")
    if (not re.fullmatch(r"[0-9a-f]{64}", expected)
            or not re.fullmatch(r"[0-9a-f]{64}", record.get("archive_sha256", ""))):
        raise ValueError("historical Cargo checksum hash is missing or malformed")
    files = {row["path"]: row["digest"] for row in record["files"]}
    if len(files) != len(record["files"]):
        raise ValueError("duplicate historical Cargo checksum member")
    manifest = {"files": files, "package": record["archive_sha256"]}
    encoded = (json.dumps(manifest, sort_keys=True) + "\n").encode()
    if hashlib.sha256(encoded).hexdigest() != expected:
        raise ValueError("historical Cargo checksum evidence hash differs")
    return manifest


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

    def cargo_digest_exceptions(self) -> dict[str, list[str]]:
        """Require exact metadata paths and literal SHA-256 values for every rule."""
        policy = tomllib.loads(CONFIG.read_text(encoding="utf-8"))
        result = {}
        prefix = "Published Cargo digests: "
        for rule in policy["allowlists"]:
            description = rule.get("description", "")
            if not description.startswith(prefix):
                continue
            name = "source-inputs/cargo/" + description[len(prefix):] + "/.cargo-checksum.json"
            self.assertNotIn(name, result, "duplicate Cargo checksum exception")
            literal_path = re.escape(name).replace(r"\-", "-")
            self.assertEqual(rule["paths"], ["^" + literal_path + "$"])
            self.assertEqual(rule["targetRules"], ["generic-api-key"])
            self.assertEqual(rule["condition"], "AND")
            self.assertEqual(rule["regexTarget"], "secret")
            digests = rule["regexes"]
            self.assertTrue(digests, "Cargo checksum exception must name exact values")
            for digest in digests:
                self.assertRegex(digest, r"\A\^[a-f0-9]{64}\$\Z")
            result[name] = [digest[1:-1] for digest in digests]
        self.assertTrue(result, "Cargo checksum policy is required")
        return result

    def test_all_exact_cargo_digest_exceptions_scan_clean_in_both_modes(self) -> None:
        files = {
            name: "".join(f'api_key = "{digest}"\n' for digest in digests).encode()
            for name, digests in self.cargo_digest_exceptions().items()
        }
        for mode in ("dir", "git"):
            with self.subTest(mode=mode):
                self.assertFalse(self.scan(files, mode), "exact published metadata must scan clean")

    def test_every_cargo_digest_exception_belongs_to_authenticated_checksum_metadata(self) -> None:
        for name, digests in self.cargo_digest_exceptions().items():
            with self.subTest(path=name):
                path = ROOT / name
                manifest = (json.loads(path.read_bytes()) if path.is_file()
                            else _historical_cargo_checksum(ROOT, name))
                authenticated_values = {manifest["package"], *manifest["files"].values()}
                self.assertTrue(set(digests).issubset(authenticated_values),
                                "checksum exception must name an authenticated published digest")

    def test_historical_checksums_are_hash_bound_and_required(self) -> None:
        original = json.loads((ROOT / HISTORICAL).read_bytes())
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            proof = base / HISTORICAL
            proof.parent.mkdir(parents=True)
            for name in original["manifests"]:
                self.assertFalse((ROOT / name).exists(), "historical evidence must describe retired inputs")
                for change in ("missing_record", "missing_hash", "bad_hash", "changed_member"):
                    with self.subTest(path=name, change=change):
                        mutated = json.loads(json.dumps(original))
                        if change == "missing_record":
                            del mutated["manifests"][name]
                        elif change == "missing_hash":
                            del mutated["manifests"][name]["original_manifest_sha256"]
                        elif change == "bad_hash":
                            mutated["manifests"][name]["original_manifest_sha256"] = "0" * 64
                        else:
                            mutated["manifests"][name]["files"][0]["digest"] = "0" * 64
                        proof.write_text(json.dumps(mutated), encoding="utf-8")
                        with self.assertRaises(ValueError):
                            _historical_cargo_checksum(base, name)
            proof.unlink()
            with self.assertRaises(FileNotFoundError):
                _historical_cargo_checksum(base, next(iter(original["manifests"])))

    def test_historical_checksum_evidence_scans_clean(self) -> None:
        self.assertFalse(self.scan({HISTORICAL: (ROOT / HISTORICAL).read_bytes()}, "git"))

    def test_every_cargo_digest_path_detects_new_generic_and_typed_credentials(self) -> None:
        synthetic = hashlib.sha256(b"rootlight synthetic checksum negative control").hexdigest()
        content = f'api_key = "{synthetic}"\n'.encode()
        content += b"token = 'glpat-" + b"0123456789" + b"abcdefghijkl'\n"
        files = {name: content for name in self.cargo_digest_exceptions()}
        for mode in ("dir", "git"):
            with self.subTest(mode=mode):
                findings = self.scan(files, mode)
                detected = {(finding["File"], finding["RuleID"]) for finding in findings}
                for name in files:
                    self.assertIn((name, "generic-api-key"), detected)
                    self.assertIn((name, "gitlab-pat"), detected)

    def test_every_cargo_digest_exception_remains_path_bound(self) -> None:
        files = {
            "relocated/" + name: f'api_key = "{digests[0]}"\n'.encode()
            for name, digests in self.cargo_digest_exceptions().items()
        }
        findings = self.scan(files)
        detected = {finding["File"] for finding in findings
                    if finding["RuleID"] == "generic-api-key"}
        self.assertEqual(detected, set(files), "known digests elsewhere must remain detectable")

    def reviewed_source_metadata_exceptions(self) -> dict[str, bytes]:
        """Bind each non-Cargo digest exception to the retained bytes it identifies."""
        policy = tomllib.loads(CONFIG.read_text(encoding="utf-8"))
        descriptions = {
            "Published native profile archive integrity tokens": (NATIVE_CLOSURE, "match"),
            "Published Scheme intertoken source digest": (GRAMMAR_LOCK, "secret"),
        }
        rules = {rule["description"]: rule for rule in policy["allowlists"]
                 if rule["description"] in descriptions}
        self.assertEqual(set(rules), set(descriptions))
        result = {}
        for description, (name, target) in descriptions.items():
            rule = rules[description]
            self.assertEqual(rule["paths"], ["^" + re.escape(name).replace(r"\-", "-") + "$"])
            self.assertEqual(rule["targetRules"], ["generic-api-key"])
            self.assertEqual(rule["condition"], "AND")
            self.assertEqual(rule["regexTarget"], target)
            if name == NATIVE_CLOSURE:
                document = json.loads((ROOT / name).read_bytes())
                self.assertEqual(len(rule["regexes"]), 3)
                matches = []
                for expression in rule["regexes"]:
                    packages = [package for package in document["packages"]
                                if re.fullmatch(expression, f'token": "{package["pin"]["integrity"]}"')]
                    self.assertEqual(len(packages), 1, "integrity exception must name one archive")
                    pin = packages[0]["pin"]
                    self.assertRegex(pin["archive_filename"], r"\A[a-f0-9]{64}\.tgz\Z")
                    archive = ROOT / Path(name).parent / "archives" / pin["archive_filename"]
                    token = "sha512-" + base64.b64encode(hashlib.sha512(archive.read_bytes()).digest()).decode()
                    self.assertEqual(token, pin["integrity"])
                    match = f'token": "{token}"'
                    self.assertEqual(expression, "^" + re.escape(match) + "$")
                    matches.append(match)
                result[name] = ("\n".join(matches) + "\n").encode()
            else:
                document = tomllib.loads((ROOT / name).read_text(encoding="utf-8"))
                grammar = next(item for item in document["grammars"] if item["language"] == "scheme")
                member = "upstream/grammar/intertoken.js"
                digest = grammar["vendored"]["files"][member]
                source = ROOT / Path(grammar["vendored"]["manifest_path"]).parent / member
                self.assertEqual(hashlib.sha256(source.read_bytes()).hexdigest(), digest)
                self.assertEqual(rule["regexes"], ["^" + digest + "$"])
                result[name] = f'api_key = "{digest}"\n'.encode()
        return result

    def test_reviewed_source_metadata_and_exact_exceptions_scan_clean(self) -> None:
        fixtures = self.reviewed_source_metadata_exceptions()
        for mode in ("dir", "git"):
            with self.subTest(mode=mode):
                self.assertFalse(self.scan(fixtures, mode))
                self.assertFalse(self.scan({name: (ROOT / name).read_bytes() for name in fixtures}, mode))

    def test_reviewed_metadata_exceptions_reject_changed_values_and_relocated_matches(self) -> None:
        fixtures = self.reviewed_source_metadata_exceptions()
        digest = hashlib.sha256(b"rootlight unrelated metadata negative control").hexdigest()
        unknown = f'api_key = "{digest}"\n'.encode()
        unknown += b"token = 'glpat-" + b"0123456789" + b"abcdefghijkl'\n"
        for mode in ("dir", "git"):
            with self.subTest(mode=mode):
                findings = self.scan({name: unknown for name in fixtures}, mode)
                detected = {(finding["File"], finding["RuleID"]) for finding in findings}
                for name in fixtures:
                    self.assertIn((name, "generic-api-key"), detected)
                    self.assertIn((name, "gitlab-pat"), detected)
                relocated = {"relocated/" + name: content for name, content in fixtures.items()}
                findings = self.scan(relocated, mode)
                self.assertEqual({finding["File"] for finding in findings
                                  if finding["RuleID"] == "generic-api-key"}, set(relocated))

    def test_authenticated_upstream_samples_remain_unchanged_and_scan_clean(self) -> None:
        paths = [CHECKSUM, PEM, WEBSOCKET, SQLITE, LINUX,
                 f"{WINDOWS}/Graphics/Direct3D12/mod.rs",
                 f"{WINDOWS}/NetworkManagement/WiFi/mod.rs",
                 f"{WINDOWS}/Security/Cryptography/Certificates/mod.rs"]
        self.assertFalse(self.scan({name: (ROOT / name).read_bytes() for name in paths}),
                         "authenticated upstream samples require exact exceptions")

    def test_non_ascii_upstream_documentation_exception_preserves_exact_text(self) -> None:
        content = (ROOT / YARN_NONASCII_DOC).read_bytes()
        for mode in ("dir", "git"):
            with self.subTest(mode=mode):
                self.assertFalse(self.scan({YARN_NONASCII_DOC: content}, mode),
                                 "the exact UTF-8 upstream documentation exception must survive edits")

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
