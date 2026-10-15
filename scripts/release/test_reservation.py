"""Only proven zero-build failures may recover an existing reservation."""

import copy
import hashlib
import os
from pathlib import Path
import unittest
from unittest.mock import patch

import reservation

TAG, SOURCE = "v0.2.0", "a" * 40


class ReservationTests(unittest.TestCase):
    def setUp(self):
        self.draft = {"id": 42, "tag_name": TAG, "target_commitish": SOURCE, "draft": True,
                      "prerelease": True, "assets": [], "body": "Frozen attempt: 1/hash. Stage B run: 7. Retain this audit."}
        self.run = {"id": 7, "repository": {"full_name": reservation.REPOSITORY},
                    "path": ".github/workflows/release.yml", "event": "workflow_dispatch", "head_branch": "main",
                    "run_attempt": 1, "status": "completed", "conclusion": "failure"}
        self.jobs = [{"name": name, "status": "completed", "conclusion": conclusion} for name, conclusion in
                     {"identity": "failure", "source": "skipped", "packages": "skipped", "candidate": "skipped"}.items()]
        self.environment = {"GITHUB_REPOSITORY": reservation.REPOSITORY, "GITHUB_REF": "refs/heads/main",
                            "GITHUB_RUN_ATTEMPT": "1", "GITHUB_RUN_ID": "8", "GITHUB_SHA": "b" * 40,
                            "FROZEN_RUN_ID": "9", "FROZEN_SHA256": "c" * 64}

    def origin(self, draft=None, run=None, jobs=None):
        with patch.object(reservation, "api", side_effect=[run or self.run, [{"jobs": jobs or self.jobs}]]):
            return reservation.unclaimed_origin(draft or self.draft, TAG, SOURCE)

    def unpublished(self, publish_steps=None, check_only=True, frozen=None):
        body = self.draft['body'] + ' Package build claim: Stage B run 8; orchestration ' + 'b' * 40 + '. Frozen attempt: 9/' + 'c' * 64 + '.'
        draft = {**self.draft, 'body': body}
        jobs = [{'name': name, 'status': 'completed', 'conclusion': conclusion, 'steps': publish_steps or [] if name == 'candidate' else []}
                for name, conclusion in {'identity': 'success', 'source': 'success', 'packages / linux': 'failure', 'packages / windows': 'cancelled', 'candidate': 'cancelled'}.items()]
        claim = {**self.run, 'id': 8, 'head_sha': 'b' * 40, 'conclusion': 'cancelled'}
        recovery = {'schema': 2, 'tag': TAG, 'source_sha': SOURCE, 'reservation_id': 42,
                    'body_sha256': hashlib.sha256(body.encode()).hexdigest(), 'origin_run_id': 7,
                    'invalidated_frozen_sha256': 'c' * 64,
                    'claim_runs': [{'run_id': 8, 'orchestration_sha': 'b' * 40, 'conclusion': 'cancelled', 'jobs': {j['name']: j['conclusion'] for j in jobs}}]}
        responses = [self.run, [{'jobs': self.jobs}], claim, [{'jobs': jobs}]]
        with patch.object(reservation, 'api', side_effect=responses), patch.object(Path, 'read_bytes', return_value=frozen or b'{}'):
            return reservation.reviewed_unpublished_resume(draft, TAG, SOURCE, recovery, check_only)

    def test_reviewed_unpublished_preflight_retains_failed_inputs_without_mutation(self):
        result = self.unpublished()
        self.assertEqual(result['invalidated_frozen_sha256'], 'c' * 64)
        self.assertEqual(result['stopped_claims'][0]['run']['id'], 8)

    def test_unpublished_resume_blocks_started_publication_and_unprepared_freeze(self):
        with self.assertRaisesRegex(ValueError, 'publication'):
            self.unpublished(publish_steps=[{'name': 'upload assets', 'conclusion': 'cancelled'}])
        with patch.dict(os.environ, {'FROZEN_SHA256': hashlib.sha256(b'{}').hexdigest()}), self.assertRaisesRegex(ValueError, 'prepared-input freeze'):
            self.unpublished(check_only=False)

    def test_identity_only_origin_and_check_only_do_not_mutate(self):
        self.assertEqual(self.origin()["original_body"], self.draft["body"])
        responses = [[[self.draft]], self.draft, self.run, [{"jobs": self.jobs}]]
        with patch.object(reservation.subprocess, "check_output", return_value=SOURCE + "\n"), \
                patch.object(reservation, "api", side_effect=responses) as api:
            result = reservation.reserve(TAG, SOURCE, check_only=True)
            self.assertEqual((result["status"], result["mutated"], result["reservation_id"]), ("eligible", False, 42))
            self.assertTrue(all(call.kwargs.get("method", "GET") == "GET" and len(call.args) == 1 for call in api.call_args_list))

    def test_assets_claim_and_wrong_destination_are_permanent_blocks(self):
        for field, value in [("assets", [{"id": 1}]), ("body", self.draft["body"] + " Package build claim: 7"),
                             ("target_commitish", "d" * 40), ("tag_name", "v0.3.0"), ("draft", False), ("prerelease", False)]:
            with self.subTest(field=field), patch.object(reservation, "api") as api:
                with self.assertRaises(ValueError):
                    reservation.unclaimed_origin({**self.draft, field: value}, TAG, SOURCE)
                api.assert_not_called()

    def test_wrong_origin_or_any_started_nonidentity_job_blocks(self):
        for field, value in [("repository", {"full_name": "other/repo"}), ("path", ".github/workflows/check.yml"),
                             ("event", "push"), ("head_branch", "feature"), ("run_attempt", 2),
                             ("status", "in_progress"), ("conclusion", "success")]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.origin(run={**self.run, field: value})
        for name in ["identity", "source", "packages", "candidate"]:
            jobs = copy.deepcopy(self.jobs)
            next(job for job in jobs if job["name"] == name)["conclusion"] = "success"
            with self.subTest(job=name), self.assertRaises(ValueError):
                self.origin(jobs=jobs)
        with self.assertRaises(ValueError):
            self.origin(jobs=self.jobs[:-1])

    def test_create_uses_post_response_id_without_collection_reread(self):
        calls = []
        def api(endpoint, method="GET", body=None, pages=False):
            calls.append((endpoint, method))
            if method == "GET":
                self.assertEqual(len(calls), 1)
                return [[]]
            self.assertEqual(method, "POST")
            self.assertIn("Package build claim", body["body"])
            return {**self.draft, **body, "id": 123}
        with patch.dict(os.environ, self.environment), \
                patch.object(reservation.subprocess, "check_output", return_value=SOURCE), patch.object(reservation, "api", side_effect=api):
            result = reservation.reserve(TAG, SOURCE)
            self.assertEqual((result["status"], result["reservation_id"]), ("claimed", 123))
        self.assertEqual([method for endpoint, method in calls], ["GET", "POST"])

    def test_recovery_preserves_body_and_claims_before_return(self):
        writes = []
        def api(endpoint, method="GET", body=None, pages=False):
            if method == "PATCH":
                writes.append(body)
                self.assertTrue(body["body"].startswith(self.draft["body"] + "\n\n"))
                self.assertEqual({key: body[key] for key in ["tag_name", "target_commitish", "draft", "prerelease", "make_latest"]},
                                 {"tag_name": TAG, "target_commitish": SOURCE, "draft": True, "prerelease": True, "make_latest": "false"})
                return {**self.draft, **body}
            if endpoint.endswith("/releases?per_page=100"):
                return [[self.draft]]
            if endpoint.endswith("/releases/42"):
                return {**self.draft, **writes[-1]} if writes else self.draft
            if endpoint.endswith("/actions/runs/7"):
                return self.run
            return [{"jobs": self.jobs}]
        with patch.dict(os.environ, self.environment), \
                patch.object(reservation.subprocess, "check_output", return_value=SOURCE), patch.object(reservation, "api", side_effect=api):
            result = reservation.reserve(TAG, SOURCE)
            self.assertEqual(len(writes), 1)
            self.assertEqual(result["origin"]["original_body"], self.draft["body"])
            self.assertIn("Stage B run 8", result["claim"])
            with self.assertRaises(ValueError):
                reservation.unclaimed_origin({**self.draft, **writes[0]}, TAG, SOURCE)

    def test_reviewed_alias_requires_both_failed_identity_runs_and_exact_body(self):
        body = self.draft["body"] + "\n\nPackage build claim: Stage B run 8; orchestration " + "b" * 40 + "."
        draft = {**self.draft, "tag_name": "untagged-test", "body": body}
        recovery = {"schema": 1, "tag": TAG, "source_sha": SOURCE, "reservation_id": 42, "alias": "untagged-test",
                    "body_sha256": hashlib.sha256(body.encode()).hexdigest(), "origin_run_id": 7,
                    "claim_run_id": 8, "claim_orchestration_sha": "b" * 40}
        claim_run = {**self.run, "id": 8, "head_sha": "b" * 40}
        responses = [self.run, [{"jobs": self.jobs}], claim_run, [{"jobs": self.jobs}]]
        with patch.object(reservation, "api", side_effect=responses):
            proof = reservation.unclaimed_origin(draft, TAG, SOURCE, recovery)
            self.assertEqual(proof["claim_origin"]["run"]["id"], 8)
        for field, value in [("tag_name", "untagged-other"), ("id", 43), ("body", body + " extra claim")]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                reservation.unclaimed_origin({**draft, field: value}, TAG, SOURCE, recovery)
        started = copy.deepcopy(self.jobs)
        next(job for job in started if job["name"] == "packages")["conclusion"] = "failure"
        with patch.object(reservation, "api", side_effect=responses[:3] + [[{"jobs": started}]]), self.assertRaises(ValueError):
            reservation.unclaimed_origin(draft, TAG, SOURCE, recovery)

    def test_reviewed_recovery_cannot_fall_through_to_new_or_different_draft(self):
        recovery = {"tag": TAG, "source_sha": SOURCE, "reservation_id": 42}
        for pages in [[[]], [[{**self.draft, "id": 99}]]]:
            with patch.object(reservation.subprocess, "check_output", return_value=SOURCE), \
                    patch.object(reservation, "api", return_value=pages) as api, self.assertRaises(ValueError):
                reservation.reserve(TAG, SOURCE, recovery=recovery)
            self.assertEqual(api.call_count, 1)


if __name__ == "__main__":
    unittest.main()
