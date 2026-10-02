"""Atomic publication and stored-tree regressions for the local producer importer.

Only authored temporary inputs are imported. No actual producer source tree,
dependency cache, installer, network or shared project input is mutated.
"""

from __future__ import annotations

import os
from pathlib import Path
import socket
import subprocess
import tarfile
import unittest
from unittest.mock import patch

from test_verify_lightning_producer_sources import AuthoredFixture, archive, verifier
import import_lightning_producer_sources as importer


class ImportTests(AuthoredFixture, unittest.TestCase):
    def publish(self):
        """Run the proposed importer against this test's authored fixture only."""
        return importer.import_bundle(self.project, self.prepared, self.capture, self.destination)

    def test_nested_empty_directory_without_explicit_parent_record_reverifies(self):
        empty = tarfile.TarInfo(verifier.ROOT + "/documentation/empty")
        empty.type, empty.mode = tarfile.DIRTYPE, 0o775
        self.payload = archive(self.files, [empty])
        with patch.object(verifier, "ARCHIVE_SHA256", verifier.digest(self.payload)):
            self.write_prepared()
            result = self.publish()
            self.assertEqual(verifier.verify_bundle(self.project, self.destination), result)
            recorded = {item["path"] for item in result["source_inventory"]["members"]}
            self.assertIn(empty.name, recorded)
            self.assertNotIn(verifier.ROOT + "/documentation", recorded)
            directory = self.destination / "tree/documentation/empty"
            self.assertTrue(directory.is_dir())
            self.assertEqual(list(directory.iterdir()), [])

    def test_publish_and_reverify_exact_tree_inputs_modes_and_false_claims(self):
        before = {path: path.read_bytes() for path in self.project.rglob("*") if path.is_file()}
        cache = self.project / "empty-cache"
        cache.mkdir()
        with patch.object(subprocess, "run", side_effect=AssertionError("subprocess is prohibited")), \
                patch.object(socket.socket, "connect", side_effect=AssertionError("network is prohibited")), \
                patch.dict(os.environ, {"CARGO_HOME": str(cache), "npm_config_cache": str(cache)}):
            result = self.publish()
            self.assertEqual(verifier.verify_bundle(self.project, self.destination), result)
        self.assertEqual(list(cache.iterdir()), [])
        self.assertEqual({path: path.read_bytes() for path in before}, before)
        self.assertEqual({entry.name for entry in self.destination.iterdir()}, verifier.STORED_NAMES)
        for name, content in self.files.items():
            relative = name[len(verifier.ROOT) + 1:]
            self.assertEqual((self.destination / "tree" / relative).read_bytes(), content)
        self.assertEqual((self.destination / verifier.ARCHIVE_NAME).read_bytes(), self.payload)
        self.assertEqual((self.destination / "publisher-capture.json").read_bytes(), self.capture.read_bytes())
        self.assertEqual((self.destination / "source-tree.json").read_bytes(), verifier.json_bytes(result))
        self.assertFalse(result["preferred_source_closure_complete"])
        self.assertFalse(result["producer_transitive_sources_complete"])
        self.assertFalse(result["source_to_binary_rebuild_verified"])
        self.assertFalse(result["build_and_packaging_acceptance_verified"])
        self.assertFalse(result["frontend_source_complete"])
        self.assertFalse(result["os_denied_acceptance_verified"])
        if os.name != "nt":
            self.assertEqual((self.destination / "tree/scripts/build.js").stat().st_mode & 0o777, 0o755)
            self.assertEqual((self.destination / "tree/src/value.rs").stat().st_mode & 0o777, 0o644)

    def test_preexisting_wrong_namespace_or_parent_traversal_reject(self):
        outside = self.project / "outside"
        for output in [outside, self.destination.parent / "other", self.destination.parent / ".." / self.destination.name]:
            with self.subTest(output=output):
                with self.assertRaises(ValueError):
                    importer.import_bundle(self.project, self.prepared, self.capture, output)
                self.assertFalse(self.destination.exists())
        self.destination.mkdir()
        (self.destination / "keep").write_bytes(b"existing source")
        with self.assertRaises(ValueError):
            self.publish()
        self.assertEqual((self.destination / "keep").read_bytes(), b"existing source")

    def test_missing_or_tampered_input_has_no_destination_changes(self):
        parent_before = set(self.destination.parent.iterdir())
        archive_path = self.prepared / verifier.ARCHIVE_NAME
        archive_path.write_bytes(self.payload + b"tampered")
        with self.assertRaises(ValueError):
            self.publish()
        self.assertEqual(set(self.destination.parent.iterdir()), parent_before)
        archive_path.unlink()
        with self.assertRaises(ValueError):
            self.publish()
        self.assertEqual(set(self.destination.parent.iterdir()), parent_before)

    def test_late_prepared_mutation_discards_staging_without_publication(self):
        original = verifier.validate_prepared
        calls = 0

        def validate(*arguments):
            nonlocal calls
            calls += 1
            if calls == 2:
                path = self.prepared / "publisher-metadata.json"
                path.write_bytes(path.read_bytes() + b"\n")
            return original(*arguments)

        with patch.object(verifier, "validate_prepared", side_effect=validate):
            with self.assertRaises(ValueError):
                self.publish()
        self.assertEqual(calls, 2)
        self.assertFalse(self.destination.exists())
        self.assertEqual(list(self.destination.parent.iterdir()), [])

    def test_late_destination_creation_is_not_replaced(self):
        original = verifier.validate_prepared
        calls = 0

        def validate(*arguments):
            nonlocal calls
            calls += 1
            result = original(*arguments)
            if calls == 2:
                self.destination.mkdir()
                (self.destination / "keep").write_bytes(b"other writer")
            return result

        with patch.object(verifier, "validate_prepared", side_effect=validate):
            with self.assertRaises(ValueError):
                self.publish()
        self.assertEqual((self.destination / "keep").read_bytes(), b"other writer")
        self.assertEqual({path.name for path in self.destination.parent.iterdir()}, {self.destination.name})

    def test_stored_sources_evidence_missing_extra_and_mutation_reject(self):
        self.publish()
        paths = [self.destination / "tree/src/value.rs", self.destination / "tree/LICENSE",
                 self.destination / "tree/patches/input.patch", self.destination / "publisher-capture.json",
                 self.destination / "producer-inputs.json", self.destination / "source-tree.json",
                 self.destination / verifier.ARCHIVE_NAME]
        for path in paths:
            with self.subTest(path=path.relative_to(self.destination)):
                original = path.read_bytes()
                path.write_bytes(original + b"changed")
                with self.assertRaises((ValueError, OSError)):
                    verifier.verify_bundle(self.project, self.destination)
                path.write_bytes(original)
                path.unlink()
                with self.assertRaises((ValueError, OSError)):
                    verifier.verify_bundle(self.project, self.destination)
                path.write_bytes(original)
                path.chmod(0o644)
        for extra in [self.destination / "extra", self.destination / "tree/extra"]:
            extra.write_bytes(b"unapproved")
            with self.assertRaises(ValueError):
                verifier.verify_bundle(self.project, self.destination)
            extra.unlink()
        (self.destination / "tree/empty-extra").mkdir()
        with self.assertRaises(ValueError):
            verifier.verify_bundle(self.project, self.destination)

    def test_stored_tree_is_rechecked_after_association_phase(self):
        self.publish()
        original = verifier.publisher_association
        calls = 0

        def association(*arguments):
            nonlocal calls
            calls += 1
            result = original(*arguments)
            if calls == 2:
                (self.destination / "tree/src/value.rs").write_bytes(b"changed late")
            return result

        with patch.object(verifier, "publisher_association", side_effect=association):
            with self.assertRaises(ValueError):
                verifier.verify_bundle(self.project, self.destination)

    def test_linked_prepared_and_stored_tree_reject(self):
        target = self.project / "link-target"
        target.write_bytes(b"link target")
        link = self.prepared / "publisher-metadata.json"
        original = link.read_bytes()
        link.unlink()
        try:
            link.symlink_to(target)
        except OSError:
            link.write_bytes(original)
            self.skipTest("symlinks unavailable on this host")
        with self.assertRaises(ValueError):
            self.publish()
        link.unlink()
        link.write_bytes(original)
        self.publish()
        source = self.destination / "tree/src/value.rs"
        source.unlink()
        source.symlink_to(target)
        with self.assertRaises(ValueError):
            verifier.verify_bundle(self.project, self.destination)

    def test_linked_capture_and_destination_parent_reject(self):
        target = self.project / "capture-target.json"
        target.write_bytes(self.capture.read_bytes())
        self.capture.unlink()
        try:
            self.capture.symlink_to(target)
        except OSError:
            self.skipTest("symlinks unavailable on this host")
        with self.assertRaises(ValueError):
            self.publish()
        self.assertFalse(self.destination.exists())
        self.capture.unlink()
        self.capture.write_bytes(target.read_bytes())
        parent = self.destination.parent
        parent.rmdir()
        other = self.project / "other-parent"
        other.mkdir()
        parent.symlink_to(other, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.publish()
        self.assertEqual(list(other.iterdir()), [])

    def test_windows_reparse_attribute_is_rejected_without_following(self):
        real = Path.lstat
        from types import SimpleNamespace

        def metadata(path, *arguments, **options):
            result = real(path, *arguments, **options)
            if path == self.prepared:
                return SimpleNamespace(st_mode=result.st_mode, st_file_attributes=1024)
            return result

        with patch.object(Path, "lstat", metadata), \
                patch.object(verifier.local.original.stat, "FILE_ATTRIBUTE_REPARSE_POINT", 1024, create=True):
            with self.assertRaises(ValueError):
                self.publish()
        self.assertFalse(self.destination.exists())

    def test_mode_only_tamper_rejects_on_posix(self):
        if os.name == "nt":
            self.skipTest("Windows filesystem does not represent POSIX executable intent")
        self.publish()
        (self.destination / "tree/scripts/build.js").chmod(0o644)
        with self.assertRaisesRegex(ValueError, "mode"):
            verifier.verify_bundle(self.project, self.destination)

    def test_empty_directory_walk_errors_do_not_pass_exact_inventory(self):
        tree = self.project / "tree-control"
        tree.mkdir()
        (tree / "empty").mkdir()

        def interrupted_walk(path, *, followlinks, onerror=None):
            self.assertFalse(followlinks)
            yield str(path), ["empty"], []
            if onerror is not None:
                onerror(PermissionError("authored unreadable empty-directory control"))

        # Without onerror, both expected sets would match despite failed descent.
        with patch.object(verifier.os, "walk", side_effect=interrupted_walk):
            with self.assertRaisesRegex(ValueError, "traversal failed"):
                verifier._check_tree(tree, {}, {"empty"}, {})


if __name__ == "__main__":
    unittest.main()
