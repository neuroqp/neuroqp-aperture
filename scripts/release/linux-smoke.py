#!/usr/bin/env python3
"""Run native Linux smoke checks and copy exact files over SSH.

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
        raise ValueError("Desktop screensaver is already active; unlock before smoke")
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


def run(identity_path, report, driver_plan):
    identity = json.loads(identity_path.read_text())
    helper = Path(__file__).with_name("smoke-desktop-linux.py")
    environment = os.environ.copy()
    environment.update(json.loads(driver_plan.read_text())["environment"])
    environment.pop("NO_AT_BRIDGE", None)
    if report.exists():
        raise ValueError("Smoke output must be fresh; preserve previous evidence")
    return subprocess.run([sys.executable, str(helper), "--identity", str(identity_path),
                           "--environment", identity["environment"], "--report", str(report)],
                          env=environment, timeout=600).returncode


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


def collect(source_sha, checksums, output, reports):
    if output.exists():
        raise ValueError("Evidence output must be fresh")
    rows = [json.loads(path.read_text()) for path in reports]
    expected = {"windows11-setup", "windows11-portable", "debian12", "ubuntu24-wayland"}
    if len(rows) != len(expected) or {row["environment"] for row in rows} != expected:
        raise ValueError("One report for each routine desktop environment is required")
    digest = sha(checksums)
    for row in rows:
        if row.get("source_sha") != source_sha or row.get("checksums_sha256") != digest:
            raise ValueError("Report source/checksums differ from candidate")
        if row.get("status") != "pass" or row.get("forced_kill") is not False:
            raise ValueError("Every native smoke report must pass without forced termination")
        steps = {step["name"]: step for step in row["steps"]}
        if any(steps.get(name, {}).get("status") != "pass" for name in ("launch", "render", "clean-exit")):
            raise ValueError("Passing native launch/render/clean-exit required")
    output.write_text(json.dumps(dict(schema=1, operation="smoke", source_sha=source_sha,
                                     checksums_sha256=digest, reports=rows), indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    child = sub.add_parser("idle-guard", help="Keep an active Debian X11 desktop awake during checks")
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
    for name in ("run",):
        child = sub.add_parser(name)
        child.add_argument("identity", type=Path)
        child.add_argument("report", type=Path)
        child.add_argument("--driver-plan", type=Path, required=True)
    child = sub.add_parser("collect", help="Combine four actual native reports for publication")
    child.add_argument("source_sha")
    child.add_argument("checksums", type=Path)
    child.add_argument("output", type=Path)
    child.add_argument("reports", type=Path, nargs="+")
    args = parser.parse_args()
    if args.command == "collect":
        collect(args.source_sha, args.checksums, args.output, args.reports)
        return 0
    if args.command == "idle-guard":
        return idle_guard(args)
    if args.command == "run":
        return run(args.identity, args.report, args.driver_plan)
    if args.command == "copy-check":
        copy_check(args)
        return 0
    copy(args.command, args.source, args.destination, args.sha256, args.ssh)
    print(json.dumps(dict(status="pass", transport="ssh-stdin", sha256=args.sha256)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
