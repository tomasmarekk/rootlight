"""Authored read-only placement controls, not a real installation acceptance proof.

Only the separately reviewed producer authentication boundary is modeled. Real
Yarn graph/archive/tree helpers authenticate synthetic inputs. Junction metadata
is modeled without link privileges; no installer, package code or child runs.
"""

from __future__ import annotations

import base64
from contextlib import redirect_stdout, redirect_stderr
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import verify_lightning_installed_placements as observer
from test_verify_lightning_yarn_sources import archive


class PlacementObservationTests(unittest.TestCase):
    """Exercise exact payload, mandatory resolution and declared-root boundaries."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.helper_root = Path(observer.yarn.__file__).parent
        self.serial = 0

    def fixture(self, *, repeated_archive=False, archive_owned_nm=False):
        """Build inert authenticated inputs and a synthetic nested placement tree."""
        self.serial += 1
        base = self.root / str(self.serial)
        source, installed = base / "source", base / "installed"
        tree = source / observer.yarn.PRODUCER_PATH / "tree"
        bundle = source / observer.yarn.BUNDLE_PATH
        tree.mkdir(parents=True)
        (bundle / "archives").mkdir(parents=True)
        (bundle / "packages").mkdir()
        manifest = {"name": "fixture-producer", "version": "1.0.0",
                    "dependencies": {"app": "^1.0.0", "leaf": "^1.0.0"},
                    "optionalDependencies": {"@scope/maybe": "^1.0.0"},
                    "resolutions": {"fixture-producer": "link:."}}
        if repeated_archive:
            manifest["devDependencies"] = {"app": "~1.0.0"}
        models = [
            ("app", "1.0.0", "^1.0.0",
             {"leaf": "^2.0.0", "fixture-producer": "^1.0.0"}, {"@scope/maybe": "^1.0.0"}),
            ("leaf", "1.0.0", "^1.0.0", {}, {}),
            ("leaf", "2.0.0", "^2.0.0", {}, {}),
            ("@scope/maybe", "1.0.0", "^1.0.0", {"helper": "^1.0.0"}, {}),
            ("helper", "1.0.0", "^1.0.0", {}, {}),
        ]
        if repeated_archive:
            models.append(("hidden", "1.0.0", "^1.0.0", {}, {}))
        expanded, sections, pins = {}, ["# yarn lockfile v1\n\n"], {}
        for name, version, request, dependencies, optional in models:
            package_manifest = {"name": name, "version": version, "license": "MIT",
                                "repository": "https://example.invalid/authored"}
            if dependencies:
                package_manifest["dependencies"] = dependencies
            if optional:
                package_manifest["optionalDependencies"] = optional
            extras = {"authored root/package.json": (json.dumps(package_manifest).encode(), 0o644)}
            if name == "app":
                extras["authored root/test/fixtures/node_modules/foreign/package.json"] = (
                    b'{"name":"ordinary-fixture-only","version":"9.0.0"}\n', 0o644)
                if archive_owned_nm:
                    extras["authored root/node_modules/leaf/package.json"] = (
                        b'{"name":"leaf","version":"2.0.0","ordinaryPayload":true}\n', 0o644)
            payload, members = archive(name, version, extras=extras)
            basename = name.rsplit("/", 1)[-1]
            resolved = (f"https://registry.yarnpkg.com/{name}/-/{basename}-{version}.tgz#"
                        + hashlib.sha1(payload).hexdigest())
            integrity = "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode("ascii")
            filename = hashlib.sha256(resolved.encode("ascii")).hexdigest() + ".tgz"
            original = bundle / "archives" / filename
            original.write_bytes(payload)
            original.chmod(0o644)
            target = bundle / "packages" / Path(filename).stem
            (target / "authored root/nested/empty").mkdir(parents=True)
            for path, (contents, mode) in members.items():
                destination = target / path
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(contents)
                destination.chmod(mode)
            expanded[(name, version)] = target / "authored root"
            pins[(name, version)] = (resolved, integrity)

            def stanza(selector, mandatory, optionals):
                resolved_pin, sri = pins[(name, version)]
                text = (json.dumps(selector) + ":\n" + f'  version "{version}"\n'
                        + f'  resolved "{resolved_pin}"\n' + f"  integrity {sri}\n")
                for label, edges in (("dependencies", mandatory), ("optionalDependencies", optionals)):
                    if edges:
                        text += "  " + label + ":\n"
                        text += "".join("    " + json.dumps(key) + " " + json.dumps(value) + "\n"
                                        for key, value in edges.items())
                return text + "\n"

            sections.append(stanza(name + "@" + request, dependencies, optional))
            if name == "app" and repeated_archive:
                sections.append(stanza("app@~1.0.0", {"hidden": "^1.0.0"}, {}))
        for selector in ("fixture-producer@^1.0.0", "fixture-producer@link:."):
            sections.append(json.dumps(selector) + ':\n  version "0.0.0"\n  uid ""\n\n')
        (tree / "package.json").write_bytes(json.dumps(manifest).encode())
        (tree / "yarn.lock").write_bytes("".join(sections).encode("ascii"))
        (tree / "src").mkdir()
        (tree / "src/authored.js").write_bytes(b"authored producer payload; never executed\n")
        producer_members = [{"path": "authored producer", "kind": "directory",
                             "bytes": 0, "archive_mode": 0o755}]
        for path in sorted(tree.rglob("*")):
            relative = "authored producer/" + path.relative_to(tree).as_posix()
            if path.is_dir():
                producer_members.append({"path": relative, "kind": "directory",
                                         "bytes": 0, "archive_mode": 0o755})
            else:
                contents = path.read_bytes()
                path.chmod(0o644)
                producer_members.append({"path": relative, "kind": "file", "bytes": len(contents),
                                         "sha256": hashlib.sha256(contents).hexdigest(), "archive_mode": 0o644})
        source_report = tree.parent / "source-tree.json"
        source_report.write_bytes(observer.yarn.producer.json_bytes({
            "source_inventory": {"archive_root": "authored producer", "members": producer_members}}))
        source_report.chmod(0o644)
        value = SimpleNamespace(source=source, installed=installed, tree=tree, bundle=bundle,
                                expanded=expanded, source_report=source_report, manifest=manifest)
        with patch.object(observer.yarn, "_inputs", side_effect=lambda project: self.inputs(value, project)):
            value.report = observer.yarn.derive_bundle(source, bundle)
        report_path = bundle / observer.yarn.REPORT_NAME
        report_path.write_bytes(observer.yarn.producer.json_bytes(value.report))
        report_path.chmod(0o644)
        shutil.copytree(tree, installed)
        self.package(value, ("app", "1.0.0"), installed / "node_modules/app")
        self.package(value, ("leaf", "1.0.0"), installed / "node_modules/leaf")
        self.package(value, ("@scope/maybe", "1.0.0"), installed / "node_modules/@scope/maybe")
        self.package(value, ("helper", "1.0.0"), installed / "node_modules/helper")
        nested = (installed / "node_modules/helper/node_modules/leaf" if archive_owned_nm
                  else installed / "node_modules/app/node_modules/leaf")
        self.package(value, ("leaf", "2.0.0"), nested)
        if repeated_archive:
            self.package(value, ("hidden", "1.0.0"), installed / "node_modules/hidden")
        link = installed / "node_modules/fixture-producer"
        link.mkdir()
        value.links = {link: (installed, getattr(stat, "IO_REPARSE_TAG_MOUNT_POINT", 0xA0000003))}
        (installed / "node_modules/.bin").mkdir()
        (installed / "node_modules/.bin/authored.cmd").write_bytes(b"unqualified inert generated bytes\r\n")
        (installed / "node_modules/.yarn-integrity").write_bytes(b'{"unqualified":"authored"}\n')
        return value

    def inputs(self, fixture, project):
        """Model producer association only; real source reads and Yarn checks remain."""
        self.assertEqual(project, fixture.source)
        read = observer.yarn.producer.read_bytes
        return ({"association": {"name": "fixture-producer", "version": "1.0.0"}},
                read(fixture.tree / "package.json", observer.yarn.MAX_MANIFEST),
                read(fixture.tree / "yarn.lock", observer.yarn.yarn.MAX_LOCK_BYTES),
                read(fixture.source_report, observer.yarn.MAX_REPORT))

    def package(self, fixture, identity, destination):
        """Copy authored package payload into a new ordinary synthetic placement."""
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(fixture.expanded[identity], destination)

    def link_metadata(self, fixture):
        """Model raw junction lstat/readlink, retaining real reads everywhere else."""
        original_stat, original_readlink = Path.lstat, os.readlink

        def metadata(path):
            value = original_stat(path)
            if path not in fixture.links:
                return value
            attributes = {name: getattr(value, name) for name in dir(value) if name.startswith("st_")}
            attributes["st_file_attributes"] = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
            attributes["st_reparse_tag"] = fixture.links[path][1]
            return SimpleNamespace(**attributes)

        def readlink(path, *args, **kwargs):
            if Path(path) in fixture.links:
                return str(fixture.links[Path(path)][0])
            return original_readlink(path, *args, **kwargs)

        return patch.object(Path, "lstat", metadata), patch.object(os, "readlink", readlink)

    def observe(self, fixture):
        """Run only the proposed observer with real authored archive helpers."""
        metadata, readlink = self.link_metadata(fixture)
        with metadata, readlink, \
             patch.object(observer.yarn, "_inputs", side_effect=lambda project: self.inputs(fixture, project)), \
             patch.object(observer.yarn, "verify_bundle", wraps=observer.yarn.verify_bundle) as strict:
            result = observer.observe(fixture.source, fixture.installed, helper_root=self.helper_root)
            self.assertEqual(strict.call_count, 2)
            return result

    def frozen(self, fixture):
        """Return ordinary fixture bytes without following any modeled junction."""
        return {path.relative_to(fixture.source.parent).as_posix(): path.read_bytes()
                for root in (fixture.source, fixture.installed) for path in root.rglob("*") if path.is_file()}

    def test_exact_nested_bytes_bindings_links_and_false_global_claims(self):
        fixture = self.fixture()
        before = self.frozen(fixture)
        result = self.observe(fixture)
        self.assertEqual(before, self.frozen(fixture))
        self.assertEqual(result["authenticated_archive_count"], len(fixture.report["packages"]))
        self.assertEqual(result["observed_registry_identity_count"], len(fixture.report["packages"]))
        self.assertEqual(result["observed_registry_placement_count"], len(fixture.report["packages"]))
        expected_files = len([row for row in json.loads(fixture.source_report.read_bytes())["source_inventory"]["members"]
                              if row["kind"] == "file"])
        expected_files += sum(row["kind"] == "file" for package in fixture.report["packages"]
                              for row in package["published_package"]["members"])
        self.assertEqual(result["verified_payload_file_count"], expected_files)
        required = {root["target_stanza"] for root in fixture.report["pin_graph"]["roots"]
                    if root["section"] not in ("optionalDependencies", "resolutions")}
        pending = list(required)
        while pending:
            index = pending.pop()
            for edge in fixture.report["pin_graph"]["stanzas"][index]["edges"]:
                if edge["section"] == "dependencies" and edge["target_stanza"] not in required:
                    required.add(edge["target_stanza"])
                    pending.append(edge["target_stanza"])
        self.assertEqual(result["globally_required_stanza_count"], len(required))
        self.assertEqual(result["declared_local_association_count"],
                         len(fixture.report["pin_graph"]["local_associations"]))
        self.assertEqual(result["observed_local_link_count"], len(fixture.links))
        self.assertTrue(result["strict_source_reverification_completed"])
        self.assertTrue(result["observed_published_package_bytes_verified"])
        self.assertTrue(result["required_nonoptional_lock_placements_verified"])
        self.assertTrue(result["declared_local_root_roles_verified"])
        self.assertFalse(result["optional_omissions_qualified"])
        self.assertTrue(all(result[key] is False for key in observer.FALSE_CLAIMS))
        self.assertEqual({row["scope"] for row in result["unqualified_auxiliary_entries"]}, {"bin", "metadata"})
        self.assertIn("node_modules/app/node_modules/leaf", {row["path"] for row in result["placements"]})
        self.assertNotIn("node_modules/app/test/fixtures/node_modules/foreign",
                         {row["path"] for row in result["placements"]})

    def test_all_archive_payload_kinds_require_exact_bytes(self):
        for relative in ("LICENSE", "native.wasm", "bin/task.js", "package.json",
                         "test/fixtures/node_modules/foreign/package.json"):
            with self.subTest(relative=relative):
                fixture = self.fixture()
                path = fixture.installed / "node_modules/app" / relative
                if relative == "package.json":
                    manifest = json.loads(path.read_bytes())
                    manifest["authoredChanged"] = True
                    path.write_bytes(json.dumps(manifest).encode())
                else:
                    path.write_bytes(b"authored changed bytes")
                with self.assertRaisesRegex(ValueError, "published member differs|installed package manifest differs"):
                    self.observe(fixture)

    def test_missing_archive_member_and_directory_are_not_waived(self):
        for relative in ("native.wasm", "nested/empty"):
            with self.subTest(relative=relative):
                fixture = self.fixture()
                path = fixture.installed / "node_modules/app" / relative
                if path.is_dir():
                    path.rmdir()
                else:
                    path.unlink()
                with self.assertRaisesRegex(ValueError, "missing ordinary entry"):
                    self.observe(fixture)

    def test_unexplained_noninstallation_payload_and_unknown_package_reject(self):
        for relative in ("src/unexplained", "node_modules/app/unexplained",
                         "node_modules/app/test/fixtures/node_modules/unexplained"):
            with self.subTest(relative=relative):
                fixture = self.fixture()
                (fixture.installed / relative).write_bytes(b"unexplained authored bytes")
                with self.assertRaisesRegex(ValueError, "unexplained payload entry"):
                    self.observe(fixture)
        fixture = self.fixture()
        foreign = fixture.installed / "node_modules/foreign"
        foreign.mkdir()
        (foreign / "package.json").write_bytes(b'{"name":"foreign","version":"1.0.0"}')
        with self.assertRaisesRegex(ValueError, "unknown installed package identity"):
            self.observe(fixture)

    def test_archive_owned_root_nm_slot_is_not_a_placement_or_bypass(self):
        fixture = self.fixture(archive_owned_nm=True)
        with self.assertRaisesRegex(ValueError, "nearer required placement differs: node_modules/app/node_modules/leaf"):
            self.observe(fixture)

    def test_required_root_cannot_exist_only_under_unrelated_package(self):
        fixture = self.fixture()
        shutil.rmtree(fixture.installed / "node_modules/leaf")
        self.package(fixture, ("leaf", "1.0.0"), fixture.installed / "node_modules/helper/node_modules/leaf")
        with self.assertRaisesRegex(ValueError, "missing required placement: leaf"):
            self.observe(fixture)

    def test_nearer_wrong_identity_cannot_fall_back_to_farther_right_identity(self):
        fixture = self.fixture()
        shutil.rmtree(fixture.installed / "node_modules/app/node_modules/leaf")
        self.package(fixture, ("leaf", "1.0.0"), fixture.installed / "node_modules/app/node_modules/leaf")
        self.package(fixture, ("leaf", "2.0.0"), fixture.installed / "node_modules/helper/node_modules/leaf")
        with self.assertRaisesRegex(ValueError, "nearer required placement differs: node_modules/app/node_modules/leaf"):
            self.observe(fixture)

    def test_matching_archive_retains_all_distinct_stanza_edge_obligations(self):
        fixture = self.fixture(repeated_archive=True)
        result = self.observe(fixture)
        app_indices = {stanza["index"] for stanza in fixture.report["pin_graph"]["stanzas"]
                       if any(row["value"].startswith("app@") for row in stanza["selectors"])}
        contexts = {row["context"] for row in result["mandatory_bindings"]
                    if row["issuer"] == "node_modules/app"}
        self.assertTrue({f"stanza:{index}" for index in app_indices}.issubset(contexts))
        shutil.rmtree(fixture.installed / "node_modules/hidden")
        with self.assertRaisesRegex(ValueError, "missing required placement: hidden"):
            self.observe(fixture)

    def test_optional_absence_is_enumerated_but_not_qualified(self):
        fixture = self.fixture()
        shutil.rmtree(fixture.installed / "node_modules/@scope/maybe")
        shutil.rmtree(fixture.installed / "node_modules/helper")
        result = self.observe(fixture)
        self.assertEqual({row["name"] for row in result["omitted_registry_identities"]}, {"@scope/maybe", "helper"})
        self.assertFalse(result["optional_omissions_qualified"])
        self.assertFalse(result["optional_placement_policy_verified"])

    def test_observed_optional_parent_still_requires_its_nonoptional_edge(self):
        fixture = self.fixture()
        shutil.rmtree(fixture.installed / "node_modules/helper")
        with self.assertRaisesRegex(ValueError, "missing required placement: helper"):
            self.observe(fixture)

    def test_declared_local_selectors_bind_one_exact_root_role(self):
        fixture = self.fixture()
        result = self.observe(fixture)
        associations = fixture.report["pin_graph"]["local_associations"]
        joined = {row["target_stanza"] for row in result["mandatory_bindings"]
                  if row["context"] == "declared_producer_role"}
        self.assertEqual(joined, {row["stanza_index"] for row in associations})
        self.assertEqual(result["observed_local_link_count"], 1)
        link = next(iter(fixture.links))
        fixture.links[link] = (fixture.source, fixture.links[link][1])
        with self.assertRaisesRegex(ValueError, "producer-role link target differs"):
            self.observe(fixture)

    def test_link_chain_alias_dangling_target_and_unknown_reparse_fail_closed(self):
        for variant in ("alias", "dangling", "unsupported"):
            with self.subTest(variant=variant):
                fixture = self.fixture()
                link = next(iter(fixture.links))
                target, tag = fixture.links[link]
                if variant == "alias":
                    alias = fixture.installed.parent / "alias"
                    alias.mkdir()
                    fixture.links[alias] = (fixture.installed, tag)
                    target = alias
                elif variant == "dangling":
                    target = fixture.installed.parent / "absent"
                else:
                    tag = 0xDEADBEEF
                fixture.links[link] = (target, tag)
                with self.assertRaisesRegex(ValueError, "producer-role link target differs|unsupported producer-role reparse"):
                    self.observe(fixture)

    def test_arbitrary_link_and_copied_producer_manifest_are_not_root_roles(self):
        fixture = self.fixture()
        app = fixture.installed / "node_modules/app"
        fixture.links[app] = (fixture.installed, getattr(stat, "IO_REPARSE_TAG_MOUNT_POINT", 0xA0000003))
        with self.assertRaisesRegex(ValueError, "undeclared package link"):
            self.observe(fixture)
        fixture = self.fixture()
        link = next(iter(fixture.links))
        fixture.links.clear()
        (link / "package.json").write_bytes((fixture.tree / "package.json").read_bytes())
        with self.assertRaisesRegex(ValueError, "unknown installed package identity"):
            self.observe(fixture)

    def test_published_payload_link_is_rejected_without_reading_target(self):
        fixture = self.fixture()
        payload = fixture.installed / "node_modules/app/LICENSE"
        target = fixture.installed.parent / "never-read"
        target.write_bytes(b"authored target bytes must not be read")
        fixture.links[payload] = (target, getattr(stat, "IO_REPARSE_TAG_MOUNT_POINT", 0xA0000003))
        original = observer.yarn.producer.read_bytes

        def read(path, maximum):
            self.assertNotEqual(path, target)
            return original(path, maximum)

        with patch.object(observer.yarn.producer, "read_bytes", side_effect=read):
            with self.assertRaisesRegex(ValueError, "linked or nonordinary entry"):
                self.observe(fixture)

    def test_auxiliary_bytes_are_bounded_unqualified_and_not_executed(self):
        fixture = self.fixture()
        result = self.observe(fixture)
        row = next(row for row in result["unqualified_auxiliary_entries"] if row["path"].endswith("authored.cmd"))
        payload = (fixture.installed / row["path"]).read_bytes()
        self.assertEqual(row["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertFalse(result["generated_bin_policy_verified"])
        with patch.object(observer, "MAX_AUXILIARY_ENTRIES", 1):
            with self.assertRaisesRegex(ValueError, "unqualified auxiliary entry limit"):
                self.observe(fixture)

    def test_observation_entry_depth_path_and_binding_caps_fail_closed(self):
        fixture = self.fixture()
        for name, value, message in (("MAX_ENTRIES", 1, "observation entry limit"),
                                     ("MAX_DEPTH", 1, "observation depth limit"),
                                     ("MAX_PATH_CHARACTERS", 1, "observation path limit"),
                                     ("MAX_BINDINGS", 1, "mandatory binding limit"),
                                     ("MAX_PLACEMENTS", 1, "placement limit")):
            with self.subTest(name=name), patch.object(observer, name, value):
                with self.assertRaisesRegex(ValueError, message):
                    self.observe(fixture)

    def test_failure_also_reauthenticates_once_and_preserves_inputs(self):
        fixture = self.fixture()
        (fixture.installed / "node_modules/app/LICENSE").write_bytes(b"authored differing bytes")
        before = self.frozen(fixture)
        metadata, readlink = self.link_metadata(fixture)
        with metadata, readlink, \
             patch.object(observer.yarn, "_inputs", side_effect=lambda project: self.inputs(fixture, project)), \
             patch.object(observer.yarn, "verify_bundle", wraps=observer.yarn.verify_bundle) as strict:
            with self.assertRaisesRegex(ValueError, "published member differs"):
                observer.observe(fixture.source, fixture.installed, helper_root=self.helper_root)
            self.assertEqual(strict.call_count, 2)
        self.assertEqual(before, self.frozen(fixture))

    def test_installed_inventory_drift_at_final_boundary_rejects(self):
        fixture = self.fixture()
        original = observer._Observer.run

        def run(value):
            result = original(value)
            (fixture.installed / "node_modules/app/LICENSE").write_bytes(b"changed after observation")
            return result

        with patch.object(observer._Observer, "run", run):
            with self.assertRaisesRegex(ValueError, "installed inventory changed"):
                self.observe(fixture)

    def test_source_drift_at_final_boundary_rejects(self):
        fixture = self.fixture()
        original = observer._Observer.run

        def run(value):
            result = original(value)
            manifest = json.loads((fixture.tree / "package.json").read_bytes())
            manifest["authoredChanged"] = True
            (fixture.tree / "package.json").write_bytes(json.dumps(manifest).encode())
            return result

        with patch.object(observer._Observer, "run", run):
            with self.assertRaisesRegex(ValueError, "canonical_report_differs"):
                self.observe(fixture)

    def test_helper_origin_mismatch_and_overlapping_roots_reject_before_authentication(self):
        fixture = self.fixture()
        with patch.object(observer.yarn, "verify_bundle", side_effect=AssertionError("must not authenticate")):
            with self.assertRaisesRegex(ValueError, "roots overlap"):
                observer.observe(fixture.source, fixture.source, helper_root=self.helper_root)
            with self.assertRaisesRegex(ValueError, "helper resolved outside declared root"):
                observer.observe(fixture.source, fixture.installed, helper_root=fixture.tree)

    def test_malformed_manifest_shapes_use_typed_failure_not_traceback(self):
        for contents in ([], {"name": [], "version": "1.0.0"}, {"name": "app", "version": {}}):
            with self.subTest(contents=contents):
                fixture = self.fixture()
                (fixture.installed / "node_modules/app/package.json").write_bytes(json.dumps(contents).encode())
                with self.assertRaisesRegex(ValueError, "invalid installed package manifest shape"):
                    self.observe(fixture)

    def test_local_role_insertion_is_charged_against_placement_bound(self):
        fixture = self.fixture()
        original = observer._Observer._local_link

        def local_link(value, path, name, metadata):
            with patch.object(observer, "MAX_PLACEMENTS", len(value.placements)):
                original(value, path, name, metadata)

        with patch.object(observer._Observer, "_local_link", local_link):
            with self.assertRaisesRegex(ValueError, "placement limit"):
                self.observe(fixture)

    def test_wide_directory_is_bounded_before_complete_materialization(self):
        fixture = self.fixture()
        original = os.scandir
        consumed = []

        class Entries:
            def __enter__(self):
                return iter(self)

            def __exit__(self, *args):
                return False

            def __iter__(self):
                for index in range(100):
                    consumed.append(index)
                    yield SimpleNamespace(path=str(fixture.installed / f"synthetic-{index}"))

        def scandir(path):
            return Entries() if Path(path) == fixture.installed else original(path)

        with patch.object(observer, "MAX_ENTRIES", 3), patch.object(os, "scandir", scandir):
            with self.assertRaisesRegex(ValueError, "observation entry limit"):
                self.observe(fixture)
        self.assertEqual(consumed, [0, 1, 2])

    def test_cli_emits_narrow_json_without_writes_or_children(self):
        fixture = self.fixture()
        before = self.frozen(fixture)
        output, errors = io.StringIO(), io.StringIO()
        metadata, readlink = self.link_metadata(fixture)
        with metadata, readlink, redirect_stdout(output), redirect_stderr(errors), \
             patch.object(observer.yarn, "_inputs", side_effect=lambda project: self.inputs(fixture, project)), \
             patch("subprocess.Popen", side_effect=AssertionError("observer must not spawn a child")):
            status = observer.main(["--source-root", str(fixture.source),
                                    "--producer-root", str(fixture.installed),
                                    "--helper-root", str(self.helper_root)])
        self.assertEqual(status, 0)
        self.assertEqual(errors.getvalue(), "")
        self.assertFalse(json.loads(output.getvalue())["whole_tree_acceptance_verified"])
        self.assertEqual(before, self.frozen(fixture))


if __name__ == "__main__":
    unittest.main()

