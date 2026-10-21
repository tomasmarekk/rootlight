#!/usr/bin/env python3
"""Export versioned Rust, npm and producer inputs without acquisition or caches.

The tracked registry sources and original archives cover workspace, native-test,
and fuzz locks, plus the reviewed producer lock when its complete tree is present.
Local npm distributions retain declared fixture provenance;
Optional producer trees retain authored source and publisher association evidence;
their separate Yarn namespaces retain authenticated published distributions.
Export proves neither transitive preferred closure nor third-party authorship.
"""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import tarfile
import tomllib

from verify_cargo_source_bundle import verify_bundle
from verify_native_producer_sources import REVIEWED_PRODUCERS, native_source_inputs
from verify_lightning_producer_sources import (
    COMMIT as LIGHTNING_COMMIT, PACKAGE as LIGHTNING_PACKAGE, ROOT as LIGHTNING_ARCHIVE_ROOT,
    verify_bundle as verify_lightning_producer_bundle,
)
from verify_lightning_yarn_sources import (
    BUNDLE_PATH as LIGHTNING_YARN_PATH, REPORT_NAME as LIGHTNING_YARN_REPORT_NAME,
    verify_bundle as verify_lightning_yarn_bundle,
)
from verify_lightning_security_sources import (
    BUNDLE_PATH as LIGHTNING_NATIVE_PATH, REPORT_NAME as LIGHTNING_NATIVE_REPORT_NAME,
    verify_bundle as verify_lightning_native_bundle,
)
from verify_local_npm_sources import (
    CANONICAL_LOCK, LOCAL_DISTRIBUTIONS as NPM_LOCAL_DISTRIBUTIONS,
    ORIGINAL_ARCHIVES as NPM_ORIGINAL_ARCHIVES, verify_local_bundle,
)


LOCK_PATHS = ("Cargo.lock", "tests/native-grammars/Cargo.lock", "fuzz/Cargo.lock",
              "source-inputs/producers/flux-9eb12f7f4702875e79189789cf9b3ac560499c11/parser/Cargo.lock")
MANIFEST_PATH = "rust-source-bundle.json"
VENDOR_PATH = "source-inputs/cargo"
ARCHIVES_PATH = "source-inputs/crates"
CLOSURE_PATH = "source-inputs/cargo-source-closure.json"
NPM_REPORT_PATH = "source-inputs/npm/npm-source-closure.json"
PRODUCERS_PATH = "source-inputs/producers"
LIGHTNING_PRODUCER_PATH = f"{PRODUCERS_PATH}/{LIGHTNING_PACKAGE}-{LIGHTNING_COMMIT}"
LIGHTNING_LOCK_PATH = f"{LIGHTNING_PRODUCER_PATH}/tree/Cargo.lock"
PRODUCER_DEPENDENCIES_PATH = "source-inputs/producer-dependencies"
LIGHTNING_YARN_PRODUCER_PATH = f"{PRODUCER_DEPENDENCIES_PATH}/{LIGHTNING_PACKAGE}-{LIGHTNING_COMMIT}"
LIGHTNING_NATIVE_LOCK_PATH = f"{LIGHTNING_NATIVE_PATH}/overlays/Cargo.lock"
FRONTEND_PATHS = ("apps/rootlight-web/frontend/package.json", CANONICAL_LOCK,
                  "apps/rootlight-web/frontend/.npmrc")
VERIFICATION_SCRIPT_PATHS = (
    "scripts/build_lightning_native.py", "scripts/build_frontend_from_sources.py",
    "scripts/install_frontend_from_archives.py", "scripts/check_lightning_frontend_boundary.cjs",
    "scripts/install_lightning_from_archives.py",
    "scripts/verify_lightning_installed_placements.py",
    "scripts/lightning_yarn_pins.py", "scripts/patch_npm_source_fixture.py",
    "scripts/prepare_rust_source_bundle.py", "scripts/verify_cargo_source_bundle.py",
    "scripts/verify_lightning_producer_sources.py", "scripts/verify_lightning_yarn_sources.py",
    "scripts/verify_lightning_security_sources.py",
    "scripts/verify_local_npm_sources.py", "scripts/verify_npm_source_bundle.py",
    "scripts/verify_native_producer_sources.py",
)
BUNDLE_SCOPE = "rust_workspace_native_grammars_fuzz_and_local_npm_inputs"
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
WINDOWS_DEVICES = {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"} | {
    f"{prefix}{number}" for prefix in ("COM", "LPT") for number in range(1, 10)
}


def relative_path(value: str) -> Path:
    """Accept only portable project-relative paths, never escaping the bundle."""
    path = PurePosixPath(value)
    if (
        not value or path.is_absolute() or "\\" in value or ":" in value
        or any(part in ("", ".", "..") or part.endswith((".", " "))
               or part.split(".")[0].upper() in WINDOWS_DEVICES for part in value.split("/"))
        or any(ord(character) < 32 or ord(character) == 127 for character in value)
    ):
        raise ValueError(f"unsafe source path: {value!r}")
    return Path(*path.parts)


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def linked_path(path: Path) -> bool:
    """Treat Windows junctions/reparse points as links, not ordinary directories."""
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return False
    return stat.S_ISLNK(metadata.st_mode) or bool(
        getattr(metadata, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    )


def registry_inputs(root: Path, *, lock_paths: tuple[str, ...] = LOCK_PATHS) -> dict[tuple[str, str], str]:
    """Read immutable archive identities without executing dependency code."""
    if (not isinstance(lock_paths, (list, tuple)) or not lock_paths
        or any(not isinstance(name, str) for name in lock_paths)
        or len(set(lock_paths)) != len(lock_paths)):
        raise ValueError("lock paths must be a nonempty distinct project-relative list")
    packages = {}
    for lock in lock_paths:
        for package in tomllib.loads(regular_source(root, lock).read_text(encoding="utf-8"))["package"]:
            if "source" not in package:
                continue
            if package["source"] != REGISTRY:
                raise ValueError("unsupported source registry in locked inputs")
            name, version, checksum = package["name"], package["version"], package["checksum"]
            if (not re.fullmatch(r"[A-Za-z0-9_-]+", name)
                or not re.fullmatch(r"[0-9A-Za-z.+-]+", version)
                or not re.fullmatch(r"[0-9a-f]{64}", checksum)):
                raise ValueError("invalid locked archive identity")
            identity = (name, version)
            if identity in packages and packages[identity] != checksum:
                raise ValueError(f"conflicting locked checksum: {name} {version}")
            packages[identity] = checksum
    return dict(sorted(packages.items()))


def materialize_archive(archive: Path, vendor: Path, name: str, version: str,
                        checksum: str) -> dict[str, int]:
    """Derive directory sources only from lock-authenticated original archives."""
    if digest(archive) != checksum:
        raise ValueError(f"archive checksum differs from lockfile: {name} {version}")
    prefix = f"{name}-{version}"
    package_root = vendor / prefix
    package_root.mkdir(parents=True)
    hashes, modes = {}, {}
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar.getmembers():
            relative_path(member.name.rstrip("/"))
            if member.name.rstrip("/") == prefix and member.isdir():
                continue
            if not member.name.startswith(prefix + "/"):
                raise ValueError(f"archive member outside package root: {member.name}")
            name_in_package = member.name[len(prefix) + 1:].rstrip("/")
            relative_path(name_in_package)
            if member.isdir():
                continue
            if member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE) or name_in_package in hashes:
                raise ValueError(f"linked, special, or duplicate archive member: {member.name}")
            if member.mode < 0 or member.mode & ~(0o7777 | stat.S_IFREG):
                raise ValueError(f"invalid archive member mode: {member.name}")
            if name_in_package == ".cargo-checksum.json":
                raise ValueError("archive contains reserved Cargo checksum metadata")
            target = package_root / relative_path(name_in_package)
            target.parent.mkdir(parents=True, exist_ok=True)
            source = tar.extractfile(member)
            if source is None:
                raise ValueError(f"unreadable archive member: {member.name}")
            with source, target.open("xb") as output:
                while chunk := source.read(1024 * 1024):
                    output.write(chunk)
            if target.stat().st_size != member.size:
                raise ValueError(f"incomplete archive member: {member.name}")
            executable = bool(member.mode & 0o111)
            target.chmod(0o755 if executable else 0o644)
            hashes[name_in_package] = digest(target)
            modes[f"{VENDOR_PATH}/{prefix}/{name_in_package}"] = 0o755 if executable else 0o644
    checksum_file = package_root / ".cargo-checksum.json"
    checksum_file.write_text(json.dumps({"files": hashes, "package": checksum}, sort_keys=True)
                             + "\n", encoding="utf-8", newline="\n")
    modes[f"{VENDOR_PATH}/{prefix}/.cargo-checksum.json"] = 0o644
    return modes


def bundle_cargo_config(original: str) -> str:
    """Validate the declared offline directory mapping without rewriting bytes.

    Args:
        original: Tracked Cargo configuration text; unrelated tables are retained.

    Returns:
        The unchanged text only when crates.io maps exactly to the project-local
        source directory and net.offline is true.

    Raises:
        ValueError: The configuration is malformed, missing the local mapping,
            permits online resolution, or declares any other source mapping.
    """
    parsed = tomllib.loads(original)
    expected = {
        "crates-io": {"replace-with": "rootlight-local-sources"},
        "rootlight-local-sources": {"directory": VENDOR_PATH},
    }
    if parsed.get("source") != expected or parsed.get("net", {}).get("offline") is not True:
        raise ValueError("Cargo configuration must declare only the project-local source and offline=true")
    return original


def _cargo_file_modes(closure: dict) -> dict[str, int]:
    modes = {}
    for package in closure["packages"]:
        prefix = f"{VENDOR_PATH}/{package['vendor_path']}"
        executable = set(package["archive_executable_files"])
        for record in package["files"]:
            modes[f"{prefix}/{record['path']}"] = 0o755 if record["path"] in executable else 0o644
        modes[f"{prefix}/.cargo-checksum.json"] = 0o644
        modes[f"{ARCHIVES_PATH}/{package['name']}-{package['version']}.crate"] = 0o644
    return dict(sorted(modes.items()))


def _versioned_closure(root: Path, *, lock_paths: tuple[str, ...] = LOCK_PATHS) -> tuple[dict, dict[str, int]]:
    locks = [regular_source(root, name) for name in lock_paths]
    closure = verify_bundle(root / VENDOR_PATH, locks, archives=root / ARCHIVES_PATH)
    modes = _cargo_file_modes(closure)
    report = {**closure, "file_modes": modes,
              "frontend_external_source_closure_verified": False,
              "build_and_packaging_acceptance_verified": False}
    expected = (json.dumps(report, sort_keys=True, indent=2) + "\n").encode("utf-8")
    if regular_source(root, CLOSURE_PATH).read_bytes() != expected:
        raise ValueError("versioned Cargo dependency report differs from verified source inputs")
    return closure, modes


def _npm_distribution_inputs(root: Path) -> tuple[dict, dict[str, int], dict[str, str]]:
    report = verify_local_bundle(root)
    encoded = (json.dumps(report, sort_keys=True, indent=2, allow_nan=False) + "\n").encode("utf-8")
    if regular_source(root, NPM_REPORT_PATH).read_bytes() != encoded:
        raise ValueError("versioned npm distribution report differs from verified inputs")
    hashes = {record["path"]: record["sha256"] for record in report["input_files"]}
    hashes[NPM_REPORT_PATH] = hashlib.sha256(encoded).hexdigest()
    for name in FRONTEND_PATHS:
        actual = digest(regular_source(root, name))
        if name in hashes and hashes[name] != actual:
            raise ValueError("canonical frontend source changed during verification")
        hashes[name] = actual
    # Compressed inputs and their provenance are data, not executable package members.
    modes = {name: 0o644 for name in hashes if name.startswith("source-inputs/npm/")}
    return report, modes, hashes


def _producer_source_inputs(root: Path) -> tuple[dict | None, dict[str, int], dict[str, str], set[str]]:
    """Bind an optional complete producer namespace to its offline verifier.

    Only complete absence supports older Rust/npm-only exports. A present parent
    admits only Lightning and the separately pinned Flux/Raffia source catalogs.
    Authored directories, including
    empty ones, are source inventory; their raw modes remain report metadata,
    not a guarantee of restored directory permissions or binary reproduction.
    """
    parent = root / PRODUCERS_PATH
    if any(linked_path(path) for path in (parent, *parent.parents)):
        raise ValueError("linked producer source namespace")
    if not parent.exists():
        return None, {}, {}, set()
    names = {entry.name for entry in parent.iterdir()} if parent.is_dir() else set()
    lightning = PurePosixPath(LIGHTNING_PRODUCER_PATH).name
    if not parent.is_dir() or not names or names - {lightning, *REVIEWED_PRODUCERS}:
        raise ValueError("producer namespace inventory differs from reviewed inputs")
    native_modes, native_hashes, native_directories = native_source_inputs(root)
    if lightning not in names:
        return None, native_modes, native_hashes, native_directories
    report = verify_lightning_producer_bundle(root, root / LIGHTNING_PRODUCER_PATH)
    hashes = {f"{LIGHTNING_PRODUCER_PATH}/{name}": record["sha256"]
              for name, record in report["retained_inputs"].items()}
    encoded = (json.dumps(report, sort_keys=True, indent=2, allow_nan=False) + "\n").encode("utf-8")
    report_path = f"{LIGHTNING_PRODUCER_PATH}/source-tree.json"
    if regular_source(root, report_path).read_bytes() != encoded:
        raise ValueError("producer tree report differs from verified inputs")
    hashes[report_path] = hashlib.sha256(encoded).hexdigest()
    modes = {name: 0o644 for name in hashes}
    directories = {PRODUCERS_PATH, LIGHTNING_PRODUCER_PATH, f"{LIGHTNING_PRODUCER_PATH}/tree"}
    for record in report["source_inventory"]["members"]:
        if record["path"] == LIGHTNING_ARCHIVE_ROOT:
            continue
        relative = record["path"].removeprefix(LIGHTNING_ARCHIVE_ROOT + "/")
        name = f"{LIGHTNING_PRODUCER_PATH}/tree/{relative}"
        relative_path(name)
        if record["kind"] == "directory":
            directories.add(name)
        else:
            hashes[name] = record["sha256"]
            modes[name] = 0o755 if record["archive_mode"] & 0o111 else 0o644
    for name, checksum in hashes.items():
        if digest(regular_source(root, name)) != checksum:
            raise ValueError("producer source inputs changed before export")
        directories.update(str(path) for path in PurePosixPath(name).parents
                           if str(path).startswith(PRODUCERS_PATH + "/"))
    return (report, dict(sorted((modes | native_modes).items())),
            dict(sorted((hashes | native_hashes).items())), directories | native_directories)


def _yarn_source_inputs(root: Path) -> tuple[dict | None, dict[str, int], dict[str, str], set[str]]:
    """Bind only the complete optional producer-dependency Yarn namespace.

    Complete parent absence supports older exports; a present parent must contain
    exactly the reviewed producer child and its Yarn originals. Canonical report
    bytes are separate from the report's archive/member file inventory. Verification
    retains distribution scope and does not establish installation or preferred
    source completeness.
    """
    parent = root / PRODUCER_DEPENDENCIES_PATH
    if any(linked_path(path) for path in (parent, *parent.parents)):
        raise ValueError("linked Yarn source namespace")
    if not parent.exists():
        return None, {}, {}, set()
    producer = root / LIGHTNING_YARN_PRODUCER_PATH
    if (not parent.is_dir() or {entry.name for entry in parent.iterdir()} != {producer.name}
        or linked_path(producer) or not producer.is_dir()
        or {entry.name for entry in producer.iterdir()} != {PurePosixPath(LIGHTNING_YARN_PATH).name}):
        raise ValueError("Yarn namespace inventory differs from reviewed inputs")
    report = verify_lightning_yarn_bundle(root, root / LIGHTNING_YARN_PATH)
    hashes = {f"{LIGHTNING_YARN_PATH}/{name}": record["sha256"]
              for name, record in report["files"].items()}
    modes = {f"{LIGHTNING_YARN_PATH}/{name}": record["mode"]
             for name, record in report["files"].items()}
    encoded = (json.dumps(report, sort_keys=True, indent=2, allow_nan=False) + "\n").encode("utf-8")
    report_path = f"{LIGHTNING_YARN_PATH}/{LIGHTNING_YARN_REPORT_NAME}"
    if regular_source(root, report_path).read_bytes() != encoded:
        raise ValueError("Yarn distribution report differs from verified inputs")
    hashes[report_path], modes[report_path] = hashlib.sha256(encoded).hexdigest(), 0o644
    directories = {PRODUCER_DEPENDENCIES_PATH, LIGHTNING_YARN_PRODUCER_PATH, LIGHTNING_YARN_PATH}
    directories.update(f"{LIGHTNING_YARN_PATH}/{name}" for name in report["directories"])
    for name, checksum in hashes.items():
        if digest(regular_source(root, name)) != checksum:
            raise ValueError("Yarn source inputs changed before export")
    for name in directories:
        relative_path(name)
    return report, dict(sorted(modes.items())), dict(sorted(hashes.items())), directories


def _native_source_inputs(root: Path) -> tuple[dict | None, dict[str, int], dict[str, str], set[str]]:
    """Authenticate complete native overlays and archives, including local Rust patches.

    Absence supports earlier project exports. A present namespace must contain
    exactly the declared profile, whose inventory fails closed on partial data.
    """
    parent = root / "source-inputs/producer-security"
    if any(linked_path(path) for path in (parent, *parent.parents)):
        raise ValueError("linked native source namespace")
    if not parent.exists():
        return None, {}, {}, set()
    bundle = root / LIGHTNING_NATIVE_PATH
    if (not parent.is_dir() or {item.name for item in parent.iterdir()} != {bundle.name}
        or linked_path(bundle) or not bundle.is_dir()):
        raise ValueError("native source namespace inventory differs")
    report = verify_lightning_native_bundle(root)
    hashes = {f"{LIGHTNING_NATIVE_PATH}/{name}": row["sha256"]
              for name, row in report["inputs"].items()}
    encoded = (json.dumps(report, sort_keys=True, indent=2, allow_nan=False) + "\n").encode()
    report_path = f"{LIGHTNING_NATIVE_PATH}/{LIGHTNING_NATIVE_REPORT_NAME}"
    if regular_source(root, report_path).read_bytes() != encoded:
        raise ValueError("native source report differs from verified inputs")
    hashes[report_path] = hashlib.sha256(encoded).hexdigest()
    directories = {"source-inputs/producer-security", LIGHTNING_NATIVE_PATH}
    for name, checksum in hashes.items():
        if digest(regular_source(root, name)) != checksum:
            raise ValueError("native source inputs changed before export")
        directories.update(str(path) for path in PurePosixPath(name).parents
                           if str(path).startswith("source-inputs/producer-security/"))
    return report, {name: 0o644 for name in sorted(hashes)}, dict(sorted(hashes.items())), directories


def _source_input_empty_directories(root: Path, source_directories: set[str]) -> dict[str, int]:
    """Retain only verified empty source directories with canonical transport modes.

    Nonempty directories are reconstructed from file/empty-directory ancestors.
    These transport modes do not assert raw archive directory permission parity.
    """
    empty = {}
    for name in sorted(source_directories | {NPM_ORIGINAL_ARCHIVES, NPM_LOCAL_DISTRIBUTIONS}):
        path = root / relative_path(name)
        if linked_path(path) or not path.is_dir():
            raise ValueError("missing or linked source input directory")
        if not any(path.iterdir()):
            empty[name] = 0o755
    return empty


def tracked_files(root: Path) -> list[tuple[str, bool]]:
    """Use Git's allowlist without copying credentials, caches, or local context."""
    records = subprocess.check_output(
        ["git", "ls-files", "--stage", "-z"], cwd=root,
    ).split(b"\x00")
    result = []
    for record in records:
        if not record:
            continue
        identity, encoded_path = record.split(b"\t", 1)
        mode, _object, stage = identity.split(b" ")
        name = encoded_path.decode("utf-8")
        relative_path(name)
        if stage != b"0" or mode not in (b"100644", b"100755"):
            raise ValueError(f"unmerged, symlink, or submodule source: {name}")
        result.append((name, mode == b"100755"))
    if not result:
        raise ValueError("tracked project source inventory is empty")
    return sorted(result)


def regular_source(root: Path, name: str) -> Path:
    """Reject symlink ancestors as well as a linked final source file."""
    path = root
    for part in relative_path(name).parts:
        path /= part
        if linked_path(path):
            raise ValueError(f"linked source input: {name}")
    if not path.is_file():
        raise ValueError(f"missing regular source input: {name}")
    return path


def create_bundle(root: Path, destination: Path, *, allow_network: bool) -> dict:
    """Copy tracked project and verified dependency sources into a fresh export.

    Args:
        root: Project with versioned Cargo and local npm distribution inputs,
            optional complete producer/Yarn namespaces, exact dependency reports,
            canonical frontend files and offline mapping.
        destination: New directory with no linked ancestors.
        allow_network: Retained CLI/API compatibility flag; true is rejected.

    Returns:
        Whole-project manifest after exact allowlist and archive verification.

    Raises:
        ValueError: Inputs are unsafe, incomplete, untracked, changed, or do not
            match the dependency report/configuration, or acquisition is requested.
        OSError: Source reading or fresh export creation fails.
        subprocess.CalledProcessError: Read-only Git provenance inspection fails.

    No dependency home is consulted and no Cargo, npm or network command runs.
    """
    if allow_network:
        raise ValueError("--allow-network is inappropriate for export; prepare versioned sources separately")
    root, destination = root.absolute(), destination.absolute()
    if any(linked_path(path) for path in (root, *root.parents)) or not root.is_dir():
        raise ValueError("project root has a symlink/reparse ancestor or is not a directory")
    if ".." in root.parts or ".." in destination.parts:
        raise ValueError("source export paths must not contain parent traversal")
    if destination.exists() or linked_path(destination):
        raise ValueError("bundle destination already exists; choose a fresh directory")
    if any(linked_path(parent) for parent in destination.parents):
        raise ValueError("bundle destination has a symlink ancestor")
    inventory = tracked_files(root)
    names = {entry[0] for entry in inventory}
    if len(names) != len(inventory) or MANIFEST_PATH in names:
        raise ValueError("duplicate or reserved tracked project source")
    for name in (*LOCK_PATHS, ".cargo/config.toml", CLOSURE_PATH, NPM_REPORT_PATH,
                 *FRONTEND_PATHS, *VERIFICATION_SCRIPT_PATHS):
        if name not in names:
            raise ValueError(f"untracked required project source: {name}")
    for name in VERIFICATION_SCRIPT_PATHS:
        regular_source(root, name)
    config = regular_source(root, ".cargo/config.toml")
    bundle_cargo_config(config.read_text(encoding="utf-8"))
    producer_inputs, producer_modes, producer_hashes, producer_directories = _producer_source_inputs(root)
    yarn_inputs, yarn_modes, yarn_hashes, yarn_directories = _yarn_source_inputs(root)
    native_inputs, native_modes, native_hashes, native_directories = _native_source_inputs(root)
    lock_paths = (*LOCK_PATHS, LIGHTNING_LOCK_PATH) if producer_inputs is not None else LOCK_PATHS
    if native_inputs is not None:
        lock_paths = (*lock_paths, LIGHTNING_NATIVE_LOCK_PATH)
    closure, cargo_modes = _versioned_closure(root, lock_paths=lock_paths)
    missing = sorted(set(cargo_modes) - names)
    if missing:
        raise ValueError(f"untracked Cargo source inputs: {missing}")
    npm_inputs, npm_modes, npm_hashes = _npm_distribution_inputs(root)
    if not set(npm_hashes).issubset(names):
        raise ValueError("untracked npm distribution or frontend inputs")
    if not set(producer_hashes).issubset(names):
        raise ValueError("untracked producer source inputs")
    if not set(yarn_hashes).issubset(names):
        raise ValueError("untracked Yarn source inputs")
    if not set(native_hashes).issubset(names):
        raise ValueError("untracked native source inputs")
    source_directories = producer_directories | yarn_directories | native_directories
    empty_directories = _source_input_empty_directories(root, source_directories)
    source_hashes = {name: digest(regular_source(root, name)) for name, _executable in inventory}
    if any(source_hashes[name] != checksum for name, checksum in npm_hashes.items()):
        raise ValueError("npm distribution inputs changed before export")
    if any(source_hashes[name] != checksum for name, checksum in producer_hashes.items()):
        raise ValueError("producer source inputs changed before export")
    if any(source_hashes[name] != checksum for name, checksum in yarn_hashes.items()):
        raise ValueError("Yarn source inputs changed before export")
    if any(source_hashes[name] != checksum for name, checksum in native_hashes.items()):
        raise ValueError("native source inputs changed before export")
    revision = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True,
    ).strip()
    tree_status = subprocess.run(["git", "diff", "--quiet", "HEAD", "--"], cwd=root,
                                 check=False).returncode
    if tree_status not in (0, 1):
        raise ValueError("cannot attribute working-tree sources to the recorded revision")
    destination.mkdir(parents=True)
    for name in sorted(source_directories | {NPM_ORIGINAL_ARCHIVES, NPM_LOCAL_DISTRIBUTIONS}):
        (destination / relative_path(name)).mkdir(parents=True, exist_ok=True)
    source_files = []
    file_modes = {}
    for name, executable in inventory:
        source = regular_source(root, name)
        target = destination / relative_path(name)
        target.parent.mkdir(parents=True, exist_ok=True)
        with source.open("rb") as input_stream, target.open("xb") as output_stream:
            while chunk := input_stream.read(1024 * 1024):
                output_stream.write(chunk)
        # Original archive intent survives Windows Git modes and host chmod.
        mode = yarn_modes.get(name, producer_modes.get(name, npm_modes.get(name, cargo_modes.get(
            name, 0o644 if name == CLOSURE_PATH else (0o755 if executable else 0o644)))))
        mode = native_modes.get(name, mode)
        executable = bool(mode & 0o111)
        target.chmod(mode)
        checksum = digest(target)
        if checksum != source_hashes[name]:
            raise ValueError(f"project source changed during export: {name}")
        source_files.append({"path": name, "sha256": checksum,
                             "executable": executable})
        file_modes[name] = mode
    exported_closure, _exported_modes = _versioned_closure(destination, lock_paths=lock_paths)
    if exported_closure != closure:
        raise ValueError("exported Cargo source closure differs from versioned inputs")
    for check_root in (root, destination):
        checked_report, checked_modes, checked_hashes = _npm_distribution_inputs(check_root)
        if checked_report != npm_inputs or checked_modes != npm_modes or checked_hashes != npm_hashes:
            raise ValueError("npm distribution inputs changed during export")
        if _producer_source_inputs(check_root) != (
            producer_inputs, producer_modes, producer_hashes, producer_directories
        ):
            raise ValueError("producer source inputs changed during export")
        if _yarn_source_inputs(check_root) != (yarn_inputs, yarn_modes, yarn_hashes, yarn_directories):
            raise ValueError("Yarn source inputs changed during export")
        if _native_source_inputs(check_root) != (native_inputs, native_modes, native_hashes, native_directories):
            raise ValueError("native source inputs changed during export")
        if _source_input_empty_directories(check_root, source_directories) != empty_directories:
            raise ValueError("source input empty directories changed during export")
    manifest = {
        "schema": "rootlight.rust-source-bundle/1",
        "scope": BUNDLE_SCOPE,
        "source_revision": revision,
        "project_sources_match_revision": tree_status == 0,
        "project_sources": source_files,
        "file_modes": file_modes,
        "cargo_config_sha256": digest(destination / ".cargo/config.toml"),
        "cargo_source_closure": closure,
        "npm_distribution_inputs": npm_inputs,
        "npm_distribution_inputs_verified": True,
        "lightning_producer_inputs": producer_inputs,
        "lightning_producer_inputs_verified": producer_inputs is not None,
        "lightning_yarn_inputs": yarn_inputs,
        "lightning_yarn_inputs_verified": yarn_inputs is not None,
        "lightning_native_inputs": native_inputs,
        "lightning_native_inputs_verified": native_inputs is not None,
        "source_input_empty_directories": empty_directories,
        "frontend_external_source_closure_verified": False,
    }
    (destination / MANIFEST_PATH).write_text(
        json.dumps(manifest, sort_keys=True, indent=2) + "\n",
        encoding="utf-8", newline="\n",
    )
    verify_project_bundle(destination)
    return manifest


def verify_project_bundle(root: Path) -> dict:
    """Recheck restored project bytes, modes and Cargo/npm/producer/Yarn inputs."""
    manifest = json.loads(regular_source(root, MANIFEST_PATH).read_bytes())
    if manifest.get("schema") != "rootlight.rust-source-bundle/1":
        raise ValueError("unsupported Rust source bundle manifest")
    if (manifest.get("scope") != BUNDLE_SCOPE
        or manifest.get("npm_distribution_inputs_verified") is not True
        or manifest.get("frontend_external_source_closure_verified") is not False):
        raise ValueError("unsupported project source scope or npm input claims")
    expected = {MANIFEST_PATH}
    for record in manifest["project_sources"]:
        name = record["path"]
        if name in expected:
            raise ValueError(f"duplicate or reserved project source: {name}")
        expected.add(name)
        actual = digest(regular_source(root, name))
        if actual != record["sha256"]:
            raise ValueError(f"changed project source: {name}")
    required = {*LOCK_PATHS, ".cargo/config.toml", CLOSURE_PATH, NPM_REPORT_PATH,
                *FRONTEND_PATHS, *VERIFICATION_SCRIPT_PATHS}
    if not required.issubset(expected):
        raise ValueError("bundle manifest omits required versioned project inputs")
    config = regular_source(root, ".cargo/config.toml")
    bundle_cargo_config(config.read_text(encoding="utf-8"))
    if digest(config) != manifest["cargo_config_sha256"]:
        raise ValueError("Cargo configuration differs from bundle manifest")
    for directory, names, files in os.walk(root, followlinks=False):
        for name in names + files:
            path = Path(directory) / name
            if linked_path(path):
                raise ValueError("source bundle contains a symlink")
        for name in files:
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if relative not in expected:
                raise ValueError(f"unmanifested project source: {relative}")
    producer_inputs, producer_modes, producer_hashes, producer_directories = _producer_source_inputs(root)
    yarn_inputs, yarn_modes, yarn_hashes, yarn_directories = _yarn_source_inputs(root)
    native_inputs, native_modes, native_hashes, native_directories = _native_source_inputs(root)
    lock_paths = (*LOCK_PATHS, LIGHTNING_LOCK_PATH) if producer_inputs is not None else LOCK_PATHS
    if native_inputs is not None:
        lock_paths = (*lock_paths, LIGHTNING_NATIVE_LOCK_PATH)
    closure, cargo_modes = _versioned_closure(root, lock_paths=lock_paths)
    if closure != manifest["cargo_source_closure"]:
        raise ValueError("Cargo source closure differs from bundle manifest")
    npm_inputs, npm_modes, npm_hashes = _npm_distribution_inputs(root)
    if not set(npm_hashes).issubset(expected):
        raise ValueError("bundle manifest omits required npm input sources")
    if (json.dumps(npm_inputs, sort_keys=True, allow_nan=False)
        != json.dumps(manifest.get("npm_distribution_inputs"), sort_keys=True, allow_nan=False)):
        raise ValueError("npm distribution inputs differ from bundle manifest")
    if not set(producer_hashes).issubset(expected):
        raise ValueError("bundle manifest omits required producer sources")
    if ("lightning_producer_inputs" not in manifest
        or manifest.get("lightning_producer_inputs_verified") is not (producer_inputs is not None)
        or json.dumps(producer_inputs, sort_keys=True, allow_nan=False)
        != json.dumps(manifest["lightning_producer_inputs"], sort_keys=True, allow_nan=False)):
        raise ValueError("producer source inputs differ from bundle manifest")
    if not set(yarn_hashes).issubset(expected):
        raise ValueError("bundle manifest omits required Yarn sources")
    if ("lightning_yarn_inputs" not in manifest
        or manifest.get("lightning_yarn_inputs_verified") is not (yarn_inputs is not None)
        or json.dumps(yarn_inputs, sort_keys=True, allow_nan=False)
        != json.dumps(manifest["lightning_yarn_inputs"], sort_keys=True, allow_nan=False)):
        raise ValueError("Yarn source inputs differ from bundle manifest")
    if not set(native_hashes).issubset(expected):
        raise ValueError("bundle manifest omits required native sources")
    if ("lightning_native_inputs" not in manifest
        or manifest.get("lightning_native_inputs_verified") is not (native_inputs is not None)
        or json.dumps(native_inputs, sort_keys=True, allow_nan=False)
        != json.dumps(manifest["lightning_native_inputs"], sort_keys=True, allow_nan=False)):
        raise ValueError("native source inputs differ from bundle manifest")
    empty_directories = _source_input_empty_directories(root, producer_directories | yarn_directories | native_directories)
    if ("source_input_empty_directories" not in manifest
        or json.dumps(empty_directories, sort_keys=True, allow_nan=False)
        != json.dumps(manifest["source_input_empty_directories"], sort_keys=True, allow_nan=False)):
        raise ValueError("source input empty directories differ from bundle manifest")
    modes = {name: 0o755 if record["executable"] else 0o644
             for record in manifest["project_sources"] for name in [record["path"]]}
    if any(modes.get(name) != mode for name, mode in cargo_modes.items()) or modes.get(CLOSURE_PATH) != 0o644:
        raise ValueError("project executable modes differ from authenticated Cargo inputs")
    if any(modes.get(name) != mode for name, mode in npm_modes.items()):
        raise ValueError("project executable modes differ from authenticated npm inputs")
    if any(modes.get(name) != mode for name, mode in producer_modes.items()):
        raise ValueError("project executable modes differ from authenticated producer inputs")
    if any(modes.get(name) != mode for name, mode in yarn_modes.items()):
        raise ValueError("project executable modes differ from authenticated Yarn inputs")
    if any(modes.get(name) != mode for name, mode in native_modes.items()):
        raise ValueError("project executable modes differ from authenticated native inputs")
    if modes != manifest["file_modes"]:
        raise ValueError("bundle executable modes differ from authoritative source inputs")
    if os.name != "nt":
        for name, mode in modes.items():
            if bool(regular_source(root, name).stat().st_mode & 0o111) != bool(mode & 0o111):
                raise ValueError(f"restored executable mode differs from source inputs: {name}")
        if any(stat.S_IMODE(regular_source(root, name).stat().st_mode) != mode
               for name, mode in npm_modes.items()):
            raise ValueError("restored npm data modes differ from authenticated inputs")
    return manifest


def archive_bundle(root: Path, archive: Path) -> None:
    """Write reproducible transport bytes without host ownership or timestamps."""
    manifest = verify_project_bundle(root)
    empty_directories = manifest.get("source_input_empty_directories", {})
    files = {path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()}
    with archive.open("xb") as output:
        with gzip.GzipFile(filename="", fileobj=output, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as tar:
                for relative in sorted(files | set(empty_directories)):
                    info = tarfile.TarInfo("rootlight/" + relative)
                    if relative in empty_directories:
                        info.type, info.mode = tarfile.DIRTYPE, empty_directories[relative]
                        tar.addfile(info)
                        continue
                    path = root / relative_path(relative)
                    info.size = path.stat().st_size
                    info.mode = (0o644 if relative == MANIFEST_PATH else manifest["file_modes"][relative])
                    with path.open("rb") as stream:
                        tar.addfile(info, stream)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--allow-network", action="store_true",
                        help="unsupported for export; acquire reviewed versioned sources separately")
    parser.add_argument("--verify-only", action="store_true")
    arguments = parser.parse_args()
    try:
        if arguments.allow_network:
            raise ValueError("--allow-network is inappropriate for export; prepare versioned sources separately")
        destination = arguments.destination.absolute()
        manifest = (verify_project_bundle(destination) if arguments.verify_only else
                    create_bundle(arguments.root.absolute(), destination,
                                  allow_network=arguments.allow_network))
        if arguments.archive:
            archive_bundle(destination, arguments.archive.absolute())
        print(json.dumps({"schema": manifest["schema"],
                          "project_source_count": len(manifest["project_sources"]),
                          "status": "verified"}, sort_keys=True))
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Rust source bundle failed: {error}\n")


if __name__ == "__main__":
    main()
