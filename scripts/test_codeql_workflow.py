#!/usr/bin/env python3
"""Guard native CodeQL coverage, permissions, and required-result handling.

The actual workflow shell is exercised without invoking scanners or downloading
tools; source-scope checks retain the files behind existing security regressions.
"""

from __future__ import annotations

import itertools
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import textwrap
import tomllib
import unittest

from test_workflow_checkout_paths import workflow_jobs


REPOSITORY = Path(__file__).resolve().parent.parent
LANGUAGES = ("rust", "javascript-typescript")
# These eight alert instances span six maintained files; none may vanish by scope.
RACE_LOCATIONS = (
    (5, "apps/rootlight-web/frontend/scripts/write-asset-manifest.mjs"),
    (6, "apps/rootlight-web/frontend/scripts/verify-installed-dependencies.mjs"),
    (7, "apps/rootlight-web/frontend/scripts/write-third-party-notices.mjs"),
    (8, "apps/rootlight-web/frontend/scripts/verify-release-assets.mjs"),
    (9, "packaging/npm/remove-cli-bridge.mjs"),
    (10, "packaging/npm/remove-cli-bridge.mjs"),
    (11, "packaging/npm/run-native.mjs"),
    (12, "packaging/npm/run-native.mjs"),
)


def _fields(text: str, indent: int) -> dict[str, str]:
    entries = re.findall(rf"(?m)^{' ' * indent}([\w-]+):([^\n]*)$", text)
    if len(entries) != len(dict(entries)):
        raise AssertionError("Duplicate workflow mapping key")
    return {key: value.split(" #", 1)[0].strip() for key, value in entries}


def _block(text: str, key: str, indent: int) -> str:
    matches = list(re.finditer(rf"(?m)^{' ' * indent}{re.escape(key)}:[^\n]*\n", text))
    if len(matches) != 1:
        raise AssertionError(f"Expected exactly one {key} block")
    lines = []
    for line in text[matches[0].end() :].splitlines():
        if line.strip() and not line.lstrip().startswith("#"):
            if len(line) - len(line.lstrip()) <= indent:
                break
        lines.append(line)
    return "\n".join(lines) + "\n"


def _steps(job: str) -> list[str]:
    starts = list(re.finditer(r"(?m)^      - (?:name|uses):", job))
    return [
        job[start.start() : starts[index + 1].start() if index + 1 < len(starts) else len(job)]
        for index, start in enumerate(starts)
    ]


def _action(job: str, name: str) -> str:
    matches = [step for step in _steps(job) if f"uses: {name}@" in step]
    if len(matches) != 1:
        raise AssertionError(f"Expected exactly one {name} action")
    return matches[0]


def _inputs(step: str) -> dict[str, str]:
    return _fields(_block(step, "with", 8), 10)


def _script(step: str) -> str:
    if _fields(step, 8).get("run") != "|":
        raise AssertionError("Expected a literal workflow shell block")
    return textwrap.dedent(_block(step, "run", 8))


def _bash(script: str) -> str:
    executable = shutil.which("bash")
    if os.name == "nt":
        # Windows' system bash is a WSL launcher; use Git Bash for native tests.
        git = shutil.which("git")
        candidate = Path(git).parent.parent / "bin/bash.exe" if git else None
        if candidate is not None and candidate.is_file():
            executable = str(candidate)
    if executable is None:
        raise AssertionError("Bash is required to execute the CodeQL gate regression")
    result = subprocess.run(
        [executable, "--noprofile", "--norc"],
        input=script,
        text=True,
        capture_output=True,
        check=True,
        timeout=30,
        cwd=REPOSITORY,
    )
    return result.stdout


class CodeQLWorkflowTests(unittest.TestCase):
    """Check published workflow contracts without running CodeQL or GitHub APIs."""

    def setUp(self) -> None:
        self.workflow = (REPOSITORY / ".github/workflows/codeql.yml").read_text(encoding="utf-8")
        self.full = (REPOSITORY / ".github/workflows/codeql-full.yml").read_text(encoding="utf-8")
        self.source = (REPOSITORY / ".github/codeql/source.yml").read_text(encoding="utf-8")
        self.jobs = workflow_jobs(self.workflow)
        self.full_jobs = workflow_jobs(self.full)

    def test_required_gate_executes_the_complete_result_truth_table(self) -> None:
        gate = self.jobs["codeql-required"]
        self.assertEqual(_fields(gate, 4)["if"], "${{ always() && !cancelled() }}")
        self.assertEqual(_fields(gate, 4)["needs"], "[analyze-upload, analyze-pull-request]")
        self.assertEqual(len(_steps(gate)), 1)
        step = _steps(gate)[0]
        self.assertEqual(
            _fields(_block(step, "env", 8), 10),
            {
                "EVENT_NAME": "${{ github.event_name }}",
                "UPLOAD_RESULT": "${{ needs.analyze-upload.result }}",
                "PULL_REQUEST_RESULT": "${{ needs.analyze-pull-request.result }}",
            },
        )
        states = ("success", "failure", "cancelled", "skipped", "", "queued", "in_progress")
        cases = list(itertools.product(("push", "pull_request", "workflow_dispatch"), states, states))
        shell = "codeql_gate() {\n" + _script(step) + "\n}\n"
        for index, (event, upload, pull_request) in enumerate(cases):
            shell += (
                f"( EVENT_NAME={shlex.quote(event)}; UPLOAD_RESULT={shlex.quote(upload)}; "
                f"PULL_REQUEST_RESULT={shlex.quote(pull_request)}; codeql_gate ) >/dev/null 2>&1\n"
                f"printf '{index}:%s\\n' \"$?\"\n"
            )
        actual = _bash(shell).splitlines()
        self.assertEqual(len(actual), len(cases))
        for index, (event, upload, pull_request) in enumerate(cases):
            expected = (
                (event != "pull_request" and upload == "success" and pull_request == "skipped")
                or (event == "pull_request" and upload == "skipped" and pull_request == "success")
            )
            with self.subTest(event=event, upload=upload, pull_request=pull_request):
                ordinal, exit_code = actual[index].split(":")
                self.assertEqual(int(ordinal), index)
                self.assertEqual(int(exit_code) == 0, expected)

    def test_language_matrix_preserves_analysis_identity_and_real_tool_selection(self) -> None:
        self.assertEqual(set(self.jobs), {"analyze-upload", "analyze-pull-request", "codeql-required"})
        self.assertEqual(set(self.full_jobs), {"analyze-upload"})
        expected_matrix = """\
        include:
          - language: rust
            build-mode: none
          - language: javascript-typescript
            build-mode: none
"""
        for job in (self.jobs["analyze-upload"], self.jobs["analyze-pull-request"], self.full_jobs["analyze-upload"]):
            with self.subTest(job=_fields(job, 4)["name"]):
                self.assertEqual(_block(job, "matrix", 6), expected_matrix)
                self.assertEqual(_fields(_block(job, "strategy", 4), 6)["fail-fast"], "false")
                acquisition = [step for step in _steps(job) if "Acquire authenticated scanner" in step]
                self.assertEqual(len(acquisition), 1)
                self.assertEqual(_fields(_block(acquisition[0], "env", 8), 10), {"LANGUAGE": "${{ matrix.language }}"})
                for language in LANGUAGES:
                    script = (
                        "bash() { printf '%s\\n' \"$1\" \"$2\"; }\n"
                        f"LANGUAGE={shlex.quote(language)}\nRUNNER_TEMP='/test runner temp'\n"
                        + _script(acquisition[0])
                    )
                    helper = "rust" if language == "rust" else "javascript"
                    self.assertEqual(_bash(script).splitlines(), [f"scripts/setup-codeql-{helper}.sh", "/test runner temp/codeql-tools"])
                initialization = _inputs(_action(job, "github/codeql-action/init"))
                self.assertEqual(initialization["tools"], "${{ env.CODEQL_BUNDLE }}")
                self.assertEqual(initialization["languages"], "${{ matrix.language }}")
                self.assertEqual(initialization["build-mode"], "${{ matrix.build-mode }}")

    def test_pr_jobs_have_no_write_permission_or_privileged_upload(self) -> None:
        self.assertEqual(_fields(self.workflow, 0)["permissions"], "{}")
        self.assertEqual(_fields(self.full, 0)["permissions"], "{}")
        pr = self.jobs["analyze-pull-request"]
        self.assertEqual(_fields(pr, 4)["if"], "github.event_name == 'pull_request'")
        self.assertEqual(_fields(_block(pr, "permissions", 4), 6), {"actions": "read", "contents": "read"})
        self.assertNotIn("upload-sarif", pr)
        self.assertNotIn("secrets.", pr)
        analysis = _inputs(_action(pr, "github/codeql-action/analyze"))
        self.assertEqual(analysis["upload"], "never")
        self.assertEqual(analysis["upload-database"], "false")
        for job in (self.jobs["analyze-upload"], self.full_jobs["analyze-upload"]):
            self.assertEqual(_fields(job, 4)["if"], "github.event_name != 'pull_request'")
            self.assertEqual(_fields(_block(job, "permissions", 4), 6), {"actions": "read", "contents": "read", "security-events": "write"})
        self.assertNotIn("continue-on-error:", self.workflow + self.full)

    def test_successful_sarif_is_retained_before_same_identity_processed_upload(self) -> None:
        for workflow, category in ((self.workflow, "/language:${{ matrix.language }}"), (self.full, "/scope:full/language:${{ matrix.language }}")):
            job = workflow_jobs(workflow)["analyze-upload"]
            analysis = _action(job, "github/codeql-action/analyze")
            retained = _action(job, "actions/upload-artifact")
            upload = _action(job, "github/codeql-action/upload-sarif")
            self.assertLess(job.index(analysis), job.index(retained))
            self.assertLess(job.index(retained), job.index(upload))
            for step in (analysis, retained, upload):
                self.assertNotIn("if", _fields(step, 8))
            self.assertEqual(_inputs(analysis)["upload"], "never")
            self.assertEqual(_inputs(analysis)["upload-database"], "false")
            self.assertEqual(_inputs(analysis)["category"], category)
            self.assertEqual(_inputs(retained)["if-no-files-found"], "error")
            self.assertEqual(_inputs(retained)["path"], _inputs(analysis)["output"] + "/*.sarif")
            self.assertIn("${{ github.run_attempt }}", _inputs(retained)["name"])
            self.assertEqual(_inputs(upload), {"sarif_file": _inputs(analysis)["output"], "category": category, "sha": "${{ github.sha }}", "ref": "${{ github.ref }}", "wait-for-processing": "true"})

    def test_maintained_scope_keeps_alert_files_tests_and_local_dependency_patches(self) -> None:
        self.assertEqual(set(_fields(self.source, 0)), {"name", "queries", "paths-ignore"})
        self.assertEqual(_block(self.source, "queries", 0).strip(), "- uses: security-extended")
        exclusions = []
        for line in _block(self.source, "paths-ignore", 0).splitlines():
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            match = re.fullmatch(r"  - ([a-zA-Z0-9_./-]+)", line)
            self.assertIsNotNone(match, "Source exclusions must remain explicit directory roots")
            exclusions.append(match[1])
        self.assertEqual(set(exclusions), {"source-inputs/cargo", "source-inputs/producer-dependencies", "source-inputs/producers", "target", "apps/rootlight-web/frontend/node_modules", "apps/rootlight-web/frontend/dist"})
        retained = [path for _, path in RACE_LOCATIONS]
        for prefix in ("source-inputs/producer-security", "third_party"):
            paths = [p.relative_to(REPOSITORY).as_posix() for p in (REPOSITORY / prefix).rglob("*") if p.is_file() and (p.suffix == ".rs" or p.name == "grammar.js")]
            self.assertTrue(paths, f"Missing real maintained dependency witnesses: {prefix}")
            retained.extend(paths)
        for path in retained:
            with self.subTest(path=path):
                self.assertTrue((REPOSITORY / path).is_file())
                self.assertFalse(any(path == prefix or path.startswith(prefix + "/") for prefix in exclusions))
        for prefix in ("apps", "crates", "packaging", "scripts", "tests", "xtask", "fuzz"):
            self.assertFalse(any(prefix == excluded or prefix.startswith(excluded + "/") for excluded in exclusions))
        for job in (self.jobs["analyze-upload"], self.jobs["analyze-pull-request"]):
            self.assertEqual(
                _inputs(_action(job, "github/codeql-action/init")),
                {
                    "tools": "${{ env.CODEQL_BUNDLE }}",
                    "languages": "${{ matrix.language }}",
                    "build-mode": "${{ matrix.build-mode }}",
                    "config-file": ".github/codeql/source.yml",
                },
            )

    def test_complete_weekly_analysis_is_independent_and_unfiltered(self) -> None:
        full_events = _fields(_block(self.full, "on", 0), 2)
        self.assertEqual(set(full_events), {"schedule", "workflow_dispatch"})
        self.assertRegex(_block(self.full, "schedule", 2), r"(?m)^    - cron: [\"'][0-9]+ [0-9]+ \* \* [0-6][\"']$")
        self.assertEqual(set(_fields(_block(self.workflow, "on", 0), 2)), {"push", "pull_request", "workflow_dispatch"})
        main_concurrency = _fields(_block(self.workflow, "concurrency", 0), 2)
        full_concurrency = _fields(_block(self.full, "concurrency", 0), 2)
        self.assertNotEqual(main_concurrency["group"], full_concurrency["group"])
        self.assertEqual(main_concurrency["cancel-in-progress"], "true")
        self.assertEqual(full_concurrency["cancel-in-progress"], "false")
        initialization = _inputs(_action(self.full_jobs["analyze-upload"], "github/codeql-action/init"))
        self.assertEqual(initialization, {"tools": "${{ env.CODEQL_BUNDLE }}", "languages": "${{ matrix.language }}", "build-mode": "${{ matrix.build-mode }}", "queries": "security-extended"})
        for forbidden in ("paths:", "paths-ignore:", "query-filters:", "disable-default-queries:"):
            self.assertNotIn(forbidden, self.full)

    def test_scanner_archives_and_actions_are_immutable_and_policy_authorized(self) -> None:
        policy = tomllib.loads((REPOSITORY / "policy/github-actions.toml").read_text(encoding="utf-8"))
        pins = {entry["repository"]: entry["commit"] for entry in policy["actions"]}
        for action, revision in re.findall(r"uses: ([\w/-]+)@([^\s]+)", self.workflow + self.full):
            self.assertRegex(revision, r"^[0-9a-f]{40}$")
            self.assertEqual(pins[action], revision)
        for language in ("rust", "javascript"):
            helper = (REPOSITORY / f"scripts/setup-codeql-{language}.sh").read_text(encoding="utf-8")
            self.assertIn("set -euo pipefail", helper)
            self.assertRegex(helper, rf"https://github.com/github/codeql-action/releases/download/codeql-bundle-v[0-9.]+/codeql-bundle-{language}-linux64.tar.zst")
            self.assertRegex(helper, r"'[0-9a-f]{64}' \"\$archive\" \| sha256sum --check --strict")
            self.assertLess(helper.index("sha256sum --check --strict"), helper.index("tar --extract"))
            self.assertLess(helper.index("tar --extract"), helper.index('>> "$GITHUB_ENV"'))
        for workflow in ("codeql.yml", "codeql-full.yml"):
            writes = [entry for entry in policy["write_permission_jobs"] if entry["workflow"] == f".github/workflows/{workflow}"]
            self.assertEqual(writes, [{"workflow": f".github/workflows/{workflow}", "job": "analyze-upload", "condition": "github.event_name != 'pull_request'", "permissions": ["actions=read", "contents=read", "security-events=write"]}])

    def test_fast_ci_executes_this_contract_suite(self) -> None:
        ci = (REPOSITORY / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        fast = workflow_jobs(ci)["fast"]
        contracts = [step for step in _steps(fast) if "- name: Architecture and contracts\n" in step]
        self.assertEqual(len(contracts), 1)
        self.assertEqual(_script(contracts[0]).splitlines().count("python -B scripts/test_codeql_workflow.py"), 1)


if __name__ == "__main__":
    unittest.main()
