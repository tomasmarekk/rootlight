"""Authored distribution controls for independent producer Yarn revalidation.

Fixtures mock only the separately reviewed authored-producer boundary. Real archive
and tree verification runs locally; no retained packages, installers, or network run.
"""

from __future__ import annotations

import base64
from contextlib import redirect_stderr
import gzip
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

import verify_lightning_yarn_sources as verifier


def archive(name="leaf", version="1.0.0", *, root="authored root", extras=None):
    """Return an authored inert gzip tar and exact regular member bytes/modes."""
    members = {
        root + "/package.json": (json.dumps({"name": name, "version": version,
                                           "license": "MIT", "repository": "https://example.invalid/source"}).encode(), 0o644),
        root + "/LICENSE": (b"Authored synthetic license fixture\n", 0o644),
        root + "/bin/task.js": (b"inert executable source bytes\r\n", 0o755),
        root + "/native.wasm": (b"\x00asm\x01\x00\x00\x00", 0o644),
    }
    members.update(extras or {})
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as output:
        empty = tarfile.TarInfo(root + "/nested/empty")
        empty.type, empty.mode = tarfile.DIRTYPE, 0o755
        output.addfile(empty)
        for path, (payload, mode) in members.items():
            member = tarfile.TarInfo(path)
            member.size, member.mode = len(payload), mode
            output.addfile(member, io.BytesIO(payload))
    return gzip.compress(stream.getvalue(), mtime=0), members


def pin_fields(name, payload):
    """Return exact Yarn-v1 URL and strong SRI for authored archive bytes."""
    return (f"https://registry.yarnpkg.com/{name}/-/{name}-1.0.0.tgz#{hashlib.sha1(payload).hexdigest()}",
            "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii"))


class YarnSourceTests(unittest.TestCase):
    """Require independent locks, originals, expansion and report conservation."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.project = Path(self.temporary.name)
        self.bundle = self.project / verifier.BUNDLE_PATH
        (self.bundle / "archives").mkdir(parents=True)
        (self.bundle / "packages").mkdir()
        self.tree = self.project / verifier.PRODUCER_PATH / "tree"
        self.tree.mkdir(parents=True)
        self.manifest = {"name": "fixture-producer", "version": "1.0.0",
                         "dependencies": {"leaf": "^1.0.0"},
                         "optionalDependencies": {"second": "^1.0.0"},
                         "resolutions": {"fixture-producer": "link:."}}
        self.rows, sections = [], ["# yarn lockfile v1\n"]
        for name in ("leaf", "second"):
            payload, members = archive(name)
            resolved, integrity = pin_fields(name, payload)
            sections.append(f'{name}@^1.0.0:\n  version "1.0.0"\n  resolved "{resolved}"\n  integrity {integrity}\n\n')
            filename = hashlib.sha256(resolved.encode("ascii")).hexdigest() + ".tgz"
            source = self.bundle / "archives" / filename
            source.write_bytes(payload)
            source.chmod(0o644)
            target = self.bundle / "packages" / Path(filename).stem
            (target / "authored root/nested/empty").mkdir(parents=True)
            for path, (contents, mode) in members.items():
                destination = target / path
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(contents)
                destination.chmod(mode)
            self.rows.append({"name": name, "archive": source, "target": target, "payload": payload})
        sections.append('"fixture-producer@link:.":\n  version "0.0.0"\n  uid ""\n')
        (self.tree / "package.json").write_text(json.dumps(self.manifest), encoding="utf-8")
        (self.tree / "yarn.lock").write_text("".join(sections), encoding="ascii")
        self.evidence = {"association": {"name": "fixture-producer", "version": "1.0.0",
                                         "caller_observed_transport_verified": False}}
        self.source_report = b'{"fixture":"separately verified producer boundary"}\n'
        self.input_patch = patch.object(verifier, "_inputs", side_effect=self.inputs)
        self.input_patch.start()
        self.addCleanup(self.input_patch.stop)

    def inputs(self, project):
        """Mock the authored producer verifier, retaining real guarded source reads."""
        self.assertEqual(project, self.project)
        return (self.evidence,
                verifier.producer.read_bytes(self.tree / "package.json", verifier.yarn.MAX_MANIFEST_BYTES),
                verifier.producer.read_bytes(self.tree / "yarn.lock", verifier.yarn.MAX_LOCK_BYTES),
                self.source_report)

    def publish_report(self):
        """Write a derived canonical fixture report, returning its independent proof."""
        report = verifier.derive_bundle(self.project, self.bundle)
        path = self.bundle / verifier.REPORT_NAME
        path.write_bytes(verifier.producer.json_bytes(report))
        path.chmod(0o644)
        return report

    def rejection(self, operation=None):
        """Require a typed validation/I/O rejection, never an incidental assertion."""
        with self.assertRaises((ValueError, OSError)):
            (operation or (lambda: verifier.derive_bundle(self.project, self.bundle)))()

    def test_exact_originals_expansion_licenses_modes_and_local_association_reverify(self):
        before = {str(path): path.read_bytes() for path in self.tree.iterdir()}
        report = self.publish_report()
        self.assertEqual(verifier.verify_bundle(self.project), report)
        self.assertEqual(report["archive_count"], 2)
        self.assertEqual(report["pin_graph"]["stanza_count"], 3)
        self.assertEqual(report["pin_graph"]["local_associations"][0]["resolution"], "link:.")
        self.assertEqual(len(report["packages"]), 2)
        for row in report["packages"]:
            proof = row["published_package"]
            self.assertEqual(proof["archive_root"], "authored root")
            self.assertEqual(proof["license_metadata"], {"license": "MIT"})
            self.assertEqual(len(proof["license_files"]), 1)
            self.assertEqual(len(proof["native_binary_files"]), 1)
            self.assertEqual(report["files"][row["expanded_path"] + "/authored root/bin/task.js"]["mode"], 0o755)
            self.assertIn(row["expanded_path"] + "/authored root/nested/empty", report["directories"])
        self.assertTrue(all(report[key] is False for key in verifier.FALSE_CLAIMS))
        self.assertEqual(before, {str(path): path.read_bytes() for path in self.tree.iterdir()})

    def test_optional_original_missing_and_extra_archive_reject(self):
        missing = self.rows[1]["archive"]
        payload = missing.read_bytes()
        missing.unlink()
        self.rejection()
        missing.write_bytes(payload)
        (self.bundle / "archives/extra.tgz").write_bytes(b"inert")
        self.rejection()

    def test_archive_sri_mismatch_precedes_decompression(self):
        self.rows[0]["archive"].write_bytes(b"not an authenticated archive")
        with patch.object(verifier, "_bounded_members", side_effect=AssertionError("must not decompress")):
            self.rejection()

    def test_sha1_fragment_cannot_be_ignored_even_with_valid_sri(self):
        text = (self.tree / "yarn.lock").read_text()
        digest = hashlib.sha1(self.rows[0]["payload"]).hexdigest()
        (self.tree / "yarn.lock").write_text(text.replace(digest, "0" * 40))
        # Filename derives the full URL, so move only the exact original to that new name.
        old = self.rows[0]["archive"]
        resolved, _ = pin_fields("leaf", self.rows[0]["payload"])
        filename = hashlib.sha256(resolved.replace(digest, "0" * 40).encode()).hexdigest() + ".tgz"
        old.rename(old.parent / filename)
        self.rows[0]["target"].rename(self.bundle / "packages" / Path(filename).stem)
        self.rejection()

    def test_expanded_mutation_missing_file_and_extra_directory_reject(self):
        path = self.rows[0]["target"] / "authored root/LICENSE"
        original = path.read_bytes()
        path.write_bytes(b"different bytes")
        self.rejection()
        path.write_bytes(original)
        path.unlink()
        self.rejection()
        path.write_bytes(original)
        (self.rows[0]["target"] / "extra empty").mkdir()
        self.rejection()

    def test_report_tamper_extra_key_false_claim_and_noncanonical_bytes_reject(self):
        report = self.publish_report()
        for change in (lambda value: value.update(extra=True),
                       lambda value: value.update(preferred_source_closure_complete=True),
                       lambda value: value["packages"][0]["published_package"].update(member_count=999)):
            candidate = json.loads(json.dumps(report))
            change(candidate)
            (self.bundle / verifier.REPORT_NAME).write_bytes(verifier.producer.json_bytes(candidate))
            self.rejection(lambda: verifier.verify_bundle(self.project))
        (self.bundle / verifier.REPORT_NAME).write_bytes(json.dumps(report).encode())
        self.rejection(lambda: verifier.verify_bundle(self.project))

    def test_verification_requires_report_and_exact_top_namespace(self):
        self.rejection(lambda: verifier.verify_bundle(self.project))
        (self.bundle / "original-archives.json").write_bytes(b"{}")
        self.rejection()

    def test_mutated_raw_lock_and_producer_association_reject_report(self):
        self.publish_report()
        with (self.tree / "yarn.lock").open("ab") as stream:
            stream.write(b"# still semantically equal but changed authored bytes\n")
        self.rejection(lambda: verifier.verify_bundle(self.project))
        self.evidence["association"]["version"] = "2.0.0"
        self.rejection(lambda: verifier.verify_bundle(self.project))

    def test_manifest_selector_omission_unreachable_stanza_and_local_rebinding_reject(self):
        path = self.tree / "package.json"
        original = path.read_bytes()
        for manifest in ({**self.manifest, "dependencies": {"leaf": "^2.0.0"}},
                         {**self.manifest, "optionalDependencies": {}},
                         {**self.manifest, "resolutions": {"fixture-producer": "link:../outside"}}):
            with self.subTest(manifest=manifest):
                path.write_text(json.dumps(manifest), encoding="utf-8")
                self.rejection()
        path.write_bytes(original)

    def test_raw_input_and_original_mutation_between_passes_reject(self):
        check = verifier._check_inventory
        for mutate in (lambda: (self.tree / "yarn.lock").write_bytes(b"changed lock bytes"),
                       lambda: self.rows[0]["archive"].write_bytes(b"changed original bytes")):
            lock = (self.tree / "yarn.lock").read_bytes()
            archive_bytes = self.rows[0]["archive"].read_bytes()
            def check_then_change(bundle, files, directories):
                check(bundle, files, directories)
                mutate()
            with self.subTest(mutation=mutate), patch.object(verifier, "_check_inventory", side_effect=check_then_change):
                self.rejection()
            (self.tree / "yarn.lock").write_bytes(lock)
            self.rows[0]["archive"].write_bytes(archive_bytes)

    def test_report_member_mode_and_directory_omissions_cannot_rebind_evidence(self):
        report = self.publish_report()
        for change in (lambda value: value["packages"][0]["published_package"]["members"][0].update(archive_mode=0),
                       lambda value: value["directories"].pop(),
                       lambda value: value["files"].pop(next(iter(value["files"])))):
            candidate = json.loads(json.dumps(report))
            change(candidate)
            (self.bundle / verifier.REPORT_NAME).write_bytes(verifier.producer.json_bytes(candidate))
            self.rejection(lambda: verifier.verify_bundle(self.project))

    def test_invalid_gzip_padding_and_duplicate_case_aliased_members_reject(self):
        payload = self.rows[0]["payload"]
        expanded = gzip.decompress(payload)
        for contents in (b"not gzip", gzip.compress(expanded + b"nonzero padding")):
            self.rejection(lambda: verifier._bounded_members(contents, verifier.MAX_TOTAL_EXPANDED,
                                                            verifier.MAX_TOTAL_MEMBERS))
        for names in (("root/data", "root/data"), ("root/data", "root/DATA")):
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode="w") as output:
                for name in names:
                    member = tarfile.TarInfo(name)
                    member.size, member.mode = 1, 0o644
                    output.addfile(member, io.BytesIO(b"x"))
            self.rejection(lambda: verifier._bounded_members(gzip.compress(stream.getvalue()),
                                                            verifier.MAX_TOTAL_EXPANDED, verifier.MAX_TOTAL_MEMBERS))

    def test_after_read_mutation_and_walk_io_error_fail_closed(self):
        check = verifier._check_inventory
        calls = 0
        def mutate(bundle, files, directories):
            nonlocal calls
            check(bundle, files, directories)
            calls += 1
            if calls == 1:
                (self.rows[0]["target"] / "authored root/LICENSE").write_bytes(b"changed after check")
        with patch.object(verifier, "_check_inventory", side_effect=mutate):
            self.rejection()
        def inaccessible(root, **keywords):
            keywords["onerror"](PermissionError("authored mock traversal failure"))
            return iter(())
        with patch.object(verifier.os, "walk", side_effect=inaccessible):
            self.rejection()

    @unittest.skipIf(os.name == "nt", "Windows does not expose authoritative POSIX execute bits")
    def test_posix_file_execute_intent_is_authoritative_not_directory_permissions(self):
        path = self.rows[0]["target"] / "authored root/bin/task.js"
        path.chmod(0o644)
        self.rejection()
        path.chmod(0o755)
        (self.rows[0]["target"] / "authored root/nested/empty").chmod(0o700)
        self.publish_report()
        self.assertEqual(verifier.verify_bundle(self.project)["archive_count"], 2)

    def test_links_parent_traversal_and_reparse_point_reject(self):
        self.rejection(lambda: verifier.derive_bundle(self.project, self.bundle / "../yarn-originals"))
        metadata = os.stat_result((stat.S_IFDIR | 0o755, 0, 0, 1, 0, 0, 0, 0, 0, 0))
        with patch.object(verifier.original, "_is_link", side_effect=lambda value: value is metadata), \
             patch.object(Path, "lstat", return_value=metadata):
            self.rejection()
        outside = self.project / "outside"
        outside.write_bytes(b"source")
        link = self.rows[0]["target"] / "authored root/linked"
        try:
            link.symlink_to(outside)
        except OSError:
            return
        self.rejection()

    def test_path_alias_extra_and_missing_implicit_empty_directory_reject(self):
        (self.rows[0]["target"] / "authored root/nested/empty").rmdir()
        self.rejection()
        (self.rows[0]["target"] / "authored root/nested/empty").mkdir()
        (self.rows[0]["target"] / "authored root/new-file").write_bytes(b"extra")
        self.rejection()

    def test_limits_and_canonical_member_projection_are_independent(self):
        for limit, value in (("MAX_COMPRESSED", 1), ("MAX_EXPANDED", 1),
                             ("MAX_TOTAL_EXPANDED", 1), ("MAX_MEMBERS", 1),
                             ("MAX_TOTAL_MEMBERS", 1), ("MAX_MANIFEST", 1), ("MAX_REPORT", 1)):
            with self.subTest(limit=limit), patch.object(verifier, limit, value):
                self.rejection()
        original = verifier.original._verify_archive
        def changed(payload, identity):
            proof = original(payload, identity)
            proof["members"][0]["archive_mode"] ^= 1
            return proof
        with patch.object(verifier.original, "_verify_archive", side_effect=changed):
            self.rejection()

    def test_unsafe_archive_members_are_rejected_before_existing_verifier(self):
        for path in ("../outside", "/absolute", "root/../escaped", "root/CON"):
            payload, _ = archive(extras={path: (b"inert", 0o644)})
            with self.subTest(path=path), patch.object(verifier.original, "_verify_archive", side_effect=AssertionError("preflight")):
                self.rejection(lambda: verifier._bounded_members(payload, verifier.MAX_TOTAL_EXPANDED,
                                                                verifier.MAX_TOTAL_MEMBERS))
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as output:
            link = tarfile.TarInfo("root/link")
            link.type, link.linkname = tarfile.SYMTYPE, "../outside"
            output.addfile(link)
        self.rejection(lambda: verifier._bounded_members(gzip.compress(stream.getvalue()),
                                                        verifier.MAX_TOTAL_EXPANDED, verifier.MAX_TOTAL_MEMBERS))

    def test_actual_archive_identity_and_multiple_roots_reject(self):
        for payload in (archive(version="2.0.0")[0],
                        archive(extras={"other-root/data": (b"inert", 0o644)})[0]):
            resolved, integrity = pin_fields("leaf", payload)
            pin = verifier.yarn._pin("leaf", "1.0.0", {"version": {"value": "1.0.0"},
                                   "resolved": {"value": resolved}, "integrity": {"value": integrity}})
            self.rejection(lambda: verifier._authenticate(payload, pin, verifier.MAX_TOTAL_EXPANDED,
                                                          verifier.MAX_TOTAL_MEMBERS))

    def test_default_input_boundary_calls_the_independent_authored_producer_verifier(self):
        self.input_patch.stop()
        with patch.object(verifier.producer, "verify_bundle", side_effect=ValueError("authored source changed")) as proof:
            self.rejection()
        proof.assert_called_once_with(self.project, self.project / verifier.PRODUCER_PATH)
        self.input_patch.start()

    def test_cli_failures_are_source_free(self):
        error = io.StringIO()
        with redirect_stderr(error):
            result = verifier.main(["--project", str(self.project)])
        self.assertEqual(result, 1)
        self.assertEqual(error.getvalue(), "producer Yarn source verification failed\n")


if __name__ == "__main__":
    unittest.main()
