#!/usr/bin/env python3
"""Exercise local npm fixture patching with tiny, wholly authored archives.

No real source archive, secret-like fixture, npm process, or network is used.
Tests pin the single-delta contract and the limited, redacted evidence scope.
"""

from __future__ import annotations

import contextlib
import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import patch_npm_source_fixture as fixture


class FixturePatchTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.archives = self.root / "originals"
        self.archives.mkdir()
        self.lock = self.root / "package-lock.json"
        self.output = self.root / "patched"
        self.output.mkdir()
        self.member = "package/src/tests/sample.test.ts"
        self.removed = b"authored-fixture-value"
        self.replacement = b"synthetic-fixture-zero"
        self.contents = b"const sample = '" + self.removed + b"';\n"
        self._install(self.contents)

    def _archive(self, contents: bytes, *, runtime: bytes = b"export const value = 1;\n", extra: list[tuple[str, bytes, bytes]] | None = None) -> bytes:
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:", format=tarfile.PAX_FORMAT) as archive:
            directory = tarfile.TarInfo("package")
            directory.type = tarfile.DIRTYPE
            directory.mode = 0o750
            directory.mtime = 123456
            archive.addfile(directory)
            records = [
                ("package/package.json", json.dumps({"name": "sample-input", "version": "1.2.3", "license": "MIT", "repository": "https://example.test/authored"}).encode(), tarfile.REGTYPE),
                ("package/LICENSE", b"authored synthetic license text\n", tarfile.REGTYPE),
                ("package/index.js", runtime, tarfile.AREGTYPE),
                ("package/bin/run.js", b"#!/usr/bin/env node\n", tarfile.REGTYPE),
                (self.member, contents, tarfile.REGTYPE),
            ] + (extra or [])
            for path, content, kind in records:
                member = tarfile.TarInfo(path)
                member.size = len(content)
                member.type = kind
                member.mode = 0o755 if path == "package/bin/run.js" else 0o640
                member.uid = 12
                member.gid = 34
                member.uname = "fixture-owner"
                member.gname = "fixture-group"
                member.mtime = 123456
                if path == self.member:
                    member.pax_headers = {"fixture.note": "authored-metadata", "size": str(len(content))}
                archive.addfile(member, io.BytesIO(content))
        return gzip.compress(output.getvalue(), mtime=0)

    def _install(self, contents: bytes, *, extra: list[tuple[str, bytes, bytes]] | None = None) -> None:
        self.original = self._archive(contents, extra=extra)
        self.url = "https://registry.npmjs.org/sample-input/-/sample-input-1.2.3.tgz"
        self.archive_path = self.archives / fixture.verifier.archive_filename(self.url)
        self.archive_path.write_bytes(self.original)
        self.lock_document = {
            "name": "fixture-project", "version": "0.1.0", "lockfileVersion": 3,
            "packages": {
                "": {"name": "fixture-project", "version": "0.1.0"},
                "node_modules/sample-input": {
                    "version": "1.2.3", "resolved": self.url,
                    "integrity": fixture._sri(self.original), "license": "MIT",
                },
            },
        }
        self.lock.write_text(json.dumps(self.lock_document), encoding="utf-8")
        self.contents = contents

    def _run(self, **changes: object) -> dict:
        arguments = {
            "lock": self.lock, "archives": self.archives,
            "package_name": "sample-input", "version": "1.2.3",
            "member_path": self.member,
            "expected_member_sha256": hashlib.sha256(self.contents).hexdigest(),
            "removed_length": len(self.removed),
            "removed_sha256": hashlib.sha256(self.removed).hexdigest(),
            "replacement": self.replacement, "output": self.output,
            "synthetic_replacement_confirmed": True,
        }
        arguments.update(changes)
        return fixture.patch_fixture_archive(**arguments)

    def _members(self, payload: bytes) -> list[tuple[dict, bytes | None]]:
        records = []
        with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
            for member in archive:
                stream = archive.extractfile(member) if member.isfile() else None
                records.append((fixture._member_metadata(member), stream.read() if stream else None))
                if stream:
                    stream.close()
        return records

    def _reject(self, **changes: object) -> None:
        with self.assertRaises((fixture.FixturePatchError, OSError, ValueError)):
            self._run(**changes)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_exactly_one_fixture_delta_preserves_every_other_byte_and_metadata(self) -> None:
        original_lock = self.lock.read_bytes()
        evidence = self._run()
        patched = (self.output / "patched.tgz").read_bytes()
        before = self._members(self.original)
        after = self._members(patched)
        self.assertEqual(len(before), len(after))
        changed = []
        for old, new in zip(before, after):
            self.assertEqual(old[0], new[0])
            if old[1] != new[1]:
                changed.append(old[0]["path"])
                self.assertEqual(new[1], self.contents.replace(self.removed, self.replacement))
        self.assertEqual(changed, [self.member])
        self.assertEqual(self.archive_path.read_bytes(), self.original)
        self.assertEqual(self.lock.read_bytes(), original_lock)
        self.assertEqual(set(path.name for path in self.output.iterdir()), {"patched.tgz", "patch-provenance.json"})
        self.assertEqual(evidence, json.loads((self.output / "patch-provenance.json").read_bytes()))
        encoded = (self.output / "patch-provenance.json").read_bytes()
        self.assertNotIn(self.removed, encoded)
        self.assertNotIn(self.contents, encoded)
        self.assertNotIn(str(self.root).encode(), encoded)
        self.assertEqual(evidence["patch"]["offset"], self.contents.index(self.removed))
        self.assertEqual(evidence["patch"]["matching_window_count"], 1)
        self.assertEqual(evidence["patch"]["member_computed_sri_sha512_before"], fixture._sri(self.contents))
        self.assertEqual(evidence["patch"]["member_computed_sri_sha512_after"], fixture._sri(self.contents.replace(self.removed, self.replacement)))
        self.assertEqual(evidence["archive_before"]["locked_integrity"], fixture._sri(self.original))
        self.assertEqual(evidence["archive_after"]["computed_sri_sha512"], fixture._sri(patched))
        self.assertNotEqual(evidence["archive_before"]["computed_sri_sha512"], evidence["archive_after"]["computed_sri_sha512"])
        self.assertTrue(evidence["locally_modified_distribution"])
        for flag in ("patched_archive_is_original_upstream_archive", "canonical_lock_updated", "locked_install_verified", "preferred_source_closure_complete", "frontend_source_complete", "os_blocked_network_verified", "provenance_signatures_verified"):
            self.assertFalse(evidence[flag], flag)

    def test_deterministic_twice_and_explicit_offset_does_not_change_result(self) -> None:
        first = self._run()
        second_output = self.root / "second"
        second_output.mkdir()
        second = self._run(output=second_output, offset=self.contents.index(self.removed))
        self.assertEqual(first, second)
        for filename in ("patched.tgz", "patch-provenance.json"):
            self.assertEqual((self.output / filename).read_bytes(), (second_output / filename).read_bytes())
        self.assertEqual(gzip.decompress((self.output / "patched.tgz").read_bytes())[-1024:], bytes(1024))

    def test_different_replacement_length_changes_only_target_size_and_pax_size(self) -> None:
        replacement = b"synthetic-short"
        evidence = self._run(replacement=replacement)
        before = self._members(self.original)
        after = self._members((self.output / "patched.tgz").read_bytes())
        for old, new in zip(before, after):
            if old[0]["path"] != self.member:
                self.assertEqual(old, new)
            else:
                expected = dict(old[0])
                expected.update(bytes=len(self.contents) - len(self.removed) + len(replacement), pax_size=len(new[1]), pax_headers_sha256=new[0]["pax_headers_sha256"])
                self.assertEqual(expected, new[0])
                self.assertEqual(new[1], self.contents.replace(self.removed, replacement))
        self.assertEqual(evidence["patch"]["replacement_length"], len(replacement))

    def test_missing_package_version_member_and_directory_target_fail_closed(self) -> None:
        for changes in ({"package_name": "missing"}, {"version": "9.9.9"}, {"member_path": "package/tests/missing.ts"}, {"member_path": "package"}):
            with self.subTest(changes=changes):
                self._reject(**changes)

    def test_wrong_member_hash_window_hash_length_or_offset_fail_closed(self) -> None:
        for changes in (
            {"expected_member_sha256": "0" * 64}, {"removed_sha256": "0" * 64},
            {"removed_length": len(self.contents) + 1}, {"removed_length": 0},
            {"removed_length": -1}, {"removed_length": True},
            {"offset": 0}, {"offset": -1}, {"offset": True},
            {"removed_sha256": "A" * 64}, {"expected_member_sha256": "not-a-digest"},
        ):
            with self.subTest(changes=changes):
                self._reject(**changes)

    def test_duplicate_matching_windows_fail_even_with_explicit_offset(self) -> None:
        self._install(self.removed + b" / " + self.removed)
        self._reject()
        self._reject(offset=0)

    def test_synthetic_ascii_assertion_and_real_delta_are_required(self) -> None:
        for changes in (
            {"synthetic_replacement_confirmed": False}, {"synthetic_replacement_confirmed": 1},
            {"replacement": b""}, {"replacement": b"line\n"}, {"replacement": b"\x7f"},
            {"replacement": b"\x80"}, {"replacement": "synthetic-text"}, {"replacement": self.removed},
        ):
            with self.subTest(changes=changes):
                self._reject(**changes)

    def test_runtime_manifest_license_and_native_targets_are_protected(self) -> None:
        self._install(self.contents, extra=[("package/tests/LICENSE", self.contents, tarfile.REGTYPE), ("package/tests/probe.wasm", b"\x00asm" + self.contents, tarfile.REGTYPE)])
        for path in ("package/index.js", "package/package.json", "package/LICENSE", "package/tests/LICENSE", "package/tests/probe.wasm"):
            target_bytes = next(content for metadata, content in self._members(self.original) if metadata["path"] == path)
            with self.subTest(path=path):
                self._reject(member_path=path, expected_member_sha256=hashlib.sha256(target_bytes).hexdigest())

    def test_original_sri_and_manifest_identity_are_still_required(self) -> None:
        self.archive_path.write_bytes(self.original + b"tampered")
        self._reject()
        self.archive_path.write_bytes(self.original)
        self.lock_document["packages"]["node_modules/sample-input"]["version"] = "1.2.4"
        self.lock.write_text(json.dumps(self.lock_document), encoding="utf-8")
        self._reject(version="1.2.4")

    def test_unsafe_member_names_and_linked_archive_entries_fail_closed(self) -> None:
        for name in ("../escape", "package/tests/../escape", "/package/tests/escape", "package/tests/CON", "package/tests/case."):
            with self.subTest(name=name):
                self._reject(member_path=name)
        for name, kind in (("package/tests/link", tarfile.SYMTYPE), ("package/tests/hard", tarfile.LNKTYPE), ("package/tests/../escape", tarfile.REGTYPE)):
            with self.subTest(name=name):
                self._install(self.contents, extra=[(name, b"", kind)])
                self._reject()

    def test_output_must_exist_empty_and_outside_inputs_without_overwrite(self) -> None:
        marker = self.output / "keep.txt"
        marker.write_bytes(b"existing-owned-data")
        with self.assertRaises(fixture.FixturePatchError):
            self._run()
        self.assertEqual(marker.read_bytes(), b"existing-owned-data")
        self.assertEqual(list(self.output.iterdir()), [marker])
        marker.unlink()
        for output in (self.root / "missing", self.archives, self.archives / "fresh", self.root, self.output / ".." / "patched"):
            with self.subTest(output=output), self.assertRaises((fixture.FixturePatchError, OSError)):
                self._run(output=output)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_link_and_reparse_output_ancestors_are_rejected(self) -> None:
        link = self.root / "linked"
        try:
            link.symlink_to(self.output, target_is_directory=True)
        except OSError:
            link = None
        if link is not None:
            self._reject(output=link)
        original_is_link = fixture.verifier._is_link
        output_stat = self.output.lstat()
        with patch.object(fixture.verifier, "_is_link", side_effect=lambda metadata: original_is_link(metadata) or (metadata.st_dev, metadata.st_ino) == (output_stat.st_dev, output_stat.st_ino)):
            self._reject()

    def test_unapproved_other_member_mutation_cannot_pass_repacked_verification(self) -> None:
        original_repack = fixture._repack
        def changed_repack(*arguments):
            _, offset, metadata, before_sri, after_sri = original_repack(*arguments)
            altered = self._archive(self.contents.replace(self.removed, self.replacement), runtime=b"unapproved-runtime-change\n")
            return altered, offset, metadata, before_sri, after_sri
        with patch.object(fixture, "_repack", side_effect=changed_repack):
            self._reject()

    def test_other_member_mode_metadata_or_order_mutation_fails_closed(self) -> None:
        before = fixture.verifier._verify_archive(self.original, fixture.verifier.locked_archives(self.lock)[0])
        metadata = fixture._metadata_inventory(self.original)
        for field, value in (("mode", 0o777), ("uid", 999), ("uname_sha256", "0" * 64), ("pax_non_size_headers_sha256", "0" * 64)):
            changed = [dict(item) for item in metadata]
            changed[2][field] = value
            with self.subTest(field=field), self.assertRaises(fixture.FixturePatchError):
                fixture._preservation(before, before, metadata, changed, self.member, 0)
        with self.assertRaises(fixture.FixturePatchError):
            fixture._preservation(before, before, metadata, list(reversed(metadata)), self.member, 0)

    def test_input_changes_before_publication_fail_without_output(self) -> None:
        original_repack = fixture._repack
        def changed_repack(*arguments):
            result = original_repack(*arguments)
            self.lock.write_bytes(self.lock.read_bytes() + b" ")
            return result
        with patch.object(fixture, "_repack", side_effect=changed_repack):
            self._reject()

    def test_repeat_publication_never_overwrites_existing_outputs(self) -> None:
        self._run()
        original_outputs = {path.name: path.read_bytes() for path in self.output.iterdir()}
        with self.assertRaises(fixture.FixturePatchError):
            self._run()
        self.assertEqual(original_outputs, {path.name: path.read_bytes() for path in self.output.iterdir()})

    def test_cli_is_redacted_on_success_and_failure(self) -> None:
        arguments = [
            "--lock", str(self.lock), "--archives", str(self.archives), "--package", "sample-input",
            "--version", "1.2.3", "--member", self.member,
            "--member-sha256", hashlib.sha256(self.contents).hexdigest(),
            "--removed-length", str(len(self.removed)), "--removed-sha256", hashlib.sha256(self.removed).hexdigest(),
            "--replacement-ascii", self.replacement.decode(), "--confirm-synthetic-replacement", "--output", str(self.output),
        ]
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            self.assertEqual(fixture.main(arguments), 0)
            self.assertEqual(fixture.main(arguments), 1)
        self.assertIn("Locally modified", stdout.getvalue())
        self.assertIn("fixture patch rejected", stderr.getvalue())
        self.assertNotIn(self.removed.decode(), stdout.getvalue() + stderr.getvalue())
        self.assertNotIn(self.contents.decode(), stdout.getvalue() + stderr.getvalue())

    def test_cli_invalid_option_values_are_not_repeated(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as result:
            fixture.main(["--removed-length", self.removed.decode()])
        self.assertEqual(result.exception.code, 2)
        self.assertNotIn(self.removed.decode(), stderr.getvalue())
        self.assertIn("invalid fixture patch arguments", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
