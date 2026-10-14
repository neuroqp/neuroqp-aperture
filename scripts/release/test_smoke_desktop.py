"""Functional adapter seams only. These checks are not native GUI evidence."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("desktop_smoke", Path(__file__).with_name("smoke-desktop-linux.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class DesktopSmokeTests(unittest.TestCase):
    @unittest.skipUnless(hasattr(smoke.signal, "SIGALRM") and hasattr(smoke.signal, "setitimer"), "POSIX observer timer")
    def test_step_guard_interrupts_blocking_action_and_restores_alarm(self):
        handler = smoke.signal.getsignal(smoke.signal.SIGALRM)
        started = time.monotonic()
        with self.assertRaisesRegex(TimeoutError, "deadline"):
            with smoke.bounded_action(.02):
                time.sleep(2)
        self.assertLess(time.monotonic() - started, .5)
        self.assertEqual(smoke.signal.getsignal(smoke.signal.SIGALRM), handler)
        self.assertEqual(smoke.signal.getitimer(smoke.signal.ITIMER_REAL), (0, 0))
        with self.assertRaisesRegex(TimeoutError, "deadline"):
            with smoke.bounded_action(0):
                self.fail("Expired action executed")
        smoke.signal.setitimer(smoke.signal.ITIMER_REAL, 2, 3)
        try:
            with smoke.bounded_action(.02):
                time.sleep(.005)
            delay, interval = smoke.signal.getitimer(smoke.signal.ITIMER_REAL)
            self.assertGreater(delay, 1.5)
            self.assertLess(delay, 2)
            self.assertEqual(interval, 3)
            self.assertEqual(smoke.signal.getsignal(smoke.signal.SIGALRM), handler)
        finally:
            smoke.signal.setitimer(smoke.signal.ITIMER_REAL, 0)

    def test_hidden_accessibility_nodes_count_toward_traversal_cap(self):
        class Node:
            def __init__(self, children=()):
                self.children = children
            def get_state_set(self):
                return SimpleNamespace(contains=lambda state: False)
            def get_child_count(self):
                return len(self.children)
            def get_child_at_index(self, index):
                return self.children[index]
            def get_process_id(self):
                return 42
        app = Node([Node() for _ in range(501)])
        atspi = SimpleNamespace(set_timeout=lambda *args: None, get_desktop=lambda index: Node([app]), StateType=SimpleNamespace(SHOWING=1))
        modules = {"gi": SimpleNamespace(require_version=lambda *args: None), "gi.repository": SimpleNamespace(Atspi=atspi)}
        with patch.dict(smoke.sys.modules, modules):
            with self.assertRaisesRegex(ValueError, "500 visited"):
                smoke.native_nodes(42, time.monotonic() + 1)
            with self.assertRaisesRegex(TimeoutError, "deadline"):
                smoke.native_nodes(42, time.monotonic() - 1)

    def test_arch_desktop_accepts_arch_and_arch_based_omarchy_only(self):
        smoke.assert_os("arch-omarchy", dict(ID="arch"))
        smoke.assert_os("arch-omarchy", dict(ID="omarchy", ID_LIKE="arch", VERSION_ID="4.0.4"))
        for actual in (dict(ID="omarchy"), dict(ID="omarchy", ID_LIKE="debian"), dict(ID="other", ID_LIKE="arch")):
            with self.assertRaisesRegex(ValueError, "Actual OS differs"):
                smoke.assert_os("arch-omarchy", actual)
        with self.assertRaisesRegex(ValueError, "Actual OS differs"):
            smoke.assert_os("fedora-wayland", dict(ID="omarchy", ID_LIKE="arch"))
        with self.assertRaisesRegex(ValueError, "Actual OS differs"):
            smoke.assert_os("debian12", dict(ID="debian", VERSION_ID="13"))

    def test_one_shot_chooser_restores_real_invoke_and_only_supplies_path(self):
        script = """
const assert=require('node:assert/strict'),calls=[];
const original=function(command){calls.push(command);return Promise.resolve('actual '+command);};
global.window={__TAURI_INTERNALS__:{invoke:original}};
let choice;
global.document={getElementById:id=>{assert.equal(id,'open-button');return {click:()=>{
  const api=window.__TAURI_INTERNALS__;
  api.invoke('image_extensions');choice=api.invoke('plugin:dialog|open');
}};}};
new Function(SCRIPT)('/prepared/tiny.tif');
choice.then(async path=>{
  assert.equal(path,'/prepared/tiny.tif');
  assert.equal(window.__TAURI_INTERNALS__.invoke,original);
  assert.equal(window.__apertureSmokeChooser.restored,true);
  assert.equal(await window.__TAURI_INTERNALS__.invoke('image_state'),'actual image_state');
  assert.deepEqual(calls,['image_extensions','image_state']);
}).catch(error=>{console.error(error);process.exitCode=1;});
""".replace("SCRIPT", json.dumps(smoke.OPEN_TIFF))
        subprocess.run(["node", "-e", script], check=True, timeout=5)

    def test_normal_exit_requires_same_pid_zero_and_absent_process(self):
        with patch.object(smoke.Path, "exists", return_value=False):
            smoke.assert_exited(dict(pid=42, exit_code=0), 42)
            for result in (dict(pid=43, exit_code=0), dict(pid=42, exit_code=1), dict(pid=42, exit_code=-15)):
                with self.assertRaisesRegex(ValueError, "exit normally"):
                    smoke.assert_exited(result, 42)
        with patch.object(smoke.Path, "exists", return_value=True), self.assertRaisesRegex(ValueError, "exit normally"):
            smoke.assert_exited(dict(pid=42, exit_code=0), 42)

    def test_ready_is_required_even_with_preview_and_pixels(self):
        identity = dict(tiny_tiff_path="/prepared/tiny.tif", tiny_tiff_width=65, tiny_tiff_height=49)
        for observation in ({}, dict(state=None), dict(state={}, phase=None, error=False)):
            self.assertFalse(smoke.assert_rendered(observation, identity))
        state = dict(path=identity["tiny_tiff_path"], width=65, height=49, phase="preview", previewReady=True)
        observation = dict(state=state, phase="preview", error=False)
        self.assertFalse(smoke.assert_rendered(observation, identity))
        state["phase"] = observation["phase"] = "ready"
        self.assertTrue(smoke.assert_rendered(observation, identity))
        observation["error"] = True
        with self.assertRaisesRegex(ValueError, "error"):
            smoke.assert_rendered(observation, identity)
        observation["error"] = False
        state["width"] = 66
        with self.assertRaisesRegex(ValueError, "dimensions"):
            smoke.assert_rendered(observation, identity)

    def test_capture_samples_only_visible_native_viewer(self):
        with tempfile.TemporaryDirectory() as folder:
            capture = Path(folder) / "desktop.ppm"
            capture.write_bytes(b"P6\n200 150\n255\n" + bytes(v for y in range(150) for x in range(200) for v in [x, x, x]))
            self.assertGreater(smoke.ppm_spread(capture, [10, 10, 180, 120])["spread"], 35)
            with self.assertRaisesRegex(ValueError, "outside"):
                smoke.ppm_spread(capture, [100, 10, 180, 120])
            capture.write_bytes(b"P6\n200 150\n255\n" + bytes([44] * 200 * 150 * 3))
            self.assertEqual(smoke.ppm_spread(capture, [10, 10, 180, 120])["spread"], 0)

    @unittest.skipUnless(hasattr(smoke.os, "getuid"), "Linux executable and UID validation")
    def test_asset_and_helper_identity_verified_before_launch(self):
        with tempfile.TemporaryDirectory() as folder:
            asset, image = Path(folder) / "incoming.AppImage", Path(folder) / "tiny.tif"
            asset.write_bytes(b"exact candidate")
            asset.chmod(0o700)
            image.write_bytes(b"source fixture")
            tool = Path(folder) / "prepared-observer"
            tool.write_bytes(b"fixed observer")
            identity = dict(asset_path=str(asset), asset_sha256=smoke.sha(asset), tiny_tiff_path=str(image), tiny_tiff_sha256=smoke.sha(image), screenshot_command=[str(tool), "{output}"], readiness=dict(ready=True, helper_sha256=smoke.sha(smoke.__file__), tools=[dict(name="observer", path=str(tool), sha256=smoke.sha(tool))]))
            with patch.object(smoke.os, "getuid", return_value=1000), patch.object(smoke.sys, "executable", str(tool)), patch.object(smoke.shutil, "which", return_value=str(tool)):
                smoke.verify_inputs(identity)
                asset.chmod(0o600)
                with self.assertRaisesRegex(ValueError, "chmod \\+x"):
                    smoke.verify_inputs(identity)
                asset.chmod(0o700)
                identity["screenshot_command"][0] = "/unpinned/screenshot"
                with self.assertRaisesRegex(ValueError, "not Stage A pinned"):
                    smoke.verify_inputs(identity)
                identity["screenshot_command"][0] = str(tool)
                identity["readiness"]["helper_sha256"] = "0" * 64
                with self.assertRaisesRegex(ValueError, "helper changed"):
                    smoke.verify_inputs(identity)
                identity["readiness"]["helper_sha256"] = smoke.sha(smoke.__file__)
                tool.write_bytes(b"observer repaired after freeze")
                with self.assertRaisesRegex(ValueError, "observer tool changed"):
                    smoke.verify_inputs(identity)
                tool.write_bytes(b"fixed observer")
                asset.write_bytes(b"changed bytes")
                with self.assertRaisesRegex(ValueError, "asset SHA256"):
                    smoke.verify_inputs(identity)

    def test_launcher_os_error_records_failure_receipt_and_log(self):
        with tempfile.TemporaryDirectory() as folder:
            receipt = Path(folder) / "exit.json"
            error = PermissionError(13, "Permission denied", "/prepared/AppImage")
            with patch.object(smoke, "verify_inputs"), patch.object(smoke.subprocess, "Popen", side_effect=error):
                self.assertEqual(smoke.launcher(dict(asset_path="/prepared/AppImage"), receipt, []), 1)
            result = json.loads(receipt.read_text())
            self.assertEqual(result["exit_code"], 1)
            self.assertIsNone(result["pid"])
            self.assertIn("Permission denied", result["error"])
            self.assertIn("Permission denied", Path(str(receipt) + ".log").read_text())
            self.assertFalse(Path(str(receipt) + ".pid").exists())

    def test_launcher_clears_inherited_extract_mode_and_uses_explicit_fallback(self):
        for fallback in (False, True):
            with tempfile.TemporaryDirectory() as folder:
                receipt = Path(folder) / "exit.json"
                child = SimpleNamespace(pid=42, wait=lambda: 0)
                with patch.object(smoke, "verify_inputs"), patch.object(smoke.subprocess, "Popen", return_value=child) as launch, patch.dict(smoke.os.environ, {"APPIMAGE_EXTRACT_AND_RUN": "1", "APPIMAGE_EXTRACT_AND_RUN_DEFAULT": "1"}):
                    self.assertEqual(smoke.launcher(dict(asset_path="/prepared/AppImage", extract_and_run=fallback), receipt, []), 0)
                    command = launch.call_args.args[0]
                    environment = launch.call_args.kwargs["env"]
                    self.assertNotIn("APPIMAGE_EXTRACT_AND_RUN", environment)
                    self.assertNotIn("APPIMAGE_EXTRACT_AND_RUN_DEFAULT", environment)
                    self.assertEqual("--appimage-extract-and-run" in command, fallback)

    @unittest.skipUnless(hasattr(smoke.os, "getuid"), "Linux native driver UID validation")
    def test_driver_requires_owned_pinned_native_listener(self):
        identity = dict(driver_url="http://127.0.0.1:4445", readiness=dict(tools=[dict(path="/prepared/WebKitWebDriver")]))
        row = 'LISTEN 0 5 127.0.0.1:4445 0.0.0.0:* users:(("WebKitWebDrive",pid=42,fd=7))'
        executable = Path("/prepared/WebKitWebDriver")
        result = SimpleNamespace(stdout=row)
        stat = SimpleNamespace(st_uid=1000)
        with patch.object(smoke.subprocess, "run", return_value=result), patch.object(smoke.Path, "resolve", autospec=True, side_effect=lambda path: executable if str(path) == "/proc/42/exe" else path), patch.object(smoke.Path, "stat", return_value=stat), patch.object(smoke.os, "getuid", return_value=1000):
            self.assertEqual(smoke.verify_driver(identity, 1)["pid"], 42)
            for executable in (Path("/prepared/tauri-driver"), Path("/unpinned/WebKitWebDriver")):
                with self.subTest(executable=executable), self.assertRaisesRegex(ValueError, "WebKitWebDriver listener directly"):
                    smoke.verify_driver(identity, 1)
            executable = Path("/prepared/WebKitWebDriver")
            stat.st_uid = 2000
            with self.assertRaisesRegex(ValueError, "owned"):
                smoke.verify_driver(identity, 1)
            stat.st_uid = 1000
            for result.stdout in ("", row.replace("4445", "4446"), row + '\n' + row.replace("pid=42", "pid=43")):
                with self.subTest(listeners=result.stdout), self.assertRaisesRegex(ValueError, "Exactly one"):
                    smoke.verify_driver(identity, 1)
            result.stdout = row
            for identity["driver_url"] in ("http://remote.example:4445", "http://127.0.0.1:4445/session", "http://user@127.0.0.1:4445"):
                with self.subTest(endpoint=identity["driver_url"]), self.assertRaisesRegex(ValueError, "loopback"):
                    smoke.verify_driver(identity, 1)


if __name__ == "__main__":
    unittest.main()
