#!/usr/bin/env python3
"""Exercise the Lightning Yarn boundary using authored archives and fake children.

The producer association and subprocess are mocked; Yarn archive verification is
real. Host-ancestor manifest absence is modeled, not an actual installation,
filesystem qualification, build or OS network-denial proof.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import install_lightning_from_archives as install
from test_verify_lightning_yarn_sources import archive


class LightningArchiveInstallationTests(unittest.TestCase):
    """Require authenticated staging and exact local-only child invocation policy."""

    def setUp(self) -> None:
        """Create synthetic inputs and model absent manifests above the owned fixture root."""
        temporary = tempfile.TemporaryDirectory()
        security_verifier = mock.patch.object(install.security, "verify_bundle", side_effect=lambda project: install.yarn.verify_bundle(project))
        security_verifier.start()
        self.addCleanup(security_verifier.stop)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        host_manifests = {ancestor / name for ancestor in self.root.parents
                          for name in ("package.json", "yarn.json")}
        actual_lstat = Path.lstat

        def fixture_lstat(path):
            if path in host_manifests:
                raise FileNotFoundError("modeled host-ancestor manifest absence")
            return actual_lstat(path)

        patcher = mock.patch.object(Path, "lstat", fixture_lstat)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.source = self.root / "immutable sources"
        self.tree = self.source / install.yarn.PRODUCER_PATH / "tree"
        self.tree.mkdir(parents=True)
        self.bundle = self.source / install.yarn.BUNDLE_PATH
        (self.bundle / "archives").mkdir(parents=True)
        (self.bundle / "packages").mkdir()
        self.work, self.cache = self.root / "fresh work", self.root / "fresh cache"
        self.work.mkdir()
        self.cache.mkdir()
        self.node = self.root / ("node.exe" if os.name == "nt" else "node")
        self.node.write_bytes(b"synthetic declared Node; never executed")
        self.node_sha256 = hashlib.sha256(self.node.read_bytes()).hexdigest()
        self.cli = self.root / "declared Yarn" / "lib" / "cli.js"
        self.cli.parent.mkdir(parents=True)
        self.cli.write_bytes(b"synthetic declared Yarn; never executed")
        self.tool_package = self.cli.parent.parent / "package.json"
        self.tool_package.write_bytes(json.dumps({"name": "yarn", "version": install.YARN_VERSION}).encode())
        for name, value in (("YARN_CLI_SHA256", self.cli.read_bytes()),
                            ("YARN_PACKAGE_SHA256", self.tool_package.read_bytes())):
            patcher = mock.patch.object(install, name, hashlib.sha256(value).hexdigest())
            patcher.start()
            self.addCleanup(patcher.stop)
        package = {"name": "fixture-producer", "version": "1.0.0",
                   "dependencies": {"@scope/leaf": "^1.0.0"}, "devDependencies": {"dev": "^1.0.0"},
                   "resolutions": {"fixture-producer": "link:."},
                   "scripts": {"prepare": "node scripts/must-not-run.js"}}
        (self.tree / "package.json").write_bytes((json.dumps(package, indent=2) + "\r\n").encode())
        (self.tree / "scripts").mkdir()
        (self.tree / "scripts/must-not-run.js").write_bytes(b"throw Error('lifecycle executed');\r\n")
        (self.tree / "scripts/must-not-run.js").chmod(0o755)
        (self.tree / ".yarnrc").write_bytes(b'yarn-path "outside.js"\n')
        self.archives, self.mirror_names = {}, {}
        sections = ["# yarn lockfile v1\r\n"]
        for name in ("@scope/leaf", "dev"):
            published = json.dumps({"name": name, "version": "1.0.0", "license": "MIT",
                                    "scripts": {"postinstall": "node must-not-run.js"}}).encode()
            payload, members = archive(name, extras={"authored root/package.json": (published, 0o644)})
            basename = name.rsplit("/", 1)[-1] + "-1.0.0.tgz"
            resolved = f"https://registry.yarnpkg.com/{name}/-/{basename}#{hashlib.sha1(payload).hexdigest()}"
            integrity = "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode()
            sections.append(f'{json.dumps(name + "@^1.0.0")}:\r\n  version "1.0.0"\r\n'
                            f'  resolved "{resolved}"\r\n  integrity {integrity}\r\n\r\n')
            filename = hashlib.sha256(resolved.encode()).hexdigest() + ".tgz"
            path = self.bundle / "archives" / filename
            path.write_bytes(payload)
            path.chmod(0o644)
            self.archives[name] = path
            self.mirror_names[name] = "@scope-" + basename if name.startswith("@") else basename
            target = self.bundle / "packages" / Path(filename).stem
            (target / "authored root/nested/empty").mkdir(parents=True)
            for relative, (value, mode) in members.items():
                destination = target / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(value)
                destination.chmod(mode)
        sections.append('"fixture-producer@link:.":\r\n  version "0.0.0"\r\n  uid ""\r\n')
        (self.tree / "yarn.lock").write_bytes("".join(sections).encode())
        members = [{"path": install.yarn.producer.ROOT, "kind": "directory", "archive_mode": 0o755}]
        for path in sorted(self.tree.rglob("*")):
            relative = path.relative_to(self.tree).as_posix()
            members.append({"path": install.yarn.producer.ROOT + "/" + relative,
                            "kind": "directory" if path.is_dir() else "file", "archive_mode": path.stat().st_mode & 0o777,
                            **({"sha256": hashlib.sha256(path.read_bytes()).hexdigest()} if path.is_file() else {})})
        self.source_report = self.tree.parent / "source-tree.json"
        self.source_report.write_bytes(install.yarn.producer.json_bytes({"source_inventory": {"members": members}}))
        npm = self.source / "source-inputs/npm"
        npm.mkdir()
        (npm / "npm-source-closure.json").write_bytes(b"synthetic separately mocked npm association\n")
        canonical_lock = self.source / install.yarn.producer.local.CANONICAL_LOCK
        canonical_lock.parent.mkdir(parents=True)
        canonical_lock.write_bytes(b"synthetic separately mocked canonical lock\n")
        self.association = {"association": {"name": "fixture-producer", "version": "1.0.0"}}
        patcher = mock.patch.object(install.yarn, "_inputs", side_effect=self.inputs)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.evidence = install.yarn.derive_bundle(self.source, self.bundle)
        (self.bundle / install.yarn.REPORT_NAME).write_bytes(install.yarn.producer.json_bytes(self.evidence))
        (self.bundle / install.yarn.REPORT_NAME).chmod(0o644)
        self.calls = []

    def inputs(self, project: Path):
        """Mock producer authentication while retaining actual guarded input reads."""
        self.assertEqual(project, self.source)
        return (self.association, install._read(self.tree / "package.json"),
                install._read(self.tree / "yarn.lock"), install._read(self.source_report))

    def process(self, arguments, **options):
        """Record exact arguments and simulate versions plus cache population."""
        self.calls.append((arguments, options))
        if arguments == [str(self.node), "--version"]:
            options["stdout"].write((install.NODE_VERSION + "\n").encode())
            return SimpleNamespace(returncode=0)
        if arguments[:3] == [str(self.node), str(self.cli), "--version"]:
            self.assertIn("--no-default-rc", arguments)
            options["stdout"].write((install.YARN_VERSION + "\n").encode())
            return SimpleNamespace(returncode=0)
        self.assertEqual(arguments[:3], [str(self.node), str(self.cli), "install"])
        self.assertEqual(list(self.cache.iterdir()), [])
        (self.cache / "v6").mkdir()
        options["stdout"].write(b"synthetic completed install\n")
        return SimpleNamespace(returncode=0)

    def invoke(self, *, process=None, **paths):
        """Invoke the real boundary with every child replaced by a fake result."""
        arguments = {"source_root": self.source, "work": self.work, "cache": self.cache,
                     "node": self.node, "yarn_cli": self.cli, "node_sha256": self.node_sha256, **paths}
        with mock.patch.object(install.subprocess, "run", side_effect=process or self.process):
            return install.install_lightning(**arguments)

    def rejected_before_process(self, message=".*", **paths):
        """Require a validation failure before any executable receives control."""
        with mock.patch.object(install.subprocess, "run") as runner:
            with self.assertRaisesRegex((ValueError, OSError), message):
                install.install_lightning(self.source, paths.get("work", self.work), paths.get("cache", self.cache),
                                         paths.get("node", self.node), paths.get("yarn_cli", self.cli),
                                         paths.get("node_sha256", self.node_sha256))
            runner.assert_not_called()

    def test_original_bytes_mirror_graph_and_controlled_process_policy(self) -> None:
        """Mirror copies preserve authenticated archives and unchanged local selectors."""
        before = {path: path.read_bytes() for path in self.source.rglob("*") if path.is_file()}
        with mock.patch.dict(os.environ, {"NODE_OPTIONS": "--require outside.js", "NPM_CONFIG_USERCONFIG": "outside",
                                         "HTTP_PROXY": "outside", "NODE_ENV": "production"}):
            report = self.invoke()
        producer = self.work / install.PRODUCER_DIRECTORY
        for path in self.tree.rglob("*"):
            if path.is_file():
                self.assertEqual((producer / path.relative_to(self.tree)).read_bytes(), path.read_bytes())
        for name, original in self.archives.items():
            self.assertEqual((self.work / install.MIRROR_DIRECTORY / self.mirror_names[name]).read_bytes(), original.read_bytes())
        self.assertEqual(before, {path: path.read_bytes() for path in before})
        self.assertEqual(len(self.calls), 3)
        arguments, options = self.calls[-1]
        for flag in ("--offline", "--ignore-scripts", "--frozen-lockfile", "--production=false", "--no-default-rc"):
            self.assertIn(flag, arguments)
        for forbidden in ("--prefer-offline", "--ignore-engines", "--ignore-platform", "--update-checksums", "--ignore-optional"):
            self.assertNotIn(forbidden, arguments)
        self.assertEqual(Path(arguments[arguments.index("--cache-folder") + 1]), self.cache)
        config = Path(arguments[arguments.index("--use-yarnrc") + 1])
        self.assertIn(str(self.work / install.MIRROR_DIRECTORY), json.loads(config.read_text().splitlines()[0].split(" ", 1)[1]))
        self.assertEqual(options["cwd"], producer)
        self.assertFalse(options["shell"])
        self.assertEqual(options["env"]["YARN_IGNORE_PATH"], "1")
        self.assertEqual(options["env"]["DISABLE_V8_COMPILE_CACHE"], "1")
        self.assertNotIn("NODE_OPTIONS", options["env"])
        self.assertNotIn("NPM_CONFIG_USERCONFIG", options["env"])
        self.assertNotIn("HTTP_PROXY", options["env"])
        self.assertEqual(report["archive_count"], 2)
        self.assertEqual(report["node_sha256"], self.node_sha256)
        self.assertEqual(report["local_root_associations"][0]["resolution"], "link:.")
        self.assertIn(b'"fixture-producer@link:."', (producer / "yarn.lock").read_bytes())
        self.assertFalse((producer / "lifecycle-sentinel").exists())
        self.assertTrue(report["yarn_install_completed"])
        self.assertTrue(report["strict_source_reverification_completed"])
        self.assertEqual(len(report["child_logs"]), 6)
        for record in report["child_logs"]:
            log = self.work / record["path"]
            self.assertEqual(hashlib.sha256(log.read_bytes()).hexdigest(), record["sha256"])
        self.assertTrue(all(options["stdout"].closed and options["stderr"].closed for _, options in self.calls))
        for field in ("lock_rewritten", "lifecycle_scripts_enabled", "installation_verified", "installed_tree_identity_verified",
                      "preferred_source_closure_complete", "os_denied_acceptance_verified", "source_to_binary_rebuild_verified",
                      "build_and_packaging_acceptance_verified"):
            self.assertFalse(report[field])

    def test_strict_authentication_runs_only_at_entry_and_final_qualification(self) -> None:
        """Trivial version probes do not repeatedly decompress the source corpus."""
        with mock.patch.object(install.yarn, "verify_bundle", wraps=install.yarn.verify_bundle) as verifier:
            self.invoke()
            self.assertEqual(verifier.call_count, 2)

    def test_reuse_overlap_and_parent_traversal_reject_without_cleanup(self) -> None:
        """No caller-owned output can be emptied or silently consumed."""
        for directory in (self.work, self.cache):
            sentinel = directory / "retain-this"
            sentinel.write_bytes(b"preserve existing output")
            self.rejected_before_process("initially be empty")
            self.assertEqual(sentinel.read_bytes(), b"preserve existing output")
            sentinel.unlink()
        inside = self.source / "empty output"
        inside.mkdir()
        self.rejected_before_process("external", work=inside)
        self.rejected_before_process("separate", cache=self.work)
        self.rejected_before_process("parent traversal", work=self.work / ".." / self.work.name)
        self.invoke()
        self.rejected_before_process("initially be empty")

    def test_linked_and_reparse_outputs_are_rejected(self) -> None:
        """Directory aliases cannot redirect mirror or cache writes."""
        actual = Path.lstat

        def metadata(path):
            value = actual(path)
            if path == self.work:
                return SimpleNamespace(st_mode=value.st_mode, st_file_attributes=0x400)
            return value

        with mock.patch.object(install.yarn.producer.local.original.stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400, create=True), \
                mock.patch.object(Path, "lstat", metadata):
            self.rejected_before_process("linked|reparse|symlink")

    def test_tool_pins_and_direct_entrypoint_are_required(self) -> None:
        """Neither a shim, alternate CLI nor changed tool metadata can run."""
        self.cli.write_bytes(b"different CLI bytes")
        self.rejected_before_process("pinned Yarn")
        self.cli.write_bytes(b"synthetic declared Yarn; never executed")
        self.tool_package.write_bytes(b'{"name":"yarn","version":"1.22.21"}')
        self.rejected_before_process("pinned Yarn")
        shim = self.root / "yarn.cmd"
        shim.write_bytes(b"synthetic acquisition-capable shim")
        self.rejected_before_process("direct Yarn", yarn_cli=shim)
        self.rejected_before_process("Node executable", node=shim)

    def test_caller_reviewed_node_hash_is_required_before_any_child(self) -> None:
        """An executable name cannot substitute for an independently selected binary pin."""
        for value in ("0" * 64, self.node_sha256.upper(), "not-a-digest", ""):
            with self.subTest(value=value):
                self.rejected_before_process("Node SHA-256", node_sha256=value)
        self.assertEqual(list(self.work.iterdir()), [])

    def test_missing_corrupt_extra_original_and_missing_selector_reject(self) -> None:
        """Archive and raw-lock verification precedes every executable invocation."""
        path = self.archives["dev"]
        payload = path.read_bytes()
        path.unlink()
        self.rejected_before_process()
        path.write_bytes(payload + b"changed")
        self.rejected_before_process()
        path.write_bytes(payload)
        extra = path.parent / "unreviewed.tgz"
        extra.write_bytes(b"not authenticated")
        self.rejected_before_process()
        extra.unlink()
        (self.tree / "yarn.lock").write_bytes(b"# yarn lockfile v1\n")
        self.rejected_before_process("missing_lock_records")
        self.assertEqual(list(self.work.iterdir()), [])

    def test_mirror_collisions_and_unsafe_names_reject_before_staging(self) -> None:
        """Distinct authenticated URLs cannot alias one filesystem mirror entry."""
        with mock.patch.object(install, "_mirror_filename", return_value="duplicate.tgz"):
            self.rejected_before_process("collision")
        with self.assertRaisesRegex(ValueError, "escaping|unsafe|unsupported"):
            install._mirror_filename({"url_path": "/leaf/-/../escape.tgz"})
        self.assertEqual(list(self.work.iterdir()), [])

    def test_wrong_version_nonzero_and_timeout_have_no_fallback(self) -> None:
        """Failed phases retain their staged outputs and never try another installer."""
        failures = (SimpleNamespace(returncode=0, stdout="wrong version\n"),
                    SimpleNamespace(returncode=9, stdout=""), OSError("unavailable fake tool"),
                    subprocess.TimeoutExpired("fake tool", 300))
        for failure in failures:
            with self.subTest(failure=type(failure).__name__):
                fixture = LightningArchiveInstallationTests()
                fixture.setUp()
                try:
                    def fail(*_arguments, **_options):
                        if isinstance(failure, Exception):
                            raise failure
                        _options["stdout"].write(failure.stdout.encode())
                        return failure

                    with mock.patch.object(install.subprocess, "run", side_effect=fail) as runner:
                        with self.assertRaisesRegex(ValueError, "failed"):
                            install.install_lightning(fixture.source, fixture.work, fixture.cache, fixture.node, fixture.cli,
                                                      fixture.node_sha256)
                        self.assertEqual(runner.call_count, 1)
                    self.assertTrue((fixture.work / install.PRODUCER_DIRECTORY / "yarn.lock").is_file())
                    self.assertEqual(list(fixture.cache.iterdir()), [])
                finally:
                    fixture.doCleanups()

    def test_installation_watchdog_is_separate_and_caller_controlled(self) -> None:
        """Long archive extraction does not inherit the version-probe deadline."""
        report = self.invoke(install_timeout_seconds=1801)
        self.assertEqual([options["timeout"] for _, options in self.calls], [300, 300, 1801])
        self.assertEqual(report["phase_timeouts_seconds"], {"version_probe": 300, "yarn_install": 1801})
        self.assertEqual(install.DEFAULT_INSTALL_TIMEOUT_SECONDS, 1800)

    def test_invalid_installation_watchdog_rejects_before_authentication_or_children(self) -> None:
        """Malformed, infinite and disabled deadlines cannot bypass the boundary."""
        with mock.patch.object(install.yarn, "verify_bundle") as verifier:
            with mock.patch.object(install.subprocess, "run") as runner:
                for value in (0, -1, True, False, None, 1.5, float("inf"), float("nan"), "1800"):
                    with self.subTest(value=value), self.assertRaisesRegex(ValueError, "positive whole number"):
                        install.install_lightning(self.source, self.work, self.cache, self.node, self.cli,
                                                  self.node_sha256, install_timeout_seconds=value)
                verifier.assert_not_called()
                runner.assert_not_called()
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_installation_timeout_retains_closed_logs_and_final_authentication(self) -> None:
        """A timed-out install is a failure, never a fallback or completion claim."""
        def fail(arguments, **options):
            result = self.process(arguments, **options)
            if len(self.calls) == 3:
                raise subprocess.TimeoutExpired(arguments, options["timeout"])
            return result

        with mock.patch.object(install.yarn, "verify_bundle", wraps=install.yarn.verify_bundle) as verifier:
            with self.assertRaisesRegex(ValueError, r"failed: yarn-install \(timeout after 1801 seconds\)"):
                self.invoke(process=fail, install_timeout_seconds=1801)
            self.assertEqual(verifier.call_count, 2)
        self.assertEqual(len(self.calls), 3)
        self.assertTrue(all(options["stdout"].closed and options["stderr"].closed for _, options in self.calls))
        self.assertTrue((self.work / "logs/yarn-install.stdout.log").is_file())
        self.assertTrue((self.work / install.PRODUCER_DIRECTORY / "yarn.lock").is_file())

    def test_nonzero_installer_retains_closed_logs_and_runs_final_authentication(self) -> None:
        """An unsuccessful real installer exit never becomes completion evidence."""
        def fail(arguments, **options):
            result = self.process(arguments, **options)
            if len(self.calls) == 3:
                result.returncode = 7
                options["stderr"].write(b"synthetic installer failure\n")
            return result

        with mock.patch.object(install.yarn, "verify_bundle", wraps=install.yarn.verify_bundle) as verifier:
            with self.assertRaisesRegex(ValueError, "command/version failed: yarn-install"):
                self.invoke(process=fail)
            self.assertEqual(verifier.call_count, 2)
        self.assertEqual(len(self.calls), 3)
        self.assertEqual((self.work / "logs/yarn-install.stderr.log").read_bytes(), b"synthetic installer failure\n")
        self.assertTrue(all(options["stdout"].closed and options["stderr"].closed for _, options in self.calls))

    def test_log_cap_failure_retains_closed_evidence(self) -> None:
        """Oversized child output cannot produce unbounded JSON or a success report."""
        with mock.patch.object(install, "MAX_LOG_BYTES", 1):
            with self.assertRaises((ValueError, OSError)):
                self.invoke()
        self.assertEqual(len(self.calls), 1)
        self.assertEqual((self.work / "logs/node-version.stdout.log").read_bytes(), (install.NODE_VERSION + "\n").encode())
        self.assertTrue(self.calls[0][1]["stdout"].closed)
        self.assertTrue(self.calls[0][1]["stderr"].closed)

    def test_input_mirror_tool_config_and_inventory_drift_stop_each_phase(self) -> None:
        """A zero exit cannot rescue any changed authored input or future child policy."""
        for phase in range(3):
            for subject in ("source", "npm-association", "canonical-lock", "staged-lock", "mirror", "tool", "config", "extra-mirror"):
                with self.subTest(phase=phase, subject=subject):
                    fixture = LightningArchiveInstallationTests()
                    fixture.setUp()
                    try:
                        def mutate(arguments, **options):
                            result = fixture.process(arguments, **options)
                            if len(fixture.calls) == phase + 1:
                                paths = {"source": fixture.tree / "package.json",
                                         "npm-association": fixture.source / "source-inputs/npm/npm-source-closure.json",
                                         "canonical-lock": fixture.source / install.yarn.producer.local.CANONICAL_LOCK,
                                         "staged-lock": fixture.work / install.PRODUCER_DIRECTORY / "yarn.lock",
                                         "mirror": fixture.work / install.MIRROR_DIRECTORY / fixture.mirror_names["dev"],
                                         "tool": fixture.cli, "config": fixture.work / "controlled.yarnrc",
                                         "extra-mirror": fixture.work / install.MIRROR_DIRECTORY / "unreviewed.tgz"}
                                with paths[subject].open("ab") as stream:
                                    stream.write(b"changed")
                            return result

                        with self.assertRaises((ValueError, OSError)):
                            fixture.invoke(process=mutate)
                        self.assertEqual(len(fixture.calls), phase + 1)
                    finally:
                        fixture.doCleanups()

    def test_cache_population_by_version_probe_is_rejected_before_install(self) -> None:
        """A cache filled by an earlier child is not a genuinely fresh installation."""
        def populate(arguments, **options):
            result = self.process(arguments, **options)
            if len(self.calls) == 1:
                (self.cache / "prefetched").write_bytes(b"not an empty cache")
            return result

        with self.assertRaisesRegex(ValueError, "not empty immediately before"):
            self.invoke(process=populate)
        self.assertEqual(len(self.calls), 2)

    def test_output_ancestor_replacement_is_detected(self) -> None:
        """Stable paths cannot be replaced by fresh directories between child phases."""
        def replace(arguments, **options):
            result = self.process(arguments, **options)
            self.cache.rename(self.root / "retained cache")
            self.cache.mkdir()
            return result

        with self.assertRaisesRegex(ValueError, "ancestor changed"):
            self.invoke(process=replace)
        self.assertEqual(len(self.calls), 1)

    def test_nonempty_lock_missing_authored_root_selector_rejects(self) -> None:
        """A retained stanza does not rescue an omitted manifest dependency selector."""
        path = self.tree / "yarn.lock"
        original = path.read_bytes()
        marker = b'"@scope/leaf@^1.0.0":\r\n'
        start = original.index(marker)
        end = original.index(b"\r\n\r\n", start) + 4
        retained = original[:start] + original[end:]
        self.assertIn(b'"dev@^1.0.0":', retained)
        self.assertNotIn(marker, retained)
        path.write_bytes(retained)
        self.rejected_before_process("missing_manifest_selector")
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_ancestor_workspace_declarations_reject_without_output_touches(self) -> None:
        """Both manifest names are rejected before staging, including at the work root."""
        payload = b'{"name":"neutral-ancestor-workspace","private":true,"workspaces":["*"]}\n'
        for directory in (self.work, self.root):
            for name in ("package.json", "yarn.json"):
                with self.subTest(directory=directory.name, name=name):
                    manifest = directory / name
                    manifest.write_bytes(payload)
                    before_work, before_cache = list(self.work.iterdir()), list(self.cache.iterdir())
                    self.rejected_before_process("ancestor package manifest")
                    self.assertEqual(list(self.work.iterdir()), before_work)
                    self.assertEqual(list(self.cache.iterdir()), before_cache)
                    self.assertEqual(before_work, [manifest] if directory == self.work else [])
                    self.assertEqual(before_cache, [])
                    self.assertEqual(manifest.read_bytes(), payload)
                    manifest.unlink()

    def test_ordinary_manifest_two_levels_above_work_is_conservatively_rejected(self) -> None:
        """Ordinary declarations do not terminate the ancestor scan or authorize Yarn discovery."""
        outer = self.root / "neutral outer"
        work = outer / "neutral inner" / "fresh work"
        work.mkdir(parents=True)
        payload = b'{"name":"neutral-parent","version":"1.0.0"}\n'
        for name in ("package.json", "yarn.json"):
            with self.subTest(name=name):
                manifest = outer / name
                manifest.write_bytes(payload)
                self.rejected_before_process("ancestor package manifest", work=work)
                self.assertEqual(list(work.iterdir()), [])
                self.assertEqual(list(self.cache.iterdir()), [])
                self.assertEqual(manifest.read_bytes(), payload)
                manifest.unlink()

    def test_ancestor_manifest_directory_dangling_link_and_reparse_entries_reject(self) -> None:
        """Manifest presence uses lstat even when exists-following-links reports absence."""
        directory = self.root / "package.json"
        directory.mkdir()
        self.rejected_before_process("ancestor package manifest")
        self.assertTrue(directory.is_dir())
        self.assertEqual(list(directory.iterdir()), [])
        directory.rmdir()
        actual = Path.lstat
        for name, mode, attributes in (("package.json", stat.S_IFLNK | 0o777, 0),
                                       ("yarn.json", stat.S_IFREG | 0o644, 0x400)):
            with self.subTest(name=name, attributes=attributes):
                manifest = self.root / name
                alias = SimpleNamespace(st_mode=mode, st_file_attributes=attributes)

                def metadata(path):
                    return alias if path == manifest else actual(path)

                with mock.patch.object(Path, "lstat", metadata):
                    self.assertFalse(manifest.exists())
                    self.rejected_before_process("ancestor package manifest")
                    self.assertIs(manifest.lstat(), alias)
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_late_ancestor_manifest_creation_stops_before_later_children(self) -> None:
        """Parent declarations cannot redirect a later phase after a successful child."""
        payload = b'{"name":"neutral-late-parent","private":true,"workspaces":["*"]}\n'
        for phase in range(3):
            for name in ("package.json", "yarn.json"):
                with self.subTest(phase=phase, name=name):
                    fixture = LightningArchiveInstallationTests()
                    fixture.setUp()
                    try:
                        manifest = fixture.root / name

                        def declare(arguments, **options):
                            result = fixture.process(arguments, **options)
                            if len(fixture.calls) == phase + 1:
                                manifest.write_bytes(payload)
                            return result

                        with self.assertRaisesRegex(ValueError, "ancestor package manifest"):
                            fixture.invoke(process=declare)
                        self.assertEqual(len(fixture.calls), phase + 1)
                        self.assertEqual(manifest.read_bytes(), payload)
                    finally:
                        fixture.doCleanups()

    def test_ancestor_guard_inspects_every_candidate_through_filesystem_root(self) -> None:
        """The actual guard visits both names at every level, not just modeled fixture ancestors."""
        ancestors = (self.work, *self.work.parents)
        expected = [ancestor / name for ancestor in ancestors for name in ("package.json", "yarn.json")]
        calls = []

        def absent(path):
            calls.append(path)
            raise FileNotFoundError("synthetic absent manifest")

        with mock.patch.object(Path, "lstat", absent), mock.patch.object(install.subprocess, "run") as runner:
            install._reject_ancestor_manifests(self.work)
            runner.assert_not_called()
        self.assertEqual(calls, expected)
        self.assertEqual(len(calls), 2 * len(ancestors))
        self.assertEqual(calls[-2:], [Path(self.work.anchor) / name for name in ("package.json", "yarn.json")])
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_ancestor_guard_propagates_permission_error_before_any_staging_or_child(self) -> None:
        """Only absence is ignored; an inaccessible root candidate must fail admission."""
        expected = [ancestor / name for ancestor in (self.work, *self.work.parents)
                    for name in ("package.json", "yarn.json")]
        candidates, calls = set(expected), []
        denied = expected[-1]
        denial = PermissionError("synthetic ancestor inspection denied")
        actual_lstat = Path.lstat

        def inspect(path):
            if path in candidates:
                calls.append(path)
                if path == denied:
                    raise denial
                raise FileNotFoundError("synthetic absent manifest")
            return actual_lstat(path)

        with mock.patch.object(Path, "lstat", inspect), mock.patch.object(install.subprocess, "run") as runner:
            with self.assertRaises(PermissionError) as direct:
                install._reject_ancestor_manifests(self.work)
            self.assertIs(direct.exception, denial)
            self.assertEqual(calls, expected)
            calls.clear()
            with self.assertRaises(PermissionError) as admission:
                install.install_lightning(self.source, self.work, self.cache, self.node, self.cli, self.node_sha256)
            self.assertIs(admission.exception, denial)
            self.assertEqual(calls, expected)
            runner.assert_not_called()
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual(list(self.cache.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
