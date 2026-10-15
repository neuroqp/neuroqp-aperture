#!/usr/bin/env python3
"""Prepared Linux smoke transport and invocation, shared by rehearsal and Stage C.

copy-to/copy-from use SSH's byte streams, without an SFTP subsystem. Incoming
copies are owned mode 0700, ready for launch. Supply the prepared SSH arguments
after --, for example: -- ssh -p 2224 user@127.0.0.1.
"""

import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
import time


def sha(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def desktop_bus(environment):
    from gi.repository import Gio
    return Gio.DBusConnection.new_for_address_sync(
        environment["DBUS_SESSION_BUS_ADDRESS"],
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None, None)


def bus_call(bus, destination, path, interface, method, signature=None, values=()):
    from gi.repository import Gio, GLib
    return bus.call_sync(destination, path, interface, method,
                         GLib.Variant(signature, values) if signature else None, None,
                         Gio.DBusCallFlags.NO_AUTO_START, 5000, None).unpack()


def idle_probe(bus, session_id, owner=None):
    output = subprocess.check_output(["loginctl", "show-session", session_id,
                                      "-p", "Id", "-p", "Active", "-p", "LockedHint", "-p", "User",
                                      "-p", "Type", "-p", "Class"], text=True, timeout=5)
    session = dict(line.split("=", 1) for line in output.splitlines() if "=" in line)
    expected = dict(Id=session_id, Active="yes", LockedHint="no", User=str(os.getuid()), Type="x11", Class="user")
    if any(session.get(key) != value for key, value in expected.items()):
        raise ValueError("Idle guard requires the owned, active, unlocked X11 desktop session: " + str(session))
    current_owner, = bus_call(bus, "org.freedesktop.DBus", "/org/freedesktop/DBus",
                              "org.freedesktop.DBus", "GetNameOwner", "(s)", ("org.freedesktop.ScreenSaver",))
    if owner is not None and current_owner != owner:
        raise ValueError("Prepared screen locker restarted; idle inhibition was lost")
    active, = bus_call(bus, current_owner, "/org/freedesktop/ScreenSaver", "org.freedesktop.ScreenSaver", "GetActive")
    if active is not False:
        raise ValueError("Desktop screensaver is already active; unlock before readiness")
    return session, current_owner


def start_ticks(pid):
    return int(Path("/proc", str(pid), "stat").read_text().rsplit(")", 1)[1].split()[19])


def write_guard_record(path, record, fresh=False):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(json.dumps(record, indent=2) + "\n")
            stream.flush()
            if fresh:
                os.link(temporary, path)
            else:
                temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def idle_guard(args):
    """Hold light-locker's acknowledged, connection-scoped inhibition for the lab lifetime."""
    plan = json.loads(args.driver_plan.read_text())
    report = Path(plan["idle_guard"])
    if not report.is_absolute() or report.exists() or not 0 < args.seconds <= 7200 or os.getuid() == 0:
        raise ValueError("Idle guard requires a fresh absolute receipt, ordinary user and 1-7200 seconds")
    if plan["environment"].get("XDG_SESSION_TYPE") != "x11":
        raise ValueError("Idle guard is prepared only for the Debian X11 lab")
    stopped = threading.Event()
    previous = {sig: signal.signal(sig, lambda *_: stopped.set()) for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)}
    bus = None
    wrote_receipt = False
    record = dict(schema=1, status="starting", pid=os.getpid(), start_ticks=start_ticks(os.getpid()),
                  runner_path=str(Path(__file__).resolve()), runner_sha256=sha(__file__),
                  driver_plan_sha256=sha(args.driver_plan), session_id=args.session_id)
    deadline = time.monotonic() + args.seconds
    try:
        bus = desktop_bus(plan["environment"])
        session, owner = idle_probe(bus, args.session_id)
        cookie, = bus_call(bus, owner, "/org/freedesktop/ScreenSaver", "org.freedesktop.ScreenSaver", "Inhibit",
                           "(ss)", ("Aperture smoke lab", "Keep prepared Debian desktop active through final smoke"))
        idle_probe(bus, args.session_id, owner)
        record.update(status="active", session=session, locker_owner=owner, cookie=cookie,
                      bus_name=bus.get_unique_name(), started_at=datetime.now(timezone.utc).isoformat(),
                      expires_at=(datetime.now(timezone.utc) + timedelta(seconds=max(0, deadline - time.monotonic()))).isoformat())
        write_guard_record(report, record, fresh=True)
        wrote_receipt = True
        print(json.dumps(record), flush=True)
        while not stopped.wait(min(5, max(0, deadline - time.monotonic()))):
            if time.monotonic() >= deadline:
                raise TimeoutError("Idle guard lab lifetime expired")
            idle_probe(bus, args.session_id, owner)
        record["status"] = "closed"
        return 0
    except Exception as error:
        record.update(status="fail", error=str(error))
        raise
    finally:
        # Closing the private connection also releases the cookie after a crash
        # or SIGHUP. No xset, xfconf or user preferences require restoration.
        try:
            if bus is not None:
                bus.close_sync(None)
        except Exception as error:
            record.update(status="fail", error="Idle guard connection closure failed: " + str(error))
            raise
        finally:
            try:
                if wrote_receipt:
                    record["finished_at"] = datetime.now(timezone.utc).isoformat()
                    write_guard_record(report, record)
            finally:
                for sig, handler in previous.items():
                    signal.signal(sig, handler)


def require_idle_guard(plan_path, environment):
    plan = json.loads(plan_path.read_text())
    report = Path(plan.get("idle_guard", ""))
    if not report.is_absolute() or report.stat().st_uid != os.getuid():
        raise ValueError("Debian smoke requires the prepared idle guard receipt")
    record = json.loads(report.read_text())
    proc = Path("/proc", str(record["pid"]))
    command = (proc / "cmdline").read_bytes().split(b"\0")
    if (record.get("status") != "active" or record.get("runner_sha256") != sha(__file__)
            or record.get("runner_path") != str(Path(__file__).resolve())
            or record.get("driver_plan_sha256") != sha(plan_path)):
        raise ValueError("Prepared Debian idle guard identity differs")
    if datetime.now(timezone.utc) >= datetime.fromisoformat(record["expires_at"]):
        raise ValueError("Prepared Debian idle guard lifetime expired")
    if (proc.stat().st_uid != os.getuid() or start_ticks(record["pid"]) != record["start_ticks"]
            or str(Path(__file__).resolve()).encode() not in command or b"idle-guard" not in command):
        raise ValueError("Prepared Debian idle guard process is no longer alive")
    bus = desktop_bus(environment)
    try:
        present, = bus_call(bus, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                            "NameHasOwner", "(s)", (record["bus_name"],))
        if present is not True:
            raise ValueError("Prepared Debian idle guard connection is no longer alive")
        idle_probe(bus, record["session_id"], record["locker_owner"])
    finally:
        bus.close_sync(None)


# The receiver verifies before atomic replacement, preserving a previous copy on
# an interrupted or corrupt transfer. The same code runs locally and over SSH.
RECEIVE = '''import hashlib, os, pathlib, shutil, sys, tempfile
destination = pathlib.Path(sys.argv[1])
with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stream:
    temporary = pathlib.Path(stream.name)
    try:
        shutil.copyfileobj(sys.stdin.buffer, stream)
        stream.flush()
        stream.seek(0)
        digest = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
        if digest.hexdigest() != sys.argv[2]:
            raise ValueError("Transferred SHA256 mismatch")
        os.chmod(temporary, 0o700)
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)
'''


def copy(direction, source, destination, expected, ssh):
    if not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise ValueError("Expected full SHA256 required")
    if not ssh or Path(ssh[0]).name != "ssh":
        raise ValueError("Supply prepared ssh command after --")
    if direction == "copy-to":
        if sha(source) != expected:
            raise ValueError("Source SHA256 mismatch")
        remote = shlex.join(["/usr/bin/python3", "-c", RECEIVE, destination, expected])
        with open(source, "rb") as stream:
            subprocess.run([*ssh, remote], stdin=stream, check=True, timeout=600)
    else:
        remote = shlex.join(["/usr/bin/python3", "-c",
                             "import shutil,sys; shutil.copyfileobj(open(sys.argv[1], 'rb'), sys.stdout.buffer)", source])
        target = Path(destination)
        with tempfile.NamedTemporaryFile(dir=target.parent, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                subprocess.run([*ssh, remote], stdout=stream, check=True, timeout=600)
                stream.flush()
                if sha(temporary) != expected:
                    raise ValueError("Transferred SHA256 mismatch")
                temporary.replace(target)
            finally:
                temporary.unlink(missing_ok=True)


def run(identity_path, report, driver_plan, rehearsal=False):
    identity = json.loads(identity_path.read_text())
    helper = Path(identity["readiness"]["helper_path"])
    if sha(helper) != identity["readiness"]["helper_sha256"]:
        raise ValueError("Prepared smoke helper SHA256 mismatch")
    if not rehearsal:
        pinned = identity["readiness"].get("preflight", {})
        if pinned.get("runner_sha256") != sha(__file__) or pinned.get("driver_plan_sha256") != sha(driver_plan):
            raise ValueError("Runner or driver plan differs from native preflight")
    # The driver's library overrides are necessary, but they are not a complete
    # desktop session: retain display, bus and Hyprland discovery variables.
    environment = os.environ.copy()
    environment.update(json.loads(driver_plan.read_text())["environment"])
    environment.pop("NO_AT_BRIDGE", None)
    if identity["readiness"]["environment"] == "debian12":
        require_idle_guard(driver_plan, environment)
    remaining = (datetime.fromisoformat(identity["deadline_at"].replace("Z", "+00:00")) - datetime.now(timezone.utc)).total_seconds()
    if remaining <= 0:
        raise ValueError("Smoke identity deadline already expired")
    if any(path.exists() for path in (report, report.with_suffix(".summary.json"), report.with_suffix(".invocation.json"))):
        raise ValueError("Smoke outputs must be fresh; preserve previous evidence")
    try:
        code = subprocess.run([sys.executable, str(helper), "--identity", str(identity_path),
                               "--environment", identity["readiness"]["environment"],
                               "--report", str(report)], env=environment, timeout=remaining + 5).returncode
        guard_error = None
        if identity["readiness"]["environment"] == "debian12":
            try:
                require_idle_guard(driver_plan, environment)
            except Exception as error:
                guard_error = "Debian idle inhibition lost during smoke: " + str(error)
                code = code or 1
        if report.exists():
            summary = compact_smoke(json.loads(report.read_text()))
            summary["raw_report_sha256"] = sha(report)
            if guard_error:
                summary.update(status="fail", error=guard_error)
            report.with_suffix(".summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        return code
    except subprocess.TimeoutExpired:
        # Stop only the stuck observer. An app surviving it is a failure requiring
        # owned cleanup, never successful native closure. Preserve its report.
        report.with_suffix(".invocation.json").write_text(json.dumps(dict(
            status="fail", timed_out=True, helper_forced_termination=True,
            error="Smoke observer exceeded identity deadline and report grace")) + "\n")
        return 124


def copy_check(args):
    expected = sha(args.fixture)
    if args.returned.exists() or args.report.exists():
        raise ValueError("Roundtrip outputs must be fresh")
    copy("copy-to", str(args.fixture), args.remote, expected, args.ssh)
    copy("copy-from", args.remote, str(args.returned), expected, args.ssh)
    result = dict(schema=1, status="pass", forced_kill=False, transport="ssh-stdin",
                  fixture_sha256=expected, returned_sha256=sha(args.returned),
                  transport_argv_sha256=hashlib.sha256(json.dumps(args.ssh).encode()).hexdigest(),
                  runner_sha256=sha(__file__))
    args.report.write_text(json.dumps(result, indent=2) + "\n")


def compact_smoke(smoke, prior=False):
    def select(value, keys):
        return {key: value[key] for key in keys.split() if key in value}
    def detail(name, value):
        if name == "launch":
            result = select(value, "pid launcher_pid backend launch_method image_argument")
            result["native_windows"] = [select(row, "role active sensitive rect") for row in value.get("native_windows", []) if row.get("role") in ("frame", "window")]
            result["process_identity"] = dict(buildinfo=select(value.get("process_identity", {}).get("buildinfo", {}), "source_sha"))
            return result
        if name == "render":
            observation = value.get("observation", {})
            return dict(observation={**select(observation, "phase error rendered_canvas"), "state": select(observation.get("state", {}), "phase previewReady width height path")},
                        **select(value, "image_input screenshot_sha256"), pixel_observation=select(value.get("pixel_observation", {}), "spread"))
        if name == "fuse-fallback":
            return {key: detail(key, value.get(key, {})) for key in ("launch", "render", "clean-exit")}
        return select(value, "pid exit_code app_pid app_exited exit_code_source")
    result = select(smoke, "schema stage status environment uid source_sha build_inputs_sha256 checksums_sha256 asset_sha256 helper_sha256 tiny_tiff_sha256 observer_tools freeze_started_at deadline_at started_at finished_at forced_kill attempts retry_reason transient_diagnosis timed_out deadline_exceeded helper_forced_termination error os os_release privilege")
    result["steps"] = [{**select(row, "name status forced_kill started_at finished_at timeout_seconds timed_out deadline_exceeded helper_forced_termination"),
                        **({"detail": detail(row["name"], row["detail"])} if "detail" in row else {})} for row in smoke.get("steps", [])]
    if "prior_attempt" in smoke:
        if prior:
            raise ValueError("Only one retained prior smoke attempt is supported")
        result["prior_attempt"] = compact_smoke(smoke["prior_attempt"], prior=True)
    return result


def preflight(args):
    if args.report.exists() or args.report.with_suffix(".readiness.json").exists():
        raise ValueError("Preflight outputs must be fresh; preserve previous evidence")
    identity = json.loads(args.identity.read_text())
    if args.transport_report:
        transport = json.loads(args.transport_report.read_text())
    elif identity["readiness"]["environment"] == "arch-omarchy" and sha(identity["tiny_tiff_path"]) == identity["tiny_tiff_sha256"]:
        transport = dict(status="pass", forced_kill=False, transport="local", sha256=identity["tiny_tiff_sha256"])
    else:
        raise ValueError("Guest preflight requires exact-fixture SSH roundtrip report")
    if args.transport_report and (transport.get("status") != "pass" or transport.get("forced_kill") is not False
            or transport.get("transport") != "ssh-stdin" or transport.get("runner_sha256") != sha(__file__)
            or transport.get("fixture_sha256") != identity["tiny_tiff_sha256"]
            or transport.get("returned_sha256") != identity["tiny_tiff_sha256"]):
        raise ValueError("Passing exact-fixture SSH roundtrip required before native preflight")
    code = run(args.identity, args.report, args.driver_plan, rehearsal=True)
    smoke = json.loads(args.report.read_text()) if args.report.exists() else {}
    steps = smoke.get("steps", [])
    passed = (code == 0 and smoke.get("status") == "pass" and smoke.get("forced_kill") is False
              and all(any(step.get("name") == name and step.get("status") == "pass" and step.get("forced_kill") is False
                          for step in steps) for name in ("launch", "render", "clean-exit")))
    result = dict(schema=1, status="pass" if passed else "fail", forced_kill=smoke.get("forced_kill"),
                  environment=identity["readiness"]["environment"], source_sha=identity["source_sha"], asset_sha256=identity["asset_sha256"],
                  helper_sha256=identity["readiness"]["helper_sha256"], tiny_tiff_sha256=identity["tiny_tiff_sha256"],
                  tools=identity["readiness"]["tools"], runner_sha256=sha(__file__),
                  driver_plan_sha256=sha(args.driver_plan), transport=transport,
                  transport_report_sha256=sha(args.transport_report) if args.transport_report else None,
                  smoke_report=compact_smoke(smoke),
                  smoke_report_sha256=sha(args.report) if args.report.exists() else None,
                  steps=[{key: step[key] for key in ("name", "status", "forced_kill") if key in step} for step in steps])
    args.report.with_suffix(".readiness.json").write_text(json.dumps(result, indent=2) + "\n")
    return code if code else (0 if passed else 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    child = sub.add_parser("idle-guard", help="Hold Debian X11 inhibition from readiness through final smoke")
    child.add_argument("--driver-plan", type=Path, required=True)
    child.add_argument("--session-id", required=True, help="Observed graphical loginctl session, never the SSH session")
    child.add_argument("--seconds", type=int, required=True)
    for name in ("copy-to", "copy-from"):
        child = sub.add_parser(name)
        child.add_argument("source")
        child.add_argument("destination")
        child.add_argument("sha256")
        child.add_argument("ssh", nargs=argparse.REMAINDER)
    child = sub.add_parser("copy-check")
    child.add_argument("fixture", type=Path)
    child.add_argument("remote")
    child.add_argument("returned", type=Path)
    child.add_argument("report", type=Path)
    child.add_argument("ssh", nargs=argparse.REMAINDER)
    for name in ("run", "preflight"):
        child = sub.add_parser(name)
        child.add_argument("identity", type=Path)
        child.add_argument("report", type=Path)
        child.add_argument("--driver-plan", type=Path, required=True)
        if name == "preflight":
            child.add_argument("--transport-report", type=Path)
    args = parser.parse_args()
    if args.command == "idle-guard":
        return idle_guard(args)
    if args.command == "run":
        return run(args.identity, args.report, args.driver_plan)
    if args.command == "preflight":
        return preflight(args)
    if args.command == "copy-check":
        copy_check(args)
        return 0
    copy(args.command, args.source, args.destination, args.sha256, args.ssh)
    print(json.dumps(dict(status="pass", transport="ssh-stdin", sha256=args.sha256)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
