"""Bind query-only CodeQL partitions to one complete finalized database.

Only fully evaluated, disjoint partitions of the pinned security suite may be
merged for upload. This helper never uploads results or acquires query code.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path

CLI_VERSION = "2.27.2"
PACK_VERSION = "2.4.7"
PACK = "codeql/javascript-queries"
SUITE = f"{PACK}@{PACK_VERSION}:codeql-suites/javascript-security-extended.qls"
CATEGORY = "/language:javascript-typescript/"
QUERY_COUNT = 105
BUNDLE_SHA256 = "c85010379fa10172b7a59c6689171cbfe692bcd1a2ab92cdc902e83c375deab5"
GROUPS = {
    "polynomial-redos": "Performance/PolynomialReDoS.ql",
    "indirect-command-injection": "Security/CWE-078/IndirectCommandInjection.ql",
    "remaining": None,
}
CONFIG_INPUTS = (
    ".github/workflows/codeql.yml",
    ".github/codeql/javascript.yml",
    "scripts/codeql_javascript_shards.py",
    "scripts/setup-codeql-javascript.sh",
)


def _sha(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _read(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def _write(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _run(*args: str, capture: bool = False):
    result = subprocess.run(args, check=True, text=True, encoding="utf-8", stdout=subprocess.PIPE if capture else None)
    return result.stdout if capture else None


def _identity(root: Path, source: str) -> dict:
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("invalid source commit")
    if _run("git", "--no-optional-locks", "-C", str(root), "rev-parse", "HEAD", capture=True).strip() != source:
        raise ValueError("checkout does not match the requested source commit")
    _run("git", "--no-optional-locks", "-C", str(root), "diff", "--quiet", "HEAD", "--")
    return {"source_sha": source, "configuration": {name: _sha(root / name) for name in CONFIG_INPUTS}}


def partition(queries: list[dict]) -> dict[str, list[dict]]:
    """Return the exact disjoint 1/1/103 partition; reject missing or duplicate queries."""
    paths = [item["path"] for item in queries]
    ids = [item["id"] for item in queries]
    if len(paths) != QUERY_COUNT or len(set(paths)) != QUERY_COUNT or len(set(ids)) != QUERY_COUNT:
        raise ValueError("security suite must contain exactly 105 distinct paths and rule IDs")
    for path in GROUPS.values():
        if path is not None and path not in paths:
            raise ValueError("required isolated query is missing")
    result = {name: [] for name in GROUPS}
    for item in queries:
        group = next((name for name, path in GROUPS.items() if path == item["path"]), "remaining")
        result[group].append(item)
    return result


def _tool_inventory(codeql: Path) -> tuple[dict, Path]:
    version = json.loads(_run(str(codeql), "version", "--format=json", capture=True))
    if version["version"] != CLI_VERSION:
        raise ValueError("unexpected CodeQL CLI version")
    pack = codeql.parent / "qlpacks" / "codeql" / "javascript-queries" / PACK_VERSION
    # Hash the complete published pack closure, including precompiled plans and libraries.
    closure = {str(p.relative_to(codeql.parent / "qlpacks")).replace("\\", "/"): _sha(p)
               for p in sorted((codeql.parent / "qlpacks").rglob("*")) if p.is_file()}
    if not closure or not pack.is_dir():
        raise ValueError("pinned query pack closure is absent")
    resolved = json.loads(_run(str(codeql), "resolve", "queries", "--format=json", SUITE, capture=True))
    queries = []
    for name in resolved:
        path = Path(name).resolve()
        relative = path.relative_to(pack.resolve()).as_posix()
        contents = path.read_text(encoding="utf-8")
        ids = re.findall(r"^\s*\*\s*@id\s+(\S+)\s*$", contents, re.MULTILINE)
        kinds = re.findall(r"^\s*\*\s*@kind\s+(\S+)\s*$", contents, re.MULTILINE)
        if len(ids) != 1 or len(kinds) != 1 or kinds[0] not in ("problem", "path-problem", "metric", "diagnostic"):
            raise ValueError("query does not have one supported kind and unambiguous rule ID")
        queries.append({"path": relative, "id": ids[0], "kind": kinds[0], "sha256": _sha(path)})
    queries.sort(key=lambda item: item["path"])
    partition(queries)
    return {"cli_version": CLI_VERSION, "cli_sha256": _sha(codeql), "pack_version": PACK_VERSION,
            "cli_jar_sha256": _sha(codeql.parent / "tools" / "codeql.jar"), "bundle_sha256": BUNDLE_SHA256,
            "pack_closure_sha256": hashlib.sha256(json.dumps(closure, sort_keys=True).encode()).hexdigest(),
            "queries": queries}, pack


def validate_manifest(manifest: dict, identity: dict, tool: dict, bundle: Path | None = None) -> None:
    """Reject a database receipt from another source, policy, scanner, or query closure."""
    if manifest.get("schema") != 1 or manifest.get("identity") != identity or manifest.get("tool") != tool:
        raise ValueError("database source, configuration, or scanner identity mismatch")
    partition(manifest["tool"]["queries"])
    if bundle is not None and (not bundle.is_file() or _sha(bundle) != manifest["database_sha256"]):
        raise ValueError("database bundle digest mismatch")


def _validate_database_metadata(database: Path, root: Path) -> None:
    metadata_path = database / "codeql-database.yml"
    if not metadata_path.is_file():
        raise ValueError("finalized database metadata is absent")
    metadata = metadata_path.read_text(encoding="utf-8")
    if not re.search(r"^finalised: true$", metadata, re.MULTILINE) or not re.search(r"^primaryLanguage: javascript$", metadata, re.MULTILINE):
        raise ValueError("JavaScript database is not finalized")
    for flag in ("overlayBaseDatabase", "overlayDatabase"):
        if not re.search(rf"^{flag}: false$", metadata, re.MULTILINE):
            raise ValueError("database must be a complete standalone extraction, not an overlay")
    prefixes = re.findall(r"^sourceLocationPrefix: (.+)$", metadata, re.MULTILINE)
    if len(prefixes) != 1 or Path(prefixes[0]).resolve() != root.resolve():
        raise ValueError("database was not extracted from the complete checkout root")


def prepare(codeql: Path, producer_codeql: Path, root: Path, source: str, database: Path, output: Path) -> None:
    """Bundle a complete finalized database only when producer and consumer scanner identities agree."""
    tool, _ = _tool_inventory(codeql)
    producer_tool, _ = _tool_inventory(producer_codeql)
    if producer_tool != tool:
        raise ValueError("database producer and query runner use different scanner inputs")
    identity = _identity(root, source)
    _validate_database_metadata(database, root)
    output.mkdir(parents=True, exist_ok=True)
    bundle = output / "javascript.zip"
    _run(str(codeql), "database", "bundle", str(database), "--output", str(bundle),
         "--name=javascript", "--cache-cleanup=clear", "--include-temp", "--no-include-results", "--no-include-logs")
    _write(output / "manifest.json", {"schema": 1, "identity": identity, "tool": tool,
                                     "database_sha256": _sha(bundle)})


def validate_sarif(document: dict, queries: list[dict]) -> dict:
    """Require one successful CodeQL run containing every expected evaluated rule."""
    runs = document.get("runs", [])
    if len(runs) != 1:
        raise ValueError("SARIF must contain exactly one merged run")
    run = runs[0]
    if run.get("automationDetails", {}).get("id") != CATEGORY:
        raise ValueError("SARIF category differs from the existing JavaScript analysis")
    driver = run.get("tool", {}).get("driver", {})
    rules = [rule["id"] for rule in driver.get("rules", [])]
    if driver.get("name") != "CodeQL" or set(rules) != {q["id"] for q in queries if q["kind"] != "diagnostic"} or len(rules) != len(set(rules)):
        raise ValueError("SARIF rule inventory is incomplete or duplicated")
    # CodeQL serializes diagnostic queries as notification descriptors. Their
    # BQRS execution remains mandatory, just like alert and metric queries.
    diagnostics = [item["id"] for item in driver.get("notifications", []) if item.get("properties", {}).get("kind") == "diagnostic"]
    if set(diagnostics) != {q["id"] for q in queries if q["kind"] == "diagnostic"} or len(diagnostics) != len(set(diagnostics)):
        raise ValueError("SARIF diagnostic query inventory is incomplete or duplicated")
    invocations = run.get("invocations", [])
    if not invocations or any(invocation.get("executionSuccessful") is not True for invocation in invocations):
        raise ValueError("SARIF contains unsuccessful or absent execution evidence")
    return run


def analyze(codeql: Path, root: Path, source: str, inputs: Path, group: str, output: Path) -> None:
    """Evaluate one partition against the verified complete database; receipt exists only on success."""
    tool, pack = _tool_inventory(codeql)
    manifest = _read(inputs / "manifest.json")
    validate_manifest(manifest, _identity(root, source), tool, inputs / "javascript.zip")
    queries = partition(tool["queries"])[group]
    output.mkdir(parents=True, exist_ok=True)
    if list(output.iterdir()):
        raise ValueError("analysis output must be a fresh directory")
    database = output / "database"
    _run(str(codeql), "database", "unbundle", str(inputs / "javascript.zip"), "--target", str(output), "--name=database")
    if list(database.rglob("*.bqrs")):
        raise ValueError("database must not contain previous query results")
    suite = output / "selected.qls"
    suite.write_text("\n".join("- query: " + json.dumps(str(pack / q["path"])) for q in queries) + "\n", encoding="utf-8")
    # Keep the established resource contract while preventing the independent heavy
    # data-flow queries from competing for the same evaluator's memory/cache.
    _run(str(codeql), "database", "run-queries", str(database), str(suite), "--threads=2", "--ram=14575",
         "--expect-discarded-cache", "--min-disk-free=1024", "-v",
         f"--evaluator-log={output / 'evaluator.jsonl'}", "--evaluator-log-minify")
    results = {}
    result_files = list((database / "results").rglob("*.bqrs"))
    for query in queries:
        suffix = "/" + query["path"].removesuffix(".ql") + ".bqrs"
        matches = [p for p in result_files if p.as_posix().endswith(suffix)]
        if len(matches) != 1 or matches[0].stat().st_size == 0:
            raise ValueError("query did not produce exactly one nonempty result")
        results[query["path"]] = _sha(matches[0])
    sarif = output / "results.sarif"
    _run(str(codeql), "database", "interpret-results", str(database), str(suite), "--format=sarif-latest",
         "--output", str(sarif), f"--sarif-category={CATEGORY}", "--sarif-include-query-help=always",
         "--sarif-add-baseline-file-info", "--print-diagnostics-summary", "--print-metrics-summary", "--sublanguage-file-coverage")
    validate_sarif(_read(sarif), queries)
    _write(output / "receipt.json", {"schema": 1, "manifest_sha256": _sha(inputs / "manifest.json"),
                                    "group": group, "queries": queries, "results": results,
                                    "sarif_sha256": _sha(sarif), "completed": True})


def validate_receipts(manifest_path: Path, results: Path) -> list[Path]:
    """Reject missing, duplicate, partial, stale, or modified shard results before merging."""
    manifest = _read(manifest_path)
    expected = partition(manifest["tool"]["queries"])
    receipts = list(results.glob("*/receipt.json"))
    if len(receipts) != len(GROUPS):
        raise ValueError("all three shard receipts are required")
    seen = set()
    files = []
    for path in sorted(receipts):
        receipt = _read(path)
        group = receipt.get("group")
        if group not in expected or group in seen:
            raise ValueError("unknown or duplicate shard")
        seen.add(group)
        queries = expected[group]
        if receipt.get("schema") != 1 or receipt.get("completed") is not True or receipt.get("manifest_sha256") != _sha(manifest_path):
            raise ValueError("partial or incorrectly bound shard receipt")
        if receipt.get("queries") != queries or set(receipt.get("results", {})) != {q["path"] for q in queries}:
            raise ValueError("shard result inventory is incomplete")
        if any(not re.fullmatch(r"[0-9a-f]{64}", value) for value in receipt["results"].values()):
            raise ValueError("invalid evaluated result digest")
        sarif = path.parent / "results.sarif"
        if _sha(sarif) != receipt["sarif_sha256"]:
            raise ValueError("shard SARIF digest mismatch")
        validate_sarif(_read(sarif), queries)
        files.append(sarif)
    return files


def merge(codeql: Path, root: Path, source: str, manifest_path: Path, results: Path, output: Path) -> None:
    """Merge all successful partitions into the existing single Code Scanning category."""
    tool, _ = _tool_inventory(codeql)
    manifest = _read(manifest_path)
    validate_manifest(manifest, _identity(root, source), tool)
    files = validate_receipts(manifest_path, results)
    output.mkdir(parents=True, exist_ok=True)
    combined = output / "javascript.sarif"
    _run(str(codeql), "github", "merge-results", *[arg for path in files for arg in ("--sarif", str(path))],
         "--sarif-merge-runs-from-equal-category", "--output", str(combined))
    run = validate_sarif(_read(combined), tool["queries"])
    # Rule-index remapping is the CLI's responsibility. Verify its merge retained
    # all findings as well as the zero-result rules before publishing the result.
    expected_count = sum(len(_read(path)["runs"][0].get("results", [])) for path in files)
    if len(run.get("results", [])) != expected_count:
        raise ValueError("merged SARIF lost or duplicated findings")
    _write(output / "receipt.json", {"schema": 1, "source_sha": source, "manifest_sha256": _sha(manifest_path),
                                    "sarif_sha256": _sha(combined), "groups": sorted(GROUPS),
                                    "query_count": QUERY_COUNT, "completed": True})
    _write(output / "manifest.json", manifest)


def verify_upload(root: Path, source: str, inputs: Path) -> None:
    """Reject any changed merged artifact or source binding before the privileged upload."""
    manifest = _read(inputs / "manifest.json")
    receipt = _read(inputs / "receipt.json")
    if manifest.get("identity") != _identity(root, source):
        raise ValueError("merged result source or configuration mismatch")
    partition(manifest["tool"]["queries"])
    expected = {"schema": 1, "source_sha": source, "manifest_sha256": _sha(inputs / "manifest.json"),
                "sarif_sha256": _sha(inputs / "javascript.sarif"), "groups": sorted(GROUPS),
                "query_count": QUERY_COUNT, "completed": True}
    if receipt != expected:
        raise ValueError("merged result receipt mismatch")
    validate_sarif(_read(inputs / "javascript.sarif"), manifest["tool"]["queries"])


def main() -> None:
    """Run a local preparation, evaluation, or merge stage; any failed contract exits nonzero."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "analyze", "merge", "verify-upload"))
    parser.add_argument("--codeql", type=Path)
    parser.add_argument("--producer-codeql", type=Path)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--source", required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--group", choices=GROUPS)
    parser.add_argument("--manifest", type=Path)
    args = parser.parse_args()
    if args.command == "verify-upload":
        verify_upload(args.root.resolve(), args.source, args.input.resolve())
        return
    if args.codeql is None or args.output is None:
        parser.error("--codeql and --output are required for scanner stages")
    if args.command == "prepare":
        if args.producer_codeql is None:
            parser.error("--producer-codeql is required to authenticate the extraction tool")
        prepare(args.codeql.resolve(), args.producer_codeql.resolve(), args.root.resolve(), args.source, args.input.resolve(), args.output.resolve())
    elif args.command == "analyze":
        if args.group is None:
            parser.error("--group is required for analysis")
        analyze(args.codeql.resolve(), args.root.resolve(), args.source, args.input.resolve(), args.group, args.output.resolve())
    else:
        if args.manifest is None:
            parser.error("--manifest is required for merge")
        merge(args.codeql.resolve(), args.root.resolve(), args.source, args.manifest.resolve(), args.input.resolve(), args.output.resolve())


if __name__ == "__main__":
    main()
