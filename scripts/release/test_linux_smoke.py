"""Exercise real transport/invocation entrypoints; these are not GUI evidence."""

import hashlib
import copy
import importlib.util
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HELPER = Path(__file__).with_name("linux-smoke.py")


class LinuxSmokeInvocationTests(unittest.TestCase):
    def test_compact_retry_preserves_prior_identity_and_deadline_rejections(self):
        import bounded
        import test_bounded
        spec = importlib.util.spec_from_file_location("linux_smoke", HELPER)
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        expected = test_bounded.identity()
        raw = test_bounded.smoke(expected, "debian12")
        prior = copy.deepcopy(raw)
        prior.update(status="fail", error="Prepared driver briefly busy")
        raw.update(attempts=2, retry_reason="transient-lab", transient_diagnosis="Prepared driver briefly busy; bytes unchanged", prior_attempt=prior)
        compact = runner.compact_smoke(raw)
        bounded.validate_smoke_report(compact, expected)
        self.assertEqual(compact["transient_diagnosis"], raw["transient_diagnosis"])
        for field, value in (("asset_sha256", "9" * 64), ("deadline_at", expected["freeze_started_at"])):
            invalid = copy.deepcopy(raw)
            invalid["prior_attempt"][field] = value
            with self.assertRaisesRegex(ValueError, "changed source"):
                bounded.validate_smoke_report(runner.compact_smoke(invalid), expected)
        invalid = copy.deepcopy(raw)
        invalid["deadline_at"] = expected["freeze_started_at"]
        with self.assertRaisesRegex(ValueError, "frozen deadline_at"):
            bounded.validate_smoke_report(runner.compact_smoke(invalid), expected)
        invalid = copy.deepcopy(raw)
        invalid["prior_attempt"]["prior_attempt"] = prior
        with self.assertRaisesRegex(ValueError, "Only one retained"):
            runner.compact_smoke(invalid)

    def test_compact_report_passes_full_linux_gate_including_fallback_without_weakening_failures(self):
        import bounded
        import test_bounded
        spec = importlib.util.spec_from_file_location("linux_smoke", HELPER)
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        for environment in ("debian12", "ubuntu24", "arch-omarchy", "fedora-wayland"):
            expected = test_bounded.identity()
            raw = test_bounded.smoke(expected, environment)
            if environment in ("debian12", "ubuntu24"):
                expected["assets"][environment]["fuse_fallback_required"] = True
                launch = {**raw["steps"][0]["detail"], "launch_method": "extract-and-run"}
                raw["steps"].append({**raw["steps"][0], "name": "fuse-fallback", "timeout_seconds": 180,
                                     "detail": dict(launch=launch, render=raw["steps"][1]["detail"], **{"clean-exit": raw["steps"][2]["detail"]})})
            compact = runner.compact_smoke(raw)
            bounded.validate_smoke_report(compact, expected)
            for flag in ("timed_out", "deadline_exceeded", "helper_forced_termination"):
                raw["steps"][0][flag] = True
                rejected = runner.compact_smoke(raw)
                self.assertIs(rejected["steps"][0][flag], True)
                with self.assertRaisesRegex(ValueError, "forced termination"):
                    bounded.validate_smoke_report(rejected, expected)
                del raw["steps"][0][flag]
            compact["steps"][2]["forced_kill"] = True
            with self.assertRaisesRegex(ValueError, "forced termination"):
                bounded.validate_smoke_report(compact, expected)
            compact["steps"][2]["forced_kill"] = False
            compact["steps"][1]["detail"]["pixel_observation"]["spread"] = 0
            with self.assertRaisesRegex(ValueError, "visible pixels"):
                bounded.validate_smoke_report(compact, expected)

    def test_compact_report_retains_strict_native_validator_proof(self):
        import bounded
        spec = importlib.util.spec_from_file_location("linux_smoke", HELPER)
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        source = "a" * 40
        launch = dict(pid=42, launcher_pid=41, backend="wayland", process_identity=dict(buildinfo=dict(source_sha=source, packages="large" * 1000)),
                      native_windows=[dict(role="frame", active=True), dict(role="label", name="large" * 1000)], socket_rows=["large" * 1000])
        render = dict(image_input="startup image command line", screenshot_sha256="b" * 64,
                      observation=dict(phase="ready", error=False, rendered_canvas=True, state=dict(phase="ready", previewReady=True, width=65, height=49)),
                      pixel_observation=dict(spread=71, samples=[0] * 1000))
        close = dict(pid=41, app_pid=42, exit_code=0, app_exited=True, exit_code_source="exact AppImage launcher wait")
        raw = dict(source_sha=source, steps=[dict(name=name, detail=value) for name, value in
                                           (("launch", launch), ("render", render), ("clean-exit", close))])
        compact = runner.compact_smoke(raw)
        bounded.validate_render(compact["steps"][1]["detail"], dict(tiny_tiff_width=65, tiny_tiff_height=49))
        bounded.validate_native_exit(compact["steps"][0]["detail"], compact["steps"][2]["detail"], "fedora-wayland", dict(source_sha=source))
        self.assertLess(len(json.dumps(compact)), len(json.dumps(raw)) // 5)

    def test_ssh_stream_round_trip_and_corruption_preserve_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # Execute the remote command through a local shell, with no SFTP or
            # scp implementation available. Quotes and shell metacharacters in
            # paths must survive the actual SSH remote-shell command boundary.
            ssh = root / "ssh"
            ssh.write_text("#!/bin/sh\nexec /bin/sh -c \"$1\"\n")
            ssh.chmod(0o700)
            source = root / "source.bin"
            data = bytes(range(256)) * 8193
            source.write_bytes(data)
            expected = hashlib.sha256(data).hexdigest()
            remote = root / "remote 'quoted' $(false).bin"
            copied = root / "downloaded.bin"
            for command, before, after in (("copy-to", source, remote), ("copy-from", remote, copied)):
                result = subprocess.run([sys.executable, str(HELPER), command, str(before), str(after), expected,
                                         "--", str(ssh)], capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["transport"], "ssh-stdin")
                self.assertEqual(after.read_bytes(), data)
            self.assertEqual(remote.stat().st_mode & 0o777, 0o700)
            remote.write_bytes(b"changed remote bytes")
            result = subprocess.run([sys.executable, str(HELPER), "copy-from", str(remote), str(copied), expected,
                                     "--", str(ssh)], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("SHA256 mismatch", result.stderr)
            self.assertEqual(copied.read_bytes(), data)
            roundtrip = root / "roundtrip.bin"
            transfer = root / "transport.json"
            result = subprocess.run([sys.executable, str(HELPER), "copy-check", str(source), str(remote), str(roundtrip),
                                     str(transfer), "--", str(ssh)], capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(transfer.read_text())["returned_sha256"], expected)
            prior = transfer.read_bytes()
            result = subprocess.run([sys.executable, str(HELPER), "copy-check", str(source), str(remote), str(root / "fresh.bin"),
                                     str(transfer), "--", str(ssh)], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(transfer.read_bytes(), prior)
            self.assertFalse((root / "fresh.bin").exists())
            # Corrupt upload in transit, after the sender checked its input.
            ssh.write_text("#!/bin/sh\nprintf corrupt | /bin/sh -c \"$1\"\n")
            result = subprocess.run([sys.executable, str(HELPER), "copy-to", str(source), str(remote), expected,
                                     "--", str(ssh)], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(remote.read_bytes(), data)
            self.assertEqual(set(root.iterdir()), {ssh, source, remote, copied, roundtrip, transfer})

    def test_run_keeps_desktop_discovery_and_prepared_overrides_and_exit_code(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            helper = root / "observer.py"
            helper.write_text("import json,os,pathlib,sys\npathlib.Path(sys.argv[-1]).write_text(json.dumps(dict(os.environ)))\nsys.exit(7)\n")
            identity, plan, report = [root / name for name in ("identity.json", "plan.json", "report.json")]
            plan.write_text(json.dumps(dict(environment=dict(LD_LIBRARY_PATH="/prepared/driver", GDK_BACKEND="wayland"))))
            identity.write_text(json.dumps(dict(deadline_at=(datetime.now(timezone.utc) + timedelta(seconds=30)).isoformat(), readiness=dict(environment="arch-omarchy", helper_path=str(helper),
                                                               preflight=dict(runner_sha256=hashlib.sha256(HELPER.read_bytes()).hexdigest(), driver_plan_sha256=hashlib.sha256(plan.read_bytes()).hexdigest()),
                                                               helper_sha256=hashlib.sha256(helper.read_bytes()).hexdigest()))))
            environment = {**os.environ, "HYPRLAND_INSTANCE_SIGNATURE": "owned-session", "WAYLAND_DISPLAY": "wayland-7",
                           "DBUS_SESSION_BUS_ADDRESS": "unix:path=/owned/bus", "NO_AT_BRIDGE": "1"}
            command = [sys.executable, str(HELPER), "run", str(identity), str(report), "--driver-plan", str(plan)]
            result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 7, result.stderr)
            observed = json.loads(report.read_text())
            for key in ("HYPRLAND_INSTANCE_SIGNATURE", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"):
                self.assertEqual(observed[key], environment[key])
            self.assertEqual(observed["LD_LIBRARY_PATH"], "/prepared/driver")
            self.assertEqual(observed["GDK_BACKEND"], "wayland")
            self.assertNotIn("NO_AT_BRIDGE", observed)
            summary = report.with_suffix(".summary.json")
            retained = summary.read_bytes()
            report.unlink()
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("outputs must be fresh", result.stderr)
            self.assertEqual(summary.read_bytes(), retained)
            self.assertFalse(report.exists())
            plan.write_text(json.dumps(dict(environment={})))
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("driver plan differs", result.stderr)
            helper.write_text("raise RuntimeError('changed helper ran')\n")
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("helper SHA256 mismatch", result.stderr)

    def test_preflight_requires_native_clean_exit_and_expired_identity_never_launches(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            helper, identity, plan, report, image = [root / name for name in
                ("observer.py", "identity.json", "plan.json", "report.json", "tiny.tif")]
            helper.write_text("import json,os,pathlib,sys\nsteps=[dict(name=n,status='pass',forced_kill=False) for n in ('launch','render','clean-exit')]\nif os.environ.get('FAIL_CLOSE'): steps[-1]['status']='fail'\npathlib.Path(sys.argv[-1]).write_text(json.dumps(dict(status='pass',forced_kill=False,steps=steps)))\n")
            image.write_bytes(b"fixed tiny fixture")
            data = dict(source_sha="a" * 40, asset_sha256="f" * 64, deadline_at=(datetime.now(timezone.utc) + timedelta(seconds=30)).isoformat(),
                        tiny_tiff_path=str(image), tiny_tiff_sha256=hashlib.sha256(image.read_bytes()).hexdigest(),
                        readiness=dict(environment="arch-omarchy", helper_path=str(helper), tools=[],
                                       helper_sha256=hashlib.sha256(helper.read_bytes()).hexdigest()))
            identity.write_text(json.dumps(data));plan.write_text(json.dumps(dict(environment={})))
            command = [sys.executable, str(HELPER), "preflight", str(identity), str(report), "--driver-plan", str(plan)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            receipt = json.loads(report.with_suffix(".readiness.json").read_text())
            self.assertEqual(receipt["status"], "pass")
            self.assertEqual((receipt["source_sha"], receipt["asset_sha256"]), (data["source_sha"], data["asset_sha256"]))
            self.assertEqual(receipt["transport"]["transport"], "local")
            self.assertEqual(receipt["smoke_report"], json.loads(report.read_text()))
            report = root / "failed-close.json"
            command[4] = str(report)
            result = subprocess.run(command, env={**os.environ, "FAIL_CLOSE": "1"}, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(json.loads(report.with_suffix(".readiness.json").read_text())["status"], "fail")
            prior = report.read_bytes()
            data["readiness"]["preflight"] = dict(runner_sha256=hashlib.sha256(HELPER.read_bytes()).hexdigest(), driver_plan_sha256=hashlib.sha256(plan.read_bytes()).hexdigest())
            data["deadline_at"] = (datetime.now(timezone.utc) - timedelta(seconds=1)).isoformat()
            identity.write_text(json.dumps(data))
            command[2] = "run"
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("deadline already expired", result.stderr)
            self.assertEqual(report.read_bytes(), prior)


if __name__ == "__main__":
    unittest.main()
