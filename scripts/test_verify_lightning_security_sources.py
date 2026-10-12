"""Reject tampered native producer profiles while preserving authored provenance.

Only upstream association is modeled. Real locks, archive authentication and
overlay projection run against small complete synthetic source inputs.
"""
import base64
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import verify_lightning_security_sources as security
from test_verify_lightning_yarn_sources import archive


class NativeSourceTests(unittest.TestCase):
    """Check executable-profile integrity independently of installer execution."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.bundle = self.root / security.BUNDLE_PATH
        (self.bundle / "archives").mkdir(parents=True)
        (self.bundle / "overlays").mkdir()
        tree = self.root / security.PRODUCER_PATH / "tree"
        tree.mkdir(parents=True)
        manifest = {"name": "lightningcss", "version": "1.0.0", "dependencies": {"detect-libc": "1.0.0"},
                    "devDependencies": {name: "1.0.0" for name in sorted(security.NATIVE_TOOLS | {"sharp"})},
                    "resolutions": {"lightningcss": "link:."},
                    "scripts": {name: "node retained.js" for name in sorted(security.NATIVE_SCRIPTS | {"website:build"})}}
        originals = {"package.json": json.dumps(manifest).encode(), "yarn.lock": b"original lock\n",
                     "Cargo.toml": b'[package]\nname="native"\nversion="1.0.0"\n',
                     "Cargo.lock": b"version = 4\n"}
        members = []
        for name, payload in originals.items():
            (tree / name).write_bytes(payload)
            members.append({"path": security.producer.ROOT + "/" + name, "kind": "file",
                            "archive_mode": 0o644, **security._record(payload)})
        self.source = {"association": {"fixture": "authenticated upstream"}, "source_inventory": {"members": members}}
        (tree.parent / "source-tree.json").write_bytes(security.producer.json_bytes(self.source))
        association = patch.object(security.producer, "verify_bundle", return_value=self.source)
        association.start()
        self.addCleanup(association.stop)
        sections = ["# yarn lockfile v1\n"]
        for name in sorted(security.NATIVE_TOOLS | {"detect-libc"}):
            payload, _ = archive(name)
            url = f"https://registry.npmjs.org/{name}/-/{name.rsplit('/', 1)[-1]}-1.0.0.tgz#{hashlib.sha1(payload).hexdigest()}"
            integrity = "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode()
            sections.append(f'{json.dumps(name + "@1.0.0")}:\n  version "1.0.0"\n  resolved "{url}"\n  integrity {integrity}\n\n')
            (self.bundle / "archives" / (hashlib.sha256(url.encode()).hexdigest() + ".tgz")).write_bytes(payload)
        self.profile = {"schema": "rootlight.lightning-native-profile/1", "upstream_commit": security.producer.COMMIT,
                        "scope": "native binding build, Flow declarations, TypeScript checking and native binding tests",
                        "overlays": []}
        replacements = {**originals, "package.json": security.native_manifest(originals["package.json"]),
                        "yarn.lock": "".join(sections).encode()}
        for name, payload in replacements.items():
            relative = "overlays/" + name
            (self.bundle / relative).write_bytes(payload)
            self.profile["overlays"].append({"path": name, "source": relative,
                                             "sha256": hashlib.sha256(payload).hexdigest(),
                                             "upstream_sha256": hashlib.sha256(originals[name]).hexdigest()})
        self.write_profile()
        self.report = security.derive_bundle(self.root)
        (self.bundle / security.REPORT_NAME).write_bytes(security.producer.json_bytes(self.report))

    def write_profile(self):
        (self.bundle / security.PROFILE_NAME).write_bytes(security.producer.json_bytes(self.profile))

    def test_complete_profile_and_effective_bytes(self):
        verified = security.verify_bundle(self.root)
        self.assertEqual(verified["archive_count"], 5)
        effective, payloads = security.effective_producer(self.root, verified, self.source)
        self.assertEqual(set(payloads), {"package.json", "yarn.lock", "Cargo.toml", "Cargo.lock", ".cargo/config.toml"})
        self.assertIn(b"offline = true", payloads[".cargo/config.toml"])
        manifest = json.loads(payloads["package.json"])
        self.assertNotIn("resolutions", manifest)
        self.assertEqual(verified["pin_graph"]["local_associations"], [])
        self.assertNotIn("sharp", manifest["devDependencies"])
        self.assertNotIn("website:build", manifest["scripts"])
        self.assertNotEqual(effective, self.source)
        self.assertFalse(verified["package_code_executed"])
        self.assertFalse(verified["build_and_packaging_acceptance_verified"])

    def test_generated_cargo_policy_preserves_authenticated_target_flags(self):
        configuration = b'[target.x86_64-pc-windows-msvc]\nrustflags = ["-C", "target-feature=+crt-static"]\n'
        path = self.root / security.PRODUCER_PATH / "tree/.cargo/config.toml"
        path.parent.mkdir()
        path.write_bytes(configuration)
        self.source["source_inventory"]["members"].append({
            "path": security.producer.ROOT + "/.cargo/config.toml", "kind": "file",
            "archive_mode": 0o644, **security._record(configuration)})
        _, payloads = security.effective_producer(self.root, self.report, self.source)
        projected = security.tomllib.loads(payloads[".cargo/config.toml"].decode())
        self.assertTrue(payloads[".cargo/config.toml"].startswith(configuration))
        self.assertEqual(projected["target"]["x86_64-pc-windows-msvc"]["rustflags"],
                         ["-C", "target-feature=+crt-static"])
        self.assertIs(projected["net"]["offline"], True)
        self.assertEqual(projected["source"]["rootlight-local-sources"]["directory"],
                         (self.root / "source-inputs/cargo").resolve().as_posix())
        path.write_bytes(b"changed flags")
        with self.assertRaisesRegex(ValueError, "configuration changed"):
            security.effective_producer(self.root, self.report, self.source)

    def test_export_binds_every_native_input_and_report(self):
        import prepare_rust_source_bundle as bundle

        report, modes, hashes, directories = bundle._native_source_inputs(self.root)
        self.assertEqual(report, self.report)
        expected = {security.BUNDLE_PATH + "/" + path for path in self.report["inputs"]}
        expected.add(security.BUNDLE_PATH + "/" + security.REPORT_NAME)
        self.assertEqual(set(hashes), expected)
        self.assertEqual(modes, {path: 0o644 for path in expected})
        self.assertIn(security.BUNDLE_PATH + "/archives", directories)
        for path, digest in hashes.items():
            self.assertEqual(hashlib.sha256((self.root / path).read_bytes()).hexdigest(), digest)
        (self.root / "source-inputs/producer-security/unreviewed").mkdir()
        with self.assertRaisesRegex(ValueError, "namespace inventory"):
            bundle._native_source_inputs(self.root)

    def test_changed_archive_rejected(self):
        path = next((self.bundle / "archives").iterdir())
        path.write_bytes(path.read_bytes() + b"foreign")
        with self.assertRaises(ValueError):
            security.verify_bundle(self.root)

    def test_changed_overlay_rejected(self):
        (self.bundle / "overlays/Cargo.lock").write_bytes(b"foreign")
        with self.assertRaisesRegex(ValueError, "overlay hash"):
            security.verify_bundle(self.root)

    def test_reintroducing_unneeded_tools_rejected_even_with_new_hash(self):
        path = self.bundle / "overlays/package.json"
        manifest = json.loads(path.read_bytes())
        manifest["devDependencies"]["sharp"] = "1.0.0"
        path.write_bytes(json.dumps(manifest).encode())
        for row in self.profile["overlays"]:
            if row["path"] == "package.json":
                row["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
        self.write_profile()
        with self.assertRaisesRegex(ValueError, "supported dependency"):
            security.verify_bundle(self.root)

    def test_unknown_and_missing_sources_fail_closed(self):
        (self.bundle / "unexpected").write_bytes(b"not declared")
        with self.assertRaisesRegex(ValueError, "inventory"):
            security.verify_bundle(self.root)
        (self.bundle / "unexpected").unlink()
        (self.bundle / security.PROFILE_NAME).unlink()
        with self.assertRaises((ValueError, OSError)):
            security.verify_bundle(self.root)

    def test_overlay_cannot_modify_arbitrary_upstream_source(self):
        self.profile["overlays"][0]["path"] = "scripts/build.js"
        self.write_profile()
        with self.assertRaisesRegex(ValueError, "unsupported native overlay"):
            security.verify_bundle(self.root)

    def test_upstream_binding_and_staging_rechecked(self):
        self.profile["overlays"][0]["upstream_sha256"] = "0" * 64
        self.write_profile()
        with self.assertRaisesRegex(ValueError, "upstream hash"):
            security.verify_bundle(self.root)
        (self.bundle / "overlays/yarn.lock").write_bytes(b"changed after authentication")
        with self.assertRaisesRegex(ValueError, "effective producer input"):
            security.effective_producer(self.root, self.report, self.source)


if __name__ == "__main__":
    unittest.main()
