"""Completion must preserve the stopped attempt and every existing asset."""

import copy
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import bounded
import resume_publication as resume
from release import ASSETS, SOURCE_ASSET, SOURCE_REVIEW, digest


class CompletionTests(unittest.TestCase):
    def setUp(self):
        self.record = bounded.read(resume.RECORD)
        self.draft = {"id": self.record["release_id"], "tag_name": self.record["tag"],
                      "target_commitish": self.record["source_sha"], "draft": True,
                      "prerelease": True, "published_at": None, "body": "fixture",
                      "assets": copy.deepcopy(self.record["assets"])}
        import hashlib
        self.record["body_sha256"] = hashlib.sha256(b"fixture").hexdigest()
        resume.FROZEN = None

    def test_exact_assets_only(self):
        resume.validate_draft(self.draft, self.record)
        for change in ("digest", "id", "missing", "unexpected", "source"):
            draft = copy.deepcopy(self.draft)
            if change in ("digest", "id"):
                draft["assets"][0][change] = "changed"
            elif change == "missing":
                draft["assets"].pop()
            elif change == "unexpected":
                draft["assets"].append({"id": 12, "name": "extra", "size": 1, "digest": "sha256:" + "1" * 64})
            else:
                draft["target_commitish"] = "f" * 40
            with self.subTest(change=change), self.assertRaises(ValueError):
                resume.validate_draft(draft, self.record)

    def test_completed_cancelled_run_requires_successful_package_jobs(self):
        record = self.record
        run = {"id": record["prior_run_id"], "repository": {"full_name": resume.REPOSITORY}, "path": ".github/workflows/release.yml", "event": "workflow_dispatch", "head_branch": "main", "head_sha": record["prior_orchestration_sha"], "run_attempt": 1, "status": "completed", "conclusion": "cancelled"}
        jobs = [{"name": name, "status": "completed", "conclusion": result} for name, result in {"identity": "success", "source": "success", "packages / linux": "success", "packages / windows": "success", "candidate": "cancelled"}.items()]
        jobs[-1]["steps"] = [{"name": "Assemble and verify candidate", "conclusion": "success"}, {"name": "Publish candidate outside latest", "conclusion": "cancelled"}]
        resume.validate_prior(run, jobs, record)
        for changed in ({**run, "conclusion": "success"}, {**run, "status": "in_progress"}, {**run, "head_sha": "f" * 40}):
            with self.assertRaises(ValueError):
                resume.validate_prior(changed, jobs, record)
        jobs[2]["conclusion"] = "failure"
        with self.assertRaises(ValueError):
            resume.validate_prior(run, jobs, record)

    def test_complete_uploads_only_missing_source_and_retains_old_receipts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = copy.deepcopy(self.record)
            record["assets"] = []
            files = {**{name: b"package" for name in ASSETS.values()}, "SHA256SUMS": b"checksums", SOURCE_REVIEW: b"original plan"}
            for index, (name, content) in enumerate(files.items(), 1):
                path = root / name; path.write_bytes(content)
                record["assets"].append({"id": index, "name": name, "size": len(content), "digest": "sha256:" + digest(path)})
            archive = root / SOURCE_ASSET; archive.write_bytes(b"source")
            record["source_archive_sha256"] = digest(archive)
            record["checksums_sha256"] = digest(root / "SHA256SUMS")
            record["source_review_sha256"] = digest(root / SOURCE_REVIEW)
            record_path = root / "record.json"; bounded.write_report(record_path, record)
            old = {"schema": 1, "source_sha": record["source_sha"], "build_inputs_sha256": record["build_inputs_sha256"], "freeze_started_at": record["prior_freeze_started_at"], "deadline_at": record["prior_deadline_at"], "readiness": {"original": True}}
            original = copy.deepcopy(old)
            historical = {**old, "checksums_sha256": record["checksums_sha256"], "source_review_sha256": record["source_review_sha256"]}
            env = {"GITHUB_REPOSITORY": resume.REPOSITORY, "GITHUB_REF": "refs/heads/main", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_RUN_ID": "123", "GITHUB_SHA": "a" * 40, "READINESS_RUN_ID": "456"}
            run = {"id": 123, "head_sha": "a" * 40, "head_branch": "main", "path": ".github/workflows/resume-release.yml", "event": "workflow_dispatch", "repository": {"full_name": resume.REPOSITORY}, "run_attempt": 1}
            draft = {**self.draft, "assets": record["assets"]}
            added = {"id": 99, "name": SOURCE_ASSET, "size": archive.stat().st_size, "digest": "sha256:" + digest(archive)}
            after_upload = {**draft, "assets": record["assets"] + [added]}
            after_publish = {**after_upload, "draft": False}
            api = [run, draft, after_upload, after_publish]
            calls = []
            def gh(*args, **kwargs):
                calls.append(args)
                if args[:2] == ("release", "download"):
                    directory = Path(args[args.index("--dir") + 1])
                    chosen = {SOURCE_ASSET: archive.read_bytes()} if args[2] == record["source_release_tag"] else files
                    for name, body in chosen.items(): (directory / name).write_bytes(body)
                return ""
            def candidate(frozen, directory):
                return {**frozen, "checksums_sha256": record["checksums_sha256"], "source_review_sha256": record["source_review_sha256"], "assets": {environment: {"name": ASSETS["windows-x86_64" if environment == "windows11-setup" else "windows-x86_64-portable" if environment == "windows11-portable" else "linux-x86_64"], "sha256": digest(directory / next(iter(ASSETS.values())))} for environment in bounded.ENVIRONMENTS}}
            plan = {"sha256": record["source_archive_sha256"], "url": f"https://github.com/{resume.REPOSITORY}/releases/download/{record['source_release_tag']}/{SOURCE_ASSET}"}
            with patch.dict(os.environ, env), patch.object(resume, "RECORD", record_path), patch.object(resume, "api", side_effect=api), patch.object(resume, "preflight", return_value=(old, historical, {"fresh": True})), patch.object(resume, "gh", side_effect=gh), patch.object(resume, "source_plan", return_value=plan), patch.object(resume, "candidate_identity", side_effect=candidate), patch.object(resume.subprocess, "run"):
                result = resume.complete(root / "input", root / "output", record)
            self.assertEqual(result["status"], "pass")
            self.assertEqual(old, original)
            frozen = bounded.read(root / "output/frozen-attempt.json")
            self.assertEqual((bounded.parsed(frozen["deadline_at"]) - bounded.parsed(frozen["freeze_started_at"])).total_seconds(), 3600)
            self.assertEqual(frozen["recovery"]["prior_deadline_at"], old["deadline_at"])
            uploads = [call for call in calls if call[:2] == ("release", "upload")]
            self.assertEqual(len(uploads), 1)
            self.assertEqual(uploads[0][3], str(root / "output/release" / SOURCE_ASSET))
            self.assertFalse(any("--clobber" in call for call in calls))

    def test_trusted_completion_lineage_rejects_changed_source_and_prior_receipts(self):
        record = bounded.read(resume.RECORD)
        run = {"id": 123, "head_sha": "a" * 40}
        start = bounded.timestamp()
        recovery = {key: record[key] for key in ("prior_run_id", "prior_orchestration_sha", "prior_conclusion", "prior_frozen_sha256", "prior_candidate_sha256", "prior_freeze_started_at", "prior_deadline_at", "release_id")}
        recovery.update(record_sha256=digest(resume.RECORD), completion_run_id="123", orchestration_sha=run["head_sha"])
        hashes = {row["name"]: row["digest"].removeprefix("sha256:") for row in record["assets"]}
        assets = {}
        for environment in bounded.ENVIRONMENTS:
            name = ASSETS["windows-x86_64" if environment == "windows11-setup" else "windows-x86_64-portable" if environment == "windows11-portable" else "linux-x86_64"]
            assets[environment] = {"name": name, "sha256": hashes[name]}
        candidate = {**record, "recovery": recovery, "freeze_started_at": start, "deadline_at": (bounded.parsed(start) + dt.timedelta(seconds=3600)).isoformat(), "assets": assets, "release_assets": record["assets"] + [{"id": 99, "name": SOURCE_ASSET, "digest": "sha256:" + record["source_archive_sha256"], "size": 1}]}
        resume.validate_lineage(candidate, run)
        for field, value in (("source_sha", "f" * 40), ("source_review_sha256", "f" * 64), ("checksums_sha256", "f" * 64), ("deadline_at", (bounded.parsed(start) + dt.timedelta(seconds=1800)).isoformat())):
            with self.subTest(field=field), self.assertRaises(ValueError):
                resume.validate_lineage({**candidate, field: value}, run)
        changed = copy.deepcopy(candidate)
        changed["recovery"]["prior_frozen_sha256"] = "f" * 64
        with self.assertRaises(ValueError):
            resume.validate_lineage(changed, run)
        changed = copy.deepcopy(candidate)
        changed["assets"]["debian12"]["sha256"] = "f" * 64
        with self.assertRaises(ValueError):
            resume.validate_lineage(changed, run)

    def test_actual_check_cli_rejects_changed_historical_evidence_before_api(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "prior").mkdir()
            (root / "prior/frozen-attempt.json").write_text("{}")
            output = root / "output"
            result = subprocess.run([sys.executable, resume.__file__, "check", str(root), "--output", str(output)], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            report = bounded.read(output / "completion-report.json")
            self.assertEqual(report["status"], "fail")
            self.assertIn("Historical frozen evidence bytes changed", report["error"])

    def test_deadline_policy_accepts_unchanged_legacy_receipts(self):
        start = bounded.timestamp()
        for seconds in (1800, 3600):
            bounded.validate_deadline({"freeze_started_at": start, "deadline_at": (bounded.parsed(start) + dt.timedelta(seconds=seconds)).isoformat()})


if __name__ == "__main__":
    unittest.main()
