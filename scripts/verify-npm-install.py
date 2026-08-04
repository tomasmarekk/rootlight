#!/usr/bin/env python3
"""Exercise the complete Rootlight npm install and uninstall contract."""

from __future__ import annotations

import argparse
import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

ROOT_PACKAGE = "@tomasmarekk/rootlight"
ORIGIN_HOST = "127.0.0.1"
ORIGIN_PORT = 43_127
COMMAND_TIMEOUT_SECONDS = 180
HTTP_TIMEOUT_SECONDS = 5
STOP_TIMEOUT_SECONDS = 30
MAX_COMMAND_OUTPUT_BYTES = 1024 * 1024
TARGET_PACKAGES = {
    "aarch64-apple-darwin": "@tomasmarekk/rootlight-darwin-arm64",
    "aarch64-unknown-linux-gnu": "@tomasmarekk/rootlight-linux-arm64-gnu",
    "x86_64-apple-darwin": "@tomasmarekk/rootlight-darwin-x64",
    "x86_64-unknown-linux-gnu": "@tomasmarekk/rootlight-linux-x64-gnu",
    "x86_64-pc-windows-msvc": "@tomasmarekk/rootlight-win32-x64-msvc",
}


class NpmInstallError(RuntimeError):
    """The observable npm installation contract was not satisfied."""


def parse_args(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--packages-dir", type=Path, required=True)
    parser.add_argument("--target", choices=tuple(TARGET_PACKAGES), required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    return parser.parse_args(arguments)


def main(arguments: list[str]) -> int:
    options = parse_args(arguments)
    try:
        verify_install(
            options.packages_dir,
            options.target,
            options.output_dir,
            options.evidence,
        )
    except (NpmInstallError, OSError, subprocess.SubprocessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


def verify_install(
    packages_dir: Path,
    target: str,
    output_dir: Path,
    evidence_path: Path,
) -> None:
    npm = npm_executable()
    packages_dir = checked_directory(packages_dir, "npm package directory")
    native_package = TARGET_PACKAGES[target]
    native_directory = native_package.rsplit("/", 1)[-1]
    root_directory = packages_dir / "rootlight"
    platform_directory = packages_dir / native_directory
    checked_directory(root_directory, "root npm package")
    checked_directory(platform_directory, "platform npm package")

    output_dir.mkdir(parents=True, exist_ok=False)
    pack_prefix = (output_dir / "pack-prefix").resolve()
    cache = (output_dir / "pack-cache").resolve()
    tarballs = (output_dir / "tarballs").resolve()
    pack_prefix.mkdir()
    cache.mkdir()
    tarballs.mkdir()
    pack_environment = npm_environment(
        pack_prefix,
        cache,
        output_dir / "pack-state",
        output_dir / "pack-runtime",
    )

    root_tarball = pack(root_directory, tarballs, pack_environment, npm)
    native_tarball = pack(platform_directory, tarballs, pack_environment, npm)
    modes = [
        verify_install_mode(
            mode,
            output_dir,
            root_tarball,
            native_tarball,
            native_directory,
            native_package,
            npm,
        )
        for mode in ("local", "global")
    ]
    evidence = {
        "modes": modes,
        "native_package": native_package,
        "origin": f"http://{ORIGIN_HOST}:{ORIGIN_PORT}",
        "schema": "rootlight.npm-install-smoke/2",
        "target": target,
    }
    write_json_new(evidence_path, evidence)


def verify_install_mode(
    mode: str,
    output_dir: Path,
    root_tarball: Path,
    native_tarball: Path,
    native_directory: str,
    native_package: str,
    npm: str,
) -> dict[str, object]:
    root = (output_dir / mode).resolve()
    prefix = root / "prefix"
    cache = root / "cache"
    state = root / "state"
    runtime = root / "runtime"
    project = root / "project"
    root.mkdir()
    prefix.mkdir()
    cache.mkdir()
    if mode == "local":
        project.mkdir()
        write_json_new(project / "package.json", {"private": True})
    environment = npm_environment(prefix, cache, state, runtime)
    cli = npm_cli_path(prefix)
    package_root = installed_package_path(mode, prefix, project)
    native_cli = native_cli_path(package_root, native_directory)
    install = [
        npm,
        "install",
        "--offline",
        "--no-audit",
        "--no-fund",
        "--no-update-notifier",
        str(root_tarball),
        str(native_tarball),
    ]
    if mode == "global":
        install.insert(2, "--global")
    else:
        install[2:2] = ["--prefix", str(project)]
    started = time.monotonic()

    try:
        run(install, environment, stage=f"{mode} npm install")
        install_seconds = time.monotonic() - started
        if not cli.is_file():
            raise NpmInstallError(
                f"{mode} npm install did not create the Rootlight command shim"
            )
        command = shutil.which("rootlight", path=environment["PATH"])
        if command is None or Path(command).resolve() != cli.resolve():
            raise NpmInstallError(
                f"{mode} npm install did not expose bare rootlight on PATH"
            )

        initial = service_status(cli, environment, f"{mode} postinstall service status")
        require_service_state(initial, registered=True, running=True)
        initial_pid = require_pid(initial)
        verify_http_session()

        stopped = run_json(
            [str(cli), "service", "stop"],
            environment,
            stage=f"{mode} service stop",
        )
        require_service_state(service_data(stopped), registered=True, running=False)
        wait_for_port(closed=True)

        restarted = run_json(
            [str(cli), "service", "restart"],
            environment,
            stage=f"{mode} service restart",
        )
        restarted_data = service_data(restarted)
        require_service_state(restarted_data, registered=True, running=True)
        restarted_pid = require_pid(restarted_data)
        verify_http_session()
        sentinel = state / "uninstall-sentinel"
        sentinel.write_bytes(b"owned state")

        run([str(cli), "uninstall"], environment, stage=f"{mode} rootlight uninstall")
        if (
            cli.exists()
            or package_root.exists()
            or native_cli.exists()
            or native_cli.parent.parent.exists()
        ):
            raise NpmInstallError(f"{mode} uninstall retained an npm package")
        if state.exists() or runtime.exists():
            raise NpmInstallError(f"{mode} uninstall retained Rootlight-owned data")
        wait_for_port(closed=True)
        return {
            "install_seconds": round(install_seconds, 3),
            "initial_pid": initial_pid,
            "mode": mode,
            "restarted_pid": restarted_pid,
            "uninstall_removed_data": True,
            "uninstall_removed_native_package": True,
            "uninstall_removed_root_package": True,
        }
    finally:
        cleanup(
            mode,
            prefix,
            project,
            native_cli,
            native_package,
            environment,
            npm,
        )


def checked_directory(path: Path, label: str) -> Path:
    resolved = path.resolve()
    if not resolved.is_dir() or resolved.is_symlink():
        raise NpmInstallError(f"{label} is invalid")
    return resolved


def npm_environment(
    prefix: Path,
    cache: Path,
    state: Path,
    runtime: Path,
) -> dict[str, str]:
    environment = os.environ.copy()
    command_directory = prefix if os.name == "nt" else prefix / "bin"
    environment.update(
        {
            "NPM_CONFIG_AUDIT": "false",
            "NPM_CONFIG_CACHE": str(cache),
            "NPM_CONFIG_FUND": "false",
            "NPM_CONFIG_PREFIX": str(prefix),
            "NPM_CONFIG_UPDATE_NOTIFIER": "false",
            "PATH": os.pathsep.join(
                (str(command_directory), environment.get("PATH", ""))
            ),
            "ROOTLIGHT_RUNTIME_DIR": str(runtime.resolve()),
            "ROOTLIGHT_STATE_DIR": str(state.resolve()),
        }
    )
    return environment


def npm_executable(platform: str = os.name) -> str:
    command = "npm.cmd" if platform == "nt" else "npm"
    executable = shutil.which(command)
    if executable is None:
        raise NpmInstallError(f"{command} is not available on PATH")
    return executable


def pack(
    package: Path,
    tarballs: Path,
    environment: dict[str, str],
    npm: str,
) -> Path:
    completed = run(
        [
            npm,
            "pack",
            str(package),
            "--json",
            "--ignore-scripts",
            "--pack-destination",
            str(tarballs),
        ],
        environment,
    )
    try:
        document = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise NpmInstallError("npm pack returned invalid JSON") from error
    if (
        not isinstance(document, list)
        or len(document) != 1
        or not isinstance(document[0], dict)
        or not isinstance(document[0].get("filename"), str)
    ):
        raise NpmInstallError("npm pack returned an unexpected result")
    tarball = (tarballs / document[0]["filename"]).resolve()
    if tarball.parent != tarballs or not tarball.is_file() or tarball.is_symlink():
        raise NpmInstallError("npm pack created an invalid tarball")
    return tarball


def run(
    command: list[str],
    environment: dict[str, str],
    *,
    check: bool = True,
    stage: str = "command",
) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        command,
        check=False,
        cwd=Path.cwd(),
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=COMMAND_TIMEOUT_SECONDS,
    )
    if (
        len(completed.stdout.encode("utf-8")) > MAX_COMMAND_OUTPUT_BYTES
        or len(completed.stderr.encode("utf-8")) > MAX_COMMAND_OUTPUT_BYTES
    ):
        raise NpmInstallError("npm smoke command output exceeded its bound")
    if check and completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise NpmInstallError(
            f"{stage} failed with exit code {completed.returncode}: {detail[:2000]}"
        )
    return completed


def run_json(
    command: list[str],
    environment: dict[str, str],
    *,
    stage: str = "Rootlight command",
) -> dict[str, Any]:
    completed = run(command, environment, stage=stage)
    try:
        document = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise NpmInstallError("Rootlight returned invalid JSON") from error
    if not isinstance(document, dict) or document.get("ok") is not True:
        raise NpmInstallError("Rootlight returned an unsuccessful response")
    return document


def service_status(
    cli: Path,
    environment: dict[str, str],
    stage: str = "service status",
) -> dict[str, Any]:
    return service_data(
        run_json(
            [str(cli), "service", "status"],
            environment,
            stage=stage,
        )
    )


def service_data(document: dict[str, Any]) -> dict[str, Any]:
    result = document.get("result")
    if (
        not isinstance(result, dict)
        or result.get("type") != "web_service"
        or not isinstance(result.get("data"), dict)
    ):
        raise NpmInstallError("Rootlight service response has the wrong shape")
    return result["data"]


def require_service_state(
    data: dict[str, Any], *, registered: bool, running: bool
) -> None:
    if (
        data.get("schema_version") != 1
        or data.get("registered") is not registered
        or data.get("running") is not running
        or data.get("origin") != f"http://{ORIGIN_HOST}:{ORIGIN_PORT}"
    ):
        raise NpmInstallError(f"Rootlight service state differs: {data!r}")
    if running != isinstance(data.get("pid"), int):
        raise NpmInstallError("Rootlight service PID state differs")


def require_pid(data: dict[str, Any]) -> int:
    pid = data.get("pid")
    if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 0:
        raise NpmInstallError("Rootlight service PID is invalid")
    return pid


def verify_http_session() -> None:
    connection = http.client.HTTPConnection(
        ORIGIN_HOST, ORIGIN_PORT, timeout=HTTP_TIMEOUT_SECONDS
    )
    try:
        connection.request("GET", "/")
        response = connection.getresponse()
        body = response.read(4 * 1024 * 1024 + 1)
        cookie = response.getheader("Set-Cookie")
    finally:
        connection.close()
    if response.status != 200 or len(body) > 4 * 1024 * 1024:
        raise NpmInstallError("Rootlight Web UI root response is invalid")
    if (
        not isinstance(cookie, str)
        or not cookie.startswith("rootlight_session=")
        or "HttpOnly" not in cookie
        or "SameSite=Strict" not in cookie
        or b"#bootstrap=" in body
    ):
        raise NpmInstallError("Rootlight Web UI session cookie contract differs")

    connection = http.client.HTTPConnection(
        ORIGIN_HOST, ORIGIN_PORT, timeout=HTTP_TIMEOUT_SECONDS
    )
    try:
        connection.request(
            "GET",
            "/api/v1/session",
            headers={
                "Cookie": cookie.split(";")[0],
                "Sec-Fetch-Site": "same-origin",
            },
        )
        response = connection.getresponse()
        session_body = response.read(64 * 1024 + 1)
    finally:
        connection.close()
    if response.status != 200 or len(session_body) > 64 * 1024:
        raise NpmInstallError("Rootlight session endpoint response is invalid")
    try:
        session = json.loads(session_body)
    except json.JSONDecodeError as error:
        raise NpmInstallError("Rootlight session endpoint returned invalid JSON") from error
    if (
        not isinstance(session, dict)
        or not isinstance(session.get("csrfToken"), str)
        or not session["csrfToken"]
        or session.get("idleTtlSeconds") != 1800
    ):
        raise NpmInstallError("Rootlight session endpoint contract differs")


def wait_for_port(*, closed: bool) -> None:
    deadline = time.monotonic() + STOP_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
            probe.settimeout(0.25)
            connected = probe.connect_ex((ORIGIN_HOST, ORIGIN_PORT)) == 0
        if connected is not closed:
            return
        time.sleep(0.1)
    expected = "close" if closed else "open"
    raise NpmInstallError(f"Rootlight Web UI port did not {expected}")


def npm_cli_path(prefix: Path) -> Path:
    if os.name == "nt":
        return prefix / "rootlight.cmd"
    return prefix / "bin" / "rootlight"


def global_root_package_path(prefix: Path) -> Path:
    if os.name == "nt":
        return prefix / "node_modules" / "@tomasmarekk" / "rootlight"
    return prefix / "lib" / "node_modules" / "@tomasmarekk" / "rootlight"


def installed_package_path(mode: str, prefix: Path, project: Path) -> Path:
    if mode == "local":
        return project / "node_modules" / "@tomasmarekk" / "rootlight"
    return global_root_package_path(prefix)


def native_cli_path(package_root: Path, native_directory: str) -> Path:
    native_root = package_root.parent / native_directory
    suffix = ".exe" if os.name == "nt" else ""
    return native_root / "bin" / f"rootlight{suffix}"


def cleanup(
    mode: str,
    prefix: Path,
    project: Path,
    native_cli: Path,
    native_package: str,
    environment: dict[str, str],
    npm: str,
) -> None:
    if native_cli.is_file():
        run(
            [str(native_cli), "service", "uninstall"],
            environment,
            check=False,
        )
    uninstall = [npm, "uninstall", ROOT_PACKAGE, native_package]
    if mode == "global":
        uninstall.insert(2, "--global")
    else:
        uninstall[2:2] = ["--prefix", str(project)]
    run(uninstall, environment, check=False)
    if prefix.exists():
        shutil.rmtree(prefix)
    if project.exists():
        shutil.rmtree(project)


def write_json_new(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
