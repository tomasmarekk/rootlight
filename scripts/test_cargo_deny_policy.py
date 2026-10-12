#!/usr/bin/env python3
"""Exercise cargo-deny's registry matching against an offline vendored workspace."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


PROJECT = Path(__file__).resolve().parents[1]


class CargoDenyPolicyTests(unittest.TestCase):
    def setUp(self) -> None:
        output = PROJECT / "target"
        output.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="cargo-deny-policy-", dir=output)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.environment = os.environ.copy()
        self.environment.update(
            CARGO_HOME=str(self.root / "empty-cargo-home"),
            CARGO_NET_OFFLINE="true",
            CARGO_REGISTRIES_CRATES_IO_PROTOCOL="sparse",
            CARGO_TARGET_DIR=str(self.root / "target"),
        )
        (self.root / ".cargo").mkdir()
        source_directory = (PROJECT / "source-inputs" / "cargo").as_posix()
        (self.root / ".cargo" / "config.toml").write_text(
            '[net]\noffline = true\n'
            '[source.crates-io]\nreplace-with = "rootlight-local-sources"\n'
            f'[source.rootlight-local-sources]\ndirectory = {json.dumps(source_directory)}\n',
            encoding="utf-8",
        )
        self.manifest = self.root / "Cargo.toml"
        self.manifest.write_text(
            '[workspace]\nmembers = ["member"]\nresolver = "2"\n'
            '[workspace.dependencies]\narrayref = "=0.3.9"\n',
            encoding="utf-8",
        )
        (self.root / "member" / "src").mkdir(parents=True)
        (self.root / "member" / "src" / "lib.rs").write_text("", encoding="utf-8")
        (self.root / "member" / "Cargo.toml").write_text(
            '[package]\nname = "policy-fixture"\nversion = "0.0.0"\n'
            'edition = "2021"\nlicense = "MIT"\n'
            '[dependencies]\narrayref.workspace = true\n',
            encoding="utf-8",
        )
        (self.root / "deny.toml").write_text(
            '[graph]\ntargets = []\nall-features = true\n'
            '[licenses]\nallow = ["MIT", "BSD-2-Clause"]\n'
            '[bans]\nmultiple-versions = "deny"\nwildcards = "deny"\n'
            '[bans.workspace-dependencies]\nunused = "deny"\n'
            '[sources]\nunknown-registry = "deny"\nunknown-git = "deny"\n',
            encoding="utf-8",
        )
        generated = self.run_command("cargo", "generate-lockfile", "--offline")
        self.assertEqual(generated.returncode, 0, generated.stdout)

    def run_command(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            arguments,
            cwd=self.root,
            env=self.environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
            timeout=60,
        )

    def check_policy(self) -> subprocess.CompletedProcess[str]:
        return self.run_command("bash", str(PROJECT / "scripts" / "check-cargo-deny.sh"))

    def test_used_registry_dependency_passes_with_empty_cache(self) -> None:
        checked = self.check_policy()
        self.assertEqual(checked.returncode, 0, checked.stdout)
        metadata = self.run_command("cargo", "metadata", "--locked", "--offline", "--format-version=1")
        self.assertEqual(metadata.returncode, 0, metadata.stdout)
        dependency = next(item for item in json.loads(metadata.stdout)["packages"] if item["name"] == "arrayref")
        self.assertEqual(dependency["source"], "registry+https://github.com/rust-lang/crates.io-index")
        self.assertEqual(
            Path(dependency["manifest_path"]).resolve(),
            (PROJECT / "source-inputs" / "cargo" / "arrayref-0.3.9" / "Cargo.toml").resolve(),
        )
        registry = Path(self.environment["CARGO_HOME"]) / "registry"
        # Cargo may initialize its cache marker without acquiring any registry data.
        cached_files = [
            str(path.relative_to(registry))
            for path in registry.rglob("*")
            if path.is_file() and path != registry / "CACHEDIR.TAG"
        ]
        self.assertEqual(cached_files, [])

    def test_unused_registry_dependency_is_rejected(self) -> None:
        with self.manifest.open("a", encoding="utf-8") as manifest:
            manifest.write('unused-policy-fixture = "=1.0.0"\n')
        checked = self.check_policy()
        self.assertNotEqual(checked.returncode, 0, checked.stdout)
        self.assertIn("unused-workspace-dependency", checked.stdout)
        self.assertIn("unused-policy-fixture", checked.stdout)

    def test_production_policy_rejects_rusqlite_session_feature(self) -> None:
        # The stand-in tests Cargo feature resolution and the real policy, without
        # acquiring or compiling the prohibited optional upstream bindgen closure.
        dependency_root = self.root / "feature-contract"
        (dependency_root / "src").mkdir(parents=True)
        (dependency_root / "src" / "lib.rs").write_text("", encoding="utf-8")
        (dependency_root / "Cargo.toml").write_text(
            '[package]\nname = "rusqlite"\nversion = "0.39.0"\n'
            'edition = "2021"\nlicense = "MIT"\n'
            '[workspace]\n[features]\nsession = []\n',
            encoding="utf-8",
        )
        manifest = self.manifest.read_text(encoding="utf-8")
        self.manifest.write_text(
            manifest.replace('members = ["member"]', 'members = ["member"]\nexclude = ["feature-contract"]'),
            encoding="utf-8",
        )
        member_manifest = self.root / "member" / "Cargo.toml"
        disabled = 'rusqlite = { path = "../feature-contract", version = "=0.39.0", default-features = false }\n'
        with member_manifest.open("a", encoding="utf-8") as member:
            member.write(disabled)
        self.environment["CARGO_REGISTRIES_CRATES_IO_PROTOCOL"] = "git"
        generated = self.run_command("cargo", "generate-lockfile", "--offline")
        self.assertEqual(generated.returncode, 0, generated.stdout)
        arguments = (
            "cargo", "deny", "--locked", "--offline", "--config", str(PROJECT / "deny.toml"), "check", "bans"
        )
        checked = self.run_command(*arguments)
        self.assertEqual(checked.returncode, 0, checked.stdout)

        member_manifest.write_text(
            member_manifest.read_text(encoding="utf-8").replace(
                disabled, disabled.replace("false }", 'false, features = ["session"] }')
            ),
            encoding="utf-8",
        )
        metadata = self.run_command(
            "cargo", "metadata", "--locked", "--offline", "--all-features", "--format-version=1"
        )
        self.assertEqual(metadata.returncode, 0, metadata.stdout)
        graph = json.loads(metadata.stdout)
        package = next(item for item in graph["packages"] if item["name"] == "rusqlite")
        node = next(item for item in graph["resolve"]["nodes"] if item["id"] == package["id"])
        self.assertEqual(node["features"], ["session"])
        checked = self.run_command(*arguments)
        self.assertNotEqual(checked.returncode, 0, checked.stdout)
        self.assertIn("feature-banned", checked.stdout)
        self.assertIn("rusqlite", checked.stdout)
        self.assertIn("session", checked.stdout)


if __name__ == "__main__":
    unittest.main()
