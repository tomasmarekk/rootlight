"""Exercise full-pin inclusion and fail-closed npm advisory transport behavior."""
import io
import json
from pathlib import Path
import tempfile
import ssl
from urllib.error import HTTPError, URLError
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


    def test_transient_open_failures_replay_the_same_complete_request(self):
        pins = {"@scope/package": ["1.0.0", "2.0.0"], "optional": ["3.0.0"]}
        errors = [TimeoutError("open timed out"), ConnectionResetError("reset"),
                  URLError(TimeoutError("wrapped timeout")),
                  URLError(ConnectionRefusedError("wrapped connection"))]
        for error in errors:
            response = self.response({})
            with self.subTest(error=type(error).__name__), \
                 patch.object(audit, "urlopen", side_effect=[error, response]) as send, \
                 patch("time.sleep") as sleep, \
                 patch("sys.stderr", new_callable=io.StringIO):
                self.assertEqual(audit.query_advisories(pins), {})
                self.assertEqual(send.call_count, 2)
                requests = [item.args[0] for item in send.call_args_list]
                self.assertIs(requests[0], requests[1])
                for item in send.call_args_list:
                    self.assertEqual(item.args[0].full_url, audit.ENDPOINT)
                    self.assertEqual(item.args[0].method, "POST")
                    self.assertEqual(json.loads(item.args[0].data), pins)
                    self.assertEqual(item.kwargs, {"timeout": 60})
                sleep.assert_called_once_with(1)
                self.assertTrue(response.closed)

    def test_response_read_transients_close_before_recovery(self):
        for error in [TimeoutError("read timed out"), ConnectionResetError("read reset")]:
            first, second = self.response({}), self.response({})
            with self.subTest(error=type(error).__name__), \
                 patch.object(first, "read", side_effect=error) as read, \
                 patch.object(audit, "urlopen", side_effect=[first, second]) as send, \
                 patch("time.sleep", side_effect=lambda _: self.assertTrue(first.closed)), \
                 patch("sys.stderr", new_callable=io.StringIO):
                self.assertEqual(audit.query_advisories({"sample": ["1.0.0"]}), {})
                self.assertEqual(send.call_count, 2)
                read.assert_called_once_with(audit.MAX_RESPONSE + 1)
                self.assertTrue(first.closed and second.closed)

    def test_retryable_http_errors_close_their_bodies_before_recovery(self):
        for status in (429, 500, 502, 503, 504):
            body, response = io.BytesIO(b"unavailable"), self.response({})
            error = HTTPError(audit.ENDPOINT, status, "temporary", {}, body)
            with self.subTest(status=status), \
                 patch.object(audit, "urlopen", side_effect=[error, response]) as send, \
                 patch("time.sleep", side_effect=lambda _: self.assertTrue(body.closed)), \
                 patch("sys.stderr", new_callable=io.StringIO):
                self.assertEqual(audit.query_advisories({"sample": ["1.0.0"]}), {})
                self.assertEqual(send.call_count, 2)
                self.assertTrue(body.closed and response.closed)

    def test_exhausted_transport_is_nonzero_without_clean_stdout(self):
        for kind in ("timeout", "http", "read"):
            bodies = []
            if kind == "timeout":
                failures = [TimeoutError("unavailable") for _ in range(3)]
            elif kind == "http":
                bodies = [io.BytesIO(b"unavailable") for _ in range(3)]
                failures = [HTTPError(audit.ENDPOINT, 503, "temporary", {}, body) for body in bodies]
            else:
                bodies = [self.response({}) for _ in range(3)]
                for response in bodies:
                    response.read = lambda _: (_ for _ in ()).throw(TimeoutError("read timed out"))
                failures = bodies
            with self.subTest(kind=kind), \
                 patch.object(audit, "package_inventory", return_value={"sample": ["1.0.0"]}), \
                 patch.object(audit, "urlopen", side_effect=failures) as send, \
                 patch("time.sleep") as sleep, \
                 patch("sys.stdout", new_callable=io.StringIO) as output, \
                 patch("sys.stderr", new_callable=io.StringIO) as errors:
                self.assertEqual(audit.main([]), 1)
                self.assertEqual(send.call_count, 3)
                self.assertEqual([item.args[0] for item in sleep.call_args_list], [1, 2])
                self.assertEqual(output.getvalue(), "")
                self.assertIn("npm advisory audit failed:", errors.getvalue())
                self.assertTrue(all(body.closed for body in bodies))

    def test_permanent_and_tls_errors_are_never_retried(self):
        errors = [OSError("unavailable"), URLError("unavailable"),
                  ssl.SSLCertVerificationError("untrusted certificate"),
                  URLError(ssl.SSLCertVerificationError("untrusted certificate"))]
        bodies = []
        for status in (400, 401, 403, 404, 501, 505):
            body = io.BytesIO(b"permanent")
            bodies.append(body)
            errors.append(HTTPError(audit.ENDPOINT, status, "permanent", {}, body))
        redirected_body = io.BytesIO(b"redirected")
        bodies.append(redirected_body)
        errors.append(HTTPError("https://example.invalid", 503, "foreign", {}, redirected_body))
        for error in errors:
            with self.subTest(error=type(error).__name__, status=getattr(error, "code", None)), \
                 patch.object(audit, "urlopen", side_effect=error) as send, \
                 patch("time.sleep") as sleep:
                with self.assertRaises(OSError) as caught:
                    audit.query_advisories({"sample": ["1.0.0"]})
                self.assertIs(caught.exception, error)
                send.assert_called_once()
                sleep.assert_not_called()
        self.assertTrue(all(body.closed for body in bodies))

    def test_valid_finding_after_recovery_is_not_retried_or_cleared(self):
        finding = {"sample": [{"id": 42, "vulnerable_versions": "<2.0.0"}]}
        response = self.response(finding)
        with patch.object(audit, "package_inventory", return_value={"sample": ["1.0.0"]}), \
             patch.object(audit, "urlopen", side_effect=[TimeoutError("timeout"), response]) as send, \
             patch("time.sleep") as sleep, \
             patch("sys.stdout", new_callable=io.StringIO) as output, \
             patch("sys.stderr", new_callable=io.StringIO):
            self.assertEqual(audit.main([]), 1)
            self.assertEqual(json.loads(output.getvalue())["advisories"], finding)
            self.assertEqual(send.call_count, 2)
            sleep.assert_called_once_with(1)
            self.assertTrue(response.closed)

    def test_invalid_results_after_recovery_are_never_retried(self):
        responses = [self.response([]), self.response({"foreign": [{"id": 1, "vulnerable_versions": "*"}]}),
                     self.response({"sample": []}), self.response({"sample": [{"id": 1}]}),
                     self.response({}, endpoint="https://example.invalid")]
        truncated = io.BytesIO(b'{"sample":')
        truncated.geturl = lambda: audit.ENDPOINT
        responses.append(truncated)
        for response in responses:
            with self.subTest(response=response.getvalue()), \
                 patch.object(audit, "urlopen", side_effect=[TimeoutError("timeout"), response]) as send, \
                 patch("time.sleep") as sleep, \
                 patch("sys.stderr", new_callable=io.StringIO):
                with self.assertRaises(ValueError):
                    audit.query_advisories({"sample": ["1.0.0"]})
                self.assertEqual(send.call_count, 2)
                sleep.assert_called_once_with(1)
                self.assertTrue(response.closed)

    def test_oversized_response_after_recovery_is_never_retried(self):
        response = io.BytesIO(b"x" * 33)
        response.geturl = lambda: audit.ENDPOINT
        with patch.object(audit, "MAX_RESPONSE", 32), \
             patch.object(audit, "urlopen", side_effect=[TimeoutError("timeout"), response]) as send, \
             patch("time.sleep") as sleep, \
             patch("sys.stderr", new_callable=io.StringIO):
            with self.assertRaisesRegex(ValueError, "exceeds limit"):
                audit.query_advisories({"sample": ["1.0.0"]})
            self.assertEqual(send.call_count, 2)
            sleep.assert_called_once_with(1)
            self.assertTrue(response.closed)


if __name__ == "__main__":
    unittest.main()
