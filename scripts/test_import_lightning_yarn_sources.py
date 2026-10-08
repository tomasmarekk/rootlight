"""Authored local Yarn publication controls without dependency execution.

Only the independently tested authored-producer boundary is mocked; lock parsing,
archive authentication, member verification and fresh publication remain real.
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
import tempfile
import unittest
from unittest.mock import patch

import import_lightning_yarn_sources as importer
from test_verify_lightning_yarn_sources import archive


def snapshot(root: Path) -> tuple[dict[str, bytes], set[str]]:
    """Capture exact fixture bytes and directories without applying ignore rules."""
    return ({path.relative_to(root).as_posix(): path.read_bytes()
             for path in root.rglob("*") if path.is_file()},
            {path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_dir()})


class YarnImportTests(unittest.TestCase):
    """Require canonical publication and reject late mutation or namespace escape."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.project = Path(self.temporary.name)
        self.prepared = self.project / "prepared-yarn"
        (self.prepared / "archives").mkdir(parents=True)
        (self.prepared / "packages").mkdir()
        self.destination = self.project / importer.verifier.BUNDLE_PATH
        self.destination.parent.mkdir(parents=True)
        self.tree = self.project / importer.verifier.PRODUCER_PATH / "tree"
        self.tree.mkdir(parents=True)
        self.manifest = {"name": "fixture-producer", "version": "1.0.0",
                         "dependencies": {"leaf": "^1.0.0"},
                         "optionalDependencies": {"second": "^1.0.0"},
                         "resolutions": {"fixture-producer": "link:."}}
        sections = ["# yarn lockfile v1\n"]
        self.archives = []
        for name in ("leaf", "second"):
            payload, members = archive(name, root="publication", extras={
                "publication/vendor/.DS_Store": (b"authored inert published member\n", 0o644),
                "publication/.gitignore": (b"vendor/\n", 0o644),
            })
            resolved = (f"https://registry.yarnpkg.com/{name}/-/{name}-1.0.0.tgz#"
                        + hashlib.sha1(payload).hexdigest())
            integrity = "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii")
            sections.append(f'{name}@^1.0.0:\n  version "1.0.0"\n  resolved "{resolved}"\n'
                            f'  integrity {integrity}\n\n')
            filename = hashlib.sha256(resolved.encode("ascii")).hexdigest() + ".tgz"
            source = self.prepared / "archives" / filename
            source.write_bytes(payload)
            source.chmod(0o644)
            self.archives.append(source)
            expanded = self.prepared / "packages" / Path(filename).stem
            (expanded / "publication/nested/empty").mkdir(parents=True)
            for relative, (contents, mode) in members.items():
                target = expanded / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(contents)
                target.chmod(mode)
        sections.append('"fixture-producer@link:.":\n  version "0.0.0"\n  uid ""\n')
        (self.tree / "package.json").write_bytes(json.dumps(self.manifest).encode())
        (self.tree / "yarn.lock").write_bytes("".join(sections).encode("ascii"))
        self.input_patch = patch.object(importer.verifier, "_inputs", side_effect=self.inputs)
        self.input_patch.start()
        self.addCleanup(self.input_patch.stop)
        self.evidence = importer.verifier.derive_bundle(self.project, self.prepared)
        (self.prepared / importer.verifier.REPORT_NAME).write_bytes(
            importer.verifier.producer.json_bytes(self.evidence))
        (self.prepared / importer.verifier.REPORT_NAME).chmod(0o644)

    def inputs(self, project: Path) -> tuple[dict, bytes, bytes, bytes]:
        """Mock only producer provenance while retaining real guarded lock reads."""
        self.assertEqual(project, self.project)
        verifier = importer.verifier
        return ({"association": {"name": "fixture-producer", "version": "1.0.0"}},
                verifier.producer.read_bytes(self.tree / "package.json", verifier.yarn.MAX_MANIFEST_BYTES),
                verifier.producer.read_bytes(self.tree / "yarn.lock", verifier.yarn.MAX_LOCK_BYTES),
                b'{"fixture":"separately verified authored producer"}\n')

    def assert_unpublished(self) -> None:
        """Require absent output and cleaned sibling staging after rejection."""
        self.assertFalse(os.path.lexists(self.destination))
        self.assertEqual(list(self.destination.parent.iterdir()), [])

    def test_exact_originals_members_report_licenses_and_modes_are_retained(self) -> None:
        before = snapshot(self.prepared)
        producer_before = snapshot(self.tree)
        evidence = importer.import_bundle(self.project, self.prepared, self.destination)
        self.assertEqual(evidence, self.evidence)
        self.assertEqual(snapshot(self.destination), before)
        self.assertEqual(snapshot(self.prepared), before)
        self.assertEqual(snapshot(self.tree), producer_before)
        self.assertEqual(importer.verifier.verify_bundle(self.project), evidence)
        self.assertEqual(evidence["archive_count"], 2)
        self.assertEqual(evidence["pin_graph"]["stanza_count"], 3)
        self.assertEqual(evidence["pin_graph"]["local_associations"][0]["resolution"], "link:.")
        for record in evidence["packages"]:
            self.assertEqual(record["published_package"]["license_metadata"], {"license": "MIT"})
            self.assertEqual(record["published_package"]["provenance_metadata"], {
                "repository": "https://example.invalid/source",
            })
            self.assertEqual(len(record["published_package"]["license_files"]), 1)
            self.assertEqual(len(record["published_package"]["native_binary_files"]), 1)
            executable = self.destination / record["expanded_path"] / "publication/bin/task.js"
            self.assertEqual(evidence["files"][executable.relative_to(self.destination).as_posix()]["mode"], 0o755)
            if os.name != "nt":
                self.assertEqual(stat.S_IMODE(executable.stat().st_mode), 0o755)
        self.assertTrue(all(evidence[key] is False for key in importer.verifier.FALSE_CLAIMS))

    def test_missing_optional_archive_extra_member_and_noncanonical_report_reject_before_copy(self) -> None:
        for mutation in ("missing", "extra", "report"):
            with self.subTest(mutation=mutation):
                path = self.archives[-1] if mutation == "missing" else self.prepared / (
                    "packages/extra" if mutation == "extra" else importer.verifier.REPORT_NAME)
                original = path.read_bytes() if path.exists() else None
                if mutation == "missing":
                    path.unlink()
                else:
                    path.write_bytes(b"unapproved bytes")
                with patch.object(importer, "_copy_inventory", side_effect=AssertionError("must prevalidate")), \
                     self.assertRaises((ValueError, OSError)):
                    importer.import_bundle(self.project, self.prepared, self.destination)
                self.assert_unpublished()
                if original is None:
                    path.unlink()
                else:
                    path.write_bytes(original)

    def test_sri_failure_precedes_decompression_and_publication(self) -> None:
        self.archives[0].write_bytes(b"unauthenticated original")
        with patch.object(importer.verifier, "_bounded_members", side_effect=AssertionError("must authenticate")), \
             self.assertRaises(ValueError):
            importer.import_bundle(self.project, self.prepared, self.destination)
        self.assert_unpublished()

    def test_existing_empty_directory_file_and_sentinel_are_preserved(self) -> None:
        before = snapshot(self.prepared)
        for kind in ("empty", "file", "sentinel"):
            with self.subTest(kind=kind):
                if kind == "file":
                    self.destination.write_bytes(b"existing file")
                else:
                    self.destination.mkdir()
                    if kind == "sentinel":
                        (self.destination / "keep").write_bytes(b"existing directory")
                with self.assertRaisesRegex(importer.YarnImportError, "already exists"):
                    importer.import_bundle(self.project, self.prepared, self.destination)
                if kind == "file":
                    self.assertEqual(self.destination.read_bytes(), b"existing file")
                    self.destination.unlink()
                else:
                    if kind == "sentinel":
                        self.assertEqual((self.destination / "keep").read_bytes(), b"existing directory")
                        (self.destination / "keep").unlink()
                    self.destination.rmdir()
        self.assertEqual(snapshot(self.prepared), before)

    def test_fixed_namespace_traversal_and_absent_parent_reject(self) -> None:
        for target in (self.project / "elsewhere", self.destination.parent / ".." / "escape",
                       self.prepared / "yarn-originals"):
            with self.subTest(target=target), self.assertRaises(importer.YarnImportError):
                importer.import_bundle(self.project, self.prepared, target)
            self.assertFalse(target.exists())
        self.destination.parent.rmdir()
        with self.assertRaises((ValueError, OSError)):
            importer.import_bundle(self.project, self.prepared, self.destination)
        self.assertFalse(self.destination.parent.exists())

    def test_source_or_lock_mutation_after_copy_prevents_publication(self) -> None:
        copy = importer._copy_inventory
        for target in (self.archives[0], self.tree / "yarn.lock"):
            original = target.read_bytes()
            def copy_then_change(*arguments):
                copy(*arguments)
                target.write_bytes(b"changed after copy")
            with self.subTest(target=target), patch.object(importer, "_copy_inventory", side_effect=copy_then_change), \
                 self.assertRaises((ValueError, OSError)):
                importer.import_bundle(self.project, self.prepared, self.destination)
            self.assert_unpublished()
            target.write_bytes(original)

    def test_copied_byte_mutation_and_late_io_failure_leave_no_output(self) -> None:
        copy = importer._copy_inventory
        def copy_then_tamper(prepared, staged, evidence):
            copy(prepared, staged, evidence)
            next((staged / "archives").iterdir()).write_bytes(b"changed copied bytes")
        with patch.object(importer, "_copy_inventory", side_effect=copy_then_tamper), self.assertRaises(ValueError):
            importer.import_bundle(self.project, self.prepared, self.destination)
        self.assert_unpublished()
        with patch.object(importer.os, "rename", side_effect=OSError("authored late I/O failure")), \
             self.assertRaises(OSError):
            importer.import_bundle(self.project, self.prepared, self.destination)
        self.assert_unpublished()

    def test_destination_appearing_before_final_absence_check_is_preserved(self) -> None:
        verify = importer.verifier.verify_bundle
        def verify_then_create(project, bundle):
            evidence = verify(project, bundle)
            if bundle == self.prepared and list(self.destination.parent.glob(".yarn-source-inputs-*")):
                self.destination.mkdir()
                (self.destination / "keep").write_bytes(b"concurrent destination")
            return evidence
        with patch.object(importer.verifier, "verify_bundle", side_effect=verify_then_create), \
             self.assertRaisesRegex(importer.YarnImportError, "already exists"):
            importer.import_bundle(self.project, self.prepared, self.destination)
        self.assertEqual((self.destination / "keep").read_bytes(), b"concurrent destination")
        self.assertEqual({path.name for path in self.destination.parent.iterdir()}, {self.destination.name})

    def test_link_and_reparse_inputs_or_output_parent_are_rejected(self) -> None:
        lstat = Path.lstat
        class LinkedStat:
            def __init__(self, metadata, reparse):
                self.st_mode = metadata.st_mode if reparse else stat.S_IFLNK
                self.st_file_attributes = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 1024)
        for target in (self.prepared, self.destination.parent, self.archives[0]):
            for reparse in (False, True):
                def linked(path):
                    metadata = lstat(path)
                    return LinkedStat(metadata, reparse) if path == target else metadata
                with self.subTest(target=target, reparse=reparse), \
                     patch.object(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 1024, create=True), \
                     patch.object(Path, "lstat", linked), \
                     self.assertRaises((ValueError, OSError)):
                    importer.import_bundle(self.project, self.prepared, self.destination)
                self.assert_unpublished()

    def test_authored_producer_verifier_is_not_bypassed_in_production(self) -> None:
        self.input_patch.stop()
        with patch.object(importer.verifier.producer, "verify_bundle", side_effect=ValueError("authored tree changed")) as proof, \
             self.assertRaisesRegex(ValueError, "authored tree changed"):
            importer.import_bundle(self.project, self.prepared, self.destination)
        proof.assert_called_once_with(self.project, self.project / importer.verifier.PRODUCER_PATH)
        self.input_patch.start()
        self.assert_unpublished()

    def test_cli_counts_keep_every_acceptance_claim_false_and_failure_source_free(self) -> None:
        arguments = ["--project", str(self.project), "--prepared", str(self.prepared),
                     "--destination", str(self.destination)]
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(importer.main(arguments), 0)
        summary = json.loads(output.getvalue())
        self.assertEqual((summary["archive_count"], summary["member_count"]), (2, 14))
        self.assertTrue(summary["original_archives_verified"])
        self.assertTrue(all(summary[key] is False for key in importer.verifier.FALSE_CLAIMS))
        error = io.StringIO()
        with redirect_stderr(error):
            self.assertEqual(importer.main(arguments), 1)
        self.assertEqual(error.getvalue(), "producer Yarn source import failed; no acceptance claim\n")


if __name__ == "__main__":
    unittest.main()
