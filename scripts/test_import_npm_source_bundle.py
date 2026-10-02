#!/usr/bin/env python3
"""Exercise local npm source publication without acquisition or installation.

Synthetic archives pin every placement, member mode and license observation;
failure tests protect canonical inputs and distinguish evidence from acceptance.
"""

from __future__ import annotations

import base64
from contextlib import redirect_stderr, redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import import_npm_source_bundle as importer
from verify_npm_source_bundle import archive_filename, verify_bundle


class NpmImportTests(unittest.TestCase):
    """Pin publication and fail-closed behavior using isolated synthetic inputs."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.lock = self.root / "package-lock.json"
        self.inputs = self.root / "acquired"
        self.inputs.mkdir()
        self.parent = self.root / "source-inputs"
        self.parent.mkdir()
        self.destination = self.parent / "npm"
        self.document = {
            "name": "fixture-root", "version": "1.0.0", "lockfileVersion": 3,
            "packages": {"": {"name": "fixture-root", "version": "1.0.0"}},
        }
        self.add_package("sample-input", "1.0.0", [
            ("node_modules/sample-input", {"dev": True}),
            ("node_modules/parent-input/node_modules/sample-input", {"optional": True}),
        ])
        self.add_package("@scope/platform-input", "2.0.0", [
            ("node_modules/@scope/platform-input", {"optional": True, "os": ["darwin"]}),
        ])
        self.write_lock()

    def add_package(self, name, version, placements) -> None:
        basename = name.split("/")[-1]
        url = f"https://registry.npmjs.org/{name}/-/{basename}-{version}.tgz"
        manifest = {
            "name": name, "version": version, "license": "MIT",
            "repository": {"type": "git", "url": "https://example.invalid/source.git"},
            "gitHead": "a" * 40,
        }
        members = [
            ("package.json", json.dumps(manifest).encode(), 0o644),
            ("LICENSE", b"synthetic license evidence\n", 0o644),
            ("bin/tool.js", b"#!/usr/bin/env node\n", 0o755),
            ("binding.node", b"\x7fELFsynthetic native observation\n", 0o644),
        ]
        contents = io.BytesIO()
        with tarfile.open(fileobj=contents, mode="w:gz") as archive:
            for path, payload, mode in members:
                member = tarfile.TarInfo("package/" + path)
                member.size, member.mode = len(payload), mode
                archive.addfile(member, io.BytesIO(payload))
        payload = contents.getvalue()
        (self.inputs / archive_filename(url)).write_bytes(payload)
        for placement, metadata in placements:
            self.document["packages"][placement] = {
                "version": version, "resolved": url,
                "integrity": "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode(),
                "license": "MIT", **metadata,
            }

    def write_lock(self) -> None:
        self.lock.write_text(json.dumps(self.document), encoding="utf-8")

    def snapshot(self) -> dict[str, bytes]:
        return {entry.name: entry.read_bytes() for entry in self.inputs.iterdir()}

    def assert_unpublished(self) -> None:
        self.assertFalse(self.destination.exists())
        self.assertEqual(list(self.parent.iterdir()), [])

    def test_exact_lock_union_bytes_modes_metadata_and_independent_reverification(self) -> None:
        original_lock, original_archives = self.lock.read_bytes(), self.snapshot()
        cache = self.root / "empty-npm-cache"
        cache.mkdir()
        with patch.dict(os.environ, {"npm_config_cache": str(cache), "NPM_CONFIG_CACHE": str(cache)}), \
                patch.object(importer.prepare, "download_archive", side_effect=AssertionError("network forbidden")) as network, \
                patch("subprocess.run", side_effect=AssertionError("installer forbidden")) as process:
            evidence = importer.import_bundle(self.lock, self.inputs, self.destination)
        network.assert_not_called()
        process.assert_not_called()
        self.assertEqual(list(cache.iterdir()), [])
        archives = self.destination / importer.ARCHIVES_NAME
        self.assertEqual({entry.name for entry in self.destination.iterdir()}, {
            importer.ARCHIVES_NAME, importer.REPORT_NAME,
        })
        self.assertEqual({entry.name: entry.read_bytes() for entry in archives.iterdir()}, original_archives)
        self.assertEqual(self.lock.read_bytes(), original_lock)
        self.assertEqual(self.snapshot(), original_archives)
        self.assertEqual(evidence["archive_count"], 2)
        self.assertEqual(evidence["lock_record_count"], 3)
        independently_verified = verify_bundle(self.lock, archives)
        for key, value in independently_verified.items():
            self.assertEqual(evidence[key], value)
        report = (self.destination / importer.REPORT_NAME).read_bytes()
        self.assertEqual(report, (json.dumps(evidence, sort_keys=True, indent=2) + "\n").encode())
        by_name = {record["name"]: record for record in evidence["packages"]}
        self.assertEqual(by_name["sample-input"]["lock_paths"], [
            "node_modules/parent-input/node_modules/sample-input", "node_modules/sample-input",
        ])
        self.assertEqual(by_name["sample-input"]["lock_records"], [
            {"path": "node_modules/parent-input/node_modules/sample-input", "optional": True, "license": "MIT"},
            {"path": "node_modules/sample-input", "dev": True, "license": "MIT"},
        ])
        for record in evidence["packages"]:
            self.assertEqual(record["license_metadata"], {"license": "MIT"})
            self.assertEqual(record["provenance_metadata"]["gitHead"], "a" * 40)
            self.assertEqual([member["path"] for member in record["license_files"]], ["package/LICENSE"])
            self.assertEqual([member["path"] for member in record["native_binary_files"]], ["package/binding.node"])
            self.assertEqual(next(member["archive_mode"] for member in record["members"]
                                  if member["path"] == "package/bin/tool.js"), 0o755)
        for claim in ("preferred_source_closure_complete", "frontend_source_complete",
                      "provenance_signatures_verified", "installation_verified",
                      "build_and_packaging_acceptance_verified", "os_denied_acceptance_verified"):
            self.assertIs(evidence[claim], False)

    def test_missing_optional_archive_is_rejected_before_any_destination_change(self) -> None:
        optional = self.document["packages"]["node_modules/@scope/platform-input"]
        (self.inputs / archive_filename(optional["resolved"])).unlink()
        original = self.snapshot()
        with self.assertRaisesRegex(importer.ImportPreparationError, "missing archives"):
            importer.import_bundle(self.lock, self.inputs, self.destination)
        self.assert_unpublished()
        self.assertEqual(self.snapshot(), original)

    def test_tampered_or_extra_archive_is_rejected_before_writes(self) -> None:
        for change in ("tamper", "extra"):
            with self.subTest(change=change):
                originals = self.snapshot()
                target = next(self.inputs.iterdir()) if change == "tamper" else self.inputs / "extra.tgz"
                target.write_bytes(b"unapproved")
                with self.assertRaises(importer.ImportPreparationError):
                    importer.import_bundle(self.lock, self.inputs, self.destination)
                self.assert_unpublished()
                if change == "extra":
                    target.unlink()
                else:
                    target.write_bytes(originals[target.name])

    def test_authenticated_unsafe_archive_is_rejected_without_extracting_it(self) -> None:
        identity = self.document["packages"]["node_modules/@scope/platform-input"]
        contents = io.BytesIO()
        with tarfile.open(fileobj=contents, mode="w:gz") as archive:
            member = tarfile.TarInfo("package/../../escape.js")
            payload = b"never extracted"
            member.size = len(payload)
            archive.addfile(member, io.BytesIO(payload))
        payload = contents.getvalue()
        target = self.inputs / archive_filename(identity["resolved"])
        target.write_bytes(payload)
        identity["integrity"] = "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode()
        self.write_lock()
        with self.assertRaises(importer.ImportPreparationError):
            importer.import_bundle(self.lock, self.inputs, self.destination)
        self.assert_unpublished()
        self.assertFalse((self.root / "escape.js").exists())
        self.assertEqual(target.read_bytes(), payload)

    def test_published_mutation_or_report_inside_archives_fails_fresh_verification(self) -> None:
        importer.import_bundle(self.lock, self.inputs, self.destination)
        archives = self.destination / importer.ARCHIVES_NAME
        report_bytes = (self.destination / importer.REPORT_NAME).read_bytes()
        misplaced = archives / importer.REPORT_NAME
        misplaced.write_bytes(report_bytes)
        with self.assertRaisesRegex(ValueError, "extra archives"):
            verify_bundle(self.lock, archives)
        misplaced.unlink()
        next(archives.iterdir()).write_bytes(b"changed after publication")
        with self.assertRaises(ValueError):
            verify_bundle(self.lock, archives)
        self.assertEqual((self.destination / importer.REPORT_NAME).read_bytes(), report_bytes)

    def test_empty_locked_closure_is_not_published(self) -> None:
        for archive in self.inputs.iterdir():
            archive.unlink()
        self.document["packages"] = {"": {"name": "fixture-root", "version": "1.0.0"}}
        self.write_lock()
        with self.assertRaisesRegex(importer.ImportPreparationError, "inventory is empty"):
            importer.import_bundle(self.lock, self.inputs, self.destination)
        self.assert_unpublished()

    def test_existing_directory_file_and_empty_destination_are_not_replaced(self) -> None:
        for kind in ("empty", "sentinel", "file"):
            with self.subTest(kind=kind):
                if kind == "file":
                    self.destination.write_bytes(b"preserve file")
                else:
                    self.destination.mkdir()
                    if kind == "sentinel":
                        (self.destination / "keep").write_bytes(b"preserve directory")
                with self.assertRaisesRegex(importer.ImportPreparationError, "already exists"):
                    importer.import_bundle(self.lock, self.inputs, self.destination)
                if kind == "file":
                    self.assertEqual(self.destination.read_bytes(), b"preserve file")
                    self.destination.unlink()
                else:
                    if kind == "sentinel":
                        self.assertEqual((self.destination / "keep").read_bytes(), b"preserve directory")
                        (self.destination / "keep").unlink()
                    self.destination.rmdir()

    def test_traversal_missing_parent_and_output_inside_input_fail_closed(self) -> None:
        for target in (self.parent / ".." / "escape", self.root / "absent" / "npm", self.inputs / "npm"):
            with self.subTest(target=target), self.assertRaises(importer.ImportPreparationError):
                importer.import_bundle(self.lock, self.inputs, target)
            self.assertFalse(target.exists())
        for lock, archives in ((self.root / "unused" / ".." / self.lock.name, self.inputs),
                               (self.lock, self.root / "unused" / ".." / self.inputs.name)):
            with self.subTest(lock=lock, archives=archives), self.assertRaisesRegex(importer.ImportPreparationError, "traversal"):
                importer.import_bundle(lock, archives, self.destination)
        self.assert_unpublished()

    def test_lock_mutation_during_copy_prevents_publication(self) -> None:
        prepare_bundle = importer.prepare.prepare_bundle

        def copy_then_change(*args, **kwargs):
            evidence = prepare_bundle(*args, **kwargs)
            self.document["packages"]["node_modules/sample-input"]["dev"] = False
            self.write_lock()
            return evidence

        with patch.object(importer.prepare, "prepare_bundle", side_effect=copy_then_change):
            with self.assertRaisesRegex(importer.ImportPreparationError, "changed during import"):
                importer.import_bundle(self.lock, self.inputs, self.destination)
        self.assert_unpublished()

    def test_source_mutation_after_copy_is_reverified_before_publication(self) -> None:
        prepare_bundle = importer.prepare.prepare_bundle

        def copy_then_change(*args, **kwargs):
            evidence = prepare_bundle(*args, **kwargs)
            next(self.inputs.iterdir()).write_bytes(b"changed after copy")
            return evidence

        with patch.object(importer.prepare, "prepare_bundle", side_effect=copy_then_change):
            with self.assertRaises(importer.ImportPreparationError):
                importer.import_bundle(self.lock, self.inputs, self.destination)
        self.assert_unpublished()

    def test_tampered_staging_and_late_verifier_failure_leave_no_output(self) -> None:
        prepare_bundle = importer.prepare.prepare_bundle

        def copy_then_tamper(*args, **kwargs):
            evidence = prepare_bundle(*args, **kwargs)
            next(Path(args[1]).iterdir()).write_bytes(b"changed copied bytes")
            return evidence

        with patch.object(importer.prepare, "prepare_bundle", side_effect=copy_then_tamper):
            with self.assertRaises(importer.ImportPreparationError):
                importer.import_bundle(self.lock, self.inputs, self.destination)
        self.assert_unpublished()
        with patch.object(importer, "verify_bundle", side_effect=[
            verify_bundle(self.lock, self.inputs), ValueError("late verifier failure"),
        ]):
            with self.assertRaisesRegex(importer.ImportPreparationError, "late verifier failure"):
                importer.import_bundle(self.lock, self.inputs, self.destination)
        self.assert_unpublished()

    def test_destination_appearing_before_publication_is_preserved(self) -> None:
        real_verify = importer.verify_bundle

        def verify_and_create(lock, archives):
            evidence = real_verify(lock, archives)
            if Path(archives) == self.inputs and any(self.parent.glob(".npm-source-inputs-*")):
                self.destination.mkdir()
                (self.destination / "keep").write_bytes(b"concurrent destination")
            return evidence

        with patch.object(importer, "verify_bundle", side_effect=verify_and_create):
            with self.assertRaisesRegex(importer.ImportPreparationError, "already exists"):
                importer.import_bundle(self.lock, self.inputs, self.destination)
        self.assertEqual((self.destination / "keep").read_bytes(), b"concurrent destination")
        self.assertEqual({entry.name for entry in self.parent.iterdir()}, {"npm"})

    def test_links_and_reparse_inputs_or_parents_are_rejected(self) -> None:
        real_lstat = Path.lstat

        class LinkedStat:
            def __init__(self, metadata, reparse):
                self.st_mode = metadata.st_mode if reparse else stat.S_IFLNK
                self.st_file_attributes = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 1024)

        for path in (self.lock, self.inputs, self.parent, next(self.inputs.iterdir())):
            for reparse in (False, True):
                def linked_lstat(candidate, *, _target=path, _reparse=reparse):
                    metadata = real_lstat(candidate)
                    return LinkedStat(metadata, _reparse) if candidate == _target else metadata

                with self.subTest(path=path, reparse=reparse), patch.object(Path, "lstat", linked_lstat):
                    with self.assertRaises(importer.ImportPreparationError):
                        importer.import_bundle(self.lock, self.inputs, self.destination)
                self.assert_unpublished()

    def test_cli_success_and_failure_do_not_expose_a_false_acceptance_claim(self) -> None:
        output = io.StringIO()
        arguments = ["--lock", str(self.lock), "--archives", str(self.inputs),
                     "--destination", str(self.destination)]
        with redirect_stdout(output):
            self.assertEqual(importer.main(arguments), 0)
        summary = json.loads(output.getvalue())
        self.assertTrue(summary["archives_verified"])
        self.assertIs(summary["build_and_packaging_acceptance_verified"], False)
        self.assertIs(summary["os_denied_acceptance_verified"], False)
        error = io.StringIO()
        with redirect_stderr(error), self.assertRaises(SystemExit) as failure:
            importer.main(arguments)
        self.assertEqual(failure.exception.code, 1)
        self.assertIn("already exists", error.getvalue())


if __name__ == "__main__":
    unittest.main()
