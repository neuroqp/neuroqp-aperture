"""Checks version synchronization and fail-closed candidate verification."""

import importlib.util
import io
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
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
        for name, data in (("SOURCE-MANIFEST.json", json.dumps({"schema": 1, "native_dependencies_sha256": identities, "inventory_sha256": {release.SOURCE_INVENTORY: inventory_hash}}).encode()), (release.SOURCE_INVENTORY, inventory), ("fixture/source.c", b"/* Synthetic source material for the gate test. */\n")):
            member = tarfile.TarInfo(name)
            member.size = len(data)
            source.addfile(member, io.BytesIO(data))
    plan = {"schema": 1, "status": "ready", "asset": release.SOURCE_ASSET, "url": "https://github.com/neuroqp/neuroqp-aperture/releases/download/source-fixture/Aperture-Corresponding-Source.tar.xz", "sha256": release.digest(directory / release.SOURCE_ASSET), "native_dependencies_sha256": identities, "application_lockfiles_sha256": locks, "application_source_inventory_sha256": inventory_hash}
    (root / "scripts/release/corresponding-source.json").write_text(json.dumps(plan), encoding="utf-8")
    release.source_material(directory, root=root)


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
