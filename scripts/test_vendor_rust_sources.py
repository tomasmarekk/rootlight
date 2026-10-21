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
        # Initial-import fixtures have no producer tree; additional-lock cases
        # explicitly select a fresh Cargo-only destination instead.
        locks = ("Cargo.lock", "tests/native-grammars/Cargo.lock", "fuzz/Cargo.lock")
        catalogue = patch.object(vendor, "LOCK_PATHS", locks)
        catalogue.start()
        self.addCleanup(catalogue.stop)
        defaults = patch.dict(vendor.vendor_sources.__kwdefaults__, {"lock_paths": locks})
        defaults.start()
        self.addCleanup(defaults.stop)
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
        groups = [(self.identities[0],), tuple(self.identities), (self.identities[1],), tuple(self.identities)]
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

    def _additional_lock(self) -> tuple[str, ...]:
        identity = ("charlie", "3.0.0")
        checksum = self._archive(*identity)
        name = "producer/Cargo.lock"
        path = self.root / name
        path.parent.mkdir()
        path.write_text('version = 4\n[[package]]\nname = "producer-local"\nversion = "0.1.0"\n'
                        '\n[[package]]\nname = "alpha"\nversion = "1.0.0"\n'
                        f'source = "{REGISTRY}"\nchecksum = "{self.checksums[self.identities[0]]}"\n'
                        '\n[[package]]\nname = "charlie"\nversion = "3.0.0"\n'
                        f'source = "{REGISTRY}"\nchecksum = "{checksum}"\n', encoding="utf-8")
        return (*vendor.LOCK_PATHS, name)

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

    def test_explicit_four_lock_union_publishes_only_fresh_cargo_inputs(self) -> None:
        locks = self._additional_lock()
        existing = self.root / "source-inputs"
        existing.mkdir()
        (existing / "other-source").write_bytes(b"preserve unrelated input")
        before = self._snapshot()
        original_archives = {path.name: path.read_bytes() for path in self.archives.iterdir()}
        destination = self.base / "cargo-union"
        empty_home = self.base / "empty-four-lock-cargo-home"
        empty_home.mkdir()
        with patch.dict(os.environ, {"CARGO_HOME": str(empty_home)}), \
                patch("prepare_rust_source_bundle.subprocess.run", side_effect=AssertionError), \
                patch("prepare_rust_source_bundle.subprocess.check_output", side_effect=AssertionError):
            result = vendor.vendor_sources(self.root, self.archives, lock_paths=locks,
                                           destination=destination)
        self.assertEqual(self._snapshot(), before)
        self.assertEqual(list(empty_home.iterdir()), [])
        self.assertEqual({path.name: path.read_bytes() for path in self.archives.iterdir()}, original_archives)
        self.assertEqual({path.name for path in destination.iterdir()}, {"cargo", "crates", vendor.REPORT_NAME})
        self.assertEqual(result["package_count"], 3)
        self.assertEqual(result["local_package_count"], 2)
        self.assertEqual([row["path"] for row in result["lockfiles"]], sorted(locks))
        self.assertEqual(len([row for row in result["packages"] if row["name"] == "alpha"]), 1)
        checked = vendor.verify_bundle(destination / "cargo", [self.root / name for name in locks],
                                       archives=destination / "crates")
        for key, value in checked.items():
            self.assertEqual(result[key], value)
        self.assertEqual(json.loads((destination / vendor.REPORT_NAME).read_bytes()), result)
        self.assertEqual(result["file_modes"]["source-inputs/cargo/charlie-3.0.0/bin/check"], 0o755)
        self.assertFalse(result["frontend_external_source_closure_verified"])
        self.assertFalse(result["build_and_packaging_acceptance_verified"])

    def test_default_lock_union_does_not_silently_include_additional_sources(self) -> None:
        self._additional_lock()
        with self.assertRaisesRegex(vendor.PreparationError, "extra=.*charlie"):
            vendor.vendor_sources(self.root, self.archives)
        self.assertFalse((self.root / "source-inputs").exists())

    def test_explicit_union_report_is_independent_of_lock_list_order(self) -> None:
        locks = self._additional_lock()
        first = vendor.vendor_sources(self.root, self.archives, lock_paths=locks,
                                      destination=self.base / "first-union")
        second = vendor.vendor_sources(self.root, self.archives, lock_paths=tuple(reversed(locks)),
                                       destination=self.base / "second-union")
        self.assertEqual(first, second)
        self.assertEqual((self.base / "first-union" / vendor.REPORT_NAME).read_bytes(),
                         (self.base / "second-union" / vendor.REPORT_NAME).read_bytes())

    def test_additional_lock_conflicts_and_foreign_sources_never_publish(self) -> None:
        locks = self._additional_lock()
        path = self.root / locks[-1]
        original = path.read_text(encoding="utf-8")
        for content, expected in (
            (original.replace(self.checksums[self.identities[0]], "0" * 64), "conflicting locked checksum"),
            (original.replace(REGISTRY, "git+https://example.invalid/repository#" + "a" * 40), "unsupported source"),
        ):
            path.write_text(content, encoding="utf-8")
            destination = self.base / "rejected-additional-lock"
            with self.subTest(expected=expected), self.assertRaisesRegex(vendor.PreparationError, expected):
                vendor.vendor_sources(self.root, self.archives, lock_paths=locks, destination=destination)
            self.assertFalse(destination.exists())
            self.assertFalse(list(self.base.glob(".rust-source-inputs-*")))
        path.write_text(original, encoding="utf-8")

    def test_explicit_lock_lists_reject_missing_root_duplicates_and_unsafe_paths(self) -> None:
        locks = self._additional_lock()
        for selected in ((), (locks[-1],), (*locks, locks[-1]), (*locks, "../outside"),
                         (*locks, "/outside"), (*locks, "producer\\Cargo.lock"),
                         (*locks, "producer/absent.lock")):
            destination = self.base / "invalid-lock-output"
            before = self._snapshot()
            with self.subTest(selected=selected), self.assertRaises(vendor.PreparationError):
                vendor.vendor_sources(self.root, self.archives, lock_paths=selected,
                                      destination=destination)
            self.assertFalse(destination.exists())
            self.assertEqual(self._snapshot(), before)
        # Identity inspection alone can inspect a nested lock without relabeling
        # a published verifier report as the project's root Cargo.lock.
        self.assertEqual(len(vendor.registry_inputs(self.root, lock_paths=(locks[-1],))), 2)

    def test_explicit_output_missing_or_tampered_additional_archive_never_publishes(self) -> None:
        locks = self._additional_lock()
        source = self.archives / "charlie-3.0.0.crate"
        original = source.read_bytes()
        for state in ("missing", "tampered"):
            if state == "missing":
                source.unlink()
            else:
                source.write_bytes(original + b"changed")
            destination = self.base / state
            before = self._snapshot()
            with self.subTest(state=state), self.assertRaises(vendor.PreparationError):
                vendor.vendor_sources(self.root, self.archives, lock_paths=locks, destination=destination)
            self.assertFalse(destination.exists())
            self.assertEqual(self._snapshot(), before)

    def test_explicit_destination_controls_preserve_inputs_and_existing_output(self) -> None:
        existing = self.base / "existing-output"
        existing.mkdir()
        (existing / "keep").write_bytes(b"existing output")
        file = self.base / "existing-file"
        file.write_bytes(b"existing file")
        for destination in (existing, file, self.base / "absent-parent/output",
                            self.base / ".." / self.base.name / "escaped",
                            self.archives / "new-output"):
            before = self._snapshot()
            with self.subTest(destination=destination), self.assertRaises(vendor.PreparationError):
                vendor.vendor_sources(self.root, self.archives, destination=destination)
            self.assertEqual(self._snapshot(), before)
        self.assertEqual((existing / "keep").read_bytes(), b"existing output")
        self.assertEqual(file.read_bytes(), b"existing file")

    def test_linked_explicit_output_parent_is_rejected_before_staging(self) -> None:
        parent = self.base / "output-parent"
        parent.mkdir()
        original = vendor.linked_path
        with patch.object(vendor, "linked_path", side_effect=lambda path: path == parent or original(path)):
            with self.assertRaisesRegex(vendor.PreparationError, "non-link directory"):
                vendor.vendor_sources(self.root, self.archives, destination=parent / "fresh")
        self.assertEqual(list(parent.iterdir()), [])

    def test_additional_lock_mutation_and_archive_mutation_fail_before_explicit_publication(self) -> None:
        locks = self._additional_lock()
        original = vendor.verify_bundle
        lock = self.root / locks[-1]
        archive = self.archives / "charlie-3.0.0.crate"
        for changed in (lock, archive):
            payload = changed.read_bytes()
            destination = self.base / ("lock-change" if changed == lock else "archive-change")

            def mutate_after_verification(sources, paths, *, archives):
                result = original(sources, paths, archives=archives)
                changed.write_bytes(payload + b"\n")
                return result

            with self.subTest(changed=changed), patch.object(vendor, "verify_bundle", side_effect=mutate_after_verification):
                with self.assertRaisesRegex(vendor.PreparationError, "changed during preparation"):
                    vendor.vendor_sources(self.root, self.archives, lock_paths=locks, destination=destination)
            self.assertFalse(destination.exists())
            self.assertFalse(list(self.base.glob(".rust-source-inputs-*")))
            changed.write_bytes(payload)

    def test_cli_explicit_locks_and_fresh_destination_preserve_default_inputs(self) -> None:
        locks = self._additional_lock()
        existing = self.root / "source-inputs"
        existing.mkdir()
        destination = self.base / "cli-output"
        arguments = ["--root", str(self.root), "--archives", str(self.archives),
                     "--destination", str(destination)]
        for name in locks:
            arguments.extend(("--lock", name))
        with contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(vendor.main(arguments), 0)
        self.assertEqual(json.loads(output.getvalue())["package_count"], 3)
        self.assertEqual(list(existing.iterdir()), [])
        self.assertEqual(len(json.loads((destination / vendor.REPORT_NAME).read_bytes())["lockfiles"]), 4)

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
