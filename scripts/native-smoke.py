#!/usr/bin/env python3
"""Exercise the real Linux Tauri app through WebDriver and its native chooser.

Run tauri-driver first, then pass a built application binary. Requires an
active Hyprland session, ydotool daemon, and grim. No Python packages required.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("images", type=Path, nargs="+")
    parser.add_argument("--driver-url", default="http://127.0.0.1:4444")
    parser.add_argument(
        "--output", type=Path, default=Path("test-data/native-validation.json")
    )
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--expect-error", type=Path, action="append", default=[])
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    error_paths = {path.resolve() for path in args.expect_error}
    env = {
        **os.environ,
        "YDOTOOL_SOCKET": str(Path(os.environ["XDG_RUNTIME_DIR"]) / ".ydotool_socket"),
    }

    def request(method, route, body=None):
        data = None if body is None else json.dumps(body).encode()
        req = Request(
            args.driver_url + route,
            data=data,
            method=method,
            headers={"Content-Type": "application/json"},
        )
        with urlopen(req, timeout=30) as response:
            result = json.load(response)
        value = result.get("value")
        if isinstance(value, dict) and isinstance(value.get("error"), str):
            raise RuntimeError(value)
        return value

    started = time.monotonic()
    session = request(
        "POST",
        "/session",
        {
            "capabilities": {
                "alwaysMatch": {
                    "browserName": "wry",
                    "tauri:options": {"application": str(args.binary.resolve())},
                }
            }
        },
    )
    sid = session["sessionId"]
    route = f"/session/{sid}"

    def js(script):
        return request("POST", route + "/execute/sync", {"script": script, "args": []})

    def state():
        return js("""
          const canvas = document.querySelector('#viewer canvas');
          let pixel = null;
          try { if(canvas) pixel = Array.from(canvas.getContext('2d')?.getImageData(canvas.width/2, canvas.height/2, 1, 1).data || []); } catch(e) {}
          return {
            filename: document.querySelector('#filename')?.textContent,
            status: document.querySelector('#status')?.textContent,
            phase: document.querySelector('#viewer')?.dataset.phase,
            dimensions: document.querySelector('#dimensions')?.textContent,
            error: document.querySelector('#status')?.classList.contains('error'),
            fitDisabled: document.querySelector('#fit-button')?.disabled,
            cancelHidden: document.querySelector('#cancel-button')?.hidden,
            saveDisabled: document.querySelector('#save-button')?.disabled,
            pixel
          };
        """)

    def keys(*values):
        subprocess.run(
            ["ydotool", "key", *values], env=env, check=True, stdout=subprocess.DEVNULL
        )

    def open_native(path):
        js("document.querySelector('#open-button').click(); return true;")
        dialog_by = time.monotonic() + 10
        while True:
            active = json.loads(
                subprocess.check_output(["hyprctl", "-j", "activewindow"])
            )
            if (
                active.get("class", "").lower() in {"aperture", "com.neuroqp.aperture"}
                and active.get("title") == "Open File"
            ):
                break
            if time.monotonic() > dialog_by:
                raise TimeoutError("Native file chooser did not gain focus")
            time.sleep(0.15)
        keys("29:1", "38:1", "38:0", "29:0")  # Ctrl+L focuses GTK's location entry.
        time.sleep(0.5)
        previous_clipboard = subprocess.run(["wl-paste", "--no-newline"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        subprocess.run(["wl-copy"], input=str(path.absolute()).encode(), check=True)
        keys("29:1", "30:1", "30:0", "29:0")  # Select the location field's contents.
        keys("29:1", "47:1", "47:0", "29:0")  # Paste the exact Unicode path.
        time.sleep(1)
        selected_at = time.monotonic()
        keys("28:1", "28:0")
        time.sleep(0.5)
        active = json.loads(subprocess.check_output(["hyprctl", "-j", "activewindow"]))
        if active.get("class", "").lower() == "aperture" and active.get("title") == "Open File":
            keys("28:1", "28:0")
        if previous_clipboard.returncode == 0:
            subprocess.run(["wl-copy"], input=previous_clipboard.stdout, check=True)
        return selected_at

    records = []
    try:
        ready_by = time.monotonic() + 30
        while not js("return Boolean(document.querySelector('#open-button'));"):
            if time.monotonic() > ready_by:
                raise TimeoutError("Application UI did not appear")
            time.sleep(0.1)
        startup = time.monotonic() - started
        initial = state()
        assert initial["saveDisabled"], "Save must be disabled"
        windows = json.loads(subprocess.check_output(["hyprctl", "-j", "clients"]))
        app_pid = next(
            w["pid"]
            for w in windows
            if w.get("class", "").lower() in {"aperture", "com.neuroqp.aperture"}
            and w.get("title") != "Open File"
        )

        def tree_rss_mb():
            rows = [
                list(map(int, line.split()))
                for line in subprocess.check_output(
                    ["ps", "-eo", "pid=,ppid=,rss="], text=True
                ).splitlines()
            ]
            children = {app_pid}
            while True:
                expanded = children | {
                    pid for pid, parent, _ in rows if parent in children
                }
                if expanded == children:
                    return sum(rss for pid, _, rss in rows if pid in children) / 1024
                children = expanded

        for image in args.images:
            expect_error = image.resolve() in error_paths
            expected_names = {image.name, image.resolve().name}
            source_stat = image.stat()
            picker_start = time.monotonic()
            start = open_native(image)
            first_pixels = None
            peak_rss = tree_rss_mb()
            last = {}
            while time.monotonic() - start < args.timeout:
                last = state()
                peak_rss = max(peak_rss, tree_rss_mb())
                if (
                    any(name in (last.get("filename") or "") for name in expected_names)
                    and last.get("pixel", [0, 0, 0, 0])
                    and last["pixel"][3] > 0
                ):
                    first_pixels = first_pixels or time.monotonic() - start
                if any(
                    name in (last.get("filename") or "") for name in expected_names
                ) and (
                    last.get("phase") in {"error", "cancelled"}
                    or last.get("phase") == "ready"
                    and not last.get("fitDisabled")
                ):
                    break
                time.sleep(0.15)
            elapsed = time.monotonic() - start
            record = {
                "image": str(image),
                "expected": "error" if expect_error else "ready",
                "picker_seconds": round(start - picker_start, 3),
                "elapsed_seconds": round(elapsed, 3),
                "sampled_process_tree_peak_rss_mb": round(peak_rss, 1),
                "first_pixels_seconds": (
                    None if first_pixels is None else round(first_pixels, 3)
                ),
                "state": last,
            }
            after_stat = image.stat()
            record["source_unchanged"] = (
                source_stat.st_size,
                source_stat.st_mtime_ns,
            ) == (after_stat.st_size, after_stat.st_mtime_ns)
            if not last.get("fitDisabled"):
                js("document.querySelector('#zoom-in-button').click(); return true;")
                time.sleep(0.25)
                js("document.querySelector('#fit-button').click(); return true;")
                time.sleep(0.3)
            windows = json.loads(subprocess.check_output(["hyprctl", "-j", "clients"]))
            window = next(
                (
                    w
                    for w in windows
                    if w.get("class", "").lower()
                    in {"aperture", "com.neuroqp.aperture"}
                    and w.get("title") != "Open File"
                ),
                None,
            )
            if window and window.get("mapped"):
                x, y = window["at"]
                width, height = window["size"]
                screenshot = args.output.parent / (image.name + ".native.png")
                subprocess.run(
                    ["grim", "-g", f"{x},{y} {width}x{height}", str(screenshot)],
                    check=True,
                )
                record["screenshot"] = str(screenshot)
                record["window"] = window
            records.append(record)
            args.output.write_text(
                json.dumps(
                    {
                        "startup_ui_seconds": round(startup, 3),
                        "initial": initial,
                        "images": records,
                    },
                    indent=2,
                )
                + "\n"
            )
            print(json.dumps(record), flush=True)
            assert any(
                name in (last.get("filename") or "") for name in expected_names
            ), f"Image was not selected: {last}"
            assert last.get("cancelHidden"), f"Load timed out: {last}"
            if expect_error:
                assert last.get("phase") == "error" or last.get(
                    "error"
                ), f"Expected an error: {last}"
            else:
                assert last.get("phase") == "ready" and not last.get(
                    "error"
                ), f"Image load failed: {last}"
            assert expect_error or not any(
                word in (last.get("status") or "").lower()
                for word in ["error", "failed", "unsupported", "unable", "cancelled"]
            ), f"Image load failed: {last}"
            assert last[
                "saveDisabled"
            ], "Save became enabled without a supported operation"
            assert record[
                "source_unchanged"
            ], "Source size or modification timestamp changed"
    finally:
        request("DELETE", route)


if __name__ == "__main__":
    main()
