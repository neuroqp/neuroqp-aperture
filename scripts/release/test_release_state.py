#!/usr/bin/env python3
"""Critical release identity and idempotency checks."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("state", Path(__file__).with_name("release-state.py"))
state = importlib.util.module_from_spec(spec)
spec.loader.exec_module(state)

SOURCE, MAIN, WORKFLOW = "a" * 40, "b" * 40, "c" * 40
WORKFLOWS = Path(__file__).resolve().parents[2] / ".github" / "workflows"


def main_ref(sha=MAIN, kind="commit", ref="refs/heads/main"):
    return json.dumps({"ref": ref, "object": {"type": kind, "sha": sha}})


def comparison(status="ahead", base=SOURCE, merge_base=None, behind=0):
    return json.dumps({"status": status, "behind_by": behind, "base_commit": {"sha": base},
                       "merge_base_commit": {"sha": base if merge_base is None else merge_base}})


def workflow_jobs(name):
    body = (WORKFLOWS / name).read_text(encoding="utf-8").split("\njobs:\n", 1)[1]
    return dict(re.findall(r"(?ms)^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)", body))


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

    def test_trusted_source_accepts_main_ancestors_and_current_main(self):
        for head, status in ((MAIN, "ahead"), (SOURCE, "identical")):
            with patch.object(state, "gh", side_effect=[main_ref(head), comparison(status)]) as github:
                self.assertEqual(state.main_commit(SOURCE), SOURCE)
            # The comparison is pinned to the resolved main head, never to a ref name.
            self.assertEqual([call.args for call in github.call_args_list],
                             [("api", f"repos/{state.REPO}/git/ref/heads/main"),
                              ("api", f"repos/{state.REPO}/compare/{SOURCE}...{head}")])

    def test_trusted_source_rejects_unreviewed_or_redirected_commits(self):
        cases = {
            "diverged branch": (main_ref(), comparison("diverged", merge_base="d" * 40)),
            "newer than main": (main_ref(), comparison("behind", merge_base=MAIN, behind=1)),
            "reported behind main": (main_ref(), comparison(behind=1)),
            "compare resolved another commit": (main_ref(), comparison(base="d" * 40)),
            "different merge base": (main_ref(), comparison(merge_base="d" * 40)),
            "missing comparison fields": (main_ref(), "{}"),
            "main is not a commit": (main_ref(kind="tag"),),
            "unexpected main reference": (main_ref(ref="refs/heads/main-old"),),
            "malformed main head": (main_ref(sha="main"),),
        }
        for name, responses in cases.items():
            with self.subTest(name), patch.object(state, "gh", side_effect=responses):
                with self.assertRaises(ValueError):
                    state.main_commit(SOURCE)

    def test_trusted_source_rejects_moving_or_abbreviated_refs_without_api_calls(self):
        for value in (None, "", "main", "HEAD", "refs/heads/main", "v0.3.0", "a" * 39, "a" * 41,
                      "A" * 40, "a" * 39 + "g", " " + "a" * 40, "a" * 40 + "\n"):
            with self.subTest(value=value), patch.object(state, "gh") as github:
                with self.assertRaises(ValueError):
                    state.main_commit(value)
                github.assert_not_called()

    def test_trusted_source_fails_closed_on_api_errors(self):
        failures = (subprocess.CalledProcessError(1, ["gh"]), subprocess.TimeoutExpired(["gh"], 600))
        for failure in failures:
            for responses in ([failure], [main_ref(), failure], ["not json"], [main_ref(), "<html>"]):
                with self.subTest(responses=responses), patch.object(state, "gh", side_effect=responses):
                    with self.assertRaises((subprocess.SubprocessError, ValueError)):
                        state.main_commit(SOURCE)

    def test_trusted_source_command_prints_only_verified_commits(self):
        argv = ["release-state.py", "trusted-source", SOURCE]
        for responses, expected in (([main_ref(), comparison()], SOURCE + "\n"),
                                    ([main_ref(), comparison("diverged")], "")):
            output = io.StringIO()
            with patch("sys.argv", argv), patch.object(state, "gh", side_effect=responses), contextlib.redirect_stdout(output):
                if expected:
                    state.main()
                else:
                    with self.assertRaises(ValueError):
                        state.main()
            self.assertEqual(output.getvalue(), expected)

    def test_trusted_run_requires_workflow_revision_in_main(self):
        row = {"repository": {"full_name": state.REPO}, "head_branch": "main", "head_sha": WORKFLOW,
               "event": "workflow_dispatch", "path": ".github/workflows/validate.yml",
               "status": "completed", "conclusion": "success"}
        accepted = [json.dumps(row), main_ref(), comparison(base=WORKFLOW), main_ref(), comparison()]
        with patch.object(state, "gh", side_effect=accepted):
            self.assertEqual(state.trusted_run("1", "validate.yml", SOURCE), row)
        # For example a tag named main at an unreviewed commit with a modified workflow.
        rejected = [json.dumps(row), main_ref(), comparison("diverged", base=WORKFLOW, merge_base="d" * 40)]
        with patch.object(state, "gh", side_effect=rejected):
            with self.assertRaises(ValueError):
                state.trusted_run("1", "validate.yml", SOURCE)

    def test_dispatched_source_is_checked_out_only_after_main_containment(self):
        guard = "needs.trust.outputs.revision == inputs.source_sha"
        trusted = (WORKFLOWS / "trusted-source.yml").read_text(encoding="utf-8")
        # The gate runs only the calling workflow revision's code and writes no cache.
        self.assertNotRegex(trusted, r"\bref:")
        self.assertEqual([action.split("@")[0] for action in re.findall(r"uses: (\S+)", trusted)], ["actions/checkout"])
        self.assertIn('revision="$(python3 scripts/release/release-state.py trusted-source "$SOURCE_SHA")"', trusted)
        for name in ("validate.yml", "package.yml", "publish.yml"):
            jobs = workflow_jobs(name)
            self.assertIn("uses: ./.github/workflows/trusted-source.yml", jobs["trust"])
            guarded = {job for job, body in jobs.items() if re.search(r"^    if: .*" + re.escape(guard) + r"$", body, re.M)}
            for job, body in jobs.items():
                if job == "trust" or "actions/checkout@" not in body:
                    continue
                with self.subTest(workflow=name, job=job):
                    needs = re.search(r"^    needs: \[?([^\]\n]*)", body, re.M)
                    needed = {item.strip() for item in needs.group(1).split(",")} if needs else set()
                    self.assertIn("trust", needed)
                    self.assertTrue(job in guarded or needed & guarded)
                    self.assertEqual(body.count("actions/checkout@"), body.count("ref: ${{ needs.trust.outputs.revision }}"))
                    self.assertNotRegex(body, r"ref: \$\{\{ *(github\.event\.)?inputs\.")


if __name__ == "__main__":
    unittest.main()
