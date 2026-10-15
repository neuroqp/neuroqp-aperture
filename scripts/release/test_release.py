"""Checks version synchronization and fail-closed candidate verification."""

import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("release", Path(__file__).with_name("release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

spec = importlib.util.spec_from_file_location("notices", Path(__file__).with_name("notices.py"))
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


def helper_module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PreviousCodeFixtureTests(unittest.TestCase):
    def test_fixed_baseline_retains_version_production_key_and_signature_checks(self):
        fixture = helper_module("previous-upgrade-fixture")
        updater = {"pubkey": "production-test-key", "requireSignedVersion": True}
        with mock.patch.dict(release.os.environ, {}, clear=True):
            fixture.validate_baseline(fixture.PRIOR_SHA, "0.1.1", updater, updater["pubkey"])
            for sha, version, config, key, signed in (
                ("0" * 40, "0.1.1", updater, updater["pubkey"], False),
                (fixture.PRIOR_SHA, "0.0.9", updater, updater["pubkey"], False),
                (fixture.PRIOR_SHA, "0.1.1", {**updater, "requireSignedVersion": False}, updater["pubkey"], False),
                (fixture.PRIOR_SHA, "0.1.1", updater, "different-key", False),
                (fixture.PRIOR_SHA, "0.1.1", updater, updater["pubkey"], True),
            ):
                with self.assertRaises(ValueError):
                    fixture.validate_baseline(sha, version, config, key, signed)
            with mock.patch.dict(release.os.environ, {"TAURI_SIGNING_PRIVATE_KEY": "synthetic-test-only"}):
                with self.assertRaisesRegex(ValueError, "unsigned context"):
                    fixture.validate_baseline(fixture.PRIOR_SHA, "0.1.1", updater, updater["pubkey"])

    def test_windows_builder_pins_the_same_baseline_and_ordinary_source_gate(self):
        fixture = helper_module("previous-upgrade-fixture")
        builder = Path(__file__).with_name("build-previous-windows-upgrade-fixture.ps1").read_text()
        self.assertIn("refs/tags/" + fixture.PRIOR_TAG, builder)
        self.assertIn(fixture.PRIOR_SHA, builder)
        self.assertIn("'--fetch', '--platform', 'windows'", builder)
        self.assertNotIn("--upgrade-fixture", builder)
        self.assertNotIn("'version'", builder)
        self.assertIn("$env:TAURI_SIGNING_PRIVATE_KEY", builder)
        self.assertIn("$env:APERTURE_NATIVE_DIR = $oldNative", builder)
        self.assertIn("$env:PATH = $oldPath", builder)

    def test_linux_builder_pins_the_same_baseline_and_restores_original_bundle(self):
        fixture = helper_module("previous-upgrade-fixture")
        builder = Path(__file__).with_name("build-previous-linux-upgrade-fixture.sh").read_text()
        self.assertIn("refs/tags/" + fixture.PRIOR_TAG, builder)
        self.assertIn(fixture.PRIOR_SHA, builder)
        self.assertIn("--fetch --platform linux", builder)
        self.assertNotIn("--upgrade-fixture", builder)
        self.assertIn("TAURI_SIGNING_PRIVATE_KEY", builder)
        self.assertIn("trap restore EXIT", builder)
        self.assertIn('mv "$scratch/original" "$bundle"', builder)
        self.assertIn('cmp "$scratch/production-before.sha256" "$scratch/production-after.sha256"', builder)
        if release.os.name != "nt":
            path = str(Path(__file__).with_name("build-previous-linux-upgrade-fixture.sh"))
            release.subprocess.run(["bash", "-n", path], check=True)
            guarded = release.subprocess.run(["bash", path], env={**release.os.environ, "SIGNED": "true"}, capture_output=True, text=True)
            self.assertNotEqual(guarded.returncode, 0)
            self.assertIn("unsigned context", guarded.stderr)


class SourceRefreshTests(unittest.TestCase):
    def test_refresh_preserves_native_bytes_binds_current_locks_and_drops_unused_crates(self):
        refresh = helper_module("refresh-source")
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            files = {name: (b"unchanged native recipe\n", 0o644) for name in refresh.NATIVE_INPUTS}
            files.update({"package.json": (b'{"version":"0.1.1"}', 0o644),
                          "pnpm-lock.yaml": (b"unchanged npm lock\n", 0o644)})
            lock = '[[package]]\nname = "aperture"\nversion = "0.1.1"\n'
            def crate(name, checksum):
                return f'\n[[package]]\nname = "{name}"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "{checksum}"\n'
            lock += crate("kept", "1" * 64)
            files["src-tauri/Cargo.lock"] = ((lock + crate("removed", "2" * 64)).encode(), 0o644)
            old_locks = {name: refresh.sha(files[name][0]) for name in release.LOCKFILES}
            inventories = {
                release.SOURCE_INVENTORY: refresh.encoded({"schema": 1, "files": [{"path": name, "sha256": refresh.sha(body)} for name, (body, _) in files.items()]}),
                "application/cargo-vendor-inventory.json": refresh.encoded({"schema": 1, "cargoLockSha256": old_locks["src-tauri/Cargo.lock"], "crates": [
                    {"name": name, "version": "1.0.0", "directory": name, "packageChecksum": checksum * 64}
                    for name, checksum in (("kept", "1"), ("removed", "2"))]}),
                "application/npm-inventory.json": refresh.encoded({"lockfile_sha256": old_locks["pnpm-lock.yaml"]}),
            }
            identities = {"linux": "3" * 64, "windows": "4" * 64}
            members = {"SOURCE-MANIFEST.json": refresh.encoded({"schema": 1, "native_dependencies_sha256": identities,
                       "inventory_sha256": {name: refresh.sha(body) for name, body in inventories.items()}}),
                       **inventories, "linux/native-source.tar": b"exact reviewed native source bytes",
                       "windows/native-source.tar": b"exact reviewed Windows source bytes",
                       "application/cargo-vendor/kept/source.rs": b"kept crate",
                       "application/cargo-vendor/removed/source.rs": b"obsolete crate",
                       **{"application/source/" + name: body for name, (body, _) in files.items()}}
            archive = directory / release.SOURCE_ASSET
            with tarfile.open(archive, "w:xz") as target:
                for name, body in members.items():
                    entry = tarfile.TarInfo(name)
                    entry.size = len(body)
                    target.addfile(entry, io.BytesIO(body))
            original_hash = release.digest(archive)
            plan = {"schema": 1, "status": "ready", "asset": release.SOURCE_ASSET,
                    "url": "https://github.com/neuroqp/neuroqp-aperture/releases/download/source-v0.1.1/" + release.SOURCE_ASSET,
                    "sha256": original_hash, "native_dependencies_sha256": identities,
                    "application_lockfiles_sha256": old_locks,
                    "application_source_inventory_sha256": refresh.sha(inventories[release.SOURCE_INVENTORY])}
            review_path = directory / "prior-review.json"
            review_path.write_bytes(refresh.encoded(plan))
            renamed = directory / "unreviewed-other-name.tar.xz"
            renamed.write_bytes(b"not the reviewed archive")
            with self.assertRaisesRegex(ValueError, "fixed corresponding-source asset name"):
                refresh.prepare(renamed, review_path, directory, "5" * 40)
            files["package.json"] = (b'{"version":"0.2.0"}', 0o644)
            files["src-tauri/Cargo.lock"] = (lock.replace('version = "0.1.1"', 'version = "0.2.0"').encode(), 0o644)
            with mock.patch.object(refresh, "snapshot", return_value=files):
                generated, snapshot, removed, review = refresh.prepare(archive, review_path, directory, "5" * 40)
                output = directory / "refreshed" / release.SOURCE_ASSET
                output.parent.mkdir()
                result = refresh.write_archive(archive, output, generated, snapshot, removed)
                for preset in (-1, 10, "0"):
                    with self.assertRaisesRegex(ValueError, "XZ preset"):
                        refresh.write_archive(archive, output, generated, snapshot, removed, preset)
                fast = output.with_name("fast.tar.xz")
                fast_result = refresh.write_archive(archive, fast, generated, snapshot, removed, 0)
                self.assertEqual(fast_result["xz_preset"], 0)
                self.assertEqual(fast_result["members_verified"], result["members_verified"])
                self.assertEqual(review["status"], "assembled-awaiting-review")
                self.assertEqual(removed, {"removed"})
                self.assertEqual(release.digest(archive), original_hash)
                review["sha256"] = result["sha256"]
                release.verify_source_material(output.parent, review)
                self.assertEqual(review["dependency_source"], {"url": plan["url"], "sha256": plan["sha256"], "review_sha256": release.digest(review_path)})
                wrong_baseline = {**review, "dependency_source": {**review["dependency_source"], "sha256": "0" * 64}}
                with self.assertRaisesRegex(ValueError, "dependency source baseline"):
                    release.verify_source_material(output.parent, wrong_baseline)
                with tarfile.open(output, "r:xz") as source:
                    self.assertEqual(source.next().name, "SOURCE-MANIFEST.json")
                    self.assertEqual(source.extractfile("linux/native-source.tar").read(), members["linux/native-source.tar"])
                    self.assertNotIn("application/cargo-vendor/removed/source.rs", source.getnames())
                    self.assertEqual(source.extractfile("application/source/src-tauri/Cargo.lock").read(), files["src-tauri/Cargo.lock"][0])
                files["src-tauri/Cargo.lock"] = (files["src-tauri/Cargo.lock"][0] + crate("new", "6" * 64).encode(), 0o644)
                with self.assertRaisesRegex(ValueError, "New or changed Cargo"):
                    refresh.prepare(archive, review_path, directory, "5" * 40)


def reviewed_source(directory, root, version="0.1.0"):
    (root / "scripts/release").mkdir(parents=True)
    (root / "CHANGELOG.md").write_text((release.ROOT / "CHANGELOG.md").read_text(encoding="utf-8"), encoding="utf-8")
    (root / "src-tauri").mkdir()
    (root / "src-tauri/Cargo.lock").write_bytes(('[[package]]\nname = "aperture"\nversion = "' + version + '"\n\n[[package]]\nname = "dep"\nversion = "1.0.0"\n').encode())
    (root / "pnpm-lock.yaml").write_bytes(b"lockfileVersion: '9.0'\n")
    locks = release.lockfile_hashes(root)
    inventory = json.dumps({"schema": 1, "files": [{"path": name, "sha256": checksum} for name, checksum in locks.items()]}).encode()
    inventory_hash = release.hashlib.sha256(inventory).hexdigest()
    identities = {}
    for platform in ("linux", "windows"):
        path = directory / f"BUILDINFO-{platform}.json"
        info = json.loads(path.read_text(encoding="utf-8"))
        info["lockfiles"] = locks
        info["native_dependencies"] = {"libvips": "8.18.0" if platform == "linux" else "8.18.7"}
        path.write_text(json.dumps(info), encoding="utf-8")
        identities[platform] = release.native_identity(info["native_dependencies"])
    with tarfile.open(directory / release.SOURCE_ASSET, "w:xz") as source:
        for name, data in (("SOURCE-MANIFEST.json", json.dumps({"schema": 1, "native_dependencies_sha256": identities, "inventory_sha256": {release.SOURCE_INVENTORY: inventory_hash}}).encode()), (release.SOURCE_INVENTORY, inventory), *(("application/source/" + name, (root / name).read_bytes()) for name in locks), ("fixture/source.c", b"/* Synthetic source material for the gate test. */\n")):
            member = tarfile.TarInfo(name)
            member.size = len(data)
            source.addfile(member, io.BytesIO(data))
    plan = {"schema": 1, "status": "ready", "asset": release.SOURCE_ASSET, "url": "https://github.com/neuroqp/neuroqp-aperture/releases/download/source-fixture/Aperture-Corresponding-Source.tar.xz", "sha256": release.digest(directory / release.SOURCE_ASSET), "native_dependencies_sha256": identities, "application_lockfiles_sha256": locks, "application_source_inventory_sha256": inventory_hash}
    (root / "scripts/release/corresponding-source.json").write_text(json.dumps(plan), encoding="utf-8")
    release.source_material(directory, root=root)


def rewrite_archive(directory, plan, transform):
    archive = directory / release.SOURCE_ASSET
    with tarfile.open(archive, "r:xz") as source:
        rows = [(member.name, source.extractfile(member).read(), member.mode) for member in source]
    rows = transform(rows)
    with tarfile.open(archive, "w:xz") as source:
        for name, body, mode in rows:
            member = tarfile.TarInfo(name)
            member.size, member.mode = len(body), mode
            source.addfile(member, io.BytesIO(body))
    plan["sha256"] = release.digest(archive)


def frozen_source(directory, root):
    for platform in ("linux", "windows"):
        (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
    reviewed_source(directory, root)
    (root / "application.txt").write_bytes(b"Frozen application implementation.\n")
    release.subprocess.run(["git", "init", "-q", str(root)], check=True)
    release.subprocess.run(["git", "add", "."], cwd=root, check=True)
    release.subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "Tiny frozen source fixture"], cwd=root, check=True)
    source_sha = release.subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    files = release.application_snapshot(root, source_sha)
    inventory = json.dumps({"schema": 1, "gitBaseSha": source_sha, "trackedChanges": [], "includedUntracked": [],
                            "excludedPublicationRecord": release.SOURCE_PUBLICATION,
                            "workingTreePatchSha256": release.hashlib.sha256(b"").hexdigest(),
                            "files": [{"path": name, "sha256": release.hashlib.sha256(body).hexdigest(), "size": len(body), "mode": mode}
                                      for name, (body, mode) in sorted(files.items())]}).encode()
    review = root / release.SOURCE_PUBLICATION
    plan = json.loads(review.read_text())
    plan.update(application_source_sha=source_sha, application_source_inventory_sha256=release.hashlib.sha256(inventory).hexdigest())
    def replace(rows):
        record = json.loads(rows[0][1])
        record["inventory_sha256"][release.SOURCE_INVENTORY] = plan["application_source_inventory_sha256"]
        return [("SOURCE-MANIFEST.json", json.dumps(record).encode(), 0o644), (release.SOURCE_INVENTORY, inventory, 0o644),
                *(("application/source/" + name, body, mode) for name, (body, mode) in files.items()),
                ("fixture/source.c", b"preserved dependency source", 0o644)]
    rewrite_archive(directory, plan, replace)
    started = release.datetime.datetime.now(release.datetime.timezone.utc) - release.datetime.timedelta(seconds=60)
    stamp = lambda value: value.isoformat().replace("+00:00", "Z")
    attempt = {**release.bounded.identity(root), "stage": "B", "freeze_started_at": stamp(started),
               "deadline_at": stamp(started + release.datetime.timedelta(seconds=1800))}
    attempt_path = root.parent / "attempt.json"
    attempt_path.write_text(json.dumps(attempt))
    plan["assembly_attempt"] = {**attempt, "attempt_sha256": release.digest(attempt_path),
                                "archive_sha256": plan["sha256"], "refresh_receipt_sha256": "e" * 64,
                                "status": "pass", "forced_kill": False,
                                "started_at": stamp(started + release.datetime.timedelta(seconds=10)),
                                "finished_at": stamp(started + release.datetime.timedelta(seconds=11))}
    # Keep the tracked plan bytes untouched after freezing the commit.
    external = root.parent / "external-review.json"
    external.write_text(json.dumps(plan))
    for platform in ("linux", "windows"):
        path = directory / f"BUILDINFO-{platform}.json"
        info = json.loads(path.read_text())
        info["source_sha"] = source_sha
        path.write_text(json.dumps(info))
    return external, source_sha, plan


class FrozenSourceTests(unittest.TestCase):
    def test_external_source_consumers_require_exact_post_freeze_assembly_proof(self):
        with tempfile.TemporaryDirectory() as folder:
            directory, root = Path(folder) / "assets", Path(folder) / "root"
            directory.mkdir()
            external, source_sha, plan = frozen_source(directory, root)
            attempt_path = root.parent / "attempt.json"
            original = attempt_path.read_bytes()
            expected = json.loads(original)
            kwargs = {"root": root, "plan_path": external, "source_sha": source_sha, "attempt_path": attempt_path}
            external.write_text(json.dumps({key: value for key, value in plan.items() if key != "assembly_attempt"}))
            with self.assertRaises(ValueError):
                release.source_material(directory, plan_sha256=release.digest(external), **kwargs)
            mutations = [
                {"assembly_attempt": None},
                {"assembly_attempt": {}},
                *({"assembly_attempt": {**plan["assembly_attempt"], field: value}} for field, value in (
                    ("schema", 2), ("stage", "A"), ("source_sha", "f" * 40), ("build_inputs_sha256", "f" * 64),
                    ("freeze_started_at", "2000-01-01T00:00:00Z"), ("deadline_at", "2100-01-01T00:00:00Z"),
                    ("attempt_sha256", "f" * 64), ("archive_sha256", "f" * 64),
                    ("refresh_receipt_sha256", "not-a-hash"), ("status", "fail"), ("forced_kill", True),
                    ("started_at", "2000-01-01T00:00:00Z"), ("finished_at", expected["deadline_at"]),
                    ("finished_at", (release.datetime.datetime.now(release.datetime.timezone.utc) + release.datetime.timedelta(seconds=60)).isoformat().replace("+00:00", "Z")),
                    ("started_at", None), ("finished_at", "invalid"),
                )),
            ]
            for index, mutation in enumerate(mutations):
                external.write_text(json.dumps({**plan, **mutation}))
                kwargs["plan_sha256"] = release.digest(external)
                with self.subTest(mutation=index), mock.patch.object(release.urllib.request, "urlopen") as download:
                    with self.assertRaises(ValueError):
                        release.source_material(directory, fetch=True, **kwargs)
                    with self.assertRaises(ValueError):
                        release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", source_sha,
                                         "2026-10-14T00:00:00Z", **{key: value for key, value in kwargs.items() if key != "source_sha"})
                    download.assert_not_called()
                    self.assertFalse((directory / "latest.json").exists())
                    self.assertFalse((directory / "SHA256SUMS").exists())
            external.write_text(json.dumps(plan))
            kwargs["plan_sha256"] = release.digest(external)
            with self.assertRaisesRegex(ValueError, "original frozen --attempt"):
                release.source_material(directory, **{**kwargs, "attempt_path": None})
            # Equivalent JSON is still different original receipt bytes.
            attempt_path.write_bytes(original + b"\n")
            with self.assertRaisesRegex(ValueError, "receipt"):
                release.source_material(directory, **kwargs)
            attempt_path.write_bytes(original)
            with mock.patch.object(release.bounded, "timestamp", return_value=expected["deadline_at"]), self.assertRaisesRegex(ValueError, "deadline exceeded"):
                release.source_material(directory, **kwargs)
            attempt_path.write_text(json.dumps({**expected, "build_inputs_sha256": "f" * 64}))
            with self.assertRaisesRegex(ValueError, "frozen inputs"):
                release.source_material(directory, **kwargs)

    def test_external_plan_hash_is_checked_before_parse_or_network(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "external.json"
            path.write_bytes(b"this is not JSON")
            with mock.patch.object(release.urllib.request, "urlopen") as download:
                with self.assertRaisesRegex(ValueError, "trusted SHA-256"):
                    release.source_material(Path(folder) / "output", fetch=True, plan_path=path, plan_sha256="f" * 64, source_sha="a" * 40)
                download.assert_not_called()
            for plan_path, checksum, source_sha in ((path, None, "a" * 40), (None, "f" * 64, "a" * 40), (path, "f" * 64, None)):
                with self.assertRaises(ValueError):
                    release.reviewed_plan(Path(folder), plan_path, checksum, source_sha)

    def test_frozen_external_plan_transfers_existing_bytes_and_preserves_tracked_plan(self):
        with tempfile.TemporaryDirectory() as folder:
            directory, root = Path(folder) / "assets", Path(folder) / "root"
            directory.mkdir()
            external, source_sha, plan = frozen_source(directory, root)
            tracked = (root / release.SOURCE_PUBLICATION).read_bytes()
            with mock.patch.object(release.urllib.request, "urlopen") as download:
                release.source_material(directory, fetch=True, root=root, platform="linux", plan_path=external,
                                        plan_sha256=release.digest(external), source_sha=source_sha, attempt_path=root.parent / "attempt.json")
                download.assert_not_called()
            self.assertEqual((root / release.SOURCE_PUBLICATION).read_bytes(), tracked)
            self.assertEqual((directory / release.SOURCE_REVIEW).read_bytes(), external.read_bytes())
            release.verify_source_material(directory, plan, root=root, source_sha=source_sha, builds=True)
            wrong = {**plan, "application_source_sha": "f" * 40}
            external.write_text(json.dumps(wrong))
            with self.assertRaisesRegex(ValueError, "pin the frozen"):
                release.source_material(directory, root=root, plan_path=external, plan_sha256=release.digest(external), source_sha=source_sha, attempt_path=root.parent / "attempt.json")
            path = directory / "BUILDINFO-linux.json"
            info = json.loads(path.read_text())
            info["source_sha"] = "f" * 40
            path.write_text(json.dumps(info))
            with self.assertRaisesRegex(ValueError, "linux frozen source SHA"):
                release.verify_source_material(directory, plan, root=root, source_sha=source_sha, platform="linux")

    def test_external_metadata_uses_frozen_review_without_tracked_plan_update(self):
        with tempfile.TemporaryDirectory() as folder:
            directory, root = Path(folder) / "assets", Path(folder) / "root"
            directory.mkdir()
            external, source_sha, plan = frozen_source(directory, root)
            tracked = (root / release.SOURCE_PUBLICATION).read_bytes()
            for name in release.ASSETS.values():
                (directory / name).write_bytes(b"synthetic binary")
                (directory / (name + ".sig")).write_text("synthetic signature")
            for platform in ("linux", "windows"):
                (directory / f"THIRD-PARTY-NOTICES-{platform}.txt").write_text("synthetic notice")
            release.source_material(directory, root=root, plan_path=external, plan_sha256=release.digest(external), source_sha=source_sha, attempt_path=root.parent / "attempt.json")
            release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", source_sha, "2026-10-14T00:00:00Z",
                             root=root, plan_path=external, plan_sha256=release.digest(external), attempt_path=root.parent / "attempt.json")
            release.verify(directory, "0.1.0", source_sha, root=root)
            self.assertEqual((root / release.SOURCE_PUBLICATION).read_bytes(), tracked)
            self.assertEqual(release.digest(directory / release.SOURCE_ASSET), plan["sha256"])
            copied = directory / release.SOURCE_REVIEW
            copied.write_bytes(copied.read_bytes() + b"\n")
            with self.assertRaisesRegex(ValueError, "Transferred source review"):
                release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", source_sha, "2026-10-14T00:00:00Z",
                                 root=root, plan_path=external, plan_sha256=release.digest(external), attempt_path=root.parent / "attempt.json")

    def test_snapshot_ignores_export_attributes_and_requires_commit_object(self):
        with tempfile.TemporaryDirectory() as folder:
            directory, root = Path(folder) / "assets", Path(folder) / "root"
            directory.mkdir()
            frozen_source(directory, root)
            (root / ".gitattributes").write_text("application.txt export-ignore\nsubstitute.txt export-subst\n")
            (root / "substitute.txt").write_bytes(b"$Format:%H$")
            release.subprocess.run(["git", "add", "."], cwd=root, check=True)
            release.subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "Fixture export attributes"], cwd=root, check=True)
            source_sha = release.subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
            with mock.patch.dict(os.environ, {"GIT_TEST_ASSUME_DIFFERENT_OWNER": "1"}):
                files = release.application_snapshot(root, source_sha)
            self.assertEqual(files["application.txt"][0], b"Frozen application implementation.\n")
            self.assertEqual(files["substitute.txt"][0], b"$Format:%H$")
            tree_sha = release.subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], cwd=root, text=True).strip()
            with self.assertRaisesRegex(ValueError, "identify a Git commit"):
                release.application_snapshot(root, tree_sha)

    def test_current_git_tree_requires_every_member_and_original_bytes(self):
        mutations = (
            lambda rows: [row for row in rows if row[0] != "application/source/application.txt"],
            lambda rows: [(name, b"forged" if name == "application/source/application.txt" else body, mode) for name, body, mode in rows],
            lambda rows: rows + [("application/source/unlisted.txt", b"extra", 0o644)],
            lambda rows: rows + [rows[-1]],
            lambda rows: rows + [("application/source/../escape", b"unsafe", 0o644)],
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as folder:
                directory, root = Path(folder) / "assets", Path(folder) / "root"
                directory.mkdir()
                _, source_sha, plan = frozen_source(directory, root)
                rewrite_archive(directory, plan, mutation)
                with self.assertRaisesRegex(ValueError, "members differ|Unsafe or duplicate"):
                    release.verify_source_material(directory, plan, root=root, source_sha=source_sha)
        with tempfile.TemporaryDirectory() as folder:
            directory, root = Path(folder) / "assets", Path(folder) / "root"
            directory.mkdir()
            _, source_sha, plan = frozen_source(directory, root)
            # Archive and inventory agree but the committed Git tree rejects the forged application.
            def forge(rows):
                inventory = json.loads(next(body for name, body, mode in rows if name == release.SOURCE_INVENTORY))
                entry = next(entry for entry in inventory["files"] if entry["path"] == "application.txt")
                entry.update(sha256=release.hashlib.sha256(b"forged").hexdigest(), size=6)
                inventory_bytes = json.dumps(inventory).encode()
                plan["application_source_inventory_sha256"] = release.hashlib.sha256(inventory_bytes).hexdigest()
                manifest = json.loads(rows[0][1])
                manifest["inventory_sha256"][release.SOURCE_INVENTORY] = plan["application_source_inventory_sha256"]
                return [(name, json.dumps(manifest).encode() if name == "SOURCE-MANIFEST.json" else
                         inventory_bytes if name == release.SOURCE_INVENTORY else
                         b"forged" if name == "application/source/application.txt" else body, mode) for name, body, mode in rows]
            rewrite_archive(directory, plan, forge)
            with self.assertRaisesRegex(ValueError, "frozen Git tree"):
                release.verify_source_material(directory, plan, root=root, source_sha=source_sha)

    def test_checksum_cache_reuses_reviewed_archive_without_network(self):
        with tempfile.TemporaryDirectory() as folder:
            directory, root = Path(folder) / "assets", Path(folder) / "root"
            directory.mkdir()
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
            reviewed_source(directory, root)
            archive = directory / release.SOURCE_ASSET
            original = archive.read_bytes()
            cache = Path(folder) / "cache"
            release.source_material(directory, root=root, cache=cache)
            checksum = release.digest(archive)
            self.assertEqual((cache / (checksum + ".tar.xz")).read_bytes(), original)
            archive.unlink()
            with mock.patch.object(release.urllib.request, "urlopen") as download:
                release.source_material(directory, fetch=True, root=root, cache=cache)
                download.assert_not_called()
            self.assertEqual(archive.read_bytes(), original)
            (cache / (checksum + ".tar.xz")).write_bytes(b"corrupt")
            # A correct local archive also repairs a corrupt cached entry without fetching.
            with mock.patch.object(release.urllib.request, "urlopen") as download:
                release.source_material(directory, fetch=True, root=root, cache=cache)
                download.assert_not_called()
            self.assertEqual((cache / (checksum + ".tar.xz")).read_bytes(), original)


def development_fixture(folder, platform="linux", version="0.2.0", mutation=None):
    root, baseline, runtime = (Path(folder) / name for name in ("repository", "baseline", "runtime"))
    baseline.mkdir()
    for target in ("linux", "windows"):
        (baseline / f"BUILDINFO-{target}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
    reviewed_source(baseline, root)
    for name in ("package.json", "src-tauri/tauri.conf.json"):
        (root / name).write_text('{"version":"0.1.0"}')
    (root / "src-tauri/Cargo.toml").write_text('[package]\nname = "aperture"\nversion = "0.1.0"\n')
    release.set_version(version, root)
    if mutation:
        mutation(root)
    release.subprocess.run(["git", "init", "-q", str(root)], check=True)
    release.subprocess.run(["git", "add", "."], cwd=root, check=True)
    release.subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "Public development source fixture"], cwd=root, check=True)
    source_sha = release.bounded.identity(root)["source_sha"]
    runtime.mkdir()
    info = json.loads((baseline / f"BUILDINFO-{platform}.json").read_text())
    info.update(platform=platform, version=version, source_sha=source_sha, lockfiles=release.lockfile_hashes(root))
    (runtime / f"BUILDINFO-{platform}.json").write_text(json.dumps(info))
    (runtime / f"THIRD-PARTY-NOTICES-{platform}.txt").write_text("Synthetic dependency notices and source provenance.\n")
    return root, baseline, runtime, source_sha


class DevelopmentSourceTests(unittest.TestCase):
    def test_development_references_baseline_and_current_git_with_exact_root_version_delta(self):
        for platform in ("linux", "windows"):
            for version in ("0.1.0", "0.2.0"):
                with self.subTest(platform=platform, version=version), tempfile.TemporaryDirectory() as folder:
                    root, baseline, runtime, source_sha = development_fixture(folder, platform, version)
                    original = release.digest(baseline / release.SOURCE_ASSET)
                    cache = Path(folder) / "cache"
                    with mock.patch.object(release.urllib.request, "urlopen") as download:
                        release.development_source(runtime, baseline, platform, source_sha, fetch=True, cache=cache, root=root)
                        download.assert_not_called()
                    record = json.loads((runtime / "DEVELOPMENT-SOURCE.json").read_text())
                    self.assertEqual(record["stage"], "A")
                    self.assertTrue(record["application_source_url"].endswith(source_sha))
                    self.assertEqual(record["baseline"]["sha256"], original)
                    self.assertEqual(record["cargo_root_version"], {"baseline": "0.1.0", "current": version,
                                     "change": "unchanged" if version == "0.1.0" else "aperture-root-version-only"})
                    self.assertEqual(record["application_lockfiles_sha256"], release.lockfile_hashes(root))
                    self.assertEqual(record["notices"]["sha256"], release.digest(runtime / f"THIRD-PARTY-NOTICES-{platform}.txt"))
                    self.assertFalse((runtime / release.SOURCE_ASSET).exists())
                    self.assertEqual(release.digest(baseline / release.SOURCE_ASSET), original)
                    self.assertEqual(release.digest(cache / (original + ".tar.xz")), original)
                    if version != "0.1.0":
                        with self.assertRaisesRegex(ValueError, "checkout application lockfiles"):
                            release.source_material(baseline, root=root)

    def test_development_dependency_delta_requires_separate_review(self):
        for filename, before, after in (("src-tauri/Cargo.lock", b'name = "dep"\nversion = "1.0.0"', b'name = "dep"\nversion = "2.0.0"'),
                                        ("src-tauri/Cargo.lock", b'[[package]]', b'# changed comment\n[[package]]'),
                                        ("pnpm-lock.yaml", b"lockfileVersion: '9.0'", b"lockfileVersion: '8.0'")):
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as folder:
                def mutate(root):
                    path = root / filename
                    path.write_bytes(path.read_bytes().replace(before, after))
                root, baseline, runtime, source_sha = development_fixture(folder, mutation=mutate)
                with self.assertRaisesRegex(ValueError, "dependencies"):
                    release.development_source(runtime, baseline, "linux", source_sha, root=root)
                self.assertFalse((runtime / "DEVELOPMENT-SOURCE.json").exists())

    def test_development_requires_actual_build_native_identity_notices_and_reviewed_archive(self):
        for change in ("source", "build-source", "build-lock", "native", "notices", "archive"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as folder:
                root, baseline, runtime, source_sha = development_fixture(folder)
                path = runtime / "BUILDINFO-linux.json"
                info = json.loads(path.read_text())
                if change == "source":
                    source_sha = "f" * 40
                elif change == "build-source":
                    info["source_sha"] = "f" * 40
                elif change == "build-lock":
                    info["lockfiles"]["src-tauri/Cargo.lock"] = "f" * 64
                elif change == "native":
                    info["native_dependencies"]["libvips"] = "unreviewed"
                elif change == "notices":
                    (runtime / "THIRD-PARTY-NOTICES-linux.txt").write_bytes(b"")
                elif change == "archive":
                    (baseline / release.SOURCE_ASSET).write_bytes(b"unreviewed source")
                path.write_text(json.dumps(info))
                with self.assertRaises(ValueError):
                    release.development_source(runtime, baseline, "linux", source_sha, root=root)
                self.assertFalse((runtime / "DEVELOPMENT-SOURCE.json").exists())


class SourceAssemblyDeadlineTests(unittest.TestCase):
    def test_assembly_uses_original_attempt_and_rejects_old_or_revised_inputs(self):
        refresh = helper_module("refresh-source")
        expected = {"schema": 1, "stage": "B", "source_sha": "a" * 40, "build_inputs_sha256": "b" * 64,
                    "freeze_started_at": "2026-10-14T00:00:00Z", "deadline_at": "2026-10-14T00:30:00Z"}
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "attempt.json"
            path.write_text(json.dumps(expected))
            with mock.patch.object(refresh.bounded, "identity", return_value=expected), mock.patch.object(refresh.bounded, "timestamp", return_value="2026-10-14T00:10:00Z"):
                attempt, checksum = refresh.attempt_inputs(path, Path(folder), "a" * 40)
                self.assertEqual(attempt, expected)
                self.assertEqual(checksum, release.digest(path))
                for source_sha in ("c" * 40, "main"):
                    with self.assertRaisesRegex(ValueError, "frozen source SHA"):
                        refresh.attempt_inputs(path, Path(folder), source_sha)
                with self.assertRaisesRegex(ValueError, "bytes changed"):
                    refresh.attempt_inputs(path, Path(folder), "a" * 40, "f" * 64)
            with mock.patch.object(refresh.bounded, "identity", return_value={**expected, "build_inputs_sha256": "c" * 64}), self.assertRaisesRegex(ValueError, "frozen inputs"):
                refresh.attempt_inputs(path, Path(folder), "a" * 40)
            with mock.patch.object(refresh.bounded, "identity", return_value=expected), mock.patch.object(refresh.bounded, "timestamp", return_value="2026-10-14T00:31:00Z"), self.assertRaisesRegex(ValueError, "deadline exceeded"):
                refresh.attempt_inputs(path, Path(folder), "a" * 40)
            path.write_text(json.dumps({**expected, "deadline_at": "2026-10-14T00:31:00Z"}))
            with mock.patch.object(refresh.bounded, "identity", return_value=expected), self.assertRaisesRegex(ValueError, "exactly 30"):
                refresh.attempt_inputs(path, Path(folder), "a" * 40)

    def test_actual_write_requires_attempt_and_dispatches_one_bounded_assembly(self):
        refresh = helper_module("refresh-source")
        expected = {"schema": 1, "stage": "B", "source_sha": "a" * 40, "build_inputs_sha256": "b" * 64,
                    "freeze_started_at": "2026-10-14T00:00:00Z", "deadline_at": "2026-10-14T00:30:00Z"}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            output = root / "material"
            args = ["refresh-source.py", "--prior-archive", str(root / release.SOURCE_ASSET), "--prior-review", str(root / "prior.json"),
                    "--source-sha", "a" * 40, "--output", str(output), "--root", str(root), "--write"]
            with mock.patch.object(refresh.sys, "argv", args), mock.patch.object(refresh, "prepare") as prepare, mock.patch.object(refresh.sys, "stderr", io.StringIO()), self.assertRaises(SystemExit) as missing:
                refresh.main()
            self.assertEqual(missing.exception.code, 2)
            prepare.assert_not_called()
            self.assertFalse(output.exists())
            def timed_out(attempt, seconds, command, report):
                self.assertEqual(attempt, expected)
                self.assertEqual(seconds, 600)
                self.assertEqual(command.count("--assemble-worker"), 1)
                self.assertIn("--attempt", command)
                self.assertIn("--attempt-sha256", command)
                output.mkdir()
                (output / "source.partial").write_bytes(b"diagnostic partial bytes")
                refresh.bounded.write_report(report, {**attempt, "status": "fail", "forced_kill": True, "started_at": "2026-10-14T00:10:00Z", "finished_at": "2026-10-14T00:20:00Z"})
                return 1
            with mock.patch.object(refresh.sys, "argv", args + ["--attempt", str(root / "attempt.json")]), mock.patch.object(refresh, "attempt_inputs", return_value=(expected, "f" * 64)), mock.patch.object(refresh.bounded, "run_bounded", side_effect=timed_out) as run, self.assertRaises(SystemExit) as stopped:
                refresh.main()
            self.assertEqual(stopped.exception.code, 1)
            run.assert_called_once()
            report = refresh.bounded.read(output / "ASSEMBLY.json")
            self.assertEqual(report["status"], "fail")
            self.assertTrue(report["forced_kill"])
            self.assertEqual(report["freeze_started_at"], expected["freeze_started_at"])
            self.assertEqual(report["attempt_sha256"], "f" * 64)
            self.assertEqual(report["diagnostic_files"], [{"name": "source.partial", "bytes": 24}])
            self.assertEqual((output / "source.partial").read_bytes(), b"diagnostic partial bytes")
            # A different output path cannot restart source assembly for this freeze.
            with self.assertRaisesRegex(ValueError, "one source assembly"):
                refresh.reserve_assembly(root, expected, "f" * 64, root / "different-output")
            revised = {**expected, "freeze_started_at": "2026-10-14T01:00:00Z", "deadline_at": "2026-10-14T01:30:00Z"}
            refresh.reserve_assembly(root, revised, "1" * 64, root / "later-attempt")

    def test_successful_assembly_review_binds_exact_original_attempt_receipt(self):
        refresh = helper_module("refresh-source")
        expected = {"schema": 1, "stage": "B", "source_sha": "a" * 40, "build_inputs_sha256": "b" * 64,
                    "freeze_started_at": "2026-10-14T00:00:00Z", "deadline_at": "2026-10-14T00:30:00Z"}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            output = root / "material"
            args = ["refresh-source.py", "--prior-archive", str(root / release.SOURCE_ASSET), "--prior-review", str(root / "prior.json"),
                    "--source-sha", "a" * 40, "--output", str(output), "--root", str(root), "--write", "--attempt", str(root / "attempt.json")]
            def completed(attempt, seconds, command, report):
                output.mkdir()
                (output / "REFRESH.json").write_text(json.dumps({"status": "assembled-awaiting-review", "deadline_exceeded": False}))
                (output / release.SOURCE_REVIEW).write_text(json.dumps({"status": "assembled-awaiting-review", "sha256": "c" * 64}))
                refresh.bounded.write_report(report, {**attempt, "status": "pass", "forced_kill": False,
                                                     "started_at": "2026-10-14T00:10:00Z", "finished_at": "2026-10-14T00:11:00Z"})
                return 0
            with mock.patch.object(refresh.sys, "argv", args), mock.patch.object(refresh, "attempt_inputs", return_value=(expected, "f" * 64)), mock.patch.object(refresh.bounded, "validate_deadline"), mock.patch.object(refresh.bounded, "run_bounded", side_effect=completed) as run, self.assertRaises(SystemExit) as finished:
                refresh.main()
            self.assertEqual(finished.exception.code, 0)
            run.assert_called_once()
            plan = json.loads((output / release.SOURCE_REVIEW).read_text())
            receipt = plan["assembly_attempt"]
            self.assertEqual(receipt["status"], "pass")
            self.assertFalse(receipt["forced_kill"])
            for key in ("source_sha", "build_inputs_sha256", "freeze_started_at", "deadline_at"):
                self.assertEqual(receipt[key], expected[key])
            self.assertEqual(receipt["attempt_sha256"], "f" * 64)
            self.assertEqual(receipt["archive_sha256"], plan["sha256"])
            self.assertEqual(receipt["refresh_receipt_sha256"], release.digest(output / "REFRESH.json"))
            self.assertEqual(plan["status"], "assembled-awaiting-review")

    def test_expired_finished_assembly_preserves_bytes_but_never_ready_review(self):
        refresh = helper_module("refresh-source")
        expected = {"schema": 1, "stage": "B", "source_sha": "a" * 40, "build_inputs_sha256": "b" * 64,
                    "freeze_started_at": "2026-10-14T00:00:00Z", "deadline_at": "2026-10-14T00:30:00Z"}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            prior = root / release.SOURCE_ASSET
            prior.write_bytes(b"tiny prior fixture")
            output = root / "material"
            args = SimpleNamespace(prior_archive=prior, prior_review=root / "prior.json", root=root, source_sha="a" * 40,
                                   output=output, write=True, timeout=600, xz_preset=0)
            generated = {"SOURCE-MANIFEST.json": b'{"refresh":{"changed_application_files":[]}}'}
            review = {"schema": 1, "status": "assembled-awaiting-review", "sha256": None}
            def write_tiny(archive, destination, *args):
                destination.write_bytes(b"preserved completed fixture")
                return {"sha256": release.digest(destination)}
            with mock.patch.object(refresh, "prepare", return_value=(generated, {}, set(), review)), mock.patch.object(refresh, "write_archive", side_effect=write_tiny), mock.patch.object(refresh.bounded, "deadline_remaining", return_value=30), mock.patch.object(refresh.bounded, "validate_deadline", side_effect=[None, ValueError("deadline exceeded")]), mock.patch.object(refresh.bounded, "timestamp", side_effect=["2026-10-14T00:29:30Z", "2026-10-14T00:29:31Z", "2026-10-14T00:30:01Z"]), self.assertRaisesRegex(ValueError, "deadline exceeded"):
                refresh.assemble(args, expected, "f" * 64)
            self.assertEqual((output / release.SOURCE_ASSET).read_bytes(), b"preserved completed fixture")
            self.assertEqual(json.loads((output / release.SOURCE_REVIEW).read_text())["status"], "assembled-awaiting-review")
            receipt = json.loads((output / "REFRESH.json").read_text())
            self.assertEqual(receipt["status"], "fail")
            self.assertTrue(receipt["deadline_exceeded"])
            self.assertEqual(receipt["sha256"], release.digest(output / release.SOURCE_ASSET))


class ReleaseTests(unittest.TestCase):
    def test_rust_runtime_notices_preserve_terms_and_reject_stale_or_changed_material(self):
        for platform in ("linux", "windows"):
            with self.subTest(platform=platform), mock.patch.object(notices.subprocess, "check_output", return_value="rustc 1.98.0 (88d9e12ae 2026-08-18)\n") as compiler:
                text = notices.rust_runtime_notice(platform)
                compiler.assert_called_once_with(["rustc", "--version"], text=True, encoding="utf-8")
                for required in ("The Rust Project Contributors", "Rust Standard Library", "Permission is hereby granted, free of charge", "Apache License", "Unicode"):
                    self.assertIn(required, text)
                self.assertNotIn("<html", text)
                self.assertNotIn("&quot;", text)
        with self.assertRaisesRegex(ValueError, "review the new toolchain"):
            notices.rust_runtime_notice("linux", compiler="rustc 1.99.0 (different)")
        with tempfile.TemporaryDirectory() as folder:
            copied = Path(folder) / "runtime"
            shutil.copytree(notices.RUST_RUNTIME, copied)
            with (copied / "LICENSE-MIT").open("ab") as license_file:
                license_file.write(b"changed")
            with mock.patch.object(notices, "RUST_RUNTIME", copied), self.assertRaisesRegex(ValueError, "checksum mismatch: LICENSE-MIT"):
                notices.rust_runtime_notice("windows", compiler="rustc 1.98.0 (88d9e12ae 2026-08-18)")

    def test_unassembled_material_blocks_candidate_before_feed_generation(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            (root / "scripts/release").mkdir(parents=True)
            (root / "scripts/release/corresponding-source.json").write_text('{"schema":1,"status":"unassembled"}')
            for signed in (True, False):
                with self.subTest(signed=signed), self.assertRaisesRegex(ValueError, "publication blocked"):
                    release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", "a" * 40, "2026-10-13T00:00:00Z", signed=signed, root=root)
            self.assertFalse((directory / "latest.json").exists())
            self.assertFalse((directory / "SHA256SUMS").exists())

    def test_source_archive_and_dependency_identity_are_verified(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            for name in release.ASSETS.values():
                (directory / name).write_bytes(b"artifact")
                (directory / (name + ".sig")).write_text("signature")
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
                (directory / f"THIRD-PARTY-NOTICES-{platform}.txt").write_text("notice")
            reviewed_source(directory, root)
            release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", "a" * 40, "2026-10-13T00:00:00Z", root=root)
            sums = (directory / "SHA256SUMS").read_text()
            self.assertIn(release.SOURCE_ASSET, sums)
            self.assertIn(release.SOURCE_REVIEW, sums)
            release.verify(directory, "0.1.0", "a" * 40)
            original = (directory / release.SOURCE_ASSET).read_bytes()
            (directory / release.SOURCE_ASSET).write_bytes(b"altered material")
            with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
                release.verify(directory, "0.1.0", "a" * 40)
            with self.assertRaisesRegex(ValueError, "reviewed bytes"):
                release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", "a" * 40, "2026-10-13T00:00:00Z", root=root)
            # A modified binary dependency record cannot use old reviewed material.
            reviewed = json.loads((root / "scripts/release/corresponding-source.json").read_text())
            info_path = directory / "BUILDINFO-linux.json"
            info = json.loads(info_path.read_text())
            info["native_dependencies"]["libvips"] = "different"
            info_path.write_text(json.dumps(info))
            (directory / release.SOURCE_ASSET).write_bytes(original)
            with self.assertRaisesRegex(ValueError, "native dependency identity"):
                release.verify_source_material(directory, reviewed, builds=True)

    def test_platform_source_gate_checks_that_platform_build(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
            reviewed_source(directory, root)
            release.source_material(directory, root=root, platform="linux")
            release.source_material(directory, root=root, platform="windows")
            for platform in ("linux", "windows"):
                path = directory / f"BUILDINFO-{platform}.json"
                original = path.read_text()
                info = json.loads(original)
                info["native_dependencies"]["libvips"] = "mismatched"
                path.write_text(json.dumps(info))
                with self.subTest(platform=platform), self.assertRaisesRegex(ValueError, f"{platform} native dependency identity"):
                    release.source_material(directory, root=root, platform=platform)
                path.write_text(original)

    def test_application_lock_changes_block_preflight_and_metadata_before_output(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
            reviewed_source(directory, root)
            for name in release.LOCKFILES:
                path = root / name
                original = path.read_bytes()
                path.write_bytes(original + b"changed dependency resolution\n")
                with self.subTest(lockfile=name), mock.patch.object(release.urllib.request, "urlopen") as download:
                    with self.assertRaisesRegex(ValueError, "checkout application lockfiles"):
                        release.source_material(directory, fetch=True, root=root)
                    download.assert_not_called()
                    with self.assertRaisesRegex(ValueError, "checkout application lockfiles"):
                        release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", "a" * 40, "2026-10-13T00:00:00Z", root=root)
                    self.assertFalse((directory / "latest.json").exists())
                    self.assertFalse((directory / "SHA256SUMS").exists())
                path.write_bytes(original)

    def test_build_lock_changes_block_both_platform_uploads_and_aggregate_verification(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
            reviewed_source(directory, root)
            plan = release.source_plan(root / "scripts/release/corresponding-source.json")
            for platform in ("linux", "windows"):
                path = directory / f"BUILDINFO-{platform}.json"
                original = path.read_text()
                for lockfile in release.LOCKFILES:
                    info = json.loads(original)
                    info["lockfiles"][lockfile] = "f" * 64
                    path.write_text(json.dumps(info))
                    with self.subTest(platform=platform, lockfile=lockfile):
                        with self.assertRaisesRegex(ValueError, f"{platform} application lockfiles"):
                            release.source_material(directory, root=root, platform=platform)
                        with self.assertRaisesRegex(ValueError, f"{platform} application lockfiles"):
                            release.verify_source_material(directory, plan, builds=True)
                        with self.assertRaisesRegex(ValueError, f"{platform} application lockfiles"):
                            release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", "a" * 40, "2026-10-13T00:00:00Z", root=root)
                    path.write_text(original)

    def test_review_requires_archived_application_lock_and_inventory_identities(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
            reviewed_source(directory, root)
            path = root / "scripts/release/corresponding-source.json"
            plan = json.loads(path.read_text())
            for field in ("application_lockfiles_sha256", "application_source_inventory_sha256"):
                changed = {key: value for key, value in plan.items() if key != field}
                path.write_text(json.dumps(changed))
                with self.subTest(field=field), self.assertRaisesRegex(ValueError, "both application lockfiles"):
                    release.source_plan(path)
            changed = {**plan, "application_source_inventory_sha256": "f" * 64}
            with self.assertRaisesRegex(ValueError, "application inventory"):
                release.verify_source_material(directory, changed)

    def test_new_plan_checkout_and_build_locks_cannot_reuse_old_source_archive(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
            reviewed_source(directory, root)
            path = root / "scripts/release/corresponding-source.json"
            plan = json.loads(path.read_text())
            for lockfile in release.LOCKFILES:
                original = (root / lockfile).read_bytes()
                (root / lockfile).write_bytes(original + b"changed resolution\n")
                falsified = {**plan, "application_lockfiles_sha256": release.lockfile_hashes(root)}
                path.write_text(json.dumps(falsified))
                for platform in ("linux", "windows"):
                    info_path = directory / f"BUILDINFO-{platform}.json"
                    info = json.loads(info_path.read_text())
                    info["lockfiles"] = falsified["application_lockfiles_sha256"]
                    info_path.write_text(json.dumps(info))
                with self.subTest(lockfile=lockfile):
                    with self.assertRaisesRegex(ValueError, "archive application lockfiles"):
                        release.source_material(directory, root=root)
                    with self.assertRaisesRegex(ValueError, "archive application lockfiles"):
                        release.source_material(directory, root=root, platform="windows")
                    with self.assertRaisesRegex(ValueError, "archive application lockfiles"):
                        release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", "a" * 40, "2026-10-13T00:00:00Z", root=root)
                (root / lockfile).write_bytes(original)

    def test_upgrade_fixture_requires_exact_preserved_root_version_delta(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.1", "source_sha": "a" * 40}))
            reviewed_source(directory, root, version="0.1.1")
            for name in ("package.json", "src-tauri/tauri.conf.json"):
                (root / name).write_text('{"version":"0.1.1"}')
            (root / "src-tauri/Cargo.toml").write_text('[package]\nname = "aperture"\nversion = "0.1.1"\n')
            production = (root / "src-tauri/Cargo.lock").read_bytes()
            delta = directory / "test-deltas/src-tauri/Cargo.lock"
            delta.parent.mkdir(parents=True)
            delta.write_bytes(production.replace(b'name = "aperture"\nversion = "0.1.1"', b'name = "aperture"\nversion = "0.0.9"'))
            path = directory / "BUILDINFO-windows.json"
            info = json.loads(path.read_text())
            info["version"] = "0.0.9"
            info["lockfiles"]["src-tauri/Cargo.lock"] = release.digest(delta)
            path.write_text(json.dumps(info))
            with self.assertRaisesRegex(ValueError, "windows application lockfiles"):
                release.source_material(directory, root=root, platform="windows")
            release.source_material(directory, root=root, platform="windows", upgrade_fixture=True)
            for other in ("0.1.0", "0.2.0"):
                with self.subTest(candidate_version=other), mock.patch.object(release, "current_version", return_value=other), self.assertRaisesRegex(ValueError, "0.1.1 to 0.0.9"):
                    release.source_material(directory, root=root, platform="windows", upgrade_fixture=True)
            plan = release.source_plan(root / "scripts/release/corresponding-source.json")
            with self.assertRaisesRegex(ValueError, "windows application lockfiles"):
                release.verify_source_material(directory, plan, builds=True)
            info["lockfiles"]["src-tauri/Cargo.lock"] = "f" * 64
            path.write_text(json.dumps(info))
            with self.assertRaisesRegex(ValueError, "windows application lockfiles"):
                release.source_material(directory, root=root, platform="windows", upgrade_fixture=True)
            delta.write_bytes(delta.read_bytes().replace(b'name = "dep"\nversion = "1.0.0"', b'name = "dep"\nversion = "2.0.0"'))
            info["lockfiles"]["src-tauri/Cargo.lock"] = release.digest(delta)
            path.write_text(json.dumps(info))
            with self.assertRaisesRegex(ValueError, "only in the aperture root version"):
                release.source_material(directory, root=root, platform="windows", upgrade_fixture=True)
            delta.unlink()
            with self.assertRaisesRegex(ValueError, "only in the aperture root version"):
                release.source_material(directory, root=root, platform="windows", upgrade_fixture=True)

    def test_windows_pnpm_shim_uses_command_interpreter(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder)
            dependency = directory / "dependencies.json"
            dependency.write_text("{}")
            interpreter = r"C:\Windows\System32\cmd.exe"
            shim = r"C:\setup-pnpm\pnpm.cmd"
            with mock.patch.object(release.os, "name", "nt"), mock.patch.dict(release.os.environ, {"COMSPEC": interpreter}), mock.patch.object(release.shutil, "which", return_value=shim), mock.patch.object(release.subprocess, "check_output", return_value="version") as command:
                release.buildinfo("windows", directory, dependency)
            self.assertIn(mock.call((interpreter, "/d", "/c", shim, "--version"), cwd=release.ROOT, text=True, encoding="utf-8"), command.call_args_list)

    def test_version_and_notes(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "src-tauri").mkdir()
            for name in ("package.json", "src-tauri/tauri.conf.json"):
                (root / name).write_text('{"version":"0.1.0"}')
            (root / "src-tauri/Cargo.toml").write_text('[package]\nname = "aperture"\nversion = "0.1.0"\n')
            (root / "src-tauri/Cargo.lock").write_bytes(b'[[package]]\nname = "aperture"\nversion = "0.1.0"\n')
            (root / "pnpm-lock.yaml").write_bytes(b"lockfileVersion: '9.0'\n")
            (root / "CHANGELOG.md").write_text("# Changelog\n\n## [Unreleased]\n\n## [0.2.0] - 2026-10-13\n\n- A change.\n\n## [0.1.0]\n\n- Old.\n")
            release.set_version("0.2.0", root)
            self.assertEqual(release.current_version(root), "0.2.0")
            self.assertEqual(release.notes("0.2.0", root), "- A change.\n")
            self.assertEqual((root / "pnpm-lock.yaml").read_text(), "lockfileVersion: '9.0'\n")
            with self.assertRaises(ValueError):
                release.set_version("0.2.0-rc.1", root)

    def test_version_change_preserves_exact_non_root_lock_bytes(self):
        lock = b'# Cargo lock, preserve comments and dependencies\nversion = 4\n\n[[package]]\nname = "aperture"\nversion = "0.1.0"\ndependencies = ["dep"]\n\n[[package]]\nname = "dep"\nversion = "0.1.0"\nchecksum = "unchanged"\n'
        for newline in (b"\n", b"\r\n"):
            with self.subTest(newline=newline), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                (root / "src-tauri").mkdir()
                for name in ("package.json", "src-tauri/tauri.conf.json"):
                    (root / name).write_text('{"version":"0.1.0"}')
                (root / "src-tauri/Cargo.toml").write_text('[package]\nname = "aperture"\nversion = "0.1.0"\n')
                original = lock.replace(b"\n", newline)
                (root / "src-tauri/Cargo.lock").write_bytes(original)
                pnpm = b"lockfileVersion: '9.0'\n"
                (root / "pnpm-lock.yaml").write_bytes(pnpm)
                release.set_version("0.0.9", root)
                expected = original.replace(b'name = "aperture"' + newline + b'version = "0.1.0"', b'name = "aperture"' + newline + b'version = "0.0.9"', 1)
                self.assertEqual((root / "src-tauri/Cargo.lock").read_bytes(), expected)
                self.assertNotEqual(expected, original)
                self.assertEqual((root / "pnpm-lock.yaml").read_bytes(), pnpm)
                self.assertEqual(release.current_version(root), "0.0.9")

    def test_candidate_rejects_changed_bytes_and_feed(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            sha = "a" * 40
            for name in release.ASSETS.values():
                (directory / name).write_bytes(b"artifact")
                (directory / (name + ".sig")).write_text("signature")
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": sha}))
                (directory / f"THIRD-PARTY-NOTICES-{platform}.txt").write_text("License notice")
            reviewed_source(directory, root)
            release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", sha, "2026-10-13T00:00:00Z", root=root)
            release.verify(directory, "0.1.0", sha)
            (directory / "Aperture-Setup-x64.exe").write_bytes(b"edited")
            with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
                release.verify(directory, "0.1.0", sha)
            (directory / "Aperture-Setup-x64.exe").write_bytes(b"artifact")
            feed = json.loads((directory / "latest.json").read_text())
            feed["platforms"]["windows-x86_64"]["url"] = "https://example.org/changed.exe"
            (directory / "latest.json").write_text(json.dumps(feed))
            checksum = directory / "SHA256SUMS"
            checksum.write_text(checksum.read_text().replace(next(line.split()[0] for line in checksum.read_text().splitlines() if line.endswith("latest.json")), release.digest(directory / "latest.json")))
            with self.assertRaisesRegex(ValueError, "Updater feed mismatch"):
                release.verify(directory, "0.1.0", sha)

    def test_unsigned_has_no_feed(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / "assets"
            directory.mkdir()
            root = Path(folder) / "repository"
            for name in release.ASSETS.values():
                (directory / name).write_bytes(b"unsigned validation")
            for platform in ("linux", "windows"):
                (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps({"version": "0.1.0", "source_sha": "a" * 40}))
                (directory / f"THIRD-PARTY-NOTICES-{platform}.txt").write_text("notices")
            reviewed_source(directory, root)
            release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", "a" * 40, "2026-10-13T00:00:00Z", signed=False, root=root)
            self.assertFalse((directory / "latest.json").exists())
            sums = (directory / "SHA256SUMS").read_text()
            self.assertIn(release.SOURCE_ASSET, sums)
            self.assertIn(release.SOURCE_REVIEW, sums)
            release.verify(directory, "0.1.0", "a" * 40, signed=False)
            with self.assertRaises(ValueError):
                release.verify(directory, "0.1.0", "a" * 40)
            (directory / release.SOURCE_ASSET).unlink()
            with self.assertRaisesRegex(ValueError, "archive missing"):
                release.manifest(directory, "0.1.0", "neuroqp/neuroqp-aperture", "a" * 40, "2026-10-13T00:00:00Z", signed=False, root=root)


if __name__ == "__main__":
    unittest.main()
