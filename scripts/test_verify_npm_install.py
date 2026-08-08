#!/usr/bin/env python3
"""Focused tests for the installed npm package verifier."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("verify-npm-install.py")
SPEC = importlib.util.spec_from_file_location("verify_npm_install", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
verify_npm_install = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify_npm_install)


class VerifyNpmInstallTests(unittest.TestCase):
    def test_windows_resolves_the_command_shim(self) -> None:
        with mock.patch.object(
            verify_npm_install.shutil,
            "which",
            return_value=r"C:\hostedtoolcache\node\npm.cmd",
        ) as which:
            executable = verify_npm_install.npm_executable("nt")

        self.assertEqual(executable, r"C:\hostedtoolcache\node\npm.cmd")
        which.assert_called_once_with("npm.cmd")

    def test_unix_resolves_the_native_command(self) -> None:
        with mock.patch.object(
            verify_npm_install.shutil,
            "which",
            return_value="/opt/node/bin/npm",
        ) as which:
            executable = verify_npm_install.npm_executable("posix")

        self.assertEqual(executable, "/opt/node/bin/npm")
        which.assert_called_once_with("npm")

    def test_missing_npm_is_actionable(self) -> None:
        with (
            mock.patch.object(verify_npm_install.shutil, "which", return_value=None),
            self.assertRaisesRegex(
                verify_npm_install.NpmInstallError,
                "npm.cmd is not available on PATH",
            ),
        ):
            verify_npm_install.npm_executable("nt")

    def test_failed_lifecycle_command_identifies_its_stage(self) -> None:
        completed = verify_npm_install.subprocess.CompletedProcess(
            args=["/private/install/rootlight", "service", "restart"],
            returncode=3,
            stdout="",
            stderr="local web service is unavailable",
        )
        with (
            mock.patch.object(
                verify_npm_install.subprocess,
                "run",
                return_value=completed,
            ),
            self.assertRaisesRegex(
                verify_npm_install.NpmInstallError,
                r"^service restart failed with exit code 3:",
            ),
        ):
            verify_npm_install.run(
                completed.args,
                {},
                stage="service restart",
            )

    def test_retryable_busy_service_command_is_replayed(self) -> None:
        command = ["/private/install/rootlight", "service", "restart"]
        busy = verify_npm_install.subprocess.CompletedProcess(
            args=command,
            returncode=3,
            stdout="",
            stderr=(
                '{"error":{"code":"BUSY","retryable":true,'
                '"retry_after_ms":1}}'
            ),
        )
        succeeded = verify_npm_install.subprocess.CompletedProcess(
            args=command,
            returncode=0,
            stdout='{"ok":true}',
            stderr="",
        )
        with (
            mock.patch.object(
                verify_npm_install.subprocess,
                "run",
                side_effect=(busy, succeeded),
            ) as run,
            mock.patch.object(verify_npm_install.time, "sleep") as sleep,
        ):
            response = verify_npm_install.run_json_retrying_busy(
                command,
                {},
                stage="service restart",
            )

        self.assertEqual(response, {"ok": True})
        self.assertEqual(run.call_count, 2)
        sleep.assert_called_once_with(0.001)

    def test_nonretryable_service_failure_is_not_replayed(self) -> None:
        command = ["/private/install/rootlight", "service", "restart"]
        failed = verify_npm_install.subprocess.CompletedProcess(
            args=command,
            returncode=3,
            stdout="",
            stderr='{"error":{"code":"BUSY","retryable":false}}',
        )
        with (
            mock.patch.object(
                verify_npm_install.subprocess,
                "run",
                return_value=failed,
            ) as run,
            self.assertRaisesRegex(
                verify_npm_install.NpmInstallError,
                r"^service restart failed with exit code 3:",
            ),
        ):
            verify_npm_install.run_json_retrying_busy(
                command,
                {},
                stage="service restart",
            )

        run.assert_called_once()

    def test_run_uses_the_explicit_local_install_directory(self) -> None:
        completed = verify_npm_install.subprocess.CompletedProcess(
            args=["npm", "install"],
            returncode=0,
            stdout="",
            stderr="",
        )
        project = Path("/private/project")
        with mock.patch.object(
            verify_npm_install.subprocess,
            "run",
            return_value=completed,
        ) as run:
            verify_npm_install.run(completed.args, {}, cwd=project)

        self.assertEqual(run.call_args.kwargs["cwd"], project)

    def test_login_registration_uses_the_disposable_user_boundary(self) -> None:
        prefix = Path("/private/prefix")
        cache = Path("/private/cache")
        state = Path("/private/state")
        runtime = Path("/private/runtime")
        login = Path("/private/login")

        for platform, variable in (
            ("linux", "XDG_CONFIG_HOME"),
            ("darwin", "HOME"),
            ("win32", "APPDATA"),
        ):
            with self.subTest(platform=platform):
                environment = verify_npm_install.npm_environment(
                    prefix,
                    cache,
                    state,
                    runtime,
                    login,
                    platform=platform,
                )

                self.assertEqual(environment[variable], str(login.resolve()))

    def test_macos_runtime_uses_the_short_physical_temporary_root(self) -> None:
        self.assertEqual(
            verify_npm_install.runtime_temporary_parent("darwin"),
            Path("/private/tmp"),
        )
        self.assertIsNone(verify_npm_install.runtime_temporary_parent("linux"))


if __name__ == "__main__":
    unittest.main()
