"""Authored source/provenance regressions for local producer reverification.

Fixtures use inert synthetic tar/npm inputs; no published source, installer or
network is executed. The reviewed archive pin is replaced only inside each test.
"""

from __future__ import annotations

import base64
import gzip
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
_PROJECT = next(parent for parent in _HERE.parents if (parent / "scripts/verify_local_npm_sources.py").is_file())
sys.path.insert(0, str(_PROJECT / "scripts"))
sys.path.insert(0, str(_HERE))
import verify_lightning_producer_sources as verifier


def archive(files: dict[str, bytes], extras: list[tarfile.TarInfo] = ()) -> bytes:
    """Return authored inert tar bytes with explicit modes and optional unsafe rows."""
    result = io.BytesIO()
    with tarfile.open(fileobj=result, mode="w", format=tarfile.PAX_FORMAT) as bundle:
        root = tarfile.TarInfo(verifier.ROOT)
        root.type, root.mode = tarfile.DIRTYPE, 0o775
        bundle.addfile(root)
        for name, content in sorted(files.items()):
            item = tarfile.TarInfo(name)
            item.size = len(content)
            item.mode = 0o775 if name.endswith("build.js") else 0o664
            bundle.addfile(item, io.BytesIO(content))
        for item in extras:
            bundle.addfile(item, io.BytesIO(b"x" * item.size))
    return gzip.compress(result.getvalue(), mtime=0)


def authored_files() -> dict[str, bytes]:
    """Return complete small authored manifest/source/license/patch controls."""
    files = {name: b"retained input\n" for name in verifier.REQUIRED}
    files["package.json"] = verifier.json_bytes({"name": verifier.PACKAGE, "version": verifier.VERSION})
    for name in ("Cargo.toml", "node/Cargo.toml", "napi/Cargo.toml"):
        files[name] = b'[package]\nname = "authored-producer"\nversion = "0.1.0"\n'
    files["Cargo.lock"] = (b'version = 4\n[[package]]\nname = "authored-input"\nversion = "1.0.0"\n'
                           b'source = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "'
                           + b"a" * 64 + b'"\n[[package]]\nname = "local-input"\nversion = "0.1.0"\n')
    files["yarn.lock"] = b'# yarn lockfile v1\n"authored-input@^1":\n  version "1.0.0"\n'
    files["src/value.rs"] = b"pub fn value() -> u8 { 1 }\n"
    files["patches/input.patch"] = b"retained authored patch\n"
    files["selectors/LICENSE"] = b"second retained license\n"
    files[".gitignore"] = b"src/value.rs\n"
    return {verifier.ROOT + "/" + name: value for name, value in files.items()}


class AuthoredFixture:
    """Build actual shared npm-verifier inputs and independent synthetic producer bytes."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.project = Path(self.temporary.name)
        self.prepared = self.project / "prepared"
        self.prepared.mkdir()
        self.capture = self.project / "capture.json"
        resolved = f"https://registry.npmjs.org/{verifier.PACKAGE}/-/{verifier.PACKAGE}-{verifier.VERSION}.tgz"
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w") as bundle:
            for name, content in {"package/package.json": verifier.json_bytes({
                "name": verifier.PACKAGE, "version": verifier.VERSION, "license": "MPL-2.0"}),
                "package/LICENSE": b"retained distribution license\n"}.items():
                info = tarfile.TarInfo(name)
                info.size, info.mode = len(content), 0o644
                bundle.addfile(info, io.BytesIO(content))
        npm = gzip.compress(buffer.getvalue(), mtime=0)
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(npm).digest()).decode()
        filename = verifier.local.original.archive_filename(resolved)
        npm_path = self.project / "source-inputs/npm/archives" / filename
        npm_path.parent.mkdir(parents=True)
        npm_path.write_bytes(npm)
        (self.project / "source-inputs/npm/local-distributions").mkdir()
        upstream = {"name": "authored-project", "version": "1.0.0", "lockfileVersion": 3,
                    "packages": {"": {"name": "authored-project", "version": "1.0.0"},
                                 "node_modules/lightningcss": {"version": verifier.VERSION,
                                 "resolved": resolved, "integrity": integrity, "license": "MPL-2.0"}}}
        upstream_bytes = verifier.json_bytes(upstream)
        (self.project / verifier.local.UPSTREAM_LOCK).write_bytes(upstream_bytes)
        canonical = json.loads(upstream_bytes)
        canonical["packages"]["node_modules/lightningcss"]["resolved"] = "file:../../../source-inputs/npm/archives/" + filename
        lock = self.project / verifier.local.CANONICAL_LOCK
        lock.parent.mkdir(parents=True)
        lock.write_bytes(verifier.json_bytes(canonical))
        (self.project / verifier.local.DISTRIBUTIONS_MANIFEST).write_bytes(verifier.json_bytes({
            "schema": "rootlight.npm-local-distributions/1", "upstream_lock_sha256": verifier.digest(upstream_bytes), "variants": []}))
        (self.project / verifier.NPM_REPORT).write_bytes(verifier.json_bytes(verifier.local.verify_local_bundle(self.project)))
        self.publisher = verifier.json_bytes({"name": verifier.PACKAGE, "version": verifier.VERSION,
            "gitHead": verifier.COMMIT, "repository": {"url": "git+https://github.com/parcel-bundler/lightningcss.git"},
            "dist": {"tarball": resolved, "integrity": integrity}})
        self.files = authored_files()
        self.payload = archive(self.files)
        pin = patch.object(verifier, "ARCHIVE_SHA256", verifier.digest(self.payload))
        pin.start()
        self.addCleanup(pin.stop)
        self.write_prepared()
        self.capture.write_bytes(verifier.json_bytes({
            "schema": "rootlight.publisher-response-capture/1", "requested_url": verifier.METADATA_URL,
            "status_code": 200, "content_type": "application/json", "payload_bytes": len(self.publisher),
            "payload_sha256": verifier.digest(self.publisher), "git_head": verifier.COMMIT,
            "observed_by_utc": "2026-01-02T03:04:05Z",
            "scope": "direct HTTPS response; proxy disabled and redirects denied; identity checked against exact locked package",
            "registry_signatures_verified": False, "source_to_binary_rebuild_verified": False}))
        parent = self.project / "source-inputs/producers"
        parent.mkdir()
        self.destination = parent / (verifier.PACKAGE + "-" + verifier.COMMIT)

    def write_prepared(self):
        """Retain authored bytes and a structurally exact preparation report."""
        (self.prepared / verifier.ARCHIVE_NAME).write_bytes(self.payload)
        (self.prepared / "publisher-metadata.json").write_bytes(self.publisher)
        report = verifier.prepared_report(verifier.publisher_association(self.project, self.publisher),
                                          verifier.inspect_archive(self.payload))
        (self.prepared / "producer-inputs.json").write_bytes(verifier.json_bytes(report))


class VerificationTests(AuthoredFixture, unittest.TestCase):
    def test_rederive_preserves_all_bytes_modes_licenses_and_raw_lock_scope(self):
        evidence, blobs, files = verifier.validate_prepared(self.project, self.prepared, self.capture)
        self.assertEqual(files, {name[len(verifier.ROOT) + 1:]: value for name, value in self.files.items()})
        self.assertEqual(blobs[verifier.ARCHIVE_NAME], self.payload)
        self.assertEqual(evidence["source_inventory"]["cargo_lock"]["external_package_count"], 1)
        self.assertEqual(len(evidence["source_inventory"]["cargo_lock"]["packages"]), 2)
        self.assertFalse(evidence["source_inventory"]["yarn_lock"]["edge_inventory_complete"])
        self.assertEqual(len(evidence["source_inventory"]["license_input_inventory"]), 2)
        modes = {row["path"]: row["archive_mode"] for row in evidence["source_inventory"]["members"]}
        self.assertEqual(modes[verifier.ROOT + "/scripts/build.js"], 0o775)
        self.assertEqual(modes[verifier.ROOT + "/src/value.rs"], 0o664)
        for key in verifier.FALSE_CLAIMS:
            self.assertIs(evidence[key], False)
        self.assertIs(evidence["publisher_capture"]["publisher_metadata_transport_verified"], False)

    def test_reviewed_pin_rejects_changed_archive_even_with_rederived_report(self):
        self.files[verifier.ROOT + "/src/value.rs"] += b"changed\n"
        self.payload = archive(self.files)
        self.write_prepared()
        with self.assertRaisesRegex(ValueError, "reviewed pin"):
            verifier.validate_prepared(self.project, self.prepared, self.capture)

    def test_report_unknown_keys_boolean_types_and_counterfeit_claims_reject(self):
        path = self.prepared / "producer-inputs.json"
        original = path.read_bytes()
        for key, value in [("unknown", False), ("frontend_source_complete", True),
                           ("producer_transitive_sources_complete", 0), ("archive_sha256_preverified", True)]:
            with self.subTest(key=key):
                report = json.loads(original)
                report[key] = value
                path.write_bytes(verifier.json_bytes(report))
                with self.assertRaisesRegex(ValueError, "rederived"):
                    verifier.validate_prepared(self.project, self.prepared, self.capture)
        path.write_bytes(original)

    def test_capture_unknown_binding_signature_or_date_fields_reject(self):
        original = self.capture.read_bytes()
        for key, value in [("unknown", False), ("payload_sha256", "0" * 64), ("status_code", True),
                           ("registry_signatures_verified", True), ("observed_by_utc", "2026-02-30T01:02:03Z")]:
            with self.subTest(key=key):
                data = json.loads(original)
                data[key] = value
                self.capture.write_bytes(verifier.json_bytes(data))
                with self.assertRaises(ValueError):
                    verifier.validate_prepared(self.project, self.prepared, self.capture)
        self.capture.write_bytes(original)

    def test_publisher_or_canonical_npm_pin_mutation_is_not_rebound(self):
        data = json.loads(self.publisher)
        data["gitHead"] = "0" * 40
        (self.prepared / "publisher-metadata.json").write_bytes(verifier.json_bytes(data))
        with self.assertRaises(ValueError):
            verifier.validate_prepared(self.project, self.prepared, self.capture)
        (self.prepared / "publisher-metadata.json").write_bytes(self.publisher)
        canonical = self.project / verifier.local.CANONICAL_LOCK
        data = json.loads(canonical.read_bytes())
        data["packages"]["node_modules/lightningcss"]["version"] = "2.0.0"
        canonical.write_bytes(verifier.json_bytes(data))
        with self.assertRaises(ValueError):
            verifier.validate_prepared(self.project, self.prepared, self.capture)

    def test_unsafe_archive_paths_links_modes_and_duplicate_aliases_reject(self):
        for path, kind in [(verifier.ROOT + "/../escape", tarfile.REGTYPE),
                           ("/absolute", tarfile.REGTYPE), (verifier.ROOT + "/bad\\path", tarfile.REGTYPE),
                           (verifier.ROOT + "/link", tarfile.SYMTYPE), (verifier.ROOT + "/hard", tarfile.LNKTYPE),
                           (verifier.ROOT + "/pipe", tarfile.FIFOTYPE),
                           (verifier.ROOT + "/src/VALUE.rs", tarfile.REGTYPE)]:
            with self.subTest(path=path, kind=kind):
                extra = tarfile.TarInfo(path)
                extra.type, extra.mode, extra.linkname = kind, 0o644, "src/value.rs"
                with self.assertRaises(ValueError):
                    verifier.inspect_archive(archive(self.files, [extra]))
        duplicate = tarfile.TarInfo(verifier.ROOT + "/src/value.rs")
        with self.assertRaises(ValueError):
            verifier.inspect_archive(archive(self.files, [duplicate]))
        special = tarfile.TarInfo(verifier.ROOT + "/mode")
        special.mode = 0o4755
        with self.assertRaises(ValueError):
            verifier.inspect_archive(archive(self.files, [special]))

    def test_unknown_cargo_fields_and_missing_authored_inputs_reject(self):
        for content in [b'version = 4\nunknown = true\npackage = []\n',
                        b'version = 4\n[[package]]\nname="x"\nversion="1.0.0"\nunknown=true\n',
                        b'version = 4\n[[package]]\nname="x"\nversion="1.0.0"\nsource="git+https://invalid/#' + b"a" * 40 + b'"\n']:
            files = dict(self.files)
            files[verifier.ROOT + "/Cargo.lock"] = content
            with self.assertRaises(ValueError):
                verifier.inspect_archive(archive(files))
        files = dict(self.files)
        del files[verifier.ROOT + "/LICENSE"]
        with self.assertRaises(ValueError):
            verifier.inspect_archive(archive(files))

    def test_limits_framing_and_input_inventory_fail_closed(self):
        with patch.object(verifier, "MAX_MEMBERS", 1):
            with self.assertRaises(ValueError):
                verifier.inspect_archive(self.payload)
        with patch.object(verifier, "MAX_COMPRESSED", len(self.payload) - 1):
            with self.assertRaises(ValueError):
                verifier.inspect_archive(self.payload)
        with patch.object(verifier, "MAX_EXPANDED", 512):
            with self.assertRaises(ValueError):
                verifier.inspect_archive(self.payload)
        with self.assertRaises(ValueError):
            verifier.inspect_archive(gzip.compress(gzip.decompress(self.payload) + b"not-tar-padding", mtime=0))
        (self.prepared / "extra").write_bytes(b"unapproved")
        with self.assertRaises(ValueError):
            verifier.validate_prepared(self.project, self.prepared, self.capture)

    def test_input_inventory_mutation_during_reverification_rejects(self):
        original = verifier.publisher_association
        calls = 0

        def association(*arguments):
            nonlocal calls
            calls += 1
            result = original(*arguments)
            if calls == 2:
                (self.prepared / "extra-late").write_bytes(b"unapproved")
            return result

        with patch.object(verifier, "publisher_association", side_effect=association):
            with self.assertRaisesRegex(ValueError, "inventory changed"):
                verifier.validate_prepared(self.project, self.prepared, self.capture)


if __name__ == "__main__":
    unittest.main()
