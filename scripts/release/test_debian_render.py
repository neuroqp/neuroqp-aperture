"""Render failure records retain the last native observation; no GUI acceptance."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("debian_render", Path(__file__).with_name("smoke-desktop-linux.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class RenderDiagnosticTests(unittest.TestCase):
    def run_failure(self, observation, active=False, pixels=None):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            identity = dict(source_sha="source", build_inputs_sha256="inputs", checksums_sha256="checksums", freeze_started_at="2026-01-01T00:00:00Z", deadline_at="2099-01-01T00:00:00Z", asset_path="/prepared/AppImage", asset_sha256="asset", tiny_tiff_path="/prepared/tiny.tif", tiny_tiff_sha256="tiff", tiny_tiff_width=65, tiny_tiff_height=49, driver_url="http://127.0.0.1:4446", screenshot_command=["/prepared/capture", "{output}"], readiness=dict(environment="debian12", source_sha="source", tools=[]))
            identity_path, report_path = root / "identity.json", root / "report.json"
            identity_path.write_text(json.dumps(identity))
            Path(str(report_path.with_suffix(".exit.json")) + ".pid").write_text("42")
            frame = dict(role="frame", name="Aperture", active=active, rect=[0, 0, 300, 200])
            viewer = dict(role="panel", name="Image viewer", rect=[0, 30, 300, 170])
            nodes = [(None, frame), (None, viewer)]

            def response(request, **kwargs):
                if request.full_url.endswith("/status"):
                    value = dict(ready=True)
                elif request.full_url.endswith("/session"):
                    value = dict(sessionId="owned")
                else:
                    value = observation
                return io.BytesIO(json.dumps(dict(value=value)).encode())

            with patch.object(smoke.sys, "argv", ["smoke", "--identity", str(identity_path), "--environment", "debian12", "--report", str(report_path)]), patch.object(smoke, "verify_inputs"), patch.object(smoke, "verify_driver", return_value={}), patch.object(smoke, "assert_os"), patch.object(smoke, "application_process", return_value=(42, {}, nodes)), patch.object(smoke, "native_nodes", return_value=nodes), patch.object(smoke, "urlopen", side_effect=response), patch.object(smoke.subprocess, "run", return_value=SimpleNamespace(stdout="")), patch.object(smoke, "ppm_spread", return_value=pixels), patch.object(smoke.time, "sleep", side_effect=TimeoutError("bounded observation expired")):
                self.assertEqual(smoke.main(), 1)
            report = json.loads(report_path.read_text())
            self.assertEqual(report["status"], "fail")
            self.assertFalse(report["forced_kill"])
            self.assertEqual(report["preserved_session_id"], "/session/owned")
            return report["steps"][-1]["diagnostic"]

    def ready(self):
        return dict(phase="ready", rendered_canvas=True, error=False, state=dict(phase="ready", previewReady=True, width=65, height=49, path="/prepared/tiny.tif"))

    def test_loading_or_application_error_retains_actual_observation(self):
        for observation in [dict(phase="loading", state=None), dict(phase="error", state=dict(error="actual application failure"))]:
            with self.subTest(observation=observation):
                diagnostic = self.run_failure(observation)
                self.assertEqual(diagnostic, dict(gate="application-state", observation=observation))

    def test_driver_error_is_retained_before_observation_exists(self):
        diagnostic = self.run_failure(dict(error="native observer failed"))
        self.assertEqual(diagnostic["gate"], "application-state")
        self.assertIn("native observer failed", diagnostic["observer_error"])

    def test_inactive_window_timeout_retains_ready_state_and_owned_frame(self):
        diagnostic = self.run_failure(self.ready())
        self.assertEqual(diagnostic["gate"], "foreground")
        self.assertEqual(diagnostic["observation"], self.ready())
        self.assertFalse(diagnostic["windows"][0]["active"])

    def test_blank_capture_timeout_retains_viewer_bounds_and_pixel_evidence(self):
        diagnostic = self.run_failure(self.ready(), active=True, pixels=dict(spread=0, samples=[44] * 25))
        self.assertEqual(diagnostic["gate"], "visible-pixels")
        self.assertEqual(diagnostic["viewer"]["rect"], [0, 30, 300, 170])
        self.assertEqual(diagnostic["pixels"]["spread"], 0)


if __name__ == "__main__":
    unittest.main()
