"""Functional adapter seams only. These checks are not native GUI evidence."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("desktop_smoke", Path(__file__).with_name("smoke-desktop-linux.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class DesktopSmokeTests(unittest.TestCase):
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

    def test_asset_and_helper_identity_verified_before_launch(self):
        with tempfile.TemporaryDirectory() as folder:
            asset, image = Path(folder) / "incoming.AppImage", Path(folder) / "tiny.tif"
            asset.write_bytes(b"exact candidate")
            image.write_bytes(b"source fixture")
            tool = Path(folder) / "prepared-observer"
            tool.write_bytes(b"fixed observer")
            identity = dict(asset_path=str(asset), asset_sha256=smoke.sha(asset), tiny_tiff_path=str(image), tiny_tiff_sha256=smoke.sha(image), readiness=dict(ready=True, helper_sha256=smoke.sha(smoke.__file__), tools=[dict(name="observer", path=str(tool), sha256=smoke.sha(tool))]))
            with patch.object(smoke.os, "getuid", return_value=1000):
                smoke.verify_inputs(identity)
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


if __name__ == "__main__":
    unittest.main()
