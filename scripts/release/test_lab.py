"""Offline lab guards using disposable files/tools; no VM or real image mutation."""

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import lab


class LabTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.addCleanup(patch.stopall)
        patch.object(lab, "inactive").start()

    def file(self, name, content=b"owned fixture"):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    def receipt(self, name, data):
        path = self.root / name
        lab.write(path, data)
        return path

    def recovery(self, disk):
        return self.receipt("recovery.json", {"schema": 1, "licensed": True,
                            "guest_shutdown": "graceful", "qemu_exited": True,
                            "full_cold_shutdown": True, "suspend_disk_event_before_shutdown": False,
                            "qemu_exit_code": 0, "forced_termination": False,
                            "disk_identity": lab.identity(disk)})

    def test_hybrid_or_unconfirmed_cold_shutdown_cannot_compact_or_freeze(self):
        disk = self.file(lab.DISK)
        receipt = self.recovery(disk)
        original = lab.read(receipt)
        for changed in ({"suspend_disk_event_before_shutdown": True}, {"full_cold_shutdown": False},
                        {"qemu_exit_code": 1}, {"forced_termination": True}):
            receipt.write_text(json.dumps({**original, **changed}))
            for action in (lab.compact, lab.freeze_base):
                with self.assertRaisesRegex(ValueError, "full cold"):
                    action(self.root, receipt)

    def test_inventory_counts_hardlinks_once_and_does_not_follow_symlinks(self):
        disk = self.file(lab.DISK)
        alias = self.root / "alias.qcow2"
        alias.hardlink_to(disk)
        (self.root / "escape").symlink_to(self.root.parent)
        report = lab.inventory(self.root)
        self.assertEqual(report["allocated_unique_inode_bytes"], lab.identity(disk)["allocated"])
        self.assertEqual(len(report["images"]), 2)
        self.assertIn("blocked", report["readiness"]["windows11"])

    def test_cleanup_removes_both_obsolete_iso_aliases_preserves_current_disk(self):
        current = self.file(lab.DISK)
        self.file(lab.OBSOLETE[0])
        iso = self.file(lab.OBSOLETE[1])
        alias = self.root / lab.OBSOLETE[2]
        alias.parent.mkdir(parents=True)
        alias.hardlink_to(iso)
        plan = lab.cleanup_plan(self.root)
        expected = lab.identity(iso)["allocated"] + lab.identity(self.root / lab.OBSOLETE[0])["allocated"]
        self.assertEqual(plan["estimated_unique_inode_reduction_bytes"], expected)
        manifest = self.receipt("manifest.json", plan)
        result = lab.cleanup(self.root, manifest)
        self.assertEqual(result["allocated_removed"], expected)
        self.assertTrue(current.exists())
        self.assertTrue(all(not (self.root / name).exists() for name in lab.OBSOLETE))

    def test_external_hardlink_is_not_claimed_as_reclaimed(self):
        iso = self.file(lab.OBSOLETE[1])
        with tempfile.TemporaryDirectory(dir=self.root.parent) as other:
            (Path(other) / "outside.iso").hardlink_to(iso)
            plan = lab.cleanup_plan(self.root)
            self.assertEqual(plan["estimated_unique_inode_reduction_bytes"], 0)

    def test_cleanup_checks_all_targets_before_unlink(self):
        original = self.file(lab.OBSOLETE[0])
        iso = self.file(lab.OBSOLETE[1])
        plan = lab.cleanup_plan(self.root)
        iso.write_bytes(b"changed since manifest")
        with self.assertRaisesRegex(ValueError, "changed"):
            lab.cleanup(self.root, self.receipt("manifest.json", plan))
        self.assertTrue(original.exists())

    def test_protected_disk_and_symlink_cleanup_refused(self):
        disk = self.file(lab.DISK)
        plan = lab.cleanup_plan(self.root)
        plan["files"] = [{"path": lab.DISK, "identity": lab.identity(disk)}]
        with self.assertRaisesRegex(ValueError, "protected"):
            lab.cleanup(self.root, self.receipt("manifest.json", plan))
        (self.root / lab.OBSOLETE[0]).parent.mkdir(parents=True)
        (self.root / lab.OBSOLETE[0]).symlink_to(disk)
        with self.assertRaisesRegex(ValueError, "Symlink"):
            lab.cleanup_plan(self.root)

    def test_install_iso_requires_current_disk_only_boot_receipt(self):
        disk = self.file(lab.DISK)
        self.file(lab.INSTALL_ISO)
        self.assertNotIn(lab.INSTALL_ISO, [row["path"] for row in lab.cleanup_plan(self.root)["files"]])
        receipt = self.receipt("boot.json", {"schema": 1, "status": "pass",
                               "install_media_attached": False, "disk_identity": lab.identity(disk)})
        self.assertIn(lab.INSTALL_ISO, [row["path"] for row in lab.cleanup_plan(self.root, receipt)["files"]])
        disk.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "stale"):
            lab.cleanup_plan(self.root, receipt)

    def test_sigterm_and_stale_recovery_refused(self):
        disk = self.file(lab.DISK)
        receipt = self.recovery(disk)
        data = lab.read(receipt)
        data["guest_shutdown"] = "SIGTERM"
        receipt.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "SIGTERM"):
            lab.freeze_base(self.root, receipt)
        data["guest_shutdown"] = "graceful"
        receipt.write_text(json.dumps(data))
        disk.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "stale"):
            lab.freeze_base(self.root, receipt)

    def test_freeze_refuses_dirty_or_backed_disks(self):
        disk = self.file(lab.DISK)
        receipt = self.recovery(disk)
        for info in [{"dirty-flag": True}, {"backing-filename": "other.qcow2"}]:
            with patch.object(lab, "qemu", return_value=json.dumps(info)):
                with self.assertRaisesRegex(ValueError, "standalone"):
                    lab.freeze_base(self.root, receipt)

    def test_registered_base_is_readonly_and_modified_base_cannot_create_overlay(self):
        disk = self.file(lab.DISK)
        with patch.object(lab, "qemu", return_value="{}"):
            lab.freeze_base(self.root, self.recovery(disk))
        self.assertFalse(disk.stat().st_mode & 0o222)
        self.assertIn("verified immutable", lab.inventory(self.root)["readiness"]["windows11"])
        disk.chmod(0o600)
        with self.assertRaisesRegex(ValueError, "immutable"):
            lab.overlay(self.root, "run-1")

    def test_freeze_refuses_existing_child_backing_chain(self):
        disk = self.file(lab.DISK)
        self.file("old-run.qcow2")
        with patch.object(lab, "qemu", side_effect=["{}", '{"backing-filename":"windows11.qcow2"}']):
            with self.assertRaisesRegex(ValueError, "backing chains"):
                lab.freeze_base(self.root, self.recovery(disk))

    def test_compact_comparison_failure_keeps_original(self):
        disk = self.file(lab.DISK, b"original disk")
        before = lab.identity(disk)
        receipt = self.recovery(disk)

        def qemu(root, args, **kwargs):
            if args[0] == "info":
                return "{}"
            if args[0] == "convert":
                Path(args[-1]).write_bytes(b"temporary output")
                return ""
            raise ValueError("comparison failed")

        with patch.object(lab, "qemu", side_effect=qemu), patch.object(lab.shutil, "disk_usage") as usage:
            usage.return_value.free = 100 * lab.GIB
            with self.assertRaisesRegex(ValueError, "comparison"):
                lab.compact(self.root, receipt)
        self.assertEqual(lab.identity(disk), before)
        self.assertTrue((self.root / (lab.DISK + ".compact.tmp")).exists())

    def test_compaction_refuses_low_free_space_before_convert(self):
        disk = self.file(lab.DISK)
        with patch.object(lab, "qemu", return_value="{}") as qemu, patch.object(lab.shutil, "disk_usage") as usage:
            usage.return_value.free = 0
            with self.assertRaisesRegex(ValueError, "4 GiB"):
                lab.compact(self.root, self.recovery(disk))
            self.assertTrue(all(call.args[1][0] == "info" for call in qemu.call_args_list))

    def test_compaction_comparison_overrun_preserves_original(self):
        disk = self.file(lab.DISK, b"original disk")
        original = lab.identity(disk)
        def qemu(root, args, **kwargs):
            if args[0] == "convert":
                Path(args[-1]).write_bytes(b"unverified temporary")
            return "{}" if args[0] == "info" else ""
        with patch.object(lab, "qemu", side_effect=qemu), patch.object(lab.shutil, "disk_usage") as usage, \
                patch.object(lab.time, "monotonic", side_effect=[0, 0, 0, 1799.9, 1801]), \
                patch.object(lab.os, "replace") as replace:
            usage.return_value.free = 100 * lab.GIB
            with self.assertRaisesRegex(ValueError, "deadline reached after comparison"):
                lab.compact(self.root, self.recovery(disk))
            replace.assert_not_called()
        self.assertEqual(lab.identity(disk), original)
        self.assertTrue((self.root / (lab.DISK + ".compact.tmp")).exists())

    def test_compaction_deadline_keeps_original_and_never_starts_compare(self):
        disk = self.file(lab.DISK)
        original = lab.identity(disk)

        def qemu(root, args, **kwargs):
            if args[0] == "info":
                return "{}"
            self.assertEqual(args[0], "convert")
            Path(args[-1]).write_bytes(b"retained output")
            return ""

        with patch.object(lab, "qemu", side_effect=qemu), patch.object(lab.shutil, "disk_usage") as usage, \
                patch.object(lab.time, "monotonic", side_effect=[0, 0, 1801]):
            usage.return_value.free = 100 * lab.GIB
            with self.assertRaisesRegex(ValueError, "deadline"):
                lab.compact(self.root, self.recovery(disk))
        self.assertEqual(lab.identity(disk), original)
        self.assertTrue((self.root / (lab.DISK + ".compact.tmp")).exists())

    def test_existing_tool_wrapper_uses_prepared_work_mountpoint(self):
        disk = self.file(lab.DISK)
        self.file("environments/debian12/rootfs/usr/bin/qemu-img")
        (self.root / "environments/debian12/rootfs/work").mkdir()
        with patch.object(lab.shutil, "which", side_effect=lambda name: "/usr/bin/bwrap" if name == "bwrap" else None), \
                patch.object(lab.subprocess, "Popen") as run:
            run.return_value.communicate.return_value = ("{}", None)
            run.return_value.returncode = 0
            lab.qemu(self.root, ["info", "--output=json", disk])
        argv = run.call_args.args[0]
        self.assertIn("/work", argv)
        self.assertIn("/work/" + lab.DISK, argv)
        self.assertNotIn("/lab", argv)
        self.assertTrue(run.call_args.kwargs["start_new_session"])

    def test_tool_timeout_stops_child_even_after_parent_exits_and_pipe_closes(self):
        pidfile = self.root / "child.pid"
        child = ("import os,signal,time;from pathlib import Path;"
                 "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                 f"Path({str(pidfile)!r}).write_text(str(os.getpid()));time.sleep(60)")
        parent = self.file("parent.py", (
            "import subprocess,sys,time\n"
            f"subprocess.Popen([sys.executable,'-c',{child!r}],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n"
            "time.sleep(60)\n").encode())
        try:
            with patch.object(lab.shutil, "which", return_value=sys.executable):
                with self.assertRaises(subprocess.TimeoutExpired):
                    lab.qemu(self.root, [parent], timeout=1)
            self.assertTrue(pidfile.exists(), "child must be ready before the timeout")
            process = Path("/proc") / pidfile.read_text()
            for _ in range(50):
                if not process.exists() or not (process / "cmdline").read_bytes():
                    break
                time.sleep(0.02)
            self.assertTrue(not process.exists() or not (process / "cmdline").read_bytes())
        finally:
            if pidfile.exists():
                try:
                    os.kill(int(pidfile.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass


if __name__ == "__main__":
    unittest.main()
