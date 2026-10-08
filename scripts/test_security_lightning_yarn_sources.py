#!/usr/bin/env python3
"""Verify exact Yarn source exceptions with the installed secret scanner.

Authenticated public metadata and compiler constructs retain their original
bytes. Exceptions must not hide changed matches, paths, or new credentials.
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

import lightning_yarn_pins as yarn


ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / ".gitleaks.toml"
PREFIX = "Published Yarn "


def _literal(expression: str) -> str:
    if not expression.startswith("^") or not expression.endswith("$"):
        raise ValueError("exception must be anchored")
    return re.sub(r"\\(.)", lambda match: {"n": "\n", "r": "\r", "t": "\t"}.get(
        match[1], match[1]), expression[1:-1])


class YarnSecurityPolicyTests(unittest.TestCase):
    """Require narrow, fail-closed exceptions for each reviewed upstream match."""

    @classmethod
    def setUpClass(cls) -> None:
        """Load exact rules and discover the scanner without installing tools."""
        cls.scanner = shutil.which("gitleaks")
        policy = tomllib.loads(CONFIG.read_text(encoding="utf-8"))
        cls.exceptions = [rule for rule in policy["allowlists"]
                          if rule.get("description", "").startswith(PREFIX)]
        if len(cls.exceptions) != 11:
            raise RuntimeError("all eleven reviewed Yarn findings require exact rules")
        cls.inventory_exception = next(rule for rule in policy["allowlists"]
                                       if rule.get("description") == "Published Lightning Yarn integrity tokens")

    def scan(self, files: dict[str, bytes], config: Path = CONFIG) -> list[dict]:
        """Scan isolated local files, returning only fully redacted findings.

        Scanner errors and absent reports are failures, not clean results.
        The fixture directory contains no upstream ignore file or allow comment.
        """
        if self.scanner is None:
            self.skipTest("installed gitleaks unavailable; scanner integration is not qualified")
        with tempfile.TemporaryDirectory(prefix="rootlight-yarn-policy-") as temporary:
            base = Path(temporary)
            source = base / "source"
            source.mkdir()
            for name, content in files.items():
                destination = source / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(content)
            report = base / "redacted.json"
            result = subprocess.run(
                [self.scanner, "dir", "--config", str(config), "--redact=100",
                 "--no-banner", "--no-color", "--log-level", "error",
                 "--max-archive-depth", "0", "--max-decode-depth", "5",
                 "--max-target-megabytes", "0", "--ignore-gitleaks-allow",
                 "--gitleaks-ignore-path", str(base / "absent-ignore"),
                 "--report-format", "json", "--report-path", str(report), "."],
                cwd=source, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=False,
            )
            self.assertIn(result.returncode, (0, 1), "scanner execution failed")
            self.assertTrue(report.is_file(), "scanner omitted its report")
            findings = json.loads(report.read_bytes())
            self.assertIsInstance(findings, list)
            for finding in findings:
                self.assertEqual(finding.get("Secret"), "REDACTED")
            self.assertEqual(result.returncode, int(bool(findings)))
            return findings

    def minimal_samples(self) -> dict[str, bytes]:
        """Return literal detector matches with syntactically inert scan context."""
        samples = {}
        for rule in self.exceptions:
            name = _literal(rule["paths"][0])
            match = _literal(rule["regexes"][0])
            if match.endswith("void "):
                match += "0;"
            samples[name] = samples.get(name, b"") + match.encode() + b"\n"
        return samples

    def test_rules_are_exact_path_rule_and_match_conjunctions(self) -> None:
        for rule in self.exceptions:
            with self.subTest(description=rule["description"]):
                self.assertEqual(rule["condition"], "AND")
                self.assertEqual(rule["regexTarget"], "match")
                self.assertIn(rule["targetRules"], (["generic-api-key"], ["private-key"]))
                self.assertEqual(len(rule["paths"]), 1)
                self.assertEqual(len(rule["regexes"]), 1)
                name = _literal(rule["paths"][0])
                self.assertTrue(name.startswith("source-inputs/producer-dependencies/"))
                self.assertIn("/yarn-originals/packages/", name)
                self.assertEqual(rule["paths"][0], "^" + re.escape(name).replace(r"\-", "-") + "$")
                self.assertTrue(re.fullmatch(rule["regexes"][0], _literal(rule["regexes"][0])))

    def test_exact_match_shapes_reject_added_and_changed_bytes(self) -> None:
        for rule in self.exceptions:
            with self.subTest(description=rule["description"]):
                expression = rule["regexes"][0]
                literal = _literal(expression)
                for changed in ("prefix" + literal, literal + "suffix",
                                literal.replace("...", "....", 1) if "..." in literal
                                else literal.replace(next(char for char in literal if char.isalpha()), "_", 1)):
                    self.assertNotEqual(changed, literal)
                    self.assertIsNone(re.fullmatch(expression, changed),
                                      "the exception must authorize only exact reviewed bytes")

    def test_exact_paths_reject_relocation_and_other_members(self) -> None:
        for rule in self.exceptions:
            with self.subTest(description=rule["description"]):
                expression = rule["paths"][0]
                literal = _literal(expression)
                for changed in ("relocated/" + literal, literal + ".other",
                                literal.replace("/packages/", "/other-packages/", 1)):
                    self.assertIsNone(re.fullmatch(expression, changed),
                                      "the exception must remain source-member bound")

    def test_authenticated_source_samples_scan_clean(self) -> None:
        names = {_literal(rule["paths"][0]) for rule in self.exceptions}
        self.assertFalse(self.scan({name: (ROOT / name).read_bytes() for name in names}),
                         "retained authenticated source members must scan clean")

    def test_each_exception_reproduces_a_real_detector_match(self) -> None:
        config = CONFIG.read_text(encoding="utf-8")
        sections = config.split("[[allowlists]]")
        baseline = sections[0] + "".join("[[allowlists]]" + section
                                        for section in sections[1:]
                                        if 'description = "' + PREFIX not in section)
        with tempfile.TemporaryDirectory(prefix="rootlight-yarn-baseline-") as temporary:
            path = Path(temporary) / "baseline.toml"
            path.write_text(baseline, encoding="utf-8")
            findings = self.scan(self.minimal_samples(), path)
        self.assertEqual(len(findings), 11, "all reviewed matches must reproduce without exceptions")
        self.assertFalse(self.scan(self.minimal_samples()), "exact minimal matches must scan clean")

    def test_known_matches_relocated_to_other_paths_are_detected(self) -> None:
        files = {"relocated/" + name: content for name, content in self.minimal_samples().items()}
        findings = self.scan(files)
        self.assertEqual(len(findings), 11, "no exception may survive relocation")

    def test_changed_context_with_same_inert_value_is_detected(self) -> None:
        samples = {}
        expected = []
        for rule in self.exceptions:
            name = _literal(rule["paths"][0])
            match = _literal(rule["regexes"][0])
            if rule["targetRules"] == ["private-key"]:
                changed = match.replace("...", "....", 1)
            else:
                first = next(index for index, char in enumerate(match) if char.isalpha())
                changed = match[:first] + match[first].swapcase() + match[first + 1:]
                if changed.endswith("void "):
                    changed += "0;"
            self.assertNotEqual(changed, match)
            samples[name] = samples.get(name, b"") + changed.encode() + b"\n"
            expected.append((name, rule["targetRules"][0]))
        findings = self.scan(samples)
        detected = [(finding["File"], finding["RuleID"]) for finding in findings]
        self.assertCountEqual(detected, expected, "exact source context must remain significant")

    def test_new_credentials_in_every_exception_path_are_detected(self) -> None:
        generic = hashlib.sha256(b"rootlight synthetic yarn negative credential").hexdigest()
        seed = hashlib.sha256(b"rootlight synthetic nonproduction yarn private key").digest()
        encoded = base64.b64encode(bytes.fromhex("302e020100300506032b657004220420") + seed)
        private = b"-----BEGIN " + b"PRIVATE KEY-----\n" + encoded
        private += b"\n-----END " + b"PRIVATE KEY-----\n"
        content = f'api_key = "{generic}"\n'.encode() + private
        content += b"token = 'glpat-" + b"0123456789" + b"abcdefghijkl'\n"
        names = {_literal(rule["paths"][0]) for rule in self.exceptions}
        findings = self.scan({name: content for name in names})
        detected = {(finding["File"], finding["RuleID"]) for finding in findings}
        for name in names:
            for rule in ("generic-api-key", "private-key", "gitlab-pat"):
                self.assertIn((name, rule), detected, "same-path new credentials must be detected")

    def test_inventory_exceptions_are_exact_authored_integrity_tokens(self) -> None:
        rule = self.inventory_exception
        self.assertEqual(rule["condition"], "AND")
        self.assertEqual(rule["regexTarget"], "match")
        self.assertEqual(rule["targetRules"], ["generic-api-key"])
        self.assertEqual(len(rule["paths"]), 1)
        self.assertEqual(len(rule["regexes"]), 24)
        name = _literal(rule["paths"][0])
        self.assertTrue(name.endswith("/yarn-originals/yarn-source-closure.json"))
        self.assertEqual(rule["paths"][0], "^" + re.escape(name).replace(r"\-", "-") + "$")
        producer = name.split("/yarn-originals/", 1)[0].replace("/producer-dependencies/", "/producers/") + "/tree"
        graph = yarn.parse_yarn_lock((ROOT / producer / "yarn.lock").read_bytes(),
                                     (ROOT / producer / "package.json").read_bytes())
        authenticated = {pin["integrity"] for pin in graph["packages"]}
        for expression in rule["regexes"]:
            literal = _literal(expression)
            self.assertTrue(literal.startswith('token": "') and literal.endswith('"'))
            value = literal.removeprefix('token": "').removesuffix('"')
            self.assertIn(value, authenticated, "only authored SRI metadata may be excepted")
            self.assertEqual(len(base64.b64decode(value[7:], validate=True)), 64)
            self.assertIsNone(re.fullmatch(expression, literal.replace("token", "api_key", 1)))

    def test_inventory_context_relocation_and_new_credentials_remain_detectable(self) -> None:
        rule = self.inventory_exception
        name = _literal(rule["paths"][0])
        originals = "\n".join(_literal(expression) for expression in rule["regexes"]).encode()
        config = CONFIG.read_text(encoding="utf-8")
        sections = config.split("[[allowlists]]")
        baseline = sections[0] + "".join("[[allowlists]]" + section
                                        for section in sections[1:]
                                        if 'description = "Published Lightning Yarn integrity tokens"' not in section)
        with tempfile.TemporaryDirectory(prefix="rootlight-yarn-inventory-") as temporary:
            path = Path(temporary) / "baseline.toml"
            path.write_text(baseline, encoding="utf-8")
            self.assertEqual(len(self.scan({name: originals}, path)), 24)
        self.assertFalse(self.scan({name: originals}))
        self.assertFalse(self.scan({name: (ROOT / name).read_bytes()}))
        self.assertEqual(len(self.scan({"relocated/" + name: originals})), 24)
        self.assertEqual(len(self.scan({name: originals.replace(b"token", b"Token")})), 24)
        synthetic = hashlib.sha256(b"rootlight synthetic yarn inventory credential").hexdigest()
        changed = f'api_key = "{synthetic}"\n'.encode()
        changed += b"token = 'glpat-" + b"0123456789" + b"abcdefghijkl'\n"
        detected = {finding["RuleID"] for finding in self.scan({name: changed})}
        self.assertIn("generic-api-key", detected)
        self.assertIn("gitlab-pat", detected)


if __name__ == "__main__":
    unittest.main()
