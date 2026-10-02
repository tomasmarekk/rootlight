#!/usr/bin/env python3
"""Regression tests for lock-bound Cargo source closure without Cargo or networking.

Synthetic archives are original inputs so expanded-tree tampering is tested
independently of the generated Cargo checksum manifest.
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import stat
import tarfile
import tempfile
import unittest


MODULE_PATH = Path(__file__).with_name("verify_cargo_source_bundle.py")
SPEC = importlib.util.spec_from_file_location("verify_cargo_source_bundle", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)


class CargoSourceBundleTests(unittest.TestCase):
    """Require exact, portable source evidence and fail-closed invalid inputs."""

    def setUp(self) -> None:
        """Create isolated source, archive, and lock inputs for every test."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.vendor = self.root / "vendor"
        self.archives = self.root / "archives"
        self.vendor.mkdir()
        self.archives.mkdir()
        self.first = self._package("first", "1.2.3")
        self.second = self._package("second", "2.0.0", license_file="legal/custom.txt")
        self.lock = self._lock("Cargo.lock", [self.first, self.second])

    def _package(
        self, name: str, version: str, *, license_file: str | None = None
    ) -> dict[str, str]:
        directory = self.vendor / f"{name}-{version}"
        directory.mkdir()
        manifest = f'[package]\nname = "{name}"\nversion = "{version}"\nlicense = "MIT OR Apache-2.0"\n'
        if license_file:
            manifest += f'license-file = "{license_file}"\n'
        files = {
            "Cargo.toml": manifest.encode(),
            "src/lib.rs": b"pub fn value() -> u8 { 1 }\n",
            "LICENSE-MIT": b"retained synthetic upstream notice\n",
            "scripts/build.sh": b"#!/bin/sh\nexit 0\n",
        }
        if license_file:
            files[license_file] = b"declared synthetic upstream license\n"
        for path, content in files.items():
            output = directory / path
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(content)
        checksum = self._archive(name, version, files)
        (directory / ".cargo-checksum.json").write_text(
            json.dumps({"package": checksum, "files": {path: _hash(data) for path, data in files.items()}}),
            encoding="utf-8",
        )
        return {"name": name, "version": version, "source": bundle.CRATES_IO_SOURCE, "checksum": checksum}

    def _archive(
        self,
        name: str,
        version: str,
        files: dict[str, bytes],
        *,
        additional: list[tarfile.TarInfo] | None = None,
        modes: dict[str, int] | None = None,
    ) -> str:
        contents = io.BytesIO()
        raw_modes = {}
        with tarfile.open(fileobj=contents, mode="w") as archive:
            for path, content in sorted(files.items()):
                info = tarfile.TarInfo(f"{name}-{version}/{path}")
                info.size = len(content)
                info.mode = (modes or {}).get(path, 0o755 if path.endswith(".sh") else 0o644)
                raw_modes[info.name] = info.mode
                archive.addfile(info, io.BytesIO(content))
            for member in additional or []:
                raw_modes[member.name.removesuffix("/") if member.isdir() else member.name] = member.mode
                archive.addfile(member, io.BytesIO(b""))
        payload = _preserve_raw_modes(contents.getvalue(), raw_modes)
        (self.archives / f"{name}-{version}.crate").write_bytes(payload)
        return _hash(payload)

    def _lock(self, name: str, packages: list[dict[str, str]]) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        sections = ["version = 4\n"]
        for package in packages:
            sections.append("[[package]]\n" + "".join(f"{key} = {json.dumps(value)}\n" for key, value in package.items()))
        path.write_text("\n".join(sections), encoding="utf-8")
        return path

    def _checksum_document(self, name: str = "first-1.2.3") -> dict:
        return json.loads((self.vendor / name / ".cargo-checksum.json").read_bytes())

    def _write_checksums(self, document: dict, name: str = "first-1.2.3") -> None:
        (self.vendor / name / ".cargo-checksum.json").write_text(json.dumps(document), encoding="utf-8")

    def _verify(self, *, locks: list[Path] | None = None, archives: bool = True) -> dict:
        return bundle.verify_bundle(self.vendor, locks or [self.lock], archives=self.archives if archives else None)

    def _original_files(self) -> dict[str, bytes]:
        return {path: (self.vendor / "first-1.2.3" / path).read_bytes() for path in self._checksum_document()["files"]}

    def _pin_changed_archive(self, checksum: str) -> None:
        self.first["checksum"] = checksum
        self._lock("Cargo.lock", [self.first, self.second])
        document = self._checksum_document()
        document["package"] = checksum
        self._write_checksums(document)

    def test_good_union_overlap_and_local_package_evidence(self) -> None:
        local = {"name": "workspace", "version": "0.1.0"}
        first_lock = self._lock("Cargo.lock", [self.first, local])
        second_lock = self._lock("tests/native/Cargo.lock", [self.first, self.second, local])
        evidence = self._verify(locks=[second_lock, first_lock])
        self.assertEqual(evidence, self._verify(locks=[first_lock, second_lock]))
        self.assertTrue(evidence["archives_verified"])
        self.assertEqual(evidence["package_count"], 2)
        self.assertEqual(evidence["local_package_count"], 1)
        self.assertEqual(evidence["local_packages"], [local])
        self.assertEqual([record["path"] for record in evidence["lockfiles"]], ["Cargo.lock", "tests/native/Cargo.lock"])
        self.assertNotIn(str(self.root), json.dumps(evidence))
        record = evidence["packages"][1]
        self.assertEqual(record["license"], "MIT OR Apache-2.0")
        self.assertEqual(record["license_file"], "legal/custom.txt")
        self.assertEqual({item["path"] for item in record["license_files"]}, {"LICENSE-MIT", "legal/custom.txt"})
        self.assertEqual(record["archive_executable_files"], ["scripts/build.sh"])
        self.assertEqual(record["archive"]["sha256"], self.second["checksum"])
        self.assertEqual({item["path"]: item["archive_mode"] for item in record["files"]}["scripts/build.sh"], 0o755)

    def test_without_original_archives_does_not_claim_archive_verification(self) -> None:
        evidence = self._verify(archives=False)
        self.assertFalse(evidence["archives_verified"])
        self.assertNotIn("archive", evidence["packages"][0])
        self.assertNotIn("archive_mode", evidence["packages"][0]["files"][0])

    def test_lock_hash_binds_changes_even_when_graph_does_not_change(self) -> None:
        before = self._verify()
        with self.lock.open("a", encoding="utf-8") as output:
            output.write("\n# changed lock bytes\n")
        after = self._verify()
        self.assertNotEqual(before["lockfiles"][0]["sha256"], after["lockfiles"][0]["sha256"])
        self.assertEqual(before["packages"], after["packages"])

    def test_rejects_altered_lock_and_conflicting_union_checksums(self) -> None:
        changed = {**self.first, "checksum": "a" * 64}
        extra_lock = self._lock("nested/Cargo.lock", [changed])
        with self.assertRaisesRegex(bundle.BundleVerificationError, "conflicting lock checksums"):
            self._verify(locks=[self.lock, extra_lock])
        with self.assertRaisesRegex(bundle.BundleVerificationError, "package checksum differs"):
            self._verify(locks=[extra_lock])

    def test_rejects_duplicate_packages_and_duplicate_lock_input(self) -> None:
        duplicate = self._lock("duplicate.lock", [self.first, self.first])
        with self.assertRaisesRegex(bundle.BundleVerificationError, "duplicate package"):
            self._verify(locks=[duplicate])
        with self.assertRaisesRegex(bundle.BundleVerificationError, "duplicate lockfile"):
            self._verify(locks=[self.lock, self.lock])

    def test_rejects_unknown_registry_git_and_local_checksum(self) -> None:
        for source in ("git+https://example.invalid/repo#" + "a" * 40, "registry+https://example.invalid/index", "sparse+https://index.crates.io/"):
            with self.subTest(source=source):
                lock = self._lock("unknown.lock", [{**self.first, "source": source}])
                with self.assertRaisesRegex(bundle.BundleVerificationError, "unsupported source"):
                    self._verify(locks=[lock])
        lock = self._lock("local.lock", [{key: value for key, value in self.first.items() if key != "source"}])
        with self.assertRaisesRegex(bundle.BundleVerificationError, "local package has a checksum"):
            self._verify(locks=[lock])

    def test_rejects_invalid_lock_and_package_identities(self) -> None:
        self.lock.write_text("not valid toml", encoding="utf-8")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "invalid TOML"):
            self._verify()
        for field, value in (("name", "../escape"), ("version", "1/../../escape"), ("checksum", "A" * 64)):
            with self.subTest(field=field):
                self._lock("Cargo.lock", [{**self.first, field: value}])
                with self.assertRaises(bundle.BundleVerificationError):
                    self._verify()

    def test_rejects_missing_extra_and_duplicate_vendor_packages(self) -> None:
        directory = self.vendor / "first-1.2.3"
        removed = self.root / "removed"
        directory.rename(removed)
        with self.assertRaisesRegex(bundle.BundleVerificationError, "missing vendor packages"):
            self._verify()
        removed.rename(directory)
        self._package("extra", "1.0.0")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "extra vendor package"):
            self._verify()
        (self.vendor / "extra-1.0.0").rename(self.root / "extra")
        shutil.copytree(directory, self.vendor / "copy")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "duplicate vendor package"):
            self._verify()

    def test_rejects_missing_extra_and_tampered_files(self) -> None:
        source = self.vendor / "first-1.2.3/src/lib.rs"
        original = source.read_bytes()
        source.unlink()
        with self.assertRaisesRegex(bundle.BundleVerificationError, "missing files"):
            self._verify()
        source.write_bytes(original)
        extra = source.parent / "unlisted.rs"
        extra.write_bytes(b"unreviewed source")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "extra files"):
            self._verify()
        extra.unlink()
        source.write_bytes(b"changed source")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "source file checksum differs"):
            self._verify()

    def test_rejects_checksum_traversal_aliases_and_self_reference(self) -> None:
        original = self._checksum_document()
        for path in ("../escape", "/absolute", "src/../escape", "src//lib.rs", "src/./lib.rs", "C:/escape", "src\\lib.rs", "LICENSE:stream", "NUL.txt", "src/trailing.", "src/trailing ", ".cargo-checksum.json"):
            with self.subTest(path=path):
                document = {**original, "files": {**original["files"], path: "a" * 64}}
                self._write_checksums(document)
                with self.assertRaises(bundle.BundleVerificationError):
                    self._verify()

    def test_rejects_invalid_digest_and_duplicate_checksum_keys(self) -> None:
        document = self._checksum_document()
        document["files"]["src/lib.rs"] = "not a digest"
        self._write_checksums(document)
        with self.assertRaisesRegex(bundle.BundleVerificationError, "expected lowercase SHA-256"):
            self._verify()
        (self.vendor / "first-1.2.3/.cargo-checksum.json").write_text('{"package":"x","package":"y","files":{}}', encoding="utf-8")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "duplicate checksum key"):
            self._verify()

    def test_rejects_license_file_outside_package_or_absent(self) -> None:
        manifest = self.vendor / "second-2.0.0/Cargo.toml"
        original = manifest.read_text(encoding="utf-8")
        for path in ("../outside.txt", "legal/absent.txt"):
            with self.subTest(path=path):
                manifest.write_text(original.replace("legal/custom.txt", path), encoding="utf-8")
                document = self._checksum_document("second-2.0.0")
                document["files"]["Cargo.toml"] = _hash(manifest.read_bytes())
                self._write_checksums(document, "second-2.0.0")
                with self.assertRaises(bundle.BundleVerificationError):
                    self._verify(archives=False)

    def test_rejects_filesystem_symlinks(self) -> None:
        source = self.vendor / "first-1.2.3/src/lib.rs"
        target = self.root / "outside.rs"
        source.rename(target)
        try:
            source.symlink_to(target)
        except OSError as error:
            self.skipTest(f"symlink creation is unavailable: {error}")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "links are prohibited"):
            self._verify()

    def test_rejects_package_root_and_checksum_symlinks(self) -> None:
        path = self.vendor / "first-1.2.3"
        target = self.root / "outside-package"
        path.rename(target)
        try:
            path.symlink_to(target, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"symlink creation is unavailable: {error}")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "non-link directory"):
            self._verify()
        path.unlink()
        target.rename(path)
        checksum = path / ".cargo-checksum.json"
        outside = self.root / "outside-checksum.json"
        checksum.rename(outside)
        checksum.symlink_to(outside)
        with self.assertRaisesRegex(bundle.BundleVerificationError, "non-link regular file"):
            self._verify()

    def test_rejects_missing_extra_and_corrupt_archives(self) -> None:
        path = self.archives / "first-1.2.3.crate"
        original = path.read_bytes()
        path.unlink()
        with self.assertRaisesRegex(bundle.BundleVerificationError, "missing archives"):
            self._verify()
        path.write_bytes(original)
        extra = self.archives / "extra.crate"
        extra.write_bytes(b"extra")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "extra archives"):
            self._verify()
        extra.unlink()
        path.write_bytes(original + b"changed")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "archive .* checksum differs"):
            self._verify()

    def test_original_archives_reject_consistently_rewritten_vendor_checksums(self) -> None:
        source = self.vendor / "first-1.2.3/src/lib.rs"
        source.write_bytes(b"unreviewed modified source")
        document = self._checksum_document()
        document["files"]["src/lib.rs"] = _hash(source.read_bytes())
        self._write_checksums(document)
        self.assertFalse(self._verify(archives=False)["archives_verified"])
        with self.assertRaisesRegex(bundle.BundleVerificationError, "content differs from vendor"):
            self._verify()

    def test_rejects_archive_file_inventory_mismatch(self) -> None:
        files = self._original_files()
        files["extra.rs"] = b"extra source"
        self._pin_changed_archive(self._archive("first", "1.2.3", files))
        with self.assertRaisesRegex(bundle.BundleVerificationError, "file set differs"):
            self._verify()

    def test_rejects_archive_traversal_wrong_prefix_and_duplicate_members(self) -> None:
        files = self._original_files()
        for name in ("../escape", "/absolute", "first-1.2.3/../escape", "other-1.0.0/file", "first-1.2.3/src/lib.rs"):
            with self.subTest(name=name):
                member = tarfile.TarInfo(name)
                checksum = self._archive("first", "1.2.3", files, additional=[member])
                self._pin_changed_archive(checksum)
                with self.assertRaises(bundle.BundleVerificationError):
                    self._verify()

    def test_rejects_archive_links_and_special_members(self) -> None:
        files = self._original_files()
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE, tarfile.BLKTYPE, tarfile.CONTTYPE):
            with self.subTest(kind=kind):
                member = tarfile.TarInfo("first-1.2.3/special")
                member.type = kind
                member.linkname = "src/lib.rs"
                checksum = self._archive("first", "1.2.3", files, additional=[member])
                self._pin_changed_archive(checksum)
                with self.assertRaisesRegex(bundle.BundleVerificationError, "nonregular member"):
                    self._verify()

    def test_preserves_archive_modes_without_using_host_execute_bits(self) -> None:
        files = self._original_files()
        checksum = self._archive("first", "1.2.3", files, modes={"scripts/build.sh": 0o644, "src/lib.rs": 0o751})
        self._pin_changed_archive(checksum)
        evidence = self._verify()
        record = evidence["packages"][0]
        self.assertEqual(record["archive_executable_files"], ["src/lib.rs"])
        self.assertEqual({item["path"]: item["archive_mode"] for item in record["files"]}["src/lib.rs"], 0o751)

    def test_accepts_matching_stat_type_bits_and_preserves_raw_archive_modes(self) -> None:
        files = self._original_files()
        directory = tarfile.TarInfo("first-1.2.3/")
        directory.type = tarfile.DIRTYPE
        directory.mode = stat.S_IFDIR | 0o755
        checksum = self._archive(
            "first", "1.2.3", files, additional=[directory],
            modes={"Cargo.toml": stat.S_IFREG | 0o644, "scripts/build.sh": stat.S_IFREG | 0o755},
        )
        self._pin_changed_archive(checksum)
        record = self._verify()["packages"][0]
        modes = {item["path"]: item["archive_mode"] for item in record["files"]}
        self.assertEqual(modes["Cargo.toml"], 0o100644)
        self.assertEqual(modes["scripts/build.sh"], 0o100755)
        self.assertEqual(record["archive_executable_files"], ["scripts/build.sh"])

    def test_rejects_conflicting_or_unknown_archive_mode_type_bits(self) -> None:
        files = self._original_files()
        for mode in (stat.S_IFDIR | 0o644, stat.S_IFCHR | 0o644, 0o200644, -1):
            with self.subTest(mode=mode):
                checksum = self._archive("first", "1.2.3", files, modes={"src/lib.rs": mode})
                self._pin_changed_archive(checksum)
                with self.assertRaisesRegex(bundle.BundleVerificationError, "invalid member mode"):
                    self._verify()
        directory = tarfile.TarInfo("first-1.2.3/")
        directory.type = tarfile.DIRTYPE
        directory.mode = stat.S_IFREG | 0o755
        self._pin_changed_archive(self._archive("first", "1.2.3", files, additional=[directory]))
        with self.assertRaisesRegex(bundle.BundleVerificationError, "invalid member mode"):
            self._verify()

    def test_rejects_malformed_archive_even_when_lock_hash_matches(self) -> None:
        path = self.archives / "first-1.2.3.crate"
        path.write_bytes(b"not a tar archive")
        self._pin_changed_archive(_hash(path.read_bytes()))
        with self.assertRaisesRegex(bundle.BundleVerificationError, "invalid tar archive"):
            self._verify()

    def test_rejects_archive_symlinks(self) -> None:
        path = self.archives / "first-1.2.3.crate"
        target = self.root / "outside.crate"
        path.rename(target)
        try:
            path.symlink_to(target)
        except OSError as error:
            self.skipTest(f"symlink creation is unavailable: {error}")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "non-link regular file"):
            self._verify()

    def test_rejects_vendor_root_symlink(self) -> None:
        target = self.root / "outside-vendor"
        self.vendor.rename(target)
        try:
            self.vendor.symlink_to(target, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"symlink creation is unavailable: {error}")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "non-link directory"):
            self._verify()

    def test_requires_lock_input_and_regular_lock_file(self) -> None:
        with self.assertRaisesRegex(bundle.BundleVerificationError, "at least one lockfile"):
            bundle.verify_bundle(self.vendor, [])
        target = self.root / "outside.lock"
        self.lock.rename(target)
        try:
            self.lock.symlink_to(target)
        except OSError as error:
            self.skipTest(f"symlink creation is unavailable: {error}")
        with self.assertRaisesRegex(bundle.BundleVerificationError, "non-link regular file"):
            self._verify()

    def test_cli_writes_evidence_and_fails_without_replacing_output(self) -> None:
        output = self.root / "evidence.json"
        args = ["--vendor", str(self.vendor), "--lock", str(self.lock), "--archives", str(self.archives), "--output", str(output)]
        self.assertEqual(bundle.main(args), 0)
        original = output.read_bytes()
        self.assertEqual(json.loads(original), self._verify())
        with contextlib.redirect_stderr(io.StringIO()) as error:
            self.assertEqual(bundle.main(args), 1)
        self.assertIn("error:", error.getvalue())
        self.assertEqual(output.read_bytes(), original)
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            self.assertEqual(bundle.main(args[:-2]), 0)
        self.assertEqual(json.loads(stdout.getvalue()), self._verify())
        (self.vendor / "first-1.2.3/src/lib.rs").write_bytes(b"invalid")
        with contextlib.redirect_stderr(io.StringIO()) as error:
            self.assertEqual(bundle.main(args[:-2]), 1)
        self.assertIn("source file checksum differs", error.getvalue())


def _hash(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def _preserve_raw_modes(contents: bytes, modes: dict[str, int]) -> bytes:
    # Python's tar writer masks stat type bits; these fixtures must retain the
    # legacy headers found in original published crates, with valid tar checksums.
    payload = bytearray(contents)
    with tarfile.open(fileobj=io.BytesIO(contents), mode="r:") as archive:
        for member in archive:
            key = member.name.removesuffix("/") if member.isdir() else member.name
            mode = modes[key]
            encoded = mode.to_bytes(8, "big", signed=True) if mode < 0 else f"{mode:07o}\0".encode()
            start = member.offset
            payload[start + 100:start + 108] = encoded
            payload[start + 148:start + 156] = b" " * 8
            checksum = sum(payload[start:start + 512])
            payload[start + 148:start + 156] = f"{checksum:06o}\0 ".encode()
    return bytes(payload)


if __name__ == "__main__":
    unittest.main()
