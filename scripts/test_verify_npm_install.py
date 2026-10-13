#!/usr/bin/env python3
"""Focused tests for the installed npm package verifier."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("verify-npm-install.py")
SPEC = importlib.util.spec_from_file_location("verify_npm_install", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
verify_npm_install = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify_npm_install)


class VerifyNpmInstallTests(unittest.TestCase):
    @staticmethod
    def startup_bytes() -> bytes:
        return b"".join(
            (json.dumps({
                "schema": "rootlight.startup-diagnostics/1",
                "stage": stage,
                "elapsed_us": index * 17,
            }) + "\n").encode()
            for index, stage in enumerate(("runtime_ready", "journal_open"))
        )

    @unittest.skipUnless(os.name == "posix", "uses the actual POSIX descriptor boundary")
    def test_initial_success_snapshot_precedes_stop_and_survives_uninstall(self) -> None:
        for diagnostics in ("valid", "malformed", "stdout_failure", "write_failure"):
            with self.subTest(diagnostics=diagnostics), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                mode = root / "local"
                state = mode / "state"
                runtime = root / "runtime"
                prefix = mode / "prefix"
                project = mode / "project"
                package = project / "node_modules" / "@tomasmarekk" / "rootlight"
                native = package.parent / "rootlight-darwin-x64"
                cli = verify_npm_install.npm_cli_path(prefix)
                snapshot = mode / "initial-install-startup-diagnostics.json"
                service = {
                    "schema_version": 1, "registered": True, "running": True,
                    "origin": "http://127.0.0.1:43127", "pid": 123,
                }
                write_json = verify_npm_install.write_json_new

                def write(path: Path, value: object) -> None:
                    if path == snapshot and diagnostics == "write_failure":
                        raise PermissionError("private-source-value")
                    write_json(path, value)

                def run(command: list[str], *args: object, **kwargs: object) -> object:
                    if command[1] == "install":
                        state.mkdir()
                        runtime.mkdir()
                        package.mkdir(parents=True)
                        (native / "bin").mkdir(parents=True)
                        (native / "bin" / "rootlight").touch()
                        cli.parent.mkdir(exist_ok=True)
                        cli.touch()
                        data = self.startup_bytes() if diagnostics != "malformed" else b'{"path":"private-source-value"}\n'
                        (state / "startup-diagnostics-123.jsonl").write_bytes(data)
                    else:
                        self.assertEqual(command, [str(cli), "uninstall"])
                        self.assertEqual(snapshot.is_file(), diagnostics != "write_failure")
                        cli.unlink()
                        shutil.rmtree(package)
                        shutil.rmtree(native)
                        shutil.rmtree(state)
                        shutil.rmtree(runtime)
                    return verify_npm_install.subprocess.CompletedProcess(command, 0, "", "")

                def stop(*args: object, **kwargs: object) -> dict[str, object]:
                    self.assertEqual(snapshot.is_file(), diagnostics != "write_failure")
                    shutil.rmtree(state)
                    state.mkdir()
                    return {"result": {"type": "web_service", "data": service | {"running": False, "pid": None}}}

                with (
                    mock.patch.object(verify_npm_install, "run", side_effect=run),
                    mock.patch.object(verify_npm_install, "write_json_new", side_effect=write),
                    mock.patch.object(verify_npm_install.shutil, "which", return_value=str(cli)),
                    mock.patch.object(verify_npm_install, "service_status", return_value=service),
                    mock.patch.object(verify_npm_install, "run_json", side_effect=stop),
                    mock.patch.object(verify_npm_install, "run_json_retrying_busy", return_value={"result": {"type": "web_service", "data": service}}),
                    mock.patch.object(verify_npm_install, "issue_web_bootstrap", return_value="private-session-token"),
                    mock.patch.object(verify_npm_install, "verify_http_session"),
                    mock.patch.object(verify_npm_install, "wait_for_port"),
                    mock.patch.object(verify_npm_install, "cleanup"),
                    mock.patch("builtins.print", side_effect=BrokenPipeError() if diagnostics == "stdout_failure" else None) as printed,
                ):
                    result = verify_npm_install.verify_install_mode(
                        "local", root, runtime, root / "root.tgz", root / "native.tgz",
                        "rootlight-darwin-x64", "@tomasmarekk/rootlight-darwin-x64", "npm",
                    )
                self.assertTrue(result["uninstall_removed_data"])
                self.assertFalse(state.exists())
                self.assertFalse(runtime.exists())
                self.assertFalse((mode / "failure.json").exists())
                self.assertFalse((mode / "startup-diagnostics.json").exists())
                printed.assert_called_once()
                emitted = printed.call_args.args[0]
                captured = json.loads(snapshot.read_text() if snapshot.exists() else emitted)
                expected_status = "rejected" if diagnostics == "malformed" else "captured"
                self.assertEqual(captured["status"], expected_status)
                self.assertEqual(json.loads(emitted), captured)
                self.assertNotIn("private-source-value", emitted)
                self.assertNotIn("private-session-token", emitted)
                self.assertNotIn(str(root), emitted)

    def test_startup_records_accept_only_ordered_closed_phase_data(self) -> None:
        raw = self.startup_bytes()
        self.assertEqual(len(verify_npm_install._startup_records(raw)), 2)
        first = json.loads(raw.splitlines()[0])
        last = first | {"stage": "discovery_published", "elapsed_us": 17}
        self.assertEqual(
            verify_npm_install._startup_records(raw + (json.dumps(last) + "\n").encode())[-1],
            last,
        )
        invalid = [b"", raw[:-1], b"private-source-value\n", raw + b"\n"]
        for mutation in (
            {"schema": "private-source-value"}, {"stage": "private-source-value"},
            {"stage": []}, {"elapsed_us": True}, {"elapsed_us": -1},
            {"elapsed_us": 1 << 64}, {"elapsed_us": 0.5},
            {"path": "private-source-value"},
        ):
            invalid.append((json.dumps(first | mutation) + "\n").encode())
        invalid.extend([
            (json.dumps(first)[:-1] + ',"elapsed_us":1}\n').encode(),
            raw + raw,
            raw.splitlines(keepends=True)[1] + raw.splitlines(keepends=True)[0],
            b"x" * (verify_npm_install.MAX_STARTUP_DIAGNOSTIC_BYTES + 1),
            b"[]\n", b"null\n", b"\xff\n",
        ])
        backward = [json.loads(line) for line in raw.splitlines()]
        backward[0]["elapsed_us"], backward[1]["elapsed_us"] = 20, 19
        invalid.append(("\n".join(map(json.dumps, backward)) + "\n").encode())
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(ValueError):
                verify_npm_install._startup_records(value)

    @unittest.skipUnless(os.name == "posix", "uses the actual POSIX descriptor boundary")
    def test_startup_capture_survives_state_cleanup_without_raw_values(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            diagnostic = {
                "schema": "rootlight.npm-service-failure/1", "action": "install",
                "reason": "daemon_start_timeout", "exit_status": 3, "signal": None,
            }

            def install(*arguments: object, **kwargs: object) -> object:
                state = root / "local" / "state"
                state.mkdir()
                (state / "startup-diagnostics-123.jsonl").write_bytes(self.startup_bytes())
                return verify_npm_install.subprocess.CompletedProcess(
                    args=["npm", "install"], returncode=1,
                    stdout="private-source-value",
                    stderr=verify_npm_install.SERVICE_FAILURE_PREFIX + json.dumps(diagnostic),
                )

            def cleanup(*arguments: object) -> None:
                self.assertTrue((root / "local" / "startup-diagnostics.json").is_file())
                shutil.rmtree(root / "local" / "state")

            with (
                mock.patch.object(verify_npm_install.subprocess, "run", side_effect=install) as run,
                mock.patch.object(verify_npm_install, "cleanup", side_effect=cleanup),
                self.assertRaises(verify_npm_install.NpmInstallError) as error,
            ):
                verify_npm_install.verify_install_mode(
                    "local", root, root / "runtime", root / "root.tgz", root / "native.tgz",
                    "rootlight-darwin-x64", "@tomasmarekk/rootlight-darwin-x64", "npm",
                )
            self.assertEqual(error.exception.command_exit_status, 1)
            run.assert_called_once()
            receipt = (root / "local" / "startup-diagnostics.json").read_text()
            self.assertNotIn("private-source-value", receipt)
            self.assertEqual(json.loads(receipt), {
                "schema": "rootlight.npm-startup-diagnostics/1", "status": "captured",
                "files": [{"pid": 123, "records": [json.loads(line) for line in self.startup_bytes().splitlines()]}],
            })
            self.assertFalse((root / "local" / "state").exists())

    @unittest.skipUnless(os.name == "posix", "uses the actual POSIX descriptor boundary")
    def test_startup_capture_rejects_links_special_files_and_unreviewed_data(self) -> None:
        for kind in ("symlink", "hardlink", "directory", "fifo", "oversize", "malformed", "writable", "filename", "file_count", "entry_count"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                state = root / "state"
                state.mkdir()
                source = root / "private-source-value"
                source.write_bytes(self.startup_bytes())
                path = state / "startup-diagnostics-123.jsonl"
                if kind == "symlink":
                    path.symlink_to(source)
                elif kind == "hardlink":
                    os.link(source, path)
                elif kind == "directory":
                    path.mkdir()
                elif kind == "fifo":
                    os.mkfifo(path)
                elif kind == "oversize":
                    path.write_bytes(b"x" * (verify_npm_install.MAX_STARTUP_DIAGNOSTIC_BYTES + 1))
                elif kind == "malformed":
                    path.write_bytes(b'{"path":"private-source-value"}\n')
                elif kind == "writable":
                    path.write_bytes(self.startup_bytes())
                    path.chmod(0o666)
                elif kind == "filename":
                    (state / "startup-diagnostics-private-source-value.jsonl").write_bytes(self.startup_bytes())
                elif kind == "file_count":
                    for pid in range(1, verify_npm_install.MAX_STARTUP_DIAGNOSTIC_FILES + 2):
                        (state / f"startup-diagnostics-{pid}.jsonl").write_bytes(self.startup_bytes())
                else:
                    for number in range(verify_npm_install.MAX_STARTUP_STATE_ENTRIES + 1):
                        (state / str(number)).touch()
                output = root / "capture.json"
                verify_npm_install._capture_startup_diagnostics(state, output)
                self.assertEqual(json.loads(output.read_text()), {
                    "schema": "rootlight.npm-startup-diagnostics/1",
                    "status": "rejected", "files": [],
                })

    @unittest.skipUnless(os.name == "posix", "uses the actual POSIX descriptor boundary")
    def test_startup_capture_rejects_replaced_file_and_state_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / "state"
            state.mkdir()
            file = state / "startup-diagnostics-123.jsonl"
            file.write_bytes(self.startup_bytes())
            private = root / "private"
            private.write_text("private-source-value")
            original_open = os.open

            def replaced_open(path: object, flags: int, *args: object, **kwargs: object) -> int:
                if path == file.name:
                    file.unlink()
                    file.symlink_to(private)
                return original_open(path, flags, *args, **kwargs)

            output = root / "capture.json"
            with mock.patch.object(verify_npm_install.os, "open", side_effect=replaced_open):
                verify_npm_install._capture_startup_diagnostics(state, output)
            self.assertEqual(json.loads(output.read_text())["status"], "rejected")
            alias = root / "state-alias"
            alias.symlink_to(state, target_is_directory=True)
            second = root / "alias.json"
            verify_npm_install._capture_startup_diagnostics(alias, second)
            self.assertEqual(json.loads(second.read_text())["status"], "rejected")

    def test_diagnostic_write_failure_does_not_mask_original_install_failure(self) -> None:
        original = verify_npm_install.NpmInstallError("initial install failed", command_exit_status=1)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write = verify_npm_install.write_json_new

            def denied_write(path: Path, value: object) -> None:
                if path.name == "startup-diagnostics.json":
                    raise PermissionError("private-source-value")
                write(path, value)

            with (
                mock.patch.object(verify_npm_install, "run", side_effect=original) as run,
                mock.patch.object(verify_npm_install, "write_json_new", side_effect=denied_write),
                mock.patch.object(verify_npm_install, "cleanup") as cleanup,
                self.assertRaises(verify_npm_install.NpmInstallError) as error,
            ):
                verify_npm_install.verify_install_mode(
                    "local", root, root / "runtime", root / "root.tgz", root / "native.tgz",
                    "rootlight-darwin-x64", "@tomasmarekk/rootlight-darwin-x64", "npm",
                )
            self.assertIs(error.exception, original)
            run.assert_called_once()
            cleanup.assert_called_once()
            self.assertEqual(json.loads((root / "local" / "failure.json").read_text())["command_exit_status"], 1)

    def test_only_intel_macos_smoke_enables_startup_diagnostics(self) -> None:
        for target, package in verify_npm_install.TARGET_PACKAGES.items():
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                with (
                    mock.patch.dict(os.environ, {"ROOTLIGHT_STARTUP_DIAGNOSTICS": "1"}),
                    mock.patch.object(verify_npm_install, "run", side_effect=verify_npm_install.NpmInstallError("failed")) as run,
                    mock.patch.object(verify_npm_install, "cleanup"),
                    mock.patch.object(verify_npm_install, "_capture_startup_diagnostics") as capture,
                    self.assertRaises(verify_npm_install.NpmInstallError),
                ):
                    verify_npm_install.verify_install_mode(
                        "local", root, root / "runtime", root / "root.tgz", root / "native.tgz",
                        package.rsplit("/", 1)[1], package, "npm",
                    )
                enabled = target == "x86_64-apple-darwin"
                self.assertEqual(run.call_args.args[1].get("ROOTLIGHT_STARTUP_DIAGNOSTICS"), "1" if enabled else None)
                self.assertEqual(capture.call_count, int(enabled))

    def test_initial_failed_install_does_not_attempt_global_mode(self) -> None:
        original = verify_npm_install.NpmInstallError("initial install failed", command_exit_status=1)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            packages = root / "packages"
            (packages / "rootlight").mkdir(parents=True)
            (packages / "rootlight-darwin-x64").mkdir()
            with (
                mock.patch.object(verify_npm_install, "npm_executable", return_value="npm"),
                mock.patch.object(verify_npm_install, "pack", return_value=root / "package.tgz"),
                mock.patch.object(verify_npm_install, "verify_install_mode", side_effect=original) as mode,
                self.assertRaises(verify_npm_install.NpmInstallError) as error,
            ):
                verify_npm_install.verify_install(packages, "x86_64-apple-darwin", root / "output", root / "receipt.json")
            self.assertIs(error.exception, original)
            mode.assert_called_once()
            self.assertEqual(mode.call_args.args[0], "local")
            self.assertFalse((root / "receipt.json").exists())

    def test_failure_receipt_survives_install_cleanup_without_raw_output(self) -> None:
        diagnostic = {
            "schema": "rootlight.npm-service-failure/1", "action": "install",
            "reason": "daemon_start_timeout", "exit_status": 3, "signal": None,
        }
        completed = verify_npm_install.subprocess.CompletedProcess(
            args=["npm", "install"], returncode=1, stdout="private-source-value",
            stderr="npm error rootlight: service failure " + json.dumps(diagnostic),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def cleanup(*arguments: object) -> None:
                shutil.rmtree(root / "local" / "project")
                shutil.rmtree(root / "local" / "prefix")

            with (
                mock.patch.object(verify_npm_install.subprocess, "run", return_value=completed),
                mock.patch.object(verify_npm_install, "cleanup", side_effect=cleanup),
                self.assertRaises(verify_npm_install.NpmInstallError),
            ):
                verify_npm_install.verify_install_mode(
                    "local", root, root / "runtime", root / "root.tgz",
                    root / "native.tgz", "rootlight-darwin-x64",
                    "@tomasmarekk/rootlight-darwin-x64", "npm",
                )

            receipt = (root / "local" / "failure.json").read_text(encoding="utf-8")
            self.assertNotIn("private-source-value", receipt)
            self.assertEqual(json.loads(receipt), {
                "schema": "rootlight.npm-install-failure/1", "mode": "local",
                "command_exit_status": 1, "service_failure": diagnostic,
            })
            self.assertFalse((root / "local" / "project").exists())

    def test_service_failure_parser_rejects_unreviewed_fields_and_values(self) -> None:
        valid = {
            "schema": "rootlight.npm-service-failure/1", "action": "install",
            "reason": "early_exit", "exit_status": 3, "signal": None,
        }
        for mutation in (
            {"reason": "private-source-value"}, {"extra": "private-source-value"},
            {"exit_status": True}, {"signal": "private-source-value"},
            {"action": "private-source-value"}, {"reason": ["early_exit"]},
        ):
            with self.subTest(mutation=mutation):
                self.assertIsNone(verify_npm_install.parse_service_failure(
                    verify_npm_install.SERVICE_FAILURE_PREFIX + json.dumps(valid | mutation)
                ))
        self.assertIsNone(verify_npm_install.parse_service_failure(
            verify_npm_install.SERVICE_FAILURE_PREFIX + "x" * 1025
        ))

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

    def test_web_bootstrap_requires_the_one_time_fragment(self) -> None:
        completed = verify_npm_install.subprocess.CompletedProcess(
            args=["rootlight", "web", "--no-open"],
            returncode=0,
            stdout=(
                "Rootlight Web UI: http://127.0.0.1:43127/"
                f"#bootstrap={'a' * 43}\n"
            ),
            stderr="",
        )
        with mock.patch.object(
            verify_npm_install,
            "run",
            return_value=completed,
        ):
            bootstrap = verify_npm_install.issue_web_bootstrap(
                Path("rootlight"),
                {},
                "web bootstrap",
            )

        self.assertEqual(bootstrap, "a" * 43)

    def test_web_bootstrap_rejects_the_legacy_bare_url(self) -> None:
        completed = verify_npm_install.subprocess.CompletedProcess(
            args=["rootlight", "web", "--no-open"],
            returncode=0,
            stdout="Rootlight Web UI: http://127.0.0.1:43127/\n",
            stderr="",
        )
        with (
            mock.patch.object(
                verify_npm_install,
                "run",
                return_value=completed,
            ),
            self.assertRaisesRegex(
                verify_npm_install.NpmInstallError,
                "bootstrap URL is invalid",
            ),
        ):
            verify_npm_install.issue_web_bootstrap(
                Path("rootlight"),
                {},
                "web bootstrap",
            )

    def test_elevated_web_startup_requires_exact_policy_denial(self) -> None:
        completed = verify_npm_install.subprocess.CompletedProcess(
            args=["rootlight", "web", "--no-open"],
            returncode=6,
            stdout="",
            stderr=(
                '{"contract_version":"1.0","ok":false,'
                '"exit_family":"security_policy","error":{'
                '"code":"PERMISSION_DENIED",'
                '"message":"local web service requires a non-elevated process",'
                '"retryable":false}}'
            ),
        )
        with mock.patch.object(
            verify_npm_install,
            "run",
            return_value=completed,
        ):
            verify_npm_install.verify_elevated_startup_denial(
                Path("rootlight"),
                {},
                "elevated web startup",
            )

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
