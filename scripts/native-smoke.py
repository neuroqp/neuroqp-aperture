#!/usr/bin/env python3
"""Exercise the real Linux Tauri app through WebDriver and its native chooser.

Run inside scripts/headless-session --driver 4444 and pass a built application
binary. Uses wl-clipboard and grim through compositor.py. No Python packages
required.
"""

import argparse
import json
from pathlib import Path
import subprocess
import time
from urllib.request import Request, urlopen

import compositor
from compositor import APP_ID


def region(window):
    return window["x"], window["y"], window["width"], window["height"]


def size(window):
    return [window["width"], window["height"]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("images", type=Path, nargs="+")
    parser.add_argument("--driver-url", help="defaults to the session driver; must match it")
    parser.add_argument(
        "--output", type=Path, default=Path("artifacts/native-smoke/native-validation.json")
    )
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--expect-error", type=Path, action="append", default=[])
    parser.add_argument(
        "--cancel-after-preview", type=Path, action="append", default=[]
    )
    parser.add_argument("--window-controls", action="store_true")
    parser.add_argument("--leave-open", action="store_true")
    args = parser.parse_args()
    args.driver_url = compositor.driver_url(args.driver_url)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    error_paths = {path.resolve() for path in args.expect_error}
    cancel_paths = {path.resolve() for path in args.cancel_after_preview}
    desktop = compositor.session()
    binary = compositor.application(args.binary)

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
        if (
            isinstance(value, dict)
            and isinstance(value.get("error"), str)
            and "message" in value
        ):
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
                    "tauri:options": {"application": str(binary)},
                }
            }
        },
    )
    sid = session["sessionId"]
    args.output.with_suffix(".session.json").write_text(
        json.dumps({"session_id": sid}) + "\n"
    )
    route = f"/session/{sid}"

    def js(script):
        return request("POST", route + "/execute/sync", {"script": script, "args": []})

    def click(element_id):
        return js(
            f"document.getElementById({json.dumps(element_id)}).click(); return true;"
        )

    def backend_state():
        return request(
            "POST",
            route + "/execute/async",
            {
                "script": "const done = arguments[arguments.length - 1]; window.__TAURI_INTERNALS__.invoke('image_state').then(done);",
                "args": [],
            },
        )

    def state():
        return js("""
          return {
            filename: document.querySelector('#filename')?.textContent,
            status: document.querySelector('#status')?.textContent,
            phase: document.querySelector('#viewer')?.dataset.phase,
            dimensions: document.querySelector('#dimensions')?.textContent,
            error: document.querySelector('#status')?.classList.contains('error'),
            fitDisabled: document.querySelector('#fit-button')?.disabled,
            cancelHidden: document.querySelector('#cancel-button')?.hidden,
            saveDisabled: document.querySelector('#save-button')?.disabled
          };
        """)

    def app_window(title=None):
        return next(
            (
                w
                for w in desktop.windows()
                if w["app_id"] == APP_ID
                and (w["title"] == title if title else w["title"] != "Open File")
            ),
            None,
        )

    def open_native(path):
        click("open-button")
        dialog_by = time.monotonic() + 10
        while not (dialog := app_window("Open File")):
            if time.monotonic() > dialog_by:
                raise TimeoutError("Native file chooser did not appear")
            time.sleep(0.15)
        desktop.focus(dialog)
        time.sleep(0.3)
        desktop.keys("ctrl+l")  # Focus GTK's location entry.
        time.sleep(0.5)
        previous_clipboard = desktop.clipboard()
        desktop.set_clipboard(str(path.absolute()).encode())
        desktop.keys("ctrl+a")  # Select the location field's contents.
        desktop.keys("ctrl+v")  # Paste the exact Unicode path.
        time.sleep(1)
        selected_at = time.monotonic()
        desktop.keys("Return")
        time.sleep(0.5)
        dialog = app_window("Open File")
        if dialog and dialog["focused"]:
            desktop.keys("Return")
        time.sleep(0.3)
        if dialog := app_window("Open File"):
            # Open button center observed in this machine's GTK chooser screenshots.
            desktop.click(dialog["x"] + dialog["width"] - 47, dialog["y"] + 23)
        if previous_clipboard is not None:
            desktop.set_clipboard(previous_clipboard)
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
        app_pid = app_window()["pid"]

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
            expect_cancel = image.resolve() in cancel_paths
            expected_names = {image.name, image.resolve().name}
            source_stat = image.stat()
            picker_start = time.monotonic()
            start = open_native(image)
            preview_ready = None
            preview_screenshot_seconds = None
            cancel_requested = False
            pan_zoom_seconds = None
            peak_rss = tree_rss_mb()
            last = {}
            while time.monotonic() - start < args.timeout:
                last = state()
                backend = backend_state()
                if backend and backend.get("previewReady"):
                    preview_ready = preview_ready or time.monotonic() - start
                if (
                    expect_cancel
                    and not cancel_requested
                    and last.get("phase") == "loading"
                    and not last.get("fitDisabled")
                ):
                    action_started = time.monotonic()
                    window = app_window("Aperture")
                    click("zoom-in-button")
                    point = js(
                        "const r=document.querySelector('#viewer').getBoundingClientRect(); return {x:Math.round(r.x+r.width/2), y:Math.round(r.y+r.height/2)};"
                    )
                    request(
                        "POST",
                        route + "/actions",
                        {
                            "actions": [
                                {
                                    "type": "pointer",
                                    "id": "mouse",
                                    "parameters": {"pointerType": "mouse"},
                                    "actions": [
                                        {
                                            "type": "pointerMove",
                                            "duration": 0,
                                            "origin": "viewport",
                                            "x": point["x"],
                                            "y": point["y"],
                                        },
                                        {"type": "pointerDown", "button": 0},
                                        {
                                            "type": "pointerMove",
                                            "duration": 250,
                                            "origin": "viewport",
                                            "x": point["x"] + 70,
                                            "y": point["y"] + 35,
                                        },
                                        {"type": "pointerUp", "button": 0},
                                    ],
                                }
                            ]
                        },
                    )
                    pan_zoom_seconds = time.monotonic() - action_started
                    time.sleep(0.3)
                    desktop.screenshot(
                        args.output.parent / "cancel-loading-zoom.native.png",
                        region(window),
                    )
                    preview_screenshot_seconds = time.monotonic() - start
                    click("cancel-button")
                    cancel_requested = True
                peak_rss = max(peak_rss, tree_rss_mb())
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
                "expected": (
                    "cancelled"
                    if expect_cancel
                    else "error" if expect_error else "ready"
                ),
                "backend_preview_ready_seconds": (
                    None if preview_ready is None else round(preview_ready, 3)
                ),
                "preview_loading_screenshot_seconds": (
                    None
                    if preview_screenshot_seconds is None
                    else round(preview_screenshot_seconds, 3)
                ),
                "picker_seconds": round(start - picker_start, 3),
                "pan_zoom_action_seconds": (
                    None if pan_zoom_seconds is None else round(pan_zoom_seconds, 3)
                ),
                "elapsed_seconds": round(elapsed, 3),
                "sampled_process_tree_peak_rss_mb": round(peak_rss, 1),
                "state": last,
            }
            after_stat = image.stat()
            record["source_unchanged"] = (
                source_stat.st_size,
                source_stat.st_mtime_ns,
            ) == (after_stat.st_size, after_stat.st_mtime_ns)
            if not last.get("fitDisabled"):
                click("zoom-in-button")
                time.sleep(0.25)
                click("fit-button")
                time.sleep(0.3)
            window = app_window()
            if window:
                screenshot = args.output.parent / (image.name + ".native.png")
                desktop.screenshot(screenshot, region(window))
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
            print(
                json.dumps(
                    {
                        key: record[key]
                        for key in [
                            "image",
                            "expected",
                            "elapsed_seconds",
                            "backend_preview_ready_seconds",
                            "sampled_process_tree_peak_rss_mb",
                            "source_unchanged",
                        ]
                    }
                ),
                flush=True,
            )
            assert any(
                name in (last.get("filename") or "") for name in expected_names
            ), f"Image was not selected: {last}"
            assert last.get("cancelHidden"), f"Load timed out: {last}"
            if expect_cancel:
                assert (
                    cancel_requested and last.get("phase") == "cancelled"
                ), f"Expected cancellation: {last}"
            elif expect_error:
                assert last.get("phase") == "error" or last.get(
                    "error"
                ), f"Expected an error: {last}"
            else:
                assert last.get("phase") == "ready" and not last.get(
                    "error"
                ), f"Image load failed: {last}"
            assert (
                expect_error
                or expect_cancel
                or not any(
                    word in (last.get("status") or "").lower()
                    for word in [
                        "error",
                        "failed",
                        "unsupported",
                        "unable",
                        "cancelled",
                    ]
                )
            ), f"Image load failed: {last}"
            assert last[
                "saveDisabled"
            ], "Save became enabled without a supported operation"
            assert record[
                "source_unchanged"
            ], "Source size or modification timestamp changed"
        if args.window_controls:
            desktop.float(app_window("Aperture"))
            desktop.focus(app_window("Aperture"))
            time.sleep(0.7)
            controls = {}

            def control_state():
                return {
                    "title": js(
                        "return document.querySelector('#maximize-button').title;"
                    ),
                    "maximized": request(
                        "POST",
                        route + "/execute/async",
                        {
                            "script": "const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke('plugin:window|is_maximized',{label:'main'}).then(done);",
                            "args": [],
                        },
                    ),
                    "window": app_window("Aperture"),
                }

            controls["before"] = control_state()
            click("maximize-button")
            time.sleep(0.5)
            controls["maximized"] = control_state()
            click("maximize-button")
            time.sleep(0.5)
            controls["restored"] = control_state()
            controls["maximize_result"] = (
                "changed"
                if size(controls["before"]["window"])
                != size(controls["maximized"]["window"])
                else "ignored_by_compositor"
            )
            args.output.with_suffix(".controls.json").write_text(
                json.dumps(controls, indent=2) + "\n"
            )
            desktop.resize(app_window("Aperture"), 640, 400)
            desktop.focus(app_window("Aperture"))
            time.sleep(0.5)
            window = app_window("Aperture")
            controls["minimum_requested"] = [640, 400]
            controls["minimum_actual"] = size(window)
            args.output.with_suffix(".controls.json").write_text(
                json.dumps(controls, indent=2) + "\n"
            )
            assert js("return document.body.scrollWidth <= window.innerWidth;")
            desktop.screenshot(
                args.output.parent / "minimum-window.native.png", region(window)
            )
            click("close-button")
            time.sleep(0.7)
            assert not any(w["app_id"] == APP_ID for w in desktop.windows())
            print(
                "Native Close passed; compositor maximize/resize results recorded separately.",
                flush=True,
            )
    finally:
        if not args.leave_open:
            request("DELETE", route)


if __name__ == "__main__":
    main()
