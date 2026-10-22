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
from unittest.mock import Mock, patch

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


def job_condition(body):
    """Return a job-level if expression, joining folded continuation lines."""
    match = re.search(r"^    if: (?:>-\n((?:      .*\n)+)|(.*)$)", body, re.M)
    return " ".join((match.group(1) or match.group(2)).split()) if match else ""


def gate_call(head=WORKFLOW, gate=None, repo=None, ref="refs/heads/main", sha=None):
    """A referenced_workflows entry shaped like GitHub's for a local reusable workflow call.

    Live python/cpython runs, including workflow_dispatch on main, report
    {"path": "OWNER/REPO/.github/workflows/FILE.yml@HEAD_SHA", "ref": "refs/heads/main", "sha": HEAD_SHA}.
    """
    return {"path": f"{repo or state.REPO}/{gate or state.TRUST_GATE}@{head}", "ref": ref, "sha": sha or head}


def run_row(**changes):
    head = changes.get("head_sha", WORKFLOW)
    return {"repository": {"full_name": state.REPO}, "head_branch": "main", "head_sha": WORKFLOW,
            "event": "workflow_dispatch", "path": ".github/workflows/validate.yml",
            "status": "completed", "conclusion": "success", "referenced_workflows": [gate_call(head)], **changes}


def historical_trusted_run(gh, run_id, workflow, source_sha=None):
    """trusted_run from main 8fafb1f with gh injected, as an old source_sha checkout would run it."""
    if not re.fullmatch(r"[1-9][0-9]*", str(run_id)):
        raise ValueError("Expected a workflow run ID")
    row = json.loads(gh("api", f"repos/{state.REPO}/actions/runs/{run_id}"))
    if (row.get("repository", {}).get("full_name") != state.REPO or row.get("head_branch") != "main"
            or row.get("event") != "workflow_dispatch" or row.get("path") != ".github/workflows/" + workflow
            or row.get("status") != "completed" or row.get("conclusion") != "success"):
        raise ValueError("Successful trusted main workflow required")
    if source_sha:
        comparison = json.loads(gh("api", f"repos/{state.REPO}/compare/{source_sha}...main"))
        if comparison.get("status") not in ("ahead", "identical"):
            raise ValueError("Source commit must be contained in main")
    return row


def smoke_report():
    """A four-target passing final smoke report and the package identity it binds."""
    assets = {name: str(n) * 64 for n, name in enumerate(state.ASSETS.values(), 1)}
    identity = {"source_sha": "a" * 40, "checksums_sha256": "b" * 64, "assets": assets}
    report = {"schema": 1, "operation": "smoke", "source_sha": identity["source_sha"], "checksums_sha256": identity["checksums_sha256"], "reports": []}
    for env in sorted(state.ENVIRONMENTS):
        name = "Aperture-Setup-x64.exe" if env == "windows11-setup" else "Aperture-Portable-x64.exe" if env == "windows11-portable" else "Aperture-x86_64.AppImage"
        report["reports"].append({"schema": 1, "operation": "smoke", "source_sha": identity["source_sha"], "checksums_sha256": identity["checksums_sha256"], "environment": env, "status": "pass", "forced_kill": False, "asset_sha256": assets[name], "steps": [
            {"name": "launch", "status": "pass"},
            {"name": "render", "status": "pass", "detail": {"observation": {"phase": "ready", "rendered_canvas": True, "state": {"phase": "ready", "previewReady": True}}, "pixel_observation": {"spread": 40, "samples": [0, 40]}}},
            {"name": "clean-exit", "status": "pass", "detail": {"exit_code": 0, "forced_cleanup": False, "app_exited": True}}]})
    return report, identity


def unverified(row, **changes):
    """Replace a passing row with an unrun target that keeps its exact package identity."""
    row = {key: row[key] for key in ("schema", "operation", "source_sha", "checksums_sha256", "environment", "asset_sha256")}
    return {**row, "status": "unverified", "forced_kill": False, "reason": "os-unavailable",
            "detail": "Host memory was below the VM launch gate.", "evidence": "unavailable-targets-resource-receipt.json",
            "evidence_sha256": "e" * 64, "follow_up": "Run the package smoke when the VM host has capacity.", **changes}


def trusted_preflight(source_sha, run_workflow, run_id, responses):
    """Run the trusted-inputs workflow's command and return its GITHUB_OUTPUT lines."""
    argv = ["release-state.py", "trusted-source", source_sha, "--run-workflow", run_workflow, "--run-id", run_id]
    output = io.StringIO()
    with patch("sys.argv", argv), patch.object(state, "gh", side_effect=responses) as github, \
            contextlib.redirect_stdout(output):
        try:
            state.main()
        finally:
            trusted_preflight.calls = [call.args for call in github.call_args_list]
            trusted_preflight.output = output.getvalue()
    return output.getvalue()


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
            with patch.object(state, "source_identity"), patch.object(state, "acceptance", return_value={"reports": []}), patch.object(state, "verify"), patch.object(state, "verify_signatures") as signatures, patch.object(state, "notes", return_value="Current notes\n") as changelog, patch.object(state, "gh", side_effect=responses) as github, patch("urllib.request.urlopen", return_value=io.BytesIO(b"{}")), patch("builtins.print"):
                state.publish(directory, package_path, smoke_path)
            calls = [call.args for call in github.call_args_list]
            self.assertTrue(calls[0][1].endswith("/git/ref/tags/v0.2.1"))
            for field in ("POST", "tag_name=v0.2.1", "draft=true", "body=Current notes\n"):
                self.assertIn(field, calls[2])
            self.assertEqual(calls[4], ("api", f"repos/{state.REPO}/releases/123"))
            self.assertEqual(calls[5][-2:], ("--notes", "Current notes\n"))
            self.assertFalse(any("/releases/tags/" in arg for call in calls for arg in call))
            # Notes and the updater key come from the exact source checkout, not the helper checkout.
            self.assertEqual(changelog.call_args.args, ("0.2.1", Path.cwd()))
            self.assertEqual(signatures.call_args.kwargs, {"root": Path.cwd()})

    def test_existing_uploads_reuse_only_identical_bytes(self):
        expected = {"a.exe": "a" * 64, "b.sig": "b" * 64}
        self.assertEqual(state.existing_assets([{"name": "a.exe", "digest": "sha256:" + "a" * 64}], expected), {"b.sig"})
        for rows in ([{"name": "a.exe", "digest": "sha256:" + "b" * 64}], [{"name": "extra", "digest": "sha256:" + "a" * 64}], [{"name": "a.exe", "digest": None}]):
            with self.assertRaises(ValueError):
                state.existing_assets(rows, expected)

    def test_smoke_binds_packages_and_normal_visible_close(self):
        report, identity = smoke_report()
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

    def test_unverified_targets_need_accepted_risk_and_cannot_hide_failures(self):
        report, identity = smoke_report()
        rows = {row["environment"]: index for index, row in enumerate(report["reports"])}
        for env in ("windows11-setup", "windows11-portable", "ubuntu24-wayland"):
            report["reports"][rows[env]] = unverified(report["reports"][rows[env]])
        self.assertIs(state.acceptance(report, identity, smoke=True, accept_unverified=True), report)
        with self.assertRaisesRegex(ValueError, "explicitly accepted"):
            state.acceptance(report, identity, smoke=True)
        windows = rows["windows11-setup"]
        debian = rows["debian12"]
        for mutation in (lambda r: r["reports"][windows].update(status="skipped"),
                         lambda r: r["reports"][windows].update(asset_sha256=r["reports"][debian]["asset_sha256"]),
                         lambda r: r["reports"][windows].update(checksums_sha256="c" * 64),
                         lambda r: r["reports"][windows].update(source_sha="d" * 40),
                         lambda r: r["reports"][windows].update(forced_kill=True),
                         lambda r: r["reports"][windows].update(steps=copy.deepcopy(r["reports"][debian]["steps"])),
                         lambda r: r["reports"][windows].update(reason="application-failure"),
                         lambda r: r["reports"][windows].pop("evidence"),
                         lambda r: r["reports"][windows].update(evidence_sha256="not-a-digest"),
                         lambda r: r["reports"][windows].update(follow_up=" "),
                         lambda r: r["reports"][windows].update(detail="one\ntwo"),
                         lambda r: r["reports"].pop(windows),
                         lambda r: r["reports"].__setitem__(debian, copy.deepcopy(r["reports"][windows])),
                         lambda r: r["reports"].__setitem__(debian, unverified(r["reports"][debian])),
                         lambda r: r["reports"][debian].update(status="fail"),
                         lambda r: r["reports"][debian]["steps"][2]["detail"].update(exit_code=1)):
            invalid = copy.deepcopy(report)
            mutation(invalid)
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                state.acceptance(invalid, identity, smoke=True, accept_unverified=True)
        # Validation evidence never accepts unverified rows.
        with self.assertRaises(ValueError):
            state.acceptance({**report, "operation": "validate"}, identity, accept_unverified=True)

    def test_release_notes_name_unverified_targets_as_known_limitations(self):
        report, _ = smoke_report()
        with patch.object(state, "notes", return_value="Changelog notes\n"):
            self.assertEqual(state.release_notes("0.3.0", Path.cwd(), report, "f" * 64), "Changelog notes\n")
            for index, row in enumerate(report["reports"]):
                if row["environment"] != "debian12":
                    report["reports"][index] = unverified(row)
            body = state.release_notes("0.3.0", Path.cwd(), report, "f" * 64)
        self.assertTrue(body.startswith("Changelog notes\n\n### Known limitations\n"))
        self.assertIn("accepted OS compatibility risk. The final package smoke passed on Debian 12.", body)
        for env in ("windows11-setup", "windows11-portable", "ubuntu24-wayland"):
            self.assertIn(f"(`{env}`, os-unavailable): Host memory was below the VM launch gate. Follow-up:", body)
        self.assertNotIn("debian12", body)
        self.assertTrue(body.endswith(f"Final smoke evidence SHA-256: `{'f' * 64}`\n"))

    def test_source_identity_reads_the_working_directory_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src-tauri").mkdir()
            (root / "package.json").write_text('{"version": "9.8.7"}')
            (root / "src-tauri/tauri.conf.json").write_text('{"version": "9.8.7"}')
            (root / "src-tauri/Cargo.toml").write_text('[package]\nname = "aperture"\nversion = "9.8.7"\n')
            (root / "src-tauri/Cargo.lock").write_text('[[package]]\nname = "aperture"\nversion = "9.8.7"\n')
            (root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n")
            git = ["git", "-C", temporary, "-c", "user.name=Test", "-c", "user.email=test@example.com"]
            subprocess.run([*git, "init", "-q"], check=True)
            subprocess.run([*git, "add", "."], check=True)
            subprocess.run([*git, "commit", "-qm", "Source"], check=True)
            head = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
            with contextlib.chdir(root):
                identity = state.source_identity(head)
            self.assertEqual(identity["version"], "9.8.7")
            self.assertEqual(identity["lockfiles"]["pnpm-lock.yaml"], state.digest(root / "pnpm-lock.yaml"))

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

    def test_trusted_source_command_prints_only_verified_outputs(self):
        self.assertEqual(trusted_preflight(SOURCE, "", "", [main_ref(), comparison()]),
                         f"revision={SOURCE}\nrun_id=\n")
        with self.assertRaises(ValueError):
            trusted_preflight(SOURCE, "", "", [main_ref(), comparison("diverged")])
        self.assertEqual(trusted_preflight.output, "")

    def test_trusted_source_command_emits_only_fixed_public_fields(self):
        # Stdout becomes GITHUB_OUTPUT: it carries the verified arguments, never a verifier result.
        extra = {"revision": "d" * 40, "run_id": "99", "token": "sentinel"}
        for run_workflow, run_id in (("", ""), ("validate.yml", "17")):
            argv = ["release-state.py", "trusted-source", SOURCE, "--run-workflow", run_workflow, "--run-id", run_id]
            output = io.StringIO()
            with self.subTest(run_workflow=run_workflow), patch("sys.argv", argv), \
                    patch.object(state, "trusted_inputs", return_value=extra) as verify, contextlib.redirect_stdout(output):
                state.main()
            verify.assert_called_once_with(SOURCE, run_workflow, run_id)
            self.assertEqual(output.getvalue(), f"revision={SOURCE}\nrun_id={run_id}\n")
        for source_sha, run_id in ((SOURCE, "017"), (SOURCE, "17\ntoken=x"), (SOURCE, " 17"), ("main", ""),
                                   (SOURCE + "\ntoken=x", ""), (None, "")):
            with self.subTest(source_sha=source_sha, run_id=run_id), self.assertRaises(ValueError):
                state.gate_outputs(source_sha, run_id)

    def test_trusted_preflight_verifies_referenced_run_from_calling_revision(self):
        accepted = [main_ref(), comparison(), json.dumps(run_row()), main_ref(), comparison(base=WORKFLOW)]
        self.assertEqual(trusted_preflight(SOURCE, "validate.yml", "17", accepted), f"revision={SOURCE}\nrun_id=17\n")
        self.assertEqual(trusted_preflight.calls[2], ("api", f"repos/{state.REPO}/actions/runs/17"))
        self.assertEqual(trusted_preflight.calls[4], ("api", f"repos/{state.REPO}/compare/{WORKFLOW}...{MAIN}"))

    def test_trusted_preflight_rejects_unmerged_run_that_historical_source_helper_accepts(self):
        # A tag named main at an unmerged commit reports head_branch "main" for its successful run.
        row = run_row()
        historical = [json.dumps(row), comparison()]
        # An old source_sha checkout's helper checks only the run fields and the source commit.
        self.assertEqual(historical_trusted_run(Mock(side_effect=historical), "17", "validate.yml", SOURCE), row)
        diverged = comparison("diverged", base=WORKFLOW, merge_base="d" * 40)
        with self.assertRaisesRegex(ValueError, "contained in main"):
            trusted_preflight(SOURCE, "validate.yml", "17", [main_ref(), comparison(), json.dumps(row), main_ref(), diverged])
        self.assertEqual(trusted_preflight.output, "")

    def test_trusted_preflight_rejects_untrusted_referenced_runs(self):
        cases = {
            "other workflow": run_row(path=".github/workflows/package.yml"),
            "tag or branch other than main": run_row(head_branch="release"),
            "not dispatched": run_row(event="push"),
            "still running": run_row(status="in_progress"),
            "failed": run_row(conclusion="failure"),
            "fork repository": run_row(repository={"full_name": "fork/neuroqp-aperture"}),
            "missing head revision": run_row(head_sha=None),
        }
        for name, row in cases.items():
            with self.subTest(name), self.assertRaises(ValueError):
                trusted_preflight(SOURCE, "validate.yml", "17", [main_ref(), comparison(), json.dumps(row)])
            self.assertEqual(trusted_preflight.output, "")

    def test_trusted_preflight_rejects_producer_whose_workflow_predates_the_gate(self):
        # A tag named main at historical main 8fafb1f dispatches its Validate without the gate. Its
        # metadata passed every earlier check: main head_branch, success and a revision in main.
        historical = "8fafb1fd42384355b6f220c15566c12be814919e"
        row = run_row(head_sha=historical, referenced_workflows=[])
        with self.assertRaisesRegex(ValueError, "trusted input gate"):
            trusted_preflight(SOURCE, "validate.yml", "17", [main_ref(), comparison(), json.dumps(row)])
        self.assertEqual(len(trusted_preflight.calls), 3)
        self.assertEqual(trusted_preflight.output, "")
        # Only the service-recorded gate call distinguishes a producer that verified its own inputs.
        row["referenced_workflows"] = [gate_call(historical)]
        accepted = [main_ref(), comparison(), json.dumps(row), main_ref(), comparison(base=historical)]
        self.assertEqual(trusted_preflight(SOURCE, "validate.yml", "17", accepted), f"revision={SOURCE}\nrun_id=17\n")

    def test_trusted_preflight_requires_recorded_gate_call_at_producer_revision(self):
        other = "d" * 40
        cases = {
            "missing record": {key: value for key, value in run_row().items() if key != "referenced_workflows"},
            "null record": run_row(referenced_workflows=None),
            "object record": run_row(referenced_workflows=gate_call()),
            "string record": run_row(referenced_workflows=gate_call()["path"]),
            "malformed entries": run_row(referenced_workflows=[None, gate_call()["path"], []]),
            "earlier gate generation": run_row(referenced_workflows=[gate_call(gate=".github/workflows/trusted-source.yml")]),
            "other reusable workflow": run_row(referenced_workflows=[gate_call(gate=".github/workflows/check.yml")]),
            "gate from a fork": run_row(referenced_workflows=[gate_call(repo="fork/neuroqp-aperture")]),
            "gate at another revision": run_row(referenced_workflows=[gate_call(other)]),
            "gate commit differs": run_row(referenced_workflows=[gate_call(sha=other)]),
            "gate by moving ref": run_row(referenced_workflows=[{**gate_call(), "path": gate_call()["path"].split("@")[0]
                                                                 + "@refs/heads/main"}]),
            # Mocked: the tag-dispatch ref value is not observed live; any non-main ref is rejected.
            "tag dispatch": run_row(referenced_workflows=[gate_call(ref="refs/tags/main")]),
            "missing ref": run_row(referenced_workflows=[{"path": gate_call()["path"], "sha": WORKFLOW}]),
            "missing commit": run_row(referenced_workflows=[{"path": gate_call()["path"], "ref": "refs/heads/main"}]),
            "case variant": run_row(referenced_workflows=[gate_call(gate=state.TRUST_GATE.upper())]),
        }
        for name, row in cases.items():
            with self.subTest(name), self.assertRaisesRegex(ValueError, "trusted input gate"):
                trusted_preflight(SOURCE, "validate.yml", "17", [main_ref(), comparison(), json.dumps(row)])
            self.assertEqual(len(trusted_preflight.calls), 3)
            self.assertEqual(trusted_preflight.output, "")

    def test_trusted_preflight_accepts_gated_producer_among_other_reusable_calls(self):
        calls = [gate_call(gate=".github/workflows/check.yml"), {**gate_call(), "extra": True}]
        row = run_row(path=".github/workflows/package.yml", referenced_workflows=calls)
        accepted = [main_ref(), comparison(), json.dumps(row), main_ref(), comparison(base=WORKFLOW)]
        self.assertEqual(trusted_preflight(SOURCE, "package.yml", "17", accepted), f"revision={SOURCE}\nrun_id=17\n")

    def test_trusted_preflight_requires_expected_run_ids(self):
        # A required run never becomes optional because its ID is empty or malformed.
        for workflow in ("validate.yml", "package.yml"):
            for run_id in ("", "0", "017", " 17", "17\n", "17 ", "-1", "abc", "1e3"):
                with self.subTest(workflow=workflow, run_id=run_id), self.assertRaises(ValueError):
                    trusted_preflight(SOURCE, workflow, run_id, [main_ref(), comparison()])
                self.assertEqual(len(trusted_preflight.calls), 2)
                self.assertEqual(trusted_preflight.output, "")
        with self.assertRaisesRegex(ValueError, "without its expected workflow"):
            trusted_preflight(SOURCE, "", "17", [main_ref(), comparison()])
        for argv in (["--run-workflow", "check.yml", "--run-id", "17"], ["--run-id", "17"], ["--run-workflow", "validate.yml"]):
            with self.subTest(argv=argv), patch("sys.argv", ["release-state.py", "trusted-source", SOURCE, *argv]), \
                    patch.object(state, "gh") as github, contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    state.main()
                github.assert_not_called()

    def test_dispatched_source_is_checked_out_only_after_main_containment(self):
        guard = "needs.trust.outputs.revision == inputs.source_sha"
        trusted = (WORKFLOWS.parents[1] / state.TRUST_GATE).read_text(encoding="utf-8")
        # The gate runs only the calling workflow revision's code and writes no cache.
        self.assertNotRegex(trusted, r"\bref:")
        self.assertEqual([action.split("@")[0] for action in re.findall(r"uses: (\S+)", trusted)], ["actions/checkout"])
        self.assertIn('trusted-source "$SOURCE_SHA" --run-workflow "$RUN_WORKFLOW" --run-id "$RUN_ID"', trusted)
        for name in ("validate.yml", "package.yml", "publish.yml"):
            jobs = workflow_jobs(name)
            self.assertIn(f"uses: ./{state.TRUST_GATE}\n", jobs["trust"])
            guarded = {job for job, body in jobs.items() if guard in job_condition(body)}
            for job, body in jobs.items():
                if job == "trust" or "actions/checkout@" not in body:
                    continue
                with self.subTest(workflow=name, job=job):
                    needs = re.search(r"^    needs: \[?([^\]\n]*)", body, re.M)
                    needed = {item.strip() for item in needs.group(1).split(",")} if needs else set()
                    self.assertIn("trust", needed)
                    self.assertTrue(job in guarded or needed & guarded)
                    # Each checkout is the verified source or this dispatched main workflow revision.
                    for step in re.findall(r"(?ms)^      - uses: actions/checkout@.*?(?=^      - |\Z)", body):
                        self.assertRegex(step, r"\n          ref: \$\{\{ (needs\.trust\.outputs\.revision|github\.sha) \}\}\n")
                    self.assertNotRegex(body, r"ref: \$\{\{ *(github\.event\.)?inputs\.")

    def test_publish_runs_main_rules_against_the_exact_source_checkout(self):
        body = workflow_jobs("publish.yml")["publish"]
        self.assertIn("github.ref == 'refs/heads/main'", job_condition(body))
        checkouts = re.findall(r"(?ms)^      - uses: actions/checkout@.*?(?=^      - |\Z)", body)
        self.assertEqual(len(checkouts), 2)
        self.assertIn("ref: ${{ needs.trust.outputs.revision }}", checkouts[0])
        self.assertNotIn("path:", checkouts[0])
        self.assertIn("ref: ${{ github.sha }}", checkouts[1])
        self.assertIn("path: .publish-policy", checkouts[1])
        self.assertIn("python3 .publish-policy/scripts/release/release-state.py publish ", body)
        self.assertIn('if [ "$ACCEPT_UNVERIFIED" = true ]; then accepted=(--accept-unverified); fi', body)
        self.assertIn("ACCEPT_UNVERIFIED: ${{ inputs.accept_unverified_targets }}", body)
        self.assertIn("      accept_unverified_targets:\n        description: Accept OS compatibility risk for smoke targets reported unverified\n"
                      "        type: boolean\n        default: false\n", (WORKFLOWS / "publish.yml").read_text(encoding="utf-8"))

    def test_referenced_runs_are_consumed_only_after_trusted_verification(self):
        expected = {"validate.yml": ("build_run_id", "${{ inputs.mode == 'seal' && 'validate.yml' || '' }}",
                                     "${{ inputs.mode == 'seal' && inputs.build_run_id || '' }}"),
                    "package.yml": ("validation_run_id", "validate.yml", "${{ inputs.validation_run_id }}"),
                    "publish.yml": ("package_run_id", "package.yml", "${{ inputs.package_run_id }}")}
        for name, (input_name, run_workflow, run_id) in expected.items():
            jobs = workflow_jobs(name)
            guard = f"needs.trust.outputs.run_id != '' && needs.trust.outputs.run_id == inputs.{input_name}"
            with self.subTest(workflow=name):
                self.assertIn(f"run_workflow: {run_workflow}\n", jobs["trust"])
                self.assertIn(f"run_id: {run_id}\n", jobs["trust"])
            consumers = 0
            for job, body in jobs.items():
                if job == "trust":
                    continue
                with self.subTest(workflow=name, job=job):
                    # Source-tree helpers no longer decide whether a referenced run is trusted.
                    self.assertNotIn("trusted-run", body)
                    self.assertNotIn("inputs." + input_name, body.replace(guard, ""))
                    run_ids = re.findall(r"run-id: (.*)", body)
                    self.assertEqual(run_ids, ["${{ needs.trust.outputs.run_id }}"] * len(run_ids))
                    if run_ids:
                        consumers += 1
                        needs = re.search(r"^    needs: \[?([^\]\n]*)", body, re.M)
                        needed = {item.strip() for item in needs.group(1).split(",")}
                        self.assertTrue(guard in job_condition(body)
                                        or any(guard in job_condition(jobs[other]) for other in needed))
            self.assertGreater(consumers, 0)

if __name__ == "__main__":
    unittest.main()
