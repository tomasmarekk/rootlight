#!/usr/bin/env python3
"""Exercise Cargo/npm/producer/Yarn exports using authored fixtures and mocked Git.

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
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
from unittest.mock import patch

import prepare_rust_source_bundle as bundle
import import_lightning_producer_sources as producer_import
import patch_npm_source_fixture as fixture
import verify_lightning_producer_sources as producer
import verify_lightning_yarn_sources as yarn
import verify_local_npm_sources as npm
import verify_npm_source_bundle as original_npm
import vendor_rust_sources as vendor
from test_verify_local_npm_sources import _archive as npm_archive
from test_verify_lightning_producer_sources import AuthoredFixture, archive as producer_archive
from test_verify_lightning_yarn_sources import archive as yarn_archive, pin_fields as yarn_pin_fields


_OBSERVER_SCRIPT_PATH = "scripts/verify_lightning_installed_placements.py"
_LIGHTNING_PLACEMENT_SCRIPT_PATHS = (
    "scripts/install_lightning_from_archives.py",
    _OBSERVER_SCRIPT_PATH,
)


_EXPORTED_VERIFIER_BOOTSTRAP = """
import hashlib
import importlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve()
paths = json.loads(sys.argv[2])
archive_pin = sys.argv[3]
fixture_locks = tuple(json.loads(sys.argv[4]))
sys.path.insert(0, str(root / "scripts"))
origins = {}
source_files = {}
for relative in paths:
    if Path(relative).suffix != ".py":
        source = root / relative
        if source.resolve() != source or not source.is_file():
            raise SystemExit("non-Python verifier resolved outside exported regular sources")
        content = source.read_bytes()
        source_files[relative] = {"bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()}
        continue
    name = Path(relative).stem
    module = importlib.import_module(name)
    origin = Path(module.__file__).resolve()
    if origin != root / relative:
        raise SystemExit("verifier module resolved outside exported sources")
    origins[name] = str(origin)
if archive_pin:
    sys.modules["verify_lightning_producer_sources"].ARCHIVE_SHA256 = archive_pin
sys.argv = [str(root / "scripts/prepare_rust_source_bundle.py"),
            "--destination", str(root), "--verify-only"]
# The authored frame uses the same explicit lock catalogue in both processes.
exporter = sys.modules["prepare_rust_source_bundle"]
exporter.LOCK_PATHS = fixture_locks
exporter.main()
print(json.dumps({"module_origins": origins, "source_files": source_files}, sort_keys=True))
"""


def _add_verification_scripts(source: Path) -> None:
    scripts = Path(__file__).resolve().parent
    for relative in bundle.VERIFICATION_SCRIPT_PATHS:
        target = source / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes((scripts / Path(relative).name).read_bytes())
        target.chmod(0o644)


def _verify_with_exported_scripts(case, exported: Path, *, archive_pin: str = "") -> None:
    unrelated = case.root / "unrelated-python-cwd"
    unrelated.mkdir()
    # Isolated startup ignores poisoned PYTHONPATH; only exported scripts are added.
    result = subprocess.run(
        [sys.executable, "-I", "-S", "-B", "-c", _EXPORTED_VERIFIER_BOOTSTRAP,
         str(exported), json.dumps(bundle.VERIFICATION_SCRIPT_PATHS), archive_pin,
         json.dumps(bundle.LOCK_PATHS)],
        cwd=unrelated, env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parent)},
        check=False, capture_output=True, text=True, timeout=60,
    )
    case.assertEqual(result.returncode, 0, result.stderr)
    case.assertEqual(result.stderr, "")
    rows = result.stdout.splitlines()
    case.assertEqual(len(rows), 2)
    case.assertEqual(json.loads(rows[0])["status"], "verified")
    verification = json.loads(rows[1])
    origins = verification["module_origins"]
    case.assertEqual(origins, {Path(relative).stem: str((exported / relative).resolve())
                              for relative in bundle.VERIFICATION_SCRIPT_PATHS
                              if Path(relative).suffix == ".py"})
    # Non-Python helpers remain byte-bound inputs without executing their runtime.
    case.assertEqual(verification["source_files"], {
        relative: {"bytes": len((exported / relative).read_bytes()),
                   "sha256": bundle.digest(exported / relative)}
        for relative in bundle.VERIFICATION_SCRIPT_PATHS if Path(relative).suffix != ".py"
    })
    case.assertIn("scripts/check_lightning_frontend_boundary.cjs", verification["source_files"])
    case.assertEqual(origins[Path(_OBSERVER_SCRIPT_PATH).stem],
                     str((exported / _OBSERVER_SCRIPT_PATH).resolve()))


class RustSourceBundleTests(unittest.TestCase):
    def setUp(self) -> None:
        # Base fixtures model the original Rust/npm frame without native siblings.
        locks = patch.object(bundle, "LOCK_PATHS",
                             ("Cargo.lock", "tests/native-grammars/Cargo.lock", "fuzz/Cargo.lock"))
        locks.start()
        self.addCleanup(locks.stop)
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
        vendor.vendor_sources(source, acquired, lock_paths=bundle.LOCK_PATHS)
        self.add_npm_inputs(source)
        _add_verification_scripts(source)
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
                       destination: Path | None = None, *,
                       derive_cargo_members: bool = False) -> tuple[Path, dict]:
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
            (contextlib.nullcontext() if derive_cargo_members else
             patch.object(bundle, "materialize_archive", side_effect=AssertionError("export must not unpack"))):
            manifest = bundle.create_bundle(source, destination, allow_network=False,
                                            derive_cargo_members=derive_cargo_members)
            self.assertEqual(run.call_count, 1)
            self.assertEqual(run.call_args.args[0], ["git", "diff", "--quiet", "HEAD", "--"])
        self.assertEqual(list(empty_home.iterdir()), [])
        return destination, manifest

    def create_project(self) -> tuple[Path, dict]:
        return self.export_project(*self.create_checkout())

    def _ignored_cargo_checkout(self):
        self.members["Cargo.lock"] = (b"version = 4\n# published dependency lock\n", 0o644)
        self.members[".gitignore"] = (b"/Cargo.lock\n", 0o644)
        source, inventory = self.create_checkout()
        name = f"{bundle.VENDOR_PATH}/sample-1.0.0/Cargo.lock"
        return source, [row for row in inventory if row[0] != name], name

    def test_explicit_archive_export_restores_ignored_published_members_only_in_export(self) -> None:
        source, inventory, name = self._ignored_cargo_checkout()
        original_report = (source / bundle.CLOSURE_PATH).read_bytes()
        (source / name).unlink()
        project, manifest = self.export_project(source, inventory, derive_cargo_members=True)
        self.assertFalse((source / name).exists())
        self.assertEqual((source / bundle.CLOSURE_PATH).read_bytes(), original_report)
        self.assertEqual((project / name).read_bytes(), self.members["Cargo.lock"][0])
        self.assertEqual((project / bundle.VENDOR_PATH / "sample-1.0.0/.gitignore").read_bytes(),
                         self.members[".gitignore"][0])
        self.assertEqual(manifest["archive_derived_cargo_members"], [{
            "path": name, "sha256": hashlib.sha256(self.members["Cargo.lock"][0]).hexdigest(),
            "archive": f"{bundle.ARCHIVES_PATH}/sample-1.0.0.crate",
            "archive_sha256": bundle.digest(self.archive), "member": "Cargo.lock", "mode": 0o644,
        }])
        self.assertEqual(bundle.verify_project_bundle(project), manifest)
        self.assertFalse(manifest["frontend_external_source_closure_verified"])
        _verify_with_exported_scripts(self, project)

    def test_explicit_archive_export_checks_existing_ignored_member_and_preserves_mode(self) -> None:
        source, inventory, name = self._ignored_cargo_checkout()
        tool = f"{bundle.VENDOR_PATH}/sample-1.0.0/bin/tool"
        inventory = [row for row in inventory if row[0] != tool]
        project, manifest = self.export_project(source, inventory, derive_cargo_members=True)
        self.assertEqual((project / name).read_bytes(), (source / name).read_bytes())
        self.assertEqual(manifest["file_modes"][tool], 0o755)
        self.assertEqual([row["path"] for row in manifest["archive_derived_cargo_members"]],
                         sorted([name, tool]))

    def test_default_export_still_rejects_untracked_published_member(self) -> None:
        source, inventory, _name = self._ignored_cargo_checkout()
        with self.assertRaisesRegex(ValueError, "untracked Cargo source inputs"):
            self.export_project(source, inventory)

    def test_archive_export_does_not_repair_missing_tracked_file_or_checksum(self) -> None:
        source, inventory, _name = self._ignored_cargo_checkout()
        for index, member in enumerate(("src/lib.rs", ".cargo-checksum.json")):
            path = source / bundle.VENDOR_PATH / "sample-1.0.0" / member
            original = path.read_bytes()
            path.unlink()
            diagnostic = ("missing regular source input" if member == ".cargo-checksum.json"
                          else "missing tracked Cargo")
            with self.subTest(member=member), self.assertRaisesRegex(ValueError, diagnostic):
                self.export_project(source, inventory, self.root / f"missing-{index}",
                                    derive_cargo_members=True)
            self.assertFalse(path.exists())
            path.write_bytes(original)

    def test_archive_export_rejects_changed_ignored_member_and_extra_file(self) -> None:
        source, inventory, name = self._ignored_cargo_checkout()
        (source / name).write_bytes(b"changed published lock\n")
        with self.assertRaisesRegex(ValueError, "changed Cargo checkout member"):
            self.export_project(source, inventory, derive_cargo_members=True)
        (source / name).write_bytes(self.members["Cargo.lock"][0])
        extra = source / bundle.VENDOR_PATH / "sample-1.0.0/foreign.txt"
        extra.write_bytes(b"not published\n")
        with self.assertRaisesRegex(ValueError, "extra Cargo checkout member"):
            self.export_project(source, inventory, derive_cargo_members=True)
        self.assertTrue(extra.is_file())

    def test_archive_export_requires_versioned_archives_report_locks_and_checksum(self) -> None:
        source, inventory, _name = self._ignored_cargo_checkout()
        for index, name in enumerate((f"{bundle.ARCHIVES_PATH}/sample-1.0.0.crate",
                                      bundle.CLOSURE_PATH, bundle.LOCK_PATHS[0],
                                      f"{bundle.VENDOR_PATH}/sample-1.0.0/.cargo-checksum.json")):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "untracked|only published"):
                self.export_project(source, [row for row in inventory if row[0] != name],
                                    self.root / f"untracked-{index}", derive_cargo_members=True)

    def test_archive_derived_provenance_is_reauthenticated_after_restore(self) -> None:
        source, inventory, _name = self._ignored_cargo_checkout()
        project, manifest = self.export_project(source, inventory, derive_cargo_members=True)
        for field, value in (("archive_sha256", "0" * 64), ("member", "src/lib.rs"),
                             ("mode", 0o755)):
            changed = copy.deepcopy(manifest)
            changed["archive_derived_cargo_members"][0][field] = value
            (project / bundle.MANIFEST_PATH).write_text(json.dumps(changed), encoding="utf-8")
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "member provenance"):
                bundle.verify_project_bundle(project)

    def test_archive_export_preserves_authenticated_compact_checksum_bytes(self) -> None:
        source, inventory, name = self._ignored_cargo_checkout()
        metadata = source / bundle.VENDOR_PATH / "sample-1.0.0/.cargo-checksum.json"
        compact = json.dumps(json.loads(metadata.read_bytes()), separators=(",", ":")).encode()
        metadata.write_bytes(compact)
        closure = bundle.verify_bundle(source / bundle.VENDOR_PATH,
                                        [source / lock for lock in bundle.LOCK_PATHS],
                                        archives=source / bundle.ARCHIVES_PATH)
        report = {**closure, "file_modes": bundle._cargo_file_modes(closure),
                  "frontend_external_source_closure_verified": False,
                  "build_and_packaging_acceptance_verified": False}
        (source / bundle.CLOSURE_PATH).write_text(json.dumps(report, sort_keys=True, indent=2) + "\n",
                                                 encoding="utf-8", newline="\n")
        (source / name).unlink()
        project, manifest = self.export_project(source, inventory, derive_cargo_members=True)
        self.assertEqual((project / metadata.relative_to(source)).read_bytes(), compact)
        self.assertEqual(metadata.read_bytes(), compact)
        self.assertFalse((source / name).exists())
        self.assertEqual(bundle.verify_project_bundle(project), manifest)

    def test_archive_export_rejects_checksum_byte_change_without_rebinding_report(self) -> None:
        source, inventory, _name = self._ignored_cargo_checkout()
        metadata = source / bundle.VENDOR_PATH / "sample-1.0.0/.cargo-checksum.json"
        metadata.write_bytes(json.dumps(json.loads(metadata.read_bytes()), separators=(",", ":")).encode())
        with self.assertRaisesRegex(ValueError, "dependency report differs"):
            self.export_project(source, inventory, derive_cargo_members=True)

    def test_archive_export_rejects_linked_checkout_members_and_ancestors(self) -> None:
        source, inventory, name = self._ignored_cargo_checkout()
        original_linked_path = bundle.linked_path
        for index, linked in enumerate((source / name, source / bundle.VENDOR_PATH / "sample-1.0.0")):
            # A reparse attribute is host metadata, independent of file bytes.
            diagnostic = "linked source input" if index == 1 else "linked Cargo checkout member"
            with self.subTest(linked=linked), patch.object(
                bundle, "linked_path", side_effect=lambda path: path == linked or original_linked_path(path)
            ), self.assertRaisesRegex(ValueError, diagnostic):
                self.export_project(source, inventory, self.root / f"linked-{index}",
                                    derive_cargo_members=True)

    def test_archive_export_rejects_extra_empty_package_and_changed_archive_or_report(self) -> None:
        source, inventory, _name = self._ignored_cargo_checkout()
        extra = source / bundle.VENDOR_PATH / "unpublished-1.0.0"
        extra.mkdir()
        with self.assertRaisesRegex(ValueError, "extra Cargo checkout package"):
            self.export_project(source, inventory, derive_cargo_members=True)
        extra.rmdir()
        for name in (f"{bundle.ARCHIVES_PATH}/sample-1.0.0.crate", bundle.CLOSURE_PATH):
            path = source / name
            original = path.read_bytes()
            path.write_bytes(original + b"changed")
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "archive checksum|dependency report"):
                self.export_project(source, inventory, derive_cargo_members=True)
            path.write_bytes(original)

    def test_archive_export_rechecks_present_and_absent_checkout_members_before_publication(self) -> None:
        source, inventory, name = self._ignored_cargo_checkout()
        (source / name).unlink()
        check = bundle._check_cargo_checkout

        def introduce_member(*args):
            observed = check(*args)
            (source / name).write_bytes(self.members["Cargo.lock"][0])
            return observed

        with patch.object(bundle, "_check_cargo_checkout", side_effect=introduce_member), \
            self.assertRaisesRegex(ValueError, "Cargo checkout changed during"):
            self.export_project(source, inventory, derive_cargo_members=True)
        self.assertFalse((self.root / "portable-project" / bundle.MANIFEST_PATH).exists())

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
        self.assertIsNone(manifest["lightning_yarn_inputs"])
        self.assertIs(manifest["lightning_yarn_inputs_verified"], False)
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
        self.assertEqual([row["path"] for row in pristine["cargo_source_closure"]["lockfiles"]],
                         sorted(bundle.LOCK_PATHS))
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

    def test_absent_yarn_requires_explicit_null_evidence_and_false_verified_flag(self) -> None:
        project, pristine = self.create_project()
        self.assertEqual(bundle._yarn_source_inputs(project), (None, {}, {}, set()))
        for field, value in (("lightning_yarn_inputs", {}), ("lightning_yarn_inputs_verified", True),
                             ("lightning_yarn_inputs_verified", 0)):
            manifest = copy.deepcopy(pristine)
            manifest[field] = value
            (project / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(manifest))
            with self.subTest(field=field, value=value), self.assertRaisesRegex(ValueError, "Yarn source inputs"):
                bundle.verify_project_bundle(project)
        for field in ("lightning_yarn_inputs", "lightning_yarn_inputs_verified"):
            manifest = copy.deepcopy(pristine)
            del manifest[field]
            (project / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(manifest))
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "Yarn source inputs"):
                bundle.verify_project_bundle(project)

    def test_frontend_source_build_helpers_are_required_export_inputs(self) -> None:
        required = {
            "scripts/build_lightning_native.py", "scripts/build_frontend_from_sources.py",
            "scripts/install_frontend_from_archives.py", "scripts/check_lightning_frontend_boundary.cjs",
        }
        self.assertTrue(required.issubset(bundle.VERIFICATION_SCRIPT_PATHS))

    def test_all_verification_modules_must_be_tracked_before_export(self) -> None:
        source, inventory = self.create_checkout()
        for index, name in enumerate(bundle.VERIFICATION_SCRIPT_PATHS):
            destination = self.root / f"untracked-verification-script-{index}"
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "untracked required project source"):
                self.export_project(source, [row for row in inventory if row[0] != name], destination)
            self.assertFalse(destination.exists())

    def test_all_verification_modules_must_be_regular_present_inputs_before_export(self) -> None:
        source, inventory = self.create_checkout()
        for index, name in enumerate(bundle.VERIFICATION_SCRIPT_PATHS):
            path = source / name
            content = path.read_bytes()
            path.unlink()
            destination = self.root / f"missing-verification-script-{index}"
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "missing regular source input"):
                self.export_project(source, inventory, destination)
            self.assertFalse(destination.exists())
            path.write_bytes(content)
            path.chmod(0o644)

    def test_restored_manifest_must_retain_every_verification_module(self) -> None:
        exported, pristine = self.create_project()
        for name in bundle.VERIFICATION_SCRIPT_PATHS:
            manifest = copy.deepcopy(pristine)
            manifest["project_sources"] = [row for row in manifest["project_sources"] if row["path"] != name]
            (exported / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(manifest))
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "omits required versioned project inputs"):
                bundle.verify_project_bundle(exported)
        (exported / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(pristine))
        for name in bundle.VERIFICATION_SCRIPT_PATHS:
            path = exported / name
            content = path.read_bytes()
            path.unlink()
            with self.subTest(missing=name), self.assertRaisesRegex(ValueError, "missing regular source input"):
                bundle.verify_project_bundle(exported)
            path.write_bytes(content)
            path.chmod(0o644)

    def test_lightning_placement_scripts_require_tracked_regular_export_sources(self) -> None:
        for relative in _LIGHTNING_PLACEMENT_SCRIPT_PATHS:
            self.assertIn(relative, bundle.VERIFICATION_SCRIPT_PATHS)
            stem = Path(relative).stem
            for state in ("untracked", "missing"):
                source, inventory = self.create_checkout(f"{stem}-{state}")
                if state == "untracked":
                    inventory = [row for row in inventory if row[0] != relative]
                    reason = "untracked required project source"
                else:
                    (source / relative).unlink()
                    reason = "missing regular source input"
                destination = self.root / f"rejected-{stem}-{state}"
                diagnostic = f"^{reason}: {relative.replace('.', '[.]')}$"
                with self.subTest(relative=relative, state=state):
                    with self.assertRaisesRegex(ValueError, diagnostic):
                        self.export_project(source, inventory, destination)
                self.assertFalse(destination.exists())

    def test_exported_lightning_placement_script_bytes_are_manifest_bound(self) -> None:
        exported, manifest = self.create_project()
        scripts = Path(__file__).resolve().parent
        for relative in _LIGHTNING_PLACEMENT_SCRIPT_PATHS:
            self.assertIn(relative, bundle.VERIFICATION_SCRIPT_PATHS)
            path = exported / relative
            original = path.read_bytes()
            self.assertEqual(original, (scripts / Path(relative).name).read_bytes())
            record = next(row for row in manifest["project_sources"]
                          if row["path"] == relative)
            self.assertEqual(record["sha256"], bundle.digest(path))
            self.assertEqual(manifest["file_modes"][relative], 0o644)
            path.write_bytes(original + b"\n# Authored exported verification module mutation.\n")
            diagnostic = f"^changed project source: {relative.replace('.', '[.]')}$"
            with self.subTest(relative=relative):
                with self.assertRaisesRegex(ValueError, diagnostic):
                    bundle.verify_project_bundle(exported)
            path.write_bytes(original)

    def test_exported_scripts_verify_without_original_workspace_import_resolution(self) -> None:
        exported, manifest = self.create_project()
        paths = {record["path"] for record in manifest["project_sources"]}
        self.assertTrue(set(_LIGHTNING_PLACEMENT_SCRIPT_PATHS).issubset(paths))
        _verify_with_exported_scripts(self, exported)


class ProducerSourceExportTests(AuthoredFixture, unittest.TestCase):
    """Exercise real producer verification on complete authored inert inputs."""

    def setUp(self) -> None:
        RustSourceBundleTests.setUp(self)
        AuthoredFixture.setUp(self)
        checksum = RustSourceBundleTests.write_archive(self)
        self.producer_dependency = self.root / "producer-input-2.0.0.crate"
        with tarfile.open(self.producer_dependency, "w:gz") as tar:
            for name, content, mode in (
                ("Cargo.toml", b'[package]\nname = "producer-input"\nversion = "2.0.0"\nlicense = "MIT"\n', 0o644),
                ("src/lib.rs", b"pub const INPUT: u8 = 2;\r\n", 0o644),
                ("bin/check", b"#!/bin/sh\nexit 0\n", 0o755),
                ("LICENSE", b"authored dependency license\n", 0o644),
            ):
                member = tarfile.TarInfo("producer-input-2.0.0/" + name)
                member.size, member.mode = len(content), mode
                tar.addfile(member, io.BytesIO(content))
        dependency_checksum = bundle.digest(self.producer_dependency)
        sample_lock = ('version = 4\n[[package]]\nname = "sample"\nversion = "1.0.0"\n'
                       f'source = "{bundle.REGISTRY}"\nchecksum = "{checksum}"\n').encode()
        self.files[producer.ROOT + "/Cargo.lock"] = sample_lock + (
            '[[package]]\nname = "producer-input"\nversion = "2.0.0"\n'
            f'source = "{bundle.REGISTRY}"\nchecksum = "{dependency_checksum}"\n'
            '[[package]]\nname = "local-input"\nversion = "0.1.0"\n').encode()
        empty = tarfile.TarInfo(producer.ROOT + "/documentation/empty")
        empty.type, empty.mode = tarfile.DIRTYPE, 0o775
        self.payload = producer_archive(self.files, [empty])
        pin = patch.object(producer, "ARCHIVE_SHA256", producer.digest(self.payload))
        pin.start()
        self.addCleanup(pin.stop)
        self.write_prepared()
        producer_import.import_bundle(self.project, self.prepared, self.capture, self.destination)
        for name, content in {**{name: sample_lock for name in bundle.LOCK_PATHS},
                              ".cargo/config.toml": RustSourceBundleTests._local_config(self)}.items():
            path = self.project / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        retained = self.project / bundle.ARCHIVES_PATH
        retained.mkdir()
        modes = {}
        for source, name, version, pin in ((self.archive, "sample", "1.0.0", checksum),
                                         (self.producer_dependency, "producer-input", "2.0.0", dependency_checksum)):
            archive = retained / source.name
            archive.write_bytes(source.read_bytes())
            modes.update(bundle.materialize_archive(archive, self.project / bundle.VENDOR_PATH,
                                                   name, version, pin))
            modes[f"{bundle.ARCHIVES_PATH}/{source.name}"] = 0o644
        closure = bundle.verify_bundle(self.project / bundle.VENDOR_PATH,
                                       [self.project / name for name in (*bundle.LOCK_PATHS, bundle.LIGHTNING_LOCK_PATH)],
                                       archives=retained)
        report = {**closure, "file_modes": dict(sorted(modes.items())),
                  "frontend_external_source_closure_verified": False,
                  "build_and_packaging_acceptance_verified": False}
        (self.project / bundle.CLOSURE_PATH).write_bytes(producer.json_bytes(report))
        frontend = (self.project / npm.CANONICAL_LOCK).parent
        package = json.loads((self.project / npm.CANONICAL_LOCK).read_bytes())["packages"][""]
        (frontend / "package.json").write_bytes(producer.json_bytes(package))
        (frontend / ".npmrc").write_bytes(b"fund=false\npackage-lock=true\n")
        _add_verification_scripts(self.project)
        self.inventory = sorted((path.relative_to(self.project).as_posix(), False)
                                for path in self.project.rglob("*") if path.is_file()
                                and self.prepared not in path.parents and path != self.capture)

    def export(self, *, inventory=None, destination=None):
        return RustSourceBundleTests.export_project(
            self, self.project, self.inventory if inventory is None else inventory, destination)

    def test_complete_producer_requires_exact_four_lock_union_and_original_archives(self) -> None:
        exported, manifest = self.export()
        closure = manifest["cargo_source_closure"]
        paths = (*bundle.LOCK_PATHS, bundle.LIGHTNING_LOCK_PATH)
        self.assertEqual([row["path"] for row in closure["lockfiles"]], sorted(paths))
        self.assertEqual(closure["package_count"], 2)
        self.assertEqual(closure["local_package_count"], 1)
        self.assertTrue(closure["archives_verified"])
        self.assertEqual([(row["name"], row["version"]) for row in closure["packages"]],
                         [("producer-input", "2.0.0"), ("sample", "1.0.0")])
        producer_lock = next(row for row in closure["lockfiles"] if row["path"] == bundle.LIGHTNING_LOCK_PATH)
        self.assertEqual(producer_lock["sha256"], bundle.digest(exported / bundle.LIGHTNING_LOCK_PATH))
        original = exported / bundle.ARCHIVES_PATH / self.producer_dependency.name
        self.assertEqual(original.read_bytes(), self.producer_dependency.read_bytes())
        dependency = next(row for row in closure["packages"] if row["name"] == "producer-input")
        self.assertEqual(dependency["license"], "MIT")
        self.assertEqual([row["path"] for row in dependency["license_files"]], ["LICENSE"])
        self.assertEqual(manifest["file_modes"][bundle.VENDOR_PATH + "/producer-input-2.0.0/bin/check"], 0o755)
        self.assertEqual(bundle.verify_project_bundle(exported), manifest)
        self.assertFalse(manifest["frontend_external_source_closure_verified"])

    def test_present_producer_cannot_use_a_report_omitting_its_lock_association(self) -> None:
        path = self.project / bundle.CLOSURE_PATH
        report = json.loads(path.read_bytes())
        report["lockfiles"] = [row for row in report["lockfiles"] if row["path"] != bundle.LIGHTNING_LOCK_PATH]
        path.write_bytes(producer.json_bytes(report))
        destination = self.root / "omitted-producer-lock"
        with self.assertRaisesRegex(ValueError, "Cargo dependency report"):
            self.export(destination=destination)
        self.assertFalse(destination.exists())

    def test_missing_or_tampered_producer_only_inputs_reject_before_export(self) -> None:
        controls = (self.project / bundle.ARCHIVES_PATH / self.producer_dependency.name,
                    self.project / bundle.VENDOR_PATH / "producer-input-2.0.0/src/lib.rs")
        for index, path in enumerate(controls):
            payload = path.read_bytes()
            for state in ("missing", "tampered"):
                if state == "missing":
                    path.unlink()
                else:
                    path.write_bytes(payload + b"changed")
                destination = self.root / f"producer-dependency-{index}-{state}"
                with self.subTest(path=path.name, state=state), self.assertRaises(ValueError):
                    self.export(destination=destination)
                self.assertFalse(destination.exists())
                path.write_bytes(payload)

    def test_restored_manifest_cannot_drop_the_qualified_producer_lock_association(self) -> None:
        exported, manifest = self.export()
        manifest["cargo_source_closure"]["lockfiles"] = [
            row for row in manifest["cargo_source_closure"]["lockfiles"]
            if row["path"] != bundle.LIGHTNING_LOCK_PATH]
        (exported / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(manifest))
        with self.assertRaisesRegex(ValueError, "Cargo source closure differs"):
            bundle.verify_project_bundle(exported)

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

            def changed_after_copy(root, **keywords):
                result = original(root, **keywords)
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


class YarnSourceExportTests(AuthoredFixture, unittest.TestCase):
    """Transport actual synthetic producer/Yarn inputs without verifier substitutes."""

    def setUp(self) -> None:
        ProducerSourceExportTests.setUp(self)
        self.yarn_root = self.project / yarn.BUNDLE_PATH
        (self.yarn_root / "archives").mkdir(parents=True)
        (self.yarn_root / "packages").mkdir()
        self.yarn_rows, sections = [], ["# yarn lockfile v1\n"]
        for name in ("leaf", "second"):
            payload, members = yarn_archive(name, extras={
                "authored root/.gitignore": (b"bin/task.js\n.DS_Store\n", 0o644),
                "authored root/vendor/.DS_Store": (b"authored inert ignored input\n", 0o644),
            })
            resolved, integrity = yarn_pin_fields(name, payload)
            sections.append(f'{name}@^1.0.0:\n  version "1.0.0"\n  resolved "{resolved}"\n'
                            f'  integrity {integrity}\n\n')
            filename = hashlib.sha256(resolved.encode("ascii")).hexdigest() + ".tgz"
            archive_path = self.yarn_root / "archives" / filename
            archive_path.write_bytes(payload)
            archive_path.chmod(0o644)
            expanded = self.yarn_root / "packages" / Path(filename).stem
            (expanded / "authored root/nested/empty").mkdir(parents=True)
            for path, (content, mode) in members.items():
                target = expanded / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(content)
                target.chmod(mode)
            self.yarn_rows.append({"archive": archive_path, "expanded": expanded, "payload": payload})
        sections.append(f'"{producer.PACKAGE}@link:.":\n  version "0.0.0"\n  uid ""\n')
        self.files[producer.ROOT + "/package.json"] = producer.json_bytes({
            "name": producer.PACKAGE, "version": producer.VERSION,
            "dependencies": {"leaf": "^1.0.0"}, "optionalDependencies": {"second": "^1.0.0"},
            "resolutions": {producer.PACKAGE: "link:."},
        })
        self.files[producer.ROOT + "/yarn.lock"] = "".join(sections).encode("ascii")
        empty = tarfile.TarInfo(producer.ROOT + "/documentation/empty")
        empty.type, empty.mode = tarfile.DIRTYPE, 0o775
        self.payload = producer_archive(self.files, [empty])
        pin = patch.object(producer, "ARCHIVE_SHA256", producer.digest(self.payload))
        pin.start()
        self.addCleanup(pin.stop)
        self.write_prepared()
        self.destination.rename(self.root / "prior-authored-producer")
        producer_import.import_bundle(self.project, self.prepared, self.capture, self.destination)
        self.yarn_report = yarn.derive_bundle(self.project, self.yarn_root)
        (self.yarn_root / yarn.REPORT_NAME).write_bytes(producer.json_bytes(self.yarn_report))
        (self.yarn_root / yarn.REPORT_NAME).chmod(0o644)
        self.inventory = sorted((path.relative_to(self.project).as_posix(), False)
                                for path in self.project.rglob("*") if path.is_file()
                                and self.prepared not in path.parents and path != self.capture)

    def export(self, *, inventory=None, destination=None):
        """Use the common export harness, replacing only read-only Git provenance."""
        return ProducerSourceExportTests.export(self, inventory=inventory, destination=destination)

    def test_exact_yarn_bytes_modes_ignore_members_and_separate_claims_survive_export(self) -> None:
        before = {name: (self.project / name).read_bytes() for name, _mode in self.inventory}
        exported, manifest = self.export()
        self.assertIs(manifest["lightning_yarn_inputs_verified"], True)
        self.assertEqual(manifest["lightning_yarn_inputs"], yarn.verify_bundle(exported))
        self.assertEqual(manifest["lightning_yarn_inputs"], self.yarn_report)
        self.assertEqual(self.yarn_report["archive_count"], 2)
        self.assertEqual(self.yarn_report["pin_graph"]["stanza_count"], 3)
        self.assertEqual(self.yarn_report["pin_graph"]["local_associations"][0]["resolution"], "link:.")
        self.assertIn("optionalDependencies", {row["section"] for row in self.yarn_report["pin_graph"]["roots"]})
        self.assertEqual(manifest["npm_distribution_inputs"]["archive_count"], 1)
        for key in yarn.FALSE_CLAIMS:
            self.assertIs(manifest["lightning_yarn_inputs"][key], False)
        self.assertIs(manifest["frontend_external_source_closure_verified"], False)
        _proof, modes, hashes, _directories = bundle._yarn_source_inputs(self.project)
        self.assertEqual(set(hashes), {yarn.BUNDLE_PATH + "/" + name for name in self.yarn_report["files"]}
                         | {yarn.BUNDLE_PATH + "/" + yarn.REPORT_NAME})
        for name, mode in modes.items():
            self.assertEqual(manifest["file_modes"][name], mode)
            self.assertIs(next(row["executable"] for row in manifest["project_sources"] if row["path"] == name),
                          bool(mode & 0o111))
        self.assertEqual(sum(name.endswith("/.DS_Store") for name in hashes), 2)
        self.assertEqual(sum(name.endswith("/.gitignore") for name in hashes), 2)
        for name, content in before.items():
            self.assertEqual((exported / name).read_bytes(), content)
            self.assertEqual((self.project / name).read_bytes(), content)
        self.assertEqual(bundle.verify_project_bundle(exported), manifest)

    def test_exported_scripts_independently_verify_actual_authored_producer_and_yarn_inputs(self) -> None:
        exported, _manifest = self.export()
        _verify_with_exported_scripts(self, exported, archive_pin=producer.digest(self.payload))

    def test_yarn_archive_transport_is_byte_reproducible_and_restores_empty_directories(self) -> None:
        exported, manifest = self.export()
        yarn_empties = {yarn.BUNDLE_PATH + "/" + row["expanded_path"] + "/authored root/nested/empty"
                        for row in self.yarn_report["packages"]}
        self.assertTrue(yarn_empties.issubset(manifest["source_input_empty_directories"]))
        for name in yarn_empties:
            self.assertEqual(manifest["source_input_empty_directories"][name], 0o755)
            self.assertEqual(list((exported / name).iterdir()), [])
        first, second = self.root / "yarn-first.tar.gz", self.root / "yarn-second.tar.gz"
        bundle.archive_bundle(exported, first)
        bundle.archive_bundle(exported, second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        restored = self.root / "yarn-restored"
        restored.mkdir()
        with tarfile.open(first) as tar:
            self.assertEqual(tar.getnames(), sorted(set(tar.getnames())))
            for name in yarn_empties:
                member = tar.getmember("rootlight/" + name)
                self.assertTrue(member.isdir())
                self.assertEqual((member.mode, member.uid, member.gid, member.mtime), (0o755, 0, 0, 0))
            for name, mode in manifest["file_modes"].items():
                if name.startswith(yarn.BUNDLE_PATH + "/"):
                    self.assertEqual(tar.getmember("rootlight/" + name).mode, mode)
            tar.extractall(restored, filter="data")
        self.assertEqual(bundle.verify_project_bundle(restored / "rootlight"), manifest)

    def test_yarn_namespace_partial_empty_foreign_and_extra_children_are_not_absence(self) -> None:
        parent = self.project / bundle.PRODUCER_DEPENDENCIES_PATH
        child = self.yarn_root.parent
        child.rename(self.root / "saved-yarn-producer")
        with self.assertRaisesRegex(ValueError, "Yarn namespace inventory"):
            bundle._yarn_source_inputs(self.project)
        foreign = parent / "unreviewed-producer"
        foreign.mkdir()
        with self.assertRaisesRegex(ValueError, "Yarn namespace inventory"):
            bundle._yarn_source_inputs(self.project)
        foreign.rmdir()
        child.mkdir()
        with self.assertRaisesRegex(ValueError, "Yarn namespace inventory"):
            bundle._yarn_source_inputs(self.project)
        child.rmdir()
        (self.root / "saved-yarn-producer").rename(child)
        extra = child / "extra-namespace"
        extra.mkdir()
        with self.assertRaisesRegex(ValueError, "Yarn namespace inventory"):
            bundle._yarn_source_inputs(self.project)
        extra.rmdir()
        parent.rename(self.root / "saved-yarn-parent")
        parent.write_bytes(b"not a namespace directory")
        with self.assertRaisesRegex(ValueError, "Yarn namespace inventory"):
            bundle._yarn_source_inputs(self.project)

    def test_every_yarn_archive_member_and_report_must_be_tracked_before_copy(self) -> None:
        _proof, _modes, hashes, _directories = bundle._yarn_source_inputs(self.project)
        for index, name in enumerate(hashes):
            destination = self.root / f"untracked-yarn-{index}"
            inventory = [row for row in self.inventory if row[0] != name]
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "untracked Yarn"):
                self.export(inventory=inventory, destination=destination)
            self.assertFalse(destination.exists())

    def test_missing_yarn_original_member_and_report_reject_before_destination_creation(self) -> None:
        controls = (self.yarn_rows[0]["archive"], self.yarn_rows[1]["expanded"] / "authored root/LICENSE",
                    self.yarn_root / yarn.REPORT_NAME)
        for index, path in enumerate(controls):
            content = path.read_bytes()
            path.unlink()
            destination = self.root / f"missing-yarn-{index}"
            with self.subTest(path=path), self.assertRaises(ValueError):
                self.export(destination=destination)
            self.assertFalse(destination.exists())
            path.write_bytes(content)
            path.chmod(0o644)

    def test_yarn_report_byte_mode_inventory_and_claim_rebinding_reject(self) -> None:
        path = self.yarn_root / yarn.REPORT_NAME
        before = path.read_bytes()
        member = next(name for name in self.yarn_report["files"] if name.endswith("/bin/task.js"))
        changes = (
            lambda report: report.update(preferred_source_closure_complete=True),
            lambda report: report.update(extra=False),
            lambda report: report["files"][member].update(mode=0o644),
            lambda report: report["files"][member].update(sha256="0" * 64),
            lambda report: report["files"].pop(member),
        )
        for index, change in enumerate(changes):
            report = copy.deepcopy(self.yarn_report)
            change(report)
            path.write_bytes(producer.json_bytes(report))
            destination = self.root / f"rebound-yarn-report-{index}"
            with self.subTest(index=index), self.assertRaisesRegex(ValueError, "canonical_report_differs"):
                self.export(destination=destination)
            self.assertFalse(destination.exists())
        path.write_bytes(json.dumps(self.yarn_report).encode())
        with self.assertRaisesRegex(ValueError, "canonical_report_differs"):
            self.export(destination=self.root / "noncanonical-yarn-report")
        path.write_bytes(before)

    def test_restored_yarn_witness_flags_and_modes_cannot_be_rebound(self) -> None:
        exported, pristine = self.export()
        manifest_path = exported / bundle.MANIFEST_PATH
        for field, value in (("lightning_yarn_inputs", None), ("lightning_yarn_inputs_verified", False),
                             ("lightning_yarn_inputs_verified", 1)):
            manifest = copy.deepcopy(pristine)
            manifest[field] = value
            manifest_path.write_bytes(producer.json_bytes(manifest))
            with self.subTest(field=field, value=value), self.assertRaisesRegex(ValueError, "Yarn source inputs"):
                bundle.verify_project_bundle(exported)
        for field in ("archive_count", "preferred_source_closure_complete"):
            manifest = copy.deepcopy(pristine)
            manifest["lightning_yarn_inputs"][field] = 9 if field == "archive_count" else True
            manifest_path.write_bytes(producer.json_bytes(manifest))
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "Yarn source inputs"):
                bundle.verify_project_bundle(exported)
        for name, expected_mode in bundle._yarn_source_inputs(self.project)[1].items():
            manifest = copy.deepcopy(pristine)
            manifest["file_modes"][name] = 0o644 if expected_mode == 0o755 else 0o755
            next(row for row in manifest["project_sources"] if row["path"] == name)["executable"] = expected_mode != 0o755
            manifest_path.write_bytes(producer.json_bytes(manifest))
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "authenticated Yarn inputs"):
                bundle.verify_project_bundle(exported)

    def test_yarn_archive_modes_override_all_nonexecutable_and_data_executable_git_bits(self) -> None:
        modes = bundle._yarn_source_inputs(self.project)[1]
        inventory = [(name, modes[name] == 0o644 if name in modes else executable)
                     for name, executable in self.inventory]
        exported, manifest = self.export(inventory=inventory)
        for name, mode in modes.items():
            self.assertEqual(manifest["file_modes"][name], mode)
        self.assertEqual(bundle.verify_project_bundle(exported), manifest)

    def test_restored_yarn_bytes_cannot_be_rebound_through_project_checksums(self) -> None:
        exported, pristine = self.export()
        prefix = yarn.BUNDLE_PATH + "/"
        controls = (prefix + yarn.REPORT_NAME,
                    next(prefix + name for name in self.yarn_report["files"] if name.startswith("archives/")),
                    next(prefix + name for name in self.yarn_report["files"] if name.endswith("/LICENSE")))
        for name in controls:
            path = exported / name
            content = path.read_bytes()
            path.write_bytes(content + b"mutation")
            manifest = copy.deepcopy(pristine)
            next(row for row in manifest["project_sources"] if row["path"] == name)["sha256"] = bundle.digest(path)
            (exported / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(manifest))
            with self.subTest(name=name), self.assertRaises(ValueError):
                bundle.verify_project_bundle(exported)
            path.write_bytes(content)

    def test_missing_yarn_empty_directories_are_never_silently_reconstructed(self) -> None:
        empty = self.yarn_rows[0]["expanded"] / "authored root/nested/empty"
        empty.rmdir()
        destination = self.root / "missing-yarn-empty"
        with self.assertRaisesRegex(ValueError, "stored_namespace_inventory_differs"):
            self.export(destination=destination)
        self.assertFalse(destination.exists())
        empty.mkdir()
        exported, pristine = self.export()
        relative = empty.relative_to(self.project).as_posix()
        manifest = copy.deepcopy(pristine)
        del manifest["source_input_empty_directories"][relative]
        (exported / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(manifest))
        with self.assertRaisesRegex(ValueError, "empty directories"):
            bundle.verify_project_bundle(exported)
        (exported / bundle.MANIFEST_PATH).write_bytes(producer.json_bytes(pristine))
        (exported / relative).rmdir()
        with self.assertRaisesRegex(ValueError, "stored_namespace_inventory_differs"):
            bundle.verify_project_bundle(exported)

    def test_source_mutation_after_verification_is_detected_without_manifest_publication(self) -> None:
        destination = self.root / "late-yarn-mutation"
        source = self.yarn_rows[0]["expanded"] / "authored root/LICENSE"

        def provenance_with_mutation(command, **_options):
            self.assertEqual(command, ["git", "diff", "--quiet", "HEAD", "--"])
            source.write_bytes(source.read_bytes() + b"changed after verification")
            result = unittest.mock.Mock()
            result.returncode = 1
            return result

        with patch.object(bundle, "tracked_files", return_value=self.inventory), \
                patch.object(bundle.subprocess, "check_output", return_value="a" * 40 + "\n"), \
                patch.object(bundle.subprocess, "run", side_effect=provenance_with_mutation):
            with self.assertRaisesRegex(ValueError, "project source changed during export"):
                bundle.create_bundle(self.project, destination, allow_network=False)
        self.assertFalse((destination / bundle.MANIFEST_PATH).exists())


if __name__ == "__main__":
    unittest.main()
