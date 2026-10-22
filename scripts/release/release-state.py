#!/usr/bin/env python3
"""Durable identities for validate, package and idempotent publication."""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import zipfile

from release import ASSETS, current_version, digest, lockfile_hashes, native_identity, notes, source_plan, verify_source_material, verify, verify_signatures

REPO = "neuroqp/neuroqp-aperture"
# Producer runs must have called this gate. Rename it when older gate generations must stop
# qualifying: the earlier trusted-source.yml did not verify the runs its callers consumed.
TRUST_GATE = ".github/workflows/trusted-inputs.yml"
ENVIRONMENTS = {"windows11-setup", "windows11-portable", "debian12", "ubuntu24-wayland"}
FUNCTIONAL_ENVIRONMENTS = {"windows11", "debian12", "ubuntu24-wayland"}


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def write(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    body = json.dumps(value, indent=2, sort_keys=True) + "\n"
    if path.exists() and path.read_text(encoding="utf-8") != body:
        raise ValueError("Refusing to replace different evidence: " + str(path))
    path.write_text(body, encoding="utf-8")


def hex_value(value, length=64):
    if not isinstance(value, str) or not re.fullmatch(f"[0-9a-f]{{{length}}}", value):
        raise ValueError("Invalid SHA identity")
    return value


def source_identity(source_sha):
    hex_value(source_sha, 40)
    actual = subprocess.check_output(["git", "-c", f"safe.directory={Path.cwd()}", "rev-parse", "HEAD"], text=True).strip()
    if actual != source_sha:
        raise ValueError("Checkout differs from requested source")
    return {"schema": 1, "source_sha": source_sha, "version": current_version(), "lockfiles": lockfile_hashes(Path.cwd())}


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True, timeout=600)


def main_commit(commit):
    """Return a full commit SHA only when the current main branch contains it."""
    hex_value(commit, 40)
    head = json.loads(gh("api", f"repos/{REPO}/git/ref/heads/main"))
    if head.get("ref") != "refs/heads/main" or head.get("object", {}).get("type") != "commit":
        raise ValueError("Unexpected main branch reference")
    # Compare with the pinned head so tags or branches named like either side cannot redirect it.
    main = hex_value(head["object"].get("sha"), 40)
    comparison = json.loads(gh("api", f"repos/{REPO}/compare/{commit}...{main}"))
    if (comparison.get("status") not in ("ahead", "identical") or comparison.get("behind_by") != 0
            or comparison.get("base_commit", {}).get("sha") != commit
            or comparison.get("merge_base_commit", {}).get("sha") != commit):
        raise ValueError("Source commit must be contained in main")
    return commit


def trusted_run(run_id, workflow):
    if not isinstance(run_id, str) or not re.fullmatch(r"[1-9][0-9]*", run_id):
        raise ValueError("Expected a workflow run ID")
    row = json.loads(gh("api", f"repos/{REPO}/actions/runs/{run_id}"))
    if (row.get("repository", {}).get("full_name") != REPO or row.get("head_branch") != "main"
            or row.get("event") != "workflow_dispatch" or row.get("path") != ".github/workflows/" + workflow
            or row.get("status") != "completed" or row.get("conclusion") != "success"):
        raise ValueError("Successful trusted main workflow required")
    # GitHub records each reusable workflow a run called. Workflows predating the gate cannot
    # add this record, unlike artifacts, logs or run names, so the producer verified its inputs.
    head = row.get("head_sha")
    gate = {"path": f"{REPO}/{TRUST_GATE}@{head}", "ref": "refs/heads/main", "sha": head}
    calls = row.get("referenced_workflows")
    if not isinstance(calls, list) or not any(
            isinstance(call, dict) and all(call.get(key) == value for key, value in gate.items()) for call in calls):
        raise ValueError("Workflow run did not call the trusted input gate at its own main revision")
    # Tags report their name as head_branch, so also require the workflow revision in main.
    main_commit(head)
    return row


def trusted_inputs(source_sha, run_workflow, run_id):
    """Verify dispatched inputs with the calling workflow revision before any source checkout.

    Raises unless every input holds; returns nothing, so the gate publishes only gate_outputs.
    """
    main_commit(source_sha)
    # The caller states whether a referenced run is required; an empty run ID never makes it optional.
    if run_workflow:
        trusted_run(run_id, run_workflow)
    elif run_id:
        raise ValueError("Workflow run ID supplied without its expected workflow")


def gate_outputs(source_sha, run_id):
    """Format the gate's only step outputs: the verified commit and the run ID or empty."""
    if not re.fullmatch(r"(?:[1-9][0-9]*)?", run_id):
        raise ValueError("Expected a workflow run ID or none")
    return f"revision={hex_value(source_sha, 40)}\nrun_id={run_id}\n"


def automated(platform, executable, info_path, output, source_sha):
    info = read(info_path)
    identity = source_identity(source_sha)
    if any(info.get(key) != identity[key] for key in ("source_sha", "version", "lockfiles")) or info.get("platform") != platform:
        raise ValueError("BUILDINFO differs from tested source")
    write(output, {**identity, "operation": "build", "status": "pass", "platform": platform,
                   "binary_sha256": digest(executable), "native_dependencies_sha256": native_identity(info["native_dependencies"])})


def acceptance(report, identity, smoke=False):
    if report.get("schema") != 1 or report.get("source_sha") != identity["source_sha"] or report.get("operation") != ("smoke" if smoke else "validate"):
        raise ValueError("Native evidence source/operation differs")
    if smoke and report.get("checksums_sha256") != identity["checksums_sha256"]:
        raise ValueError("Native smoke used different package bytes")
    rows = report.get("reports", [])
    required = ENVIRONMENTS if smoke else FUNCTIONAL_ENVIRONMENTS
    if len(rows) != len(required) or {row.get("environment") for row in rows} != required:
        raise ValueError("Required native environments are missing or duplicated")
    for row in rows:
        if row.get("schema") != 1 or row.get("operation") != report["operation"]:
            raise ValueError("Native report schema/operation differs")
        if row.get("source_sha") != identity["source_sha"]:
            raise ValueError("Native report belongs to a different source commit")
        if smoke and row.get("checksums_sha256") != identity["checksums_sha256"]:
            raise ValueError("Native report package checksums differ")
        if row.get("status") != "pass" or row.get("forced_kill") is not False:
            raise ValueError("Native check failed or required forced termination")
        platform = "windows" if row["environment"].startswith("windows") else "linux"
        key = "asset_sha256" if smoke else "binary_sha256"
        if smoke:
            name = "Aperture-Setup-x64.exe" if row["environment"] == "windows11-setup" else "Aperture-Portable-x64.exe" if platform == "windows" else "Aperture-x86_64.AppImage"
            expected = identity["assets"][name]
        else:
            expected = identity["platforms"][platform]["binary_sha256"]
        if row.get(key) != expected:
            raise ValueError("Native evidence executable/package differs")
        steps = row.get("steps", [])
        if [step.get("name") for step in steps] != ["launch", "render", "clean-exit"] or any(step.get("status") != "pass" for step in steps):
            raise ValueError("Native launch/render/clean-exit evidence is incomplete")
        rendered = steps[1].get("detail", {})
        observation = rendered.get("observation", {})
        state = observation.get("state", {})
        pixels = rendered.get("pixel_observation", {})
        closed = steps[2].get("detail", {})
        if (observation.get("phase") != "ready" or observation.get("rendered_canvas") is not True
                or state.get("phase") != "ready" or state.get("previewReady") is not True
                or observation.get("error") or state.get("error")):
            raise ValueError("Native evidence lacks a ready rendered image")
        if not isinstance(pixels.get("spread"), (int, float)) or pixels["spread"] < 35 or not pixels.get("samples"):
            raise ValueError("Native evidence lacks visible rendered pixels")
        if closed.get("exit_code") != 0 or closed.get("forced_cleanup", False) is not False or (platform == "linux" and closed.get("app_exited") is not True):
            raise ValueError("Native evidence requires normal application close with exit 0")
    return report


def validation(directory, source_sha, build_run_id, native_report, runtime_archive, output):
    identity = source_identity(source_sha)
    platforms = {}
    for platform in ("linux", "windows"):
        report = read(directory / f"build-{platform}.json")
        if report.get("operation") != "build" or report.get("status") != "pass" or any(report.get(key) != identity[key] for key in ("source_sha", "version", "lockfiles")):
            raise ValueError("Exact-source successful build evidence required")
        platforms[platform] = {key: report[key] for key in ("binary_sha256", "native_dependencies_sha256")}
    plan = source_plan(directory / "source/Aperture-Corresponding-Source.json")
    verify_source_material(directory / "source", plan, root=Path.cwd(), source_sha=source_sha)
    if plan["native_dependencies_sha256"] != {platform: row["native_dependencies_sha256"] for platform, row in platforms.items()}:
        raise ValueError("Tested native dependencies lack corresponding source")
    result = {**identity, "operation": "validate", "build_run_id": str(build_run_id), "platforms": platforms,
              "linux_runtime_sha256": digest(runtime_archive), "windows_archive_sha256": digest(directory / "development-windows.zip")}
    acceptance(read(native_report), result)
    result["native_evidence_sha256"] = digest(native_report)
    result["source_assets"] = {p.name: digest(p) for p in sorted((directory / "source").iterdir()) if p.is_file()}
    if not result["source_assets"]:
        raise ValueError("Corresponding source is missing")
    write(output, result)


def extract(archive_path, destination):
    """Extract trusted hashed runtime archives without traversal or link writes."""
    destination = Path(destination)
    with (zipfile.ZipFile(archive_path) if zipfile.is_zipfile(archive_path) else tarfile.open(archive_path, "r:gz")) as archive:
        zipped = isinstance(archive, zipfile.ZipFile)
        for entry in archive.infolist() if zipped else archive.getmembers():
            name = entry.filename.rstrip("/") if zipped else entry.name
            path = PurePosixPath(name)
            if not name or path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name or str(path) != name:
                raise ValueError("Unsafe runtime archive entry")
            if (not zipped and not (entry.isfile() or entry.isdir())) or (zipped and (entry.external_attr >> 16) & 0o170000 == 0o120000):
                raise ValueError("Runtime archive links are not supported")
            target = destination / name
            if any(p.is_symlink() for p in (target, *target.parents)):
                raise ValueError("Runtime destination contains symlink")
        if zipped:
            archive.extractall(destination)
        else:
            archive.extractall(destination, filter="data")


def stage(directory, platform, source_sha):
    validated = read(directory / "validation.json")
    identity = source_identity(source_sha)
    if any(validated.get(key) != identity[key] for key in identity) or validated.get("operation") != "validate":
        raise ValueError("Validated inputs differ from checkout")
    for name, sha in validated["source_assets"].items():
        if digest(directory / "source" / name) != sha:
            raise ValueError("Source coverage bytes differ")
    expected = validated["platforms"][platform]
    if platform == "linux":
        from importlib.util import spec_from_file_location, module_from_spec
        spec = spec_from_file_location("linux_runtime", Path(__file__).with_name("linux-runtime.py"))
        runtime = module_from_spec(spec)
        spec.loader.exec_module(runtime)
        runtime.stage(directory / "linux-runtime.tar.gz", Path("build/linux-prepared"), validated["linux_runtime_sha256"],
                      {"source_sha": source_sha, **expected})
    else:
        archive = directory / "development-windows.zip"
        if digest(archive) != validated["windows_archive_sha256"]:
            raise ValueError("Validated Windows runtime archive differs")
        shutil.copytree(directory / "dist", "dist")
        payload = Path("build/windows/payload")
        extract(archive, payload)
        info = read(payload / "BUILDINFO-windows.json")
        if (any(info.get(key) != identity[key] for key in ("source_sha", "version", "lockfiles"))
                or native_identity(info["native_dependencies"]) != expected["native_dependencies_sha256"]
                or digest(payload / "aperture.exe") != expected["binary_sha256"]):
            raise ValueError("Validated Windows executable/runtime identity differs")
        executable = Path("src-tauri/target/x86_64-pc-windows-msvc/release/aperture.exe")
        executable.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(payload / "aperture.exe", executable)
        (payload / "aperture.exe").unlink()
        release = Path("build/release")
        release.mkdir(parents=True, exist_ok=True)
        for name, alias in (("BUILDINFO-windows.json", "BUILDINFO.json"), ("THIRD-PARTY-NOTICES-windows.txt", "THIRD-PARTY-NOTICES.txt")):
            shutil.copy2(payload / name, release / name)
            shutil.copy2(payload / name, payload / alias)
        for name in (".aperture-installed", "DEVELOPMENT-SOURCE.json"):
            (payload / name).unlink(missing_ok=True)


def package_manifest(directory, validation_path, output):
    validated = read(validation_path)
    if source_identity(validated["source_sha"]) != {key: validated[key] for key in ("schema", "source_sha", "version", "lockfiles")}:
        raise ValueError("Package checkout differs from validation")
    verify(directory, validated["version"], validated["source_sha"], root=Path.cwd())
    verify_signatures(directory)
    for name, sha in validated["source_assets"].items():
        if digest(directory / name) != sha:
            raise ValueError("Source material differs from validation")
    write(output, {"schema": 1, "operation": "package", "source_sha": validated["source_sha"], "version": validated["version"],
                   "validation_sha256": digest(validation_path), "checksums_sha256": digest(directory / "SHA256SUMS"),
                   "assets": {p.name: digest(p) for p in sorted(directory.iterdir()) if p.is_file()}})


def existing_assets(existing, expected):
    names = [asset["name"] for asset in existing]
    if len(names) != len(set(names)) or not set(names) <= set(expected):
        raise ValueError("Existing release contains unexpected or duplicate assets")
    for asset in existing:
        if asset.get("digest") != "sha256:" + expected[asset["name"]]:
            raise ValueError("Existing release asset differs: " + asset["name"])
    return set(expected) - set(names)


def publish(directory, package_path, smoke_path):
    package = read(package_path)
    source_identity(package["source_sha"])
    if package.get("operation") != "package":
        raise ValueError("Package manifest required")
    acceptance(read(smoke_path), package, smoke=True)
    if {p.name: digest(p) for p in directory.iterdir() if p.is_file()} != package["assets"]:
        raise ValueError("Publish directory differs from tested package")
    verify(directory, package["version"], package["source_sha"], root=Path.cwd())
    verify_signatures(directory)
    tag = "v" + package["version"]
    ref = json.loads(gh("api", f"repos/{REPO}/git/ref/tags/{tag}"))["object"]
    while ref["type"] == "tag":
        ref = json.loads(gh("api", f"repos/{REPO}/git/tags/{ref['sha']}"))["object"]
    if ref["type"] != "commit" or ref["sha"] != package["source_sha"]:
        raise ValueError("Release tag differs from tested source")
    # Listing avoids treating a network/auth failure as an absent release.
    releases = json.loads(gh("api", "--paginate", "--slurp", f"repos/{REPO}/releases?per_page=100"))
    matches = [r for page in releases for r in page if r["tag_name"] == tag]
    if len(matches) > 1:
        raise ValueError("Duplicate GitHub release tag")
    if matches:
        release = matches[0]
    else:
        release = json.loads(gh("api", f"repos/{REPO}/releases", "--method", "POST",
                                "-f", "tag_name=" + tag, "-f", "name=Aperture " + package["version"],
                                "-f", "body=" + notes(package["version"]), "-F", "draft=true"))
    missing = existing_assets(release["assets"], package["assets"])
    if not release["draft"] and not release["prerelease"] and not missing:
        print(release["html_url"])
        return
    numbers = lambda value: tuple(map(int, value.removeprefix("v").split(".")))
    if any(not r["draft"] and not r["prerelease"] and re.fullmatch(r"v\d+\.\d+\.\d+", r["tag_name"])
           and numbers(r["tag_name"]) >= numbers(tag) for page in releases for r in page):
        raise ValueError("A newer or equal stable release already exists")
    if missing and not release["draft"]:
        raise ValueError("An incomplete public release is immutable")
    for name in sorted(missing):
        gh("release", "upload", tag, str(directory / name), "--repo", REPO)
    after = json.loads(gh("api", f"repos/{REPO}/releases/{release['id']}"))
    if existing_assets(after["assets"], package["assets"]):
        raise ValueError("Release upload incomplete; rerun publish with these same bytes")
    gh("release", "edit", tag, "--repo", REPO, "--draft=false", "--prerelease=false", "--latest", "--title", "Aperture " + package["version"])
    final = json.loads(gh("api", f"repos/{REPO}/releases/{release['id']}"))
    if final["draft"] or final["prerelease"] or existing_assets(final["assets"], package["assets"]):
        raise ValueError("Published release identity differs")
    latest = json.loads(gh("api", f"repos/{REPO}/releases/latest"))
    if latest["id"] != final["id"]:
        raise ValueError("GitHub latest release differs")
    from urllib.request import urlopen
    with urlopen(f"https://github.com/{REPO}/releases/download/{tag}/latest.json", timeout=60) as response:
        if response.read(2 * 1024 * 1024) != (directory / "latest.json").read_bytes():
            raise ValueError("Public updater feed differs from tested bytes")
    print(final["html_url"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    p = commands.add_parser("build")
    p.add_argument("platform", choices=("linux", "windows"))
    for name in ("executable", "buildinfo", "output"):
        p.add_argument(name, type=Path)
    p.add_argument("--source-sha", required=True)
    p = commands.add_parser("trusted-source")
    p.add_argument("source_sha")
    p.add_argument("--run-workflow", required=True, choices=("", "validate.yml", "package.yml"))
    p.add_argument("--run-id", required=True)
    p = commands.add_parser("validate")
    p.add_argument("directory", type=Path)
    p.add_argument("--source-sha", required=True)
    p.add_argument("--build-run-id", required=True)
    p.add_argument("--native-report", type=Path, required=True)
    p.add_argument("--runtime", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p = commands.add_parser("extract")
    p.add_argument("archive", type=Path)
    p.add_argument("destination", type=Path)
    p = commands.add_parser("stage")
    p.add_argument("directory", type=Path)
    p.add_argument("--platform", required=True, choices=("linux", "windows"))
    p.add_argument("--source-sha", required=True)
    p = commands.add_parser("package")
    p.add_argument("directory", type=Path)
    p.add_argument("validation", type=Path)
    p.add_argument("output", type=Path)
    p = commands.add_parser("publish")
    p.add_argument("directory", type=Path)
    p.add_argument("package", type=Path)
    p.add_argument("smoke", type=Path)
    args = parser.parse_args()
    if args.command == "build":
        automated(args.platform, args.executable, args.buildinfo, args.output, args.source_sha)
    elif args.command == "trusted-source":
        trusted_inputs(args.source_sha, args.run_workflow, args.run_id)
        print(gate_outputs(args.source_sha, args.run_id), end="")
    elif args.command == "validate":
        validation(args.directory, args.source_sha, args.build_run_id, args.native_report, args.runtime, args.output)
    elif args.command == "extract":
        extract(args.archive, args.destination)
    elif args.command == "stage":
        stage(args.directory, args.platform, args.source_sha)
    elif args.command == "package":
        package_manifest(args.directory, args.validation, args.output)
    else:
        publish(args.directory, args.package, args.smoke)


if __name__ == "__main__":
    main()
