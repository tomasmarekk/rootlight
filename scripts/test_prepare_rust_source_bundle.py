#!/usr/bin/env python3
"""Exercise source preparation and portable archives without Cargo or downloads."""

import contextlib
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
import vendor_rust_sources as vendor


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
        # A Windows Git index may mark all vendor files non-executable; the
        # original archives, not those host/index bits, remain authoritative.
        inventory = sorted((path.relative_to(source).as_posix(), path.name == "run.sh")
                           for path in source.rglob("*") if path.is_file())
        return source, inventory

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
        self.assertEqual(bundle.verify_project_bundle(project), manifest)
        self.assertEqual((project / bundle.ARCHIVES_PATH / self.archive.name).read_bytes(),
                         self.archive.read_bytes())
        self.assertEqual((project / ".cargo/config.toml").read_bytes(), self._local_config())
        paths = [record["path"] for record in manifest["project_sources"]]
        self.assertEqual(len(paths), len(set(paths)))
        self.assertIn(bundle.CLOSURE_PATH, paths)
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
                         f"{bundle.ARCHIVES_PATH}/unreviewed.crate"):
            path = project / relative
            path.write_bytes(b"not a tracked input")
            with self.subTest(relative=relative), self.assertRaisesRegex(ValueError, "unmanifested project source"):
                bundle.verify_project_bundle(project)
            path.unlink()

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


if __name__ == "__main__":
    unittest.main()
