"""Geometry boundaries and native capture ownership; not desktop evidence."""

import importlib.util
import os
from pathlib import Path
import unittest


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


gnome = module("capture-gnome")
qemu = module("capture-qemu")


class NativeCaptureTests(unittest.TestCase):
    def test_crop_uses_measured_top_panel_and_rejects_non_full_width_or_invalid_bounds(self):
        pixels = b"".join(bytes([y]) * 6 * 3 for y in range(8))
        frame = b"P6\n6 8\n255\n" + pixels
        for panel in (0, 1, 3):
            self.assertEqual(gnome.crop(frame, 6, 8 - panel),
                             f"P6\n6 {8 - panel}\n255\n".encode() + pixels[panel * 6 * 3:])
        for width, height in ((5, 7), (7, 7), (6, 9), (6, 0), (6, -1)):
            with self.assertRaisesRegex(ValueError, "maximized frame"):
                gnome.crop(frame, width, height)
        with self.assertRaisesRegex(ValueError, "maximized frame"):
            gnome.crop(frame[:-1], 6, 7)

    def test_capture_bridge_rejects_a_real_owned_non_qemu_process(self):
        with self.assertRaisesRegex(ValueError, "QEMU identity"):
            qemu.process(os.getpid(), Path.cwd())


if __name__ == "__main__":
    unittest.main()
