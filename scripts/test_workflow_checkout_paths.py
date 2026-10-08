#!/usr/bin/env python3
"""Keep Windows Git path preparation ahead of native workflow checkouts."""

from pathlib import Path
import re
import unittest


REPOSITORY = Path(__file__).parent.parent
PREPARATION = (
    "      - name: Enable long Git paths on Windows\n"
    "        if: runner.os == 'Windows'\n"
    "        shell: pwsh\n"
    "        run: git config --global core.longpaths true\n"
)


def workflow_jobs(workflow: str) -> dict[str, str]:
    """Select top-level job blocks without interpreting embedded shell text."""
    jobs = workflow.partition("\njobs:\n")[2]
    matches = list(re.finditer(r"(?m)^  ([a-zA-Z0-9_-]+):\n", jobs))
    return {
        match[1]: jobs[match.start() : matches[index + 1].start() if index + 1 < len(matches) else len(jobs)]
        for index, match in enumerate(matches)
    }


def windows_capable(job: str) -> bool:
    return re.search(
        r"(?m)^    runs-on: windows-|^\s+- os: windows-|^\s+os: \[[^\]]*windows-",
        job,
    ) is not None


class WorkflowCheckoutPathTests(unittest.TestCase):
    def test_windows_jobs_prepare_git_before_checkout(self) -> None:
        observed = 0
        for path in sorted((REPOSITORY / ".github/workflows").glob("*.yml")):
            for name, job in workflow_jobs(path.read_text(encoding="utf-8")).items():
                if not windows_capable(job):
                    continue
                with self.subTest(workflow=path.name, job=name):
                    self.assertEqual(job.count(PREPARATION), 1)
                    self.assertLess(job.index(PREPARATION), job.index("uses: actions/checkout@"))
                    observed += 1
        self.assertGreater(observed, 0)

    def test_runner_detection_covers_static_and_matrix_jobs(self) -> None:
        self.assertTrue(windows_capable("    runs-on: windows-2025\n"))
        self.assertTrue(windows_capable("          - os: windows-2025\n"))
        self.assertTrue(windows_capable("        os: [ubuntu-24.04, windows-2025]\n"))
        self.assertFalse(windows_capable("    runs-on: ubuntu-24.04\n"))
        self.assertFalse(windows_capable("        run: echo windows-2025\n"))


if __name__ == "__main__":
    unittest.main()
