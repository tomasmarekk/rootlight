#!/usr/bin/env python3
"""Exercise the installed native web command without a Node.js runtime."""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path, PurePosixPath
import queue
import re
import shutil
import signal
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import zipfile


PACKAGE_SCHEMA = "rootlight.package-manifest/3"
REPORT_SCHEMA = "rootlight.installed-web-smoke/1"
SOURCE_REVISION = re.compile(r"^[0-9a-f]{40}$")
TARGET = re.compile(
    r"^(?:aarch64-apple-darwin|aarch64-unknown-linux-gnu|"
    r"x86_64-apple-darwin|x86_64-pc-windows-msvc|"
    r"x86_64-unknown-linux-gnu)$"
)
VERSION = re.compile(
    r"^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)(?:-alpha\.(?:0|[1-9][0-9]*))?$"
)
WEB_URL = re.compile(
    r"^Rootlight Web UI: "
    r"(http://127\.0\.0\.1:([1-9][0-9]{0,4}))/"
    r"#bootstrap=([A-Za-z0-9_-]{43})\r?\n$"
)
MAX_ARCHIVE_ENTRIES = 2_048
MAX_ARCHIVE_FILE_BYTES = 256 * 1024 * 1024
MAX_ARCHIVE_TOTAL_BYTES = 1024 * 1024 * 1024
MAX_HTTP_BYTES = 1024 * 1024
START_TIMEOUT_SECONDS = 30.0
STOP_TIMEOUT_SECONDS = 20.0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--candidate-version", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--target", required=True)
    return parser.parse_args()


def validated_archive_members(package: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    members = package.infolist()
    if not 0 < len(members) <= MAX_ARCHIVE_ENTRIES:
        raise ValueError("package archive entry count is invalid")
    names: set[str] = set()
    total_bytes = 0
    for member in members:
        name = member.filename
        path = PurePosixPath(name)
        unix_mode = member.external_attr >> 16
        if (
            not name
            or "\\" in name
            or path.is_absolute()
            or any(component in ("", ".", "..") for component in path.parts)
            or name in names
            or unix_mode & 0o170000 == 0o120000
            or member.file_size > MAX_ARCHIVE_FILE_BYTES
        ):
            raise ValueError("package archive contains an unsafe entry")
        names.add(name)
        total_bytes += member.file_size
        if total_bytes > MAX_ARCHIVE_TOTAL_BYTES:
            raise ValueError("package archive exceeds its total-byte limit")
    return members


def extract_archive(archive: Path, destination: Path) -> dict[str, object]:
    with zipfile.ZipFile(archive) as package:
        members = validated_archive_members(package)
        manifest_members = [
            member for member in members if member.filename == "package-manifest.json"
        ]
        if len(manifest_members) != 1:
            raise ValueError("package archive lacks one canonical manifest")
        manifest_bytes = read_zip_member(package, manifest_members[0], 4 * 1024 * 1024)
        manifest = json.loads(manifest_bytes)
        for member in members:
            target = destination.joinpath(*PurePosixPath(member.filename).parts)
            if member.is_dir():
                if target.exists():
                    if target.is_symlink() or not target.is_dir():
                        raise ValueError("package archive directory target is unsafe")
                else:
                    target.mkdir(parents=True, exist_ok=False)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.exists() or target.is_symlink():
                raise ValueError("package archive extraction target already exists")
            with package.open(member) as source, target.open("xb") as output:
                copied = shutil.copyfileobj(source, output, length=1024 * 1024)
                if copied is not None:
                    raise AssertionError("copyfileobj unexpectedly returned a value")
            if target.stat().st_size != member.file_size:
                raise ValueError("package archive member changed while extracting")
            mode = (member.external_attr >> 16) & 0o777
            if mode:
                target.chmod(mode)
    if not isinstance(manifest, dict):
        raise ValueError("package manifest is not a JSON object")
    return manifest


def read_zip_member(
    package: zipfile.ZipFile, member: zipfile.ZipInfo, maximum_bytes: int
) -> bytes:
    if member.file_size > maximum_bytes:
        raise ValueError("package archive metadata file is oversized")
    with package.open(member) as source:
        content = source.read(maximum_bytes + 1)
    if len(content) != member.file_size or len(content) > maximum_bytes:
        raise ValueError("package archive metadata file changed while reading")
    return content


def validate_manifest(
    manifest: dict[str, object],
    candidate_version: str,
    source_revision: str,
    target: str,
) -> dict[str, str]:
    expected = {
        "schema": PACKAGE_SCHEMA,
        "source_revision": source_revision,
        "target": target,
        "version": candidate_version,
    }
    if {key: manifest.get(key) for key in expected} != expected:
        raise ValueError("package manifest identity differs from the smoke request")
    entries = manifest.get("entries")
    if not isinstance(entries, list):
        raise ValueError("package manifest entries are missing")
    identities: dict[str, str] = {}
    for entry in entries:
        if (
            not isinstance(entry, dict)
            or not isinstance(entry.get("path"), str)
            or not isinstance(entry.get("sha256"), str)
            or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"])
            or entry["path"] in identities
        ):
            raise ValueError("package manifest entry is invalid")
        identities[entry["path"]] = entry["sha256"]
    suffix = ".exe" if target == "x86_64-pc-windows-msvc" else ""
    required = (
        f"bin/rootlight{suffix}",
        f"bin/rootlight-daemon{suffix}",
        f"bin/rootlight-web{suffix}",
        "share/rootlight/web/asset-manifest.json",
        "share/rootlight/web/index.html",
    )
    if any(path not in identities for path in required):
        raise ValueError("package manifest lacks installed web runtime entries")
    return identities


def verify_installed_hashes(root: Path, identities: dict[str, str]) -> None:
    for path, expected in identities.items():
        if not (
            path.startswith("bin/")
            or path
            in (
                "share/rootlight/web/asset-manifest.json",
                "share/rootlight/web/index.html",
            )
        ):
            continue
        source = root.joinpath(*PurePosixPath(path).parts)
        if source.is_symlink() or not source.is_file():
            raise ValueError(f"installed package file is unavailable: {path}")
        if hashlib.sha256(source.read_bytes()).hexdigest() != expected:
            raise ValueError(f"installed package file hash differs: {path}")


def process_environment(root: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment["ROOTLIGHT_STATE_DIR"] = str(root / "state")
    environment["ROOTLIGHT_RUNTIME_DIR"] = str(root / "runtime")
    return environment


def wait_for_daemon(
    rootlight: Path,
    environment: dict[str, str],
    process: subprocess.Popen[bytes],
) -> None:
    discovery = Path(environment["ROOTLIGHT_RUNTIME_DIR"]) / "daemon.json"
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("installed daemon exited before becoming ready")
        if not discovery.is_file():
            time.sleep(0.1)
            continue
        try:
            completed = subprocess.run(
                [rootlight, "health"],
                env=environment,
                check=False,
                capture_output=True,
                timeout=3,
            )
            document = json.loads(completed.stdout)
            if (
                completed.returncode == 0
                and document.get("ok") is True
                and document.get("result", {}).get("data", {}).get("ready") is True
            ):
                return
        except (json.JSONDecodeError, subprocess.TimeoutExpired):
            pass
        time.sleep(0.1)
    raise TimeoutError("installed daemon did not become ready")


def start_web(
    rootlight: Path, environment: dict[str, str]
) -> tuple[subprocess.Popen[str], str, str]:
    options: dict[str, object] = {}
    if os.name == "nt":
        options["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
    else:
        options["start_new_session"] = True
    process = subprocess.Popen(
        [rootlight, "web", "--no-open"],
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        **options,
    )
    assert process.stdout is not None
    line_queue: queue.Queue[str] = queue.Queue(maxsize=1)
    reader = threading.Thread(
        target=lambda: line_queue.put(process.stdout.readline()),
        name="rootlight-web-url",
        daemon=True,
    )
    reader.start()
    try:
        line = line_queue.get(timeout=START_TIMEOUT_SECONDS)
    except queue.Empty as error:
        raise TimeoutError("installed web host did not publish its URL") from error
    match = WEB_URL.fullmatch(line)
    if match is None or int(match.group(2)) > 65_535:
        raise ValueError("installed web host published an invalid URL")
    return process, match.group(1), match.group(3)


def read_http_response(response: http.client.HTTPResponse) -> bytes:
    content = response.read(MAX_HTTP_BYTES + 1)
    if len(content) > MAX_HTTP_BYTES:
        raise ValueError("installed web response exceeded its byte limit")
    return content


def exercise_session(origin: str, secret: str, expected_index_sha256: str) -> None:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(f"{origin}/", timeout=5) as response:
        index = read_http_response(response)
        if (
            response.status != 200
            or not response.headers.get("Content-Security-Policy")
            or hashlib.sha256(index).hexdigest() != expected_index_sha256
        ):
            raise ValueError("installed web entrypoint response is invalid")

    body = json.dumps({"secret": secret}, separators=(",", ":")).encode("utf-8")
    request = urllib.request.Request(
        f"{origin}/api/v1/session/bootstrap",
        data=body,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Origin": origin,
            "Sec-Fetch-Site": "same-origin",
        },
    )
    with opener.open(request, timeout=5) as response:
        session_body = read_http_response(response)
        cookie = response.headers.get("Set-Cookie", "").split(";", 1)[0]
    session = json.loads(session_body)
    if (
        not cookie.startswith("rootlight_session=")
        or not isinstance(session.get("csrfToken"), str)
        or len(session["csrfToken"]) != 43
        or secret.encode("ascii") in session_body
    ):
        raise ValueError("installed web bootstrap response is invalid")

    request = urllib.request.Request(
        f"{origin}/api/v1/health",
        method="GET",
        headers={
            "Cookie": cookie,
            "Origin": origin,
            "Sec-Fetch-Site": "same-origin",
        },
    )
    with opener.open(request, timeout=5) as response:
        health_body = read_http_response(response)
        if (
            response.status != 200
            or response.headers.get("Cache-Control") != "no-store"
            or not response.headers.get("Content-Security-Policy")
        ):
            raise ValueError("installed web health response headers are invalid")
    health = json.loads(health_body)
    if health.get("webReady") is not True or health.get("daemonReady") is not True:
        raise ValueError("installed web health response is not ready")


def stop_web(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        raise RuntimeError("installed web command exited before shutdown")
    if os.name == "nt":
        process.send_signal(signal.CTRL_BREAK_EVENT)
    else:
        os.killpg(process.pid, signal.SIGINT)
    try:
        process.wait(timeout=STOP_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as error:
        process.kill()
        process.wait()
        raise TimeoutError(
            "installed web command did not stop after interrupt"
        ) from error


def stop_daemon(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        raise RuntimeError("installed daemon exited before shutdown")
    assert process.stdin is not None
    process.stdin.write(b"shutdown\n")
    process.stdin.flush()
    process.stdin.close()
    try:
        return_code = process.wait(timeout=STOP_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as error:
        process.kill()
        process.wait()
        raise TimeoutError("installed daemon did not stop after shutdown") from error
    if return_code != 0:
        raise RuntimeError("installed daemon shutdown failed")


def main() -> None:
    args = parse_args()
    if (
        not VERSION.fullmatch(args.candidate_version)
        or not SOURCE_REVISION.fullmatch(args.source_revision)
        or not TARGET.fullmatch(args.target)
    ):
        raise ValueError("installed web smoke identity is invalid")
    if args.archive.is_symlink():
        raise ValueError("installed web smoke archive is not a regular file")
    archive = args.archive.resolve(strict=True)
    if not archive.is_file():
        raise ValueError("installed web smoke archive is not a regular file")
    output = args.output.resolve(strict=False)
    if output.exists() or output.is_symlink() or not output.parent.is_dir():
        raise ValueError("installed web smoke output must be a new file")

    with tempfile.TemporaryDirectory(prefix="rootlight-web-smoke-") as temporary:
        root = Path(temporary)
        package_root = root / "package"
        package_root.mkdir()
        manifest = extract_archive(archive, package_root)
        identities = validate_manifest(
            manifest, args.candidate_version, args.source_revision, args.target
        )
        verify_installed_hashes(package_root, identities)
        suffix = ".exe" if os.name == "nt" else ""
        rootlight = package_root / "bin" / f"rootlight{suffix}"
        daemon = package_root / "bin" / f"rootlight-daemon{suffix}"
        environment = process_environment(root)
        daemon_process = subprocess.Popen(
            [daemon, "--supervised-stdio"],
            env=environment,
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        web_process: subprocess.Popen[str] | None = None
        try:
            wait_for_daemon(rootlight, environment, daemon_process)
            web_process, origin, secret = start_web(rootlight, environment)
            exercise_session(
                origin, secret, identities["share/rootlight/web/index.html"]
            )
            stop_web(web_process)
            web_process = None
            stop_daemon(daemon_process)
        finally:
            if web_process is not None and web_process.poll() is None:
                web_process.kill()
                web_process.wait()
            if daemon_process.poll() is None:
                daemon_process.kill()
                daemon_process.wait()

        report = {
            "schema": REPORT_SCHEMA,
            "archive": archive.name,
            "asset_manifest_sha256": identities[
                "share/rootlight/web/asset-manifest.json"
            ],
            "authenticated_health_observed": True,
            "candidate_version": args.candidate_version,
            "graceful_shutdown_observed": True,
            "native_cli_dispatch_observed": True,
            "node_runtime_required": False,
            "session_bootstrap_observed": True,
            "source_revision": args.source_revision,
            "target": args.target,
        }
        with output.open("x", encoding="utf-8", newline="\n") as destination:
            json.dump(report, destination, indent=2, sort_keys=True)
            destination.write("\n")


if __name__ == "__main__":
    main()
