#!/usr/bin/env python3
"""Exercise checksum-pinned OSV acquisition without network access.

The downloader copy uses a synthetic digest; curl is replaced, while filesystem
publication and SHA-256 verification use the real Linux tools used by CI.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("download-osv-database.sh")
PAYLOAD = b"reviewed OSV test snapshot\n"
DIGEST = hashlib.sha256(PAYLOAD).hexdigest()


class DownloadOsvTests(unittest.TestCase):
    """Check fail-closed publication and verified-cache reuse with real hashing."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.cache = self.root / "cache with spaces"
        self.directory = self.cache / "osv-scanner" / "crates.io"
        self.directory.mkdir(parents=True)
        self.archive = self.directory / "all.zip"
        self.sidecar = self.directory / "all.zip.sha256"
        source, count = re.subn(
            r'^expected_sha256="[0-9a-f]{64}"$',
            f'expected_sha256="{DIGEST}"',
            SCRIPT.read_text(encoding="utf-8"),
            flags=re.MULTILINE,
        )
        self.assertEqual(count, 1)
        source, count = re.subn(
            r"^expected_bytes=[0-9]+$",
            f"expected_bytes={len(PAYLOAD)}",
            source,
            flags=re.MULTILINE,
        )
        self.assertEqual(count, 1)
        self.script = self.root / "download.sh"
        self.script.write_text(source, encoding="utf-8")
        self.payload = self.root / "payload"
        self.payload.write_bytes(PAYLOAD)
        self.calls = self.root / "curl-arguments.json"
        binary = self.root / "bin"
        binary.mkdir()
        curl = binary / "curl"
        curl.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, pathlib, sys\n"
            "args = sys.argv[1:]\n"
            "pathlib.Path(os.environ['CURL_CALLS']).write_text(json.dumps(args))\n"
            "output = pathlib.Path(args[args.index('--output') + 1])\n"
            "output.write_bytes(pathlib.Path(os.environ['CURL_PAYLOAD']).read_bytes())\n"
            "sys.exit(int(os.environ.get('CURL_STATUS', '0')))\n",
            encoding="utf-8",
        )
        curl.chmod(0o700)
        self.environment = {
            **os.environ,
            "PATH": f"{binary}{os.pathsep}{os.environ['PATH']}",
            "CURL_CALLS": str(self.calls),
            "CURL_PAYLOAD": str(self.payload),
            "CURL_STATUS": "0",
        }

    def run_download(self, *, default_destination: bool = False) -> subprocess.CompletedProcess:
        """Run the isolated downloader with bounded execution and captured diagnostics."""
        arguments = ["bash", str(self.script)]
        if not default_destination:
            arguments.append(str(self.cache))
        result = subprocess.run(
            arguments, cwd=self.root, env=self.environment,
            text=True, capture_output=True, timeout=10, check=False,
        )
        self.assertEqual(list(self.directory.glob(".all.zip.*")), [])
        return result

    def assert_snapshot(self) -> None:
        """Require exact pinned bytes and the scanner's checksum-sidecar format."""
        self.assertEqual(self.archive.read_bytes(), PAYLOAD)
        self.assertEqual(self.sidecar.read_text(), f"{DIGEST}  all.zip\n")

    def test_download_publishes_only_verified_bytes(self) -> None:
        result = self.run_download()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_snapshot()
        arguments = json.loads(self.calls.read_text())
        self.assertEqual(arguments[arguments.index("--proto") + 1], "=https")
        self.assertIn("--tlsv1.2", arguments)
        self.assertEqual(arguments[arguments.index("--max-filesize") + 1], str(len(PAYLOAD)))
        self.assertTrue(any("?generation=" in argument for argument in arguments))

    def test_valid_cache_is_reused_and_wrong_sidecar_repaired_without_network(self) -> None:
        self.archive.write_bytes(PAYLOAD)
        self.sidecar.write_text("wrong sidecar")
        self.environment["CURL_STATUS"] = "22"
        result = self.run_download()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.calls.exists())
        self.assert_snapshot()

    def test_invalid_cache_is_replaced_after_verified_download(self) -> None:
        self.archive.write_bytes(b"old snapshot")
        result = self.run_download()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_snapshot()

    def test_network_failure_does_not_publish_partial_bytes(self) -> None:
        self.payload.write_bytes(b"partial response")
        self.environment["CURL_STATUS"] = "22"
        result = self.run_download()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.archive.exists())
        self.assertFalse(self.sidecar.exists())

    def test_hash_mismatch_preserves_previous_cache_and_sidecar(self) -> None:
        self.archive.write_bytes(b"old snapshot")
        self.sidecar.write_text("old checksum")
        self.payload.write_bytes(b"unreviewed snapshot")
        result = self.run_download()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.archive.read_bytes(), b"old snapshot")
        self.assertEqual(self.sidecar.read_text(), "old checksum")

    def test_symlink_is_replaced_without_modifying_its_target(self) -> None:
        outside = self.root / "unrelated"
        outside.write_bytes(PAYLOAD)
        self.archive.symlink_to(outside)
        result = self.run_download()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.archive.is_symlink())
        self.assertEqual(outside.read_bytes(), PAYLOAD)
        self.assert_snapshot()

    def test_directory_destination_is_not_treated_as_a_download_folder(self) -> None:
        self.archive.mkdir()
        sentinel = self.archive / "unrelated"
        sentinel.write_bytes(b"retain")
        result = self.run_download()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(self.archive.iterdir()), [sentinel])
        self.assertEqual(sentinel.read_bytes(), b"retain")
        self.assertFalse(self.sidecar.exists())

    def test_checksum_symlink_is_replaced_without_touching_its_target(self) -> None:
        self.archive.write_bytes(PAYLOAD)
        outside = self.root / "unrelated"
        outside.write_bytes(b"retain")
        self.sidecar.symlink_to(outside)
        result = self.run_download()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.sidecar.is_symlink())
        self.assertEqual(outside.read_bytes(), b"retain")
        self.assert_snapshot()

    def test_default_cache_layout_is_preserved(self) -> None:
        result = self.run_download(default_destination=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = self.root / "artifacts" / "osv-db" / "osv-scanner" / "crates.io"
        self.assertEqual((expected / "all.zip").read_bytes(), PAYLOAD)
        self.assertEqual((expected / "all.zip.sha256").read_text(), f"{DIGEST}  all.zip\n")
        self.assertEqual(list(expected.glob(".all.zip.*")), [])


if __name__ == "__main__":
    unittest.main()
