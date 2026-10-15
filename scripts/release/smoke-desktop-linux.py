#!/usr/bin/env python3
"""Stage C only: exact AppImage, one prepared TIFF, native pixels and normal close.

Stage A supplies the active desktop, a direct WebKitWebDriver endpoint, GI/AT-SPI
and screenshot command. See RELEASE-ACCEPTANCE.md for this prepared mode.
No dependency installation, desktop repair or package rebuilding here.
"""

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from urllib.request import Request, urlopen
from urllib.parse import urlsplit

STEP_SECONDS = {"launch": 90, "render": 60, "clean-exit": 30, "fuse-fallback": 180}
IMAGE_INPUT = "startup image command line"


def utc():
    return datetime.now(timezone.utc).isoformat()


def request_timeout(method, suffix, remaining):
    # Session creation includes real native WebKit startup on software-rendered VMs.
    return max(.1, remaining if method == "POST" and suffix == "/session" else min(remaining, 10))


@contextmanager
def bounded_action(seconds):
    """Interrupt blocking observer work without terminating the application."""
    if seconds <= 0:
        raise TimeoutError("Step or frozen attempt deadline reached")
    previous_handler = signal.getsignal(signal.SIGALRM)
    previous_timer = signal.getitimer(signal.ITIMER_REAL)
    started = time.monotonic()
    def expired(signum, frame):
        raise TimeoutError("Step or frozen attempt deadline reached")
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, min(seconds, previous_timer[0]) if previous_timer[0] else seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous_handler)
        delay, interval = previous_timer
        signal.setitimer(signal.ITIMER_REAL, max(.001, delay - (time.monotonic() - started)) if delay else 0, interval)


def sha(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def verify_inputs(identity):
    for name in ("asset", "tiny_tiff"):
        if sha(identity[name + "_path"]) != identity[name + "_sha256"]:
            raise ValueError(name + " SHA256 mismatch before launch")
    if not os.access(identity["asset_path"], os.X_OK):
        raise ValueError("AppImage is not executable. In Stage A prepare an owned copy with chmod +x; verify its unchanged SHA256 before launch")
    if identity["readiness"]["helper_sha256"] != sha(__file__):
        raise ValueError("Stage A reviewed helper changed")
    if not identity["readiness"]["ready"] or os.getuid() == 0:
        raise ValueError("Prepared standard-user desktop required")
    tools = identity["readiness"]["tools"]
    if not tools:
        raise ValueError("Prepared observer tools required")
    for tool in tools:
        if not Path(tool["path"]).is_absolute() or sha(tool["path"]) != tool["sha256"]:
            raise ValueError("Stage A observer tool changed: " + tool["name"])
    pinned = {Path(tool["path"]).resolve() for tool in tools}
    for path in (sys.executable, shutil.which("ss"), identity["screenshot_command"][0]):
        if not path or not Path(path).is_absolute() or Path(path).resolve() not in pinned:
            raise ValueError("Actual observer executable is not Stage A pinned: " + str(path))


def verify_driver(identity, timeout):
    url = urlsplit(identity["driver_url"])
    if url.scheme != "http" or url.hostname not in ("127.0.0.1", "localhost") or not url.port or url.path not in ("", "/") or url.username or url.query or url.fragment:
        raise ValueError("Prepared native driver must use an owned loopback endpoint")
    rows = subprocess.run([shutil.which("ss"), "-H", "-ltnp"], check=True, capture_output=True, text=True, timeout=timeout).stdout.splitlines()
    listeners = [row for row in rows if row.split()[3].rsplit(":", 1)[-1] == str(url.port)]
    pids = {int(pid) for row in listeners for pid in re.findall(r"\bpid=(\d+)\b", row)}
    if len(pids) != 1:
        raise ValueError("Exactly one owned native driver listener required")
    proc = Path("/proc", str(pids.pop()))
    executable = (proc / "exe").resolve()
    if proc.stat().st_uid != os.getuid() or executable.name != "WebKitWebDriver" or executable not in {Path(tool["path"]).resolve() for tool in identity["readiness"]["tools"]}:
        raise ValueError("Stage C requires the owned Stage A pinned WebKitWebDriver listener directly; prepare its endpoint before freeze as described in RELEASE-ACCEPTANCE.md")
    return {"pid": int(proc.name), "path": str(executable), "endpoint": identity["driver_url"]}


def assert_os(environment, actual_os):
    expected_id, expected_version = {"debian12": ("debian", "12"), "ubuntu24": ("ubuntu", "24.04"), "arch-omarchy": ("arch", None), "fedora-wayland": ("fedora", None)}[environment]
    actual_id = actual_os.get("ID", "").strip('"')
    omarchy = environment == "arch-omarchy" and actual_id == "omarchy" and "arch" in actual_os.get("ID_LIKE", "").strip('"').split()
    if not (actual_id == expected_id or omarchy) or expected_version and actual_os.get("VERSION_ID", "").strip('"') != expected_version:
        raise ValueError("Actual OS differs from required native environment")


def assert_rendered(observation, identity):
    state = observation.get("state") or {}
    if observation.get("phase") == "error" or observation.get("error") or state.get("phase") == "error" or state.get("error"):
        raise ValueError("Actual application reports image error")
    if not (state.get("phase") == "ready" and state.get("previewReady") and observation.get("phase") == "ready"):
        return False
    if state.get("path") != identity["tiny_tiff_path"] or (state.get("width"), state.get("height")) != (identity["tiny_tiff_width"], identity["tiny_tiff_height"]):
        raise ValueError("Actual image path/dimensions differ")
    return True


def assert_exited(result, pid):
    if result.get("pid") != pid or result.get("exit_code") != 0 or Path("/proc", str(pid)).exists():
        raise ValueError("Native close did not exit normally with code 0")


def display_sockets(rows, pid):
    """Client sockets are unnamed; correlate their reciprocal connected peers."""
    owned = [row for row in rows if re.search(r"\bpid=" + str(pid) + r"\b", row)]
    pairs = {tuple(row.split()[5:8:2]) for row in owned if len(row.split()) >= 8}
    peers = [row for row in rows if len(row.split()) >= 8 and tuple(row.split()[5:8:2])[::-1] in pairs]
    observed = owned + peers
    wayland = any("/wayland-" in row for row in observed)
    x11 = any(".X11-unix/" in row for row in observed)
    return observed, wayland, x11


def ppm_spread(path, rect):
    """Read prepared desktop P6 capture; sample only inside native viewer bounds."""
    with open(path, "rb") as stream:
        def token():
            value = bytearray()
            while True:
                byte = stream.read(1)
                if byte == b"#":
                    stream.readline()
                elif byte and not byte.isspace():
                    value.extend(byte)
                    break
                elif not byte:
                    raise ValueError("Truncated PPM")
            while (byte := stream.read(1)) and not byte.isspace():
                value.extend(byte)
            return bytes(value)
        if token() != b"P6":
            raise ValueError("Stage A screenshot command must produce P6 PPM")
        width, height, maximum = int(token()), int(token()), int(token())
        pixels = stream.read()
    if maximum != 255 or len(pixels) != width * height * 3:
        raise ValueError("Invalid desktop pixel capture")
    x, y, w, h = rect
    if w < 100 or h < 100 or x < 0 or y < 0 or x + w > width or y + h > height:
        raise ValueError("Native viewer extent outside observed desktop")
    values = []
    for fx in (.25, .4, .5, .6, .75):
        for fy in (.25, .4, .5, .6, .75):
            offset = (int(y + h * fy) * width + int(x + w * fx)) * 3
            values.append(sum(pixels[offset:offset + 3]) / 3)
    ordered = sorted(values)
    # A cursor or one transitional bright pixel must not prove a rendered image.
    return {"spread": ordered[-5] - ordered[4], "sample_range": max(values) - min(values), "samples": values, "viewer_rect": rect, "desktop_size": [width, height]}


def native_nodes(pid, deadline):
    import gi
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
    Atspi.set_timeout(1000, 1000)
    desktop = Atspi.get_desktop(0)
    found = []
    visited = 0
    def check():
        if time.monotonic() >= deadline:
            raise TimeoutError("Native accessibility observation deadline reached")
    def visit(node, depth=0):
        nonlocal visited
        check()
        visited += 1
        if visited > 500:
            raise ValueError("Native accessibility observation exceeds 500 visited nodes")
        if depth > 16:
            return
        state = node.get_state_set()
        check()
        if state.contains(Atspi.StateType.SHOWING):
            component = node.get_component_iface()
            extent = component.get_extents(Atspi.CoordType.SCREEN) if component else None
            found.append((node, {"name": node.get_name(), "role": node.get_role_name(), "active": state.contains(Atspi.StateType.ACTIVE), "sensitive": state.contains(Atspi.StateType.SENSITIVE), "rect": [extent.x, extent.y, extent.width, extent.height] if extent else None}))
        for n in range(node.get_child_count()):
            check()
            child = node.get_child_at_index(n)
            if child:
                visit(child, depth + 1)
    for n in range(desktop.get_child_count()):
        check()
        app = desktop.get_child_at_index(n)
        if app and app.get_process_id() == pid:
            visit(app)
    return found


def owned_process(pid, identity):
    proc = Path("/proc", str(pid))
    env = dict(v.split(b"=", 1) for v in (proc / "environ").read_bytes().split(b"\0") if b"=" in v)
    if proc.stat().st_uid != os.getuid() or env.get(b"APPIMAGE") != os.fsencode(identity["asset_path"]):
        raise ValueError("Actual native process does not own this exact AppImage")
    appdir = Path(os.fsdecode(env[b"APPDIR"]))
    if (proc / "exe").resolve() != (appdir / "usr/bin/aperture").resolve():
        raise ValueError("Observed native process is not the packaged Aperture executable")
    info = json.loads((appdir / "usr/share/aperture/BUILDINFO.json").read_text())
    if info["source_sha"] != identity["source_sha"]:
        raise ValueError("Embedded source differs from frozen candidate")
    return {"start_ticks": (proc / "stat").read_text().rsplit(")", 1)[1].split()[19], "buildinfo": info}


def application_process(launcher_pid, identity, deadline):
    """AppImage extraction can keep a launcher parent while AppRun owns the UI."""
    matches = []
    for item in Path("/proc").iterdir():
        if not item.name.isdecimal():
            continue
        try:
            if item.stat().st_uid != os.getuid() or (item / "exe").resolve().name != "aperture":
                continue
            ancestor = int(item.name)
            while ancestor != launcher_pid and ancestor > 1:
                ancestor = int(Path("/proc", str(ancestor), "stat").read_text().rsplit(")", 1)[1].split()[1])
            if ancestor == launcher_pid:
                process_identity = owned_process(int(item.name), identity)
                nodes = native_nodes(int(item.name), deadline)
                if any(row["role"] in ("frame", "window") for _, row in nodes):
                    matches.append((int(item.name), process_identity, nodes))
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
    if len(matches) > 1:
        raise ValueError("Exactly one owned native Aperture process required")
    return matches[0] if matches else None


def publish_receipt(receipt, result):
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=receipt.parent,
                                     prefix=f".{receipt.name}.", delete=False) as stream:
        temporary = Path(stream.name)
        try:
            json.dump(result, stream)
            stream.close()
            temporary.replace(receipt)
        finally:
            temporary.unlink(missing_ok=True)


def launcher(identity, receipt, extra):
    verify_inputs(identity)
    command = [identity["asset_path"]]
    if identity.get("extract_and_run"):
        command.append("--appimage-extract-and-run")
    command.extend(extra)
    environment = os.environ.copy()
    for name in ("APPIMAGE_EXTRACT_AND_RUN", "APPIMAGE_EXTRACT_AND_RUN_DEFAULT"):
        environment.pop(name, None)
    with open(str(receipt) + ".log", "xb") as log:
        try:
            child = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, env=environment)
        except OSError as error:
            log.write((str(error) + "\n").encode())
            publish_receipt(receipt, {"pid": None, "exit_code": 1, "error": str(error), "finished_at": utc()})
            return 1
        Path(str(receipt) + ".pid").write_text(str(child.pid))
        code = child.wait()
    publish_receipt(receipt, {"pid": child.pid, "exit_code": code, "finished_at": utc()})
    return code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity", type=Path, required=True)
    parser.add_argument("--environment", choices=["debian12", "ubuntu24", "arch-omarchy", "fedora-wayland"])
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--launcher", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--extract-and-run", action="store_true", help=argparse.SUPPRESS)
    args, extra = parser.parse_known_args()
    identity = json.loads(args.identity.read_text())
    if args.extract_and_run:
        identity["extract_and_run"] = True
    if args.launcher:
        return launcher(identity, args.report, extra)
    if extra or not args.environment:
        parser.error("environment required; unexpected arguments")
    if args.report.exists() or not args.report.parent.is_dir():
        raise ValueError("Stage A must prepare a fresh evidence destination")
    if identity.get("extract_and_run"):
        raise ValueError("Primary launch must use FUSE; prepared fallback follows normal close")
    report = {k: identity[k] for k in ("source_sha", "build_inputs_sha256", "checksums_sha256", "freeze_started_at", "deadline_at")}
    report.update(schema=1, stage="C", environment=args.environment, status="inconclusive", attempts=1, forced_kill=False, started_at=utc(), steps=[], helper_sha256=sha(__file__), helper_revision=identity["source_sha"], asset_sha256=identity["asset_sha256"], tiny_tiff_sha256=identity["tiny_tiff_sha256"], os_release=Path("/etc/os-release").read_text(), uid=os.getuid())
    route = None
    current_launch = None
    receipt = args.report.with_suffix(".exit.json")
    limit = 0
    def remaining():
        freeze = datetime.fromisoformat(identity["deadline_at"].replace("Z", "+00:00")).timestamp() - time.time()
        return max(0, min(limit - time.monotonic(), freeze))
    def request(method, suffix, body=None):
        if remaining() <= 0:
            raise TimeoutError("Frozen attempt deadline reached")
        with urlopen(Request(identity["driver_url"] + suffix, data=json.dumps(body).encode() if body is not None else None, method=method, headers={"Content-Type": "application/json"}), timeout=request_timeout(method, suffix, remaining())) as response:
            value = json.load(response)["value"]
        if isinstance(value, dict) and value.get("error"):
            raise RuntimeError(str(value))
        return value
    def wait(fn):
        while remaining() > 0:
            if value := fn():
                return value
            time.sleep(.15)
        raise TimeoutError("Step deadline reached")
    def step(name, action):
        nonlocal limit
        seconds = STEP_SECONDS[name]
        limit = time.monotonic() + seconds
        row = dict(name=name, status="fail", started_at=utc(), timeout_seconds=seconds, forced_kill=False)
        report["steps"].append(row)
        try:
            if remaining() <= 0:
                raise TimeoutError("Frozen attempt deadline reached")
            with bounded_action(remaining()):
                row["detail"] = action()
            if remaining() <= 0:
                raise TimeoutError("Step or frozen attempt deadline reached")
            row["status"] = "pass"
        finally:
            row["finished_at"] = utc()
    def launch():
        nonlocal route, current_launch
        verify_inputs(identity)
        report["observer_tools"] = identity["readiness"]["tools"]
        report["driver_observer"] = verify_driver(identity, max(.1, min(remaining(), 5)))
        actual_os = dict(line.split("=", 1) for line in Path("/etc/os-release").read_text().splitlines() if "=" in line)
        assert_os(args.environment, actual_os)
        if identity["readiness"]["environment"] != args.environment or identity["readiness"]["source_sha"] != identity["source_sha"]:
            raise ValueError("Stage A receipt does not match candidate/environment")
        if identity.get("extract_and_run") and args.environment not in ("debian12", "ubuntu24"):
            raise ValueError("Fallback is confined to Debian/Ubuntu")
        if not request("GET", "/status")["ready"]:
            raise ValueError("Prepared native driver unavailable")
        launch_args = ["--launcher", "--identity", str(args.identity.resolve()), "--report", str(receipt.resolve()), identity["tiny_tiff_path"]]
        if identity.get("extract_and_run"):
            launch_args.append("--extract-and-run")
        session = request("POST", "/session", {"capabilities": {"alwaysMatch": {"browserName": "wry", "webkitgtk:browserOptions": {"binary": str(Path(__file__).resolve()), "args": launch_args}}}})
        route = "/session/" + session["sessionId"]
        pid_path = Path(str(receipt) + ".pid")
        def launched_pid():
            if receipt.exists():
                result = json.loads(receipt.read_text())
                if result.get("error"):
                    raise RuntimeError("AppImage launch failed: " + result["error"])
            return int(pid_path.read_text()) if pid_path.exists() else None
        launcher_pid = wait(launched_pid)
        pid, process_identity, nodes = wait(lambda: application_process(launcher_pid, identity, time.monotonic() + remaining()))
        rows = subprocess.run([shutil.which("ss"), "-xnp"], check=True, capture_output=True, text=True, timeout=max(.1, remaining())).stdout.splitlines()
        peers, wayland, x11 = display_sockets(rows, pid)
        if args.environment in ("fedora-wayland", "arch-omarchy") and not (wayland and not x11):
            raise ValueError("Actual native Wayland socket proof absent or mixed")
        current_launch = dict(pid=pid, launcher_pid=launcher_pid, launch_method="extract-and-run" if identity.get("extract_and_run") else "FUSE", image_argument=identity["tiny_tiff_path"], process_identity=process_identity, native_windows=[row for _, row in nodes], socket_rows=peers, backend="wayland" if wayland and not x11 else "x11" if x11 and not wayland else "unproven")
        return current_launch
    def render():
        diagnostic = {"gate": "application-state"}
        report["steps"][-1]["diagnostic"] = diagnostic
        expression = "const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke('image_state').then(state=>{const canvas=document.querySelector('#viewer canvas')?.getBoundingClientRect();done({state,phase:document.querySelector('#viewer')?.dataset.phase??null,error:document.querySelector('#status')?.classList.contains('error')??false,rendered_canvas:!!canvas&&canvas.width>0&&canvas.height>0});}).catch(e=>done({error:String(e)}));"
        def observation():
            try:
                value = request("POST", route + "/execute/async", {"script": expression, "args": []})
            except Exception as error:
                diagnostic["observer_error"] = str(error)
                raise
            diagnostic["observation"] = value
            return value if assert_rendered(value, identity) and value.get("rendered_canvas") else None
        state = wait(observation)
        diagnostic["gate"] = "foreground"
        pid = current_launch["pid"]
        def active_window():
            nodes = native_nodes(pid, time.monotonic() + remaining())
            diagnostic["windows"] = [row for _, row in nodes if row["role"] in ("frame", "window")]
            return next((row for _, row in nodes if row["role"] in ("frame", "window") and row["active"]), None)
        # A prepared owned-window capture adapter may activate only this process.
        capture = receipt.with_suffix(".ppm")
        command = [arg.replace("{output}", str(capture)).replace("{pid}", str(pid)) for arg in identity["screenshot_command"]]
        if identity.get("screenshot_scope") == "owned-window":
            subprocess.run(command, check=True, timeout=max(.1, remaining()))
        foreground = wait(active_window)
        diagnostic["gate"] = "visible-pixels"
        def visible_pixels():
            nonlocal foreground
            subprocess.run(command, check=True, timeout=max(.1, remaining()))
            # Prepared capture can maximize the owned window. Observe its new bounds.
            foreground = active_window()
            viewer = next((row for _, row in native_nodes(pid, time.monotonic() + remaining()) if row["name"] == "Image viewer" and row["rect"]), None)
            diagnostic["viewer"] = viewer
            if not foreground or not viewer:
                return None
            rect = viewer["rect"]
            if identity.get("screenshot_scope") == "owned-window":
                frame = foreground["rect"]
                rect = [rect[0] - frame[0], rect[1] - frame[1], rect[2], rect[3]]
            pixels = ppm_spread(capture, rect)
            diagnostic["pixels"] = pixels
            return pixels if pixels["spread"] >= 35 else None
        pixels = wait(visible_pixels)
        diagnostic["gate"] = "foreground-after-capture"
        if not active_window():
            raise ValueError("Owned native window lost foreground during capture")
        diagnostic["gate"] = "complete"
        return dict(observation=state, screenshot=str(capture), screenshot_sha256=sha(capture), pixel_observation=pixels, foreground_window=foreground, image_input=IMAGE_INPUT)
    def close():
        pid = current_launch["pid"]
        if owned_process(pid, identity) != current_launch["process_identity"]:
            raise ValueError("Owned process identity changed before native Close")
        nodes = native_nodes(pid, time.monotonic() + remaining())
        if not any(row["role"] in ("frame", "window") and row["active"] for _, row in nodes):
            raise ValueError("Owned native window must be active before Close")
        matches = [(node, row) for node, row in nodes if row["name"] == "Close" and row["role"] in ("push button", "button") and row["sensitive"]]
        if len(matches) != 1:
            raise ValueError("Exactly one visible owned Close button required")
        action = matches[0][0].get_action_iface()
        if not action or not action.do_action(0):
            raise ValueError("Native Close action rejected")
        result = wait(lambda: json.loads(receipt.read_text()) if receipt.exists() else None)
        assert_exited(result, current_launch["launcher_pid"])
        if Path("/proc", str(pid)).exists():
            raise ValueError("Actual native application remains alive after normal Close")
        verify_inputs(identity)
        return {**result, "app_pid": pid, "app_exited": True, "exit_code_source": "exact AppImage launcher wait"}
    try:
        step("launch", launch)
        step("render", render)
        step("clean-exit", close)
        if identity.get("fuse_fallback_required"):
            request("DELETE", route)
            route = None
            identity["extract_and_run"] = True
            receipt = args.report.with_suffix(".fallback-exit.json")
            def fallback():
                return {"launch": launch(), "render": render(), "clean-exit": close()}
            step("fuse-fallback", fallback)
        report["status"] = "pass"
    except Exception as error:
        report.update(status="fail", error=str(error))
        if isinstance(error, (TimeoutError, subprocess.TimeoutExpired)):
            report["timed_out"] = report["steps"][-1]["timed_out"] = True
        if isinstance(error, subprocess.TimeoutExpired):
            report["forced_kill"] = report["steps"][-1]["forced_kill"] = True
    finally:
        # Failed processes stay available for diagnosis; driver deletion can kill.
        if route and report["status"] == "pass":
            try:
                request("DELETE", route)
            except Exception as error:
                report["cleanup_error"] = str(error)
                report["status"] = "fail"
        elif route:
            report["preserved_session_id"] = route
        report["finished_at"] = utc()
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    return 0 if report["status"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
