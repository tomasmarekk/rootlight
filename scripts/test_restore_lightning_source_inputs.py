"""Authenticate explicit metadata restoration using real authored producer/Yarn inputs.

Only disposable fixtures and their reviewed archive pin differ from production.
Strict verifiers are never mocked; package sources, installers and network never run.
"""

from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
import copy
import io
import json
import os
from pathlib import Path
import shutil
import socket
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import restore_lightning_source_inputs as restore
import test_prepare_rust_source_bundle as exports
from test_verify_lightning_producer_sources import AuthoredFixture


class SourceRestorationTests(AuthoredFixture, unittest.TestCase):
    """Pin byte conservation and reject untrusted hints before original mutation."""

    def setUp(self):
        exports.YarnSourceExportTests.setUp(self)

    def state(self):
        """Record all original namespace bytes and permissions, excluding temporary clones."""
        result = {}
        for root in (self.destination, self.yarn_root):
            if not os.path.lexists(root):
                result[str(root)] = ("absent", None)
                continue
            for path in (root, *sorted(root.rglob("*"))):
                metadata = path.lstat()
                result[str(path)] = (stat.S_IMODE(metadata.st_mode),
                                     path.read_bytes() if path.is_file() else None)
        return result

    def empties(self, project=None):
        """Return the three authenticated explicit empty-directory paths."""
        project = project or self.project
        return [project / restore.yarn.PRODUCER_PATH / "tree/documentation/empty",
                *(project / restore.yarn.BUNDLE_PATH / row["expanded_path"] / "authored root/nested/empty"
                  for row in self.yarn_report["packages"])]

    def remove_empties(self):
        """Simulate omitted directory metadata while retaining every source-file byte."""
        for path in self.empties():
            path.rmdir()

    def reject_unchanged(self):
        """Require a typed preparation rejection and an untouched original frame."""
        before = self.state()
        with self.assertRaises((ValueError, OSError)):
            restore.restore_project(self.project, restore_file_modes=True)
        self.assertEqual(self.state(), before)

    def test_fresh_git_index_checkout_loses_empty_directories_and_requires_explicit_restoration(self):
        git = shutil.which("git")
        if git is None:
            self.skipTest("Git is a declared checkout prerequisite")
        repository, checkout = self.root / "neutral-git", self.root / "neutral-checkout"
        repository.mkdir()
        checkout.mkdir()
        environment = {name: value for name, value in os.environ.items() if not name.startswith("GIT_")}
        environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)

        def command(*arguments, data=None):
            result = subprocess.run([git, "-c", "core.longpaths=true", "-c", "core.autocrlf=false",
                                     "-c", "core.eol=lf", "-c", "core.attributesFile=" + os.devnull,
                                     "-C", str(repository), *arguments], input=data, env=environment,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
            self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
            return result.stdout

        command("init", "--quiet")
        before, index = {}, bytearray()
        for name, _executable in self.inventory:
            path = self.project / name
            payload = path.read_bytes()
            before[name] = payload
            blob = command("hash-object", "-w", "--stdin", data=payload).strip()
            mode = b"100755" if path.stat().st_mode & 0o111 else b"100644"
            index.extend(mode + b" " + blob + b"\t" + name.encode("utf-8") + b"\0")
        # Exact authored-fixture plumbing avoids upstream ignore rules without
        # changing .gitignore bytes, using git add -f, or touching the real index.
        command("update-index", "-z", "--index-info", data=bytes(index))
        command("checkout-index", "--all", "--prefix=" + checkout.as_posix() + "/")
        for path in self.empties(checkout):
            self.assertFalse(path.exists())
        self.assertEqual({name: (checkout / name).read_bytes() for name in before}, before)
        # The synthetic consumer's unrelated empty namespace is an npm prerequisite,
        # not part of this producer/Yarn restoration contract.
        (checkout / exports.npm.LOCAL_DISTRIBUTIONS).mkdir()
        with self.assertRaises(ValueError):
            restore.producer.verify_bundle(checkout, checkout / restore.yarn.PRODUCER_PATH)
        destination = self.root / "strict-rejected-export"
        with self.assertRaises(ValueError):
            exports.RustSourceBundleTests.export_project(self, checkout, self.inventory, destination)
        self.assertFalse(destination.exists())
        proof = restore.restore_project(checkout)
        self.assertEqual(sum(len(row["restored_directories"]) for row in proof["namespaces"]), 6)
        for path in self.empties(checkout):
            self.assertTrue(path.is_dir())
            self.assertEqual(list(path.iterdir()), [])
        self.assertEqual({name: (checkout / name).read_bytes() for name in before}, before)
        exported, manifest = exports.RustSourceBundleTests.export_project(
            self, checkout, self.inventory, self.root / "strict-restored-export")
        self.assertEqual(exports.bundle.verify_project_bundle(exported), manifest)
        repeated = restore.restore_project(checkout)
        self.assertTrue(all(not row["restored_directories"] and not row["restored_file_modes"]
                            for row in repeated["namespaces"]))

    def test_restoration_preserves_regular_bytes_existing_directories_and_repeat_noop(self):
        if os.name != "nt":
            (self.destination / "tree").chmod(0o710)
        self.remove_empties()
        before = self.state()
        proof = restore.restore_project(self.project)
        after = self.state()
        self.assertEqual({name: record for name, record in after.items() if name in before}, before)
        self.assertEqual(sum(len(row["restored_directories"]) for row in proof["namespaces"]), 3)
        self.assertTrue(all(row["all_regular_file_bytes_conserved"]
                            and row["strict_original_verification_passed"] for row in proof["namespaces"]))
        repeated = restore.restore_project(self.project)
        self.assertEqual(self.state(), after)
        self.assertTrue(all(not row["restored_directories"] and not row["restored_file_modes"]
                            for row in repeated["namespaces"]))

    def test_only_entire_optional_parent_absence_is_accepted(self):
        with tempfile.TemporaryDirectory() as temporary:
            project = Path(temporary)
            self.assertEqual(restore.restore_project(project)["namespaces"], [])
            for parent in ("producers", "producer-dependencies"):
                path = project / "source-inputs" / parent
                path.mkdir(parents=True)
                with self.assertRaisesRegex(ValueError, "namespace_inventory_differs"):
                    restore.restore_project(project)
                path.rmdir()

    def test_partial_foreign_and_extra_namespace_children_are_not_optional_absence(self):
        parent = self.yarn_root.parent
        original = parent / "yarn-originals"
        saved = self.root / "saved-yarn"
        original.rename(saved)
        self.reject_unchanged()
        foreign = parent / "unreviewed"
        foreign.mkdir()
        self.reject_unchanged()
        foreign.rmdir()
        saved.rename(original)
        extra = parent / "extra"
        extra.mkdir()
        self.reject_unchanged()

    def test_tampered_archives_reports_and_members_reject_without_metadata_changes(self):
        self.remove_empties()
        # Keep the earlier producer complete while testing a later Yarn rejection.
        self.empties()[0].mkdir()
        controls = (self.destination / restore.producer.ARCHIVE_NAME,
                    self.destination / "source-tree.json", self.destination / "tree/src/value.rs",
                    self.yarn_rows[0]["archive"], self.yarn_root / restore.yarn.REPORT_NAME,
                    self.yarn_rows[1]["expanded"] / "authored root/LICENSE")
        for path in controls:
            original = path.read_bytes()
            path.write_bytes(original + b"changed")
            with self.subTest(path=path):
                self.reject_unchanged()
            path.write_bytes(original)

    def test_missing_regular_inputs_are_never_reconstructed_from_archives(self):
        controls = (self.destination / "tree/src/value.rs", self.yarn_rows[1]["archive"],
                    self.yarn_rows[0]["expanded"] / "authored root/bin/task.js")
        for path in controls:
            payload, mode = path.read_bytes(), stat.S_IMODE(path.stat().st_mode)
            path.unlink()
            with self.subTest(path=path):
                self.reject_unchanged()
                self.assertFalse(path.exists())
            path.write_bytes(payload)
            path.chmod(mode)

    def test_extra_regular_files_and_directories_cannot_be_whitelisted_by_hints(self):
        for root in (self.destination / "tree", self.yarn_root / "packages"):
            for directory in (False, True):
                path = root / "unreviewed"
                if directory:
                    path.mkdir()
                else:
                    path.write_bytes(b"unreviewed inert source")
                with self.subTest(root=root, directory=directory):
                    self.reject_unchanged()
                path.rmdir() if directory else path.unlink()

    def test_forged_directory_mode_path_and_claim_hints_never_authorize_original_changes(self):
        self.empties()[1].rmdir()
        path = self.yarn_root / restore.yarn.REPORT_NAME
        original = path.read_bytes()
        member = next(name for name in self.yarn_report["files"] if name.endswith("/bin/task.js"))
        changes = (lambda report: report["directories"].append("packages/unreviewed-empty"),
                   lambda report: report["directories"].append("../escape"),
                   lambda report: report["files"][member].update(mode=0o644),
                   lambda report: report["files"][member].update(mode=True),
                   lambda report: report.update(preferred_source_closure_complete=True),
                   lambda report: report["directories"].append("PACKAGES"))
        for change in changes:
            report = copy.deepcopy(self.yarn_report)
            change(report)
            path.write_bytes(restore.producer.json_bytes(report))
            self.reject_unchanged()
        path.write_bytes(original)

    def test_forged_producer_empty_directory_hint_is_not_trusted(self):
        path = self.destination / "source-tree.json"
        report = json.loads(path.read_bytes())
        report["source_inventory"]["members"].append({
            "path": restore.producer.ROOT + "/invented-empty", "kind": "directory",
            "bytes": 0, "archive_mode": 0o755})
        path.write_bytes(restore.producer.json_bytes(report))
        self.reject_unchanged()
        self.assertFalse((self.destination / "tree/invented-empty").exists())

    def test_noncanonical_or_duplicate_json_report_hints_reject(self):
        path = self.yarn_root / restore.yarn.REPORT_NAME
        for payload in (json.dumps(self.yarn_report).encode(), b'{"schema":"x","schema":"y"}\n'):
            path.write_bytes(payload)
            self.reject_unchanged()

    def test_existing_copy_bounds_are_enforced_before_original_metadata_changes(self):
        for module, limit, value in ((restore.producer, "MAX_JSON", 1),
                                     (restore.producer, "MAX_EXPANDED", 1),
                                     (restore.yarn, "MAX_COMPRESSED", 1),
                                     (restore.yarn, "MAX_TOTAL_EXPANDED", 1)):
            with self.subTest(limit=limit), patch.object(module, limit, value):
                self.reject_unchanged()

    def test_excess_directory_hints_reject_before_temporary_creation(self):
        path = self.yarn_root / restore.yarn.REPORT_NAME
        report = copy.deepcopy(self.yarn_report)
        report["directories"].append("packages/authored-extra-empty")
        path.write_bytes(restore.producer.json_bytes(report))
        before = self.state()
        with patch.object(restore, "MAX_HINT_DIRECTORY_COUNT", len(self.yarn_report["directories"])), \
                patch.object(restore.tempfile, "TemporaryDirectory",
                             side_effect=AssertionError("hints must reject before temporary creation")):
            with self.assertRaisesRegex(ValueError, "hint_directory_limit"):
                restore._restore_namespace(self.project, self.yarn_root, is_yarn=True, restore_file_modes=True)
        self.assertEqual(self.state(), before)

    def test_excess_path_bytes_reject_before_temporary_creation(self):
        path = self.yarn_root / restore.yarn.REPORT_NAME
        report = copy.deepcopy(self.yarn_report)
        report["directories"].append("packages/" + "long-authored-directory-" * 8)
        path.write_bytes(restore.producer.json_bytes(report))
        before = self.state()
        with patch.object(restore, "MAX_HINT_PATH_BYTES", 128), \
                patch.object(restore.tempfile, "TemporaryDirectory",
                             side_effect=AssertionError("hints must reject before temporary creation")):
            with self.assertRaisesRegex(ValueError, "hint_path_byte_limit"):
                restore._restore_namespace(self.project, self.yarn_root, is_yarn=True, restore_file_modes=True)
        self.assertEqual(self.state(), before)

    def test_parent_derivation_and_alias_prefixes_obey_utility_only_budgets(self):
        with patch.object(restore, "MAX_HINT_DIRECTORY_COUNT", 2):
            with self.assertRaisesRegex(ValueError, "hint_directory_limit"):
                restore._parents({"authored/one/two/three/file"})
        with patch.object(restore, "MAX_HINT_PATH_IDENTITIES", 2):
            with self.assertRaisesRegex(ValueError, "hint_path_identity_limit"):
                restore._relative("authored/one/two/file", {})

    def test_links_and_portable_reparse_guards_reject_before_original_changes(self):
        target = self.yarn_rows[0]["archive"]
        inode = target.lstat().st_ino
        original = restore.producer.local.original._is_link
        with patch.object(restore.producer.local.original, "_is_link",
                          side_effect=lambda metadata: metadata.st_ino == inode or original(metadata)):
            self.reject_unchanged()
        link = self.yarn_root / "linked-input"
        try:
            link.symlink_to(target)
        except OSError:
            self.skipTest("symlink creation is unavailable; portable reparse rejection was checked")
        before = self.state()
        with self.assertRaises(ValueError):
            restore.restore_project(self.project)
        self.assertTrue(link.is_symlink())
        self.assertEqual(self.state(), before)

    @unittest.skipIf(os.name == "nt", "POSIX file execute intent is not a Windows permission guarantee")
    def test_file_mode_normalization_requires_opt_in_and_never_changes_source_bytes(self):
        self.remove_empties()
        paths = (self.destination / "tree/scripts/build.js",
                 self.yarn_rows[0]["expanded"] / "authored root/bin/task.js")
        for path in paths:
            path.chmod(0o644)
        before = self.state()
        with self.assertRaisesRegex(ValueError, "file_modes_require_explicit_restoration"):
            restore.restore_project(self.project)
        self.assertEqual(self.state(), before)
        proof = restore.restore_project(self.project, restore_file_modes=True)
        self.assertEqual(sum(len(row["restored_file_modes"]) for row in proof["namespaces"]), 2)
        for path in paths:
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o755)
        self.assertEqual({name: record[1] for name, record in self.state().items() if record[1] is not None},
                         {name: record[1] for name, record in before.items() if record[1] is not None})

    def test_late_source_change_after_clone_authentication_rejects_before_restoration(self):
        self.remove_empties()
        original = restore._snapshot
        changed = self.destination / "tree/src/value.rs"
        payload = changed.read_bytes()
        mutated = False

        def snapshot(bundle, is_yarn, nodes, staged=None):
            nonlocal mutated
            if bundle == self.destination and staged is None and not mutated:
                changed.write_bytes(payload + b"late change")
                mutated = True
            return original(bundle, is_yarn, nodes, staged)

        with patch.object(restore, "_snapshot", side_effect=snapshot):
            with self.assertRaisesRegex(ValueError, "source_frame_changed_before_restoration"):
                restore.restore_project(self.project)
        self.assertTrue(mutated)
        self.assertEqual(changed.read_bytes(), payload + b"late change")
        self.assertTrue(all(not path.exists() for path in self.empties()))

    def test_authenticated_metadata_partial_io_failure_is_safe_to_reverify_and_resume(self):
        empty = self.empties()[0]
        empty.rmdir()
        empty.parent.rmdir()
        before = {name: record[1] for name, record in self.state().items() if record[1] is not None}
        original = Path.mkdir

        def mkdir(path, *arguments, **keywords):
            if path == empty:
                raise OSError("injected neutral metadata I/O failure")
            return original(path, *arguments, **keywords)

        with patch.object(Path, "mkdir", mkdir):
            with self.assertRaises(OSError):
                restore.restore_project(self.project)
        self.assertTrue(empty.parent.is_dir())
        self.assertFalse(empty.exists())
        proof = restore.restore_project(self.project)
        self.assertEqual(proof["namespaces"][0]["restored_directories"], ["tree/documentation/empty"])
        self.assertEqual({name: record[1] for name, record in self.state().items() if record[1] is not None}, before)

    def test_cli_is_source_free_and_keeps_build_and_preferred_acceptance_false(self):
        self.remove_empties()
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.object(subprocess, "run", side_effect=AssertionError("subprocess prohibited")), \
                patch.object(socket.socket, "connect", side_effect=AssertionError("network prohibited")), \
                redirect_stdout(stdout), redirect_stderr(stderr):
            code = restore.main(["--project", str(self.project)])
        self.assertEqual(code, 0)
        self.assertEqual(stderr.getvalue(), "")
        proof = json.loads(stdout.getvalue())
        for name in ("source_file_bytes_written", "package_code_executed", "preferred_source_closure_complete",
                     "os_denied_acceptance_verified", "build_and_packaging_acceptance_verified"):
            self.assertIs(proof[name], False)
        self.assertEqual(len(proof["namespaces"]), 2)

    def test_cli_failure_diagnostic_never_contains_report_source_contents(self):
        marker = "authored confidential-looking test marker"
        (self.destination / "source-tree.json").write_bytes(('{"fixture":"' + marker + '"}\n').encode())
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = restore.main(["--project", str(self.project)])
        self.assertEqual(code, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertNotIn(marker, stderr.getvalue())
        self.assertEqual(stderr.getvalue(), "source metadata restoration failed; no acceptance claim\n")


if __name__ == "__main__":
    unittest.main()
