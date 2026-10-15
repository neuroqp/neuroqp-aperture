#!/usr/bin/env python3
"""Focused checks for the frozen packaging handoff and archive trust boundary."""

import copy
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("prepared", Path(__file__).with_name("prepared-inputs.py"))
prepared = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepared)


class PreparedInputsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.manifest = {"schema": 1, "stage": "A-packaging", "status": "pass", "forced_kill": False,
                         "platform": "linux", "source_sha": "a" * 40, "validation_run_id": "123",
                         "packaging_run_id": "456", "binary_sha256": "b" * 64,
                         "native_dependencies_sha256": "c" * 64, "archive_sha256": "d" * 64,
                         "tool_archive_sha256": "e" * 64, "development_archive_sha256": "f" * 64,
                         "source_plan_sha256": "0" * 64, "verifier_archive_sha256": "1" * 64}
        self.material = {"asset": "Aperture-Corresponding-Source.tar.xz",
                         "url": "https://github.com/neuroqp/neuroqp-aperture/releases/download/source/Aperture-Corresponding-Source.tar.xz",
                         "sha256": "2" * 64, "application_source_inventory_sha256": "3" * 64,
                         "application_source_sha": "a" * 40, "application_lockfiles_sha256": {"lock": "x"},
                         "native_dependencies_sha256": {"linux": "c" * 64, "windows": "4" * 64}}
        self.manifest["source_material"] = copy.deepcopy(self.material)
        self.path = self.root / "prepared-linux.json"
        self.path.write_text(json.dumps(self.manifest))
        self.frozen = {"source_sha": "a" * 40, "packaging_run_id": "456",
                       "validated_inputs": {"linux": {"manifest_sha256": prepared.digest(self.path),
                           "binary_sha256": "b" * 64, "native_dependencies_sha256": "c" * 64}},
                       "readiness": {"automated": [{"platform": "linux", "source_sha": "a" * 40,
                                                    "binary_sha256": "b" * 64}]}}

    def test_frozen_manifest_binds_bytes_identity_and_automated_executable(self):
        self.assertEqual(prepared.validate(self.path, self.frozen, "linux"), self.manifest)
        for change in (lambda f: f.update(packaging_run_id="789"),
                       lambda f: f.update(source_sha="9" * 40),
                       lambda f: f["validated_inputs"]["linux"].update(native_dependencies_sha256="9" * 64),
                       lambda f: f["readiness"]["automated"][0].update(binary_sha256="9" * 64)):
            frozen = copy.deepcopy(self.frozen)
            change(frozen)
            with self.assertRaises(ValueError):
                prepared.validate(self.path, frozen, "linux")
        self.path.write_text(json.dumps({**self.manifest, "validation_run_id": "789"}))
        with self.assertRaisesRegex(ValueError, "frozen reviewed bytes"):
            prepared.validate(self.path, self.frozen, "linux")

    def test_create_freezes_pnpm_links_into_self_contained_archive(self):
        (self.root / "store").mkdir()
        (self.root / "store/cli").write_text("pinned cli")
        (self.root / "node_modules").mkdir()
        (self.root / "node_modules/cli").symlink_to(self.root / "store/cli")
        (self.root / "dist").mkdir()
        (self.root / "dist/index.html").write_text("tested frontend")
        (self.root / "build/prepared").mkdir(parents=True)
        runtime = self.root / "build/prepared/linux-runtime.tar.gz"
        runtime.write_bytes(b"sealed runtime")
        executable = self.root / "app"
        executable.write_bytes(b"tested executable")
        current = {"schema": 1, "source_sha": "a" * 40, "build_inputs_sha256": "b" * 64}
        report = self.root / "automated.json"
        report.write_text(json.dumps({**current, "stage": "A", "platform": "linux", "status": "pass",
                                      "forced_kill": False, "binary_sha256": prepared.digest(executable)}))
        info = self.root / "BUILDINFO.json"
        info.write_text(json.dumps({"source_sha": "a" * 40, "platform": "linux", "lockfiles": {}, "native_dependencies": {"vips": "pinned"}}))
        args = SimpleNamespace(platform="linux", automated_report=report, buildinfo=info, source_sha="a" * 40,
                               executable=executable, validation_run_id="123", packaging_run_id="456",
                               development_archive=runtime, tool_archive=runtime, verifier_archive=runtime, source_plan=self.root / "plan.json",
                               source_plan_sha256="c" * 64, archive=self.root / "prepared.tar.gz", manifest=self.path)
        with patch.object(prepared, "ROOT", self.root), patch.object(prepared, "identity", return_value=current), patch.object(prepared, "lockfile_hashes", return_value={}), patch.object(prepared, "check_plan"), patch.object(prepared, "source_plan", return_value=self.material):
            prepared.create(args)
        (self.root / "store/cli").unlink()
        restored = self.root / "restored"
        prepared.extract(args.archive, restored, prepared.PATHS["linux"])
        self.assertEqual((restored / "node_modules/cli").read_text(), "pinned cli")
        self.assertFalse((restored / "node_modules/cli").is_symlink())
        self.assertEqual(prepared.read(self.path)["archive_sha256"], prepared.digest(args.archive))

    def test_new_frozen_source_receipt_can_cover_same_source_and_dependencies(self):
        plan = {**copy.deepcopy(self.material), "assembly_attempt": {"stage": "B", "new_receipt": True}}
        with patch.object(prepared, "source_plan", return_value=plan), patch.object(prepared, "lockfile_hashes", return_value={"lock": "x"}):
            # Original plan digest is provenance; only the frozen assembly receipt changes.
            prepared.check_plan(self.manifest, self.root / "final-plan.json", "1" * 64)
            for key, value in (("url", self.material["url"].replace("/source/", "/different/")),
                               ("sha256", "9" * 64), ("application_source_inventory_sha256", "9" * 64)):
                previous = plan[key]
                plan[key] = value
                with self.assertRaisesRegex(ValueError, "archive coverage"):
                    prepared.check_plan(self.manifest, self.root / "final-plan.json", "1" * 64)
                plan[key] = previous
            plan["native_dependencies_sha256"]["linux"] = "9" * 64
            with self.assertRaises(ValueError):
                prepared.check_plan(self.manifest, self.root / "final-plan.json", "1" * 64)

    def archive(self, name, kind=tarfile.REGTYPE, linkname=""):
        path = self.root / "input.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            member = tarfile.TarInfo(name)
            member.type, member.linkname = kind, linkname
            data = b"prepared"
            member.size = len(data) if kind == tarfile.REGTYPE else 0
            archive.addfile(member, io.BytesIO(data) if member.size else None)
        return path

    def test_archive_rejects_escape_links_and_unexpected_paths(self):
        for name, kind, target in (("../escape", tarfile.REGTYPE, ""),
                                   ("node_modules/..\\..\\escape", tarfile.REGTYPE, ""),
                                   ("node_modules/C:/escape", tarfile.REGTYPE, ""),
                                   ("node_modules/link", tarfile.SYMTYPE, "../../escape"),
                                   ("node_modules/link", tarfile.LNKTYPE, "node_modules/file"),
                                   ("src-tauri/src/main.rs", tarfile.REGTYPE, "")):
            with self.assertRaises(ValueError):
                prepared.extract(self.archive(name, kind, target), self.root / "restored", prepared.PATHS["linux"])
        path = self.archive("node_modules/file")
        restored = self.root / "restored"
        prepared.extract(path, restored, prepared.PATHS["linux"])
        self.assertEqual((restored / "node_modules/file").read_bytes(), b"prepared")
        with self.assertRaises(ValueError):
            prepared.extract(path, restored, prepared.PATHS["linux"])

    def test_signature_verifier_seals_closure_and_restores_only_frozen_archive(self):
        tools = self.root / "system"
        tools.mkdir()
        binary = tools / "minisign"
        binary.write_bytes(b"verifier")
        libraries = [tools / name for name in ("libsodium.so.23", "libc.so.6", "ld-linux-x86-64.so.2")]
        for path in libraries:
            path.write_text(path.name)
        archive = self.root / "verifier.tar.gz"
        ldd = "\n".join(str(path) + " (0x00)" for path in libraries)
        with patch.object(prepared.shutil, "which", return_value=str(binary)), patch.object(prepared.subprocess, "check_output", return_value=ldd), patch.object(prepared.subprocess, "run"):
            prepared.prepare_verifier(self.root / "sealed", archive)
        self.manifest["verifier_archive_sha256"] = prepared.digest(archive)
        self.path.write_text(json.dumps(self.manifest))
        self.frozen["validated_inputs"]["linux"]["manifest_sha256"] = prepared.digest(self.path)
        with patch.object(prepared, "preparation_run") as trusted, patch.object(prepared.subprocess, "run"):
            directory = prepared.restore_verifier(self.frozen, self.path, archive, self.root / "restored")
            trusted.assert_called_once_with("456")
        self.assertEqual((directory / "lib/libsodium.so.23").read_text(), "libsodium.so.23")
        self.assertIn('--library-path "$here/lib"', (directory / "minisign").read_text())
        archive.write_bytes(b"tampered")
        with patch.object(prepared, "preparation_run"), self.assertRaisesRegex(ValueError, "verifier archive differs"):
            prepared.restore_verifier(self.frozen, self.path, archive, self.root / "tampered")

    def test_preparation_run_requires_trusted_workflow_and_reviewed_main_ancestry(self):
        with patch.object(prepared, "trusted_run", return_value={"head_sha": "a" * 40}) as trust, patch.object(prepared.subprocess, "run") as command:
            prepared.preparation_run("456")
            trust.assert_called_once_with("456", "native-validation.yml")
            self.assertEqual(command.call_args.args[0], ["git", "merge-base", "--is-ancestor", "a" * 40, "origin/main"])

    def test_final_workflow_restores_and_wraps_without_dependency_resolution_or_app_build(self):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/package-build.yml").read_text()
        for forbidden in ("pnpm install", "apt-get", "choco install", "cargo build", "tauri build", "prepare-linux.sh", "prepare-windows.ps1"):
            self.assertNotIn(forbidden, workflow)
        self.assertNotIn("dtolnay/rust-toolchain", workflow)
        self.assertIn("prepare-windows-rust.ps1 -CheckOnly", workflow)
        self.assertIn("build/windows-packaging-rust", prepared.PATHS["windows"])
        self.assertIn("--no-binary-patching", workflow)
        self.assertIn("prepare-windows-packaging.ps1 -CheckOnly", workflow)
        self.assertIn('CARGO_NET_OFFLINE: "true"', workflow)
        self.assertIn("build/tools/package-linux.sh", workflow)
        self.assertIn("linux-runtime.py verify-package", workflow)
        self.assertNotIn("smoke-linux.sh", workflow)
        self.assertIn("Bundler changed the validated application executable", workflow)
        root = Path(__file__).resolve().parents[2]
        for name in ("release.yml", "promote-release.yml"):
            verification = (root / ".github/workflows" / name).read_text()
            self.assertNotIn("apt-get", verification)
            self.assertIn("restore-verifier", verification)


if __name__ == "__main__":
    unittest.main()
