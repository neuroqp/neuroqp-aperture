"""SSH byte integrity and native invocation checks, not GUI evidence."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HELPER = Path(__file__).with_name("linux-smoke.py")
spec = importlib.util.spec_from_file_location("linux_smoke", HELPER)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

class LinuxSmokeInvocationTests(unittest.TestCase):
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

    def test_collect_requires_four_matching_actual_reports(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checksums = root / "SHA256SUMS"
            checksums.write_text("candidate hashes")
            rows = []
            for environment in ("windows11-setup", "windows11-portable", "debian12", "ubuntu24-wayland"):
                path = root / (environment + ".json")
                path.write_text(json.dumps(dict(environment=environment, source_sha="a" * 40,
                    checksums_sha256=runner.sha(checksums), status="pass", forced_kill=False,
                    steps=[dict(name=name, status="pass") for name in ("launch", "render", "clean-exit")])))
                rows.append(path)
            output = root / "smoke.json"
            runner.collect("a" * 40, checksums, output, rows)
            self.assertEqual(len(json.loads(output.read_text())["reports"]), 4)
            for reports in (rows[:-1], rows[:-1] + [rows[0]]):
                with self.assertRaisesRegex(ValueError, "each routine"):
                    runner.collect("a" * 40, checksums, root / "invalid.json", reports)
            rows[0].write_text(rows[0].read_text().replace('"forced_kill": false', '"forced_kill": true'))
            with self.assertRaisesRegex(ValueError, "forced termination"):
                runner.collect("a" * 40, checksums, root / "invalid.json", rows)

    def test_collect_keeps_unrun_targets_unverified_only_when_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checksums = root / "SHA256SUMS"
            checksums.write_text("candidate hashes")
            identity = dict(source_sha="a" * 40, checksums_sha256=runner.sha(checksums))
            unrun = dict(identity, status="unverified", forced_kill=False, reason="os-unavailable")
            passing = dict(identity, status="pass", forced_kill=False,
                           steps=[dict(name=name, status="pass") for name in ("launch", "render", "clean-exit")])
            rows = []
            for environment in ("windows11-setup", "windows11-portable", "debian12", "ubuntu24-wayland"):
                rows.append(root / (environment + ".json"))
                rows[-1].write_text(json.dumps(dict(passing if environment == "debian12" else unrun, environment=environment)))
            with self.assertRaisesRegex(ValueError, "must pass"):
                runner.collect("a" * 40, checksums, root / "refused.json", rows)
            output = root / "smoke.json"
            runner.collect("a" * 40, checksums, output, rows, accept_unverified=True)
            self.assertEqual([row["status"] for row in json.loads(output.read_text())["reports"]],
                             ["unverified", "unverified", "pass", "unverified"])
            debian = rows[2].read_text()
            for changed in (dict(unrun, steps=passing["steps"]), dict(unrun, forced_kill=True), unrun):
                rows[2].write_text(json.dumps(dict(changed, environment="debian12")))
                with self.assertRaisesRegex(ValueError, "pass"):
                    runner.collect("a" * 40, checksums, root / "invalid.json", rows, accept_unverified=True)
            rows[2].write_text(debian)

    def test_run_retains_desktop_environment_without_expiring_receipts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            identity, plan, report = [root / name for name in ("identity.json", "plan.json", "report.json")]
            identity.write_text(json.dumps(dict(environment="ubuntu24-wayland")))
            plan.write_text(json.dumps(dict(environment=dict(GDK_BACKEND="wayland"))))
            with patch.dict(os.environ, {"WAYLAND_DISPLAY": "wayland-1", "NO_AT_BRIDGE": "1"}), patch.object(runner.subprocess, "run") as invoke:
                invoke.return_value.returncode = 7
                self.assertEqual(runner.run(identity, report, plan), 7)
                self.assertEqual(invoke.call_args.kwargs["env"]["WAYLAND_DISPLAY"], "wayland-1")
                self.assertEqual(invoke.call_args.kwargs["env"]["GDK_BACKEND"], "wayland")
                self.assertNotIn("NO_AT_BRIDGE", invoke.call_args.kwargs["env"])
                self.assertIn("ubuntu24-wayland", invoke.call_args.args[0])
            report.write_text("preserved evidence")
            with self.assertRaisesRegex(ValueError, "fresh"):
                runner.run(identity, report, plan)

if __name__ == "__main__":
    unittest.main()
