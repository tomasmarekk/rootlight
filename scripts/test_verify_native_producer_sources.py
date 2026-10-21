"""Exercise shared producer custody with small authored catalog-shaped sources.

Only reviewed metadata pins differ from production. The same source verifier
checks every fixture byte; no parser, compiler or acquisition is executed.
"""

import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import verify_native_producer_sources as native


def authored_siblings(project):
    """Write two inert source catalogs and return their explicit fixture pins."""
    pins = {}
    for name in native.REVIEWED_PRODUCERS:
        root = project / native.PRODUCERS_PATH / name
        root.mkdir(parents=True)
        payloads = {"LICENSE": b"authored fixture license\n"}
        if name.startswith("flux-"):
            payloads.update({"upstream/source.rs": b"authored upstream\n",
                             "parser/src/lib.rs": b"authored derivative\n"})
            row = lambda path: {"path": path, "bytes": len(payloads[path]),
                                "sha256": hashlib.sha256(payloads[path]).hexdigest()}
            upstream = {**row("upstream/source.rs"), "path": "source.rs", "mode": "100644"}
            manifest = {"schema": "rootlight.native-producer-source-inputs/1",
                        "upstream_files": 1, "selected_files": [row("parser/src/lib.rs")]}
            payloads["UPSTREAM_MANIFEST.json"] = (json.dumps([upstream]) + "\n").encode()
        else:
            payloads["raffia/src/lib.rs"] = b"authored parser\n"
            manifest = {"schema": "rootlight.raffia-source-bundle/1", "upstream": [{
                "path": "raffia/src/lib.rs", "local_bytes": len(payloads["raffia/src/lib.rs"]),
                "local_sha256": hashlib.sha256(payloads["raffia/src/lib.rs"]).hexdigest()}],
                "added_licenses": []}
        payloads["SOURCE_MANIFEST.json"] = (json.dumps(manifest) + "\n").encode()
        pins[name] = {path: hashlib.sha256(raw).hexdigest() for path, raw in payloads.items()
                      if "/" not in path}
        for path, raw in payloads.items():
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(raw)
    return pins


class NativeProducerSourceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        pins = authored_siblings(self.root)
        authority = patch.dict(native.REVIEWED_PRODUCERS, pins, clear=True)
        authority.start()
        self.addCleanup(authority.stop)

    def test_pinned_catalogs_preserve_every_source_and_reject_forged_or_extra_inputs(self):
        modes, hashes, directories = native.native_source_inputs(self.root)
        expected = {path.relative_to(self.root).as_posix() for path in self.root.rglob("*")
                    if path.is_file()}
        self.assertEqual(set(hashes), expected)
        self.assertEqual(set(modes), expected)
        self.assertTrue(directories)
        self.assertEqual(native.native_source_inputs(self.root), (modes, hashes, directories))
        original = next(self.root.rglob("lib.rs"))
        raw = original.read_bytes()
        for action in ("changed", "missing"):
            original.write_bytes(raw + b"changed") if action == "changed" else original.unlink()
            with self.subTest(action=action), self.assertRaises(ValueError):
                native.native_source_inputs(self.root)
            original.write_bytes(raw)
        metadata = next(self.root.rglob("SOURCE_MANIFEST.json"))
        raw = metadata.read_bytes()
        metadata.write_bytes(raw + b" ")
        with self.assertRaisesRegex(ValueError, "metadata differs"):
            native.native_source_inputs(self.root)
        metadata.write_bytes(raw)
        extra = original.parent / "unreviewed.rs"
        extra.write_bytes(b"unreviewed inert input")
        with self.assertRaisesRegex(ValueError, "extra native producer"):
            native.native_source_inputs(self.root)
        extra.unlink()
        extra.mkdir()
        with self.assertRaisesRegex(ValueError, "extra native producer source directory"):
            native.native_source_inputs(self.root)


if __name__ == "__main__":
    unittest.main()
