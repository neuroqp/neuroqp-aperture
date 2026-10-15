"""Frozen Linux payload identity and safe extraction, using tiny local fixtures."""

import importlib.util
import io
import json
from pathlib import Path
import stat
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from release import digest, native_identity

spec = importlib.util.spec_from_file_location("linux_runtime", Path(__file__).with_name("linux-runtime.py"))
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


class RuntimeTests(unittest.TestCase):
    def test_package_comparison_rejects_changed_missing_and_extra_runtime_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            extracted = root / "extracted"
            extracted.mkdir()
            (extracted / "usr").mkdir(mode=0o755)
            binary = extracted / "aperture"
            binary.write_bytes(b"validated app")
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"source_sha": "a" * 40, "binary_sha256": digest(binary),
                "files": {"runtime/" + name: row for name, row in runtime.inventory(extracted).items()}}))
            self.assertEqual(runtime.verify_package(extracted, manifest)["status"], "pass")
            (extracted / "usr").chmod(0o700)
            self.assertEqual(runtime.verify_package(extracted, manifest)["extractor_private_directories"], 1)
            original_mode = binary.stat().st_mode
            binary.chmod(0o700)
            with self.assertRaisesRegex(ValueError, "aperture"):
                runtime.verify_package(extracted, manifest)
            binary.chmod(original_mode)
            binary.write_bytes(b"changed app")
            with self.assertRaisesRegex(ValueError, "differs from sealed"):
                runtime.verify_package(extracted, manifest)
            binary.write_bytes(b"validated app")
            (extracted / "unexpected.so").write_bytes(b"extra")
            with self.assertRaisesRegex(ValueError, "unexpected.so"):
                runtime.verify_package(extracted, manifest)
            (extracted / "unexpected.so").unlink()
            binary.unlink()
            with self.assertRaisesRegex(ValueError, "aperture"):
                runtime.verify_package(extracted, manifest)

    def fixture(self, root):
        app, tools = root / "app", root / "tools"
        for name in ("AppRun", "AppRun.wrapped", "usr/bin/aperture", "usr/bin/vips", "usr/bin/vipsheader", "usr/bin/tiffinfo"):
            path = app / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(name.encode())
            path.chmod(0o755)
        for name in ("appimagetool", "mksquashfs", "desktop-file-validate"):
            path = tools / "usr/bin" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(name.encode())
            path.chmod(0o755)
        metadata = app / "usr/share/aperture"
        metadata.mkdir(parents=True)
        dependencies = {"baseline": "12", "libvips": "8.18.0"}
        info = {"source_sha": "a" * 40, "platform": "linux", "native_dependencies": dependencies}
        (metadata / "BUILDINFO.json").write_text(json.dumps(info))
        (metadata / "LICENSE.txt").write_text("license")
        (metadata / "THIRD-PARTY-NOTICES.txt").write_text("notices")
        (app / "QA-DRIVER-PROVENANCE.json").write_text("driver only")
        (app / "usr/bin/WebKitWebDriver").write_text("driver only")
        (app / "aperture-link").symlink_to("usr/bin/aperture")
        (app / "usr/lib").mkdir()
        (app / "usr/lib/libvips.so.42").write_bytes(b"validated native library")
        expected = runtime.identities("b" * 40, digest(app / "usr/bin/aperture"), native_identity(dependencies))
        archive = root / "development.tar.gz"
        (root / "type2-runtime").write_bytes(b"\x7fELFsealed runtime")
        info["source_sha"] = expected["source_sha"]
        with tarfile.open(archive, "w:gz") as output:
            payload = {"linux/bin/aperture": (app / "usr/bin/aperture").read_bytes(),
                       "linux/BUILDINFO-linux.json": json.dumps(info).encode(),
                       "linux/LICENSE.txt": b"new license", "linux/THIRD-PARTY-NOTICES-linux.txt": b"new notices"}
            for name, content in payload.items():
                member = tarfile.TarInfo(name)
                member.size = len(content)
                output.addfile(member, io.BytesIO(content))
        return app, tools, archive, expected, runtime.inventory_sha256(runtime.inventory(app))

    def test_round_trip_preserves_binary_wrappers_and_records_removed_qa(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app, tools, development, expected, pin = self.fixture(root)
            output = root / "runtime.tar.gz"
            receipt = runtime.seal(app, tools, development, output, expected, root / "type2-runtime", pin)
            destination = root / "staged"
            result = runtime.stage(output, destination, receipt["archive_sha256"], expected)
            self.assertEqual(result["files"], receipt["files"])
            self.assertIn("usr/bin/WebKitWebDriver", receipt["removed"])
            self.assertEqual((destination / "runtime/AppRun").read_bytes(), (app / "AppRun").read_bytes())
            self.assertEqual(digest(destination / "runtime/usr/bin/aperture"), expected["binary_sha256"])
            self.assertEqual((destination / "runtime/aperture-link").read_bytes(), b"usr/bin/aperture")
            self.assertTrue((app / "usr/bin/WebKitWebDriver").exists())
            self.assertEqual(json.loads((destination / "runtime/usr/share/aperture/BUILDINFO.json").read_text())["source_sha"], "b" * 40)

    def test_wrong_source_binary_native_or_archive_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app, tools, development, expected, pin = self.fixture(root)
            output = root / "runtime.tar.gz"
            receipt = runtime.seal(app, tools, development, output, expected, root / "type2-runtime", pin)
            for key in expected:
                bad = {**expected, key: "c" * len(expected[key])}
                with self.subTest(key=key), self.assertRaisesRegex(ValueError, "identity differs"):
                    runtime.stage(output, root / "staged", receipt["archive_sha256"], bad)
                self.assertFalse((root / "staged").exists())
            with self.assertRaisesRegex(ValueError, "SHA256 differs"):
                runtime.stage(output, root / "staged", "0" * 64, expected)
            for key in ("source_sha", "binary_sha256", "native_dependencies_sha256"):
                with self.subTest(seal=key), self.assertRaises(ValueError):
                    runtime.seal(app, tools, development, root / "bad.tar.gz", {**expected, key: "c" * len(expected[key])}, root / "type2-runtime", pin)

    def test_changed_library_or_copy_race_is_rejected_before_metadata_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app, tools, development, expected, pin = self.fixture(root)
            library = app / "usr/lib/libvips.so.42"
            original = library.read_bytes()
            library.write_bytes(b"changed .so with unchanged BUILDINFO")
            with self.assertRaisesRegex(ValueError, "Validated runtime inventory differs"):
                runtime.seal(app, tools, development, root / "bad.tar.gz", expected, root / "type2-runtime", pin)
            library.write_bytes(original)
            original_copy = runtime.shutil.copytree
            def changed_copy(source, destination, *args, **kwargs):
                result = original_copy(source, destination, *args, **kwargs)
                if source == app:
                    (destination / "usr/lib/libvips.so.42").write_bytes(b"changed during copying")
                return result
            with patch.object(runtime.shutil, "copytree", side_effect=changed_copy), \
                    self.assertRaisesRegex(ValueError, "Copied runtime inventory differs"):
                runtime.seal(app, tools, development, root / "bad.tar.gz", expected, root / "type2-runtime", pin)
            self.assertFalse((root / "bad.tar.gz").exists())

    def test_private_extraction_directories_are_normalized_without_file_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app, tools, development, expected, _ = self.fixture(root)
            for path in [app, *app.rglob("*")]:
                if path.is_dir() and not path.is_symlink():
                    path.chmod(0o700)
            before = runtime.inventory(app)
            pin = runtime.inventory_sha256(before)
            output = root / "runtime.tar.gz"
            receipt = runtime.seal(app, tools, development, output, expected, root / "type2-runtime", pin)
            runtime.stage(output, root / "staged", receipt["archive_sha256"], expected)
            staged = root / "staged/runtime"
            self.assertIn(".", receipt["directory_mode_normalization"]["paths"])
            self.assertIn("usr/lib", receipt["directory_mode_normalization"]["paths"])
            for path in [staged, *staged.rglob("*")]:
                if path.is_dir() and not path.is_symlink():
                    self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o755)
            for name in ("AppRun", "AppRun.wrapped", "usr/bin/aperture", "usr/lib/libvips.so.42", "aperture-link"):
                self.assertEqual(runtime.inventory(staged)[name], before[name])
            self.assertEqual(stat.S_IMODE(app.stat().st_mode), 0o700)

    def test_nested_archive_extracts_in_original_order(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app, tools, development, expected, pin = self.fixture(root)
            nested = app / "usr/share/aperture/nested/deeper/payload"
            nested.parent.mkdir(parents=True)
            nested.write_bytes(b"nested payload")
            pin = runtime.inventory_sha256(runtime.inventory(app))
            output = root / "runtime.tar.gz"
            runtime.seal(app, tools, development, output, expected, root / "type2-runtime", pin)
            with tarfile.open(output) as archive:
                members = [(member, archive.extractfile(member).read() if member.isfile() else None) for member in archive.getmembers()]
            # Deep files and links intentionally precede their directories and targets.
            members.reverse()
            reordered = root / "reordered.tar.gz"
            with tarfile.open(reordered, "w:gz") as archive:
                for member, content in members:
                    archive.addfile(member, io.BytesIO(content) if content is not None else None)
            calls = []
            original = tarfile.TarFile.extractfile
            def observe(archive, member):
                calls.append(member.name if isinstance(member, tarfile.TarInfo) else member)
                return original(archive, member)
            with patch.object(tarfile.TarFile, "extractfile", observe):
                runtime.stage(reordered, root / "staged", digest(reordered), expected)
            self.assertEqual(calls, ["manifest.json"] + [member.name for member, _ in members if member.isfile()])
            self.assertEqual((root / "staged/runtime/usr/share/aperture/nested/deeper/payload").read_bytes(), b"nested payload")
            self.assertEqual(digest(root / "staged/runtime/aperture-link"), expected["binary_sha256"])

    def test_unsafe_links_traversal_hardlinks_and_manifest_corruption_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app, tools, development, expected, pin = self.fixture(root)
            (app / "escape").symlink_to("../outside")
            (root / "outside").write_text("outside")
            with self.assertRaisesRegex(ValueError, "Escaping symlink"):
                runtime.seal(app, tools, development, root / "bad.tar.gz", expected, root / "type2-runtime", pin)
            (app / "escape").unlink()
            output = root / "runtime.tar.gz"
            runtime.seal(app, tools, development, output, expected, root / "type2-runtime", pin)
            with tarfile.open(output) as archive:
                originals = [(member, archive.extractfile(member).read() if member.isfile() else None) for member in archive.getmembers()]
            for attack in ("traversal", "symlink", "symlink-parent", "hardlink", "changed-file", "duplicate"):
                corrupt = root / (attack + ".tar.gz")
                with tarfile.open(corrupt, "w:gz") as archive:
                    for member, content in originals:
                        if attack == "changed-file" and member.name == "runtime/usr/bin/aperture":
                            content = b"X" * member.size
                        archive.addfile(member, io.BytesIO(content) if content is not None else None)
                    extra = tarfile.TarInfo("../escape" if attack == "traversal" else "runtime/extra")
                    if attack in ("symlink", "symlink-parent"):
                        extra.type, extra.linkname = tarfile.SYMTYPE, "../../../escape" if attack == "symlink" else "usr"
                    elif attack == "hardlink":
                        extra.type, extra.linkname = tarfile.LNKTYPE, "runtime/usr/bin/aperture"
                    elif attack == "duplicate":
                        extra.name = "runtime/AppRun"
                    archive.addfile(extra)
                    if attack == "symlink-parent":
                        archive.addfile(tarfile.TarInfo("runtime/extra/written-through-link"))
                with self.subTest(attack=attack), self.assertRaises(ValueError):
                    runtime.stage(corrupt, root / "staged", digest(corrupt), expected)
                self.assertFalse((root / "staged").exists())


if __name__ == "__main__":
    unittest.main()
