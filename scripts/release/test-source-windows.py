#!/usr/bin/env python3
"""Run with python scripts/release/test-source-windows.py."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("source_windows", Path(__file__).with_name("source-windows.py"))
source_windows = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source_windows)


class SourceInventoryTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "source.tar").write_bytes(b"preserved source")
        self.archive = {"archive": "source.tar", "verified": True,
                        "sha256": hashlib.sha256(b"preserved source").hexdigest()}
        self.inventory = {
            "files": [{"path": "source.tar", "size": 16, "sha256": self.archive["sha256"]}],
            "sourceArchives": [{**self.archive, "nativeKey": "vips", "version": "1"}],
            "nativeVendor": {"sha256": "vendor"}, "nativeComponents": {"vips": "1"},
            "staticSources": {"gvdb": self.archive, "nsis": self.archive,
                              "nsisTauriUtils": {**self.archive, "licenseEvidenceSources": []},
                              "patchedCargoLock": "source.tar", "compilerAndRustStd": "source.tar",
                              "librsvgCargo": {"nonRegistrySources": [], "packages": []}},
            "patchApplication": [], "nativePeFiles": [{"sourceArchives": ["source.tar"]}],
        }
        self.save()

    def save(self):
        (self.root / "inventory.json").write_text(json.dumps(self.inventory), encoding="utf-8")

    def test_verified_bundle_and_matching_binary_identity(self):
        dependencies = self.root / "BUILDINFO.json"
        dependencies.write_text(json.dumps({"native_dependencies": {
            "nativeComponents": {"vips": "1"}, "libvips": {"sha256": "vendor"}}}))
        source_windows.verify(self.root, dependencies)
        dependencies.write_text(json.dumps({"nativeComponents": {"vips": "2"}, "libvips": {"sha256": "vendor"}}))
        with self.assertRaisesRegex(ValueError, "different native"):
            source_windows.verify(self.root, dependencies)

    def test_tampered_source_rejected(self):
        (self.root / "source.tar").write_bytes(b"corrupted source")
        with self.assertRaisesRegex(ValueError, "differs"):
            source_windows.verify(self.root)

    def test_source_hash_or_missing_static_archive_rejected(self):
        self.inventory["sourceArchives"][0]["sha256"] = "wrong"
        self.save()
        with self.assertRaisesRegex(ValueError, "recipe source"):
            source_windows.verify(self.root)
        self.inventory["sourceArchives"][0]["sha256"] = self.archive["sha256"]
        self.inventory["staticSources"]["gvdb"] = {**self.archive, "archive": "missing"}
        self.save()
        with self.assertRaisesRegex(ValueError, "gvdb"):
            source_windows.verify(self.root)

    def test_escape_and_missing_native_mapping_rejected(self):
        self.inventory["files"][0]["path"] = "../escape"
        self.save()
        with self.assertRaisesRegex(ValueError, "Unsafe"):
            source_windows.verify(self.root)
        self.inventory["files"][0]["path"] = "source.tar"
        self.inventory["nativePeFiles"][0]["sourceArchives"] = []
        self.save()
        with self.assertRaisesRegex(ValueError, "source mapping"):
            source_windows.verify(self.root)


if __name__ == "__main__":
    unittest.main()
