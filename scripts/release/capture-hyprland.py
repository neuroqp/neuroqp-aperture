#!/usr/bin/env python3
"""Prepared Omarchy capture: activate and capture only the owned Aperture window."""
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    pid, output = int(sys.argv[1]), sys.argv[2]
    process = Path("/proc", str(pid))
    if process.stat().st_uid != os.getuid() or (process / "exe").resolve().name != "aperture":
        raise ValueError("Capture requires the owned native Aperture process")
    windows = json.loads(subprocess.check_output(["/usr/bin/hyprctl", "clients", "-j"], text=True, timeout=3))
    owned = [window for window in windows if window["pid"] == pid and window["mapped"] and not window["hidden"] and not window["xwayland"]]
    if len(owned) != 1:
        raise ValueError("Exactly one visible owned native Wayland window required")
    subprocess.run(["/usr/bin/hyprctl", "eval", f"for _, w in ipairs(hl.get_windows()) do if w.pid == {pid} then hl.dispatch(hl.dsp.focus({{window=w}})) end end"], check=True, capture_output=True, timeout=3)
    active = json.loads(subprocess.check_output(["/usr/bin/hyprctl", "activewindow", "-j"], text=True, timeout=3))
    if active.get("pid") != pid or active.get("address") != owned[0]["address"]:
        raise ValueError("Owned native window did not become foreground")
    x, y = active["at"]
    width, height = active["size"]
    subprocess.run(["/usr/bin/grim", "-s", "1", "-g", f"{x},{y} {width}x{height}", "-t", "ppm", output], check=True, timeout=5)
    Path(output + ".geometry.json").write_text(json.dumps(active, indent=2) + "\n")


if __name__ == "__main__":
    main()
