#!/usr/bin/env python3
"""Tests for deterministic native npm package preparation."""

from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
import zipfile
from pathlib import Path

import prepare_npm_packages as npm_packages

REVISION = "0123456789abcdef0123456789abcdef01234567"
VERSION = "0.1.0-alpha.1"


class PackagePreparationTests(unittest.TestCase):
    def test_release_packages_bind_every_native_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            candidates = root / "candidates"
            candidates.mkdir()
            for target in npm_packages.TARGETS:
                write_candidate(candidates, target, VERSION, REVISION)

            output = root / "output"
            npm_packages.prepare_release(
                workspace_root(), candidates, VERSION, REVISION, output
            )

            order = json.loads((output / "publish-order.json").read_bytes())
            self.assertEqual(len(order), 6)
            self.assertEqual(order[-1]["name"], "@tomasmarekk/rootlight")
            root_package = json.loads((output / "rootlight/package.json").read_bytes())
            self.assertEqual(root_package["version"], VERSION)
            self.assertEqual(root_package["gitHead"], REVISION)
            self.assertEqual(root_package["license"], npm_packages.PACKAGE_LICENSE)
            self.assertEqual(root_package["publishConfig"]["provenance"], True)
            self.assertEqual(
                root_package["bin"],
                {
                    "rootlight": "bin/rootlight.mjs",
                    "rootlight-mcp": "bin/rootlight-mcp.mjs",
                },
            )
            self.assertEqual(len(root_package["optionalDependencies"]), 5)
            for runtime_file in (
                "rootlight.mjs",
                "rootlight-mcp.mjs",
                "run-native.mjs",
            ):
                self.assertTrue((output / "rootlight/bin" / runtime_file).is_file())
            for target in npm_packages.TARGETS:
                package_dir = output / target.directory_name
                package = json.loads((package_dir / "package.json").read_bytes())
                self.assertEqual(package["name"], target.package_name)
                self.assertEqual(package["license"], npm_packages.PACKAGE_LICENSE)
                self.assertEqual(package["os"], [target.os_name])
                self.assertEqual(package["cpu"], [target.cpu])
                suffix = ".exe" if target.os_name == "win32" else ""
                for executable in npm_packages.PUBLIC_EXECUTABLES:
                    self.assertTrue(
                        (package_dir / "bin" / f"{executable}{suffix}").is_file()
                    )

    def test_archive_digest_and_paths_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            candidates = root / "candidates"
            candidates.mkdir()
            for target in npm_packages.TARGETS:
                write_candidate(candidates, target, VERSION, REVISION)
            first = npm_packages.TARGETS[0]
            archive = candidates / f"rootlight-{VERSION}-{first.triple}.zip"
            with archive.open("ab") as output:
                output.write(b"tampered")

            with self.assertRaises(npm_packages.PackagePreparationError):
                npm_packages.prepare_release(
                    workspace_root(), candidates, VERSION, REVISION, root / "output"
                )

    def test_missing_public_executable_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            candidates = root / "candidates"
            candidates.mkdir()
            for index, target in enumerate(npm_packages.TARGETS):
                executables = (
                    ("rootlight",)
                    if index == 0
                    else npm_packages.PUBLIC_EXECUTABLES
                )
                write_candidate(
                    candidates,
                    target,
                    VERSION,
                    REVISION,
                    public_executables=executables,
                )

            with self.assertRaises(npm_packages.PackagePreparationError):
                npm_packages.prepare_release(
                    workspace_root(), candidates, VERSION, REVISION, root / "output"
                )

    def test_legacy_manifest_schema_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            candidates = root / "candidates"
            candidates.mkdir()
            for index, target in enumerate(npm_packages.TARGETS):
                write_candidate(
                    candidates,
                    target,
                    VERSION,
                    REVISION,
                    manifest_schema=(
                        "rootlight.package-manifest/1"
                        if index == 0
                        else npm_packages.PACKAGE_MANIFEST_SCHEMA
                    ),
                )

            with self.assertRaises(npm_packages.PackagePreparationError):
                npm_packages.prepare_release(
                    workspace_root(), candidates, VERSION, REVISION, root / "output"
                )

    def test_bootstrap_is_bounded_and_has_no_runtime_entrypoint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "bootstrap"
            npm_packages.prepare_bootstrap(
                workspace_root(), npm_packages.BOOTSTRAP_VERSION, output
            )
            order = json.loads((output / "publish-order.json").read_bytes())
            self.assertEqual(len(order), 6)
            package = json.loads((output / "rootlight/package.json").read_bytes())
            self.assertEqual(package["license"], npm_packages.PACKAGE_LICENSE)
            self.assertNotIn("bin", package)
            self.assertNotIn("optionalDependencies", package)


def write_candidate(
    directory: Path,
    target: npm_packages.NativeTarget,
    version: str,
    source_revision: str,
    *,
    public_executables: tuple[str, ...] = npm_packages.PUBLIC_EXECUTABLES,
    manifest_schema: str = npm_packages.PACKAGE_MANIFEST_SCHEMA,
) -> None:
    archive = directory / f"rootlight-{version}-{target.triple}.zip"
    manifest = {
        "schema": manifest_schema,
        "source_revision": source_revision,
        "target": target.triple,
        "version": version,
    }
    manifest_info = zipfile.ZipInfo("package-manifest.json")
    manifest_info.create_system = 3
    manifest_info.external_attr = 0o100644 << 16
    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_STORED) as bundle:
        suffix = ".exe" if target.os_name == "win32" else ""
        for name in public_executables:
            executable = zipfile.ZipInfo(f"bin/{name}{suffix}")
            executable.create_system = 3
            executable.external_attr = 0o100755 << 16
            bundle.writestr(executable, b"native")
        bundle.writestr(manifest_info, json.dumps(manifest).encode("utf-8"))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(f"{archive.name}.sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="ascii", newline="\n"
    )


def workspace_root() -> Path:
    return Path(__file__).resolve().parent.parent


if __name__ == "__main__":
    unittest.main()
