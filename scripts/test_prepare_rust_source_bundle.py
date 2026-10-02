#!/usr/bin/env python3
"""Exercise Cargo/local npm/producer exports using authored fixtures and mocked Git.

No Cargo, npm, registry or product process runs; the portable export retains
strict source/mode boundaries without claiming installation or OS isolation.
"""

import base64
import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import tomllib
import unittest
from unittest.mock import patch

import prepare_rust_source_bundle as bundle
import import_lightning_producer_sources as producer_import
import patch_npm_source_fixture as fixture
import verify_lightning_producer_sources as producer
import verify_local_npm_sources as npm
import verify_npm_source_bundle as original_npm
import vendor_rust_sources as vendor
from test_verify_local_npm_sources import _archive as npm_archive
from test_verify_lightning_producer_sources import AuthoredFixture, archive as producer_archive


class RustSourceBundleTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.archive = self.root / "sample-1.0.0.crate"
        self.vendor = self.root / "vendor"
        self.members = {
            "Cargo.toml": (b'[package]\nname = "sample"\nversion = "1.0.0"\n'
                           b'license = "MIT"\n', 0o644),
            "src/lib.rs": (b"pub fn value() -> u8 { 1 }\n", 0o644),
            "bin/tool": (b"#!/bin/sh\nexit 0\n", 0o755),
            "LICENSE": (b"synthetic license fixture\n", 0o644),
        }

    def write_archive(self, *, extra: tarfile.TarInfo | None = None,
                      duplicate: bool = False) -> str:
        with tarfile.open(self.archive, "w:gz") as tar:
            for name, (content, mode) in self.members.items():
                member = tarfile.TarInfo(f"sample-1.0.0/{name}")
                member.size, member.mode = len(content), mode
                tar.addfile(member, io.BytesIO(content))
                if duplicate:
                    tar.addfile(member, io.BytesIO(content))
                    duplicate = False
            if extra:
                tar.addfile(extra)
        return bundle.digest(self.archive)

    def materialize(self, checksum: str) -> dict:
        return bundle.materialize_archive(self.archive, self.vendor,
                                          "sample", "1.0.0", checksum)

    def test_materialize_preserves_exact_bytes_checksums_and_executable_intent(self) -> None:
        checksum = self.write_archive()
        modes = self.materialize(checksum)
        package = self.vendor / "sample-1.0.0"
        for name, (content, mode) in self.members.items():
            self.assertEqual((package / name).read_bytes(), content)
            self.assertEqual(modes[f"source-inputs/cargo/sample-1.0.0/{name}"], mode)
        metadata = json.loads((package / ".cargo-checksum.json").read_bytes())
        self.assertEqual(metadata["package"], checksum)
        self.assertEqual(metadata["files"], {
            name: hashlib.sha256(content).hexdigest()
            for name, (content, _mode) in self.members.items()
        })

    def test_materialize_rejects_wrong_locked_digest_before_creating_sources(self) -> None:
        self.write_archive()
        with self.assertRaisesRegex(ValueError, "archive checksum"):
            self.materialize("0" * 64)
        self.assertFalse(self.vendor.exists())

    def test_materialize_rejects_escaping_members(self) -> None:
        member = tarfile.TarInfo("sample-1.0.0/../outside")
        with self.assertRaisesRegex(ValueError, "unsafe source path"):
            self.materialize(self.write_archive(extra=member))
        self.assertFalse((self.root / "outside").exists())

    def test_materialize_rejects_symlink_hardlink_and_special_members(self) -> None:
        for member_type in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CONTTYPE):
            with self.subTest(member_type=member_type):
                member = tarfile.TarInfo("sample-1.0.0/linked")
                member.type, member.linkname = member_type, "src/lib.rs"
                destination = self.root / f"vendor-{member_type.decode()}"
                with self.assertRaisesRegex(ValueError, "linked, special"):
                    bundle.materialize_archive(self.archive, destination,
                                               "sample", "1.0.0", self.write_archive(extra=member))

    def test_materialize_rejects_duplicate_members(self) -> None:
        with self.assertRaisesRegex(ValueError, "duplicate archive member"):
            self.materialize(self.write_archive(duplicate=True))

    def test_materialize_rejects_reserved_checksum_file(self) -> None:
        member = tarfile.TarInfo("sample-1.0.0/.cargo-checksum.json")
        with self.assertRaisesRegex(ValueError, "reserved Cargo"):
            self.materialize(self.write_archive(extra=member))

    def test_config_preserves_alias_net_options_and_later_tables(self) -> None:
        original = ('[alias]\nxtask = "run --package xtask --"\n\n[net]\n'
                    'git-fetch-with-cli = true\noffline = true\n\n[build]\njobs = 2\n'
                    '[source.crates-io]\nreplace-with = "rootlight-local-sources"\n'
                    '[source.rootlight-local-sources]\ndirectory = "source-inputs/cargo"\n')
        self.assertEqual(bundle.bundle_cargo_config(original), original)
        config = tomllib.loads(bundle.bundle_cargo_config(original))
        self.assertEqual(config["alias"], tomllib.loads(original)["alias"])
        self.assertEqual(config["net"], {"git-fetch-with-cli": True, "offline": True})
        self.assertEqual(config["build"], {"jobs": 2})
        self.assertEqual(config["source"]["rootlight-local-sources"]["directory"],
                         "source-inputs/cargo")

    def test_config_rejects_missing_offline_and_foreign_source_mappings(self) -> None:
        valid = self._local_config().decode()
        invalid = (
            "[alias]\nx = 'y'\n", "[net]\noffline = true\n",
            valid.replace("offline = true", "offline = false"),
            valid.replace("offline = true\n", ""),
            valid.replace("source-inputs/cargo", "../external"),
            valid.replace("source-inputs/cargo", "/external"),
            valid.replace("rootlight-local-sources", "foreign"),
            valid + "registry = 'https://registry.invalid'\n",
            valid + "[source.foreign]\nregistry = 'https://registry.invalid'\n",
        )
        for original in invalid:
            with self.subTest(original=original), self.assertRaisesRegex(ValueError, "project-local source"):
                bundle.bundle_cargo_config(original)

    def test_portable_paths_reject_absolute_escaping_and_windows_aliases(self) -> None:
        for path in ("", "/root", "a/../b", "a//b", "./a", "C:/a", "a\\b", "a\x00b",
                     "NUL.txt", "src/trailing.", "src/trailing ", "a\nb"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                bundle.relative_path(path)
        self.assertEqual(bundle.relative_path("src/café.rs"), Path("src/café.rs"))

    def test_git_allowlist_rejects_links_submodules_and_unmerged_inputs(self) -> None:
        for mode, stage in (("120000", "0"), ("160000", "0"), ("100644", "1")):
            encoded = f"{mode} {'a' * 40} {stage}\tfile\x00".encode()
            with patch.object(bundle.subprocess, "check_output", return_value=encoded):
                with self.assertRaisesRegex(ValueError, "unmerged, symlink, or submodule"):
                    bundle.tracked_files(self.root)

    def test_git_allowlist_retains_authoritative_execute_mode(self) -> None:
        encoded = (f"100644 {'a' * 40} 0\tsrc/lib.rs\x00"
                   f"100755 {'b' * 40} 0\tscripts/run.sh\x00").encode()
        with patch.object(bundle.subprocess, "check_output", return_value=encoded):
            self.assertEqual(bundle.tracked_files(self.root),
                             [("scripts/run.sh", True), ("src/lib.rs", False)])

    def test_archive_is_reproducible_and_uses_manifest_modes_not_host_modes(self) -> None:
        source = self.root / "project"
        source.mkdir()
        (source / "tool").write_bytes(b"executable input\n")
        (source / "B").write_bytes(b"uppercase input\n")
        (source / "a").write_bytes(b"lowercase input\n")
        (source / bundle.MANIFEST_PATH).write_bytes(b"{}\n")
        manifest = {"file_modes": {"tool": 0o755, "B": 0o644, "a": 0o644}}
        first, second = self.root / "first.tar.gz", self.root / "second.tar.gz"
        with patch.object(bundle, "verify_project_bundle", return_value=manifest):
            bundle.archive_bundle(source, first)
            os.utime(source / "tool", (10, 10))
            bundle.archive_bundle(source, second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with tarfile.open(first) as tar:
            self.assertEqual(tar.getnames(), sorted(tar.getnames()))
            tool = tar.getmember("rootlight/tool")
            self.assertEqual((tool.mode, tool.uid, tool.gid, tool.mtime), (0o755, 0, 0, 0))
        with patch.object(bundle, "verify_project_bundle", return_value=manifest):
            with self.assertRaises(FileExistsError):
                bundle.archive_bundle(source, first)

    def _local_config(self) -> bytes:
        return (b"[alias]\nx = 'y'\n[net]\ngit-fetch-with-cli = true\noffline = true\n"
                b"[source.crates-io]\nreplace-with = 'rootlight-local-sources'\n"
                b"[source.rootlight-local-sources]\ndirectory = 'source-inputs/cargo'\n")

    def create_checkout(self, name: str = "checkout") -> tuple[Path, list[tuple[str, bool]]]:
        checksum = self.write_archive()
        source = self.root / name
        source.mkdir()
        lock = ('version = 4\n\n[[package]]\nname = "sample"\nversion = "1.0.0"\n'
                f'source = "{bundle.REGISTRY}"\nchecksum = "{checksum}"\n').encode()
        inputs = {name: lock for name in bundle.LOCK_PATHS}
        inputs[".cargo/config.toml"] = self._local_config()
        inputs["scripts/run.sh"] = b"#!/bin/sh\nexit 0\n"
        for relative, content in inputs.items():
            output = source / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(content)
        acquired = self.root / f"{name}-archives"
        acquired.mkdir()
        (acquired / self.archive.name).write_bytes(self.archive.read_bytes())
        vendor.vendor_sources(source, acquired)
        self.add_npm_inputs(source)
        # A Windows Git index may mark all vendor files non-executable; the
        # original archives, not those host/index bits, remain authoritative.
        inventory = sorted((path.relative_to(source).as_posix(), path.name == "run.sh")
                           for path in source.rglob("*") if path.is_file())
        return source, inventory

    def add_npm_inputs(self, source: Path) -> None:
        def encode(document):
            return (json.dumps(document, sort_keys=True, indent=2) + "\n").encode("utf-8")

        def sri(payload):
            return "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii")

        packages = {}
        payloads = {}
        for name, version in (("fixture-lib", "1.0.0"), ("support-lib", "2.0.0")):
            payload = npm_archive(name, version)
            resolved = f"https://registry.npmjs.org/{name}/-/{name}-{version}.tgz"
            packages["node_modules/" + name] = {"version": version, "resolved": resolved,
                                                  "integrity": sri(payload), "license": "MIT"}
            payloads[original_npm.archive_filename(resolved)] = payload
        packages["node_modules/support-lib"]["optional"] = True
        packages["node_modules/support-lib"]["os"] = ["linux"]
        packages["node_modules/support-lib/node_modules/fixture-lib"] = dict(packages["node_modules/fixture-lib"])
        package = {"name": "authored-ui", "version": "1.0.0", "dependencies": {"fixture-lib": "1.0.0", "support-lib": "2.0.0"}}
        upstream = {"name": package["name"], "version": package["version"], "lockfileVersion": 3,
                    "packages": {"": package, **packages}}
        upstream_path = source / npm.UPSTREAM_LOCK
        upstream_path.parent.mkdir(parents=True)
        upstream_path.write_bytes(encode(upstream))
        prepared = self.root / (source.name + "-npm-prepared")
        prepared.mkdir()
        for name, payload in payloads.items():
            (prepared / name).write_bytes(payload)
        patch_output = self.root / (source.name + "-npm-patched")
        patch_output.mkdir()
        fixture.patch_fixture_archive(
            upstream_path, prepared, "fixture-lib", "1.0.0", "authored/tests/string.test.js",
            hashlib.sha256(b"prefix original fixture suffix").hexdigest(), len(b"original fixture"),
            hashlib.sha256(b"original fixture").hexdigest(), b"replacement fixture", patch_output,
            synthetic_replacement_confirmed=True,
        )
        archive_path = npm.LOCAL_DISTRIBUTIONS + "/fixture-lib.tgz"
        provenance_path = npm.LOCAL_DISTRIBUTIONS + "/fixture-lib.json"
        (source / npm.LOCAL_DISTRIBUTIONS).mkdir()
        (source / archive_path).write_bytes((patch_output / "patched.tgz").read_bytes())
        (source / provenance_path).write_bytes((patch_output / "patch-provenance.json").read_bytes())
        (source / npm.ORIGINAL_ARCHIVES).mkdir()
        identities = original_npm._locked_archives(upstream)
        for identity in identities:
            if identity["name"] != "fixture-lib":
                filename = identity["archive_filename"]
                (source / npm.ORIGINAL_ARCHIVES / filename).write_bytes(payloads[filename])
        manifest = {"schema": "rootlight.npm-local-distributions/1", "upstream_lock_sha256": bundle.digest(upstream_path),
                    "variants": [{"name": "fixture-lib", "version": "1.0.0", "archive_path": archive_path,
                                  "archive_sha256": bundle.digest(source / archive_path), "provenance_path": provenance_path,
                                  "provenance_sha256": bundle.digest(source / provenance_path)}]}
        (source / npm.DISTRIBUTIONS_MANIFEST).write_bytes(encode(manifest))
        canonical = copy.deepcopy(upstream)
        for identity in identities:
            path = archive_path if identity["name"] == "fixture-lib" else npm.ORIGINAL_ARCHIVES + "/" + identity["archive_filename"]
            for location in identity["lock_paths"]:
                canonical["packages"][location]["resolved"] = "file:../../../" + path
                if identity["name"] == "fixture-lib":
                    canonical["packages"][location]["integrity"] = sri((source / path).read_bytes())
        frontend = (source / npm.CANONICAL_LOCK).parent
        frontend.mkdir(parents=True)
        (frontend / "package.json").write_bytes(encode(package))
        (frontend / ".npmrc").write_bytes(b"audit-level=high\nfund=false\npackage-lock=true\n")
        (source / npm.CANONICAL_LOCK).write_bytes(encode(canonical))
        (source / bundle.NPM_REPORT_PATH).write_bytes(encode(npm.verify_local_bundle(source)))

    def export_project(self, source: Path, inventory: list[tuple[str, bool]],
                       destination: Path | None = None) -> tuple[Path, dict]:
        destination = destination or self.root / "portable-project"
        empty_home = self.root / "empty-cargo-home"
        empty_home.mkdir(exist_ok=True)

        def provenance_only(command, **_options):
            self.assertEqual(command, ["git", "diff", "--quiet", "HEAD", "--"])
            result = unittest.mock.Mock()
            result.returncode = 1
            return result

        with patch.object(bundle, "tracked_files", return_value=inventory), \
            patch.object(bundle.subprocess, "check_output", return_value="a" * 40 + "\n"), \
            patch.object(bundle.subprocess, "run", side_effect=provenance_only) as run, \
            patch.dict(os.environ, {"CARGO_HOME": str(empty_home)}), \
            patch.object(bundle, "materialize_archive", side_effect=AssertionError("export must not unpack")):
            manifest = bundle.create_bundle(source, destination, allow_network=False)
            self.assertEqual(run.call_count, 1)
            self.assertEqual(run.call_args.args[0], ["git", "diff", "--quiet", "HEAD", "--"])
        self.assertEqual(list(empty_home.iterdir()), [])
        return destination, manifest

    def create_project(self) -> tuple[Path, dict]:
        return self.export_project(*self.create_checkout())

    def test_complete_project_bundle_keeps_original_locks_and_archive_provenance(self) -> None:
        project, manifest = self.create_project()
        self.assertEqual(manifest["cargo_source_closure"]["package_count"], 1)
        self.assertTrue(manifest["cargo_source_closure"]["archives_verified"])
        self.assertFalse(manifest["project_sources_match_revision"])
        self.assertFalse(manifest["frontend_external_source_closure_verified"])
        self.assertEqual(manifest["scope"], bundle.BUNDLE_SCOPE)
        self.assertIs(manifest["npm_distribution_inputs_verified"], True)
        self.assertIsNone(manifest["lightning_producer_inputs"])
        self.assertIs(manifest["lightning_producer_inputs_verified"], False)
        self.assertEqual(manifest["source_input_empty_directories"], {})
        npm_inputs = manifest["npm_distribution_inputs"]
        self.assertEqual((npm_inputs["archive_count"], npm_inputs["lock_record_count"],
                          npm_inputs["original_archive_count"], npm_inputs["local_distribution_count"]),
                         (2, 3, 1, 1))
        self.assertFalse(npm_inputs["upstream_original_sri_recomputed"])
        for field in ("installation_verified", "build_and_packaging_acceptance_verified",
                      "os_denied_acceptance_verified", "frontend_source_complete"):
            self.assertIs(npm_inputs[field], False)
        self.assertEqual(bundle.verify_project_bundle(project), manifest)
        self.assertEqual((project / bundle.ARCHIVES_PATH / self.archive.name).read_bytes(),
                         self.archive.read_bytes())
        self.assertEqual((project / ".cargo/config.toml").read_bytes(), self._local_config())
        paths = [record["path"] for record in manifest["project_sources"]]
        self.assertEqual(len(paths), len(set(paths)))
        self.assertIn(bundle.CLOSURE_PATH, paths)
        self.assertIn(bundle.NPM_REPORT_PATH, paths)
        self.assertTrue(set(bundle.FRONTEND_PATHS).issubset(paths))
        self.assertTrue({item["path"] for item in npm_inputs["input_files"]}.issubset(paths))
        self.assertEqual(npm.verify_local_bundle(project), npm_inputs)
        for record in npm_inputs["input_files"]:
            self.assertEqual(bundle.digest(project / record["path"]), record["sha256"])
        self.assertIn(f"{bundle.ARCHIVES_PATH}/{self.archive.name}", paths)
        self.assertIn(f"{bundle.VENDOR_PATH}/sample-1.0.0/.cargo-checksum.json", paths)
        self.assertNotIn(bundle.MANIFEST_PATH, paths)
        self.assertEqual(manifest["cargo_config_sha256"], bundle.digest(project / ".cargo/config.toml"))
        tool = f"{bundle.VENDOR_PATH}/sample-1.0.0/bin/tool"
        self.assertEqual(manifest["file_modes"][tool], 0o755)
        self.assertTrue(next(record for record in manifest["project_sources"]
                             if record["path"] == tool)["executable"])

    def test_missing_versioned_archive_rejects_before_export_without_cache_fallback(self) -> None:
        source, inventory = self.create_checkout()
        (source / bundle.ARCHIVES_PATH / self.archive.name).unlink()
        populated = self.root / "prefetched/registry/cache/registry-fixture"
        populated.mkdir(parents=True)
        (populated / self.archive.name).write_bytes(self.archive.read_bytes())
        destination = self.root / "rejected"
        with patch.dict(os.environ, {"CARGO_HOME": str(populated.parents[2])}), \
                patch.object(bundle, "tracked_files", return_value=inventory), \
                patch.object(bundle.subprocess, "run", side_effect=AssertionError), \
                patch.object(bundle.subprocess, "check_output", side_effect=AssertionError):
            with self.assertRaisesRegex(ValueError, "missing archives"):
                bundle.create_bundle(source, destination, allow_network=False)
        self.assertFalse(destination.exists())

    def test_missing_versioned_source_or_report_rejects_before_export(self) -> None:
        for index, missing in enumerate((f"{bundle.VENDOR_PATH}/sample-1.0.0/src/lib.rs",
                                         bundle.CLOSURE_PATH)):
            with self.subTest(missing=missing):
                source, inventory = self.create_checkout(f"missing-{index}")
                (source / missing).unlink()
                destination = self.root / f"rejected-{index}"
                with patch.object(bundle, "tracked_files", return_value=inventory):
                    with self.assertRaises(ValueError):
                        bundle.create_bundle(source, destination, allow_network=False)
                self.assertFalse(destination.exists())

    def test_tampered_versioned_inputs_reject_before_export(self) -> None:
        cases = (f"{bundle.VENDOR_PATH}/sample-1.0.0/src/lib.rs",
                 f"{bundle.ARCHIVES_PATH}/{self.archive.name}", bundle.CLOSURE_PATH)
        for index, changed in enumerate(cases):
            with self.subTest(changed=changed):
                source, inventory = self.create_checkout(f"changed-{index}")
                path = source / changed
                path.write_bytes(path.read_bytes() + b"tampered")
                destination = self.root / f"rejected-{index}"
                with patch.object(bundle, "tracked_files", return_value=inventory):
                    with self.assertRaises(ValueError):
                        bundle.create_bundle(source, destination, allow_network=False)
                self.assertFalse(destination.exists())

    def test_untracked_vendor_source_and_dependency_report_are_not_exported(self) -> None:
        source, inventory = self.create_checkout()
        for missing in (f"{bundle.VENDOR_PATH}/sample-1.0.0/src/lib.rs", bundle.CLOSURE_PATH):
            with self.subTest(missing=missing), patch.object(bundle, "tracked_files", return_value=[
                record for record in inventory if record[0] != missing
            ]):
                destination = self.root / "rejected"
                with self.assertRaisesRegex(ValueError, "untracked"):
                    bundle.create_bundle(source, destination, allow_network=False)
                self.assertFalse(destination.exists())

    def test_every_npm_input_and_authored_frontend_file_must_be_tracked(self) -> None:
        source, inventory = self.create_checkout()
        report = npm.verify_local_bundle(source)
        required = {item["path"] for item in report["input_files"]} | {
            bundle.NPM_REPORT_PATH, *bundle.FRONTEND_PATHS}
        for missing in sorted(required):
            with self.subTest(missing=missing), patch.object(bundle, "tracked_files", return_value=[
                record for record in inventory if record[0] != missing
            ]):
                destination = self.root / "rejected"
                with self.assertRaisesRegex(ValueError, "untracked"):
                    bundle.create_bundle(source, destination, allow_network=False)
                self.assertFalse(destination.exists())

    def test_missing_npm_and_frontend_inputs_reject_without_cache_or_process_fallback(self) -> None:
        source, inventory = self.create_checkout()
        report = npm.verify_local_bundle(source)
        required = {item["path"] for item in report["input_files"]} | {
            bundle.NPM_REPORT_PATH, *bundle.FRONTEND_PATHS}
        for missing in sorted(required):
            path = source / missing
            payload = path.read_bytes()
            path.unlink()
            with self.subTest(missing=missing), patch.object(bundle, "tracked_files", return_value=inventory), \
                    patch.object(bundle.subprocess, "run", side_effect=AssertionError("no process fallback")), \
                    patch.object(bundle.subprocess, "check_output", side_effect=AssertionError("no process fallback")):
                destination = self.root / "rejected"
                with self.assertRaises(ValueError):
                    bundle.create_bundle(source, destination, allow_network=False)
                self.assertFalse(destination.exists())
            path.write_bytes(payload)

    def test_tampered_npm_report_canonical_upstream_manifest_provenance_and_archives_reject(self) -> None:
        source, inventory = self.create_checkout()
        report = npm.verify_local_bundle(source)
        required = {item["path"] for item in report["input_files"]} | {bundle.NPM_REPORT_PATH}
        for changed in sorted(required):
            path = source / changed
            payload = path.read_bytes()
            path.write_bytes(payload + b"tampered")
            with self.subTest(changed=changed), patch.object(bundle, "tracked_files", return_value=inventory), \
                    patch.object(bundle.subprocess, "run", side_effect=AssertionError("no process fallback")), \
                    patch.object(bundle.subprocess, "check_output", side_effect=AssertionError("no process fallback")):
                destination = self.root / "rejected"
                with self.assertRaises(ValueError):
                    bundle.create_bundle(source, destination, allow_network=False)
                self.assertFalse(destination.exists())
            path.write_bytes(payload)

    def test_npm_dependency_report_cannot_expand_verification_claims(self) -> None:
        source, inventory = self.create_checkout()
        path = source / bundle.NPM_REPORT_PATH
        original = path.read_bytes()
        for field in ("frontend_source_complete", "installation_verified", "os_denied_acceptance_verified",
                      "upstream_original_sri_recomputed", "archive_count"):
            report = json.loads(original)
            report[field] = True if field != "archive_count" else 9
            path.write_bytes((json.dumps(report, sort_keys=True, indent=2) + "\n").encode())
            with self.subTest(field=field), patch.object(bundle, "tracked_files", return_value=inventory):
                with self.assertRaisesRegex(ValueError, "npm distribution report differs"):
                    bundle.create_bundle(source, self.root / "rejected", allow_network=False)
            self.assertFalse((self.root / "rejected").exists())

    def test_export_rechecks_both_source_and_copied_npm_archives(self) -> None:
        source, inventory = self.create_checkout()
        report = npm.verify_local_bundle(source)
        archive_path = next(item["archive_path"] for item in report["packages"]
                            if item["distribution_kind"] == "locally_modified_distribution")
        digest = bundle.digest
        for changed_root in ("source", "destination"):
            destination = self.root / ("changed-" + changed_root)
            changed = False
            original_bytes = (source / archive_path).read_bytes()

            def mutate(path):
                nonlocal changed
                result = digest(path)
                if Path(path) == destination / archive_path and not changed:
                    changed = True
                    target = source / archive_path if changed_root == "source" else destination / archive_path
                    target.write_bytes(target.read_bytes() + b"changed after copying")
                return result

            with self.subTest(changed_root=changed_root), patch.object(bundle, "digest", side_effect=mutate), \
                    self.assertRaises(ValueError):
                self.export_project(source, inventory, destination)
            self.assertTrue(changed)
            self.assertTrue(destination.exists())
            self.assertFalse((destination / bundle.MANIFEST_PATH).exists())
            (source / archive_path).write_bytes(original_bytes)

    def test_export_rejects_acquisition_flag_without_inspecting_or_writing_inputs(self) -> None:
        destination = self.root / "rejected"
        with patch.object(bundle, "tracked_files", side_effect=AssertionError), \
                patch.object(bundle.subprocess, "run", side_effect=AssertionError), \
                patch.object(bundle.subprocess, "check_output", side_effect=AssertionError):
            with self.assertRaisesRegex(ValueError, "inappropriate for export"):
                bundle.create_bundle(self.root, destination, allow_network=True)
        self.assertFalse(destination.exists())

    def test_cli_rejects_acquisition_flag_even_in_verify_only_mode(self) -> None:
        output, errors = io.StringIO(), io.StringIO()
        arguments = ["prepare_rust_source_bundle.py", "--destination", str(self.root / "rejected"),
                     "--verify-only", "--allow-network"]
        with patch("sys.argv", arguments), contextlib.redirect_stdout(output), \
                contextlib.redirect_stderr(errors), \
                patch.object(bundle, "verify_project_bundle", side_effect=AssertionError), \
                patch.object(bundle.subprocess, "run", side_effect=AssertionError):
            with self.assertRaises(SystemExit) as failure:
                bundle.main()
        self.assertEqual(failure.exception.code, 1)
        self.assertEqual(output.getvalue(), "")
        self.assertIn("inappropriate for export", errors.getvalue())
        self.assertFalse((self.root / "rejected").exists())

    def test_foreign_configuration_rejects_before_export(self) -> None:
        source, inventory = self.create_checkout()
        path = source / ".cargo/config.toml"
        path.write_bytes(path.read_bytes().replace(b"offline = true", b"offline = false"))
        with patch.object(bundle, "tracked_files", return_value=inventory):
            with self.assertRaisesRegex(ValueError, "project-local source"):
                bundle.create_bundle(source, self.root / "rejected", allow_network=False)
        self.assertFalse((self.root / "rejected").exists())

    def test_dependency_report_rejects_expanded_claims_and_changed_mode_evidence(self) -> None:
        source, inventory = self.create_checkout()
        path = source / bundle.CLOSURE_PATH
        original = path.read_bytes()
        for field in ("frontend_external_source_closure_verified", "file_modes"):
            with self.subTest(field=field):
                report = json.loads(original)
                if field == "file_modes":
                    report[field][f"{bundle.VENDOR_PATH}/sample-1.0.0/bin/tool"] = 0o644
                else:
                    report[field] = True
                path.write_text(json.dumps(report, sort_keys=True, indent=2) + "\n",
                                encoding="utf-8", newline="\n")
                with patch.object(bundle, "tracked_files", return_value=inventory):
                    with self.assertRaisesRegex(ValueError, "dependency report differs"):
                        bundle.create_bundle(source, self.root / "rejected", allow_network=False)
                self.assertFalse((self.root / "rejected").exists())

    def test_tracked_generated_manifest_is_rejected_without_copy_collision(self) -> None:
        source, inventory = self.create_checkout()
        (source / bundle.MANIFEST_PATH).write_bytes(b"old generated manifest")
        with patch.object(bundle, "tracked_files", return_value=[*inventory, (bundle.MANIFEST_PATH, False)]):
            with self.assertRaisesRegex(ValueError, "reserved tracked"):
                bundle.create_bundle(source, self.root / "rejected", allow_network=False)
        self.assertFalse((self.root / "rejected").exists())

    def test_project_bundle_rejects_extra_and_changed_project_sources(self) -> None:
        project, _manifest = self.create_project()
        extra = project / "unreviewed.txt"
        extra.write_bytes(b"not in source inventory")
        with self.assertRaisesRegex(ValueError, "unmanifested project source"):
            bundle.verify_project_bundle(project)
        extra.unlink()
        (project / "Cargo.lock").write_bytes(b"changed lock")
        with self.assertRaisesRegex(ValueError, "changed project source"):
            bundle.verify_project_bundle(project)

    def test_restored_dependency_directories_have_no_allowlist_exemptions(self) -> None:
        project, _manifest = self.create_project()
        for relative in (f"{bundle.VENDOR_PATH}/sample-1.0.0/unreviewed",
                         f"{bundle.ARCHIVES_PATH}/unreviewed.crate",
                         f"{npm.ORIGINAL_ARCHIVES}/unreviewed.tgz",
                         f"{npm.LOCAL_DISTRIBUTIONS}/unreviewed.json",
                         "apps/rootlight-web/frontend/node_modules/unreviewed/index.js",
                         "apps/rootlight-web/frontend/dist/unreviewed.js"):
            path = project / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"not a tracked input")
            with self.subTest(relative=relative), self.assertRaisesRegex(ValueError, "unmanifested project source"):
                bundle.verify_project_bundle(project)
            path.unlink()

    def test_restored_npm_contract_is_independent_of_project_checksum_rebinding(self) -> None:
        project, manifest = self.create_project()
        report = manifest["npm_distribution_inputs"]
        inputs = {item["path"] for item in report["input_files"]} | {bundle.NPM_REPORT_PATH}
        pristine_manifest = copy.deepcopy(manifest)
        for changed in sorted(inputs):
            path = project / changed
            before = path.read_bytes()
            path.write_bytes(before + b"tampered")
            manifest = copy.deepcopy(pristine_manifest)
            record = next(item for item in manifest["project_sources"] if item["path"] == changed)
            record["sha256"] = bundle.digest(path)
            (project / bundle.MANIFEST_PATH).write_bytes(json.dumps(manifest).encode())
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                bundle.verify_project_bundle(project)
            path.write_bytes(before)

    def test_restored_manifest_requires_npm_source_witness_and_false_frontend_claims(self) -> None:
        project, manifest = self.create_project()
        pristine = copy.deepcopy(manifest)
        changes = (
            lambda: manifest.update(scope="rust_workspace_native_grammars_and_fuzz"),
            lambda: manifest.update(npm_distribution_inputs_verified=False),
            lambda: manifest.update(npm_distribution_inputs_verified=1),
            lambda: manifest.update(frontend_external_source_closure_verified=True),
            lambda: manifest["npm_distribution_inputs"].update(installation_verified=True),
            lambda: manifest["npm_distribution_inputs"].update(archive_count=99),
            lambda: manifest.pop("npm_distribution_inputs"),
        )
        for change in changes:
            manifest = copy.deepcopy(pristine)
            change()
            (project / bundle.MANIFEST_PATH).write_bytes(json.dumps(manifest).encode())
            with self.subTest(change=changes.index(change)), self.assertRaises(ValueError):
                bundle.verify_project_bundle(project)

    def test_restored_npm_sources_cannot_be_removed_from_the_project_allowlist(self) -> None:
        project, manifest = self.create_project()
        pristine = copy.deepcopy(manifest)
        for omitted in (bundle.NPM_REPORT_PATH, npm.UPSTREAM_LOCK, npm.DISTRIBUTIONS_MANIFEST,
                        *manifest["npm_distribution_inputs"]["archive_paths"],
                        *manifest["npm_distribution_inputs"]["provenance_paths"], *bundle.FRONTEND_PATHS):
            manifest = copy.deepcopy(pristine)
            manifest["project_sources"] = [record for record in manifest["project_sources"] if record["path"] != omitted]
            (project / bundle.MANIFEST_PATH).write_bytes(json.dumps(manifest).encode())
            with self.subTest(omitted=omitted), self.assertRaises(ValueError):
                bundle.verify_project_bundle(project)

    def test_npm_data_modes_are_authoritative_and_authored_git_modes_are_preserved(self) -> None:
        source, inventory = self.create_checkout()
        inventory = [(name, True if name.startswith("source-inputs/npm/") or name.endswith("package.json") else executable)
                     for name, executable in inventory]
        project, manifest = self.export_project(source, inventory)
        for name, _executable in inventory:
            if name.startswith("source-inputs/npm/"):
                self.assertEqual(manifest["file_modes"][name], 0o644)
                self.assertIs(next(record["executable"] for record in manifest["project_sources"] if record["path"] == name), False)
        authored = "apps/rootlight-web/frontend/package.json"
        self.assertEqual(manifest["file_modes"][authored], 0o755)
        self.assertIs(next(record["executable"] for record in manifest["project_sources"] if record["path"] == authored), True)
        self.assertEqual(bundle.verify_project_bundle(project), manifest)

    def test_npm_data_execute_claims_cannot_be_rebound_in_restored_manifest(self) -> None:
        project, manifest = self.create_project()
        pristine = copy.deepcopy(manifest)
        for name in (bundle.NPM_REPORT_PATH, npm.UPSTREAM_LOCK, npm.DISTRIBUTIONS_MANIFEST,
                     *manifest["npm_distribution_inputs"]["archive_paths"],
                     *manifest["npm_distribution_inputs"]["provenance_paths"]):
            manifest = copy.deepcopy(pristine)
            manifest["file_modes"][name] = 0o755
            next(record for record in manifest["project_sources"] if record["path"] == name)["executable"] = True
            (project / bundle.MANIFEST_PATH).write_bytes(json.dumps(manifest).encode())
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "authenticated npm inputs"):
                bundle.verify_project_bundle(project)

    @unittest.skipIf(os.name == "nt", "POSIX permission modes are not exposed by the Windows filesystem")
    def test_restored_npm_data_requires_exact_644_not_just_absence_of_execute_bits(self) -> None:
        project, _manifest = self.create_project()
        path = project / bundle.NPM_REPORT_PATH
        path.chmod(0o600)
        with self.assertRaisesRegex(ValueError, "restored npm data modes"):
            bundle.verify_project_bundle(project)

    def test_restored_dependency_input_cannot_be_removed_from_project_manifest(self) -> None:
        project, manifest = self.create_project()
        omitted = f"{bundle.VENDOR_PATH}/sample-1.0.0/src/lib.rs"
        manifest["project_sources"] = [record for record in manifest["project_sources"]
                                       if record["path"] != omitted]
        (project / bundle.MANIFEST_PATH).write_text(json.dumps(manifest), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "unmanifested project source"):
            bundle.verify_project_bundle(project)

    def test_exported_archive_contains_every_tracked_input_once_with_archive_modes(self) -> None:
        project, manifest = self.create_project()
        archive = self.root / "complete.tar.gz"
        bundle.archive_bundle(project, archive)
        with tarfile.open(archive) as tar:
            names = tar.getnames()
            expected = {f'rootlight/{record["path"]}' for record in manifest["project_sources"]}
            expected.add(f"rootlight/{bundle.MANIFEST_PATH}")
            self.assertEqual(set(names), expected)
            self.assertEqual(len(names), len(expected))
            self.assertEqual(tar.getmember(f"rootlight/{bundle.VENDOR_PATH}/sample-1.0.0/bin/tool").mode,
                             0o755)
            original = tar.extractfile(f"rootlight/{bundle.ARCHIVES_PATH}/{self.archive.name}")
            self.assertIsNotNone(original)
            with original:
                self.assertEqual(original.read(), self.archive.read_bytes())

    def test_project_bundle_rejects_changed_authoritative_executable_modes(self) -> None:
        project, manifest = self.create_project()
        manifest["file_modes"]["source-inputs/cargo/sample-1.0.0/bin/tool"] = 0o644
        (project / bundle.MANIFEST_PATH).write_text(json.dumps(manifest), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "executable modes"):
            bundle.verify_project_bundle(project)

    def test_existing_destination_is_preserved(self) -> None:
        destination = self.root / "existing"
        destination.mkdir()
        sentinel = destination / "retain"
        sentinel.write_bytes(b"existing data")
        with self.assertRaisesRegex(ValueError, "already exists"):
            bundle.create_bundle(self.root, destination, allow_network=False)
        self.assertEqual(sentinel.read_bytes(), b"existing data")

    def test_absent_producer_requires_explicit_null_evidence_and_false_verified_flag(self) -> None:
        project, pristine = self.create_project()
        self.assertEqual(bundle._producer_source_inputs(project), (None, {}, {}, set()))
        for field, value in (("lightning_producer_inputs", {}), ("lightning_producer_inputs_verified", True),
                             ("lightning_producer_inputs_verified", 0)):
            manifest = copy.deepcopy(pristine)
            manifest[field] = value
            (project / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(manifest))
            with self.subTest(field=field, value=value), self.assertRaisesRegex(ValueError, "producer source inputs"):
                bundle.verify_project_bundle(project)
        manifest = copy.deepcopy(pristine)
        del manifest["lightning_producer_inputs"]
        (project / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(manifest))
        with self.assertRaisesRegex(ValueError, "producer source inputs"):
            bundle.verify_project_bundle(project)


class ProducerSourceExportTests(AuthoredFixture, unittest.TestCase):
    """Exercise real producer verification on complete authored inert inputs."""

    def setUp(self) -> None:
        RustSourceBundleTests.setUp(self)
        AuthoredFixture.setUp(self)
        empty = tarfile.TarInfo(producer.ROOT + "/documentation/empty")
        empty.type, empty.mode = tarfile.DIRTYPE, 0o775
        self.payload = producer_archive(self.files, [empty])
        pin = patch.object(producer, "ARCHIVE_SHA256", producer.digest(self.payload))
        pin.start()
        self.addCleanup(pin.stop)
        self.write_prepared()
        producer_import.import_bundle(self.project, self.prepared, self.capture, self.destination)
        checksum = RustSourceBundleTests.write_archive(self)
        lock = ('version = 4\n[[package]]\nname = "sample"\nversion = "1.0.0"\n'
                f'source = "{bundle.REGISTRY}"\nchecksum = "{checksum}"\n').encode()
        for name, content in {**{name: lock for name in bundle.LOCK_PATHS},
                              ".cargo/config.toml": RustSourceBundleTests._local_config(self)}.items():
            path = self.project / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        retained = self.project / bundle.ARCHIVES_PATH
        retained.mkdir()
        archive = retained / self.archive.name
        archive.write_bytes(self.archive.read_bytes())
        modes = bundle.materialize_archive(archive, self.project / bundle.VENDOR_PATH,
                                           "sample", "1.0.0", checksum)
        modes[f"{bundle.ARCHIVES_PATH}/{self.archive.name}"] = 0o644
        closure = bundle.verify_bundle(self.project / bundle.VENDOR_PATH,
                                       [self.project / name for name in bundle.LOCK_PATHS], archives=retained)
        report = {**closure, "file_modes": dict(sorted(modes.items())),
                  "frontend_external_source_closure_verified": False,
                  "build_and_packaging_acceptance_verified": False}
        (self.project / bundle.CLOSURE_PATH).write_bytes(producer.json_bytes(report))
        frontend = (self.project / npm.CANONICAL_LOCK).parent
        package = json.loads((self.project / npm.CANONICAL_LOCK).read_bytes())["packages"][""]
        (frontend / "package.json").write_bytes(producer.json_bytes(package))
        (frontend / ".npmrc").write_bytes(b"fund=false\npackage-lock=true\n")
        self.inventory = sorted((path.relative_to(self.project).as_posix(), False)
                                for path in self.project.rglob("*") if path.is_file()
                                and self.prepared not in path.parents and path != self.capture)

    def export(self, *, inventory=None, destination=None):
        return RustSourceBundleTests.export_project(
            self, self.project, self.inventory if inventory is None else inventory, destination)

    def test_complete_producer_bytes_modes_reports_and_empty_directories_survive_transport(self) -> None:
        before = {name: (self.project / name).read_bytes() for name, _mode in self.inventory}
        exported, manifest = self.export()
        self.assertIs(manifest["lightning_producer_inputs_verified"], True)
        evidence = producer.verify_bundle(exported, exported / bundle.LIGHTNING_PRODUCER_PATH)
        self.assertEqual(manifest["lightning_producer_inputs"], evidence)
        for key in producer.FALSE_CLAIMS:
            self.assertIs(evidence[key], False)
        self.assertIs(manifest["frontend_external_source_closure_verified"], False)
        for name, payload in before.items():
            self.assertEqual((exported / name).read_bytes(), payload)
            self.assertEqual((self.project / name).read_bytes(), payload)
        tool = bundle.LIGHTNING_PRODUCER_PATH + "/tree/scripts/build.js"
        self.assertEqual(manifest["file_modes"][tool], 0o755)
        self.assertTrue(next(row for row in manifest["project_sources"] if row["path"] == tool)["executable"])
        empty = bundle.LIGHTNING_PRODUCER_PATH + "/tree/documentation/empty"
        self.assertEqual(manifest["source_input_empty_directories"],
                         {empty: 0o755, npm.LOCAL_DISTRIBUTIONS: 0o755})
        self.assertEqual(list((exported / empty).iterdir()), [])
        first, second = self.root / "producer-first.tar.gz", self.root / "producer-second.tar.gz"
        bundle.archive_bundle(exported, first)
        bundle.archive_bundle(exported, second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        restored = self.root / "producer-restored"
        restored.mkdir()
        with tarfile.open(first) as tar:
            names = tar.getnames()
            self.assertEqual(len(names), len(set(names)))
            self.assertEqual(names, sorted(names))
            member = tar.getmember("rootlight/" + empty)
            self.assertTrue(member.isdir())
            self.assertEqual((member.mode, member.uid, member.gid, member.mtime), (0o755, 0, 0, 0))
            self.assertEqual(tar.getmember("rootlight/" + tool).mode, 0o755)
            directories = {item.name for item in tar.getmembers() if item.isdir()}
            self.assertEqual(directories, {"rootlight/" + empty, "rootlight/" + npm.LOCAL_DISTRIBUTIONS})
            files = {item.name for item in tar.getmembers() if item.isfile()}
            expected_files = {"rootlight/" + row["path"] for row in manifest["project_sources"]}
            self.assertEqual(files, expected_files | {"rootlight/" + bundle.MANIFEST_PATH})
            tar.extractall(restored, filter="data")
        self.assertEqual(bundle.verify_project_bundle(restored / "rootlight"), manifest)
        self.assertEqual(list((restored / "rootlight" / empty).iterdir()), [])

    def test_every_producer_file_must_be_tracked_before_destination_creation(self) -> None:
        _report, _modes, hashes, _directories = bundle._producer_source_inputs(self.project)
        controls = (bundle.LIGHTNING_PRODUCER_PATH + "/source-tree.json",
                    bundle.LIGHTNING_PRODUCER_PATH + "/publisher-capture.json",
                    bundle.LIGHTNING_PRODUCER_PATH + "/" + producer.ARCHIVE_NAME,
                    bundle.LIGHTNING_PRODUCER_PATH + "/tree/.gitignore",
                    bundle.LIGHTNING_PRODUCER_PATH + "/tree/selectors/LICENSE")
        self.assertTrue(set(controls).issubset(hashes))
        for index, name in enumerate(controls):
            destination = self.root / f"untracked-producer-{index}"
            inventory = [row for row in self.inventory if row[0] != name]
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "untracked producer"):
                self.export(inventory=inventory, destination=destination)
            self.assertFalse(destination.exists())

    def test_changed_source_and_original_evidence_reject_before_copy(self) -> None:
        controls = ("tree/src/value.rs", "source-tree.json", "publisher-capture.json", producer.ARCHIVE_NAME)
        for index, name in enumerate(controls):
            path = self.destination / name
            original = path.read_bytes()
            path.write_bytes(original + b"changed")
            destination = self.root / f"tampered-producer-{index}"
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.export(destination=destination)
            self.assertFalse(destination.exists())
            path.write_bytes(original)

    def test_present_empty_foreign_partial_or_extra_namespace_never_counts_as_absent(self) -> None:
        parent = self.destination.parent
        foreign = parent / "unreviewed-producer"
        foreign.mkdir()
        with self.assertRaisesRegex(ValueError, "namespace inventory"):
            bundle._producer_source_inputs(self.project)
        foreign.rmdir()
        self.destination.rename(self.root / "saved-producer")
        with self.assertRaisesRegex(ValueError, "namespace inventory"):
            bundle._producer_source_inputs(self.project)
        self.destination.mkdir()
        with self.assertRaisesRegex(ValueError, "stored producer inventory"):
            bundle._producer_source_inputs(self.project)
        self.destination.rmdir()
        (self.root / "saved-producer").rename(self.destination)
        empty = self.destination / "tree/foreign-empty"
        empty.mkdir()
        with self.assertRaisesRegex(ValueError, "source path inventory"):
            bundle._producer_source_inputs(self.project)

    def test_missing_authored_empty_directory_is_not_silently_reconstructed(self) -> None:
        (self.destination / "tree/documentation/empty").rmdir()
        destination = self.root / "missing-empty-directory"
        with self.assertRaisesRegex(ValueError, "source path inventory"):
            self.export(destination=destination)
        self.assertFalse(destination.exists())

    def test_restored_empty_directory_inventory_modes_and_extras_cannot_be_rebound(self) -> None:
        exported, pristine = self.export()
        empty = bundle.LIGHTNING_PRODUCER_PATH + "/tree/documentation/empty"
        manifest_path = exported / bundle.MANIFEST_PATH
        for value in ({}, {empty: 0o755}, {empty: True, npm.LOCAL_DISTRIBUTIONS: 0o755},
                      {empty: 0o700, npm.LOCAL_DISTRIBUTIONS: 0o755},
                      {**pristine["source_input_empty_directories"], "source-inputs/foreign": 0o755}):
            manifest = copy.deepcopy(pristine)
            manifest["source_input_empty_directories"] = value
            manifest_path.write_bytes(producer.json_bytes(manifest))
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "empty directories"):
                bundle.verify_project_bundle(exported)
        manifest_path.write_bytes(producer.json_bytes(pristine))
        (exported / empty).rmdir()
        with self.assertRaisesRegex(ValueError, "source path inventory"):
            bundle.verify_project_bundle(exported)

    def test_restored_evidence_flags_modes_and_rebound_tree_reject(self) -> None:
        exported, original = self.export()
        manifest_path = exported / bundle.MANIFEST_PATH
        for key, value in (("lightning_producer_inputs", None), ("lightning_producer_inputs_verified", False),
                           ("lightning_producer_inputs_verified", 1)):
            manifest = copy.deepcopy(original)
            manifest[key] = value
            manifest_path.write_bytes(producer.json_bytes(manifest))
            with self.subTest(key=key, value=value), self.assertRaisesRegex(ValueError, "producer source inputs"):
                bundle.verify_project_bundle(exported)
        tool = bundle.LIGHTNING_PRODUCER_PATH + "/tree/scripts/build.js"
        manifest = copy.deepcopy(original)
        manifest["file_modes"][tool] = 0o644
        next(row for row in manifest["project_sources"] if row["path"] == tool)["executable"] = False
        manifest_path.write_bytes(producer.json_bytes(manifest))
        with self.assertRaisesRegex(ValueError, "authenticated producer inputs"):
            bundle.verify_project_bundle(exported)
        changed = bundle.LIGHTNING_PRODUCER_PATH + "/tree/src/value.rs"
        (exported / changed).write_bytes(b"rebound source bytes\n")
        manifest = copy.deepcopy(original)
        next(row for row in manifest["project_sources"] if row["path"] == changed)["sha256"] = bundle.digest(exported / changed)
        manifest_path.write_bytes(producer.json_bytes(manifest))
        with self.assertRaisesRegex(ValueError, "stored source bytes"):
            bundle.verify_project_bundle(exported)

    def test_source_mutation_after_initial_producer_verification_is_detected(self) -> None:
        original = bundle._producer_source_inputs
        calls = 0

        def changed_after_verification(root):
            nonlocal calls
            result = original(root)
            calls += 1
            if calls == 1:
                path = self.destination / "tree/src/value.rs"
                path.write_bytes(path.read_bytes() + b"mutation")
            return result

        destination = self.root / "late-producer-mutation"
        with patch.object(bundle, "_producer_source_inputs", side_effect=changed_after_verification):
            with self.assertRaisesRegex(ValueError, "producer source inputs changed before export"):
                self.export(destination=destination)
        self.assertFalse(destination.exists())

    def test_export_rechecks_source_and_copied_producer_tree_before_manifest_publication(self) -> None:
        original = bundle._versioned_closure
        source = self.destination / "tree/src/value.rs"
        before = source.read_bytes()
        for phase in ("source", "copied"):
            destination = self.root / ("producer-post-copy-" + phase)

            def changed_after_copy(root):
                result = original(root)
                if root == destination:
                    target = source if phase == "source" else destination / bundle.LIGHTNING_PRODUCER_PATH / "tree/src/value.rs"
                    target.write_bytes(target.read_bytes() + b"mutation")
                return result

            with self.subTest(phase=phase), patch.object(bundle, "_versioned_closure", side_effect=changed_after_copy):
                with self.assertRaisesRegex(ValueError, "stored source bytes"):
                    self.export(destination=destination)
            self.assertFalse((destination / bundle.MANIFEST_PATH).exists())
            source.write_bytes(before)

    def test_linked_producer_parent_is_not_an_optional_absence(self) -> None:
        original = bundle.linked_path
        parent = self.destination.parent
        with patch.object(bundle, "linked_path", side_effect=lambda path: path == parent or original(path)):
            with self.assertRaisesRegex(ValueError, "linked producer"):
                bundle._producer_source_inputs(self.project)


if __name__ == "__main__":
    unittest.main()
