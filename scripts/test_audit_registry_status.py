#!/usr/bin/env python3
"""Verify complete registry withdrawal checks fail closed without package downloads."""
from datetime import datetime, timedelta, timezone
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import audit_registry_status as audit


class RegistryStatusTests(unittest.TestCase):
    def setUp(self):
        output = Path(__file__).resolve().parents[1] / "target"
        output.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="registry-status-", dir=output)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.snapshot = self.root / "snapshot"
        self.locks = ("root.lock", "native.lock", "fuzz.lock", "producer.lock")
        contract = patch.object(audit, "LOCKFILES", self.locks)
        contract.start()
        self.addCleanup(contract.stop)
        self.checksum = "a" * 64
        for relative in self.locks:
            (self.root / relative).write_text(
                'version = 4\n[[package]]\nname = "arrayref"\nversion = "0.3.9"\n'
                f'source = "{audit.REGISTRY}"\nchecksum = "{self.checksum}"\n', encoding="utf-8",
            )
        self.record = {"name": "arrayref", "vers": "0.3.9", "cksum": self.checksum, "yanked": False}
        with patch.object(audit, "fetch_index", return_value=self.payload(self.record)) as fetch:
            audit.acquire(self.root, self.snapshot)
        fetch.assert_called_once_with("arrayref")

    @staticmethod
    def payload(*records):
        return ("\n".join(json.dumps(row) for row in records) + "\n").encode()

    def report(self):
        return json.loads((self.snapshot / "report.json").read_bytes())

    def write_report(self, report):
        (self.snapshot / "report.json").write_text(json.dumps(report), encoding="utf-8")

    def replace_metadata(self, payload):
        (self.snapshot / "entries/arrayref.jsonl").write_bytes(payload)
        report = self.report()
        report["entries"][0].update(sha256=hashlib.sha256(payload).hexdigest(), bytes=len(payload))
        self.write_report(report)

    def test_complete_snapshot_verifies_without_network(self):
        with patch.object(audit, "urlopen", side_effect=AssertionError("network forbidden")) as network:
            result = audit.verify(self.root, self.snapshot)
        network.assert_not_called()
        self.assertEqual((result["locks"], result["packages"], result["entries"], result["yanked"]), (4, 1, 1, 0))

    def test_missing_entry_is_rejected(self):
        (self.snapshot / "entries/arrayref.jsonl").unlink()
        with self.assertRaisesRegex(ValueError, "file inventory"):
            audit.verify(self.root, self.snapshot)

    def test_malformed_index_is_rejected(self):
        self.replace_metadata(b"not json\n")
        with self.assertRaises(ValueError):
            audit.verify(self.root, self.snapshot)

    def test_yanked_locked_version_is_rejected(self):
        self.replace_metadata(self.payload({**self.record, "yanked": True}))
        with self.assertRaisesRegex(ValueError, "yanked: arrayref@0.3.9"):
            audit.verify(self.root, self.snapshot)

    def test_registry_checksum_must_match_lock(self):
        self.replace_metadata(self.payload({**self.record, "cksum": "b" * 64}))
        with self.assertRaisesRegex(ValueError, "checksum differs"):
            audit.verify(self.root, self.snapshot)

    def test_missing_locked_version_is_rejected(self):
        self.replace_metadata(self.payload({**self.record, "vers": "0.3.8"}))
        with self.assertRaisesRegex(ValueError, "missing locked"):
            audit.verify(self.root, self.snapshot)

    def test_duplicate_version_is_rejected(self):
        self.replace_metadata(self.payload(self.record, self.record))
        with self.assertRaisesRegex(ValueError, "duplicate sparse-index version"):
            audit.verify(self.root, self.snapshot)

    def test_status_must_be_present_and_boolean(self):
        for value in (None, "false", 0):
            with self.subTest(value=value):
                self.replace_metadata(self.payload({**self.record, "yanked": value}))
                with self.assertRaisesRegex(ValueError, "malformed yanked status"):
                    audit.verify(self.root, self.snapshot)

    def test_changed_active_lock_is_rejected(self):
        with (self.root / self.locks[-1]).open("a", encoding="utf-8") as lock:
            lock.write("\n# changed lock input\n")
        with self.assertRaisesRegex(ValueError, "does not match the active locks"):
            audit.verify(self.root, self.snapshot)

    def test_stale_snapshot_is_rejected(self):
        report = self.report()
        report["captured_at"] = (datetime.now(timezone.utc) - timedelta(days=2)).isoformat()
        self.write_report(report)
        with self.assertRaisesRegex(ValueError, "stale"):
            audit.verify(self.root, self.snapshot)

    def test_incomplete_package_inventory_is_rejected(self):
        report = self.report()
        report["packages"] = []
        self.write_report(report)
        with self.assertRaisesRegex(ValueError, "active locks"):
            audit.verify(self.root, self.snapshot)

    def test_duplicate_metadata_inventory_is_rejected(self):
        report = self.report()
        report["entries"].append(report["entries"][0])
        self.write_report(report)
        with self.assertRaisesRegex(ValueError, "incomplete or duplicated"):
            audit.verify(self.root, self.snapshot)

    def test_changed_payload_is_rejected(self):
        (self.snapshot / "entries/arrayref.jsonl").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "payload changed"):
            audit.verify(self.root, self.snapshot)

    def test_noncanonical_source_is_rejected(self):
        report = self.report()
        report["entries"][0]["url"] = "https://example.invalid/arrayref"
        self.write_report(report)
        with self.assertRaisesRegex(ValueError, "noncanonical"):
            audit.verify(self.root, self.snapshot)

    def response(self):
        response = MagicMock()
        response.__enter__.return_value = response
        response.status = 200
        response.geturl.return_value = audit.index_url("arrayref")
        response.headers = {}
        response.read.return_value = self.payload(self.record)
        return response

    def test_metadata_request_uses_only_official_endpoint_and_bounded_read(self):
        response = self.response()
        with patch.object(audit, "urlopen", return_value=response) as network:
            self.assertEqual(audit.fetch_index("arrayref"), self.payload(self.record))
        self.assertEqual(network.call_args.args[0].full_url, "https://index.crates.io/ar/ra/arrayref")
        self.assertEqual(network.call_args.kwargs["timeout"], 30)
        response.read.assert_called_once_with(audit.MAX_BYTES + 1)

    def test_redirected_endpoint_is_rejected(self):
        response = self.response()
        response.geturl.return_value = "https://example.invalid/arrayref"
        with patch.object(audit, "urlopen", return_value=response):
            with self.assertRaisesRegex(ValueError, "endpoint"):
                audit.fetch_index("arrayref")
        response.read.assert_not_called()

    def test_oversized_metadata_is_rejected_before_read(self):
        response = self.response()
        response.headers = {"Content-Length": str(audit.MAX_BYTES + 1)}
        with patch.object(audit, "urlopen", return_value=response):
            with self.assertRaisesRegex(ValueError, "byte limit"):
                audit.fetch_index("arrayref")
        response.read.assert_not_called()

    def test_total_acquisition_size_is_bounded(self):
        payload = self.payload(self.record)
        with (patch.object(audit, "fetch_index", return_value=payload),
              patch.object(audit, "MAX_TOTAL_BYTES", len(payload) - 1)):
            with self.assertRaisesRegex(ValueError, "total snapshot byte limit"):
                audit.acquire(self.root, self.root / "oversized")
        self.assertFalse((self.root / "oversized/report.json").exists())


if __name__ == "__main__":
    unittest.main()
