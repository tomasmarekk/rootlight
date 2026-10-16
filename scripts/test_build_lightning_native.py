#!/usr/bin/env python3
"""Exercise native build admission and artifact joins with finite synthetic inputs.

Source authentication and Cargo children are modeled. No native implementation,
package installer, compiler or independently blocked network is executed.
"""

from __future__ import annotations

import hashlib
from contextlib import contextmanager, ExitStack
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import build_lightning_native as native


ANCESTOR_CHECK = native._ancestor_configs
HELPER_CHECK = native._helper_inputs


class NativeBuildTests(unittest.TestCase):
    """Keep byte/source obligations independent of a successful child summary."""

    def setUp(self) -> None:
        """Stage a tiny authored cdylib source tree and declared fake compilers."""
        temporary = tempfile.TemporaryDirectory(prefix="rootlight-native-build-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source, self.work = self.root / "immutable", self.root / "work"
        self.source.mkdir()
        self.work.mkdir()
        self.original = self.source / native.install.yarn.PRODUCER_PATH
        self.tree = self.original / "tree"
        self.tree.mkdir(parents=True)
        members = [{"path": native.install.yarn.producer.ROOT, "kind": "directory", "archive_mode": 0o755}]
        for name, payload in {
            "Cargo.toml": b'[workspace]\nmembers = ["node"]\n',
            "Cargo.lock": b"fixture lock\n",
            "node/Cargo.toml": b'[package]\nname = "lightningcss_node"\nversion = "0.1.0"\n[lib]\ncrate-type = ["cdylib"]\n',
            "node/src/lib.rs": b"// synthetic source; never compiled\n",
            ".cargo/config.toml": b"[net]\noffline = true\n",
        }.items():
            path = self.tree / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(payload)
            members.append({"path": native.install.yarn.producer.ROOT + "/" + name, "kind": "file",
                            "archive_mode": 0o644, "sha256": hashlib.sha256(payload).hexdigest()})
        self.proof = {"source_inventory": {"members": members}}
        self.report = self.original / "source-tree.json"
        self.report.write_text(json.dumps(self.proof), encoding="utf-8")
        self.security = {"schema": "synthetic authentication", "producer": {
            "source_tree_report": native._record(self.report)}}
        self.cargo = {"synthetic": "independent registry verifier result"}
        self.tool_file = self.root / "toolchain.json"
        tools = self.root / "tools"
        tools.mkdir()
        self.target = "x86_64-pc-windows-msvc" if os.name == "nt" else "x86_64-unknown-linux-gnu"
        self.toolchain = {"schema": "rootlight.lightning-native-toolchain/1", "target": self.target,
                          "environment": {"PATH": str(tools)}, "inputs": []}
        for name in ("cargo", "rustc"):
            path = tools / (name + (".exe" if os.name == "nt" else ""))
            path.write_bytes(("synthetic " + name + "; never executed").encode())
            self.toolchain[name] = {"path": str(path), "sha256": native._record(path)["sha256"],
                                    "version": name + " 1.97.1 (synthetic)"}
        self.tool_file.write_text(json.dumps(self.toolchain), encoding="utf-8")
        self.calls = []
        self.behavior = "positive"
        for owner, name, value in (
            (native, "_authenticate", mock.Mock(return_value=(self.security, self.cargo))),
            (native.install.security, "effective_producer", mock.Mock(return_value=(self.proof, {}))),
            (native, "_ancestor_configs", mock.Mock()),
            (native, "_helper_inputs", mock.Mock(return_value=[])),
            (native.subprocess, "Popen", self.process),
        ):
            patcher = mock.patch.object(owner, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def process(self, command, **options):
        """Model direct Cargo events while asserting fresh-target and offline policy."""
        self.calls.append(command)
        self.assertFalse(options["shell"])
        env = options["env"]
        self.assertNotIn("RUSTC_WRAPPER", env)
        self.assertNotIn("RUSTFLAGS", env)
        self.assertEqual(env["CARGO_NET_OFFLINE"], "true")
        if command[-1] == "--version":
            options["stdout"].write((self.toolchain["cargo"]["version"] + "\n").encode())
        elif command[-1] == "-vV":
            host = self.target if self.behavior != "wrong-host" else "aarch64-apple-darwin"
            options["stdout"].write((self.toolchain["rustc"]["version"] + "\nhost: " + host + "\n").encode())
        else:
            self.assertEqual(command, [self.toolchain["cargo"]["path"], "build", "--manifest-path",
                                      str(self.work / "producer/node/Cargo.toml"), "--package", "lightningcss_node",
                                      "--lib", "--release", "--locked", "--offline", "--target", self.target,
                                      "--message-format=json"])
            target = Path(env["CARGO_TARGET_DIR"])
            self.assertEqual(list(target.iterdir()), [])
            self.assertEqual(list(Path(env["CARGO_HOME"]).iterdir()), [])
            output = target / self.target / "release" / native.TARGETS[self.target][0]
            output.parent.mkdir(parents=True)
            output.write_bytes(b"synthetic compiled native bytes\x00\xff")
            dependency = target / "release/deps/dependency.rlib"
            dependency.parent.mkdir(parents=True)
            dependency.write_bytes(b"synthetic actual dependency artifact")
            out_dir = target / "release/build/napi/out"
            out_dir.mkdir(parents=True)
            (out_dir / "node-x64.lib").write_bytes(b"synthetic generated native import library")
            event = {"reason": "compiler-artifact", "package_id": "synthetic-native-id",
                     "manifest_path": str(self.work / "producer/node/Cargo.toml"),
                     "target": {"name": "lightningcss_node", "kind": ["cdylib"], "crate_types": ["cdylib"]},
                     "profile": {"test": False, "opt_level": "3"}, "fresh": False, "filenames": [str(output)]}
            events = [{"reason": "compiler-artifact", "target": {"name": "dependency"},
                       "filenames": [str(dependency)]},
                      {"reason": "build-script-executed", "out_dir": str(out_dir)}, event,
                      {"reason": "build-finished", "success": True}]
            if self.behavior == "no-native":
                events.remove(event)
            elif self.behavior == "duplicate-native":
                events.insert(0, event.copy())
            elif self.behavior == "false-finished":
                events[-1]["success"] = False
            elif self.behavior == "debug-native":
                event["profile"]["opt_level"] = "0"
            elif self.behavior == "fresh-native":
                event["fresh"] = True
            elif self.behavior == "wrong-manifest":
                event["manifest_path"] = str(self.work / "producer/Cargo.toml")
            elif self.behavior == "escape-artifact":
                event["filenames"] = [str(self.tree / "Cargo.lock")]
            elif self.behavior == "escape-dependency":
                events[0]["filenames"] = [str(self.tree / "Cargo.lock")]
            elif self.behavior == "malformed-events":
                options["stdout"].write(b"not-json\n")
                events = []
            elif self.behavior == "source-mutation":
                (self.work / "producer/node/src/lib.rs").write_bytes(b"changed during child")
            elif self.behavior == "source-addition":
                (self.work / "producer/node/unreported.rs").write_bytes(b"unreported input")
            elif self.behavior == "tool-mutation":
                Path(self.toolchain["rustc"]["path"]).write_bytes(b"changed compiler")
            elif self.behavior == "helper-mutation":
                (self.source / "scripts/patch_npm_source_fixture.py").write_bytes(b"changed transitive helper during child")
            elif self.behavior == "registry-cache":
                cached = Path(env["CARGO_HOME"]) / "registry/src/foreign"
                cached.mkdir(parents=True)
            elif self.behavior == "registry-junk":
                cached = Path(env["CARGO_HOME"]) / "registry"
                cached.mkdir()
                (cached / "unreported").write_bytes(b"unexpected registry payload")
            for row in events:
                options["stdout"].write((json.dumps(row) + "\n").encode())
        code = 31 if self.behavior == "nonzero" and command[1] == "build" else 0
        options["stderr"].write(b"synthetic child stderr\n")
        return SimpleNamespace(pid=12345, wait=lambda: code, poll=lambda: code)

    def build(self):
        """Invoke the real wrapper with source/child authentication explicitly modeled."""
        return native.build_lightning_native(self.source, self.work, self.tool_file)

    def test_direct_cargo_native_and_every_emitted_artifact_are_bound(self):
        result = self.build()
        self.assertTrue(result["completed"])
        self.assertEqual(len(self.calls), 3)
        self.assertTrue(result["node_copy_matches_cargo_artifact"])
        self.assertEqual(len(result["artifact_evidence"]["bindings"]), 3)
        self.assertEqual(Path(result["node_artifact"]["path"]).read_bytes(), b"synthetic compiled native bytes\x00\xff")
        self.assertTrue(all(result[key] is False for key in native.FALSE_CLAIMS))
        self.assertFalse(any("yarn" in part or "build.js" in part for command in self.calls for part in command))
        self.assertTrue((self.work / "native-build.json").exists())

    def test_nonzero_child_retains_raw_logs_and_never_publishes_node(self):
        self.behavior = "nonzero"
        with self.assertRaisesRegex(native.NativeBuildError, "child failed"):
            self.build()
        receipt = json.loads((self.work / "native-build.json").read_bytes())
        self.assertEqual(receipt["processes"][-1]["exit_code"], 31)
        self.assertTrue(receipt["processes"][-1]["writer_closed"])
        self.assertFalse(receipt["completed"])
        self.assertFalse((self.work / native.TARGETS[self.target][1]).exists())

    def test_artifact_and_source_adverse_cases_cannot_qualify(self):
        cases = ["no-native", "duplicate-native", "false-finished", "debug-native", "fresh-native",
                 "wrong-manifest", "escape-artifact", "escape-dependency", "malformed-events",
                 "source-mutation", "source-addition", "tool-mutation", "registry-cache", "registry-junk"]
        for behavior in cases:
            with self.subTest(behavior=behavior):
                self.behavior = behavior
                previous = self.work
                self.work = self.root / behavior
                self.work.mkdir()
                old_tool = Path(self.toolchain["rustc"]["path"]).read_bytes()
                try:
                    with self.assertRaises((native.NativeBuildError, ValueError)):
                        self.build()
                    self.assertFalse(json.loads((self.work / "native-build.json").read_bytes())["completed"])
                finally:
                    Path(self.toolchain["rustc"]["path"]).write_bytes(old_tool)
                    self.work = previous

    def test_host_mismatch_stops_before_native_build(self):
        self.behavior = "wrong-host"
        with self.assertRaisesRegex(native.NativeBuildError, "cross-compilation"):
            self.build()
        self.assertEqual(len(self.calls), 2)

    def test_reused_work_is_rejected_before_child(self):
        (self.work / "old").write_bytes(b"old output")
        with self.assertRaisesRegex(native.NativeBuildError, "fresh external"):
            self.build()
        self.assertEqual(self.calls, [])

    def test_changed_declared_tool_is_rejected_before_child(self):
        Path(self.toolchain["cargo"]["path"]).write_bytes(b"unreviewed replacement")
        with self.assertRaisesRegex(native.NativeBuildError, "changed tool"):
            self.build()
        self.assertEqual(self.calls, [])

    def test_toolchain_cannot_inject_ambient_cargo_environment(self):
        self.toolchain["environment"]["RUSTC_WRAPPER"] = "unreviewed wrapper"
        self.tool_file.write_text(json.dumps(self.toolchain), encoding="utf-8")
        with self.assertRaisesRegex(native.NativeBuildError, "compiler environment"):
            self.build()
        self.assertEqual(self.calls, [])

    def test_unknown_spawn_preserves_unknown_state_and_stops_dispatch(self):
        with mock.patch.object(native.subprocess, "Popen", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.build()
        receipt = json.loads((self.work / "native-build.json").read_bytes())
        self.assertEqual(len(receipt["processes"]), 1)
        self.assertTrue(receipt["processes"][0]["spawn_attempted"])
        self.assertFalse(receipt["processes"][0]["writer_closed"])
        self.assertFalse(receipt["completed"])

    def test_interrupted_live_child_is_never_killed_or_claimed_closed(self):
        child = SimpleNamespace(pid=6677, wait=mock.Mock(side_effect=KeyboardInterrupt), poll=lambda: None)
        with mock.patch.object(native.subprocess, "Popen", return_value=child):
            with self.assertRaises(KeyboardInterrupt):
                self.build()
        receipt = json.loads((self.work / "native-build.json").read_bytes())
        self.assertEqual(receipt["processes"][0]["pid"], 6677)
        self.assertFalse(receipt["processes"][0]["writer_closed"])
        self.assertEqual(child.wait.call_count, 1)

    def test_receipt_io_failure_cannot_replace_interruption(self):
        with mock.patch.object(native.subprocess, "Popen", side_effect=KeyboardInterrupt), \
                mock.patch.object(native, "_save", side_effect=OSError("modeled receipt write failure")):
            with self.assertRaises(KeyboardInterrupt) as caught:
                self.build()
        self.assertTrue(any("receipt reporting" in note for note in caught.exception.__notes__))

    def test_closed_child_log_failure_cannot_replace_interruption(self):
        child = SimpleNamespace(pid=6677, wait=mock.Mock(side_effect=KeyboardInterrupt), poll=lambda: 130)
        records = []
        with mock.patch.object(native.subprocess, "Popen", return_value=child), \
                mock.patch.object(native, "_record", side_effect=OSError("modeled log hash failure")):
            with self.assertRaises(KeyboardInterrupt):
                native._run(["synthetic"], self.root, {}, self.work, "interrupt", records)
        self.assertTrue(records[0]["writer_closed"])
        self.assertEqual(records[0]["reporting_error"], "OSError")

    def test_ambient_ancestor_configuration_is_rejected(self):
        config = self.root / ".cargo/config.toml"
        config.parent.mkdir()
        config.write_bytes(b"[build]\nrustc-wrapper = 'unexpected'\n")
        with self.assertRaisesRegex(native.NativeBuildError, "ancestor Cargo configuration"):
            ANCESTOR_CHECK(self.work)

    def test_mixed_helper_origin_is_rejected_before_authentication(self):
        with self.assertRaisesRegex(native.NativeBuildError, "outside immutable source root"):
            HELPER_CHECK(self.source)

    @contextmanager
    def _real_helper_source_copies(self):
        helper_root = Path(native.install.__file__).resolve().parent
        modules = {name: module for name, module in list(sys.modules.items())
                   if getattr(module, "__file__", None)
                   and Path(module.__file__).resolve().parent == helper_root}
        self.assertIn("patch_npm_source_fixture", modules)
        copies = self.source / "scripts"
        copies.mkdir()
        for name, module in modules.items():
            (copies / (name + ".py")).write_bytes(Path(module.__file__).read_bytes())
        with ExitStack() as stack:
            for name, module in modules.items():
                stack.enter_context(mock.patch.object(module, "__file__", str(copies / (name + ".py"))))
            stack.enter_context(mock.patch.object(native, "_helper_inputs", HELPER_CHECK))
            yield modules

    def test_transitive_helper_foreign_origin_is_rejected_before_authentication(self):
        with self._real_helper_source_copies() as modules:
            foreign = self.root / "foreign-patch-helper.py"
            foreign.write_bytes((self.source / "scripts/patch_npm_source_fixture.py").read_bytes())
            with mock.patch.object(modules["patch_npm_source_fixture"], "__file__", str(foreign)):
                with self.assertRaisesRegex(native.NativeBuildError, "outside immutable source root"):
                    self.build()
        self.assertEqual(self.calls, [])
        native._authenticate.assert_not_called()

    def test_transitive_helper_byte_change_stops_before_artifact_admission(self):
        self.behavior = "helper-mutation"
        with self._real_helper_source_copies():
            with self.assertRaisesRegex(native.NativeBuildError, "compiler/helper input changed"):
                self.build()
        receipt = json.loads((self.work / "native-build.json").read_bytes())
        self.assertFalse(receipt["completed"])
        self.assertTrue(receipt["processes"][-1]["writer_closed"])
        self.assertNotIn("artifact_evidence", receipt)
        self.assertFalse((self.work / native.TARGETS[self.target][1]).exists())

    def test_source_authentication_change_stops_before_build(self):
        with mock.patch.object(native, "_authenticate", side_effect=[
                (self.security, self.cargo), ({**self.security, "changed": True}, self.cargo)]):
            with self.assertRaisesRegex(native.NativeBuildError, "authenticated source"):
                self.build()
        self.assertEqual(len(self.calls), 2)

    def test_only_exact_cargo_administrative_registry_marker_is_permitted(self):
        home = self.root / "cache-only"
        registry = home / "registry"
        registry.mkdir(parents=True)
        marker = registry / "CACHEDIR.TAG"
        marker.write_bytes(b"Signature: 8a477f597d28d172789f06886806bc55\n"
                           b"# This file is a cache directory tag created by cargo.\n"
                           b"# For information about cache directory tags see https://bford.info/cachedir/\n")
        self.assertEqual(len(native._cache_inventory(home)), 1)
        marker.write_bytes(b"forged marker")
        with self.assertRaisesRegex(native.NativeBuildError, "registry cache"):
            native._cache_inventory(home)


class FiniteChildTests(unittest.TestCase):
    """Check log/exit handling on finite Python processes, never a native build."""

    def test_real_finite_child_success_and_failure(self):
        with tempfile.TemporaryDirectory(prefix="rootlight-native-child-") as directory:
            root = Path(directory)
            for code in (0, 17):
                processes = []
                command = [sys.executable, "-c", "import sys; print('finite'); print('stderr',file=sys.stderr); sys.exit(" + str(code) + ")"]
                if code:
                    with self.assertRaisesRegex(native.NativeBuildError, "child failed"):
                        native._run(command, root, dict(os.environ), root, str(code), processes)
                else:
                    native._run(command, root, dict(os.environ), root, str(code), processes)
                self.assertEqual(processes[0]["exit_code"], code)
                self.assertTrue(processes[0]["writer_closed"])
                self.assertEqual(Path(processes[0]["stdout"]["path"]).read_text().strip(), "finite")


class FixtureLayoutTests(unittest.TestCase):
    """Keep temporary ownership independent of a private or tracked source path."""

    def test_private_and_promoted_layouts_write_only_owned_temporary_files(self):
        source = Path(__file__).read_text(encoding="utf-8")
        temporary_directory = tempfile.TemporaryDirectory
        with temporary_directory(prefix="rootlight-native-layout-") as temporary:
            root = Path(temporary)
            owned = root / "owned-temporary"
            owned.mkdir()
            repository = root / "repository"
            layouts = (
                repository / "target/diagnostics/private/source/scripts/test_build_lightning_native.py",
                repository / "scripts/test_build_lightning_native.py",
            )
            for location in layouts:
                location.parent.mkdir(parents=True, exist_ok=True)
                location.write_text(source, encoding="utf-8")
            baseline = set(repository.rglob("*"))

            def owned_directory(*arguments, **keywords):
                self.assertNotIn("dir", keywords)
                self.assertLessEqual(len(arguments), 2)
                return temporary_directory(*arguments, **keywords, dir=owned)

            for location in layouts:
                with self.subTest(layout=location.relative_to(repository).as_posix()):
                    namespace = {"__file__": str(location), "__name__": "native_fixture_layout"}
                    exec(compile(source, str(location), "exec"), namespace)
                    with mock.patch.object(tempfile, "TemporaryDirectory", side_effect=owned_directory):
                        case = namespace["NativeBuildTests"]("test_direct_cargo_native_and_every_emitted_artifact_are_bound")
                        try:
                            case.setUp()
                            self.assertTrue(case.root.is_relative_to(owned))
                            case.test_direct_cargo_native_and_every_emitted_artifact_are_bound()
                        finally:
                            case.doCleanups()
                        finite = namespace["FiniteChildTests"]("test_real_finite_child_success_and_failure")
                        finite.test_real_finite_child_success_and_failure()
                    self.assertEqual(list(owned.iterdir()), [])
                    self.assertEqual(set(repository.rglob("*")), baseline)


if __name__ == "__main__":
    unittest.main()
