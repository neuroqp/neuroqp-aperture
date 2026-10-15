#!/usr/bin/env python3
"""Small, fail-closed evidence and deadline checks for the A/B/C release stages."""

import argparse
import datetime as dt
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import subprocess
import sys

ROOT = Path(os.environ.get("APERTURE_SOURCE_ROOT", Path(__file__).resolve().parents[2]))
ENVIRONMENTS = ("windows11-setup", "windows11-portable", "debian12", "ubuntu24", "arch-omarchy", "fedora-wayland")
SMOKE_STEPS = ("launch", "render", "clean-exit")
ATTEMPT_SECONDS = 3600
STEP_LIMITS = {"launch": 90, "render": 60, "clean-exit": 30, "fuse-fallback": 180}


def timestamp():
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def parsed(value):
    if not isinstance(value, str):
        raise ValueError("Evidence timestamps must be UTC strings")
    result = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    if result.utcoffset() != dt.timedelta(0):
        raise ValueError("Evidence timestamps must use UTC")
    return result


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def write_report(path, report):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def git_inputs(root=ROOT):
    subprocess.run(["git", "-c", f"safe.directory={root}", "diff", "--exit-code", "HEAD", "--"], cwd=root, check=True, stdout=subprocess.DEVNULL)
    # Git blobs normalize checkout line endings and include tracked file modes.
    return subprocess.check_output(["git", "-c", f"safe.directory={root}", "ls-tree", "-rz", "HEAD"], cwd=root)


def build_inputs(root=ROOT):
    return hashlib.sha256(git_inputs(root)).hexdigest()


def identity(root=ROOT):
    entries = git_inputs(root).split(b"\0")
    components = {}
    for component, prefixes in (
        ("frontend", ("src/", "index.html", "package.json", "pnpm-lock.yaml", "tsconfig", "vite.config")),
        ("backend", ("src-tauri/",)),
        ("packaging", (".github/workflows/native-validation.yml", ".github/workflows/package-build.yml", ".github/workflows/release.yml", "scripts/release/build-portable.ps1", "scripts/release/windows-portable.nsi", "scripts/release/windows-installer-hooks.nsh", "scripts/release/prepare-", "scripts/release/package-linux.sh", "scripts/release/windows-dependencies.json")),
    ):
        rows = [entry for entry in entries if entry and entry.split(b"\t", 1)[1].decode().startswith(prefixes)]
        components[component] = hashlib.sha256(b"\0".join(rows)).hexdigest()
    return {"schema": 1, "source_sha": subprocess.check_output(["git", "-c", f"safe.directory={root}", "rev-parse", "HEAD"], cwd=root, text=True).strip(), "build_inputs_sha256": build_inputs(root), "component_inputs": components}


def same_identity(report, expected):
    if type(report.get("schema")) is not int or report["schema"] != 1:
        raise ValueError("Unsupported evidence schema")
    for key, size in (("source_sha", 40), ("build_inputs_sha256", 64)):
        value = report.get(key, "")
        if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{" + str(size) + "}", value) or value != expected.get(key):
            raise ValueError(f"Evidence {key} does not match frozen inputs")


def passing(record):
    if record.get("status") != "pass" or record.get("forced_kill") is not False or any(record.get(key) is True for key in ("timed_out", "deadline_exceeded", "helper_forced_termination")):
        raise ValueError("Required failure, inconclusive result or forced termination blocks release")


def validate_readiness(report, expected, now=None):
    same_identity(report, expected)
    if report.get("component_inputs") != expected.get("component_inputs"):
        raise ValueError("Stage A component input identity mismatch")
    passing(report)
    if report.get("stage") != "A":
        raise ValueError("Stage A completion evidence required before freeze")
    for key, required in (("automated", {"linux", "windows"}), ("native_functional", {"linux", "windows"})):
        rows = report.get(key, [])
        if len(rows) != len(required) or {row.get("platform") for row in rows} != required:
            raise ValueError(f"Complete {key} evidence is required")
        for row in rows:
            if key == "automated":
                same_identity(row, expected)
            elif row.get("component_inputs") != expected.get("component_inputs"):
                raise ValueError("Native functional evidence is stale for the current frontend/backend inputs")
            passing(row)
            if not re.fullmatch(r"[0-9a-f]{64}", row.get("binary_sha256", "")):
                raise ValueError("Stage A must identify its development binary")
            if key == "native_functional" and not re.fullmatch(r"[0-9a-f]{40}", row.get("source_sha", "")):
                raise ValueError("Native functional evidence must identify its source")
            if key == "native_functional" and (row.get("suite") != "native-functional" or not row.get("areas") or not row.get("evidence")):
                raise ValueError("Native functional checks of the affected areas require recorded evidence")
            if key == "native_functional" and row.get("source_sha") == expected["source_sha"]:
                automated = next(item for item in report["automated"] if item["platform"] == row["platform"])
                if row["binary_sha256"] != automated["binary_sha256"]:
                    raise ValueError("Native functional checks must use the Stage A development binary")
    labs = report.get("labs", [])
    if len(labs) != len(ENVIRONMENTS) or {row.get("environment") for row in labs} != set(ENVIRONMENTS):
        raise ValueError("All six native desktop environments must be ready")
    if len({(row.get("tiny_tiff_sha256"), row.get("tiny_tiff_width"), row.get("tiny_tiff_height")) for row in labs}) != 1:
        raise ValueError("All desktops must use the same fixed tiny TIFF")
    current = parsed(now or timestamp())
    for row in labs:
        passing(row)
        if row.get("ready") is not True or row.get("source_sha") != expected["source_sha"]:
            raise ValueError("Stage A lab readiness must be ready for the frozen source")
        if row.get("native_desktop") is not True or not row.get("tools"):
            raise ValueError("Native desktop, fixed tiny TIFF and tools must be ready before freeze")
        if any(not isinstance(tool, dict) or not tool.get("name") or not re.fullmatch(r"[0-9a-f]{64}", tool.get("sha256", "")) for tool in row["tools"]):
            raise ValueError("Stage A must pin every prepared smoke tool")
        path_type = PureWindowsPath if row["environment"].startswith("windows") else PurePosixPath
        if any(not path_type(tool.get("path", "")).is_absolute() for tool in row["tools"]):
            raise ValueError("Prepared observer tool paths must be absolute")
        for field in ("tiny_tiff_sha256", "helper_sha256"):
            if not re.fullmatch(r"[0-9a-f]{64}", row.get(field, "")):
                raise ValueError("Stage A must pin tiny TIFF and smoke helper bytes")
        if any(type(row.get(field)) is not int or not 1 <= row[field] <= 1024 for field in ("tiny_tiff_width", "tiny_tiff_height")):
            raise ValueError("Stage A requires a fixed tiny TIFF no larger than 1024 pixels per dimension")
        if not parsed(report["completed_at"]) <= current < parsed(row["expires_at"]):
            raise ValueError("Desktop lab readiness has expired or is from the future")
        if not row["environment"].startswith("windows"):
            validate_linux_preflight(row, report["completed_at"])


def validate_linux_preflight(lab, completed_at):
    """Require observed transfer/render/exit, rather than a declared ready flag."""
    probe = lab.get("preflight", {})
    passing(probe)
    for key in ("environment", "helper_sha256", "tiny_tiff_sha256"):
        if probe.get(key) != lab[key]:
            raise ValueError("Linux preflight differs from prepared " + key)
    runner = probe.get("runner_sha256", "")
    if not re.fullmatch(r"[0-9a-f]{64}", runner) or not any(tool["name"] == "linux-smoke.py" and tool["sha256"] == runner for tool in lab["tools"]):
        raise ValueError("Linux preflight invocation must be a pinned tool")
    for key in ("driver_plan_sha256", "smoke_report_sha256"):
        if not re.fullmatch(r"[0-9a-f]{64}", probe.get(key, "")):
            raise ValueError("Linux preflight must retain input/report hashes")
    for key, size in (("source_sha", 40), ("asset_sha256", 64)):
        if not re.fullmatch(r"[0-9a-f]{" + str(size) + "}", probe.get(key, "")):
            raise ValueError("Linux preflight must pin its rehearsal " + key)
    transfer = probe.get("transport", {})
    passing(transfer)
    expected_transport = "local" if lab["environment"] == "arch-omarchy" else "ssh-stdin"
    hashes = [transfer.get("sha256")] if expected_transport == "local" else [transfer.get("fixture_sha256"), transfer.get("returned_sha256")]
    if transfer.get("transport") != expected_transport or any(value != lab["tiny_tiff_sha256"] for value in hashes):
        raise ValueError("Linux preflight requires the fixed TIFF SSH roundtrip")
    smoke = probe.get("smoke_report", {})
    passing(smoke)
    if smoke.get("stage") != "C" or smoke.get("uid") in (None, 0):
        raise ValueError("Linux rehearsal must observe a native user desktop")
    for key in ("source_sha", "asset_sha256"):
        if smoke.get(key) != probe[key]:
            raise ValueError("Linux rehearsal differs from its pinned " + key)
    for key in ("environment", "helper_sha256", "tiny_tiff_sha256"):
        if smoke.get(key) != lab[key]:
            raise ValueError("Linux rehearsal differs from prepared " + key)
    if smoke.get("observer_tools") != lab["tools"]:
        raise ValueError("Linux rehearsal tools differ from prepared tools")
    if not parsed(smoke["started_at"]) <= parsed(smoke["finished_at"]) <= parsed(completed_at):
        raise ValueError("Linux rehearsal must finish before readiness")
    steps = smoke.get("steps", [])
    names = [step.get("name") for step in steps]
    if names not in (list(SMOKE_STEPS), [*SMOKE_STEPS, "fuse-fallback"]):
        raise ValueError("Linux preflight requires launch, render and clean exit")
    for step in steps:
        passing(step)
        duration = (parsed(step["finished_at"]) - parsed(step["started_at"])).total_seconds()
        if not 0 <= duration <= step.get("timeout_seconds", 0) <= STEP_LIMITS[step["name"]]:
            raise ValueError("Linux rehearsal exceeded its fixed step timeout")
    if lab["environment"] in ("arch-omarchy", "fedora-wayland") and steps[0].get("detail", {}).get("backend") != "wayland":
        raise ValueError("Linux rehearsal requires native Wayland for this desktop")
    validate_render(steps[1].get("detail", {}), lab)
    validate_native_exit(steps[0].get("detail", {}), steps[2].get("detail", {}), lab["environment"], smoke)
    if len(steps) == 4:
        fallback = steps[3].get("detail", {})
        validate_render(fallback.get("render", {}), lab)
        validate_native_exit(fallback.get("launch", {}), fallback.get("clean-exit", {}), lab["environment"], smoke)
        if lab["environment"] not in ("debian12", "ubuntu24") or fallback.get("launch", {}).get("launch_method") != "extract-and-run":
            raise ValueError("Linux rehearsal fallback is confined to Debian/Ubuntu extraction")


def validate_deadline(expected, now=None):
    start, deadline = parsed(expected["freeze_started_at"]), parsed(expected["deadline_at"])
    if (deadline - start).total_seconds() not in (1800, ATTEMPT_SECONDS):
        raise ValueError("Attempt deadline must be exactly 30 or 60 minutes after source freeze")
    if not start <= parsed(now or timestamp()) < deadline:
        raise ValueError("Frozen attempt deadline exceeded; blocked, not published; return to Stage A")


def deadline_remaining(expected):
    validate_deadline(expected)
    return (parsed(expected["deadline_at"]) - parsed(timestamp())).total_seconds()


def load_identity(path):
    result = read(path)
    same_identity(result, result)
    validate_deadline(result)
    return result


def validate_smoke_report(report, expected):
    same_identity(report, expected)
    passing(report)
    if report.get("stage") != "C" or report.get("environment") not in ENVIRONMENTS:
        raise ValueError("Fixed Stage C desktop smoke report required")
    for key in ("checksums_sha256", "freeze_started_at", "deadline_at"):
        if report.get(key) != expected.get(key):
            raise ValueError(f"Smoke report differs from frozen {key}")
    validate_deadline(expected)
    start, finish = parsed(report["started_at"]), parsed(report["finished_at"])
    if not parsed(expected["freeze_started_at"]) <= start <= finish < parsed(expected["deadline_at"]):
        raise ValueError("Smoke ran outside the frozen attempt deadline")
    if finish > parsed(timestamp()):
        raise ValueError("Smoke report cannot finish in the future")
    asset = expected["assets"][report["environment"]]
    if report.get("asset_sha256") != asset["sha256"]:
        raise ValueError("Native smoke did not run the exact candidate asset")
    for field in ("tiny_tiff_sha256", "helper_sha256"):
        if report.get(field) != asset.get(field):
            raise ValueError("Native smoke helper or fixed TIFF differs from Stage A readiness")
    if report.get("observer_tools") != asset.get("tools"):
        raise ValueError("Observed tools differ from Stage A prepared tool identities")
    attempts = report.get("attempts")
    if type(attempts) is not int or attempts not in (1, 2) or (attempts == 2 and report.get("retry_reason") != "transient-lab"):
        raise ValueError("Only one unchanged-byte transient lab retry is permitted")
    if attempts == 2:
        previous = report.get("prior_attempt", {})
        if previous.get("status") not in ("fail", "inconclusive") or previous.get("attempts") != 1 or not report.get("transient_diagnosis"):
            raise ValueError("Retry requires retained failed first receipt and an identified transient cause")
        if previous.get("forced_kill") is not False or any(previous.get(key) is True for key in ("timed_out", "deadline_exceeded", "helper_forced_termination")):
            raise ValueError("Timeout or forced termination ends the attempt and cannot be retried")
        for step in previous.get("steps", []):
            if step.get("forced_kill") is not False or step.get("timed_out") is True or (parsed(step.get("finished_at")) - parsed(step.get("started_at"))).total_seconds() >= step.get("timeout_seconds", 0):
                raise ValueError("A timed-out or forced native step cannot be retried")
        for field in ("source_sha", "build_inputs_sha256", "checksums_sha256", "asset_sha256", "environment", "helper_sha256", "tiny_tiff_sha256", "observer_tools", "freeze_started_at", "deadline_at"):
            if previous.get(field) != report.get(field):
                raise ValueError("Retry changed source, helper, fixture, bytes or frozen attempt")
        previous_start, previous_finish = parsed(previous["started_at"]), parsed(previous["finished_at"])
        if not parsed(expected["freeze_started_at"]) <= previous_start <= previous_finish <= start:
            raise ValueError("Retained failed attempt timestamps do not precede unchanged-byte retry")
    steps = report.get("steps", [])
    required_steps = list(SMOKE_STEPS) + (["fuse-fallback"] if asset.get("fuse_fallback_required") else [])
    if [row.get("name") for row in steps] != required_steps:
        raise ValueError("Stage C is limited to launch, render and clean exit")
    for row in steps:
        passing(row)
        if row.get("timed_out") is True:
            raise ValueError("A timed-out native step cannot pass")
        duration = (parsed(row["finished_at"]) - parsed(row["started_at"])).total_seconds()
        cap = STEP_LIMITS[row["name"]]
        if row["name"] == "launch" and report["environment"].startswith("windows"):
            cap = 180 if report["environment"] == "windows11-setup" else 600
        if not 0 <= duration <= row.get("timeout_seconds", 0) <= cap:
            raise ValueError("Native smoke step exceeded its bounded timeout")
        if not start <= parsed(row["started_at"]) <= parsed(row["finished_at"]) <= finish:
            raise ValueError("Native smoke step timestamps lie outside the report")
    render = steps[1].get("detail", {})
    validate_render(render, asset)
    validate_native_exit(steps[0].get("detail", {}), steps[2].get("detail", {}), report["environment"], expected)
    if asset.get("fuse_fallback_required"):
        fallback = steps[3].get("detail", {})
        validate_render(fallback.get("render", {}), asset)
        validate_native_exit(fallback.get("launch", {}), fallback.get("clean-exit", {}), report["environment"], expected)
        if fallback.get("launch", {}).get("launch_method") != "extract-and-run":
            raise ValueError("Extraction fallback must render and close naturally with exact bytes")
    launch = steps[0].get("detail", {})
    environment = report["environment"]
    if environment.startswith("windows"):
        if "Windows 11" not in report.get("os", {}).get("Caption", "") or report.get("privilege", {}).get("is_admin") is not False or report.get("privilege", {}).get("session_id", 0) <= 0 or launch.get("backend") != "native-Windows-WebView2":
            raise ValueError("Windows 11 native user desktop smoke required")
        if environment == "windows11-setup" and (launch.get("installer_exit_code") != 0 or launch.get("installed_marker_verified") is not True):
            raise ValueError("Setup must install successfully without elevation")
        if environment == "windows11-portable" and (launch.get("cold_baseline") is not True or not re.fullmatch(r"[0-9a-f]{64}", launch.get("incoming_payload_id", ""))):
            raise ValueError("Portable smoke requires exact incoming clean extraction baseline")
    else:
        if report.get("uid") in (None, 0) or not launch.get("native_windows") or launch.get("backend") not in ("wayland", "x11"):
            raise ValueError("Native Linux user desktop smoke required")
        if environment in ("fedora-wayland", "arch-omarchy") and launch["backend"] != "wayland":
            raise ValueError("Native Wayland backend required for this environment")


def validate_native_exit(launch, close, environment, expected):
    pid = launch.get("pid")
    if type(pid) is not int or pid <= 0 or type(close.get("exit_code")) is not int or close["exit_code"] != 0:
        raise ValueError("Normal native close must identify its process and produce exit code zero")
    if environment.startswith("windows"):
        if close.get("pid") != pid or close.get("forced_cleanup") is not False or close.get("action") != "actual mouse SendInput" or type(launch.get("hwnd")) is not int or launch["hwnd"] <= 0:
            raise ValueError("Windows close must observe its owned native window and process")
        info = launch.get("embedded_buildinfo", {})
    else:
        launcher = launch.get("launcher_pid")
        if type(launcher) is not int or launcher <= 0 or close.get("pid") != launcher or close.get("app_pid") != pid or close.get("app_exited") is not True or close.get("exit_code_source") != "exact AppImage launcher wait":
            raise ValueError("AppImage close must retain exact launcher wait and native process exit")
        if launch.get("backend") not in ("wayland", "x11") or not any(row.get("role") in ("frame", "window") for row in launch.get("native_windows", [])):
            raise ValueError("Owned native Linux window and backend must be observed")
        info = launch.get("process_identity", {}).get("buildinfo", {})
    if info.get("source_sha") != expected["source_sha"]:
        raise ValueError("Observed native process build source differs from frozen candidate")


def validate_render(render, asset):
    observation = render.get("observation", {})
    state = observation.get("state", {})
    pixels = render.get("pixel_observation", {})
    screenshot_hash = render.get("screenshot_sha256", pixels.get("screenshot_sha256", ""))
    if render.get("image_input") != "startup image command line" or observation.get("rendered_canvas") is not True:
        raise ValueError("Native smoke must open its fixed TIFF through the startup path and observe its visible canvas")
    if state.get("phase") != "ready" or state.get("previewReady") is not True or observation.get("error") is not False or observation.get("phase") != "ready":
        raise ValueError("Native smoke must observe ready image state without errors")
    spread = pixels.get("spread")
    if any(type(state.get(key)) is not int for key in ("width", "height")) or (state.get("width"), state.get("height")) != (asset["tiny_tiff_width"], asset["tiny_tiff_height"]) or type(spread) not in (int, float) or not math.isfinite(spread) or spread < 35 or not isinstance(screenshot_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", screenshot_hash):
        raise ValueError("Native TIFF dimensions and visible pixels must be recorded")


def validate_acceptance(report, expected):
    same_identity(report, expected)
    passing(report)
    if report.get("stage") != "C" or report.get("checksums_sha256") != expected.get("checksums_sha256"):
        raise ValueError("Exact candidate acceptance required")
    rows = report.get("reports", [])
    if len(rows) != len(ENVIRONMENTS) or {row.get("environment") for row in rows} != set(ENVIRONMENTS):
        raise ValueError("All six required package environments must pass")
    if sum(row.get("attempts") == 2 for row in rows) > 1:
        raise ValueError("At most one transient retry is permitted for the complete release attempt")
    for row in rows:
        validate_smoke_report(row, expected)


def frozen(readiness, readiness_run_id, prepared_directory, packaging_run_id, source_root=ROOT):
    current = identity(source_root)
    validate_readiness(readiness, current)
    if not re.fullmatch(r"[1-9][0-9]*", str(packaging_run_id)):
        raise ValueError("Successful packaging preparation run required")
    spec = importlib.util.spec_from_file_location("prepared_inputs", Path(__file__).with_name("prepared-inputs.py"))
    prepared = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(prepared)
    result = {**current, "stage": "B", "readiness": readiness,
              "readiness_run_id": str(readiness_run_id), "packaging_run_id": str(packaging_run_id),
              "validated_inputs": {}}
    manifests = []
    for platform in ("linux", "windows"):
        path = Path(prepared_directory) / f"prepared-{platform}.json"
        manifest = read(path)
        result["validated_inputs"][platform] = {"manifest_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            **{key: manifest.get(key) for key in prepared.FIELDS}}
        manifests.append(prepared.validate(path, result, platform))
    if manifests[0]["validation_run_id"] != manifests[1]["validation_run_id"]:
        raise ValueError("Prepared platforms reference different automated validation runs")
    if prepared.common_source_coverage(manifests[0]) != prepared.common_source_coverage(manifests[1]):
        raise ValueError("Prepared platforms reference different source material")
    prepared.preparation_run(str(packaging_run_id))
    trusted_run(manifests[0]["validation_run_id"], "native-validation.yml", current["source_sha"])
    start = timestamp()
    return {**result, "freeze_started_at": start, "deadline_at": (parsed(start) + dt.timedelta(seconds=ATTEMPT_SECONDS)).isoformat().replace("+00:00", "Z")}


def validate_source_receipt(plan, expected, attempt_sha256):
    same_identity(expected, expected)
    proof = plan.get("assembly_attempt", {})
    if not isinstance(proof, dict):
        raise ValueError("Stage B source assembly proof required")
    passing(proof)
    same_identity(proof, expected)
    if expected.get("stage") != "B" or proof.get("stage") != "B" or any(not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value) for value in (attempt_sha256, plan.get("sha256", ""), proof.get("archive_sha256", ""), proof.get("refresh_receipt_sha256", ""))):
        raise ValueError("Stage B source assembly and valid archive/attempt identities required")
    for key in ("source_sha", "build_inputs_sha256", "freeze_started_at", "deadline_at"):
        if proof.get(key) != expected.get(key):
            raise ValueError("Source assembly belongs to another frozen attempt")
    if proof.get("attempt_sha256") != attempt_sha256 or proof.get("archive_sha256") != plan.get("sha256"):
        raise ValueError("Source assembly receipt does not identify frozen attempt and archive bytes")
    start, finish = parsed(proof.get("started_at")), parsed(proof.get("finished_at"))
    if not parsed(expected["freeze_started_at"]) <= start <= finish < parsed(expected["deadline_at"]) or finish > parsed(timestamp()):
        raise ValueError("Corresponding source preparation ran outside original release deadline")
    validate_deadline(expected)


def candidate_identity(expected, directory):
    from release import ASSETS, digest
    validate_deadline(expected)
    result = {**expected, "checksums_sha256": digest(directory / "SHA256SUMS"), "source_review_sha256": digest(directory / "Aperture-Corresponding-Source.json"), "assets": {}}
    for environment in ENVIRONMENTS:
        name = ASSETS["windows-x86_64" if environment == "windows11-setup" else "windows-x86_64-portable" if environment == "windows11-portable" else "linux-x86_64"]
        lab = next(row for row in expected["readiness"]["labs"] if row["environment"] == environment)
        result["assets"][environment] = {"name": name, "sha256": digest(directory / name), "fuse_fallback_required": environment in ("debian12", "ubuntu24"), "tools": lab["tools"], **{field: lab[field] for field in ("tiny_tiff_sha256", "tiny_tiff_width", "tiny_tiff_height", "helper_sha256")}}
    return result


def compact_reports(expected, reports):
    """Keep full native diagnostics locally; submit bounded decisions and raw hashes."""
    from release import digest
    result = []
    def trim(report):
        row = {key: value for key, value in report.items() if key not in ("steps", "prior_attempt")}
        row["steps"] = []
        for step in report.get("steps", []):
            detail = step.get("detail", {})
            def launch(value):
                value = dict(value)
                if "native_windows" in value:
                    value["native_windows"] = [row for row in value["native_windows"] if row.get("role") in ("frame", "window")][:1]
                return value
            def render(value):
                observation = value.get("observation", {})
                state = observation.get("state", {})
                return {"observation": {"phase": observation.get("phase"), "error": observation.get("error"), "rendered_canvas": observation.get("rendered_canvas"), "state": {key: state.get(key) for key in ("phase", "previewReady", "path", "width", "height")}}, "pixel_observation": {key: val for key, val in value.get("pixel_observation", {}).items() if key != "samples"}, **{key: value[key] for key in ("screenshot", "screenshot_sha256", "image_input") if key in value}}
            if step["name"] == "launch": detail = launch(detail)
            elif step["name"] == "render": detail = render(detail)
            elif step["name"] == "fuse-fallback": detail = {"launch": launch(detail.get("launch", {})), "render": render(detail.get("render", {})), "clean-exit": detail.get("clean-exit", {})}
            row["steps"].append({**step, "detail": detail})
        if report.get("prior_attempt"):
            row["prior_attempt"] = trim(report["prior_attempt"])
        return row
    for environment in ENVIRONMENTS:
        path = reports / f"{environment}.json"
        original = read(path)
        validate_smoke_report(original, expected)
        row = trim(original)
        row["raw_report_sha256"] = digest(path)
        validate_smoke_report(row, expected)
        result.append(row)
    if len(json.dumps(result).encode()) > 60000:
        raise ValueError("Compact dispatch evidence exceeds input limit; stop, do not truncate assertions")
    validate_acceptance({**expected, "stage": "C", "status": "pass", "forced_kill": False, "reports": result}, expected)
    return result


def run_bounded(expected, seconds, command, report_path):
    limit = min(seconds, deadline_remaining(expected))
    started = timestamp()
    forced = False
    error = None
    try:
        process = subprocess.Popen(command, start_new_session=os.name != "nt")
        code = process.wait(timeout=limit)
    except subprocess.TimeoutExpired:
        forced = True
        if os.name == "nt":
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(process.pid)], check=False, timeout=15)
        else:
            import signal
            os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=15)
        code = 1
    except OSError as exc:
        code = 1
        error = str(exc)
    finished = timestamp()
    expired = parsed(finished) >= parsed(expected["deadline_at"])
    report = {**{key: expected[key] for key in ("schema", "source_sha", "build_inputs_sha256", "freeze_started_at", "deadline_at")}, "stage": "B", "status": "pass" if code == 0 and not forced and not expired else "fail", "forced_kill": forced, "started_at": started, "finished_at": finished, "elapsed_seconds": (parsed(finished) - parsed(started)).total_seconds(), "timeout_seconds": limit, "command": command, "error": error, "deadline_exceeded": expired}
    write_report(report_path, report)
    return code or (1 if expired else 0)


def trusted_run(run_id, workflow, source_sha=None):
    if not re.fullmatch(r"[1-9][0-9]*", str(run_id)):
        raise ValueError("Workflow run ID must be a positive integer")
    repository = "neuroqp/neuroqp-aperture"
    result = json.loads(subprocess.check_output(["gh", "api", f"repos/{repository}/actions/runs/{run_id}"], text=True, timeout=30))
    if result.get("repository", {}).get("full_name") != repository or result.get("conclusion") != "success" or result.get("event") != "workflow_dispatch" or result.get("path") != f".github/workflows/{workflow}":
        raise ValueError("Successful trusted release workflow evidence is required")
    if workflow in ("release.yml", "resume-release.yml", "release-evidence.yml") and result.get("head_branch") != "main":
        raise ValueError("Release and evidence workflows must run from trusted main")
    if workflow in ("release.yml", "resume-release.yml") and result.get("run_attempt") != 1:
        raise ValueError("Final package workflow reruns are forbidden")
    if source_sha is not None and result.get("head_sha") != source_sha:
        raise ValueError("Stage A validation run checked another source commit")
    return result


def trusted_candidate(run_id, candidate):
    """Only normal successful builds or this reviewed, explicit completion lineage."""
    recovery = candidate.get("recovery")
    workflow = "resume-release.yml" if recovery is not None else "release.yml"
    result = trusted_run(run_id, workflow)
    if recovery is not None:
        from resume_publication import validate_lineage
        validate_lineage(candidate, result)
    return result


def submit(input_path, output):
    stage = os.environ["EVIDENCE_STAGE"]
    evidence = json.loads(os.environ["REPORT_JSON"])
    expected = identity()
    if stage == "A":
        trusted_run(os.environ["INPUT_RUN_ID"], "native-validation.yml", expected["source_sha"])
    if stage == "A":
        evidence["automated"] = []
        for platform in ("linux", "windows"):
            matches = list(input_path.rglob(f"stage-a-{platform}.json"))
            if len(matches) != 1:
                raise ValueError("Missing or duplicate Stage A automated evidence")
            evidence["automated"].append(read(matches[0]))
        validate_readiness(evidence, expected)
        write_report(output / "release-readiness.json", evidence)
    elif stage == "B":
        if str(evidence.get("readiness_run_id")) != os.environ["INPUT_RUN_ID"]:
            raise ValueError("Frozen attempt must reference the supplied readiness run")
        if evidence.get("readiness") != read(input_path / "release-readiness.json"):
            raise ValueError("Frozen readiness differs from the exact trusted Stage A artifact")
        subprocess.run(["git", "merge-base", "--is-ancestor", "HEAD", "origin/main"], cwd=ROOT, check=True, timeout=30)
        same_identity(evidence, expected)
        validate_deadline(evidence)
        validate_readiness(evidence["readiness"], expected)
        trusted_run(evidence["readiness_run_id"], "release-evidence.yml")
        write_report(output / "frozen-attempt.json", evidence)
    elif stage == "C":
        frozen_identity = load_identity(input_path / "frozen-candidate.json")
        trusted_candidate(os.environ["INPUT_RUN_ID"], frozen_identity)
        same_identity(frozen_identity, expected)
        report = {**{key: frozen_identity[key] for key in ("schema", "source_sha", "build_inputs_sha256", "checksums_sha256", "freeze_started_at", "deadline_at")}, "stage": "C", "status": "pass", "forced_kill": False, "candidate_run_id": os.environ["INPUT_RUN_ID"], "reports": evidence}
        validate_acceptance(report, frozen_identity)
        write_report(output / "desktop-acceptance.json", report)
    else:
        raise ValueError("Only Stage A readiness, frozen Stage B or Stage C acceptance may be submitted")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("trusted-run"); p.add_argument("run_id"); p.add_argument("workflow")
    p = sub.add_parser("submit"); p.add_argument("input", type=Path); p.add_argument("output", type=Path)
    p = sub.add_parser("identity"); p.add_argument("output", type=Path)
    p = sub.add_parser("automated"); p.add_argument("platform"); p.add_argument("binary", type=Path); p.add_argument("output", type=Path)
    p = sub.add_parser("readiness"); p.add_argument("evidence", type=Path); p.add_argument("output", type=Path)
    p = sub.add_parser("freeze"); p.add_argument("readiness", type=Path); p.add_argument("output", type=Path); p.add_argument("--readiness-run-id", required=True)
    p.add_argument("--prepared-directory", type=Path, required=True); p.add_argument("--packaging-run-id", required=True); p.add_argument("--source-root", type=Path, default=ROOT)
    p = sub.add_parser("source-receipt"); p.add_argument("identity", type=Path); p.add_argument("plan", type=Path); p.add_argument("--plan-sha256", required=True)
    p = sub.add_parser("candidate"); p.add_argument("identity", type=Path); p.add_argument("directory", type=Path); p.add_argument("output", type=Path)
    p = sub.add_parser("acceptance"); p.add_argument("identity", type=Path); p.add_argument("reports", type=Path); p.add_argument("output", type=Path)
    p = sub.add_parser("compact"); p.add_argument("identity", type=Path); p.add_argument("reports", type=Path); p.add_argument("output", type=Path)
    p = sub.add_parser("deadline"); p.add_argument("identity", type=Path)
    p = sub.add_parser("run"); p.add_argument("identity", type=Path); p.add_argument("report", type=Path); p.add_argument("--seconds", type=int, required=True); p.add_argument("argv", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command == "trusted-run": trusted_run(args.run_id, args.workflow)
    elif args.command == "submit": submit(args.input, args.output)
    elif args.command == "identity": write_report(args.output, identity())
    elif args.command == "automated":
        from release import digest
        write_report(args.output, {**identity(), "stage": "A", "platform": args.platform, "status": "pass", "forced_kill": False, "binary_sha256": digest(args.binary), "completed_at": timestamp()})
    elif args.command == "readiness":
        result = read(args.evidence / "native-functional-evidence.json")
        result["automated"] = [read(args.evidence / f"stage-a-{platform}.json") for platform in ("linux", "windows")]
        validate_readiness(result, identity()); write_report(args.output, result)
    elif args.command == "freeze": write_report(args.output, frozen(read(args.readiness), args.readiness_run_id, args.prepared_directory, args.packaging_run_id, args.source_root))
    elif args.command == "source-receipt":
        from release import source_plan
        body = args.identity.read_bytes()
        validate_source_receipt(source_plan(args.plan, args.plan_sha256), json.loads(body.decode("utf-8-sig")), hashlib.sha256(body).hexdigest())
    elif args.command == "candidate": write_report(args.output, candidate_identity(load_identity(args.identity), args.directory))
    elif args.command == "acceptance":
        expected = load_identity(args.identity)
        rows = [read(args.reports / f"{name}.json") for name in ENVIRONMENTS]
        result = {**{key: expected[key] for key in ("schema", "source_sha", "build_inputs_sha256", "checksums_sha256", "freeze_started_at", "deadline_at")}, "stage": "C", "status": "pass", "forced_kill": False, "reports": rows}
        validate_acceptance(result, expected); write_report(args.output, result)
    elif args.command == "compact": write_report(args.output, compact_reports(load_identity(args.identity), args.reports))
    elif args.command == "deadline": validate_deadline(read(args.identity))
    elif args.command == "run":
        command = args.argv[1:] if args.argv[:1] == ["--"] else args.argv
        if not command or args.seconds <= 0: parser.error("A command and positive timeout are required")
        sys.exit(run_bounded(load_identity(args.identity), args.seconds, command, args.report))


if __name__ == "__main__":
    main()
