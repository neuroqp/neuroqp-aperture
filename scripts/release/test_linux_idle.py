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
    def test_optional_guard_releases_cookie_on_signal_or_timeout(self):
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
                guard.send_signal(signal.SIGHUP)
                stdout, stderr = guard.communicate(timeout=5)
                self.assertEqual(guard.returncode, 0, stderr)
                self.assertEqual(json.loads(receipt.read_text())["status"], "closed")
                self.assertFalse((root / ("cookie-" + str(guard.pid))).exists())
                # No activation, unlock, simulated-input or preference calls.
                self.assertEqual(set((root / "calls").read_text().splitlines()),
                                 {"GetNameOwner", "GetActive", "Inhibit"})
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
