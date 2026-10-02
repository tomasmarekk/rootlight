#!/usr/bin/env python3
"""Prove local Rust source import with synthetic archives and independent locks.

The fixtures exercise orchestration around the existing verifier/materializer;
no Cargo, network, dependency execution, or real repository inputs are used.
"""

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from prepare_rust_source_bundle import REGISTRY
import vendor_rust_sources as vendor


class VendorRustSourcesTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "project"
        self.root.mkdir()
        self.archives = self.base / "archives"
        self.archives.mkdir()
        self.identities = [("alpha", "1.0.0"), ("bravo", "2.0.0")]
        self.members = {
            "src/lib.rs": (b"pub const VALUE: u8 = 1;\r\n", 0o644),
            "bin/check": (b"#!/bin/sh\nexit 0\n", 0o755),
            "LICENSE": (b"synthetic license fixture\r\n", 0o644),
        }
        self.checksums = {identity: self._archive(*identity) for identity in self.identities}
        self._locks(self.root)

    def _archive(self, name: str, version: str, extra: tarfile.TarInfo | None = None) -> str:
        path = self.archives / f"{name}-{version}.crate"
        members = {"Cargo.toml": (f'[package]\nname = "{name}"\nversion = "{version}"\n'
                                   'license = "MIT"\n'.encode(), 0o644), **self.members}
        with tarfile.open(path, "w:gz") as archive:
            for relative, (contents, mode) in members.items():
                member = tarfile.TarInfo(f"{name}-{version}/{relative}")
                member.size, member.mode = len(contents), mode
                archive.addfile(member, io.BytesIO(contents))
            if extra is not None:
                archive.addfile(extra)
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def _locks(self, root: Path) -> None:
        groups = [(self.identities[0],), tuple(self.identities), (self.identities[1],)]
        for relative, identities in zip(vendor.LOCK_PATHS, groups):
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            contents = 'version = 4\n\n[[package]]\nname = "local-fixture"\nversion = "0.1.0"\n'
            for name, version in identities:
                contents += (f'\n[[package]]\nname = "{name}"\nversion = "{version}"\n'
                             f'source = "{REGISTRY}"\n'
                             f'checksum = "{self.checksums[(name, version)]}"\n')
            path.write_text(contents, encoding="utf-8", newline="\n")

    def _snapshot(self) -> dict[str, bytes | None]:
        return {path.relative_to(self.root).as_posix(): path.read_bytes() if path.is_file() else None
                for path in self.root.rglob("*")}

    def test_imports_the_exact_lock_union_original_bytes_and_verified_evidence(self) -> None:
        before = {name: (self.root / name).read_bytes() for name in vendor.LOCK_PATHS}
        unrelated_home = self.base / "empty-cargo-home"
        with patch.dict(os.environ, {"CARGO_HOME": str(unrelated_home)}), \
                patch("prepare_rust_source_bundle.subprocess.run", side_effect=AssertionError), \
                patch("prepare_rust_source_bundle.subprocess.check_output", side_effect=AssertionError):
            result = vendor.vendor_sources(self.root, self.archives)
        self.assertFalse(unrelated_home.exists())
        self.assertEqual(result["package_count"], 2)
        self.assertEqual(result["local_package_count"], 1)
        self.assertEqual([record["path"] for record in result["lockfiles"]], sorted(vendor.LOCK_PATHS))
        self.assertTrue(result["archives_verified"])
        self.assertFalse(result["frontend_external_source_closure_verified"])
        self.assertFalse(result["build_and_packaging_acceptance_verified"])
        retained = self.root / vendor.ARCHIVES_PATH
        sources = self.root / vendor.VENDOR_PATH
        independently_verified = vendor.verify_bundle(
            sources, [self.root / name for name in vendor.LOCK_PATHS], archives=retained,
        )
        for key, value in independently_verified.items():
            self.assertEqual(result[key], value)
        for name, version in self.identities:
            filename = f"{name}-{version}.crate"
            self.assertEqual((retained / filename).read_bytes(), (self.archives / filename).read_bytes())
            for relative, (contents, _mode) in self.members.items():
                self.assertEqual((sources / f"{name}-{version}" / relative).read_bytes(), contents)
        report = self.root / "source-inputs" / vendor.REPORT_NAME
        self.assertEqual(json.loads(report.read_bytes()), result)
        self.assertNotIn(str(self.base), report.read_text(encoding="utf-8"))
        for name, contents in before.items():
            self.assertEqual((self.root / name).read_bytes(), contents)

    def test_retains_archive_executable_modes_independently_of_host_filesystem(self) -> None:
        result = vendor.vendor_sources(self.root, self.archives)
        for package in result["packages"]:
            prefix = f'{vendor.VENDOR_PATH}/{package["vendor_path"]}'
            self.assertEqual(result["file_modes"][f"{prefix}/bin/check"], 0o755)
            self.assertEqual(result["file_modes"][f"{prefix}/src/lib.rs"], 0o644)
            self.assertEqual(result["file_modes"][f"{prefix}/.cargo-checksum.json"], 0o644)
            self.assertEqual(package["archive_executable_files"], ["bin/check"])
            record = next(record for record in package["files"] if record["path"] == "bin/check")
            self.assertEqual(record["archive_mode"], 0o755)
            if os.name != "nt":
                self.assertTrue((self.root / prefix / "bin/check").stat().st_mode & 0o111)

    def test_dependency_report_is_deterministic_across_project_locations(self) -> None:
        first = vendor.vendor_sources(self.root, self.archives)
        second_root = self.base / "other-project"
        second_root.mkdir()
        self._locks(second_root)
        second = vendor.vendor_sources(second_root, self.archives)
        self.assertEqual(first, second)
        self.assertEqual((self.root / "source-inputs" / vendor.REPORT_NAME).read_bytes(),
                         (second_root / "source-inputs" / vendor.REPORT_NAME).read_bytes())

    def test_missing_archive_leaves_project_unchanged(self) -> None:
        before = self._snapshot()
        (self.archives / "bravo-2.0.0.crate").unlink()
        with self.assertRaisesRegex(vendor.PreparationError, "missing=.*bravo"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_tampered_archive_leaves_project_unchanged(self) -> None:
        before = self._snapshot()
        path = self.archives / "bravo-2.0.0.crate"
        path.write_bytes(path.read_bytes() + b"tampered")
        with self.assertRaisesRegex(vendor.PreparationError, "checksum.*bravo"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_extra_archive_is_not_silently_imported_or_ignored(self) -> None:
        before = self._snapshot()
        (self.archives / "unexpected.crate").write_bytes(b"unused")
        with self.assertRaisesRegex(vendor.PreparationError, "extra=.*unexpected"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_conflicting_lock_checksums_leave_project_unchanged(self) -> None:
        path = self.root / vendor.LOCK_PATHS[1]
        path.write_text(path.read_text(encoding="utf-8").replace(
            self.checksums[self.identities[0]], "0" * 64,
        ), encoding="utf-8", newline="\n")
        before = self._snapshot()
        with self.assertRaisesRegex(vendor.PreparationError, "conflicting locked checksum"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_lock_authenticated_malformed_archive_cleans_staging_and_normalizes_failure(self) -> None:
        path = self.archives / "bravo-2.0.0.crate"
        path.write_bytes(b"not a gzip tar archive")
        self.checksums[self.identities[1]] = hashlib.sha256(path.read_bytes()).hexdigest()
        self._locks(self.root)
        before = self._snapshot()
        with self.assertRaisesRegex(vendor.PreparationError, "not a gzip file"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_unsafe_archive_cleans_staging_without_publishing_a_partial_union(self) -> None:
        member = tarfile.TarInfo("bravo-2.0.0/../escaped")
        member.type = tarfile.SYMTYPE
        member.linkname = "outside"
        self.checksums[self.identities[1]] = self._archive(*self.identities[1], extra=member)
        self._locks(self.root)
        before = self._snapshot()
        with self.assertRaisesRegex(vendor.PreparationError, "unsafe source path"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)
        self.assertFalse((self.base / "escaped").exists())

    def test_final_verifier_failure_cleans_staging_without_publishing(self) -> None:
        before = self._snapshot()
        original = vendor.verify_bundle

        def reject_modified_source(sources, locks, *, archives):
            (sources / "bravo-2.0.0/src/lib.rs").write_bytes(b"changed after extraction")
            return original(sources, locks, archives=archives)

        with patch.object(vendor, "verify_bundle", side_effect=reject_modified_source):
            with self.assertRaisesRegex(vendor.PreparationError, "source file checksum differs"):
                vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_lockfile_change_during_verification_does_not_publish_stale_evidence(self) -> None:
        original = vendor.verify_bundle

        def change_lock_after_verification(sources, locks, *, archives):
            result = original(sources, locks, archives=archives)
            path = self.root / vendor.LOCK_PATHS[-1]
            path.write_bytes(path.read_bytes() + b"\n")
            return result

        with patch.object(vendor, "verify_bundle", side_effect=change_lock_after_verification):
            with self.assertRaisesRegex(vendor.PreparationError, "lockfiles changed"):
                vendor.vendor_sources(self.root, self.archives)
        self.assertFalse((self.root / "source-inputs").exists())
        self.assertFalse(list(self.root.glob(".rust-source-inputs-*")))

    def test_destination_appearing_before_publication_is_preserved(self) -> None:
        original = vendor.verify_bundle
        destination = self.root / "source-inputs"

        def create_destination_after_verification(sources, locks, *, archives):
            result = original(sources, locks, archives=archives)
            destination.mkdir()
            (destination / "other-input").write_bytes(b"independent input")
            return result

        with patch.object(vendor, "verify_bundle", side_effect=create_destination_after_verification):
            with self.assertRaisesRegex(vendor.PreparationError, "already exists"):
                vendor.vendor_sources(self.root, self.archives)
        self.assertEqual((destination / "other-input").read_bytes(), b"independent input")
        self.assertEqual(list(destination.iterdir()), [destination / "other-input"])
        self.assertFalse(list(self.root.glob(".rust-source-inputs-*")))

    def test_preexisting_destination_is_preserved(self) -> None:
        destination = self.root / "source-inputs"
        destination.mkdir()
        (destination / "retained").write_bytes(b"existing input")
        before = self._snapshot()
        with self.assertRaisesRegex(vendor.PreparationError, "already exists"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_preexisting_empty_directory_and_regular_file_are_rejected(self) -> None:
        for directory in (False, True):
            with self.subTest(directory=directory), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                self._locks(root)
                destination = root / "source-inputs"
                if directory:
                    destination.mkdir()
                else:
                    destination.write_bytes(b"existing file")
                with self.assertRaisesRegex(vendor.PreparationError, "already exists"):
                    vendor.vendor_sources(root, self.archives)
                self.assertEqual(destination.is_dir(), directory)
                if not directory:
                    self.assertEqual(destination.read_bytes(), b"existing file")

    def test_reparse_or_linked_source_roots_are_rejected_before_writes(self) -> None:
        before = self._snapshot()
        original = vendor.linked_path
        for linked in (self.root, self.archives):
            with self.subTest(linked=linked), patch.object(
                vendor, "linked_path", side_effect=lambda path: path == linked or original(path),
            ):
                with self.assertRaisesRegex(vendor.PreparationError, "non-link directory"):
                    vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_symlinked_archive_is_rejected(self) -> None:
        path = self.archives / "alpha-1.0.0.crate"
        original = self.base / "original.crate"
        shutil.copyfile(path, original)
        path.unlink()
        try:
            path.symlink_to(original)
        except OSError as error:
            self.skipTest(f"filesystem cannot create symlinks: {error}")
        before = self._snapshot()
        with self.assertRaisesRegex(vendor.PreparationError, "linked source"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertEqual(self._snapshot(), before)

    def test_invalid_roots_and_parent_traversal_do_not_create_destination(self) -> None:
        for root in (self.base / "absent", self.root / "Cargo.lock", self.root / ".." / "project"):
            with self.subTest(root=root), self.assertRaises(vendor.PreparationError):
                vendor.vendor_sources(root, self.archives)
        self.assertFalse((self.root / "source-inputs").exists())

    def test_cli_rejects_missing_input_without_stdout_or_destination(self) -> None:
        (self.archives / "alpha-1.0.0.crate").unlink()
        output, errors = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            with self.assertRaises(SystemExit) as failure:
                vendor.main(["--root", str(self.root), "--archives", str(self.archives)])
        self.assertEqual(failure.exception.code, 1)
        self.assertEqual(output.getvalue(), "")
        self.assertIn("Rust source vendoring failed", errors.getvalue())
        self.assertFalse((self.root / "source-inputs").exists())

    def test_cli_emits_only_a_summary_after_verified_publication(self) -> None:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            status = vendor.main(["--root", str(self.root), "--archives", str(self.archives)])
        self.assertEqual(status, 0)
        self.assertEqual(json.loads(output.getvalue()), {
            "schema": "rootlight.cargo-source-bundle/1", "package_count": 2,
            "status": "verified", "frontend_external_source_closure_verified": False,
        })


if __name__ == "__main__":
    unittest.main()
