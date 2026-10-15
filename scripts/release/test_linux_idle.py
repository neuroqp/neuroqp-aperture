"""Process-lifetime adapter checks with a disposable bus fixture, not native GUI evidence."""

from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).with_name("linux-smoke.py").resolve()


class LinuxIdleTests(unittest.TestCase):
    @unittest.skipIf(os.getuid() == 0, "Idle guard requires an ordinary desktop user")
    def test_guard_spans_preflight_and_run_then_releases_and_blocks_stale_readiness(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = root / "gi"
            package.mkdir()
            (package / "__init__.py").touch()
            # Only the external desktop API is substituted. Run the real CLI,
            # signal handling, /proc identities, plan hashes and launch gates.
            (package / "repository.py").write_text('''import json,os,pathlib
root=pathlib.Path(os.environ['IDLE_FIXTURE'])
class Variant:
 def __init__(self,signature,values):self.values=values
 def unpack(self):return self.values
class Flags:
 AUTHENTICATION_CLIENT=1
 MESSAGE_BUS_CONNECTION=2
 NO_AUTO_START=1
class Connection:
 @staticmethod
 def new_for_address_sync(*args):return Connection()
 def get_unique_name(self):return ':1.'+str(os.getpid())
 def call_sync(self,destination,path,interface,method,parameters,*args):
  state=json.loads((root/'state.json').read_text())
  with (root/'calls').open('a') as f:f.write(method+'\\n')
  if method=='GetNameOwner':result=(state.get('owner',':1.locker'),)
  elif method=='GetActive':result=(state.get('blanked',False),)
  elif method=='Inhibit':
   (root/('cookie-'+str(os.getpid()))).touch();result=(19,)
  elif method=='NameHasOwner':result=((root/('cookie-'+parameters.values[0].split('.')[-1])).exists(),)
  else:raise ValueError(method)
  return Variant(None,result)
 def close_sync(self,*args):(root/('cookie-'+str(os.getpid()))).unlink(missing_ok=True)
class Gio:
 DBusConnection=Connection
 DBusConnectionFlags=Flags
 DBusCallFlags=Flags
class GLib:Variant=Variant
''')
            loginctl = root / "loginctl"
            loginctl.write_text(f"#!{sys.executable}\nimport json,os,pathlib\ns=json.loads(pathlib.Path(os.environ['IDLE_FIXTURE'],'state.json').read_text());print('Id=c1\\nActive='+s.get('active','yes')+'\\nLockedHint='+s.get('locked','no')+'\\nUser='+str(os.getuid())+'\\nType=x11\\nClass=user')\n")
            loginctl.chmod(0o700)
            state = root / "state.json"
            state.write_text("{}")
            environment = {**os.environ, "PYTHONPATH": str(root), "IDLE_FIXTURE": str(root),
                           "PATH": str(root) + os.pathsep + os.environ["PATH"]}
            plan, receipt = root / "plan.json", root / "guard.json"
            plan.write_text(json.dumps(dict(environment=dict(XDG_SESSION_TYPE="x11", DBUS_SESSION_BUS_ADDRESS="unix:path=/fixture"), idle_guard=str(receipt))))
            command = [sys.executable, str(RUNNER), "idle-guard", "--driver-plan", str(plan), "--session-id", "c1", "--seconds", "30"]
            guard = subprocess.Popen(command, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                self.assertTrue(select.select([guard.stdout], [], [], 5)[0], "Guard did not acknowledge inhibition")
                acknowledged = json.loads(guard.stdout.readline())
                self.assertEqual(acknowledged["status"], "active")
                self.assertTrue((root / ("cookie-" + str(guard.pid))).exists())
                helper, image = root / "observer.py", root / "tiny.tif"
                helper.write_text("import json,os,pathlib,sys\npathlib.Path(sys.argv[-1]).write_text(json.dumps(dict(status='pass',forced_kill=False,steps=[dict(name=n,status='pass',forced_kill=False) for n in ('launch','render','clean-exit')])))\nif os.environ.get('IDLE_MARK_LOCKED'):pathlib.Path(os.environ['IDLE_FIXTURE'],'state.json').write_text(json.dumps(dict(locked='yes')))\n")
                image.write_bytes(b"tiny fixture")
                sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
                identity = root / "identity.json"
                data = dict(source_sha="a" * 40, asset_sha256="f" * 64,
                            tiny_tiff_path=str(image), tiny_tiff_sha256=sha(image),
                            deadline_at=(datetime.now(timezone.utc) + timedelta(seconds=30)).isoformat(),
                            readiness=dict(environment="debian12", helper_path=str(helper), helper_sha256=sha(helper), tools=[],
                                           preflight=dict(runner_sha256=sha(RUNNER), driver_plan_sha256=sha(plan))))
                identity.write_text(json.dumps(data))
                transfer = root / "transfer.json"
                transfer.write_text(json.dumps(dict(status="pass", forced_kill=False, transport="ssh-stdin", runner_sha256=sha(RUNNER),
                                                   fixture_sha256=sha(image), returned_sha256=sha(image))))
                def invoke(name, label):
                    report = root / (label + ".json")
                    args = [sys.executable, str(RUNNER), name, str(identity), str(report), "--driver-plan", str(plan)]
                    if name == "preflight":
                        args += ["--transport-report", str(transfer)]
                    return subprocess.run(args, env=environment, capture_output=True, text=True, timeout=5), report
                for name in ("preflight", "run"):
                    result, report = invoke(name, name)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(report.exists())
                environment["IDLE_MARK_LOCKED"] = "1"
                result, report = invoke("run", "lost-during-smoke")
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(json.loads(report.read_text())["status"], "pass")
                self.assertEqual(json.loads(report.with_suffix(".summary.json").read_text())["status"], "fail")
                del environment["IDLE_MARK_LOCKED"]
                state.write_text("{}")
                for bad_state, message in ((dict(locked="yes"), "unlocked"), (dict(active="no"), "active"),
                                           (dict(blanked=True), "already active"), (dict(owner=":1.restarted"), "restarted")):
                    state.write_text(json.dumps(bad_state))
                    result, report = invoke("preflight", "reject")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertFalse(report.exists())
                state.write_text("{}")
                # PID reuse or an unrelated process never satisfies the gate.
                record = json.loads(receipt.read_text())
                receipt.write_text(json.dumps({**record, "expires_at": datetime.now(timezone.utc).isoformat()}))
                result, report = invoke("run", "expired-guard")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("lifetime expired", result.stderr)
                receipt.write_text(json.dumps({**record, "start_ticks": record["start_ticks"] + 1}))
                result, report = invoke("run", "wrong-process")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("process is no longer alive", result.stderr)
                receipt.write_text(json.dumps(record))
                guard.send_signal(signal.SIGHUP)
                stdout, stderr = guard.communicate(timeout=5)
                self.assertEqual(guard.returncode, 0, stderr)
                self.assertEqual(json.loads(receipt.read_text())["status"], "closed")
                self.assertFalse((root / ("cookie-" + str(guard.pid))).exists())
                for name in ("preflight", "run"):
                    result, report = invoke(name, name + "-stopped")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(report.exists())
                # No activation, unlock, simulated-input or preference calls.
                self.assertEqual(set((root / "calls").read_text().splitlines()),
                                 {"GetNameOwner", "GetActive", "Inhibit", "NameHasOwner"})
                # Starting on an already locked desktop cannot create readiness.
                receipt.unlink()
                state.write_text(json.dumps(dict(locked="yes")))
                result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("unlocked", result.stderr)
                self.assertFalse(receipt.exists())
                self.assertFalse(list(root.glob("cookie-*")))
                state.write_text("{}")
                result = subprocess.run([*command[:-1], "1"], env=environment, capture_output=True, text=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("lifetime expired", result.stderr)
                self.assertEqual(json.loads(receipt.read_text())["status"], "fail")
                self.assertFalse(list(root.glob("cookie-*")))
            finally:
                if guard.poll() is None:
                    guard.terminate()
                    guard.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
