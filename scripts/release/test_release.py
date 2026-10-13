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


def reviewed_source(directory, root):
    (root / "scripts/release").mkdir(parents=True)
    (root / "CHANGELOG.md").write_text((release.ROOT / "CHANGELOG.md").read_text(encoding="utf-8"), encoding="utf-8")
    identities = {}
    for platform in ("linux", "windows"):
        path = directory / f"BUILDINFO-{platform}.json"
        info = json.loads(path.read_text(encoding="utf-8"))
        info["native_dependencies"] = {"libvips": "8.18.0" if platform == "linux" else "8.18.7"}
        path.write_text(json.dumps(info), encoding="utf-8")
        identities[platform] = release.native_identity(info["native_dependencies"])
    with tarfile.open(directory / release.SOURCE_ASSET, "w:xz") as source:
        for name, data in (("SOURCE-MANIFEST.json", json.dumps({"schema": 1, "native_dependencies_sha256": identities}).encode()), ("fixture/source.c", b"/* Synthetic source material for the gate test. */\n")):
            member = tarfile.TarInfo(name)
            member.size = len(data)
            source.addfile(member, io.BytesIO(data))
    plan = {"schema": 1, "status": "ready", "asset": release.SOURCE_ASSET, "url": "https://github.com/neuroqp/neuroqp-aperture/releases/download/source-fixture/Aperture-Corresponding-Source.tar.xz", "sha256": release.digest(directory / release.SOURCE_ASSET), "native_dependencies_sha256": identities}
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
            (root / "src-tauri/Cargo.lock").write_text('[[package]]\nname = "aperture"\nversion = "0.1.0"\n')
            (root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n")
            (root / "CHANGELOG.md").write_text("# Changelog\n\n## [Unreleased]\n\n## [0.2.0] - 2026-10-13\n\n- A change.\n\n## [0.1.0]\n\n- Old.\n")
            release.set_version("0.2.0", root)
            self.assertEqual(release.current_version(root), "0.2.0")
            self.assertEqual(release.notes("0.2.0", root), "- A change.\n")
            self.assertEqual((root / "pnpm-lock.yaml").read_text(), "lockfileVersion: '9.0'\n")
            with self.assertRaises(ValueError):
                release.set_version("0.2.0-rc.1", root)

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
