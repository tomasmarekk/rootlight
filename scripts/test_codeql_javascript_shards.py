"""Exercise fail-closed query coverage, provenance, and SARIF merge contracts.

The optional pinned CLI check verifies real rule-index remapping, including a
finding from each group; fabricated receipts alone cannot qualify that behavior.
"""

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import codeql_javascript_shards as shards


def _queries():
    paths = [path for path in shards.GROUPS.values() if path is not None]
    paths.extend(f"Security/Example{index}.ql" for index in range(103))
    return [{"path": path, "id": f"js/example-{index}", "kind": "diagnostic" if index >= 103 else "path-problem",
             "sha256": "1" * 64} for index, path in enumerate(paths)]


def _sarif(queries):
    return {"version": "2.1.0", "$schema": "https://json.schemastore.org/sarif-2.1.0.json", "runs": [{
        "automationDetails": {"id": shards.CATEGORY},
        "tool": {"driver": {"name": "CodeQL", "semanticVersion": shards.CLI_VERSION,
                            "rules": [{"id": q["id"]} for q in queries if q["kind"] != "diagnostic"],
                            "notifications": [{"id": q["id"], "properties": {"kind": "diagnostic"}}
                                              for q in queries if q["kind"] == "diagnostic"]}},
        "invocations": [{"executionSuccessful": True}],
        "results": [{"ruleId": queries[0]["id"], "ruleIndex": 0, "message": {"text": "fixture finding"}}],
    }]}


class ShardContracts(unittest.TestCase):
    """Check that changed or incomplete evidence cannot become a successful upload."""

    def setUp(self):
        """Create complete local receipts with real file digests for each negative control."""
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.queries = _queries()
        self.identity = {"source_sha": "a" * 40, "configuration": {"config": "b" * 64}}
        self.tool = {"queries": self.queries, "cli_version": shards.CLI_VERSION}
        self.bundle = self.root / "database.zip"
        self.bundle.write_bytes(b"database fixture")
        self.manifest = {"schema": 1, "identity": self.identity, "tool": self.tool,
                         "database_sha256": shards._sha(self.bundle)}
        self.manifest_path = self.root / "manifest.json"
        shards._write(self.manifest_path, self.manifest)
        self.results = self.root / "results"
        for name, queries in shards.partition(self.queries).items():
            folder = self.results / name
            shards._write(folder / "results.sarif", _sarif(queries))
            shards._write(folder / "receipt.json", {
                "schema": 1, "manifest_sha256": shards._sha(self.manifest_path), "group": name,
                "queries": queries, "results": {q["path"]: "c" * 64 for q in queries},
                "sarif_sha256": shards._sha(folder / "results.sarif"), "completed": True,
            })

    def _change_receipt(self, change):
        path = self.results / "remaining" / "receipt.json"
        receipt = shards._read(path)
        change(receipt)
        shards._write(path, receipt)

    def test_complete_partition_and_receipts_pass(self):
        """The full suite is a disjoint 1/1/103 union, including zero-result rules."""
        self.assertEqual([len(value) for value in shards.partition(self.queries).values()], [1, 1, 103])
        shards.validate_manifest(self.manifest, self.identity, self.tool, self.bundle)
        self.assertEqual(len(shards.validate_receipts(self.manifest_path, self.results)), 3)

    def test_missing_duplicate_and_changed_heavy_queries_fail(self):
        """Count-preserving duplicates or replacement of an isolated query cannot shrink coverage."""
        variants = [self.queries[:-1], self.queries[:-1] + [self.queries[0]]]
        changed = copy.deepcopy(self.queries)
        changed[0]["path"] = "Security/Unexpected.ql"
        variants.append(changed)
        for queries in variants:
            with self.subTest(queries=queries[:1]), self.assertRaises(ValueError):
                shards.partition(queries)

    def test_wrong_source_config_and_scanner_fail(self):
        """A valid database cannot be reattributed to another revision, policy, or tool closure."""
        for key in ("identity", "tool"):
            changed = copy.deepcopy(self.manifest)
            changed[key]["unexpected"] = True
            with self.subTest(key=key), self.assertRaises(ValueError):
                shards.validate_manifest(changed, self.identity, self.tool, self.bundle)

    def test_changed_database_bytes_fail(self):
        """Consumers authenticate database bytes before unbundling or executing queries."""
        self.bundle.write_bytes(b"other database fixture")
        with self.assertRaisesRegex(ValueError, "digest"):
            shards.validate_manifest(self.manifest, self.identity, self.tool, self.bundle)

    def test_partial_overlay_or_wrong_source_database_fails(self):
        """A finalized-looking overlay or subdirectory database cannot replace full extraction."""
        database = self.root / "database"
        database.mkdir()
        valid = (f"finalised: true\nprimaryLanguage: javascript\nsourceLocationPrefix: {self.root}\n"
                 "overlayBaseDatabase: false\noverlayDatabase: false\n")
        path = database / "codeql-database.yml"
        path.write_text(valid, encoding="utf-8")
        shards._validate_database_metadata(database, self.root)
        for value in (valid.replace("finalised: true", "finalised: false"),
                      valid.replace("overlayDatabase: false", "overlayDatabase: true"),
                      valid.replace("overlayBaseDatabase: false", "overlayBaseDatabase: true"),
                      valid.replace(str(self.root), str(self.root / "subset"))):
            path.write_text(value, encoding="utf-8")
            with self.subTest(value=value), self.assertRaises(ValueError):
                shards._validate_database_metadata(database, self.root)

    def test_different_extraction_tool_is_rejected_before_bundle(self):
        """Action extraction cannot silently use another evaluator than the authenticated runner."""
        other = {**self.tool, "cli_jar_sha256": "d" * 64}
        with patch.object(shards, "_tool_inventory", side_effect=[(self.tool, self.root), (other, self.root)]), \
                self.assertRaisesRegex(ValueError, "different scanner"):
            shards.prepare(self.root / "runner", self.root / "producer", self.root, "a" * 40,
                           self.root / "database", self.root / "bundle")

    def test_missing_and_duplicate_shards_fail(self):
        """Two completed shards or a duplicated third identity never qualify the full suite."""
        self._change_receipt(lambda row: row.update(group="polynomial-redos"))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            shards.validate_receipts(self.manifest_path, self.results)
        (self.results / "remaining" / "receipt.json").unlink()
        with self.assertRaisesRegex(ValueError, "three"):
            shards.validate_receipts(self.manifest_path, self.results)

    def test_partial_execution_and_missing_bqrs_fail(self):
        """Successful-looking SARIF cannot substitute for completion of every query."""
        original = shards._read(self.results / "remaining" / "receipt.json")
        for change in (lambda row: row.update(completed=False), lambda row: row["results"].pop(next(iter(row["results"]))),
                       lambda row: row.update(manifest_sha256="d" * 64)):
            shards._write(self.results / "remaining" / "receipt.json", original)
            self._change_receipt(change)
            with self.assertRaises(ValueError):
                shards.validate_receipts(self.manifest_path, self.results)

    def test_changed_sarif_after_completion_fails(self):
        """Post-execution changes are rejected even when the replacement is valid JSON."""
        path = self.results / "remaining" / "results.sarif"
        document = shards._read(path)
        document["runs"][0]["results"] = []
        shards._write(path, document)
        with self.assertRaisesRegex(ValueError, "digest"):
            shards.validate_receipts(self.manifest_path, self.results)

    def test_partial_runs_wrong_category_and_missing_rules_fail(self):
        """The merged upload must be one successful run retaining all rule identities."""
        for mutate in (lambda run: run["automationDetails"].update(id="another-category"),
                       lambda run: run["invocations"][0].update(executionSuccessful=False),
                       lambda run: run["tool"]["driver"]["rules"].pop(),
                       lambda run: run["tool"]["driver"]["notifications"].pop()):
            document = _sarif(self.queries)
            mutate(document["runs"][0])
            with self.assertRaises(ValueError):
                shards.validate_sarif(document, self.queries)
        document = _sarif(self.queries)
        document["runs"].append(copy.deepcopy(document["runs"][0]))
        with self.assertRaisesRegex(ValueError, "one merged run"):
            shards.validate_sarif(document, self.queries)

    @unittest.skipUnless(os.environ.get("CODEQL_TEST_CLI"), "pinned CLI integration is enabled after scanner setup")
    def test_real_cli_merge_preserves_findings_and_rule_indices(self):
        """Official CLI merges all three runs and remaps each finding to its original rule."""
        codeql = Path(os.environ["CODEQL_TEST_CLI"])
        version = json.loads(subprocess.check_output([str(codeql), "version", "--format=json"], text=True))
        self.assertEqual(version["version"], shards.CLI_VERSION)
        output = self.root / "merged"
        with patch.object(shards, "_tool_inventory", return_value=(self.tool, self.root)), \
                patch.object(shards, "_identity", return_value=self.identity):
            shards.merge(codeql, self.root, self.identity["source_sha"], self.manifest_path, self.results, output)
            shards.verify_upload(self.root, self.identity["source_sha"], output)
        run = shards._read(output / "javascript.sarif")["runs"][0]
        self.assertEqual(len(run["results"]), 3)
        for finding in run["results"]:
            self.assertEqual(run["tool"]["driver"]["rules"][finding["ruleIndex"]]["id"], finding["ruleId"])
        (output / "javascript.sarif").write_text("{}", encoding="utf-8")
        with patch.object(shards, "_identity", return_value=self.identity), self.assertRaisesRegex(ValueError, "receipt"):
            shards.verify_upload(self.root, self.identity["source_sha"], output)


if __name__ == "__main__":
    unittest.main()
