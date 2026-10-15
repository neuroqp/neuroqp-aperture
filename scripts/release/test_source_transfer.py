"""Transport trust boundaries and verification-before-upload ordering."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import release

spec = importlib.util.spec_from_file_location("source_transfer", Path(__file__).with_name("source-transfer.py"))
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)


class TransportTests(unittest.TestCase):
    def manifest(self):
        baseline = release.source_plan(release.ROOT / release.SOURCE_PUBLICATION)
        return {"schema": 1, "source_sha": "a" * 40, "source_release_tag": "source-test",
                "baseline_url": baseline["url"], "baseline_sha256": baseline["sha256"],
                "patch_asset": "source-transfer.zst", "patch_sha256": "b" * 64,
                "patch_bytes": 123, "target_sha256": "c" * 64, "target_bytes": 456,
                "source_review_sha256": "d" * 64}

    def test_manifest_rejects_untrusted_bytes_and_redirected_baseline(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            record = self.manifest()
            path.write_text(json.dumps(record))
            self.assertEqual(transport.validate_manifest(path, release.digest(path), release.ROOT), record)
            with self.assertRaisesRegex(ValueError, "reviewed SHA256"):
                transport.validate_manifest(path, "0" * 64, release.ROOT)
            for key, value in [("baseline_url", "https://example.invalid/archive"),
                               ("baseline_sha256", "0" * 64), ("source_release_tag", "v0.2.0"),
                               ("patch_asset", "../patch"), ("target_bytes", True)]:
                bad = {**record, key: value}
                path.write_text(json.dumps(bad))
                with self.assertRaises(ValueError):
                    transport.validate_manifest(path, release.digest(path), release.ROOT)

    def test_draft_refuses_public_or_existing_target(self):
        for record in [{"tag_name": "source-test", "draft": False, "assets": []},
                       {"tag_name": "source-test", "draft": True, "assets": [{"name": release.SOURCE_ASSET}]}]:
            with patch.object(transport, "command", return_value=json.dumps([record])):
                with self.assertRaises(ValueError):
                    transport.draft_release("source-test")

    def test_draft_lookup_selects_exact_unique_tag_from_authenticated_list(self):
        draft = {"id": 99, "tag_name": "source-test", "draft": True, "assets": []}
        other = {"id": 100, "tag_name": "source-test-other", "draft": True, "assets": []}
        with patch.object(transport, "command", return_value=json.dumps([other, draft])) as command:
            self.assertEqual(transport.draft_release("source-test"), draft)
            command.assert_called_once_with(["gh", "api", f"repos/{transport.REPOSITORY}/releases?per_page=100"])
        for records in [[other], [draft, draft]]:
            with patch.object(transport, "command", return_value=json.dumps(records)):
                with self.assertRaisesRegex(ValueError, "exactly one release"):
                    transport.draft_release("source-test")

    def test_failed_full_source_gate_never_uploads_and_success_verifies_github(self):
        manifest = self.manifest()
        plan = {"application_source_sha": manifest["source_sha"], "sha256": manifest["target_sha256"],
                "url": f"https://github.com/{transport.REPOSITORY}/releases/download/source-test/{release.SOURCE_ASSET}"}
        uploaded = {"id": 99, "tag_name": "source-test", "draft": True, "assets": [
            {"id": 101, "name": release.SOURCE_ASSET, "size": 456,
             "digest": "sha256:" + "c" * 64, "state": "uploaded"}]}
        for failure in [True, False]:
            calls = []
            def command(arguments, **kwargs):
                calls.append(arguments)
                return json.dumps(uploaded) if arguments[:2] == ["gh", "api"] else ""
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(transport, "command", side_effect=command), \
                    patch.object(transport, "draft_release", return_value={"id": 99}), \
                    patch.object(transport, "checked_file"), \
                    patch.object(release, "source_plan", return_value=plan), \
                    patch.object(release, "verify_application_locks") as locks, \
                    patch.object(release, "verify_source_material", side_effect=ValueError("coverage") if failure else None) as verify:
                receipt = {}
                output = Path(directory) / "output"
                if failure:
                    with self.assertRaisesRegex(ValueError, "coverage"):
                        transport.transfer(release.ROOT, manifest, output, receipt)
                else:
                    transport.transfer(release.ROOT, manifest, output, receipt)
                    self.assertEqual(receipt["status"], "passed")
                    self.assertEqual(receipt["uploaded_asset_id"], 101)
                uploads = [call for call in calls if call[:3] == ["gh", "release", "upload"]]
                self.assertEqual(len(uploads), 0 if failure else 1)
                self.assertFalse(any("--clobber" in call for call in calls))
                locks.assert_called_once_with(release.ROOT, plan)
                verify.assert_called_once_with(output, plan, root=release.ROOT, source_sha=manifest["source_sha"])

    def test_wrong_size_or_digest_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "patch"
            path.write_bytes(b"transport")
            transport.checked_file(path, release.digest(path), 9)
            for digest, size in [(release.digest(path), 8), ("0" * 64, 9)]:
                with self.assertRaises(ValueError):
                    transport.checked_file(path, digest, size)


if __name__ == "__main__":
    unittest.main()
