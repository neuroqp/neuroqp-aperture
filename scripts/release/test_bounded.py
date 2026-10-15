"""Bounded evidence rejects stale source, changed bytes, timeouts and missing desktops."""

import copy
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import bounded


class CheckoutTests(unittest.TestCase):
    def test_container_ownership_exception_still_rejects_dirty_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "input.txt").write_text("reviewed")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "input"], cwd=root, check=True)
            with patch.dict(os.environ, {"GIT_TEST_ASSUME_DIFFERENT_OWNER": "1"}):
                self.assertIn(b"input.txt", bounded.git_inputs(root))
                (root / "input.txt").write_text("changed")
                with self.assertRaises(subprocess.CalledProcessError):
                    bounded.git_inputs(root)


def identity():
    now = bounded.parsed(bounded.timestamp())
    return {"schema": 1, "source_sha": "a" * 40, "build_inputs_sha256": "b" * 64, "component_inputs": {"frontend": "c" * 64, "backend": "d" * 64}, "freeze_started_at": (now - dt.timedelta(seconds=60)).isoformat(), "deadline_at": (now + dt.timedelta(seconds=1740)).isoformat(), "checksums_sha256": "e" * 64, "assets": {name: {"sha256": "f" * 64, "tiny_tiff_sha256": "1" * 64, "helper_sha256": "2" * 64, "tiny_tiff_width": 16, "tiny_tiff_height": 16, "tools": [{"name": "observer", "path": "C:/observer.exe" if name.startswith("windows") else "/usr/bin/observer", "sha256": "2" * 64}]} for name in bounded.ENVIRONMENTS}}


def smoke(expected, environment):
    now = bounded.timestamp()
    result = {**{key: expected[key] for key in ("schema", "source_sha", "build_inputs_sha256", "checksums_sha256", "freeze_started_at", "deadline_at")}, "stage": "C", "status": "pass", "forced_kill": False, "environment": environment, "asset_sha256": "f" * 64, "tiny_tiff_sha256": "1" * 64, "helper_sha256": "2" * 64, "attempts": 1, "started_at": now, "finished_at": now, "observer_tools": expected["assets"][environment]["tools"], "steps": [{"name": name, "status": "pass", "forced_kill": False, "started_at": now, "finished_at": now, "timeout_seconds": bounded.STEP_LIMITS[name], "detail": {}} for name in bounded.SMOKE_STEPS]}
    result.update(uid=1000, os={"Caption": "Windows 11"}, privilege={"is_admin": False, "session_id": 1})
    result["steps"][0]["detail"] = {"pid": 123, "launcher_pid": 122, "hwnd": 1234, "native_windows": [{"role": "frame"}], "process_identity": {"buildinfo": {"source_sha": expected["source_sha"]}}, "embedded_buildinfo": {"source_sha": expected["source_sha"]}, "backend": "native-Windows-WebView2" if environment.startswith("windows") else "wayland", "installer_exit_code": 0, "installed_marker_verified": True, "cold_baseline": True, "incoming_payload_id": "5" * 64}
    result["steps"][1]["detail"] = {"image_input": "startup image command line", "observation": {"phase": "ready", "error": False, "rendered_canvas": True, "state": {"phase": "ready", "previewReady": True, "width": 16, "height": 16}}, "pixel_observation": {"spread": 100}, "screenshot_sha256": "3" * 64}
    result["steps"][2]["detail"] = {"pid": 123 if environment.startswith("windows") else 122, "exit_code": 0, "app_pid": 123, "app_exited": True, "exit_code_source": "exact AppImage launcher wait", "forced_cleanup": False, "action": "actual mouse SendInput"}
    return result


class BoundedTests(unittest.TestCase):
    def test_freeze_records_validated_manifests_and_rejects_wrong_prepared_binary(self):
        current = identity()
        readiness = {"automated": [{"platform": platform, "source_sha": current["source_sha"],
                                  "binary_sha256": "b" * 64} for platform in ("linux", "windows")]}
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            for platform in ("linux", "windows"):
                bounded.write_report(directory / f"prepared-{platform}.json", {
                    "schema": 1, "stage": "A-packaging", "status": "pass", "forced_kill": False,
                    "platform": platform, "source_sha": current["source_sha"],
                    "validation_run_id": "123", "packaging_run_id": "456",
                    "source_material": {"asset": "source.tar.xz", "url": "https://example.invalid/source.tar.xz",
                        "sha256": "b" * 64, "application_source_inventory_sha256": "c" * 64,
                        "application_source_sha": current["source_sha"], "application_lockfiles_sha256": {},
                        "native_dependencies_sha256": {"linux": "b" * 64, "windows": "b" * 64}},
                    **{key: "b" * 64 for key in ("binary_sha256", "native_dependencies_sha256",
                        "archive_sha256", "tool_archive_sha256", "development_archive_sha256",
                        "source_plan_sha256", "verifier_archive_sha256")}})
            with patch.object(bounded, "identity", return_value=current), patch.object(bounded, "validate_readiness"), \
                    patch.object(bounded, "trusted_run", return_value={"head_sha": "a" * 40}), patch.object(bounded.subprocess, "run"):
                result = bounded.frozen(readiness, "789", directory, "456")
                self.assertEqual(result["packaging_run_id"], "456")
                for platform in ("linux", "windows"):
                    path = directory / f"prepared-{platform}.json"
                    self.assertEqual(result["validated_inputs"][platform]["manifest_sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
                path = directory / "prepared-windows.json"
                manifest = json.loads(path.read_text())
                manifest["source_material"]["url"] = "https://example.invalid/changed.tar.xz"
                bounded.write_report(path, manifest)
                with self.assertRaisesRegex(ValueError, "different source material"):
                    bounded.frozen(readiness, "789", directory, "456")
                manifest["source_material"]["url"] = "https://example.invalid/source.tar.xz"
                manifest["binary_sha256"] = "9" * 64
                bounded.write_report(path, manifest)
                with self.assertRaisesRegex(ValueError, "executable differs"):
                    bounded.frozen(readiness, "789", directory, "456")

    def test_workflow_build_changes_invalidate_reusable_native_inputs(self):
        files = {"src/main.ts": "1", "src-tauri/src/lib.rs": "2", **{f".github/workflows/{name}.yml": "3" for name in ("native-validation", "package-build", "release")}}
        def inputs():
            return b"\0".join(f"100644 blob {blob * 40}\t{path}".encode() for path, blob in files.items()) + b"\0"
        with patch.object(bounded, "git_inputs", side_effect=lambda root: inputs()), patch.object(bounded.subprocess, "check_output", return_value="a" * 40):
            before = bounded.identity()["component_inputs"]
            for name in ("native-validation", "package-build", "release"):
                path = f".github/workflows/{name}.yml"
                files[path] = "4"
                after = bounded.identity()["component_inputs"]
                self.assertNotEqual(before["packaging"], after["packaging"])
                self.assertEqual(before["frontend"], after["frontend"])
                self.assertEqual(before["backend"], after["backend"])
                files[path] = "3"

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
        changed = copy.deepcopy(rows[0]); changed["finished_at"] = (bounded.parsed(bounded.timestamp()) + dt.timedelta(seconds=1)).isoformat()
        with self.assertRaisesRegex(ValueError, "future"): bounded.validate_smoke_report(changed, expected)
        changed = copy.deepcopy(rows[0]); changed["timed_out"] = True
        with self.assertRaisesRegex(ValueError, "blocks release"): bounded.validate_smoke_report(changed, expected)

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
        labs = [{"environment": name, "ready": True, "source_sha": expected["source_sha"], "status": "pass", "forced_kill": False, "native_desktop": True, "tools": [{"name": "native smoke helper", "path": "C:/observer.exe" if name.startswith("windows") else "/usr/bin/observer", "sha256": "2" * 64}], "expires_at": expected["deadline_at"], **{key: expected["assets"][name][key] for key in ("tiny_tiff_sha256", "helper_sha256", "tiny_tiff_width", "tiny_tiff_height")}} for name in bounded.ENVIRONMENTS]
        report = {**expected, "stage": "A", "status": "pass", "forced_kill": False, "completed_at": now, "automated": automated, "native_functional": native, "labs": labs}
        bounded.validate_readiness(report, expected)
        for index, lab in enumerate(labs):
            for key, value in (("ready", None), ("ready", False), ("ready", 1), ("source_sha", None), ("source_sha", "9" * 40), ("environment", "other-desktop")):
                with self.subTest(environment=lab["environment"], field=key, value=value):
                    invalid = copy.deepcopy(report)
                    if value is None:
                        del invalid["labs"][index][key]
                    else:
                        invalid["labs"][index][key] = value
                    with self.assertRaisesRegex(ValueError, "lab readiness|six native desktop"):
                        bounded.validate_readiness(invalid, expected)
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

    def test_transient_label_cannot_retry_forced_or_timed_out_attempt(self):
        expected = identity(); row = smoke(expected, "debian12")
        prior = copy.deepcopy(row); prior["status"] = "fail"
        row.update(attempts=2, retry_reason="transient-lab", transient_diagnosis="Prepared observer briefly busy", prior_attempt=prior)
        bounded.validate_smoke_report(row, expected)
        for field in ("forced_kill", "timed_out", "deadline_exceeded", "helper_forced_termination"):
            changed = copy.deepcopy(row); changed["prior_attempt"][field] = True
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "cannot be retried"):
                bounded.validate_smoke_report(changed, expected)
        for field in ("forced_kill", "timed_out"):
            changed = copy.deepcopy(row); changed["prior_attempt"]["steps"][0][field] = True
            with self.subTest(step_field=field), self.assertRaisesRegex(ValueError, "cannot be retried"):
                bounded.validate_smoke_report(changed, expected)
        changed = copy.deepcopy(row)
        step = changed["prior_attempt"]["steps"][0]
        step["finished_at"] = (bounded.parsed(step["started_at"]) + dt.timedelta(seconds=step["timeout_seconds"])).isoformat()
        with self.assertRaisesRegex(ValueError, "cannot be retried"):
            bounded.validate_smoke_report(changed, expected)

    def test_fallback_must_really_render_and_exit_zero(self):
        expected = identity(); expected["assets"]["debian12"]["fuse_fallback_required"] = True
        report = smoke(expected, "debian12")
        fallback = copy.deepcopy(report["steps"][0])
        fallback.update(name="fuse-fallback", timeout_seconds=180, detail={"launch": {**copy.deepcopy(report["steps"][0]["detail"]), "launch_method": "extract-and-run"}, "render": copy.deepcopy(report["steps"][1]["detail"]), "clean-exit": copy.deepcopy(report["steps"][2]["detail"])})
        report["steps"].append(fallback)
        bounded.validate_smoke_report(report, expected)
        fallback["detail"]["clean-exit"]["exit_code"] = -9
        with self.assertRaisesRegex(ValueError, "exit code"):
            bounded.validate_smoke_report(report, expected)

    def test_source_assembly_is_bound_to_archive_and_original_deadline(self):
        expected = {**identity(), "stage": "B"}; now = bounded.timestamp()
        proof = {**expected, "stage": "B", "status": "pass", "forced_kill": False, "started_at": now, "finished_at": now, "attempt_sha256": "6" * 64, "archive_sha256": "7" * 64, "refresh_receipt_sha256": "8" * 64}
        plan = {"sha256": "7" * 64, "assembly_attempt": proof}
        bounded.validate_source_receipt(plan, expected, "6" * 64)
        for key, value in (("schema", 2), ("stage", "A"), ("source_sha", "9" * 40), ("build_inputs_sha256", "9" * 64), ("archive_sha256", "9" * 64), ("attempt_sha256", "9" * 64), ("status", "fail"), ("forced_kill", True), ("freeze_started_at", "2020-01-01T00:00:00Z"), ("deadline_at", "2020-01-01T00:30:00Z"), ("started_at", "2020-01-01T00:00:00Z"), ("refresh_receipt_sha256", "")):
            changed = copy.deepcopy(plan); changed["assembly_attempt"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                bounded.validate_source_receipt(changed, expected, "6" * 64)

    def test_compact_dispatch_keeps_assertions_and_raw_receipt_identity(self):
        expected = identity()
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for environment in bounded.ENVIRONMENTS:
                row = smoke(expected, environment)
                row["steps"][0]["detail"]["native_windows"] = [{"name": "App", "role": "application"}] + [{"name": "Frame", "role": "frame"}] * 500
                if environment in ("debian12", "ubuntu24"):
                    expected["assets"][environment]["fuse_fallback_required"] = True
                    fallback = copy.deepcopy(row["steps"][0])
                    fallback.update(name="fuse-fallback", timeout_seconds=180, detail={"launch": {**copy.deepcopy(row["steps"][0]["detail"]), "launch_method": "extract-and-run"}, "render": copy.deepcopy(row["steps"][1]["detail"]), "clean-exit": copy.deepcopy(row["steps"][2]["detail"])})
                    row["steps"].append(fallback)
                bounded.write_report(root / f"{environment}.json", row)
            compact = bounded.compact_reports(expected, root)
            self.assertEqual(len(compact), 6)
            self.assertEqual(len(compact[0]["steps"][0]["detail"]["native_windows"]), 1)
            self.assertEqual(compact[2]["steps"][3]["detail"]["launch"]["native_windows"][0]["role"], "frame")
            self.assertRegex(compact[0]["raw_report_sha256"], "^[0-9a-f]{64}$")
            broken = bounded.read(root / "debian12.json")
            broken["steps"][2]["detail"]["exit_code"] = -9
            bounded.write_report(root / "debian12.json", broken)
            with self.assertRaisesRegex(ValueError, "exit code"):
                bounded.compact_reports(expected, root)

    def test_native_receipt_cannot_substitute_canvas_or_process_outcome(self):
        expected = identity()
        for environment in ("windows11-setup", "debian12"):
            report = smoke(expected, environment)
            mutations = [(1, "observation", {**report["steps"][1]["detail"]["observation"], "rendered_canvas": False, "rendered_tiles": True}), (1, "observation", {**report["steps"][1]["detail"]["observation"], "error": None}), (1, "pixel_observation", {"spread": float("nan")}), (2, "exit_code", False), (2, "pid", 999)]
            if environment.startswith("windows"):
                mutations.append((2, "forced_cleanup", True))
            else:
                mutations.extend(((2, "app_pid", 999), (2, "app_exited", False), (2, "exit_code_source", "PID absent")))
            for step, key, value in mutations:
                changed = copy.deepcopy(report); changed["steps"][step]["detail"][key] = value
                with self.subTest(environment=environment, key=key), self.assertRaises(ValueError):
                    bounded.validate_smoke_report(changed, expected)

    def test_trusted_run_rejects_wrong_repository_workflow_and_source(self):
        import json
        run = {"repository": {"full_name": "neuroqp/neuroqp-aperture"}, "conclusion": "success", "event": "workflow_dispatch", "path": ".github/workflows/native-validation.yml", "head_sha": "a" * 40, "head_branch": "feature-reviewed"}
        with patch.object(bounded.subprocess, "check_output", return_value=json.dumps(run)):
            bounded.trusted_run("123", "native-validation.yml", "a" * 40)
            with self.assertRaisesRegex(ValueError, "another source"):
                bounded.trusted_run("123", "native-validation.yml", "9" * 40)
        for changed in ({**run, "repository": {"full_name": "other/fork"}}, {**run, "conclusion": "failure"}, {**run, "event": "pull_request"}):
            with patch.object(bounded.subprocess, "check_output", return_value=json.dumps(changed)), self.assertRaises(ValueError):
                bounded.trusted_run("123", "native-validation.yml")
        for workflow in ("release.yml", "release-evidence.yml"):
            main = {**run, "head_branch": "main", "path": f".github/workflows/{workflow}", "run_attempt": 1}
            with patch.object(bounded.subprocess, "check_output", return_value=json.dumps(main)):
                bounded.trusted_run("123", workflow)
            for changed in ({**main, "head_branch": "feature-reviewed"}, {**main, "head_branch": None}):
                with patch.object(bounded.subprocess, "check_output", return_value=json.dumps(changed)), self.assertRaisesRegex(ValueError, "trusted main"):
                    bounded.trusted_run("123", workflow)
            if workflow == "release.yml":
                with patch.object(bounded.subprocess, "check_output", return_value=json.dumps({**main, "run_attempt": 2})), self.assertRaisesRegex(ValueError, "reruns"):
                    bounded.trusted_run("123", workflow)

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
        self.assertIn("--release --features custom-protocol --locked", development)
        self.assertNotIn("target/debug/aperture", development)
        self.assertNotIn("tauri build", development)
        self.assertNotIn("package-linux.sh", development)
        for platform, job in zip(("linux", "windows"), development.split("  windows:\n")):
            # Public development bytes must pass source coverage before archive/upload.
            gate = job.index("release.py development-source")
            archived = job.index("tar -czf" if platform == "linux" else "shutil.make_archive")
            upload = job.index("actions/upload-artifact")
            self.assertLess(job.index("release.py buildinfo"), gate)
            self.assertLess(job.index("release/notices.py"), gate)
            self.assertLess(gate, archived)
            self.assertLess(archived, upload)
            self.assertEqual(job.count("release.py development-source"), 1)
            self.assertIn("--baseline-directory build/source-baseline --fetch", job)
            self.assertNotIn("build/source-baseline", job[upload:])

    def test_run_cli_accepts_workflow_option_order(self):
        import sys
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            expected = identity()
            frozen = root / "frozen-attempt.json"
            report = root / "command.json"
            bounded.write_report(frozen, expected)
            workflow = (bounded.ROOT / ".github/workflows/release.yml").read_text()
            caller = next(line for line in workflow.splitlines() if "bounded.py run " in line)
            arguments = caller.split("bounded.py ", 1)[1].split(" -- ", 1)[0].split()
            replacements = {"build/evidence/frozen-attempt.json": str(frozen), "build/evidence/source-material.json": str(report)}
            arguments = [replacements.get(value, value) for value in arguments]
            arguments[arguments.index("--seconds") + 1] = "5"
            command = [sys.executable, "-c", "print('bounded command completed')"]
            result = subprocess.run([sys.executable, str(Path(bounded.__file__)), *arguments, "--", *command], capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "bounded command completed")
            evidence = bounded.read(report)
            self.assertEqual(evidence["command"], command)
            self.assertEqual(evidence["status"], "pass")
            self.assertFalse(evidence["forced_kill"])
            self.assertEqual(evidence["timeout_seconds"], 5)

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
