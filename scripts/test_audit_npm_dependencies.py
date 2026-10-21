"""Exercise full-pin inclusion and fail-closed npm advisory transport behavior."""
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import audit_npm_dependencies as audit


class AdvisoryTests(unittest.TestCase):
    """Model transport responses without querying the registry from unit tests."""

    def response(self, payload, *, endpoint=audit.ENDPOINT):
        response = io.BytesIO(json.dumps(payload).encode())
        response.geturl = lambda: endpoint
        return response

    def test_no_advisories_and_only_exact_pins_are_sent(self):
        pins = {"@scope/package": ["1.0.0", "2.0.0"]}
        with patch.object(audit, "urlopen", return_value=self.response({})) as send:
            self.assertEqual(audit.query_advisories(pins), {})
        request = send.call_args.args[0]
        self.assertEqual(request.full_url, audit.ENDPOINT)
        self.assertEqual(request.method, "POST")
        self.assertEqual(json.loads(request.data), pins)

    def test_advisories_are_retained_and_fail_the_command(self):
        finding = {"sample": [{"name": "sample", "id": 42, "vulnerable_versions": "<2.0.0"}]}
        with patch.object(audit, "package_inventory", return_value={"sample": ["1.0.0"]}), \
             patch.object(audit, "urlopen", return_value=self.response(finding)), \
             patch("sys.stdout", new_callable=io.StringIO) as output:
            self.assertEqual(audit.main([]), 1)
            self.assertEqual(json.loads(output.getvalue())["advisories"], finding)

    def test_response_key_binds_records_without_redundant_package_names(self):
        finding = {"sample": [{"id": 42, "vulnerable_versions": "<2.0.0",
                               "severity": "high", "title": "Source audit regression"}]}
        with patch.object(audit, "package_inventory", return_value={"sample": ["1.0.0"]}), \
             patch.object(audit, "urlopen", return_value=self.response(finding)), \
             patch("sys.stdout", new_callable=io.StringIO) as output:
            self.assertEqual(audit.main([]), 1)
            self.assertEqual(json.loads(output.getvalue())["advisories"], finding)

    def test_malformed_foreign_and_redirected_responses_reject(self):
        for payload in ([], {"foreign": []}, {"sample": []}, {"sample": [{"id": 1}]},
                        *({"sample": [{"id": 1, "vulnerable_versions": "<2.0.0", "name": name}]}
                          for name in ("foreign", None, 17))):
            with self.subTest(payload=payload), patch.object(audit, "urlopen", return_value=self.response(payload)):
                with self.assertRaises(ValueError):
                    audit.query_advisories({"sample": ["1.0.0"]})
        with patch.object(audit, "urlopen", return_value=self.response({}, endpoint="https://example.invalid")):
            with self.assertRaisesRegex(ValueError, "redirected"):
                audit.query_advisories({"sample": ["1.0.0"]})

    def test_network_failure_is_not_a_clean_audit(self):
        with patch.object(audit, "package_inventory", return_value={"sample": ["1.0.0"]}), \
             patch.object(audit, "urlopen", side_effect=OSError("unavailable")), \
             patch("sys.stderr", new_callable=io.StringIO):
            self.assertEqual(audit.main([]), 1)

    def test_native_authentication_failure_never_reaches_advisory_service(self):
        with tempfile.TemporaryDirectory() as temporary:
            project = Path(temporary)
            registry = project / "source-inputs/npm/upstream-package-lock.json"
            registry.parent.mkdir(parents=True)
            registry.write_text('{"packages": {"": {}}}')
            with patch.object(audit.frontend, "verify_local_bundle"), \
                 patch.object(audit.native, "verify_bundle", side_effect=ValueError("tampered native overlay")), \
                 patch.object(audit, "urlopen") as send, patch("sys.stderr", new_callable=io.StringIO):
                self.assertEqual(audit.main(["--project", str(project)]), 1)
                send.assert_not_called()

    def test_development_optional_and_repeated_versions_are_included(self):
        with tempfile.TemporaryDirectory() as temporary:
            project = Path(temporary)
            registry = project / "source-inputs/npm/upstream-package-lock.json"
            registry.parent.mkdir(parents=True)
            registry.write_text(json.dumps({"packages": {"": {},
                "node_modules/dev-tool": {"version": "1.0.0", "dev": True},
                "node_modules/optional": {"version": "2.0.0", "optional": True},
                "node_modules/parent/node_modules/dev-tool": {"version": "3.0.0", "dev": True}}}))
            native = project / audit.native.BUNDLE_PATH / "overlays"
            native.mkdir(parents=True)
            (native / "package.json").write_bytes(b"{}")
            (native / "yarn.lock").write_bytes(b"fixture")
            with patch.object(audit.frontend, "verify_local_bundle"), \
                 patch.object(audit.native, "verify_bundle", return_value={"pin_graph": {"packages": [
                     {"name": "dev-tool", "version": "3.0.0"}, {"name": "producer", "version": "4.0.0"}]}}):
                self.assertEqual(audit.package_inventory(project), {
                    "dev-tool": ["1.0.0", "3.0.0"], "optional": ["2.0.0"], "producer": ["4.0.0"]})


if __name__ == "__main__":
    unittest.main()
