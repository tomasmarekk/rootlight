"""Exercise fresh-builder/source joins with tiny local files and mocked children.

No Node, npm, Cargo, native module or product code executes in these controls.
Synthetic receipts are deliberately not treated as evidence of compilation.
"""

from __future__ import annotations

import ast
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parent
import build_frontend_from_sources as build


class SelectionControls(unittest.TestCase):
    """Reject stale, changed or unrelated artifacts before a frontend child."""

    def setUp(self):
        """Create only synthetic native byte copies and tiny source metadata."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "source"
        self.work = self.root / "native"
        self.frontend_work = self.root / "frontend"
        self.cache = self.root / "cache"
        for path in (self.source, self.work, self.frontend_work, self.cache):
            path.mkdir()
        self.node, self.npm = self.root / "node", self.root / "npm-cli.js"
        for path in (self.node, self.npm):
            path.write_bytes(b"synthetic tool; never executed")
        report = self.source / build.frontend.REPORT
        report.parent.mkdir(parents=True)
        report.write_text(json.dumps({"packages": [{"name": "lightningcss-linux-x64-gnu", "version": "1.33.0",
            "lock_paths": ["node_modules/vite/node_modules/lightningcss-linux-x64-gnu"]}]}), encoding="utf-8")
        (self.source / "source-inputs/cargo-source-closure.json").write_bytes(b"synthetic cargo input report")
        guard = self.source / build.frontend.BUILD_GUARD
        guard.parent.mkdir()
        guard.write_bytes(b"// synthetic guard, not executed\n")
        self.runtime = dict(version=build.frontend.NODE_VERSION, platform="linux", arch="x64", napi="10", modules="137")
        self.authenticated = ({"producer": {"source_tree_report": {"sha256": "a" * 64}}},
                              {"lockfiles": [{"path": "synthetic-lock-" + str(i), "sha256": str(i) * 64} for i in range(5)]})
        self.auth = mock.patch.object(build.native, "_authenticate", return_value=self.authenticated)
        self.auth.start()
        self.addCleanup(self.auth.stop)

    def native_result(self):
        """Model the returned in-process builder contract without compiling."""
        target = "x86_64-unknown-linux-gnu"
        relative = Path(target) / "release/liblightningcss_node.so"
        original = self.work / "producer/target" / relative
        retained = self.work / "artifacts" / relative
        node = self.work / "lightningcss.linux-x64-gnu.node"
        for path in (original, retained, node):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"synthetic compiled-byte join; never loaded")
        original_pin, retained_pin, node_pin = map(build.native._record, (original, retained, node))
        event = dict(reason="compiler-artifact", manifest_path=str(self.work / "producer/node/Cargo.toml"),
            target=dict(name="lightningcss_node", kind=["cdylib"], crate_types=["cdylib"]),
            fresh=False, profile=dict(test=False, opt_level="3"), filenames=[str(original)])
        return dict(completed=True, source_and_tools_conserved=True, native_cargo_build_observed=True,
            node_copy_matches_cargo_artifact=True, source_root=str(self.source), target=target,
            processes=[dict(phase=name, writer_closed=True, exit_code=0) for name in
                ("cargo-version", "rustc-version", "cargo-build")], node_artifact=node_pin,
            artifact_evidence=dict(native=dict(original=original_pin, retained=retained_pin), cargo_events=[event]),
            authenticated_source_reports=copy.deepcopy(self.authenticated), effective_sources={"src/lib.rs": {"sha256": "b" * 64}},
            toolchain=dict(cargo=dict(version="cargo synthetic"), rustc=dict(version="rustc synthetic")))

    def capsule(self, result=None, runtime=None):
        """Validate modeled metadata against actual tiny file bytes."""
        return build._capsule(self.source, self.work, result or self.native_result(), runtime or self.runtime)

    def test_canonical_package_and_three_byte_copies_are_bound(self):
        capsule = self.capsule()
        self.assertEqual(capsule["package"]["name"], "lightningcss-linux-x64-gnu")
        self.assertEqual(capsule["runtime"], self.runtime)
        self.assertEqual(capsule["source"]["five_lockfiles_sha256"], build._digest(self.authenticated[1]["lockfiles"]))
        self.assertIn("compilation proof belongs to the fresh in-process builder", capsule["authorization_scope"])
        self.assertNotIn("compiled", capsule)

    def test_changed_selected_native_bytes_reject(self):
        result = self.native_result()
        Path(result["node_artifact"]["path"]).write_bytes(b"changed native bytes")
        with self.assertRaisesRegex(ValueError, "artifact/copy join"):
            self.capsule(result)

    def test_changed_original_and_retained_cargo_artifacts_reject(self):
        for name in ("original", "retained"):
            with self.subTest(name=name):
                result = self.native_result()
                Path(result["artifact_evidence"]["native"][name]["path"]).write_bytes(b"changed compiler output")
                with self.assertRaisesRegex(ValueError, "dependency artifact changed"):
                    self.capsule(result)

    def test_node_copy_from_other_source_root_rejects(self):
        result = self.native_result()
        result["source_root"] = str(self.root / "foreign")
        with self.assertRaisesRegex(ValueError, "this source frame"):
            self.capsule(result)

    def test_false_fresh_or_unclosed_native_builder_rejects(self):
        for field in ("completed", "source_and_tools_conserved", "native_cargo_build_observed", "node_copy_matches_cargo_artifact"):
            with self.subTest(field=field):
                result = self.native_result()
                result[field] = False
                with self.assertRaises(ValueError):
                    self.capsule(result)
        for field, value in (("writer_closed", False), ("exit_code", None), ("exit_code", 7)):
            with self.subTest(field=field, value=value):
                result = self.native_result()
                result["processes"][-1][field] = value
                with self.assertRaisesRegex(ValueError, "successful and closed"):
                    self.capsule(result)

    def test_native_event_cache_hit_wrong_manifest_or_artifact_rejects(self):
        for field, value in (("fresh", True), ("manifest_path", str(self.root / "unrelated/Cargo.toml")),
                             ("filenames", [str(self.root / "other.so")])):
            with self.subTest(field=field):
                result = self.native_result()
                result["artifact_evidence"]["cargo_events"][0][field] = value
                with self.assertRaisesRegex(ValueError, "Cargo event/source"):
                    self.capsule(result)

    def test_copy_paths_outside_this_fresh_work_reject(self):
        result = self.native_result()
        result["artifact_evidence"]["native"]["original"]["path"] = str(self.root / "prebuilt.so")
        with self.assertRaisesRegex(ValueError, "artifact/copy join"):
            self.capsule(result)

    def test_runtime_target_version_and_nonnumeric_abi_reject(self):
        for field, value in (("arch", "arm64"), ("platform", "darwin"), ("version", "v18.19.1"),
                             ("napi", "unknown"), ("modules", 137)):
            with self.subTest(field=field):
                runtime = {**self.runtime, field: value}
                with self.assertRaisesRegex(ValueError, "Node/native target"):
                    self.capsule(runtime=runtime)

    def test_effective_producer_and_each_of_five_locks_reject_change(self):
        result = self.native_result()
        result["authenticated_source_reports"][0]["producer"]["source_tree_report"]["sha256"] = "c" * 64
        with self.assertRaisesRegex(ValueError, "producer/five-lock"):
            self.capsule(result)
        for index in range(5):
            result = self.native_result()
            result["authenticated_source_reports"][1]["lockfiles"][index]["sha256"] = "c" * 64
            with self.subTest(index=index), self.assertRaisesRegex(ValueError, "producer/five-lock"):
                self.capsule(result)

    def test_changed_platform_package_association_rejects(self):
        report = self.source / build.frontend.REPORT
        report.write_text('{"packages":[]}', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "canonical package"):
            self.capsule()

    def test_actual_helper_import_outside_source_root_rejects(self):
        with mock.patch.object(build.native, "_helper_inputs", return_value=[]):
            with self.assertRaisesRegex(ValueError, "helper resolved outside"):
                build._helpers(self.source)

    def test_runner_has_no_elapsed_watchdog_and_transmits_exact_payload(self):
        logs = self.root / "logs"
        logs.mkdir()
        child = mock.Mock(pid=123, returncode=0)
        processes = []
        with mock.patch.object(build.subprocess, "Popen", return_value=child) as spawn:
            build._run([str(self.node), "never-executed.js"], self.source, {"HOME": "fresh"}, logs, "modeled", processes, b"capsule\n")
        self.assertEqual(child.communicate.call_args.kwargs, {"input": b"capsule\n"})
        self.assertEqual(spawn.call_args.kwargs["stdin"], subprocess.PIPE)
        self.assertFalse(spawn.call_args.kwargs["shell"])
        self.assertNotIn("timeout", spawn.call_args.kwargs)
        self.assertTrue(processes[0]["writer_closed"])
        self.assertEqual(processes[0]["exit_code"], 0)

    def test_runner_nonzero_is_retained_and_rejected(self):
        logs = self.root / "logs"
        logs.mkdir()
        processes = []
        child = mock.Mock(pid=123, returncode=23)
        with mock.patch.object(build.subprocess, "Popen", return_value=child):
            with self.assertRaisesRegex(ValueError, "frontend child failed"):
                build._run([str(self.node)], self.source, {}, logs, "failed", processes)
        self.assertEqual(processes[0]["exit_code"], 23)
        self.assertTrue(processes[0]["writer_closed"])
        self.assertIn("stdout", processes[0])

    def test_runner_interruption_preserves_unknown_writer_and_never_kills(self):
        logs = self.root / "logs"
        logs.mkdir()
        processes = []
        child = mock.Mock(pid=123)
        child.communicate.side_effect = KeyboardInterrupt()
        child.poll.return_value = None
        with mock.patch.object(build.subprocess, "Popen", return_value=child):
            with self.assertRaises(KeyboardInterrupt):
                build._run([str(self.node)], self.source, {}, logs, "interrupted", processes)
        self.assertIsNone(processes[0]["exit_code"])
        self.assertFalse(processes[0]["writer_closed"])
        self.assertNotIn("stdout", processes[0])
        child.kill.assert_not_called()
        child.terminate.assert_not_called()

    def test_pipeline_always_calls_builder_and_passes_fresh_capsule_only_to_vite(self):
        def install(*_arguments):
            frontend_path = self.frontend_work / build.frontend.FRONTEND
            frontend_path.mkdir(parents=True)
            (frontend_path / "package.json").write_text(json.dumps({"scripts": {"build": build.AUTHORED_BUILD}}), encoding="utf-8")
            guard = self.frontend_work / build.frontend.BUILD_GUARD
            guard.parent.mkdir()
            guard.write_bytes((self.source / build.frontend.BUILD_GUARD).read_bytes())
            return dict(npm_source_report_sha256=build.native._record(self.source / build.frontend.REPORT)["sha256"])

        calls = []
        def run(command, cwd, environment, logs, name, processes, payload=None):
            calls.append(dict(command=command, environment=environment, phase=name, payload=payload))
            record = dict(phase=name, exit_code=0, writer_closed=True)
            stdout = logs / (name + ".stdout")
            if name == "node-runtime":
                value = self.runtime
            elif name == "vite":
                capsule = json.loads(payload)
                value = dict(schema="rootlight.lightning-frontend-boundary/2", transform_calls=2,
                    derived_selection_observed=True, selection_capsule_sha256=hashlib.sha256(payload).hexdigest(),
                    native_artifacts=[dict(selected_native=capsule["artifact"])])
            else:
                value = {}
            stdout.write_text(json.dumps(value) + "\n", encoding="utf-8")
            record["stdout"] = build.native._record(stdout)
            processes.append(record)
            return record

        with mock.patch.object(build.frontend, "install_frontend", side_effect=install) as installer, \
             mock.patch.object(build.native, "build_lightning_native", side_effect=lambda *_args: self.native_result()) as builder, \
             mock.patch.object(build, "_helpers", return_value=[]), mock.patch.object(build, "_run", side_effect=run):
            result = build.build_frontend_from_sources(self.source, self.frontend_work, self.work, self.cache,
                                                      self.root / "declared-toolchain.json", self.node, self.npm)
        installer.assert_called_once()
        builder.assert_called_once_with(self.source, self.work, self.root / "declared-toolchain.json")
        self.assertEqual([row["phase"] for row in calls], ["node-runtime", "typescript", "vite", "asset-manifest"])
        self.assertEqual([row["phase"] for row in calls if row["payload"] is not None], ["vite"])
        self.assertEqual(calls[2]["environment"][build.SELECTION_ENV], "1")
        self.assertNotIn(build.SELECTION_ENV, calls[1]["environment"])
        self.assertTrue(result["fresh_in_process_native_build_observed"])
        self.assertTrue(result["authored_build_stages_completed"])
        self.assertTrue(all(result[field] is False for field in build.FALSE_CLAIMS))
        self.assertEqual(json.loads((self.frontend_work / "source-build.json").read_bytes())["completed"], True)

    def test_public_api_accepts_no_artifact_or_persisted_compilation_receipt(self):
        tree = ast.parse(Path(build.__file__).read_bytes())
        function = next(item for item in tree.body if isinstance(item, ast.FunctionDef) and item.name == "build_frontend_from_sources")
        self.assertEqual([item.arg for item in function.args.args],
            ["source_root", "frontend_work", "native_work", "cache", "toolchain", "node", "npm_cli"])
        self.assertIsNone(function.args.kwarg)

    def test_original_archive_member_and_public_wrapper_guards_remain_mandatory(self):
        candidate = (HERE / "check_lightning_frontend_boundary.cjs").read_text(encoding="utf-8")
        for block in (
            'if (digest !== publicEntries.get(key)) {\n    throw new Error("Lightning CSS JavaScript interface differs from its authenticated archive member");\n  }',
            'if (!member || digest !== member.sha256) {\n    throw new Error("Lightning CSS native artifact differs from its authenticated archive member");\n  }',
        ):
            self.assertIn(block, candidate)
        self.assertLess(candidate.index('if (!member || digest !== member.sha256)'),
                        candidate.index('loadNative(module, selection.value.artifact.path)'))
        self.assertIn("source_to_binary_rebuild_verified: false", candidate)


if __name__ == "__main__":
    unittest.main(verbosity=2)
