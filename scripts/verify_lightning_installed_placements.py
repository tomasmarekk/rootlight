#!/usr/bin/env python3
"""Observe authenticated published package bytes and mandatory lock placements.

Generated bins, metadata, optional selection and runtime/build behavior remain
unqualified. This read-only observer never invokes an installer or package code.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat
import sys

import verify_lightning_yarn_sources as yarn
import verify_lightning_security_sources as security


MAX_ENTRIES = yarn.MAX_TOTAL_MEMBERS
MAX_PLACEMENTS = yarn.yarn.MAX_STANZAS
MAX_BINDINGS = yarn.yarn.MAX_EDGES
MAX_DEPTH = 128
MAX_PATH_CHARACTERS = 4096
MAX_AUXILIARY_ENTRIES = yarn.MAX_MEMBERS
FALSE_CLAIMS = (*yarn.FALSE_CLAIMS, "installed_tree_identity_verified",
                "whole_tree_acceptance_verified", "generated_bin_policy_verified",
                "auxiliary_metadata_verified", "optional_placement_policy_verified",
                "peer_placement_policy_verified", "posix_modes_verified")


class PlacementObservationError(ValueError):
    """A byte, ordinary-entry, mandatory-binding or conservation check failed."""


def _require(condition, message):
    if not condition:
        raise PlacementObservationError(message)


def _stamp(metadata):
    # Directory allocation size is unstable on Windows even without byte changes.
    # Exact child inventories, file sizes and all remaining identity fields stay bound.
    size = 0 if stat.S_ISDIR(metadata.st_mode) else metadata.st_size
    return (metadata.st_dev, metadata.st_ino, metadata.st_mode, size,
            metadata.st_mtime_ns, metadata.st_ctime_ns,
            getattr(metadata, "st_file_attributes", 0), getattr(metadata, "st_reparse_tag", 0))


def _ancestor_ids(root):
    rows = []
    for path in (root, *root.parents):
        metadata = path.lstat()
        _require(stat.S_ISDIR(metadata.st_mode) and not yarn.original._is_link(metadata),
                 "linked or non-directory observation ancestor")
        rows.append((path, metadata.st_dev, metadata.st_ino, metadata.st_mode,
                     getattr(metadata, "st_file_attributes", 0)))
    return rows


def _projection(proof):
    root = proof["archive_root"]
    files, directories = {}, {"."}
    for member in proof["members"]:
        parts = PurePosixPath(member["path"]).parts
        _require(parts and parts[0] == root, "member archive-root mismatch")
        relative = PurePosixPath(*parts[1:]).as_posix()
        if relative != ".":
            yarn.original._safe_path(relative, "projected archive member")
            directories.update(str(parent) for parent in PurePosixPath(relative).parents)
        if member["kind"] == "directory":
            directories.add(relative)
        else:
            _require(member["kind"] == "file" and relative != "." and relative not in files,
                     "unsupported or duplicate projected member")
            files[relative] = member
    _require(not files.keys() & directories, "projected file/directory collision")
    return files, directories


def _helper_rows(helper_root):
    modules = (yarn, security, yarn.yarn, yarn.producer, yarn.original,
               yarn.producer.local, yarn.producer.local.fixture)
    _require(yarn.original is yarn.producer.local.original, "mixed npm verifier modules")
    rows = []
    for module in modules:
        path = yarn.producer.safe_path(Path(module.__file__), directory=False)
        _require(path == helper_root / (module.__name__ + ".py"), "helper resolved outside declared root")
        payload = yarn.producer.read_bytes(path, yarn.MAX_EXPANDED)
        rows.append({"path": path.name, "sha256": hashlib.sha256(payload).hexdigest()})
    return rows


class _Observer:
    def __init__(self, root, report, producer_report, manifest):
        self.root, self.report, self.graph = root, report, report["pin_graph"]
        self.ancestors = _ancestor_ids(root)
        self.baseline = self._snapshot()
        self.payload_files, self.auxiliary, self.placements, self.links = {}, [], {}, {}
        self.bindings = []
        self.by_identity = {}
        for package in report["packages"]:
            key = (package["pin"]["name"], package["pin"]["version"])
            _require(key not in self.by_identity, "duplicate authenticated archive identity")
            self.by_identity[key] = {"package": package, "stanzas": []}
        self.targets = {}
        self.locals = {row["stanza_index"]: row for row in self.graph["local_associations"]}
        for stanza in self.graph["stanzas"]:
            if stanza["distribution_kind"] == "local_manifest_resolution":
                association = self.locals.get(stanza["index"])
                _require(association is not None and association["name"] == manifest["name"]
                         and association["manifest_version"] == manifest["version"]
                         and association["resolution"] == "link:."
                         and manifest.get("resolutions", {}).get(manifest["name"]) == "link:.",
                         "undeclared producer-role association")
                self.targets[stanza["index"]] = ("local", manifest["name"])
            else:
                fields = stanza["fields"]
                candidates = [key for key, model in self.by_identity.items()
                              if model["package"]["pin"]["resolved"] == fields["resolved"]["value"]]
                _require(len(candidates) == 1, "stanza/archive identity join differs")
                key = candidates[0]
                pin = self.by_identity[key]["package"]["pin"]
                _require(fields["version"]["value"] == pin["version"]
                         and fields["integrity"]["value"] == pin["integrity"], "stanza/archive pin differs")
                self.by_identity[key]["stanzas"].append(stanza["index"])
                self.targets[stanza["index"]] = ("registry", *key)
        _require(all(model["stanzas"] for model in self.by_identity.values()),
                 "authenticated archive has no stanza")
        self.local_name = manifest["name"] if self.locals else None
        self.producer_proof = producer_report["source_inventory"]
        self.required = set()
        pending = [row["target_stanza"] for row in self.graph["roots"]
                   if row["section"] not in ("optionalDependencies", "resolutions")]
        while pending:
            index = pending.pop()
            if index not in self.required:
                self.required.add(index)
                pending.extend(edge["target_stanza"] for edge in self.graph["stanzas"][index]["edges"]
                               if edge["section"] == "dependencies")

    def _relative(self, path):
        relative = path.relative_to(self.root).as_posix()
        _require(len(relative) <= MAX_PATH_CHARACTERS, "observation path limit")
        return relative

    def _snapshot(self):
        rows, aliases, pending = {}, {}, [(self.root, 0)]
        total = 0
        while pending:
            path, depth = pending.pop()
            _require(depth <= MAX_DEPTH, "observation depth limit")
            relative = self._relative(path)
            if relative != ".":
                yarn.original._safe_path(relative, "observed entry")
                yarn.original._path_aliases(relative, aliases)
            metadata = path.lstat()
            linked = yarn.original._is_link(metadata)
            target = None
            if stat.S_ISLNK(metadata.st_mode) or (
                linked and getattr(metadata, "st_reparse_tag", None)
                == getattr(stat, "IO_REPARSE_TAG_MOUNT_POINT", 0xA0000003)
            ):
                target = os.readlink(path)
            rows[relative] = (_stamp(metadata), target)
            _require(len(rows) <= MAX_ENTRIES, "observation entry limit")
            if stat.S_ISREG(metadata.st_mode) and not linked:
                _require(metadata.st_size <= yarn.MAX_EXPANDED, "observation file-byte limit")
                total += metadata.st_size
                _require(total <= yarn.MAX_TOTAL_EXPANDED, "observation total-byte limit")
            if stat.S_ISDIR(metadata.st_mode) and not linked:
                with os.scandir(path) as entries:
                    for entry in entries:
                        _require(len(rows) + len(pending) < MAX_ENTRIES, "observation entry limit")
                        pending.append((Path(entry.path), depth + 1))
        return rows

    def _children(self, path, *, maximum=None):
        category = "observation directory-entry limit" if maximum is None else "unqualified auxiliary entry limit"
        maximum = MAX_ENTRIES if maximum is None else maximum
        children = []
        with os.scandir(path) as entries:
            for entry in entries:
                _require(len(children) < maximum, category)
                child = Path(entry.path)
                _require(self._relative(child) in self.baseline, "installed inventory changed during observation")
                children.append(child)
        return sorted(children, key=lambda item: item.name)

    def _ordinary(self, path, *, directory):
        relative = self._relative(path)
        try:
            metadata = path.lstat()
        except FileNotFoundError as error:
            raise PlacementObservationError(f"missing ordinary entry: {relative}") from error
        correct = stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode)
        _require(correct and not yarn.original._is_link(metadata), f"linked or nonordinary entry: {relative}")
        _require(self.baseline.get(relative) == (_stamp(metadata), None),
                 f"entry changed during observation: {relative}")
        return metadata

    def _read(self, path, maximum):
        before = self._ordinary(path, directory=False)
        payload = yarn.producer.read_bytes(path, maximum)
        _require(_stamp(path.lstat()) == _stamp(before),
                 f"entry changed during read: {self._relative(path)}")
        return payload

    def _members(self, path, proof):
        files, directories = _projection(proof)
        for relative in sorted(directories):
            self._ordinary(path / relative, directory=True)
        for relative, member in sorted(files.items()):
            payload = self._read(path / relative, yarn.MAX_EXPANDED)
            _require(len(payload) == member["bytes"]
                     and hashlib.sha256(payload).hexdigest() == member["sha256"],
                     f"published member differs: {self._relative(path / relative)}")
            self.payload_files[self._relative(path / relative)] = {
                "bytes": len(payload), "sha256": member["sha256"]}
        self._payload_entries(path, path, files, directories)
        return len(files)

    def _payload_entries(self, owner, path, files, directories):
        children = self._children(path)
        for child in children:
            relative = child.relative_to(owner).as_posix()
            if relative == "node_modules" and relative not in files:
                self._ordinary(child, directory=True)
                self._node_modules(owner, child, files, directories)
            elif relative in files:
                self._ordinary(child, directory=False)
            elif relative in directories:
                self._ordinary(child, directory=True)
                self._payload_entries(owner, child, files, directories)
            else:
                raise PlacementObservationError(f"unexplained payload entry: {self._relative(child)}")

    def _owned_subtree(self, owner, path, files, directories):
        relative = path.relative_to(owner).as_posix()
        if relative in files:
            self._ordinary(path, directory=False)
            return
        _require(relative in directories, f"unexplained archive-owned entry: {self._relative(path)}")
        self._ordinary(path, directory=True)
        for child in self._children(path):
            self._owned_subtree(owner, child, files, directories)

    def _auxiliary(self, path, kind):
        pending = [path]
        while pending:
            current = pending.pop()
            metadata = current.lstat()
            relative = self._relative(current)
            _require(not yarn.original._is_link(metadata), f"linked unqualified auxiliary entry: {relative}")
            row = {"path": relative, "scope": kind}
            if stat.S_ISDIR(metadata.st_mode):
                self._ordinary(current, directory=True)
                row["kind"] = "directory"
                available = MAX_AUXILIARY_ENTRIES - len(self.auxiliary) - len(pending) - 1
                _require(available >= 0, "unqualified auxiliary entry limit")
                children = self._children(current, maximum=available)
                pending.extend(children)
            else:
                maximum = yarn.MAX_REPORT if kind == "metadata" else yarn.MAX_EXPANDED
                payload = self._read(current, maximum)
                row.update(kind="file", bytes=len(payload), sha256=hashlib.sha256(payload).hexdigest())
            self.auxiliary.append(row)
            _require(len(self.auxiliary) <= MAX_AUXILIARY_ENTRIES, "unqualified auxiliary entry limit")

    def _local_link(self, path, name, metadata):
        _require(self.local_name == name, f"undeclared package link: {self._relative(path)}")
        _require(stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_reparse_tag", None)
                 == getattr(stat, "IO_REPARSE_TAG_MOUNT_POINT", 0xA0000003),
                 f"unsupported producer-role reparse entry: {self._relative(path)}")
        raw = os.readlink(path)
        if os.name == "nt":
            if raw.startswith("\\\\?\\UNC\\"):
                raw = "\\\\" + raw[8:]
            elif raw.startswith("\\\\?\\") or raw.startswith("\\??\\"):
                raw = raw[4:]
        target = Path(raw)
        _require(target.is_absolute() and ".." not in target.parts and target == self.root,
                 f"producer-role link target differs: {self._relative(path)}")
        _require(_ancestor_ids(self.root) == self.ancestors, "producer-root ancestor identity changed")
        _require(self.baseline.get(self._relative(path)) == (_stamp(metadata), os.readlink(path)),
                 "producer-role link changed")
        placement = self._relative(path)
        _require(placement not in self.placements and len(self.placements) < MAX_PLACEMENTS,
                 "duplicate placement or placement limit")
        self.links[placement] = {"name": name, "target_role": "authored_producer"}
        self.placements[placement] = {"kind": "local", "name": name}

    def _slot(self, owner, path, name, files, directories):
        relative = path.relative_to(owner).as_posix()
        if relative in files or relative in directories:
            self._owned_subtree(owner, path, files, directories)
            return
        yarn.yarn._name(name)
        metadata = path.lstat()
        if yarn.original._is_link(metadata):
            self._local_link(path, name, metadata)
            return
        self._ordinary(path, directory=True)
        payload = self._read(path / "package.json", yarn.MAX_MANIFEST)
        try:
            manifest = json.loads(payload)
        except (ValueError, RecursionError) as error:
            raise PlacementObservationError("invalid installed package manifest") from error
        _require(isinstance(manifest, dict) and isinstance(manifest.get("name"), str)
                 and isinstance(manifest.get("version"), str), "invalid installed package manifest shape")
        key = (manifest.get("name"), manifest.get("version"))
        _require(key[0] == name and key in self.by_identity,
                 f"unknown installed package identity: {self._relative(path)}")
        package = self.by_identity[key]["package"]
        _require(hashlib.sha256(payload).hexdigest() == package["published_package"]["package_json_sha256"],
                 f"installed package manifest differs: {self._relative(path)}")
        placement = self._relative(path)
        _require(placement not in self.placements and len(self.placements) < MAX_PLACEMENTS,
                 "duplicate placement or placement limit")
        self.placements[placement] = {"kind": "registry", "name": key[0], "version": key[1],
                                      "archive_sha256": package["published_package"]["archive"]["sha256"],
                                      "members_sha256": package["published_package"]["members_sha256"]}
        self._members(path, package["published_package"])

    def _node_modules(self, owner, path, files, directories):
        self._ordinary(path, directory=True)
        children = self._children(path)
        for child in children:
            relative = child.relative_to(owner).as_posix()
            if relative in files:
                self._owned_subtree(owner, child, files, directories)
            elif child.name == ".bin":
                if relative in directories:
                    self._owned_subtree(owner, child, files, directories)
                else:
                    self._ordinary(child, directory=True)
                    self._auxiliary(child, "bin")
            elif child.name == ".yarn-integrity" and path == self.root / "node_modules":
                if relative in directories:
                    self._owned_subtree(owner, child, files, directories)
                else:
                    self._ordinary(child, directory=False)
                    self._auxiliary(child, "metadata")
            elif child.name.startswith("@"):
                yarn.yarn._name(child.name + "/scope-validation")
                self._ordinary(child, directory=True)
                scoped = self._children(child)
                for package in scoped:
                    self._slot(owner, package, child.name + "/" + package.name, files, directories)
            else:
                self._slot(owner, child, child.name, files, directories)

    def _lookup(self, issuer, name, stanza_index, context):
        expected = self.targets[stanza_index]
        current = issuer
        while True:
            if current.name != "node_modules":
                candidate = current / "node_modules" / name
                try:
                    candidate.lstat()
                except FileNotFoundError:
                    pass
                else:
                    placement = self.placements.get(self._relative(candidate))
                    actual = (("local", placement["name"]) if placement and placement["kind"] == "local"
                              else ("registry", placement["name"], placement["version"]) if placement else None)
                    _require(actual == expected, f"nearer required placement differs: {self._relative(candidate)}")
                    self.bindings.append({"issuer": self._relative(issuer), "name": name,
                                          "target_stanza": stanza_index, "placement": self._relative(candidate),
                                          "context": context})
                    _require(len(self.bindings) <= MAX_BINDINGS, "mandatory binding limit")
                    return
            if current == self.root:
                break
            current = current.parent
        raise PlacementObservationError(f"missing required placement: {name}")

    def run(self):
        self._members(self.root, self.producer_proof)
        for root in self.graph["roots"]:
            if root["section"] not in ("optionalDependencies", "resolutions"):
                self._lookup(self.root, root["name"], root["target_stanza"], "ordinary_root")
        for relative, placement in sorted(self.placements.items()):
            if placement["kind"] != "registry":
                continue
            model = self.by_identity[(placement["name"], placement["version"])]
            # An archive pin does not incorporate stanza edges; retain every matching stanza.
            for index in model["stanzas"]:
                for edge in self.graph["stanzas"][index]["edges"]:
                    if edge["section"] == "dependencies":
                        self._lookup(self.root / relative, edge["name"]["value"], edge["target_stanza"],
                                     f"stanza:{index}")
        for index, association in sorted(self.locals.items()):
            self._lookup(self.root, association["name"], index, "declared_producer_role")
        identities = {(row["name"], row["version"]) for row in self.placements.values()
                      if row["kind"] == "registry"}
        required = {self.targets[index][1:] for index in self.required
                    if self.targets[index][0] == "registry"}
        _require(required.issubset(identities), "globally required archive identity absent")
        observations = [{"path": path, "kind": "file", **row}
                        for path, row in sorted(self.payload_files.items())]
        auxiliary_paths = {row["path"] for row in self.auxiliary}
        observations.extend({"path": path, "kind": "directory"}
                            for path, (stamp, _) in sorted(self.baseline.items())
                            if stat.S_ISDIR(stamp[2]) and path not in self.links
                            and path not in auxiliary_paths)
        observations.extend(sorted(self.auxiliary, key=lambda row: row["path"]))
        observations.extend({"path": path, **row} for path, row in sorted(self.links.items()))
        return {"schema": "rootlight.lightning-placement-observation/1",
                "source_report_sha256": hashlib.sha256(yarn.producer.json_bytes(self.report)).hexdigest(),
                "authenticated_archive_count": len(self.by_identity),
                "observed_registry_identity_count": len(identities),
                "observed_registry_placement_count": sum(row["kind"] == "registry"
                                                         for row in self.placements.values()),
                "globally_required_registry_identity_count": len(required),
                "globally_required_stanza_count": len(self.required),
                "declared_local_association_count": len(self.locals),
                "observed_local_link_count": len(self.links),
                "verified_payload_file_count": len(self.payload_files),
                "observed_entry_count": len(self.baseline),
                "observed_inventory_sha256": hashlib.sha256(yarn.producer.json_bytes(observations)).hexdigest(),
                "placements": [{"path": path, **row} for path, row in sorted(self.placements.items())],
                "mandatory_bindings": self.bindings,
                "omitted_registry_identities": [{"name": name, "version": version}
                                                for name, version in sorted(self.by_identity.keys() - identities)],
                "unqualified_auxiliary_entries": sorted(self.auxiliary, key=lambda row: row["path"]),
                "observed_published_package_bytes_verified": True,
                "required_nonoptional_lock_placements_verified": True,
                "declared_local_root_roles_verified": True,
                "optional_omissions_qualified": False,
                "stable_source_contract_not_hostile_concurrency": True,
                **{claim: False for claim in FALSE_CLAIMS}}

    def conserve(self):
        _require(_ancestor_ids(self.root) == self.ancestors, "producer-root ancestor identity changed")
        _require(self._snapshot() == self.baseline, "installed inventory changed during observation")


def observe(source_root: Path, producer_root: Path, *, helper_root: Path | None = None) -> dict:
    """Read exact package payloads and mandatory lock bindings without writes/children.

    Args:
        source_root: Complete authenticated producer/Yarn and npm association inputs.
        producer_root: Separately closed installed producer directory, disjoint from sources.
        helper_root: Exact imported first-party helper directory; defaults to source scripts.

    Returns:
        Bounded evidence for narrow byte/required-placement/local-role checks. Generated
        bins and metadata are enumerated unqualified; all global acceptance claims are false.

    Raises:
        ValueError: Source/helper identities, payloads, paths, bindings or conservation fail.
        OSError: A guarded read or traversal fails. No cache or tool executable is consulted.

    Requires stable nonhostile inputs. Final strict authentication runs once on success or
    failure, never once per placement. Optional omission, peer and runtime policy is unproved.
    """
    source_root = yarn.producer.safe_path(Path(source_root), directory=True)
    producer_root = yarn.producer.safe_path(Path(producer_root), directory=True)
    _require(not source_root.is_relative_to(producer_root)
             and not producer_root.is_relative_to(source_root), "source/installed roots overlap")
    helper_root = yarn.producer.safe_path(Path(helper_root) if helper_root else source_root / "scripts",
                                          directory=True)
    helpers = _helper_rows(helper_root)
    source_ancestors = _ancestor_ids(source_root)
    verified = security.verify_bundle(source_root)
    observer = None
    try:
        report_bytes = yarn.producer.read_bytes(source_root / yarn.PRODUCER_PATH / "source-tree.json",
                                               yarn.MAX_REPORT)
        _require(hashlib.sha256(report_bytes).hexdigest()
                 == verified["producer"]["source_tree_report"]["sha256"], "producer report changed")
        producer_report, overlays = security.effective_producer(source_root, verified, json.loads(report_bytes))
        manifest_bytes = (overlays["package.json"] if "package.json" in overlays else
                          yarn.producer.read_bytes(source_root / yarn.PRODUCER_PATH / "tree/package.json",
                                                   yarn.MAX_MANIFEST))
        observer = _Observer(producer_root, verified, producer_report, json.loads(manifest_bytes))
        result = observer.run()
    finally:
        try:
            _require(security.verify_bundle(source_root) == verified, "authenticated source inputs changed")
            _require(_helper_rows(helper_root) == helpers, "helper bytes changed")
            _require(_ancestor_ids(source_root) == source_ancestors, "source-root ancestor identity changed")
        finally:
            if observer is not None:
                observer.conserve()
    result["strict_source_reverification_completed"] = True
    result["helper_modules"] = helpers
    _require(len(yarn.producer.json_bytes(result)) <= yarn.MAX_REPORT, "observation report-byte limit")
    return result


def main(arguments: list[str] | None = None) -> int:
    """Print bounded read-only narrow evidence, or a failure without changing inputs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--producer-root", type=Path, required=True)
    parser.add_argument("--helper-root", type=Path)
    options = parser.parse_args(arguments)
    try:
        result = observe(options.source_root, options.producer_root, helper_root=options.helper_root)
    except (ValueError, OSError) as error:
        print(f"placement observation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
