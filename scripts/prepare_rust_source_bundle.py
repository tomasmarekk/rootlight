#!/usr/bin/env python3
"""Export versioned Rust and local npm inputs without acquisition or caches.

The tracked registry sources and original archives cover workspace, native-test,
and fuzz locks. Local npm distributions retain declared fixture provenance;
export proves neither preferred frontend closure nor third-party authorship.
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
from verify_local_npm_sources import CANONICAL_LOCK, verify_local_bundle


LOCK_PATHS = ("Cargo.lock", "tests/native-grammars/Cargo.lock", "fuzz/Cargo.lock")
MANIFEST_PATH = "rust-source-bundle.json"
VENDOR_PATH = "source-inputs/cargo"
ARCHIVES_PATH = "source-inputs/crates"
CLOSURE_PATH = "source-inputs/cargo-source-closure.json"
NPM_REPORT_PATH = "source-inputs/npm/npm-source-closure.json"
FRONTEND_PATHS = ("apps/rootlight-web/frontend/package.json", CANONICAL_LOCK,
                  "apps/rootlight-web/frontend/.npmrc")
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


def registry_inputs(root: Path) -> dict[tuple[str, str], str]:
    """Read immutable archive identities without executing dependency code."""
    packages = {}
    for lock in LOCK_PATHS:
        for package in tomllib.loads((root / lock).read_text(encoding="utf-8"))["package"]:
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


def _versioned_closure(root: Path) -> tuple[dict, dict[str, int]]:
    locks = [regular_source(root, name) for name in LOCK_PATHS]
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
            exact dependency reports, canonical frontend files and offline mapping.
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
                 *FRONTEND_PATHS):
        if name not in names:
            raise ValueError(f"untracked required project source: {name}")
    config = regular_source(root, ".cargo/config.toml")
    bundle_cargo_config(config.read_text(encoding="utf-8"))
    closure, cargo_modes = _versioned_closure(root)
    missing = sorted(set(cargo_modes) - names)
    if missing:
        raise ValueError(f"untracked Cargo source inputs: {missing}")
    npm_inputs, npm_modes, npm_hashes = _npm_distribution_inputs(root)
    if not set(npm_hashes).issubset(names):
        raise ValueError("untracked npm distribution or frontend inputs")
    source_hashes = {name: digest(regular_source(root, name)) for name, _executable in inventory}
    if any(source_hashes[name] != checksum for name, checksum in npm_hashes.items()):
        raise ValueError("npm distribution inputs changed before export")
    revision = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True,
    ).strip()
    tree_status = subprocess.run(["git", "diff", "--quiet", "HEAD", "--"], cwd=root,
                                 check=False).returncode
    if tree_status not in (0, 1):
        raise ValueError("cannot attribute working-tree sources to the recorded revision")
    destination.mkdir(parents=True)
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
        mode = npm_modes.get(name, cargo_modes.get(name, 0o644 if name == CLOSURE_PATH else
                                                  (0o755 if executable else 0o644)))
        executable = bool(mode & 0o111)
        target.chmod(mode)
        checksum = digest(target)
        if checksum != source_hashes[name]:
            raise ValueError(f"project source changed during export: {name}")
        source_files.append({"path": name, "sha256": checksum,
                             "executable": executable})
        file_modes[name] = mode
    exported_closure, _exported_modes = _versioned_closure(destination)
    if exported_closure != closure:
        raise ValueError("exported Cargo source closure differs from versioned inputs")
    for check_root in (root, destination):
        checked_report, checked_modes, checked_hashes = _npm_distribution_inputs(check_root)
        if checked_report != npm_inputs or checked_modes != npm_modes or checked_hashes != npm_hashes:
            raise ValueError("npm distribution inputs changed during export")
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
        "frontend_external_source_closure_verified": False,
    }
    (destination / MANIFEST_PATH).write_text(
        json.dumps(manifest, sort_keys=True, indent=2) + "\n",
        encoding="utf-8", newline="\n",
    )
    verify_project_bundle(destination)
    return manifest


def verify_project_bundle(root: Path) -> dict:
    """Recheck exact restored project bytes, modes and Cargo/local npm inputs."""
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
                *FRONTEND_PATHS}
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
    closure, cargo_modes = _versioned_closure(root)
    if closure != manifest["cargo_source_closure"]:
        raise ValueError("Cargo source closure differs from bundle manifest")
    npm_inputs, npm_modes, npm_hashes = _npm_distribution_inputs(root)
    if not set(npm_hashes).issubset(expected):
        raise ValueError("bundle manifest omits required npm input sources")
    if (json.dumps(npm_inputs, sort_keys=True, allow_nan=False)
        != json.dumps(manifest.get("npm_distribution_inputs"), sort_keys=True, allow_nan=False)):
        raise ValueError("npm distribution inputs differ from bundle manifest")
    modes = {name: 0o755 if record["executable"] else 0o644
             for record in manifest["project_sources"] for name in [record["path"]]}
    if any(modes.get(name) != mode for name, mode in cargo_modes.items()) or modes.get(CLOSURE_PATH) != 0o644:
        raise ValueError("project executable modes differ from authenticated Cargo inputs")
    if any(modes.get(name) != mode for name, mode in npm_modes.items()):
        raise ValueError("project executable modes differ from authenticated npm inputs")
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
    with archive.open("xb") as output:
        with gzip.GzipFile(filename="", fileobj=output, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as tar:
                for path in sorted(root.rglob("*"), key=lambda entry: entry.relative_to(root).as_posix()):
                    if not path.is_file():
                        continue
                    info = tarfile.TarInfo("rootlight/" + path.relative_to(root).as_posix())
                    info.size = path.stat().st_size
                    relative = path.relative_to(root).as_posix()
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
