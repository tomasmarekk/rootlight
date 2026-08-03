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

    def test_readiness_probe_waits_for_service_daemon_discovery(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            runtime = Path(temporary)
            discovery = runtime / "daemon.json"
            environment = {"ROOTLIGHT_RUNTIME_DIR": str(runtime)}
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
                )

            run.assert_called_once()

    def test_start_web_accepts_the_stable_service_url(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="Rootlight Web UI: http://127.0.0.1:43127/\n",
            stderr="",
        )
        with mock.patch.object(
            installed_web.subprocess,
            "run",
            return_value=completed,
        ) as run:
            origin = installed_web.start_web(Path("rootlight"), {})

        self.assertEqual(origin, "http://127.0.0.1:43127")
        run.assert_called_once()

    def test_start_web_rejects_a_bootstrap_fragment(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=(
                "Rootlight Web UI: http://127.0.0.1:43127/"
                f"#bootstrap={'a' * 43}\n"
            ),
            stderr="",
        )
        with (
            mock.patch.object(
                installed_web.subprocess,
                "run",
                return_value=completed,
            ),
            self.assertRaisesRegex(ValueError, "invalid URL"),
        ):
            installed_web.start_web(Path("rootlight"), {})

    def test_stop_web_uses_the_service_control_command(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=json.dumps(
                {
                    "ok": True,
                    "result": {
                        "type": "web_service",
                        "data": {"running": False},
                    },
                }
            ),
            stderr="",
        )
        with mock.patch.object(
            installed_web.subprocess,
            "run",
            return_value=completed,
        ) as run:
            installed_web.stop_web(Path("rootlight"), {})

        self.assertEqual(
            run.call_args.args[0],
            [Path("rootlight"), "service", "stop"],
        )

    def test_shutdown_probe_waits_for_owned_daemon_discovery_removal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            runtime = Path(temporary)
            discovery = runtime / "daemon.json"
            discovery.write_text("{}", encoding="utf-8")
            environment = {"ROOTLIGHT_RUNTIME_DIR": str(runtime)}

            def remove_discovery(_seconds: float) -> None:
                discovery.unlink()

            with mock.patch.object(
                installed_web.time,
                "sleep",
                side_effect=remove_discovery,
            ) as sleep:
                installed_web.wait_for_daemon_shutdown(environment)

            sleep.assert_called_once()

    @unittest.skipUnless(installed_web.os.name == "nt", "Windows image-lock probe")
    def test_cleanup_removes_the_released_daemon_executable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary)
            binary = package / "bin/rootlight-daemon.exe"
            binary.parent.mkdir()
            binary.write_bytes(b"daemon")

            installed_web.remove_windows_daemon_after_shutdown(package)

            self.assertFalse(binary.exists())

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
