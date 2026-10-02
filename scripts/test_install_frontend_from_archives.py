#!/usr/bin/env python3
"""Exercise local frontend staging and npm invocation with synthetic archives.

Every subprocess is mocked. These tests prove argument/filesystem boundaries,
not actual npm behavior, kernel network denial or frontend source completeness.
"""

from __future__ import annotations

import base64
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import install_frontend_from_archives as install
from patch_npm_source_fixture import patch_fixture_archive
from verify_npm_source_bundle import archive_filename
from verify_local_npm_sources import verify_local_bundle


class FrontendArchiveInstallationTests(unittest.TestCase):
    """Require canonical inputs and local-only invocations without running npm."""

    def setUp(self) -> None:
        """Create independently hashed exported inputs and fresh external outputs."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "immutable sources"
        self.frontend = self.source / install.FRONTEND
        self.frontend.mkdir(parents=True)
        self.archives = self.source / install.ARCHIVES
        self.archives.mkdir(parents=True)
        self.local_distributions = self.source / install.LOCAL_DISTRIBUTIONS
        self.local_distributions.mkdir()
        self.variants = []
        self.work, self.cache = self.root / "fresh work", self.root / "fresh cache"
        self.work.mkdir()
        self.cache.mkdir()
        self.staged_frontend = self.work / install.FRONTEND
        self.node, self.npm_cli = self.root / "declared node", self.root / "declared npm.js"
        self.node.write_bytes(b"synthetic declared tool; never executed")
        self.npm_cli.write_bytes(b"synthetic declared CLI; never executed")
        self.package = {
            "name": "fixture-ui", "version": "0.1.0", "dependencies": {"alpha": "1.0.0"},
            "devDependencies": {}, "engines": {"node": "24.11.1", "npm": "11.6.2"},
            "packageManager": "npm@11.6.2",
        }
        self.lock = {
            "name": "fixture-ui", "version": "0.1.0", "lockfileVersion": 3,
            "packages": {"": {key: self.package[key] for key in (
                "name", "version", "dependencies", "devDependencies", "engines")}},
        }
        self.add_archive("alpha", ["node_modules/alpha", "node_modules/alpha/node_modules/alpha"])
        (self.frontend / "package.json").write_text(json.dumps(self.package), encoding="utf-8")
        (self.frontend / ".npmrc").write_bytes(b"engine-strict=true\nfund=false\npackage-lock=true\n")
        (self.frontend / "src").mkdir()
        (self.frontend / "src/main.ts").write_bytes(b"export const value = 1;\n")
        self.freeze()
        self.calls = []

    def add_archive(self, name: str, placements: list[str]) -> None:
        """Retain one original archive for every supplied synthetic lock placement."""
        resolved = f"https://registry.npmjs.org/{name}/-/{name}-1.0.0.tgz"
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            for filename, value in {
                "authored-root/package.json": json.dumps({
                    "name": name, "version": "1.0.0", "license": "MIT",
                    "scripts": {"install": "node should-not-run.js"},
                }).encode(),
                "authored-root/index.js": b"module.exports = 1;\n",
                "authored-root/LICENSE": b"synthetic retained notice\n",
                "authored-root/should-not-run.js": b"throw Error('install hook executed');\n",
                "authored-root/tests/fixture.test.js": b"const fixture = 'authored-fixture-value';\n",
            }.items():
                member = tarfile.TarInfo(filename)
                member.size, member.mode = len(value), 0o644
                archive.addfile(member, io.BytesIO(value))
        payload = output.getvalue()
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii")
        (self.archives / archive_filename(resolved)).write_bytes(payload)
        for index, path in enumerate(placements):
            self.lock["packages"][path] = {
                "version": "1.0.0", "resolved": resolved, "integrity": integrity,
                "license": "MIT", "hasInstallScript": True,
                **({"optional": True, "os": ["other-platform"]} if index else {}),
            }

    def freeze(self) -> None:
        """Derive exact source/report evidence from current synthetic inputs."""
        lock_path = self.frontend / "package-lock.json"
        upstream_bytes = json.dumps(self.lock).encode("utf-8")
        (self.source / install.UPSTREAM_LOCK).write_bytes(upstream_bytes)
        self.canonical_lock = json.loads(upstream_bytes)
        variants = {(item["name"], item["version"]): item for item in self.variants}
        for path, package in self.canonical_lock["packages"].items():
            if not path:
                continue
            name = path.rsplit("node_modules/", 1)[-1]
            variant = variants.get((name, package["version"]))
            archive_path = variant["archive_path"] if variant else install.ARCHIVES + "/" + archive_filename(package["resolved"])
            package["resolved"] = "file:../../../" + archive_path
            if variant:
                package["integrity"] = "sha512-" + base64.b64encode(hashlib.sha512((self.source / archive_path).read_bytes()).digest()).decode("ascii")
        lock_path.write_text(json.dumps(self.canonical_lock), encoding="utf-8")
        (self.source / install.LOCAL_MANIFEST).write_text(json.dumps({
            "schema": "rootlight.npm-local-distributions/1",
            "upstream_lock_sha256": hashlib.sha256(upstream_bytes).hexdigest(),
            "variants": self.variants,
        }), encoding="utf-8")
        evidence = verify_local_bundle(self.source)
        evidence.update(dict.fromkeys(install._FALSE_REPORT_FIELDS, False))
        (self.source / install.REPORT).write_text(json.dumps(evidence, sort_keys=True, indent=2) + "\n", encoding="utf-8", newline="\n")
        records = [{"path": path.relative_to(self.source).as_posix(),
                    "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "executable": False}
                   for path in sorted(self.source.rglob("*")) if path.is_file() and path.name != install.MANIFEST]
        (self.source / install.MANIFEST).write_text(json.dumps({
            "schema": "rootlight.rust-source-bundle/1", "project_sources": records,
        }), encoding="utf-8")

    def add_local_variant(self) -> dict:
        """Create one reviewed synthetic patch without invoking package code."""
        self.add_archive("beta", ["node_modules/beta"])
        self.freeze()
        fixture_bytes = b"const fixture = 'authored-fixture-value';\n"
        removed = b"authored-fixture-value"
        replacement = b"synthetic-fixture-zero"
        publication = self.root / "synthetic patch publication"
        publication.mkdir()
        patch_fixture_archive(
            self.source / install.UPSTREAM_LOCK, self.archives, "beta", "1.0.0",
            "authored-root/tests/fixture.test.js", hashlib.sha256(fixture_bytes).hexdigest(),
            len(removed), hashlib.sha256(removed).hexdigest(), replacement, publication,
            synthetic_replacement_confirmed=True,
        )
        relative = install.LOCAL_DISTRIBUTIONS + "/beta-1.0.0"
        destination = self.source / relative
        destination.mkdir()
        patched = (publication / "patched.tgz").read_bytes()
        provenance = (publication / "patch-provenance.json").read_bytes()
        (destination / "patched.tgz").write_bytes(patched)
        (destination / "patch-provenance.json").write_bytes(provenance)
        variant = {
            "name": "beta", "version": "1.0.0", "archive_path": relative + "/patched.tgz",
            "archive_sha256": hashlib.sha256(patched).hexdigest(),
            "provenance_path": relative + "/patch-provenance.json",
            "provenance_sha256": hashlib.sha256(provenance).hexdigest(),
        }
        self.variants.append(variant)
        (self.archives / archive_filename("https://registry.npmjs.org/beta/-/beta-1.0.0.tgz")).unlink()
        self.freeze()
        return variant

    def process(self, arguments, **options):
        """Capture policy and simulate only successful declared version/install exits."""
        self.calls.append((arguments, options))
        if arguments == [str(self.node), "--version"]:
            return SimpleNamespace(returncode=0, stdout="v24.11.1\n")
        if arguments == [str(self.node), str(self.npm_cli), "--version"]:
            return SimpleNamespace(returncode=0, stdout="11.6.2\n")
        return SimpleNamespace(returncode=0, stdout="")

    def invoke(self, *, process=None, **paths):
        """Run the installer with a mocked subprocess seam and explicit paths."""
        with mock.patch.object(install.subprocess, "run", side_effect=process or self.process) as runner:
            result = install.install_frontend(
                paths.get("source", self.source), paths.get("work", self.work),
                paths.get("cache", self.cache), self.node, self.npm_cli,
            )
        return result, runner

    def rejected_before_process(self, pattern: str, **paths) -> None:
        """Require rejection before staging, cache population or subprocess launch."""
        with mock.patch.object(install.subprocess, "run") as runner:
            with self.assertRaisesRegex((ValueError, OSError), pattern):
                install.install_frontend(paths.get("source", self.source), paths.get("work", self.work),
                                         paths.get("cache", self.cache), self.node, self.npm_cli)
            runner.assert_not_called()

    def test_shared_archive_preserves_placements_optional_inventory_and_canonical_bytes(self) -> None:
        """Two locked placements retain one archive and unchanged canonical sources."""
        before = {path: path.read_bytes() for path in self.source.rglob("*") if path.is_file()}
        result, _runner = self.invoke()
        self.assertEqual((result["archive_count"], result["lock_record_count"]), (1, 2))
        self.assertEqual(result["cache_add_batches"], 0)
        self.assertTrue(result["npm_ci_completed"])
        for field in ("installed_tree_identity_verified", "preferred_source_closure_complete",
                      "frontend_source_complete", "build_and_packaging_acceptance_verified",
                      "os_denied_acceptance_verified"):
            self.assertIs(result[field], False)
        self.assertEqual(before, {path: path.read_bytes() for path in before})
        for name in ("package.json", "package-lock.json", ".npmrc", "src/main.ts"):
            self.assertEqual((self.staged_frontend / name).read_bytes(), (self.frontend / name).read_bytes())
        staged_lock = json.loads((self.staged_frontend / "package-lock.json").read_bytes())
        self.assertEqual(staged_lock, self.canonical_lock)
        self.assertTrue(staged_lock["packages"]["node_modules/alpha/node_modules/alpha"]["optional"])
        self.assertFalse((self.staged_frontend / "should-not-run.js").exists())
        self.assertEqual(result["frontend_subdirectory"], install.FRONTEND.rstrip("/"))
        for package in verify_local_bundle(self.source)["packages"]:
            staged_archive = self.work / package["archive_path"]
            self.assertEqual(staged_archive.read_bytes(), (self.source / package["archive_path"]).read_bytes())
            for path in package["lock_paths"]:
                resolved = staged_lock["packages"][path]["resolved"]
                self.assertTrue(resolved.startswith("file:"))
                self.assertEqual((self.staged_frontend / resolved.removeprefix("file:")).resolve(), staged_archive)
        for name in (install.UPSTREAM_LOCK, install.LOCAL_MANIFEST, install.REPORT):
            self.assertEqual((self.work / name).read_bytes(), (self.source / name).read_bytes())

    def test_subprocesses_use_only_declared_tools_local_archives_and_fail_closed_environment(self) -> None:
        """Inherited configuration cannot select caches, hooks, proxies or injected Node code."""
        poison = {"NODE_OPTIONS": "--require unwanted.js", "NODE_PATH": "unwanted",
                  "NPM_CONFIG_CACHE": "external cache", "npm_config_ignore_scripts": "false",
                  "NPM_CONFIG_USERCONFIG": "credentials", "HTTP_PROXY": "unwanted",
                  "HOME": "external home", "PATH": "unwanted executable path"}
        with mock.patch.dict(os.environ, poison):
            self.invoke()
        self.assertEqual(len(self.calls), 3)
        for arguments, options in self.calls:
            self.assertEqual(arguments[0], str(self.node))
            self.assertEqual(options["cwd"], self.staged_frontend)
            self.assertIs(options["shell"], False)
            self.assertIs(options["check"], False)
            self.assertEqual(options["timeout"], 300)
            environment = options["env"]
            for key in ("NODE_OPTIONS", "NODE_PATH", "npm_config_ignore_scripts", "HTTP_PROXY"):
                self.assertNotIn(key, environment)
            self.assertEqual(environment["NPM_CONFIG_CACHE"], str(self.cache))
            self.assertEqual(environment["HOME"], str(self.work / "home"))
            for key in ("NPM_CONFIG_OFFLINE", "NPM_CONFIG_IGNORE_SCRIPTS"):
                self.assertEqual(environment[key], "true")
            for key in ("NPM_CONFIG_USERCONFIG", "NPM_CONFIG_GLOBALCONFIG"):
                self.assertEqual(Path(environment[key]).read_bytes(), b"")
        self.assertFalse(any(arguments[2:4] == ["cache", "add"] for arguments, _ in self.calls))
        for arguments, _ in self.calls[2:]:
            for flag in ("--offline", "--ignore-scripts", "--audit=false", "--fund=false"):
                self.assertIn(flag, arguments)
            self.assertEqual(arguments[arguments.index("--cache") + 1], str(self.cache))
            self.assertFalse(any(argument.startswith(("https:", "http:", "git:")) for argument in arguments))
        self.assertEqual(self.calls[-1][0][2], "ci")
        self.assertEqual(self.calls[-1][0][-3:], ["--include=dev", "--include=optional", "--include=peer"])

    def test_untracked_sources_and_generated_files_are_not_staged(self) -> None:
        """Only authenticated authored files are selected from the exported frontend."""
        for name in ("untracked.ts", ".env", "dist/output.js"):
            path = self.frontend / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"untracked synthetic input")
        self.invoke()
        for name in ("untracked.ts", ".env", "dist/output.js"):
            self.assertFalse((self.staged_frontend / name).exists())

    def test_manifest_cannot_stage_generated_private_or_escaping_paths(self) -> None:
        """Manifest membership does not authorize generated outputs or private files."""
        for name in ("dist/output.js", ".env", "secret.pem", "npm-shrinkwrap.json", "../outside.ts"):
            with self.subTest(name=name):
                manifest_path = self.source / install.MANIFEST
                original = manifest_path.read_bytes()
                document = json.loads(original)
                document["project_sources"].append({"path": install.FRONTEND + name,
                                                     "sha256": "0" * 64, "executable": False})
                manifest_path.write_text(json.dumps(document), encoding="utf-8")
                self.rejected_before_process("generated|private|unsupported|escaping")
                manifest_path.write_bytes(original)

    def test_missing_tampered_and_untracked_archives_fail_before_process(self) -> None:
        """Archive count, SRI and manifest membership cannot fall back to a cache."""
        archive = next(self.archives.iterdir())
        original = archive.read_bytes()
        archive.unlink()
        self.rejected_before_process("missing|unreadable|archive")
        archive.write_bytes(original + b"changed")
        self.rejected_before_process("SRI|archive|local npm source input validation failed")
        archive.write_bytes(original)
        manifest = json.loads((self.source / install.MANIFEST).read_bytes())
        manifest["project_sources"] = [record for record in manifest["project_sources"] if record["path"] != archive.relative_to(self.source).as_posix()]
        (self.source / install.MANIFEST).write_text(json.dumps(manifest), encoding="utf-8")
        self.rejected_before_process("source differs from manifest")
        self.assertEqual(list(self.cache.iterdir()), [])
        self.assertEqual(list(self.work.iterdir()), [])

    def test_report_requires_exact_local_keys_and_three_false_import_flags(self) -> None:
        """Expanded claims, absent false flags and unrecognized report keys are rejected."""
        path = self.source / install.REPORT
        original = path.read_bytes()
        for field in (*install._FALSE_REPORT_FIELDS, "extra_claim"):
            with self.subTest(field=field):
                document = json.loads(original)
                document[field] = True
                path.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n", encoding="utf-8", newline="\n")
                self.rehash_manifest(path)
                self.rejected_before_process("versioned npm report differs")
                path.write_bytes(original)
                self.rehash_manifest(path)
        document = json.loads(original)
        del document[install._FALSE_REPORT_FIELDS[0]]
        path.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n", encoding="utf-8", newline="\n")
        self.rehash_manifest(path)
        self.rejected_before_process("versioned npm report differs")

    def rehash_manifest(self, path: Path) -> None:
        """Make a changed fixture manifest-consistent without relaxing report checks."""
        manifest_path = self.source / install.MANIFEST
        manifest = json.loads(manifest_path.read_bytes())
        for record in manifest["project_sources"]:
            if record["path"] == path.relative_to(self.source).as_posix():
                record["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

    def test_nonempty_outputs_are_preserved_and_nested_outputs_reject(self) -> None:
        """An existing cache/workspace is never deleted or silently reused."""
        for directory in (self.work, self.cache):
            sentinel = directory / "retain-this"
            sentinel.write_bytes(b"retained existing output")
            self.rejected_before_process("initially be empty")
            self.assertEqual(sentinel.read_bytes(), b"retained existing output")
            sentinel.unlink()
        nested = self.work / "nested"
        nested.mkdir()
        self.rejected_before_process("empty|separate", cache=nested)
        inside_source = self.source / "fresh work"
        inside_source.mkdir()
        self.rejected_before_process("external", work=inside_source)

    def test_linked_output_and_source_ancestors_are_rejected(self) -> None:
        """A symlink cannot redirect verified inputs or fresh output writes."""
        link = self.root / "linked output"
        try:
            link.symlink_to(self.work, target_is_directory=True)
        except OSError:
            self.skipTest("symlink creation unavailable; synthetic reparse guard remains covered")
        self.rejected_before_process("linked|reparse", work=link)
        link.unlink()
        link.symlink_to(self.source, target_is_directory=True)
        self.rejected_before_process("linked|reparse", source=link)

    def test_windows_reparse_attribute_is_rejected(self) -> None:
        """The Windows reparse bit is checked independently of symlink mode."""
        actual = Path.lstat

        def metadata(path):
            value = actual(path)
            if path == self.work:
                return SimpleNamespace(st_mode=value.st_mode, st_file_attributes=0x400)
            return value

        with mock.patch.object(install.stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400, create=True), mock.patch.object(Path, "lstat", metadata):
            self.rejected_before_process("reparse")

    def test_every_process_phase_rechecks_immutable_inputs_and_stops_on_mutation(self) -> None:
        """Input changes after either version probe or ci prevent success."""
        for phase in ("node", "npm", "ci"):
            with self.subTest(phase=phase):
                fixture = FrontendArchiveInstallationTests()
                fixture.setUp()
                self.addCleanup(fixture.doCleanups)

                def mutate(arguments, **options):
                    result = fixture.process(arguments, **options)
                    selected = (phase == "node" and len(arguments) == 2 or
                                phase == "npm" and arguments[-1] == "--version" and len(arguments) == 3 or
                                phase == "ci" and len(arguments) > 3 and arguments[2] == phase)
                    if selected:
                        with (fixture.frontend / "src/main.ts").open("ab") as stream:
                            stream.write(b"changed")
                    return result

                with self.assertRaisesRegex(ValueError, "immutable source changed"):
                    fixture.invoke(process=mutate)
                self.assertEqual(len(fixture.calls), {"node": 1, "npm": 2, "ci": 3}[phase])

    def test_staged_canonical_mutation_does_not_report_success(self) -> None:
        """A successful exit cannot rescue a changed staged canonical lock."""
        def mutate(arguments, **options):
            result = self.process(arguments, **options)
            (self.staged_frontend / "package-lock.json").write_bytes(b"changed staged lock")
            return result

        with self.assertRaisesRegex(ValueError, "staged authored source changed"):
            self.invoke(process=mutate)
        self.assertEqual(len(self.calls), 1)

    def test_controlled_configs_remain_empty_after_every_process_phase(self) -> None:
        """A child cannot alter user/global npm policy for a later phase."""
        for filename in ("user.npmrc", "global.npmrc"):
            for phase in ("node", "npm", "ci"):
                with self.subTest(filename=filename, phase=phase):
                    fixture = FrontendArchiveInstallationTests()
                    fixture.setUp()
                    self.addCleanup(fixture.doCleanups)

                    def mutate(arguments, **options):
                        result = fixture.process(arguments, **options)
                        selected = (phase == "node" and len(arguments) == 2 or
                                    phase == "npm" and arguments[-1] == "--version" and len(arguments) == 3 or
                                    phase == "ci" and len(arguments) > 3 and arguments[2] == phase)
                        if selected:
                            (fixture.work / filename).write_bytes(b"fund=true\n")
                        return result

                    with self.assertRaisesRegex(ValueError, "controlled npm configuration changed"):
                        fixture.invoke(process=mutate)
                    self.assertEqual(len(fixture.calls), {"node": 1, "npm": 2, "ci": 3}[phase])

    def test_wrong_version_stops_before_cache_population(self) -> None:
        """Tool versions cannot select an alternative executable or download."""
        with self.assertRaisesRegex(ValueError, "command/version failed"):
            self.invoke(process=lambda *_args, **_options: SimpleNamespace(returncode=0, stdout="wrong\n"))
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_nonzero_exit_and_subprocess_exception_have_no_fallback(self) -> None:
        """Failed child commands stop before any later phase without deleting outputs."""
        cases = (SimpleNamespace(returncode=7, stdout=""), OSError("synthetic unavailable tool"),
                 subprocess.TimeoutExpired("synthetic tool", 300))
        for failure in cases:
            with self.subTest(failure=type(failure).__name__):
                fixture = FrontendArchiveInstallationTests()
                fixture.setUp()
                self.addCleanup(fixture.doCleanups)

                def fail(*_arguments, **_options):
                    if isinstance(failure, Exception):
                        raise failure
                    return failure

                with mock.patch.object(install.subprocess, "run", side_effect=fail) as runner:
                    with self.assertRaisesRegex(ValueError, "failed"):
                        install.install_frontend(fixture.source, fixture.work, fixture.cache,
                                                 fixture.node, fixture.npm_cli)
                    self.assertEqual(runner.call_count, 1)
                self.assertEqual(list(fixture.cache.iterdir()), [])
                self.assertTrue((fixture.staged_frontend / "package-lock.json").is_file())

    def test_parent_replacement_is_detected_after_child_phase(self) -> None:
        """An output root may not be replaced even when the child exits successfully."""
        def replace(arguments, **options):
            result = self.process(arguments, **options)
            self.cache.rename(self.root / "retained original cache")
            self.cache.mkdir()
            return result

        with self.assertRaisesRegex(ValueError, "ancestor changed"):
            self.invoke(process=replace)
        self.assertEqual(len(self.calls), 1)
        self.assertTrue((self.root / "retained original cache").is_dir())

    def test_manifest_and_report_mutation_are_rechecked(self) -> None:
        """Metadata edits after an invoked phase cannot weaken the original input set."""
        for filename in (install.MANIFEST, install.REPORT, install.UPSTREAM_LOCK, install.LOCAL_MANIFEST):
            with self.subTest(filename=filename):
                fixture = FrontendArchiveInstallationTests()
                fixture.setUp()
                self.addCleanup(fixture.doCleanups)

                def mutate(arguments, **options):
                    result = fixture.process(arguments, **options)
                    with (fixture.source / filename).open("ab") as stream:
                        stream.write(b" ")
                    return result

                with self.assertRaisesRegex(ValueError, "immutable source changed"):
                    fixture.invoke(process=mutate)
                self.assertEqual(len(fixture.calls), 1)

    def test_project_config_credentials_and_missing_manifest_are_rejected(self) -> None:
        """Private npm configuration and unauthenticated source selection never reach npm."""
        npmrc = self.frontend / ".npmrc"
        npmrc.write_bytes(b"userconfig=outside-the-staged-home\n")
        self.rehash_manifest(npmrc)
        self.rejected_before_process("unsupported or credential-bearing")
        (self.source / install.MANIFEST).unlink()
        self.rejected_before_process(".*")

    def test_duplicate_json_keys_and_malformed_lock_root_reject_before_process(self) -> None:
        """Ambiguous metadata is not normalized into a different dependency plan."""
        manifest = self.source / install.MANIFEST
        original = manifest.read_bytes()
        manifest.write_bytes(b'{"schema":"first","schema":"second"}')
        self.rejected_before_process("duplicate JSON key")
        manifest.write_bytes(original)
        lock_path = self.frontend / "package-lock.json"
        lock_path.write_bytes(b'{"packages":[]}')
        self.rehash_manifest(lock_path)
        self.rejected_before_process("canonical package/lock root")

    def test_all_local_archive_paths_are_staged_without_cache_add_or_platform_filtering(self) -> None:
        """All archives are staged once; no registry-key cache bootstrap is needed."""
        for index in range(8):
            self.add_archive(f"dependency-{index}", [f"node_modules/dependency-{index}"])
        self.freeze()
        result, _runner = self.invoke()
        self.assertEqual(result["archive_count"], 9)
        self.assertEqual(result["staged_archive_files"], 9)
        self.assertEqual(result["cache_add_batches"], 0)
        self.assertEqual(len(self.calls), 3)
        self.assertEqual(sorted(path.name for path in (self.work / install.ARCHIVES).iterdir()), sorted(path.name for path in self.archives.iterdir()))
        self.assertFalse(any(arguments[2:4] == ["cache", "add"] for arguments, _ in self.calls))

    def test_modified_distribution_stages_current_archive_and_exact_upstream_provenance(self) -> None:
        """A modified archive stays distinct from its historical registry integrity."""
        variant = self.add_local_variant()
        before = {path: path.read_bytes() for path in self.source.rglob("*") if path.is_file()}
        result, _runner = self.invoke()
        self.assertEqual((result["archive_count"], result["original_archive_count"], result["local_distribution_count"]), (2, 1, 1))
        self.assertEqual(result["staged_provenance_files"], 1)
        self.assertEqual(result["cache_add_batches"], 0)
        self.assertEqual(len(self.calls), 3)
        for name in (variant["archive_path"], variant["provenance_path"], install.UPSTREAM_LOCK, install.LOCAL_MANIFEST):
            self.assertEqual((self.work / name).read_bytes(), (self.source / name).read_bytes())
        upstream = json.loads((self.work / install.UPSTREAM_LOCK).read_bytes())["packages"]["node_modules/beta"]
        current = json.loads((self.staged_frontend / "package-lock.json").read_bytes())["packages"]["node_modules/beta"]
        self.assertTrue(upstream["resolved"].startswith("https://registry.npmjs.org/"))
        self.assertEqual(current["resolved"], "file:../../../" + variant["archive_path"])
        self.assertNotEqual(current["integrity"], upstream["integrity"])
        self.assertEqual((self.staged_frontend / current["resolved"].removeprefix("file:")).resolve(), self.work / variant["archive_path"])
        self.assertEqual(before, {path: path.read_bytes() for path in before})

    def test_local_archive_and_provenance_tamper_or_absence_fail_before_process(self) -> None:
        """A valid export hash cannot rescue changed or absent modified-source proof."""
        variant = self.add_local_variant()
        for name in (variant["archive_path"], variant["provenance_path"]):
            path = self.source / name
            original = path.read_bytes()
            with self.subTest(name=name, mutation="missing"):
                path.unlink()
                self.rejected_before_process("missing|unreadable|archive|provenance|input")
                path.write_bytes(original)
            with self.subTest(name=name, mutation="changed"):
                path.write_bytes(original + b" changed synthetic data")
                self.rehash_manifest(path)
                self.rejected_before_process("digest|hash|archive|provenance")
                path.write_bytes(original)
                self.rehash_manifest(path)
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_every_archive_provenance_and_metadata_requires_export_manifest_membership(self) -> None:
        """Verifier success alone does not authorize unmanifested build inputs."""
        variant = self.add_local_variant()
        manifest_path = self.source / install.MANIFEST
        original = manifest_path.read_bytes()
        for name in (variant["archive_path"], variant["provenance_path"], install.UPSTREAM_LOCK, install.LOCAL_MANIFEST):
            with self.subTest(name=name):
                document = json.loads(original)
                document["project_sources"] = [record for record in document["project_sources"] if record["path"] != name]
                manifest_path.write_text(json.dumps(document), encoding="utf-8")
                self.rejected_before_process("source differs from manifest|omits required")
                manifest_path.write_bytes(original)

    def test_current_lock_cannot_reintroduce_registry_or_escaping_archive_resolution(self) -> None:
        """Only the exact canonical local-file projection may reach npm."""
        lock_path = self.frontend / "package-lock.json"
        original = lock_path.read_bytes()
        for resolved in ("https://registry.npmjs.org/alpha/-/alpha-1.0.0.tgz", "file:../../../../escape.tgz", "file:../../../source-inputs/npm/archives/../escape.tgz"):
            with self.subTest(resolved=resolved):
                document = json.loads(original)
                document["packages"]["node_modules/alpha"]["resolved"] = resolved
                lock_path.write_text(json.dumps(document), encoding="utf-8")
                self.rehash_manifest(lock_path)
                self.rejected_before_process("canonical|projection|resolved|local|path")
                lock_path.write_bytes(original)
                self.rehash_manifest(lock_path)

    def test_modified_input_and_staged_archive_mutation_stop_each_process_phase(self) -> None:
        """Both source proof and staged archives remain pinned through every child."""
        for phase in ("node", "npm", "ci"):
            for subject in ("source-archive", "source-provenance", "staged-archive", "staged-provenance"):
                with self.subTest(phase=phase, subject=subject):
                    fixture = FrontendArchiveInstallationTests()
                    fixture.setUp()
                    self.addCleanup(fixture.doCleanups)
                    variant = fixture.add_local_variant()

                    def mutate(arguments, **options):
                        result = fixture.process(arguments, **options)
                        selected = (phase == "node" and len(arguments) == 2 or
                                    phase == "npm" and len(arguments) == 3 and arguments[-1] == "--version" or
                                    phase == "ci" and len(arguments) > 3 and arguments[2] == "ci")
                        if selected:
                            root = fixture.work if subject.startswith("staged") else fixture.source
                            name = variant["provenance_path"] if subject.endswith("provenance") else variant["archive_path"]
                            with (root / name).open("ab") as stream:
                                stream.write(b"changed synthetic bytes")
                        return result

                    with self.assertRaisesRegex(ValueError, "immutable source changed|staged authored source changed"):
                        fixture.invoke(process=mutate)
                    self.assertEqual(len(fixture.calls), {"node": 1, "npm": 2, "ci": 3}[phase])

    def test_archive_inventory_changes_after_child_phase_prevent_success(self) -> None:
        """An extra archive is not ignored after initial authenticated admission."""
        def mutate(arguments, **options):
            result = self.process(arguments, **options)
            (self.archives / "untracked.tgz").write_bytes(b"synthetic untracked archive")
            return result

        with self.assertRaisesRegex(ValueError, "immutable archive inventory changed"):
            self.invoke(process=mutate)
        self.assertEqual(len(self.calls), 1)

    def test_export_execute_intent_is_preserved_in_staging(self) -> None:
        """Manifest execute intent is retained where the host exposes it."""
        source_path = self.frontend / "src/main.ts"
        source_path.chmod(0o755)
        manifest_path = self.source / install.MANIFEST
        document = json.loads(manifest_path.read_bytes())
        for record in document["project_sources"]:
            if record["path"] == source_path.relative_to(self.source).as_posix():
                record["executable"] = True
        manifest_path.write_text(json.dumps(document), encoding="utf-8")
        self.invoke()
        staged = self.staged_frontend / "src/main.ts"
        self.assertEqual(staged.read_bytes(), source_path.read_bytes())
        if os.name != "nt":
            self.assertTrue(staged.stat().st_mode & 0o111)

    def test_observable_source_or_staged_permission_changes_stop_after_child(self) -> None:
        """Unchanged file contents cannot hide a changed source/output file mode."""
        for subject in ("source", "staged"):
            with self.subTest(subject=subject):
                fixture = FrontendArchiveInstallationTests()
                fixture.setUp()
                self.addCleanup(fixture.doCleanups)

                def mutate(arguments, **options):
                    result = fixture.process(arguments, **options)
                    frontend = fixture.frontend if subject == "source" else fixture.staged_frontend
                    (frontend / "src/main.ts").chmod(0o444)
                    return result

                with self.assertRaisesRegex(ValueError, "immutable source changed|staged authored source changed"):
                    fixture.invoke(process=mutate)
                self.assertEqual(len(fixture.calls), 1)

    @unittest.skipIf(os.name == "nt", "Windows does not expose POSIX execute intent")
    def test_source_execute_mode_mismatch_rejects_before_any_process(self) -> None:
        """An authenticated hash does not bypass the manifest's executable bit."""
        (self.frontend / "src/main.ts").chmod(0o755)
        self.rejected_before_process("source executable mode differs")


if __name__ == "__main__":
    unittest.main()
