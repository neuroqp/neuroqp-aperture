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


def fixture(folder):
    root, assets, cache = folder / "repo", folder / "assets", folder / "cache"
    (root / "src-tauri").mkdir(parents=True)
    (root / "scripts/release").mkdir(parents=True)
    cache.mkdir()
    cargo = ('[[package]]\nname = "aperture"\nversion = "0.2.1"\n\n'
             '[[package]]\nname = "dep"\nversion = "1.0.0"\n'
             'source = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "' + '1' * 64 + '"\n').encode()
    for name, body in {"package.json": b'{"version":"0.2.1"}', "src-tauri/tauri.conf.json": b'{"version":"0.2.1"}',
            "src-tauri/Cargo.toml": b'[package]\nname = "aperture"\nversion = "0.2.1"\n',
            "src-tauri/Cargo.lock": cargo, "pnpm-lock.yaml": b'lockfileVersion: 9\n',
            "scripts/release/package-linux.sh": b'original reviewed recipe\n',
            "CHANGELOG.md": b'## [0.2.1]\n\n- Source composition.\n'}.items():
        (root / name).write_bytes(body)
    native = {platform: {"library": platform + "-1.0"} for platform in ("linux", "windows")}
    identities = {platform: release.native_identity(value) for platform, value in native.items()}
    records = {release.DEPENDENCY_INVENTORIES[0]: release.encoded({"crates": [
        {"name": "dep", "version": "1.0.0", "packageChecksum": "1" * 64}]}),
        release.DEPENDENCY_INVENTORIES[1]: release.encoded({"lockfile_sha256": release.digest(root / "pnpm-lock.yaml")})}
    pins = {name: release.sha(body) for name, body in records.items()}
    baseline_archive = cache / "baseline.tar.xz"
    with tarfile.open(baseline_archive, "w:xz") as target:
        for name, body in {"SOURCE-MANIFEST.json": release.encoded({"native_dependencies_sha256": identities,
                "inventory_sha256": pins}), **records, "native/sources.c": b'original native source; recipes; notices; patches'}.items():
            member = tarfile.TarInfo(name)
            member.size = len(body)
            target.addfile(member, io.BytesIO(body))
    checksum = release.digest(baseline_archive)
    baseline_archive.rename(cache / (checksum + '.tar.xz'))
    baseline = {"schema": 1, "status": "ready", "asset": release.DEPENDENCY_ASSET, "sha256": checksum,
                "url": 'https://github.com/neuroqp/neuroqp-aperture/releases/download/source-baseline/' + release.DEPENDENCY_ASSET,
                "native_dependencies_sha256": identities, "application_lockfiles_sha256": release.lockfile_hashes(root),
                "application_source_inventory_sha256": "9" * 64, "dependency_inventories_sha256": pins}
    (root / release.SOURCE_PUBLICATION).write_bytes(release.encoded(baseline))
    git(root, "init", "-q")
    source_sha = commit(root)
    plan = release.source_material(assets, root=root, source_sha=source_sha, cache=cache)
    for platform in native:
        (assets / f"BUILDINFO-{platform}.json").write_bytes(release.encoded({"version": "0.2.1", "platform": platform,
            "source_sha": source_sha, "lockfiles": release.lockfile_hashes(root), "native_dependencies": native[platform]}))
        (assets / f"THIRD-PARTY-NOTICES-{platform}.txt").write_bytes(b'License notices')
    for name in release.ASSETS.values():
        (assets / name).write_bytes(b'fixed executable bytes')
        (assets / (name + '.sig')).write_text('updater signature')
    return root, assets, cache, source_sha, plan


def git(root, *args):
    return release.subprocess.check_output(['git', '-c', 'user.name=Source Test', '-c', 'user.email=source@example.invalid', *args], cwd=root).decode().strip()


def commit(root):
    git(root, 'add', '.')
    git(root, 'commit', '-qm', 'source fixture')
    return git(root, 'rev-parse', 'HEAD')


class SourceCompositionTests(unittest.TestCase):
    def test_app_fix_reuses_immutable_dependencies_and_contains_complete_git_source(self):
        with tempfile.TemporaryDirectory() as folder:
            root, assets, cache, source_sha, plan = fixture(Path(folder))
            baseline = plan['dependency_source']
            archive = cache / (baseline['sha256'] + '.tar.xz')
            (root / 'scripts/release/package-linux.sh').write_bytes(b'corrected packaging recipe\n')
            source_sha = commit(root)
            # Hash-verified cached inventories avoid decompressing the large bundle again.
            real_open = tarfile.open
            def no_bundle(path, *args, **kwargs):
                self.assertNotEqual(Path(path), archive)
                return real_open(path, *args, **kwargs)
            with mock.patch.object(release.tarfile, 'open', side_effect=no_bundle), mock.patch.object(release.urllib.request, 'urlopen') as fetch:
                plan = release.source_material(assets, root=root, source_sha=source_sha, fetch=True, cache=cache)
                fetch.assert_not_called()
            self.assertEqual(release.digest(archive), baseline['sha256'])
            self.assertFalse((assets / release.DEPENDENCY_ASSET).exists())
            release.verify_source_material(assets, plan, root=root, source_sha=source_sha)
            with tarfile.open(assets / release.SOURCE_ASSET) as source:
                self.assertEqual(source.extractfile('application/source/' + release.SOURCE_PUBLICATION).read(), (root / release.SOURCE_PUBLICATION).read_bytes())
                self.assertEqual(source.extractfile('application/source/scripts/release/package-linux.sh').read(), b'corrected packaging recipe\n')
            self.assertLess((assets / release.SOURCE_ASSET).stat().st_size, 10000)

    def test_dependency_changes_block_before_snapshot_publication(self):
        for change in ('npm', 'cargo', 'git-source'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as folder:
                root, assets, cache, _, _ = fixture(Path(folder))
                if change == 'npm':
                    (root / 'pnpm-lock.yaml').write_bytes(b'unreviewed npm dependency')
                else:
                    path = root / 'src-tauri/Cargo.lock'
                    body = path.read_bytes().replace(b'checksum = "' + b'1' * 64, b'checksum = "' + b'2' * 64)
                    if change == 'git-source':
                        body = body.replace(b'registry+https://github.com/rust-lang/crates.io-index', b'git+https://example.invalid/repo')
                    path.write_bytes(body)
                source_sha = commit(root)
                before = release.digest(assets / release.SOURCE_ASSET)
                with mock.patch.object(release.urllib.request, 'urlopen') as fetch, self.assertRaises(ValueError):
                    release.source_material(assets, root=root, source_sha=source_sha, fetch=True, cache=cache)
                fetch.assert_not_called()
                self.assertEqual(release.digest(assets / release.SOURCE_ASSET), before)

    def test_source_composition_checks_archive_inventory_git_and_native_identity(self):
        for change in ('archive', 'closure', 'baseline', 'build-lock', 'build-native', 'build-sha', 'git-tree'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as folder:
                root, assets, _, source_sha, plan = fixture(Path(folder))
                if change == 'archive':
                    with (assets / release.SOURCE_ASSET).open('ab') as output: output.write(b'altered')
                elif change == 'closure':
                    plan['dependency_closure']['cargo_registry_packages']['dep@1.0.0'] = '2' * 64
                elif change == 'baseline':
                    plan['dependency_source']['sha256'] = '2' * 64
                elif change == 'git-tree':
                    (root / 'extra.txt').write_text('another committed file')
                    source_sha = commit(root)
                else:
                    path = assets / 'BUILDINFO-linux.json'; info = json.loads(path.read_bytes())
                    if change == 'build-lock': info['lockfiles']['pnpm-lock.yaml'] = '2' * 64
                    elif change == 'build-native': info['native_dependencies']['library'] = 'unreviewed'
                    else: info['source_sha'] = '2' * 40
                    path.write_bytes(release.encoded(info))
                with self.assertRaises(ValueError):
                    release.verify_source_material(assets, plan, root=root, source_sha=source_sha, builds=True)

    def test_untracked_local_dependency_and_missing_local_package_are_rejected(self):
        for change in ('path', 'missing'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as folder:
                root, assets, cache, _, _ = fixture(Path(folder))
                if change == 'path':
                    with (root / 'src-tauri/Cargo.toml').open('ab') as output:
                        output.write(b'[dependencies]\nexternal = { path = "../../untracked" }\n')
                else:
                    with (root / 'src-tauri/Cargo.lock').open('ab') as output:
                        output.write(b'\n[[package]]\nname = "untracked"\nversion = "1.0.0"\n')
                with self.assertRaisesRegex(ValueError, 'Local Cargo'):
                    release.source_material(assets, root=root, source_sha=commit(root), cache=cache)

    def test_archive_member_and_dependency_inventory_tamper_is_rejected_after_rehash(self):
        for change in ('duplicate', 'traversal', 'inventory', 'file'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as folder:
                root, assets, _, source_sha, plan = fixture(Path(folder))
                archive = assets / release.SOURCE_ASSET
                with tarfile.open(archive) as source:
                    members = [(member, source.extractfile(member).read()) for member in source]
                with tarfile.open(archive, 'w:xz') as target:
                    for member, body in members:
                        if change == 'inventory' and member.name == release.DEPENDENCY_INVENTORIES[0]:
                            body = body.replace(b'"1.0.0"', b'"9.0.0"')
                        if change == 'file' and member.name == 'application/source/package.json':
                            body = body.replace(b'0.2.1', b'0.2.2')
                        member.size = len(body)
                        target.addfile(member, io.BytesIO(body))
                    if change in ('duplicate', 'traversal'):
                        member = tarfile.TarInfo(members[0][0].name if change == 'duplicate' else '../outside')
                        member.size = 1
                        target.addfile(member, io.BytesIO(b'x'))
                plan['sha256'] = release.digest(archive)
                with self.assertRaises(ValueError):
                    release.verify_source_material(assets, plan, root=root, source_sha=source_sha)

    def test_release_metadata_checks_asset_hashes_feed_and_exact_source(self):
        with tempfile.TemporaryDirectory() as folder:
            root, assets, _, source_sha, _ = fixture(Path(folder))
            release.manifest(assets, '0.2.1', 'neuroqp/neuroqp-aperture', source_sha, '2026-10-15T00:00:00Z', root=root)
            release.verify(assets, '0.2.1', source_sha, root=root)
            self.assertIn(release.SOURCE_ASSET, (assets / 'SHA256SUMS').read_text())
            self.assertNotIn(release.DEPENDENCY_ASSET, (assets / 'SHA256SUMS').read_text())
            (assets / 'Aperture-Setup-x64.exe').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'Checksum mismatch'):
                release.verify(assets, '0.2.1', source_sha, root=root)
            (assets / 'Aperture-Setup-x64.exe').write_bytes(b'fixed executable bytes')
            feed = json.loads((assets / 'latest.json').read_bytes())
            feed['platforms']['linux-x86_64']['url'] = 'https://example.invalid/wrong'
            (assets / 'latest.json').write_bytes(release.encoded(feed))
            sums = assets / 'SHA256SUMS'
            lines = sums.read_text().splitlines()
            sums.write_text('\n'.join((release.digest(assets / 'latest.json') + '  latest.json') if line.endswith('  latest.json') else line for line in lines) + '\n')
            with self.assertRaisesRegex(ValueError, 'Updater feed mismatch'):
                release.verify(assets, '0.2.1', source_sha, root=root)

    def test_development_coverage_pins_runtime_metadata_and_notices(self):
        with tempfile.TemporaryDirectory() as folder:
            root, assets, cache, source_sha, plan = fixture(Path(folder))
            material = Path(folder) / 'source-material'
            release.development_source(assets, material, 'linux', source_sha, cache=cache, root=root)
            with mock.patch.object(release, 'dependency_records', side_effect=AssertionError('large baseline must stay unused')):
                release.development_source(assets, material, 'linux', source_sha, root=root)
            record = json.loads((assets / 'DEVELOPMENT-SOURCE.json').read_bytes())
            self.assertEqual(record['dependency_source'], plan['dependency_source'])
            self.assertEqual(record['buildinfo']['sha256'], release.digest(assets / 'BUILDINFO-linux.json'))
            (assets / 'THIRD-PARTY-NOTICES-linux.txt').write_bytes(b'')
            with self.assertRaises(ValueError):
                release.development_source(assets, material, 'linux', source_sha, cache=cache, root=root)


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

    def test_pinned_crate_licenses_cover_locked_crates_and_reject_changed_material(self):
        lock = (notices.ROOT / "src-tauri/Cargo.lock").read_text(encoding="utf-8")
        pins = json.loads((notices.RUST_RUNTIME / "crates.json").read_text(encoding="utf-8"))["crates"]
        for item in pins:
            self.assertIn(f'name = "{item["name"]}"\nversion = "{item["version"]}"\nsource = "{item["source"]}"\nchecksum = "{item["checksum"]}"\n', lock)
        registry = "registry+https://github.com/rust-lang/crates.io-index"
        expected = {
            ("alloc-stdlib", "0.2.4"): ("Copyright (c) 2016 Dropbox, Inc.", "Redistributions in binary form must reproduce", "ae42d22078b98549e987d2f03d12df7b984fde47"),
            ("dlopen2", "0.8.2"): ("Copyright (c) 2017 Szymon Wieloch", "Copyright (C) 2022 OpenByte", "cc80e4a0a90d499b677fdf7743699b4b3a43a989"),
            ("dlopen2_derive", "0.4.3"): ("Copyright (c) 2017 Szymon Wieloch", "Copyright (C) 2022 OpenByte", "cc80e4a0a90d499b677fdf7743699b4b3a43a989"),
            ("selectors", "0.38.0"): ("Mozilla Public License Version 2.0", "Exhibit B", "a15178c3c7c976c67b3641af77cae0b66093a175"),
            ("webview2-com", "0.39.1"): ("Copyright (c) 2021 Bill Avery", "MIT License", "edc2caf886175ccaebe86078c9cfe1ae2a187328"),
            ("webview2-com-macros", "0.8.1"): ("Copyright (c) 2021 Bill Avery", "MIT License", "dffa41a8a46d3f5565eefbff2de57d38d399f158"),
            ("webview2-com-sys", "0.39.1"): ("Copyright (c) 2021 Bill Avery", "MIT License", "edc2caf886175ccaebe86078c9cfe1ae2a187328"),
        }
        self.assertEqual({(item["name"], item["version"]) for item in pins}, set(expected))
        for (name, version), required in expected.items():
            text = notices.pinned_crate_license({"name": name, "version": version, "source": registry})
            for value in required:
                self.assertIn(value, text)
        crate = {"name": "alloc-stdlib", "version": "0.2.4", "source": registry}
        self.assertEqual(notices.pinned_crate_license({**crate, "version": "0.3.0"}), "")
        self.assertEqual(notices.pinned_crate_license({**crate, "source": "git+https://example.invalid/alloc"}), "")
        with tempfile.TemporaryDirectory() as folder:
            copied = Path(folder) / "runtime"
            shutil.copytree(notices.RUST_RUNTIME, copied)
            with (copied / "alloc-stdlib-0.2.4-LICENSE").open("ab") as license_file:
                license_file.write(b"changed")
            with mock.patch.object(notices, "RUST_RUNTIME", copied), self.assertRaisesRegex(ValueError, "checksum mismatch: alloc-stdlib-0.2.4-LICENSE"):
                notices.pinned_crate_license(crate)
            with (copied / "MPL-2.0.txt").open("ab") as license_file:
                license_file.write(b"changed")
            with mock.patch.object(notices, "RUST_RUNTIME", copied), self.assertRaisesRegex(ValueError, "checksum mismatch: MPL-2.0.txt"):
                notices.pinned_crate_license({"name": "selectors", "version": "0.38.0", "source": registry})

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


if __name__ == "__main__":
    unittest.main()
