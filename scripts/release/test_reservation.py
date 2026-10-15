"""Only proven zero-build failures may recover an existing reservation."""

import copy
import os
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
                return {**self.draft, **body}
            if endpoint.endswith("/releases?per_page=100"):
                return [[self.draft]]
            if endpoint.endswith("/releases/42"):
                return self.draft
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


if __name__ == "__main__":
    unittest.main()
