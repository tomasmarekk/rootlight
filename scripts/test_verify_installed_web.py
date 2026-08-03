#!/usr/bin/env python3
"""Focused tests for installed web package extraction and identity checks."""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
import zipfile


MODULE_PATH = Path(__file__).with_name("verify-installed-web.py")
SPEC = importlib.util.spec_from_file_location("verify_installed_web", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
installed_web = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(installed_web)


class InstalledWebSmokeTests(unittest.TestCase):
    def test_smoke_temporary_parent_is_short_and_macos_only(self) -> None:
        self.assertEqual(
            installed_web.smoke_temporary_parent("darwin"),
            Path("/private/tmp"),
        )
        self.assertIsNone(installed_web.smoke_temporary_parent("linux"))
        self.assertIsNone(installed_web.smoke_temporary_parent("win32"))

    def test_readiness_probe_waits_for_spawned_daemon_discovery(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            runtime = Path(temporary)
            discovery = runtime / "daemon.json"
            environment = {"ROOTLIGHT_RUNTIME_DIR": str(runtime)}
            process = mock.Mock()
            process.poll.return_value = None
            healthy = subprocess.CompletedProcess(
                args=[],
                returncode=0,
                stdout=json.dumps(
                    {
                        "ok": True,
                        "result": {"data": {"ready": True}},
                    }
                ).encode(),
                stderr=b"",
            )

            def publish_discovery(_seconds: float) -> None:
                discovery.write_text("{}", encoding="utf-8")

            def probe(*_args: object, **_kwargs: object) -> subprocess.CompletedProcess[bytes]:
                self.assertTrue(discovery.is_file())
                return healthy

            with (
                mock.patch.object(
                    installed_web.time,
                    "sleep",
                    side_effect=publish_discovery,
                ),
                mock.patch.object(
                    installed_web.subprocess,
                    "run",
                    side_effect=probe,
                ) as run,
            ):
                installed_web.wait_for_daemon(
                    Path("rootlight"),
                    environment,
                    process,
                )

            run.assert_called_once()
            self.assertGreaterEqual(process.poll.call_count, 2)

    def test_readiness_probe_rejects_an_exited_spawned_daemon(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            environment = {"ROOTLIGHT_RUNTIME_DIR": temporary}
            process = mock.Mock()
            process.poll.return_value = 1
            with (
                mock.patch.object(installed_web.subprocess, "run") as run,
                self.assertRaisesRegex(RuntimeError, "before becoming ready"),
            ):
                installed_web.wait_for_daemon(
                    Path("rootlight"),
                    environment,
                    process,
                )
            run.assert_not_called()

    def test_archive_extraction_and_manifest_identity_are_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "package.zip"
            suffix = ".exe" if installed_web.os.name == "nt" else ""
            files = {
                f"bin/rootlight{suffix}": b"rootlight",
                f"bin/rootlight-daemon{suffix}": b"daemon",
                f"bin/rootlight-web{suffix}": b"web",
                "share/rootlight/web/asset-manifest.json": b"{}",
                "share/rootlight/web/index.html": b"<!doctype html>",
            }
            manifest = {
                "schema": installed_web.PACKAGE_SCHEMA,
                "version": "0.1.0",
                "source_revision": "0" * 40,
                "target": (
                    "x86_64-pc-windows-msvc"
                    if installed_web.os.name == "nt"
                    else "x86_64-unknown-linux-gnu"
                ),
                "entries": [
                    {
                        "path": path,
                        "sha256": hashlib.sha256(content).hexdigest(),
                    }
                    for path, content in sorted(files.items())
                ],
            }
            with zipfile.ZipFile(archive, "w") as package:
                package.writestr("package-manifest.json", json.dumps(manifest))
                for path, content in files.items():
                    package.writestr(path, content)

            destination = root / "extracted"
            destination.mkdir()
            observed = installed_web.extract_archive(archive, destination)
            identities = installed_web.validate_manifest(
                observed,
                "0.1.0",
                "0" * 40,
                manifest["target"],
            )
            installed_web.verify_installed_hashes(destination, identities)

    def test_archive_rejects_traversal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / "hostile.zip"
            with zipfile.ZipFile(archive, "w") as package:
                package.writestr("../outside", b"hostile")
            with zipfile.ZipFile(archive) as package:
                with self.assertRaisesRegex(ValueError, "unsafe entry"):
                    installed_web.validated_archive_members(package)


if __name__ == "__main__":
    unittest.main()
