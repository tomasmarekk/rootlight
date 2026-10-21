#!/usr/bin/env python3
"""Exercise source-bundle shell boundaries without Cargo or privileged operations.

An isolated command inventory captures namespace arguments and replaces the TCP
sentinel, so these tests do not claim actual kernel-enforced network denial.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
OFFLINE_SCRIPT = SCRIPTS / "run-offline-tests.sh"
NAMESPACE_SCRIPT = SCRIPTS / "run-in-network-namespace.sh"
PREPARE_SCRIPT = SCRIPTS / "prepare-offline.sh"
SUPPORTED = sys.platform.startswith("linux") and shutil.which("bash") is not None
CACHE_MARKER = (
    b"Signature: 8a477f597d28d172789f06886806bc55\n"
    b"# This file is a cache directory tag created by cargo.\n"
    b"# For information about cache directory tags see https://bford.info/cachedir/\n"
)

_MOCK_COMMAND = r'''"""Capture a shell command without executing Cargo or namespace operations."""
import json
import os
from pathlib import Path
import sys

command = Path(sys.argv[0]).name
with Path(os.environ["MOCK_CALLS"]).open("a", encoding="utf-8") as output:
    output.write(json.dumps({"command": command, "args": sys.argv[1:]}) + "\n")
if command == "id":
    print(os.environ["MOCK_UID"])
elif command == "bash":
    if sys.argv[1:] != ["-c", "exec 3<>/dev/tcp/1.1.1.1/443"]:
        raise SystemExit(97)
    if os.environ["MOCK_EGRESS"] == "blocked":
        print("Network is unreachable", file=sys.stderr)
        raise SystemExit(1)
    if os.environ["MOCK_EGRESS"] == "refused":
        print("Connection refused", file=sys.stderr)
        raise SystemExit(1)
elif command == "cargo":
    if sys.argv[-1] == "id-vectors":
        print('{"fixture":true}')
    cargo_home = Path(os.environ["CARGO_HOME"])
    registry = cargo_home / "registry"
    state = os.environ.get("MOCK_CARGO_CACHE_STATE", "marker")
    if state == "linked-registry" and not registry.is_symlink():
        linked_registry = cargo_home.parent / (cargo_home.name + " linked registry")
        linked_registry.mkdir(exist_ok=True)
        registry.symlink_to(linked_registry, target_is_directory=True)
    registry.mkdir(exist_ok=True)
    marker = registry / "CACHEDIR.TAG"
    marker_bytes = os.environ["MOCK_CACHE_MARKER"].encode("ascii")
    if state == "linked-marker" and not marker.is_symlink():
        linked_marker = cargo_home / "linked marker"
        linked_marker.write_bytes(marker_bytes)
        marker.symlink_to(linked_marker)
    if state == "wrong-marker":
        marker_bytes = b"unrecognized cache marker\n"
    elif state == "missing-final-lf":
        marker_bytes = marker_bytes[:-1]
    elif state == "extra-final-lf":
        marker_bytes += b"\n"
    marker.write_bytes(marker_bytes)
    if state == "extra-entry":
        (registry / "unexpected-entry").write_bytes(b"external cache input")
    elif state == "git":
        (cargo_home / "git").mkdir(exist_ok=True)
    if os.environ.get("MOCK_CARGO_POISON_CACHE") == "1":
        (registry / "src").mkdir(exist_ok=True)
elif command in ("env", "setpriv"):
    raise SystemExit(98)
elif command not in ("unshare", "python3"):
    raise SystemExit(99)
'''

_MOCK_FIND = r'''"""Delegate directory inspection unless a fixture requests silent failure."""
import os
from pathlib import Path
import sys

failure = os.environ.get("MOCK_FIND_FAIL")
directories = {
    "cargo-home": os.environ["CARGO_HOME"],
    "target": os.environ["CARGO_TARGET_DIR"],
    "registry": str(Path(os.environ["CARGO_HOME"]) / "registry"),
}
if failure in directories and directories[failure] in sys.argv[1:]:
    raise SystemExit(73)
os.execv(os.environ["MOCK_REAL_FIND"], ["find", *sys.argv[1:]])
'''


@unittest.skipUnless(SUPPORTED, "source-bundle shell boundary tests require Linux Bash")
class SourceBundleGateTests(unittest.TestCase):
    """Verify fail-closed shell guards with isolated filesystem and command fixtures."""

    def setUp(self) -> None:
        """Provision command mocks and empty caches solely inside a temporary root."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.bin = self.root / "mock commands"
        self.home = self.root / "runner home"
        self.cargo_home = self.root / "fresh cargo home"
        self.target = self.root / "fresh target"
        for directory in (self.bin, self.home, self.cargo_home, self.target):
            directory.mkdir()
        self.bash = shutil.which("bash")
        assert self.bash is not None
        self.calls = self.root / "calls.jsonl"
        for command in ("bash", "cargo", "python3", "id", "unshare", "setpriv", "env"):
            executable = self.bin / command
            executable.write_text(
                f"#!{sys.executable}\n" + _MOCK_COMMAND, encoding="utf-8"
            )
            executable.chmod(0o700)
        real_find = shutil.which("find")
        if real_find is None:
            self.skipTest("required Linux utility is unavailable: find")
        find_mock = self.bin / "find"
        find_mock.write_text(f"#!{sys.executable}\n" + _MOCK_FIND, encoding="utf-8")
        find_mock.chmod(0o700)
        for command in ("grep", "cmp"):
            executable = shutil.which(command)
            if executable is None:
                self.skipTest(f"required Linux utility is unavailable: {command}")
            (self.bin / command).symlink_to(executable)
        fixture = self.root / "tests/fixtures/id-vectors.json"
        fixture.parent.mkdir(parents=True)
        fixture.write_text('{"fixture":true}\n', encoding="utf-8")
        self.environment = {
            "HOME": str(self.home),
            "PATH": str(self.bin),
            "LANG": "C",
            "PYTHONDONTWRITEBYTECODE": "1",
            "MOCK_CALLS": str(self.calls),
            "MOCK_UID": "0",
            "MOCK_EGRESS": "blocked",
            "MOCK_CACHE_MARKER": CACHE_MARKER.decode("ascii"),
            "MOCK_REAL_FIND": real_find,
            "CARGO_HOME": str(self.cargo_home),
            "CARGO_TARGET_DIR": str(self.target),
            "RUSTUP_HOME": str(self.home / "toolchain"),
            "REQUIRE_BLOCKED_EGRESS": "1",
            "SUDO_UID": "1111",
            "SUDO_GID": "2222",
            "RUNNER_HOME": str(self.home),
            "RUNNER_PATH": str(self.bin),
            "RUNNER_CARGO_HOME": str(self.cargo_home),
            "RUNNER_TARGET_DIR": str(self.target),
        }

    def _run(
        self, script: Path, arguments: list[str] | None = None,
        *, overrides: dict[str, str | None] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        environment = dict(self.environment)
        for name, value in (overrides or {}).items():
            if value is None:
                environment.pop(name, None)
            else:
                environment[name] = value
        return subprocess.run(
            [self.bash, str(script), *(arguments or [])],
            cwd=self.root, env=environment, capture_output=True, text=True,
            timeout=10, check=False,
        )

    def _calls(self) -> list[dict]:
        if not self.calls.exists():
            return []
        return [json.loads(line) for line in self.calls.read_text(encoding="utf-8").splitlines()]

    def _assert_rejected_before_build(
        self, result: subprocess.CompletedProcess[str], diagnostic: str,
    ) -> None:
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(diagnostic, result.stderr)
        self.assertFalse(
            any(call["command"] in ("cargo", "python3") for call in self._calls()),
            self._calls(),
        )

    def test_offline_gate_requires_independent_egress_denial(self) -> None:
        for value in (None, "0", "2"):
            with self.subTest(value=value):
                result = self._run(OFFLINE_SCRIPT, overrides={"REQUIRE_BLOCKED_EGRESS": value})
                self._assert_rejected_before_build(
                    result, "the source-bundle gate requires independent egress denial"
                )
        self.assertEqual(self._calls(), [])

    def test_preparation_exports_sources_without_enabling_acquisition(self) -> None:
        result = self._run(PREPARE_SCRIPT)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self._calls(), [{
            "command": "python3",
            "args": [
                "scripts/prepare_rust_source_bundle.py",
                "--derive-cargo-members-from-archives",
                "--destination", "target/rust-source-bundle/rootlight",
                "--archive", "target/rust-source-bundle/rootlight-rust-sources.tar.gz",
            ],
        }])
        self.assertEqual(list(self.cargo_home.iterdir()), [])
        self.assertEqual(list(self.target.iterdir()), [])

    def test_offline_gate_rejects_nonempty_cargo_home_without_altering_it(self) -> None:
        sentinel = self.cargo_home / "retained-cache-input"
        sentinel.write_bytes(b"retain existing cache")
        self._assert_rejected_before_build(
            self._run(OFFLINE_SCRIPT),
            f"acceptance requires an existing empty directory: {self.cargo_home}",
        )
        self.assertEqual(sentinel.read_bytes(), b"retain existing cache")
        self.assertEqual(list(self.cargo_home.iterdir()), [sentinel])

    def test_offline_gate_rejects_nonempty_target_without_altering_it(self) -> None:
        sentinel = self.target / "retained-build-output"
        sentinel.write_bytes(b"retain existing build output")
        self._assert_rejected_before_build(
            self._run(OFFLINE_SCRIPT),
            f"acceptance requires an existing empty directory: {self.target}",
        )
        self.assertEqual(sentinel.read_bytes(), b"retain existing build output")
        self.assertEqual(list(self.target.iterdir()), [sentinel])

    def test_offline_gate_rejects_symlink_cargo_home(self) -> None:
        original = self.root / "original cargo home"
        self.cargo_home.rename(original)
        self.cargo_home.symlink_to(original, target_is_directory=True)
        self._assert_rejected_before_build(
            self._run(OFFLINE_SCRIPT),
            f"acceptance requires an existing empty directory: {self.cargo_home}",
        )
        self.assertTrue(self.cargo_home.is_symlink())
        self.assertEqual(list(original.iterdir()), [])

    def test_offline_gate_rejects_symlink_target(self) -> None:
        original = self.root / "original target"
        self.target.rename(original)
        self.target.symlink_to(original, target_is_directory=True)
        self._assert_rejected_before_build(
            self._run(OFFLINE_SCRIPT),
            f"acceptance requires an existing empty directory: {self.target}",
        )
        self.assertEqual(list(original.iterdir()), [])

    def test_offline_gate_requires_explicit_cache_and_target(self) -> None:
        for variable in ("CARGO_HOME", "CARGO_TARGET_DIR"):
            with self.subTest(variable=variable):
                self._assert_rejected_before_build(
                    self._run(OFFLINE_SCRIPT, overrides={variable: None}),
                    f"{variable} is required",
                )

    def test_offline_gate_rejects_missing_empty_directory(self) -> None:
        self.cargo_home.rmdir()
        self._assert_rejected_before_build(
            self._run(OFFLINE_SCRIPT),
            f"acceptance requires an existing empty directory: {self.cargo_home}",
        )
        self.assertFalse(self.cargo_home.exists())

    def test_successful_egress_sentinel_fails_before_source_or_cargo_checks(self) -> None:
        self._assert_rejected_before_build(
            self._run(OFFLINE_SCRIPT, overrides={"MOCK_EGRESS": "success"}),
            "egress sentinel did not fail closed",
        )
        self.assertEqual(
            self._calls(),
            [{"command": "bash", "args": ["-c", "exec 3<>/dev/tcp/1.1.1.1/443"]}],
        )

    def test_connection_refused_does_not_prove_egress_denial(self) -> None:
        self._assert_rejected_before_build(
            self._run(OFFLINE_SCRIPT, overrides={"MOCK_EGRESS": "refused"}),
            "egress sentinel did not fail closed",
        )
        self.assertIn("Connection refused", self._run(
            OFFLINE_SCRIPT, overrides={"MOCK_EGRESS": "refused"}
        ).stderr)

    def test_accepted_mock_boundary_checks_sources_before_and_after_cargo(self) -> None:
        result = self._run(OFFLINE_SCRIPT)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self._calls()
        verifications = [index for index, call in enumerate(calls) if call["command"] == "python3"]
        cargo_indices = [index for index, call in enumerate(calls) if call["command"] == "cargo"]
        self.assertEqual(len(verifications), 2)
        self.assertLess(verifications[0], min(cargo_indices))
        self.assertGreater(verifications[1], max(cargo_indices))
        for index in verifications:
            self.assertEqual(calls[index]["args"], [
                "scripts/prepare_rust_source_bundle.py", "--destination", str(self.root), "--verify-only",
            ])
        for index in cargo_indices:
            self.assertIn("--offline", calls[index]["args"])
            self.assertIn("--locked", calls[index]["args"])
        registry = self.cargo_home / "registry"
        self.assertEqual(list(self.cargo_home.iterdir()), [registry])
        marker = registry / "CACHEDIR.TAG"
        self.assertEqual(list(registry.iterdir()), [marker])
        self.assertEqual(marker.read_bytes(), CACHE_MARKER)
        self.assertEqual(len(CACHE_MARKER), 177)

    def test_gate_rejects_cache_population_after_mock_build(self) -> None:
        result = self._run(OFFLINE_SCRIPT, overrides={"MOCK_CARGO_POISON_CACHE": "1"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("the build populated an external-source cache", result.stderr)
        self.assertEqual(sum(call["command"] == "python3" for call in self._calls()), 1)
        self.assertTrue((self.cargo_home / "registry/src").is_dir())

    def test_gate_rejects_malformed_administrative_markers(self) -> None:
        for state in ("wrong-marker", "missing-final-lf", "extra-final-lf"):
            with self.subTest(state=state):
                self._assert_cache_state_rejected(state)

    def test_gate_rejects_linked_administrative_marker(self) -> None:
        self._assert_cache_state_rejected("linked-marker")

    def test_gate_rejects_linked_registry_directory(self) -> None:
        self._assert_cache_state_rejected("linked-registry")

    def test_gate_rejects_extra_registry_entry(self) -> None:
        self._assert_cache_state_rejected("extra-entry")

    def test_gate_rejects_git_cache_even_with_exact_registry_marker(self) -> None:
        self._assert_cache_state_rejected("git")

    def test_offline_gate_rejects_silent_initial_inventory_failure(self) -> None:
        for directory in ("cargo-home", "target"):
            with self.subTest(directory=directory):
                failed_path = self.cargo_home if directory == "cargo-home" else self.target
                self._assert_rejected_before_build(
                    self._run(OFFLINE_SCRIPT, overrides={"MOCK_FIND_FAIL": directory}),
                    f"acceptance could not inspect the empty directory: {failed_path}",
                )

    def test_gate_rejects_silent_registry_inventory_failure(self) -> None:
        result = self._run(OFFLINE_SCRIPT, overrides={"MOCK_FIND_FAIL": "registry"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("acceptance could not inspect the registry directory", result.stderr)
        self.assertEqual(sum(call["command"] == "python3" for call in self._calls()), 1)

    def _assert_cache_state_rejected(self, state: str) -> None:
        previous_verifications = sum(call["command"] == "python3" for call in self._calls())
        with (
            tempfile.TemporaryDirectory(dir=self.root) as cargo_home,
            tempfile.TemporaryDirectory(dir=self.root) as target,
        ):
            result = self._run(OFFLINE_SCRIPT, overrides={
                "CARGO_HOME": cargo_home, "CARGO_TARGET_DIR": target,
                "MOCK_CARGO_CACHE_STATE": state,
            })
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("the build populated an external-source cache", result.stderr)
            self.assertEqual(
                sum(call["command"] == "python3" for call in self._calls()),
                previous_verifications + 1,
            )

    def test_namespace_requires_script_argument(self) -> None:
        result = self._run(NAMESPACE_SCRIPT)
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)
        self.assertEqual(self._calls(), [])

    def test_namespace_requires_root_without_invoking_unshare(self) -> None:
        result = self._run(NAMESPACE_SCRIPT, ["fixture.sh"], overrides={"MOCK_UID": "1000"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("network namespace setup requires root", result.stderr)
        self.assertEqual(self._calls(), [{"command": "id", "args": ["-u"]}])

    def test_namespace_requires_both_sudo_identity_values(self) -> None:
        for variable in ("SUDO_UID", "SUDO_GID"):
            with self.subTest(variable=variable):
                result = self._run(NAMESPACE_SCRIPT, ["fixture.sh"], overrides={variable: None})
                self.assertEqual(result.returncode, 1)
                self.assertIn("SUDO_UID and SUDO_GID are required to drop privileges", result.stderr)
        self.assertTrue(all(call["command"] == "id" for call in self._calls()))

    def test_namespace_requires_explicit_runner_inputs(self) -> None:
        for variable in ("RUNNER_HOME", "RUNNER_PATH", "RUNNER_CARGO_HOME", "RUNNER_TARGET_DIR"):
            with self.subTest(variable=variable):
                result = self._run(NAMESPACE_SCRIPT, ["fixture.sh"], overrides={variable: None})
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"{variable} is required", result.stderr)
        self.assertTrue(all(call["command"] == "id" for call in self._calls()))

    def test_namespace_passes_chosen_empty_homes_and_drops_all_capabilities(self) -> None:
        legacy = self.home / ".cargo"
        legacy.mkdir()
        retained = legacy / "retained-source-cache"
        retained.write_bytes(b"must not be used")
        arguments = ["fixture.sh", "argument with spaces", "value=retained"]
        result = self._run(NAMESPACE_SCRIPT, arguments, overrides={"CARGO_HOME": str(legacy)})
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self._calls()
        self.assertEqual([call["command"] for call in calls], ["id", "unshare"])
        argv = calls[-1]["args"]
        self.assertEqual(argv[:3], ["--net", "--", "setpriv"])
        self.assertEqual(argv[3:12], [
            "--reuid", "1111", "--regid", "2222", "--clear-groups",
            "--bounding-set=-all", "--inh-caps=-all", "--ambient-caps=-all", "--no-new-privs",
        ])
        environment = self._namespace_environment(argv)
        self.assertEqual(environment["CARGO_HOME"], str(self.cargo_home))
        self.assertEqual(environment["CARGO_TARGET_DIR"], str(self.target))
        self.assertEqual(environment["HOME"], str(self.home))
        self.assertEqual(environment["PATH"], str(self.bin))
        self.assertNotEqual(environment["CARGO_HOME"], str(legacy))
        self.assertEqual(environment["RUSTUP_HOME"], str(self.home / ".rustup"))
        self.assertEqual(environment["REQUIRE_BLOCKED_EGRESS"], "1")
        self.assertEqual(environment["CARGO_NET_OFFLINE"], "true")
        self.assertEqual(environment["CARGO_BUILD_JOBS"], "2")
        self.assertEqual(argv[-4:], ["bash", *arguments])
        self.assertEqual(retained.read_bytes(), b"must not be used")
        self.assertEqual(list(self.cargo_home.iterdir()), [])
        self.assertEqual(list(self.target.iterdir()), [])

    def test_namespace_preserves_explicit_toolchain_and_job_limits(self) -> None:
        toolchain = self.root / "provisioned toolchain"
        result = self._run(NAMESPACE_SCRIPT, ["fixture.sh"], overrides={
            "RUNNER_RUSTUP_HOME": str(toolchain), "RUNNER_BUILD_JOBS": "3",
        })
        self.assertEqual(result.returncode, 0, result.stderr)
        environment = self._namespace_environment(self._calls()[-1]["args"])
        self.assertEqual(environment["RUSTUP_HOME"], str(toolchain))
        self.assertEqual(environment["CARGO_BUILD_JOBS"], "3")

    def _namespace_environment(self, argv: list[str]) -> dict[str, str]:
        start = argv.index("env") + 1
        end = argv.index("bash", start)
        return dict(argument.split("=", 1) for argument in argv[start:end])


if __name__ == "__main__":
    unittest.main()
