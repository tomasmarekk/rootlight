#!/usr/bin/env python3
"""Exercise controlled npm archive preparation without npm or real downloads.

Synthetic locked tarballs bind acquisition to immutable integrity; mock network
paths distinguish preparation from any compilation or installation acceptance.
"""

from __future__ import annotations

import base64
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import prepare_npm_source_bundle as prepare


class NpmPreparationTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.lock = self.root / "package-lock.json"
        self.inputs = self.root / "acquired"
        self.inputs.mkdir()
        self.destination = self.root / "prepared"
        self.url = "https://registry.npmjs.org/sample-input/-/sample-input-1.0.0.tgz"
        content = io.BytesIO()
        with tarfile.open(fileobj=content, mode="w:gz") as archive:
            for name, payload in {
                "package.json": json.dumps({"name": "sample-input", "version": "1.0.0", "license": "MIT"}).encode(),
                "index.js": b"export const value = 1;\n",
                "LICENSE": b"synthetic license fixture\n",
            }.items():
                member = tarfile.TarInfo("package/" + name)
                member.size = len(payload)
                member.mode = 0o644
                archive.addfile(member, io.BytesIO(payload))
        self.payload = content.getvalue()
        self.archive = self.inputs / prepare.archive_filename(self.url)
        self.archive.write_bytes(self.payload)
        self.document = {
            "name": "fixture-root", "version": "1.0.0", "lockfileVersion": 3,
            "packages": {
                "": {"name": "fixture-root", "version": "1.0.0"},
                "node_modules/sample-input": {
                    "version": "1.0.0", "resolved": self.url,
                    "integrity": "sha512-" + base64.b64encode(hashlib.sha512(self.payload).digest()).decode(),
                    "license": "MIT",
                },
            },
        }
        self.write_lock()

    def write_lock(self) -> None:
        self.lock.write_text(json.dumps(self.document), encoding="utf-8")

    def test_offline_copy_authenticates_inputs_without_touching_canonical_sources(self) -> None:
        original_lock = self.lock.read_bytes()
        with patch.object(prepare, "download_archive") as download:
            evidence = prepare.prepare_bundle(self.lock, self.destination, acquired_archives=self.inputs)
        download.assert_not_called()
        self.assertTrue(evidence["archives_verified"])
        self.assertEqual(evidence["archive_count"], 1)
        self.assertEqual(evidence["lock_record_count"], 1)
        self.assertFalse(evidence["preferred_source_closure_complete"])
        self.assertFalse(evidence["frontend_source_complete"])
        self.assertEqual(self.lock.read_bytes(), original_lock)
        self.assertEqual(self.archive.read_bytes(), self.payload)
        self.assertEqual((self.destination / self.archive.name).read_bytes(), self.payload)

    def test_network_acquisition_requires_explicit_separate_mode(self) -> None:
        for mode in ({}, {"acquired_archives": self.inputs, "allow_network": True}):
            with self.subTest(mode=mode), self.assertRaises(prepare.PreparationError):
                prepare.prepare_bundle(self.lock, self.destination, **mode)
        self.assertFalse(self.destination.exists())

    def test_explicit_network_acquisition_is_verified_without_subprocesses(self) -> None:
        def acquire(url, destination):
            self.assertEqual(url, self.url)
            destination.write_bytes(self.payload)

        with patch.object(prepare, "download_archive", side_effect=acquire) as download:
            evidence = prepare.prepare_bundle(self.lock, self.destination, allow_network=True)
        self.assertEqual(download.call_count, 1)
        self.assertTrue(evidence["archives_verified"])

    def test_tampered_acquired_archive_fails_instead_of_publishing_verified_evidence(self) -> None:
        self.archive.write_bytes(b"not the authenticated archive")
        with self.assertRaises(ValueError):
            prepare.prepare_bundle(self.lock, self.destination, acquired_archives=self.inputs)

    def test_missing_optional_archive_is_not_silently_excluded(self) -> None:
        self.document["packages"]["node_modules/sample-input"]["optional"] = True
        self.write_lock()
        self.archive.unlink()
        with self.assertRaises(OSError):
            prepare.prepare_bundle(self.lock, self.destination, acquired_archives=self.inputs)

    def test_existing_output_is_preserved(self) -> None:
        self.destination.mkdir()
        sentinel = self.destination / "retain"
        sentinel.write_bytes(b"existing evidence")
        with self.assertRaisesRegex(prepare.PreparationError, "already exists"):
            prepare.prepare_bundle(self.lock, self.destination, acquired_archives=self.inputs)
        self.assertEqual(sentinel.read_bytes(), b"existing evidence")

    def test_output_inside_acquired_input_is_rejected_before_writes(self) -> None:
        nested = self.inputs / "prepared"
        with self.assertRaisesRegex(prepare.PreparationError, "inside"):
            prepare.prepare_bundle(self.lock, nested, acquired_archives=self.inputs)
        self.assertFalse(nested.exists())

    def test_changed_lock_fails_after_acquisition(self) -> None:
        def acquire(_url, destination):
            destination.write_bytes(self.payload)
            self.document["packages"]["node_modules/sample-input"]["dev"] = True
            self.write_lock()

        with patch.object(prepare, "download_archive", side_effect=acquire):
            with self.assertRaisesRegex(prepare.PreparationError, "changed during"):
                prepare.prepare_bundle(self.lock, self.destination, allow_network=True)

    def test_network_failure_cannot_return_verified_evidence(self) -> None:
        with patch.object(prepare, "download_archive", side_effect=OSError("unavailable")):
            with self.assertRaises(OSError):
                prepare.prepare_bundle(self.lock, self.destination, allow_network=True)

    def test_redirects_outside_approved_archive_urls_fail_before_request(self) -> None:
        for url in ("http://registry.npmjs.org/sample.tgz", "https://example.invalid/sample.tgz",
                    "https://registry.npmjs.org/sample.tgz?token=secret", "file:///local.tgz"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                prepare.RegistryRedirectHandler().redirect_request(None, None, 302, "redirect", {}, url)

    def test_evidence_inside_exact_archive_inventory_is_rejected(self) -> None:
        completed = subprocess.run([
            sys.executable, str(Path(prepare.__file__)), "--lock", str(self.lock),
            "--destination", str(self.destination), "--from-archives", str(self.inputs),
            "--output", str(self.destination / "evidence.json"),
        ], capture_output=True, text=True, check=False)
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("outside the exact archive inventory", completed.stderr)
        self.assertFalse(self.destination.exists())

    @unittest.skipIf(sys.platform == "win32", "unprivileged symlinks are not guaranteed on Windows")
    def test_linked_inputs_and_output_ancestors_are_rejected(self) -> None:
        linked = self.root / "linked"
        linked.symlink_to(self.inputs, target_is_directory=True)
        for mode in (
            {"destination": self.destination, "acquired_archives": linked},
            {"destination": linked / "output", "acquired_archives": self.inputs},
        ):
            with self.subTest(mode=mode), self.assertRaises(prepare.PreparationError):
                prepare.prepare_bundle(self.lock, **mode)


if __name__ == "__main__":
    unittest.main()
