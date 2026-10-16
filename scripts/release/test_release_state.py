#!/usr/bin/env python3
"""Critical release identity and idempotency checks."""
import copy
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("state", Path(__file__).with_name("release-state.py"))
state = importlib.util.module_from_spec(spec)
spec.loader.exec_module(state)


class ReleaseStateTests(unittest.TestCase):
    def test_new_draft_uses_creation_response_without_public_tag_lookup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / "release"
            directory.mkdir()
            (directory / "latest.json").write_bytes(b"{}")
            package = {"operation": "package", "version": "0.2.1", "source_sha": "a" * 40,
                       "assets": {"latest.json": state.digest(directory / "latest.json")}}
            package_path, smoke_path = root / "package.json", root / "smoke.json"
            package_path.write_text(json.dumps(package))
            smoke_path.write_text("{}")
            draft = {"id": 123, "tag_name": "v0.2.1", "draft": True, "prerelease": False,
                     "assets": [], "html_url": "https://github.com/example/release"}
            uploaded = {**draft, "assets": [{"name": "latest.json", "digest": "sha256:" + package["assets"]["latest.json"]}]}
            published = {**uploaded, "draft": False}
            responses = [json.dumps({"object": {"type": "commit", "sha": package["source_sha"]}}), "[[]]",
                         json.dumps(draft), "", json.dumps(uploaded), "", json.dumps(published), json.dumps(published)]
            with patch.object(state, "source_identity"), patch.object(state, "acceptance"), patch.object(state, "verify"), patch.object(state, "verify_signatures"), patch.object(state, "notes", return_value="Current notes\n"), patch.object(state, "gh", side_effect=responses) as github, patch("urllib.request.urlopen", return_value=io.BytesIO(b"{}")), patch("builtins.print"):
                state.publish(directory, package_path, smoke_path)
            calls = [call.args for call in github.call_args_list]
            self.assertTrue(calls[0][1].endswith("/git/ref/tags/v0.2.1"))
            for field in ("POST", "tag_name=v0.2.1", "draft=true", "body=Current notes\n"):
                self.assertIn(field, calls[2])
            self.assertEqual(calls[4], ("api", f"repos/{state.REPO}/releases/123"))
            self.assertFalse(any("/releases/tags/" in arg for call in calls for arg in call))

    def test_existing_uploads_reuse_only_identical_bytes(self):
        expected = {"a.exe": "a" * 64, "b.sig": "b" * 64}
        self.assertEqual(state.existing_assets([{"name": "a.exe", "digest": "sha256:" + "a" * 64}], expected), {"b.sig"})
        for rows in ([{"name": "a.exe", "digest": "sha256:" + "b" * 64}], [{"name": "extra", "digest": "sha256:" + "a" * 64}], [{"name": "a.exe", "digest": None}]):
            with self.assertRaises(ValueError):
                state.existing_assets(rows, expected)

    def test_smoke_binds_packages_and_normal_visible_close(self):
        assets = {name: str(n) * 64 for n, name in enumerate(state.ASSETS.values(), 1)}
        identity = {"source_sha": "a" * 40, "checksums_sha256": "b" * 64, "assets": assets}
        report = {"schema": 1, "operation": "smoke", "source_sha": identity["source_sha"], "checksums_sha256": identity["checksums_sha256"], "reports": []}
        for env in sorted(state.ENVIRONMENTS):
            name = "Aperture-Setup-x64.exe" if env == "windows11-setup" else "Aperture-Portable-x64.exe" if env == "windows11-portable" else "Aperture-x86_64.AppImage"
            report["reports"].append({"schema": 1, "operation": "smoke", "source_sha": identity["source_sha"], "checksums_sha256": identity["checksums_sha256"], "environment": env, "status": "pass", "forced_kill": False, "asset_sha256": assets[name], "steps": [
                {"name": "launch", "status": "pass"},
                {"name": "render", "status": "pass", "detail": {"observation": {"phase": "ready", "rendered_canvas": True, "state": {"phase": "ready", "previewReady": True}}, "pixel_observation": {"spread": 40, "samples": [0, 40]}}},
                {"name": "clean-exit", "status": "pass", "detail": {"exit_code": 0, "forced_cleanup": False, "app_exited": True}}]})
        state.acceptance(report, identity, smoke=True)
        for mutation in (lambda r: r.update(checksums_sha256="c" * 64),
                         lambda r: r["reports"].pop(),
                         lambda r: r["reports"][0].update(asset_sha256="f" * 64),
                         lambda r: r["reports"][0]["steps"][1]["detail"]["pixel_observation"].update(spread=0),
                         lambda r: r["reports"][0]["steps"][2]["detail"].update(forced_cleanup=True)):
            invalid = copy.deepcopy(report)
            mutation(invalid)
            with self.assertRaises(ValueError):
                state.acceptance(invalid, identity, smoke=True)

    def test_validation_rejects_pass_stubs_and_missing_native_observations(self):
        identity = {"source_sha": "a" * 40, "platforms": {platform: {"binary_sha256": digit * 64} for platform, digit in (("windows", "1"), ("linux", "2"))}}
        report = {"schema": 1, "operation": "validate", "source_sha": identity["source_sha"], "reports": []}
        for env in sorted(state.FUNCTIONAL_ENVIRONMENTS):
            platform = "windows" if env == "windows11" else "linux"
            report["reports"].append({"schema": 1, "operation": "validate", "source_sha": identity["source_sha"], "environment": env, "status": "pass", "forced_kill": False, "binary_sha256": identity["platforms"][platform]["binary_sha256"], "steps": [
                {"name": "launch", "status": "pass"},
                {"name": "render", "status": "pass", "detail": {"observation": {"phase": "ready", "rendered_canvas": True, "state": {"phase": "ready", "previewReady": True}}, "pixel_observation": {"spread": 40, "samples": [0, 40]}}},
                {"name": "clean-exit", "status": "pass", "detail": {"exit_code": 0, "forced_cleanup": False, "app_exited": True}}]})
        state.acceptance(report, identity)
        for mutation in (lambda r: [row.pop(key) for row in r["reports"] for key in ("schema", "operation", "steps")],
                         lambda r: r["reports"][0].update(operation="smoke"),
                         lambda r: r["reports"][0].update(steps=[]),
                         lambda r: r["reports"][0]["steps"][1]["detail"].pop("observation"),
                         lambda r: r["reports"][0]["steps"][1]["detail"]["observation"]["state"].update(phase="error"),
                         lambda r: r["reports"][0]["steps"][1]["detail"].pop("pixel_observation"),
                         lambda r: r["reports"][0]["steps"][2]["detail"].update(exit_code=1)):
            invalid = copy.deepcopy(report)
            mutation(invalid)
            with self.assertRaises(ValueError):
                state.acceptance(invalid, identity)

    def test_evidence_has_no_expiry_and_cannot_be_overwritten(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "validation.json"
            state.write(path, {"source_sha": "a" * 40})
            state.write(path, {"source_sha": "a" * 40})
            with self.assertRaises(ValueError):
                state.write(path, {"source_sha": "b" * 40})

    def test_archive_rejects_traversal_and_links(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, link in (("../escape", False), ("safe", True)):
                archive = root / "archive.tar.gz"
                with tarfile.open(archive, "w:gz") as output:
                    item = tarfile.TarInfo(name)
                    if link:
                        item.type, item.linkname = tarfile.SYMTYPE, "/tmp/outside"
                    output.addfile(item)
                with self.assertRaises(ValueError):
                    state.extract(archive, root / "out")


if __name__ == "__main__":
    unittest.main()
