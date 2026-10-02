#!/usr/bin/env python3
"""Exercise lock-authenticated npm archives using synthetic, offline inputs.

Fixtures include native-looking files to pin the distinction between original
distribution-byte evidence and an unproven preferred-source implementation closure.
"""

from __future__ import annotations

import base64
import contextlib
import gzip
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("verify_npm_source_bundle.py")
SPEC = importlib.util.spec_from_file_location("verify_npm_source_bundle", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)


def _sri(payload: bytes, algorithm: str = "sha512") -> str:
    return algorithm + "-" + base64.b64encode(hashlib.new(algorithm, payload).digest()).decode("ascii")


class NpmSourceBundleTests(unittest.TestCase):
    """Require portable exact-set evidence without installation or source execution."""

    def setUp(self) -> None:
        """Create independent synthetic lock/archive inputs for each test."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.archives = self.root / "archives"
        self.archives.mkdir()
        self.lock = self.root / "package-lock.json"
        self.document = {
            "name": "fixture-project",
            "version": "0.1.0",
            "lockfileVersion": 3,
            "requires": True,
            "packages": {"": {"name": "fixture-project", "version": "0.1.0"}},
        }
        self.first_path = "node_modules/alpha"
        self.second_path = "node_modules/alpha/node_modules/@scope/beta"
        self._package(self.first_path, "alpha", "1.2.3", dev=True)
        self._package(self.second_path, "@scope/beta", "2.0.0", optional=True, os=["other-os"])
        self._write_lock()

    def _package(self, path: str, name: str, version: str, **metadata) -> None:
        resolved = f"{bundle.REGISTRY_URL}{name}/-/{name.rsplit('/', 1)[-1]}-{version}.tgz"
        self.document["packages"][path] = {
            "version": version,
            "resolved": resolved,
            "integrity": "",
            "license": "MIT",
            **metadata,
        }
        self._archive(path)

    def _archive(
        self,
        path: str,
        *,
        manifest: dict | bytes | None = None,
        include_manifest: bool = True,
        additional: list[tuple[tarfile.TarInfo, bytes]] | None = None,
        files: dict[str, bytes] | None = None,
        root: str = "package",
        include_root: bool = False,
    ) -> bytes:
        package = self.document["packages"][path]
        name = package["resolved"][len(bundle.REGISTRY_URL):].split("/-/")[0]
        if manifest is None:
            manifest = {"name": name, "version": package["version"], "license": "MIT"}
        contents = {
            f"{root}/index.js": b"export const value = 7;\n",
            f"{root}/LICENSE-MIT": b"synthetic retained notice\n",
            f"{root}/legal/NOTICE": b"synthetic additional attribution\n",
            f"{root}/native/addon.node": b"\x7fELFsynthetic shared library",
            f"{root}/native/module.wasm": b"\x00asm\x01\x00\x00\x00",
            f"{root}/native/library.a": b"!<arch>\nsynthetic static library",
            f"{root}/bin/command": b"MZsynthetic executable",
        }
        if files is not None:
            contents = dict(files)
        if include_manifest:
            contents[f"{root}/package.json"] = manifest if isinstance(manifest, bytes) else json.dumps(manifest).encode()
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            if include_root:
                directory = tarfile.TarInfo(root + "/")
                directory.type, directory.mode = tarfile.DIRTYPE, 0o755
                archive.addfile(directory)
            for member_path, data in sorted(contents.items()):
                info = tarfile.TarInfo(member_path)
                info.size = len(data)
                info.mode = 0o755 if member_path == f"{root}/bin/command" else 0o644
                archive.addfile(info, io.BytesIO(data))
            for info, data in additional or []:
                archive.addfile(info, io.BytesIO(data))
        payload = output.getvalue()
        package["integrity"] = _sri(payload)
        (self.archives / bundle.archive_filename(package["resolved"])).write_bytes(payload)
        return payload

    def _write_lock(self) -> None:
        self.lock.write_text(json.dumps(self.document), encoding="utf-8")

    def _verify(self) -> dict:
        return bundle.verify_bundle(self.lock, self.archives)

    def _rejected(self, message: str = ".*") -> None:
        self._write_lock()
        with self.assertRaisesRegex(bundle.BundleVerificationError, message):
            self._verify()

    def test_complete_nested_optional_dev_distribution_evidence(self) -> None:
        evidence = self._verify()
        self.assertEqual(evidence, self._verify())
        self.assertTrue(evidence["archives_verified"])
        self.assertFalse(evidence["preferred_source_closure_complete"])
        self.assertFalse(evidence["frontend_source_complete"])
        self.assertEqual(evidence["lock_record_count"], 2)
        self.assertEqual(evidence["archive_count"], 2)
        self.assertNotIn(str(self.root), json.dumps(evidence))
        scoped = next(record for record in evidence["packages"] if record["name"] == "@scope/beta")
        self.assertEqual(scoped["archive_root"], "package")
        self.assertEqual(scoped["lock_paths"], [self.second_path])
        self.assertTrue(scoped["lock_records"][0]["optional"])
        self.assertEqual(scoped["license_metadata"], {"license": "MIT"})
        self.assertEqual({record["path"] for record in scoped["license_files"]}, {"package/LICENSE-MIT", "package/legal/NOTICE"})
        native = {record["path"]: record["detected_formats"] for record in scoped["native_binary_files"]}
        self.assertIn("elf", native["package/native/addon.node"])
        self.assertIn("wasm", native["package/native/module.wasm"])
        self.assertIn("static-archive", native["package/native/library.a"])
        self.assertEqual(native["package/bin/command"], ["pe"])
        command = next(record for record in scoped["members"] if record["path"] == "package/bin/command")
        self.assertEqual(command["archive_mode"], 0o755)

    def test_generic_single_directory_roots_preserve_raw_inventory_and_identity(self) -> None:
        for root in ("authored-input", "authored input v7.8"):
            for include_root in (False, True):
                with self.subTest(root=root, include_root=include_root):
                    payload = self._archive(self.first_path, root=root, include_root=include_root)
                    self._write_lock()
                    original_lock = self.lock.read_bytes()
                    evidence = self._verify()
                    self.assertEqual(evidence, self._verify())
                    record = next(item for item in evidence["packages"] if item["name"] == "alpha")
                    self.assertEqual(record["archive_root"], root)
                    self.assertEqual(record["archive"]["sha256"], hashlib.sha256(payload).hexdigest())
                    members = {item["path"]: item for item in record["members"]}
                    expected = {f"{root}/{path}" for path in (
                        "index.js", "LICENSE-MIT", "legal/NOTICE", "native/addon.node",
                        "native/module.wasm", "native/library.a", "bin/command", "package.json",
                    )}
                    if include_root:
                        expected.add(root)
                        self.assertEqual(members[root], {
                            "path": root, "kind": "directory", "bytes": 0, "archive_mode": 0o755,
                        })
                    self.assertEqual(set(members), expected)
                    self.assertEqual(record["package_json_sha256"], members[f"{root}/package.json"]["sha256"])
                    self.assertEqual(members[f"{root}/bin/command"]["archive_mode"], 0o755)
                    self.assertEqual({item["path"] for item in record["license_files"]}, {
                        f"{root}/LICENSE-MIT", f"{root}/legal/NOTICE",
                    })
                    self.assertEqual(record["license_metadata"], {"license": "MIT"})
                    self.assertEqual(record["lock_paths"], [self.first_path])
                    self.assertEqual(self.lock.read_bytes(), original_lock)
                    archive_path = self.archives / record["archive_filename"]
                    self.assertEqual(archive_path.read_bytes(), payload)

    def test_alternative_roots_do_not_permit_multiple_or_aliased_root_directories(self) -> None:
        for second_root in ("other-input", "Authored-input", "authored input v7.8"):
            with self.subTest(second_root=second_root):
                extra = tarfile.TarInfo(second_root + "/extra.js")
                extra.size = 1
                self._archive(self.first_path, root="authored-input", additional=[(extra, b"x")])
                self._rejected("multiple top-level roots|aliased")

    def test_alternative_root_manifest_must_be_unique_direct_valid_and_exactly_locked(self) -> None:
        root = "authored input"
        for manifest in ({"name": "other", "version": "1.2.3"},
                         {"name": "alpha", "version": "9.0.0"},
                         b'{"name":"alpha","name":"alpha","version":"1.2.3"}',
                         b"invalid JSON"):
            with self.subTest(manifest=manifest):
                self._archive(self.first_path, root=root, manifest=manifest)
                self._rejected("package.json")
        for files in ({}, {f"{root}/nested/package.json": b'{"name":"alpha","version":"1.2.3"}'}):
            with self.subTest(files=files):
                self._archive(self.first_path, root=root, include_manifest=False, files=files)
                self._rejected("package.json identity")
        duplicate = tarfile.TarInfo(f"{root}/package.json")
        data = b'{"name":"alpha","version":"1.2.3"}'
        duplicate.size = len(data)
        self._archive(self.first_path, root=root, additional=[(duplicate, data)])
        self._rejected("duplicate archive member")

    def test_alternative_roots_preserve_unsafe_path_and_member_type_rejections(self) -> None:
        root = "authored input"
        for path in (f"{root}/../escape", f"{root}/./file", f"{root}//file", f"/{root}/file",
                     f"C:/{root}/file", f"{root}\\file", f"{root}/file:stream", f"{root}/NUL.txt"):
            with self.subTest(path=path):
                extra = tarfile.TarInfo(path)
                extra.size = 1
                self._archive(self.first_path, root=root, additional=[(extra, b"x")])
                self._rejected("unsafe|invalid")
        for member_type in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE,
                            tarfile.BLKTYPE, tarfile.GNUTYPE_SPARSE):
            with self.subTest(member_type=member_type):
                extra = tarfile.TarInfo(f"{root}/special")
                extra.type, extra.linkname = member_type, f"{root}/package.json"
                self._archive(self.first_path, root=root, additional=[(extra, b"")])
                self._rejected("special|invalid gzip")

    def test_single_root_entry_cannot_be_file_link_special_or_nonempty_directory(self) -> None:
        for member_type in (tarfile.REGTYPE, tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.DIRTYPE):
            with self.subTest(member_type=member_type):
                entry = tarfile.TarInfo("authored-input")
                entry.type = member_type
                entry.linkname = "authored-input/package.json"
                entry.size = 1 if member_type == tarfile.DIRTYPE else 0
                self._archive(self.first_path, root="authored-input", additional=[(entry, b"x")])
                self._rejected("invalid archive file|special|nonempty archive directory")

    def test_identical_archive_retains_all_placements(self) -> None:
        repeated = "node_modules/gamma/node_modules/alpha"
        self.document["packages"][repeated] = dict(self.document["packages"][self.first_path], optional=True)
        self._write_lock()
        evidence = self._verify()
        self.assertEqual(evidence["archive_count"], 2)
        self.assertEqual(evidence["lock_record_count"], 3)
        record = next(record for record in evidence["packages"] if record["name"] == "alpha")
        self.assertEqual(record["lock_paths"], sorted([self.first_path, repeated]))
        self.assertEqual(len(record["lock_records"]), 2)

    def test_filename_mapping_separates_scopes_and_is_url_derived(self) -> None:
        first = "https://registry.npmjs.org/@one/item/-/item-1.0.0.tgz"
        second = "https://registry.npmjs.org/@two/item/-/item-1.0.0.tgz"
        self.assertNotEqual(bundle.archive_filename(first), bundle.archive_filename(second))
        self.assertEqual(bundle.archive_filename(first), hashlib.sha256(first.encode()).hexdigest() + ".tgz")

    def test_locked_archives_api_reads_same_deduplicated_identities(self) -> None:
        inputs = bundle.locked_archives(self.lock)
        evidence = self._verify()
        self.assertEqual(inputs, [{key: record[key] for key in inputs[0]} for record in evidence["packages"]])

    def test_lock_bytes_are_authenticated_independently_of_record_order(self) -> None:
        first = self._verify()
        self.document["packages"] = dict(reversed(list(self.document["packages"].items())))
        self._write_lock()
        second = self._verify()
        self.assertNotEqual(first["lockfile_sha256"], second["lockfile_sha256"])
        self.assertEqual(first["packages"], second["packages"])

    def test_conflicting_repeated_archive_identity_is_rejected(self) -> None:
        self.document["packages"]["node_modules/gamma/node_modules/alpha"] = dict(self.document["packages"][self.first_path], integrity=_sri(b"different input"))
        self._rejected("conflicting package archive identities")

    def test_missing_optional_archive_is_not_platform_filtered(self) -> None:
        package = self.document["packages"][self.second_path]
        (self.archives / bundle.archive_filename(package["resolved"])).unlink()
        self._rejected("missing archives")

    def test_extra_file_and_subdirectory_are_rejected(self) -> None:
        for extra in ("unexpected.tgz", "unexpected-directory"):
            with self.subTest(extra=extra):
                path = self.archives / extra
                if extra.endswith(".tgz"):
                    path.write_bytes(b"extra")
                else:
                    path.mkdir()
                self._rejected("extra archives")
                path.unlink() if path.is_file() else path.rmdir()

    def test_tampered_original_compressed_archive_is_rejected(self) -> None:
        filename = bundle.archive_filename(self.document["packages"][self.first_path]["resolved"])
        path = self.archives / filename
        path.write_bytes(path.read_bytes() + b"tampered")
        self._rejected("differs from locked SRI")

    def test_unsupported_lock_formats_and_root_identity_are_rejected(self) -> None:
        for version in (1, 2, True, "3"):
            with self.subTest(version=version):
                self.document["lockfileVersion"] = version
                self._rejected("unsupported lockfile format")
        self.document["lockfileVersion"] = 3
        self.document["packages"][""]["name"] = "other-project"
        self._rejected("root package identity")

    def test_link_git_unpinned_and_malformed_record_inputs_are_rejected(self) -> None:
        original = dict(self.document["packages"][self.first_path])
        mutations = ({"link": True}, {"inBundle": True}, {"resolved": "git+https://example.invalid/source#abc"}, {"version": "^1.2.3"}, {"integrity": ""}, {"name": "different"}, {"dev": 1})
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.document["packages"][self.first_path] = {**original, **mutation}
                self._rejected()

    def test_registry_url_canonical_identity_is_required(self) -> None:
        original = dict(self.document["packages"][self.first_path])
        urls = (
            "http://registry.npmjs.org/alpha/-/alpha-1.2.3.tgz",
            "https://registry.npmjs.org:443/alpha/-/alpha-1.2.3.tgz",
            "https://user@registry.npmjs.org/alpha/-/alpha-1.2.3.tgz",
            "https://registry.npmjs.org.other.invalid/alpha/-/alpha-1.2.3.tgz",
            "https://registry.npmjs.org/alpha/-/alpha-1.2.3.tgz?token=unused",
            "https://registry.npmjs.org/alpha/-/alpha-1.2.3.tgz#fragment",
            "https://registry.npmjs.org/alpha/-/alpha-1.2.3.tgz?",
            "https://registry.npmjs.org/alpha/-/alpha-1.2.3.tgz#",
            "https://registry.npmjs.org/alpha/-/alpha-9.0.0.tgz",
            "https://registry.npmjs.org/alpha/-/other-1.2.3.tgz",
            "https://registry.npmjs.org/%61lpha/-/alpha-1.2.3.tgz",
            "https://registry.npmjs.org/alpha/../alpha/-/alpha-1.2.3.tgz",
            "https://[",
        )
        for url in urls:
            with self.subTest(url=url):
                self.document["packages"][self.first_path] = {**original, "resolved": url}
                self._rejected()

    def test_scoped_and_nested_path_grammar_is_required(self) -> None:
        package = self.document["packages"].pop(self.second_path)
        paths = ("@scope/beta", "node_modules/@scope", "node_modules/alpha/@scope/beta", "node_modules/alpha/node_modules/@scope/beta/extra", "node_modules/../beta", "node_modules//beta", "node_modules\\beta")
        for path in paths:
            with self.subTest(path=path):
                self.document["packages"][path] = package
                self._rejected()
                del self.document["packages"][path]

    def test_invalid_sri_encoding_algorithm_and_length_are_rejected(self) -> None:
        for value in ("sha512-%%%", "sha512-YQ==", "md5-" + base64.b64encode(b"x" * 16).decode(), "sha512-" + "a" * 86, "sha512-" + base64.b64encode(b"x" * 64).decode() + "?options"):
            with self.subTest(value=value):
                self.document["packages"][self.first_path]["integrity"] = value
                self._rejected("SRI")

    def test_sha1_only_pin_is_not_source_authentication(self) -> None:
        package = self.document["packages"][self.first_path]
        payload = (self.archives / bundle.archive_filename(package["resolved"])).read_bytes()
        package["integrity"] = _sri(payload, "sha1")
        self._rejected("SHA-1-only")

    def test_sri_uses_strongest_algorithm_and_accepts_matching_alternative(self) -> None:
        package = self.document["packages"][self.first_path]
        payload = (self.archives / bundle.archive_filename(package["resolved"])).read_bytes()
        package["integrity"] = _sri(payload, "sha1") + " " + _sri(b"wrong", "sha512")
        self._rejected("differs from locked SRI")
        package["integrity"] = _sri(b"wrong", "sha1") + " " + _sri(payload, "sha512")
        self._write_lock()
        self.assertTrue(self._verify()["archives_verified"])
        package["integrity"] = _sri(b"wrong", "sha512") + " " + _sri(payload, "sha512")
        self._write_lock()
        self.assertTrue(self._verify()["archives_verified"])

    def test_manifest_name_version_missing_and_duplicate_keys_are_rejected(self) -> None:
        for manifest in ({"name": "different", "version": "1.2.3"}, {"name": "alpha", "version": "9.0.0"}, b'{"name":"alpha","name":"alpha","version":"1.2.3"}'):
            with self.subTest(manifest=manifest):
                self._archive(self.first_path, manifest=manifest)
                self._rejected("package.json")
        self._archive(self.first_path, include_manifest=False)
        self._rejected("package.json identity")

    def test_duplicate_lock_json_keys_and_nonfinite_json_are_rejected(self) -> None:
        original = json.dumps(self.document)
        for contents in (original.replace('"lockfileVersion": 3', '"lockfileVersion": 3, "lockfileVersion": 3'), original.replace('"requires": true', '"requires": NaN'), original.replace('"license": "MIT"', '"license": 1e999')):
            with self.subTest(contents=contents):
                self.lock.write_text(contents, encoding="utf-8")
                with self.assertRaisesRegex(bundle.BundleVerificationError, "invalid JSON"):
                    self._verify()

    def test_unsafe_archive_paths_are_rejected_before_normalization(self) -> None:
        paths = ("outside/file", "/package/file", "package/../file", "package/./file", "package//file", "package\\file", "package/C:/file", "package/file:stream", "package/file.", "package/file ", "package/NUL.txt", "package/file\nname", "package/trailing/")
        for path in paths:
            with self.subTest(path=path):
                info = tarfile.TarInfo(path)
                info.size = 1
                self._archive(self.first_path, additional=[(info, b"x")])
                self._rejected()

    def test_links_and_special_archive_members_are_rejected(self) -> None:
        for member_type in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE, tarfile.BLKTYPE, tarfile.GNUTYPE_SPARSE):
            with self.subTest(member_type=member_type):
                info = tarfile.TarInfo("package/special")
                info.type = member_type
                info.linkname = "package/package.json"
                self._archive(self.first_path, additional=[(info, b"")])
                self._rejected("special|invalid gzip")

    def test_duplicate_members_and_implicit_path_aliases_are_rejected(self) -> None:
        fixtures = (
            {"package/LICENSE-MIT": b"duplicate"},
            {"package/Foo/x.js": b"x", "package/foo/y.js": b"y"},
            {"package/a": b"file", "package/a/b.js": b"child"},
            {"package/\u00e9/x.js": b"x", "package/e\u0301/y.js": b"y"},
        )
        for files in fixtures:
            with self.subTest(files=files):
                additional = []
                for path, data in files.items():
                    info = tarfile.TarInfo(path)
                    info.size = len(data)
                    additional.append((info, data))
                self._archive(self.first_path, additional=additional)
                self._rejected("duplicate|aliased|ancestor")

    def test_safe_directory_members_and_zero_byte_files_are_retained(self) -> None:
        directory = tarfile.TarInfo("package/empty/")
        directory.type = tarfile.DIRTYPE
        directory.mode = 0o755
        empty = tarfile.TarInfo("package/empty/zero")
        self._archive(self.first_path, additional=[(directory, b""), (empty, b"")])
        self._write_lock()
        record = next(record for record in self._verify()["packages"] if record["name"] == "alpha")
        members = {member["path"]: member for member in record["members"]}
        self.assertEqual(members["package/empty"]["kind"], "directory")
        self.assertEqual(members["package/empty/zero"]["sha256"], hashlib.sha256(b"").hexdigest())

    def test_original_regular_file_mode_bits_are_preserved_as_evidence(self) -> None:
        package = self.document["packages"][self.first_path]
        archive_path = self.archives / bundle.archive_filename(package["resolved"])
        raw_tar = bytearray(gzip.decompress(archive_path.read_bytes()))
        with tarfile.open(fileobj=io.BytesIO(raw_tar), mode="r:") as archive:
            offset = archive.getmember("package/index.js").offset
        raw_tar[offset + 100:offset + 108] = b"0100644\x00"
        raw_tar[offset + 148:offset + 156] = b" " * 8
        checksum = sum(raw_tar[offset:offset + tarfile.BLOCKSIZE])
        raw_tar[offset + 148:offset + 156] = f"{checksum:06o}\x00 ".encode("ascii")
        payload = gzip.compress(bytes(raw_tar))
        archive_path.write_bytes(payload)
        package["integrity"] = _sri(payload)
        self._write_lock()
        record = next(record for record in self._verify()["packages"] if record["name"] == "alpha")
        member = next(member for member in record["members"] if member["path"] == "package/index.js")
        self.assertEqual(member["archive_mode"], 0o100644)

    def test_invalid_gzip_tar_with_matching_new_lock_digest_is_rejected(self) -> None:
        package = self.document["packages"][self.first_path]
        payload = b"not a gzip tar archive"
        package["integrity"] = _sri(payload)
        (self.archives / bundle.archive_filename(package["resolved"])).write_bytes(payload)
        self._rejected("invalid gzip tar archive")

    def test_gzip_crc_and_hidden_trailing_tar_data_are_verified(self) -> None:
        package = self.document["packages"][self.first_path]
        archive_path = self.archives / bundle.archive_filename(package["resolved"])
        original = archive_path.read_bytes()
        corrupted = bytearray(original)
        corrupted[-8] ^= 1
        alternatives = (bytes(corrupted), gzip.compress(gzip.decompress(original) + b"hidden trailing data"))
        for payload in alternatives:
            with self.subTest(payload=payload[:12]):
                archive_path.write_bytes(payload)
                package["integrity"] = _sri(payload)
                self._rejected("invalid gzip|padding")

    def test_linked_archive_and_linked_ancestor_are_rejected(self) -> None:
        filename = bundle.archive_filename(self.document["packages"][self.first_path]["resolved"])
        archive = self.archives / filename
        original = archive.read_bytes()
        outside = self.root / "outside.tgz"
        outside.write_bytes(original)
        archive.unlink()
        try:
            archive.symlink_to(outside)
        except OSError:
            archive.write_bytes(original)
            self.skipTest("filesystem does not permit symbolic links")
        self._rejected("linked")
        alias = self.root / "archive-alias"
        alias.symlink_to(self.archives, target_is_directory=True)
        with self.assertRaisesRegex(bundle.BundleVerificationError, "linked"):
            bundle.verify_bundle(self.lock, alias)

    def test_windows_reparse_metadata_is_always_linked(self) -> None:
        metadata = type("Metadata", (), {"st_mode": stat.S_IFDIR, "st_file_attributes": getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)})()
        with mock.patch.object(bundle.stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400, create=True):
            self.assertTrue(bundle._is_link(metadata))

    def test_regular_input_changed_while_reading_is_rejected(self) -> None:
        original = bundle._check_path
        calls = 0

        def check(path, *, directory, label):
            nonlocal calls
            if Path(path) == self.lock:
                calls += 1
                if calls == 2:
                    self.lock.write_bytes(self.lock.read_bytes() + b" ")
            return original(path, directory=directory, label=label)

        with mock.patch.object(bundle, "_check_path", side_effect=check):
            with self.assertRaisesRegex(bundle.BundleVerificationError, "changed while reading"):
                self._verify()

    def test_windows_ctime_semantics_keep_same_api_mutation_guards(self) -> None:
        before = SimpleNamespace(st_ctime_ns=10)
        current = SimpleNamespace(st_ctime_ns=10)
        opened = SimpleNamespace(st_ctime_ns=20)
        after = SimpleNamespace(st_ctime_ns=20)
        self.assertTrue(bundle._stable_change_times(before, opened, after, current, windows=True))
        self.assertFalse(bundle._stable_change_times(before, opened, after, current, windows=False))
        self.assertFalse(bundle._stable_change_times(before, opened, SimpleNamespace(st_ctime_ns=21), current, windows=True))
        self.assertFalse(bundle._stable_change_times(before, opened, after, SimpleNamespace(st_ctime_ns=11), windows=True))

    def test_archive_directory_changes_during_verification_are_rejected(self) -> None:
        original = bundle._verify_archive

        def verify(payload, identity):
            record = original(payload, identity)
            (self.archives / "inserted.tgz").write_bytes(b"extra")
            return record

        with mock.patch.object(bundle, "_verify_archive", side_effect=verify):
            with self.assertRaisesRegex(bundle.BundleVerificationError, "directory changed"):
                self._verify()

    def test_verified_archive_changed_later_in_the_bundle_is_rejected(self) -> None:
        original = bundle._verify_archive
        previous = None

        def verify(payload, identity):
            nonlocal previous
            record = original(payload, identity)
            if previous is not None:
                previous.write_bytes(previous.read_bytes() + b"changed after verification")
            previous = self.archives / identity["archive_filename"]
            return record

        with mock.patch.object(bundle, "_verify_archive", side_effect=verify):
            with self.assertRaisesRegex(bundle.BundleVerificationError, "archive changed|directory changed"):
                self._verify()

    def test_cli_prints_or_exclusively_writes_evidence_and_fails_closed(self) -> None:
        arguments = ["--lock", str(self.lock), "--archives", str(self.archives)]
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(bundle.main(arguments), 0)
        self.assertFalse(json.loads(output.getvalue())["frontend_source_complete"])
        destination = self.root / "evidence.json"
        self.assertEqual(bundle.main(arguments + ["--output", str(destination)]), 0)
        contents = destination.read_bytes()
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(bundle.main(arguments + ["--output", str(destination)]), 1)
        self.assertEqual(destination.read_bytes(), contents)
        inside_archives = self.archives / "evidence.json"
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(bundle.main(arguments + ["--output", str(inside_archives)]), 1)
        self.assertFalse(inside_archives.exists())
        self.document["packages"][self.first_path]["integrity"] = _sri(b"wrong")
        self._write_lock()
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(bundle.main(arguments), 1)


if __name__ == "__main__":
    unittest.main()
