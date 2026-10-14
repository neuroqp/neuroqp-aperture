"""Bounded evidence rejects stale source, changed bytes, timeouts and missing desktops."""

import copy
import datetime as dt
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import bounded


def identity():
    now = bounded.parsed(bounded.timestamp())
    return {"schema": 1, "source_sha": "a" * 40, "build_inputs_sha256": "b" * 64, "component_inputs": {"frontend": "c" * 64, "backend": "d" * 64}, "freeze_started_at": (now - dt.timedelta(seconds=60)).isoformat(), "deadline_at": (now + dt.timedelta(seconds=1740)).isoformat(), "checksums_sha256": "e" * 64, "assets": {name: {"sha256": "f" * 64, "tiny_tiff_sha256": "1" * 64, "helper_sha256": "2" * 64, "tiny_tiff_width": 16, "tiny_tiff_height": 16, "tools": [{"name": "observer", "path": "C:/observer.exe" if name.startswith("windows") else "/usr/bin/observer", "sha256": "2" * 64}]} for name in bounded.ENVIRONMENTS}}


def smoke(expected, environment):
    now = bounded.timestamp()
    result = {**{key: expected[key] for key in ("schema", "source_sha", "build_inputs_sha256", "checksums_sha256", "freeze_started_at", "deadline_at")}, "stage": "C", "status": "pass", "forced_kill": False, "environment": environment, "asset_sha256": "f" * 64, "tiny_tiff_sha256": "1" * 64, "helper_sha256": "2" * 64, "attempts": 1, "started_at": now, "finished_at": now, "observer_tools": expected["assets"][environment]["tools"], "steps": [{"name": name, "status": "pass", "forced_kill": False, "started_at": now, "finished_at": now, "timeout_seconds": bounded.STEP_LIMITS[name], "detail": {}} for name in bounded.SMOKE_STEPS]}
    result.update(uid=1000, os={"Caption": "Windows 11"}, privilege={"is_admin": False, "session_id": 1})
    result["steps"][0]["detail"] = {"native_windows": [{}], "backend": "native-Windows-WebView2" if environment.startswith("windows") else "wayland", "installer_exit_code": 0, "installed_marker_verified": True, "cold_baseline": True, "incoming_payload_id": "5" * 64}
    result["steps"][1]["detail"] = {"dialog_input": "prepared fixed TIFF chooser response", "observation": {"phase": "ready", "error": False, "chooser_restored": True, "rendered_tiles": True, "state": {"phase": "ready", "previewReady": True, "width": 16, "height": 16}}, "pixel_observation": {"spread": 100}, "screenshot_sha256": "3" * 64}
    result["steps"][2]["detail"] = {"exit_code": 0}
    return result


class BoundedTests(unittest.TestCase):
    def test_only_complete_exact_byte_native_smoke_passes(self):
        expected = identity()
        rows = [smoke(expected, name) for name in bounded.ENVIRONMENTS]
        report = {**expected, "stage": "C", "status": "pass", "forced_kill": False, "reports": rows}
        bounded.validate_acceptance(report, expected)
        mutations = [("source_sha", "9" * 40), ("asset_sha256", "9" * 64), ("status", "inconclusive"), ("forced_kill", True), ("attempts", 3), ("helper_sha256", "9" * 64), ("checksums_sha256", "9" * 64)]
        for key, value in mutations:
            changed = copy.deepcopy(rows[0]); changed[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError): bounded.validate_smoke_report(changed, expected)
        report["reports"] = rows[:-1]
        with self.assertRaisesRegex(ValueError, "six"): bounded.validate_acceptance(report, expected)
        changed = copy.deepcopy(rows[0]); changed["steps"][1]["detail"]["pixel_observation"]["spread"] = 0
        with self.assertRaisesRegex(ValueError, "visible pixels"): bounded.validate_smoke_report(changed, expected)
        changed = copy.deepcopy(rows[0]); changed["steps"][2]["detail"]["exit_code"] = 1
        with self.assertRaisesRegex(ValueError, "exit code"): bounded.validate_smoke_report(changed, expected)

    def test_original_deadline_cannot_reset_at_promotion(self):
        expected = identity()
        bounded.validate_deadline(expected)
        expired = (bounded.parsed(expected["deadline_at"]) + dt.timedelta(seconds=1)).isoformat()
        with self.assertRaisesRegex(ValueError, "deadline exceeded"): bounded.validate_deadline(expected, expired)
        changed = {**expected, "deadline_at": (bounded.parsed(expected["deadline_at"]) + dt.timedelta(seconds=1)).isoformat()}
        with self.assertRaisesRegex(ValueError, "exactly 30"): bounded.validate_deadline(changed)

    def test_transient_retry_keeps_bytes_and_step_scope(self):
        expected = identity(); report = smoke(expected, "debian12")
        prior = copy.deepcopy(report); prior.update(status="fail", error="Prepared driver briefly unavailable")
        report.update(attempts=2, retry_reason="transient-lab", transient_diagnosis="Driver probe returned busy without any source/tool changes", prior_attempt=prior)
        bounded.validate_smoke_report(report, expected)
        report["retry_reason"] = "repaired-helper"
        with self.assertRaisesRegex(ValueError, "transient"): bounded.validate_smoke_report(report, expected)
        report.update(attempts=1); report["steps"].append(report["steps"][0])
        with self.assertRaisesRegex(ValueError, "limited"): bounded.validate_smoke_report(report, expected)

    def test_readiness_binds_changed_components_and_dev_binary(self):
        expected = identity(); now = bounded.timestamp()
        automated = [{**expected, "platform": platform, "status": "pass", "forced_kill": False, "binary_sha256": "4" * 64} for platform in ("linux", "windows")]
        native = [{**row, "suite": "native-functional", "areas": ["changed areas"], "evidence": ["receipt.json"]} for row in automated]
        labs = [{"environment": name, "status": "pass", "forced_kill": False, "native_desktop": True, "tools": [{"name": "native smoke helper", "path": "C:/observer.exe" if name.startswith("windows") else "/usr/bin/observer", "sha256": "2" * 64}], "expires_at": expected["deadline_at"], **{key: expected["assets"][name][key] for key in ("tiny_tiff_sha256", "helper_sha256", "tiny_tiff_width", "tiny_tiff_height")}} for name in bounded.ENVIRONMENTS]
        report = {**expected, "stage": "A", "status": "pass", "forced_kill": False, "completed_at": now, "automated": automated, "native_functional": native, "labs": labs}
        bounded.validate_readiness(report, expected)
        native[0]["source_sha"] = "9" * 40
        bounded.validate_readiness(report, expected)  # Unchanged component evidence can be reused.
        native[0]["component_inputs"] = {"frontend": "9" * 64, "backend": "d" * 64}
        with self.assertRaisesRegex(ValueError, "stale"): bounded.validate_readiness(report, expected)
        native[0]["component_inputs"] = expected["component_inputs"]; native[0]["source_sha"] = expected["source_sha"]; native[0]["binary_sha256"] = "9" * 64
        with self.assertRaisesRegex(ValueError, "development binary"): bounded.validate_readiness(report, expected)

    def test_cold_portable_has_bounded_ten_minute_launch(self):
        expected = identity(); report = smoke(expected, "windows11-portable")
        report["steps"][0]["timeout_seconds"] = 600
        bounded.validate_smoke_report(report, expected)
        report["steps"][0]["timeout_seconds"] = 601
        with self.assertRaisesRegex(ValueError, "bounded timeout"):
            bounded.validate_smoke_report(report, expected)
        report["steps"][0]["timeout_seconds"] = 600
        report["steps"][0]["detail"]["cold_baseline"] = False
        with self.assertRaisesRegex(ValueError, "clean extraction"):
            bounded.validate_smoke_report(report, expected)

    def test_retry_preserves_failed_receipt_and_is_once_per_release(self):
        expected = identity(); rows = [smoke(expected, name) for name in bounded.ENVIRONMENTS]
        for row in rows[:2]:
            prior = copy.deepcopy(row); prior["status"] = "fail"
            row.update(attempts=2, retry_reason="transient-lab", transient_diagnosis="Prepared driver briefly busy", prior_attempt=prior)
            bounded.validate_smoke_report(row, expected)
        report = {**expected, "stage": "C", "status": "pass", "forced_kill": False, "reports": rows}
        with self.assertRaisesRegex(ValueError, "At most one"):
            bounded.validate_acceptance(report, expected)
        rows[0]["prior_attempt"]["helper_sha256"] = "9" * 64
        with self.assertRaisesRegex(ValueError, "Retry changed"):
            bounded.validate_smoke_report(rows[0], expected)
        rows[0].pop("prior_attempt")
        with self.assertRaisesRegex(ValueError, "retained failed"):
            bounded.validate_smoke_report(rows[0], expected)

    def test_fallback_must_really_render_and_exit_zero(self):
        expected = identity(); expected["assets"]["debian12"]["fuse_fallback_required"] = True
        report = smoke(expected, "debian12")
        fallback = copy.deepcopy(report["steps"][0])
        fallback.update(name="fuse-fallback", timeout_seconds=180, detail={"launch": {"launch_method": "extract-and-run", "native_windows": [{}]}, "render": copy.deepcopy(report["steps"][1]["detail"]), "clean-exit": {"exit_code": 0}})
        report["steps"].append(fallback)
        bounded.validate_smoke_report(report, expected)
        fallback["detail"]["clean-exit"]["exit_code"] = -9
        with self.assertRaisesRegex(ValueError, "close naturally"):
            bounded.validate_smoke_report(report, expected)

    def test_source_assembly_is_bound_to_archive_and_original_deadline(self):
        expected = identity(); now = bounded.timestamp()
        proof = {**expected, "status": "pass", "forced_kill": False, "started_at": now, "finished_at": now, "attempt_sha256": "6" * 64, "archive_sha256": "7" * 64, "refresh_receipt_sha256": "8" * 64}
        plan = {"sha256": "7" * 64, "assembly_attempt": proof}
        bounded.validate_source_receipt(plan, expected, "6" * 64)
        for key, value in (("archive_sha256", "9" * 64), ("attempt_sha256", "9" * 64), ("status", "fail"), ("forced_kill", True), ("freeze_started_at", "2020-01-01T00:00:00Z")):
            changed = copy.deepcopy(plan); changed["assembly_attempt"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                bounded.validate_source_receipt(changed, expected, "6" * 64)

    def test_compact_dispatch_keeps_assertions_and_raw_receipt_identity(self):
        expected = identity()
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for environment in bounded.ENVIRONMENTS:
                row = smoke(expected, environment)
                row["steps"][0]["detail"]["native_windows"] = [{"name": "Frame", "role": "frame"}] * 500
                bounded.write_report(root / f"{environment}.json", row)
            compact = bounded.compact_reports(expected, root)
            self.assertEqual(len(compact), 6)
            self.assertEqual(len(compact[0]["steps"][0]["detail"]["native_windows"]), 1)
            self.assertRegex(compact[0]["raw_report_sha256"], "^[0-9a-f]{64}$")
            broken = bounded.read(root / "debian12.json")
            broken["steps"][2]["detail"]["exit_code"] = -9
            bounded.write_report(root / "debian12.json", broken)
            with self.assertRaisesRegex(ValueError, "exit code"):
                bounded.compact_reports(expected, root)

    def test_trusted_run_rejects_wrong_repository_workflow_and_source(self):
        import json
        run = {"repository": {"full_name": "neuroqp/neuroqp-aperture"}, "conclusion": "success", "event": "workflow_dispatch", "path": ".github/workflows/native-validation.yml", "head_sha": "a" * 40}
        with patch.object(bounded.subprocess, "check_output", return_value=json.dumps(run)):
            bounded.trusted_run("123", "native-validation.yml", "a" * 40)
            with self.assertRaisesRegex(ValueError, "another source"):
                bounded.trusted_run("123", "native-validation.yml", "9" * 40)
        for changed in ({**run, "repository": {"full_name": "other/fork"}}, {**run, "conclusion": "failure"}, {**run, "event": "pull_request"}):
            with patch.object(bounded.subprocess, "check_output", return_value=json.dumps(changed)), self.assertRaises(ValueError):
                bounded.trusted_run("123", "native-validation.yml")

    def test_final_package_workflow_never_runs_functional_matrix(self):
        root = Path(__file__).resolve().parents[2]
        packages = (root / ".github/workflows/package-build.yml").read_text()
        self.assertNotIn("pnpm test", packages)
        self.assertNotIn("cargo test", packages)
        self.assertNotIn("smoke-worker", packages)
        self.assertNotIn("build-upgrade-fixture", packages)
        self.assertIn("bounded.py run", packages)
        development = (root / ".github/workflows/native-validation.yml").read_text()
        self.assertIn("cargo test", development)
        self.assertNotIn("tauri build", development)
        self.assertNotIn("package-linux.sh", development)

    def test_runner_forced_timeout_is_fail_and_records_evidence(self):
        import sys
        with tempfile.TemporaryDirectory() as folder:
            report = Path(folder) / "timeout.json"
            result = bounded.run_bounded(identity(), 0.05, [sys.executable, "-c", "import time; time.sleep(2)"], report)
            self.assertNotEqual(result, 0)
            self.assertEqual(bounded.read(report)["status"], "fail")
            self.assertTrue(bounded.read(report)["forced_kill"])


if __name__ == "__main__":
    unittest.main()
