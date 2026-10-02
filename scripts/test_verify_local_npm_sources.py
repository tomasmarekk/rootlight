#!/usr/bin/env python3
"""Exercise local npm lock projection using tiny authored archive fixtures.

No npm, registry, or product process runs. Fixture provenance comes from the
existing approved patcher rather than a reduced mock of its preservation schema.
"""

from __future__ import annotations

import base64
import contextlib
import copy
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock

import patch_npm_source_fixture as fixture
import verify_local_npm_sources as local
import verify_npm_source_bundle as original


def _json_bytes(document: object) -> bytes:
    return (json.dumps(document, sort_keys=True, indent=2) + "\n").encode("utf-8")


def _sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _sri(payload: bytes) -> str:
    return "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii")


def _archive(name: str, version: str, *, fixture_data: bytes = b"prefix original fixture suffix") -> bytes:
    members = {
        "authored/package.json": _json_bytes({"name": name, "version": version, "license": "MIT", "repository": "https://example.invalid/authored"}),
        "authored/LICENSE": b"Authored test license\n",
        "authored/index.js": b"export const value = 1;\n",
        "authored/tests/string.test.js": fixture_data,
        "authored/runtime.wasm": b"\x00asm\x01\x00\x00\x00",
    }
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:", format=tarfile.PAX_FORMAT) as archive:
        root = tarfile.TarInfo("authored")
        root.type, root.mode, root.mtime = tarfile.DIRTYPE, 0o755, 100
        archive.addfile(root)
        for path, payload in sorted(members.items()):
            member = tarfile.TarInfo(path)
            member.size, member.mode, member.mtime = len(payload), 0o644, 100
            member.uid, member.gid, member.uname, member.gname = 1, 2, "builder", "build"
            if "/tests/" in path:
                member.pax_headers = {"size": str(len(payload)), "comment": "authored fixture metadata"}
            archive.addfile(member, io.BytesIO(payload))
    return gzip.compress(output.getvalue(), mtime=0)


class LocalSourcesTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name)
        self.root = self.parent / "project"
        self.root.mkdir()
        self.original_dir = self.root / local.ORIGINAL_ARCHIVES
        self.local_dir = self.root / local.LOCAL_DISTRIBUTIONS
        self.original_dir.mkdir(parents=True)
        self.local_dir.mkdir()
        self.upstream_path = self.root / local.UPSTREAM_LOCK
        self.canonical_path = self.root / local.CANONICAL_LOCK
        self.canonical_path.parent.mkdir(parents=True)
        self.manifest_path = self.root / local.DISTRIBUTIONS_MANIFEST
        self.first_key = "node_modules/toolkit"
        self.nested_key = "node_modules/container/node_modules/toolkit"
        self.second_key = "node_modules/@scope/platform"
        self.original_payload = _archive("toolkit", "1.2.3")
        self.second_payload = _archive("@scope/platform", "2.0.0")
        first = {"version": "1.2.3", "resolved": "https://registry.npmjs.org/toolkit/-/toolkit-1.2.3.tgz", "integrity": _sri(self.original_payload), "dev": True, "license": "MIT"}
        second = {"version": "2.0.0", "resolved": "https://registry.npmjs.org/@scope/platform/-/platform-2.0.0.tgz", "integrity": _sri(self.second_payload), "optional": True, "os": ["linux"], "cpu": ["x64"], "hasInstallScript": True}
        self.upstream = {
            "name": "authored-frontend", "version": "1.0.0", "lockfileVersion": 3, "requires": True,
            "packages": {"": {"name": "authored-frontend", "version": "1.0.0", "dependencies": {"toolkit": "1.2.3", "@scope/platform": "2.0.0"}},
                         self.first_key: first, self.nested_key: {**first, "optional": True}, self.second_key: second},
        }
        self.upstream_path.write_bytes(_json_bytes(self.upstream))
        prepared = self.parent / "prepared-originals"
        prepared.mkdir()
        self.first_filename = original.archive_filename(first["resolved"])
        self.second_filename = original.archive_filename(second["resolved"])
        (prepared / self.first_filename).write_bytes(self.original_payload)
        (prepared / self.second_filename).write_bytes(self.second_payload)
        (self.original_dir / self.second_filename).write_bytes(self.second_payload)
        output = self.parent / "patched"
        output.mkdir()
        before_member = b"prefix original fixture suffix"
        self.provenance = fixture.patch_fixture_archive(
            self.upstream_path, prepared, "toolkit", "1.2.3", "authored/tests/string.test.js",
            _sha256(before_member), len(b"original fixture"), _sha256(b"original fixture"),
            b"replacement fixture", output, synthetic_replacement_confirmed=True, offset=len(b"prefix "),
        )
        self.archive_path = local.LOCAL_DISTRIBUTIONS + "/toolkit-fixture.tgz"
        self.provenance_path = local.LOCAL_DISTRIBUTIONS + "/toolkit-provenance.json"
        self.patched_payload = (output / "patched.tgz").read_bytes()
        (self.root / self.archive_path).write_bytes(self.patched_payload)
        (self.root / self.provenance_path).write_bytes((output / "patch-provenance.json").read_bytes())
        self.variant = {"name": "toolkit", "version": "1.2.3", "archive_path": self.archive_path,
                        "archive_sha256": _sha256(self.patched_payload), "provenance_path": self.provenance_path,
                        "provenance_sha256": _sha256((self.root / self.provenance_path).read_bytes())}
        self.manifest = {"schema": "rootlight.npm-local-distributions/1", "upstream_lock_sha256": _sha256(self.upstream_path.read_bytes()), "variants": [self.variant]}
        self.canonical = copy.deepcopy(self.upstream)
        for path, package in self.canonical["packages"].items():
            if path:
                archive_path = self.archive_path if path in (self.first_key, self.nested_key) else local.ORIGINAL_ARCHIVES + "/" + self.second_filename
                package["resolved"] = "file:../../../" + archive_path
                if path in (self.first_key, self.nested_key):
                    package["integrity"] = _sri(self.patched_payload)
        self._write_inputs()

    def _write_inputs(self) -> None:
        self.manifest_path.write_bytes(_json_bytes(self.manifest))
        self.canonical_path.write_bytes(_json_bytes(self.canonical))

    def _write_provenance(self) -> None:
        payload = _json_bytes(self.provenance)
        (self.root / self.provenance_path).write_bytes(payload)
        self.variant["provenance_sha256"] = _sha256(payload)
        self._write_inputs()

    def _verify(self) -> dict:
        return local.verify_local_bundle(self.root)

    def _rejected(self, pattern: str = ".*") -> None:
        with self.assertRaisesRegex(local.LocalBundleVerificationError, pattern):
            self._verify()

    def test_complete_projection_retains_nested_optional_and_current_evidence(self) -> None:
        report = self._verify()
        self.assertEqual(report, self._verify())
        self.assertEqual(report["schema"], "rootlight.npm-local-archive-bundle/1")
        self.assertEqual((report["archive_count"], report["lock_record_count"], report["original_archive_count"], report["local_distribution_count"]), (2, 3, 1, 1))
        package = next(item for item in report["packages"] if item["name"] == "toolkit")
        self.assertEqual(package["lock_paths"], sorted([self.first_key, self.nested_key]))
        self.assertEqual(package["archive_path"], self.archive_path)
        self.assertEqual(package["archive_filename"], "toolkit-fixture.tgz")
        self.assertEqual(package["resolved"], "file:../../../" + self.archive_path)
        self.assertEqual(package["integrity"], _sri(self.patched_payload))
        self.assertEqual(package["distribution_kind"], "locally_modified_distribution")
        self.assertFalse(package["upstream_original_sri_recomputed"])
        self.assertTrue(package["original_sri_verified_during_preparation"])
        self.assertEqual(package["upstream_pin"], self.provenance["original_pin"])
        original_package = next(item for item in report["packages"] if item["name"] == "@scope/platform")
        self.assertTrue(original_package["upstream_original_sri_recomputed"])
        self.assertEqual(original_package["distribution_kind"], "original_registry_archive")
        self.assertEqual(original_package["lock_records"][0]["os"], ["linux"])
        self.assertTrue(original_package["lock_records"][0]["hasInstallScript"])
        expected_inputs = {local.CANONICAL_LOCK, local.UPSTREAM_LOCK, local.DISTRIBUTIONS_MANIFEST, self.archive_path, self.provenance_path, local.ORIGINAL_ARCHIVES + "/" + self.second_filename}
        self.assertEqual({item["path"] for item in report["input_files"]}, expected_inputs)
        self.assertEqual(report["provenance_paths"], [self.provenance_path])
        encoded = json.dumps(report)
        self.assertNotIn(str(self.root), encoded)
        self.assertNotIn("original fixture", encoded)
        self.assertNotIn("replacement_ascii", encoded)
        for field in ("installation_verified", "build_and_packaging_acceptance_verified", "os_denied_acceptance_verified", "preferred_source_closure_complete", "frontend_source_complete", "provenance_signatures_verified", "upstream_original_sri_recomputed"):
            self.assertIs(report[field], False)

    def test_original_only_manifest_is_valid_without_local_artifacts(self) -> None:
        for path in (self.archive_path, self.provenance_path):
            (self.root / path).unlink()
        (self.original_dir / self.first_filename).write_bytes(self.original_payload)
        self.manifest["variants"] = []
        for path in (self.first_key, self.nested_key):
            self.canonical["packages"][path]["resolved"] = "file:../../../" + local.ORIGINAL_ARCHIVES + "/" + self.first_filename
            self.canonical["packages"][path]["integrity"] = _sri(self.original_payload)
        self._write_inputs()
        report = self._verify()
        self.assertEqual((report["original_archive_count"], report["local_distribution_count"]), (2, 0))
        self.assertEqual(report["provenance_paths"], [])
        self.assertTrue(report["upstream_original_sri_recomputed"])

    def test_nested_declared_local_paths_are_exact_and_stable(self) -> None:
        nested = self.local_dir / "approved"
        nested.mkdir()
        for field in ("archive_path", "provenance_path"):
            before = self.variant[field]
            after = local.LOCAL_DISTRIBUTIONS + "/approved/" + before.rsplit("/", 1)[-1]
            (self.root / before).rename(self.root / after)
            self.variant[field] = after
        for path in (self.first_key, self.nested_key):
            self.canonical["packages"][path]["resolved"] = "file:../../../" + self.variant["archive_path"]
        self._write_inputs()
        report = self._verify()
        self.assertIn(self.variant["archive_path"], report["archive_paths"])

    def test_canonical_projection_rejects_every_other_lock_change(self) -> None:
        before = copy.deepcopy(self.canonical)
        changes = [
            lambda: self.canonical["packages"][self.first_key].update(resolved=self.upstream["packages"][self.first_key]["resolved"]),
            lambda: self.canonical["packages"][self.first_key].update(integrity=_sri(self.original_payload)),
            lambda: self.canonical["packages"][self.first_key].update(version="1.2.4"),
            lambda: self.canonical["packages"][self.first_key].update(dev=1),
            lambda: self.canonical["packages"][self.second_key].update(optional=False),
            lambda: self.canonical["packages"][self.second_key].update(os=["win32"]),
            lambda: self.canonical["packages"][""].update(dependencies={}),
            lambda: self.canonical["packages"].update({"node_modules/extra": dict(self.canonical["packages"][self.first_key])}),
            lambda: self.canonical.update(requires=1),
        ]
        for change in changes:
            with self.subTest(change=changes.index(change)):
                self.canonical = copy.deepcopy(before)
                change()
                self._write_inputs()
                self._rejected("exact local projection")

    def test_manifest_schema_hash_and_identity_are_strict(self) -> None:
        before = copy.deepcopy(self.manifest)
        changes = [
            lambda: self.manifest.update(schema="other"),
            lambda: self.manifest.update(upstream_lock_sha256="0" * 64),
            lambda: self.manifest.update(extra=True),
            lambda: self.manifest.update(variants={}),
            lambda: self.manifest["variants"][0].update(name="different"),
            lambda: self.manifest["variants"][0].update(version="^1.2.3"),
            lambda: self.manifest["variants"][0].update(archive_sha256="A" * 64),
            lambda: self.manifest["variants"][0].update(extra=True),
            lambda: self.manifest["variants"].append(dict(self.manifest["variants"][0])),
        ]
        for change in changes:
            with self.subTest(change=changes.index(change)):
                self.manifest = copy.deepcopy(before)
                change()
                self._write_inputs()
                self._rejected()

    def test_manifest_paths_cannot_escape_alias_or_change_artifact_types(self) -> None:
        for field, values in (("archive_path", ["/outside.tgz", "source-inputs/npm/archives/input.tgz", local.LOCAL_DISTRIBUTIONS + "/../input.tgz", local.LOCAL_DISTRIBUTIONS + "/input.zip", local.LOCAL_DISTRIBUTIONS + "/NUL.tgz", local.LOCAL_DISTRIBUTIONS + "/x\\input.tgz", local.LOCAL_DISTRIBUTIONS + "/%69nput.tgz", local.LOCAL_DISTRIBUTIONS + "/input#fragment.tgz"]), ("provenance_path", [local.LOCAL_DISTRIBUTIONS + "/input.tgz", local.LOCAL_DISTRIBUTIONS + "/../input.json"])):
            original_value = self.variant[field]
            for value in values:
                with self.subTest(field=field, value=value):
                    self.variant[field] = value
                    self._write_inputs()
                    self._rejected()
            self.variant[field] = original_value

    def test_missing_extra_and_original_variant_archives_fail_closed(self) -> None:
        for directory in (self.original_dir, self.local_dir):
            extra = directory / "extra.tgz"
            extra.write_bytes(b"undeclared input")
            self._rejected("undeclared entry")
            extra.unlink()
            extra.mkdir()
            self._rejected("undeclared entry")
            extra.rmdir()
        unsafe_original = self.original_dir / self.first_filename
        unsafe_original.write_bytes(self.original_payload)
        self._rejected("undeclared entry")
        unsafe_original.unlink()
        path = self.root / self.provenance_path
        payload = path.read_bytes()
        path.unlink()
        self._rejected("missing declared files")
        path.write_bytes(payload)
        (self.original_dir / self.second_filename).unlink()
        self._rejected("missing declared files")

    def test_original_archive_tamper_is_not_rescued_by_local_projection(self) -> None:
        path = self.original_dir / self.second_filename
        path.write_bytes(path.read_bytes() + b"changed")
        self._rejected()

    def test_variant_archive_and_provenance_hashes_are_required(self) -> None:
        archive = self.root / self.archive_path
        archive.write_bytes(self.patched_payload + b"changed")
        self._rejected("archive digest differs")
        archive.write_bytes(self.patched_payload)
        provenance = self.root / self.provenance_path
        provenance.write_bytes(provenance.read_bytes() + b" ")
        self._rejected("provenance digest differs")

    def test_provenance_claims_unknown_fields_and_pin_are_rejected_after_repin(self) -> None:
        before = copy.deepcopy(self.provenance)
        changes = [
            lambda: self.provenance.update(schema="other"),
            lambda: self.provenance.update(extra="untrusted source text"),
            lambda: self.provenance.update(original_locked_sri_verified=False),
            lambda: self.provenance.update(original_archive_unchanged=1),
            lambda: self.provenance.update(locked_install_verified=True),
            lambda: self.provenance.update(lockfile_sha256="0" * 64),
            lambda: self.provenance["original_pin"].update(version="1.2.4"),
            lambda: self.provenance["archive_before"].update(locked_integrity=_sri(b"different")),
            lambda: self.provenance["archive_after"].update(sha256="0" * 64),
            lambda: self.provenance.update(unchanged_other_member_count=1),
            lambda: self.provenance.update(package_json_sha256="0" * 64),
        ]
        for change in changes:
            with self.subTest(change=changes.index(change)):
                self.provenance = copy.deepcopy(before)
                change()
                self._write_provenance()
                self._rejected()

    def test_exact_patch_window_coordinates_and_after_bytes_are_checked(self) -> None:
        before = copy.deepcopy(self.provenance)
        changes = [{"offset": True}, {"offset": 0}, {"offset": 1000000}, {"removed_length": 0}, {"replacement_length": True}, {"matching_window_count": 2}, {"replacement_ascii": "different synthetic"}, {"replacement_sha256": "0" * 64}, {"removed_sha256": self.provenance["patch"]["replacement_sha256"]}, {"member_sha256_before": "0" * 64}, {"member_bytes_after": 0}, {"member_computed_sri_sha512_after": _sri(b"different")}]
        for change in changes:
            with self.subTest(change=change.keys()):
                self.provenance = copy.deepcopy(before)
                self.provenance["patch"].update(change)
                self._write_provenance()
                self._rejected()

    def test_only_one_member_and_its_size_metadata_may_differ(self) -> None:
        before = copy.deepcopy(self.provenance)
        changes = [
            lambda: self.provenance["member_inventory_before"][1].update(sha256="0" * 64),
            lambda: self.provenance["member_inventory_before"].append(dict(self.provenance["member_inventory_before"][0])),
            lambda: self.provenance["member_metadata_before"][1].update(uid=9),
            lambda: self.provenance["member_metadata_before"][1].update(uid=True),
            lambda: self.provenance["member_metadata_before"][1].update(extra="opaque"),
            lambda: self.provenance["member_metadata_after"][1].update(mode=0o777),
            lambda: self.provenance.update(license_files_preserved=[]),
            lambda: self.provenance.update(native_binary_files_preserved=[]),
        ]
        for change in changes:
            with self.subTest(change=changes.index(change)):
                self.provenance = copy.deepcopy(before)
                change()
                self._write_provenance()
                self._rejected()

    def test_input_mutation_and_inventory_growth_during_verification_are_rejected(self) -> None:
        verify = original._verify_archive
        for kind in ("lock", "archive", "directory"):
            with self.subTest(kind=kind):
                changed = False
                lock_bytes = self.upstream_path.read_bytes()
                archive_path = self.original_dir / self.second_filename
                archive_bytes = archive_path.read_bytes()
                extra = self.original_dir / "unexpected.tgz"

                def mutate(payload, identity):
                    nonlocal changed
                    record = verify(payload, identity)
                    if not changed:
                        changed = True
                        if kind == "lock":
                            self.upstream_path.write_bytes(lock_bytes + b" ")
                        elif kind == "archive":
                            archive_path.write_bytes(archive_bytes + b"changed")
                        else:
                            extra.write_bytes(b"new input")
                    return record

                with mock.patch.object(original, "_verify_archive", side_effect=mutate):
                    self._rejected("changed")
                self.upstream_path.write_bytes(lock_bytes)
                archive_path.write_bytes(archive_bytes)
                if extra.exists():
                    extra.unlink()

    def test_mutation_of_previously_read_provenance_is_rejected(self) -> None:
        read = original._read_checked_file
        target = self.root / self.provenance_path
        changed = False

        def mutate(path, label):
            nonlocal changed
            result = read(path, label)
            if Path(path) == target and not changed:
                changed = True
                target.write_bytes(result[0] + b" ")
            return result

        with mock.patch.object(original, "_read_checked_file", side_effect=mutate):
            self._rejected("changed")

    def test_linked_inputs_and_project_ancestors_are_rejected(self) -> None:
        target = self.root / self.archive_path
        outside = self.parent / "outside.tgz"
        outside.write_bytes(target.read_bytes())
        target.unlink()
        try:
            target.symlink_to(outside)
        except OSError:
            target.write_bytes(self.patched_payload)
            self.skipTest("filesystem does not permit symbolic links")
        self._rejected()
        target.unlink()
        target.write_bytes(self.patched_payload)
        alias = self.parent / "project-alias"
        alias.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(local.LocalBundleVerificationError):
            local.verify_local_bundle(alias)

    def test_reparse_classification_and_root_parent_traversal_fail_closed(self) -> None:
        target_inode = (self.root / self.archive_path).stat().st_ino
        is_link = original._is_link
        with mock.patch.object(original, "_is_link", side_effect=lambda stamp: is_link(stamp) or stamp.st_ino == target_inode):
            self._rejected()
        with self.assertRaisesRegex(local.LocalBundleVerificationError, "parent traversal"):
            local.verify_local_bundle(self.root / ".." / "project")

    def test_duplicate_json_keys_and_nonfinite_numbers_never_enter_report(self) -> None:
        original_bytes = self.manifest_path.read_bytes()
        for malformed in (original_bytes.replace(b'"variants":', b'"variants": [], "variants":'), original_bytes.replace(b'"variants":', b'"extra": NaN, "variants":')):
            self.manifest_path.write_bytes(malformed)
            self._rejected()

    def test_cli_exclusively_creates_root_path_free_report_and_redacts_errors(self) -> None:
        args = ["--project-root", str(self.root)]
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(local.main(args), 0)
        expected = _json_bytes(self._verify())
        self.assertEqual(output.getvalue().encode(), expected)
        destination = self.parent / "report.json"
        self.assertEqual(local.main(args + ["--output", str(destination)]), 0)
        self.assertEqual(destination.read_bytes(), expected)
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            self.assertEqual(local.main(args + ["--output", str(destination)]), 1)
        self.assertEqual(destination.read_bytes(), expected)
        inside = self.local_dir / "report.json"
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(local.main(args + ["--output", str(inside)]), 1)
        self.assertFalse(inside.exists())
        self.manifest_path.write_bytes(b'{"opaque-payload": "do-not-print-this",}')
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            self.assertEqual(local.main(args), 1)
        self.assertNotIn("do-not-print-this", error.getvalue())
        with contextlib.redirect_stderr(error), self.assertRaises(SystemExit):
            local.main(["--unknown=do-not-print-this"])
        self.assertNotIn("do-not-print-this", error.getvalue())


if __name__ == "__main__":
    unittest.main()
