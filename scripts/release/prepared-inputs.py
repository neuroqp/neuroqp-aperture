#!/usr/bin/env python3
"""Bind prepared packaging bytes to trusted Stage A evidence, then restore offline."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import zipfile

ROOT = Path.cwd()
sys.path.insert(0, str(Path(__file__).resolve().parent))
from release import current_version, digest, lockfile_hashes, native_identity, source_plan
from bounded import identity, passing, read, same_identity, trusted_run, write_report

REPOSITORY = "neuroqp/neuroqp-aperture"
FIELDS = ("binary_sha256", "native_dependencies_sha256")
SOURCE_MATERIAL_FIELDS = ("asset", "url", "sha256", "application_source_inventory_sha256",
                          "application_source_sha", "application_lockfiles_sha256", "native_dependencies_sha256")
PATHS = {
    "linux": ("node_modules", "dist", "build/packaging-node", "build/prepared/linux-runtime.tar.gz"),
    "windows": ("node_modules", "dist", "build/packaging-node", "build/windows/payload", "build/windows-packaging-tools.tar.gz", "build/packaging-cargo-home", "build/windows-packaging-rust",
                "build/release/BUILDINFO-windows.json", "build/release/THIRD-PARTY-NOTICES-windows.txt",
                "src-tauri/target/x86_64-pc-windows-msvc/release/aperture.exe"),
}


def hex_value(value, size):
    if not isinstance(value, str) or not re.fullmatch(f"[0-9a-f]{{{size}}}", value):
        raise ValueError("Invalid hexadecimal input identity")
    return value


def node_path(platform, root=ROOT):
    return root / "build/packaging-node" / ("node.exe" if platform == "windows" else "node")


def check_node(path, expected_sha256):
    if digest(path) != expected_sha256:
        raise ValueError("Prepared Node executable SHA256 differs")
    if subprocess.check_output([str(path.resolve()), "--version"], text=True, timeout=30).strip() != "v22.22.0":
        raise ValueError("Prepared Node executable version differs")


def prepare_node(platform):
    source = Path(subprocess.check_output(["node", "-p", "process.execPath"], text=True, timeout=30).strip()).resolve(strict=True)
    destination = node_path(platform)
    destination.parent.mkdir(parents=True, exist_ok=False)
    shutil.copy2(source, destination)
    check_node(destination, digest(source))


def source_material(plan, source_sha):
    return {key: plan.get(key, source_sha) if key == "application_source_sha" else plan[key]
            for key in SOURCE_MATERIAL_FIELDS}


def baseline_identity(plan, review_sha256):
    return {"url": plan["url"], "sha256": plan["sha256"], "review_sha256": review_sha256}


def development_members(archive, platform):
    """Read coverage from the actual trusted CI archive, never a detached JSON."""
    prefix = "linux/" if platform == "linux" else ""
    names = ["DEVELOPMENT-SOURCE.json", f"BUILDINFO-{platform}.json",
             f"THIRD-PARTY-NOTICES-{platform}.txt", "bin/aperture" if platform == "linux" else "aperture.exe"]
    result = {}
    with (tarfile.open(archive, "r:gz") if platform == "linux" else zipfile.ZipFile(archive)) as source:
        entries = source.getmembers() if platform == "linux" else source.infolist()
        for entry in entries:
            name = entry.name if platform == "linux" else entry.filename
            if name.startswith("/") or "\\" in name or ":" in name or ".." in PurePosixPath(name).parts or str(PurePosixPath(name)) != name.rstrip("/"):
                raise ValueError("Unsafe development archive member")
        for name in names:
            selected = [entry for entry in entries if (entry.name if platform == "linux" else entry.filename) == prefix + name]
            if len(selected) != 1:
                raise ValueError("Development archive requires exactly one " + name)
            member = selected[0]
            if platform == "linux" and not member.isfile():
                raise ValueError("Development coverage members must be regular files")
            if platform == "windows" and stat.S_ISLNK(member.external_attr >> 16):
                raise ValueError("Development coverage members must not be links")
            size = member.size if platform == "linux" else member.file_size
            limit = 1024 ** 2 if name.endswith(".json") else 128 * 1024 ** 2 if name.endswith(("aperture", ".exe")) else 64 * 1024 ** 2
            if size <= 0 or size > limit:
                raise ValueError("Development coverage member exceeds its bound")
            with (source.extractfile(member) if platform == "linux" else source.open(member)) as stream:
                body = stream.read()
            result[name] = body
    return result


def development_coverage(archive, platform, report, info_path, source_sha, plan_path, plan_sha256, root):
    hex_value(source_sha, 40)
    passing(report)
    plan = source_plan(plan_path, plan_sha256)
    members = development_members(archive, platform)
    record_body = members["DEVELOPMENT-SOURCE.json"]
    record = json.loads(record_body)
    info = read(info_path)
    locks = lockfile_hashes(root)
    native = native_identity(info.get("native_dependencies"))
    binary = "bin/aperture" if platform == "linux" else "aperture.exe"
    hashed = lambda body: hashlib.sha256(body).hexdigest()
    if type(report.get("schema")) is not int or report["schema"] != 1 or (report.get("stage"), report.get("status"), report.get("forced_kill"), report.get("platform"), report.get("source_sha")) != ("A", "pass", False, platform, source_sha):
        raise ValueError("Exact-source passing CI development report required")
    if type(record.get("schema")) is not int or (record.get("schema"), record.get("stage"), record.get("status"), record.get("platform"), record.get("source_sha")) != (1, "A", "ready", platform, source_sha):
        raise ValueError("Development source coverage differs from the exact CI source/platform")
    if (record.get("build_inputs_sha256") != report.get("build_inputs_sha256")
            or not re.fullmatch(r"[0-9a-f]{64}", record.get("build_inputs_sha256", ""))
            or record.get("application_source_url") != f"https://github.com/{REPOSITORY}/tree/{source_sha}"
            or hashed(members[binary]) != report.get("binary_sha256")):
        raise ValueError("Development archive differs from trusted automated inputs/binary")
    expected_baseline = {**baseline_identity(plan, plan_sha256), "application_lockfiles_sha256": plan["application_lockfiles_sha256"]}
    if record.get("baseline") != expected_baseline:
        raise ValueError("Development dependency coverage differs from reviewed baseline")
    if (info.get("source_sha") != source_sha or info.get("platform") != platform
            or info.get("version") != current_version(root) or info.get("lockfiles") != locks
            or record.get("application_lockfiles_sha256") != locks
            or record.get("native_dependencies_sha256") != native
            or native != plan["native_dependencies_sha256"][platform]
            or locks["pnpm-lock.yaml"] != plan["application_lockfiles_sha256"]["pnpm-lock.yaml"]):
        raise ValueError("Development application locks/version/native coverage differs")
    for key, name in (("buildinfo", f"BUILDINFO-{platform}.json"), ("notices", f"THIRD-PARTY-NOTICES-{platform}.txt")):
        if record.get(key) != {"file": name, "sha256": hashed(members[name])}:
            raise ValueError("Development coverage metadata bytes differ: " + key)
    if hashed(members[f"BUILDINFO-{platform}.json"]) != digest(info_path):
        raise ValueError("Prepared BUILDINFO differs from trusted development archive")
    delta = record.get("cargo_root_version", {})
    changed = locks["src-tauri/Cargo.lock"] != plan["application_lockfiles_sha256"]["src-tauri/Cargo.lock"]
    if (delta.get("current") != info["version"] or not re.fullmatch(r"\d+\.\d+\.\d+", delta.get("baseline", ""))
            or delta.get("change") != ("aperture-root-version-only" if changed else "unchanged")
            or (delta["baseline"] != delta["current"]) != changed):
        raise ValueError("Development Cargo coverage lacks its CI-proven root-version delta")
    return {"baseline": expected_baseline, "native_dependencies_sha256": plan["native_dependencies_sha256"],
            "application_source_sha": source_sha, "application_lockfiles_sha256": locks,
            "development_source_sha256": hashed(record_body), "cargo_root_version": delta}


def common_source_coverage(manifest):
    if manifest["schema"] == 1:
        return {"schema": 1, "source_material": manifest["source_material"]}
    coverage = manifest["dependency_coverage"]
    return {"schema": 2, **{key: coverage[key] for key in ("baseline", "native_dependencies_sha256", "application_source_sha", "application_lockfiles_sha256", "cargo_root_version")}}


def validate_coverage(manifest):
    coverage = manifest.get("dependency_coverage")
    required = {"baseline", "native_dependencies_sha256", "application_source_sha", "application_lockfiles_sha256", "development_source_sha256", "cargo_root_version"}
    if (not isinstance(coverage, dict) or set(coverage) != required
            or coverage["application_source_sha"] != manifest["source_sha"]):
        raise ValueError("Prepared dependency source coverage is missing or inconsistent")
    baseline = coverage["baseline"]
    if (not isinstance(baseline, dict) or set(baseline) != {"url", "sha256", "review_sha256", "application_lockfiles_sha256"}
            or baseline["review_sha256"] != manifest["source_plan_sha256"]
            or not re.fullmatch(r"https://github.com/neuroqp/neuroqp-aperture/releases/download/[A-Za-z0-9._-]+/Aperture-Corresponding-Source\.tar\.xz", baseline.get("url", ""))):
        raise ValueError("Prepared reviewed dependency baseline differs")
    for name in ("sha256", "review_sha256"):
        hex_value(baseline[name], 64)
    for locks in (baseline["application_lockfiles_sha256"], coverage["application_lockfiles_sha256"]):
        if not isinstance(locks, dict) or set(locks) != {"pnpm-lock.yaml", "src-tauri/Cargo.lock"}:
            raise ValueError("Prepared dependency coverage requires both application locks")
        for value in locks.values():
            hex_value(value, 64)
    native = coverage["native_dependencies_sha256"]
    if not isinstance(native, dict) or set(native) != set(PATHS) or native[manifest["platform"]] != manifest["native_dependencies_sha256"]:
        raise ValueError("Prepared native dependency coverage differs")
    for value in native.values():
        hex_value(value, 64)
    hex_value(coverage["development_source_sha256"], 64)
    delta = coverage["cargo_root_version"]
    changed = baseline["application_lockfiles_sha256"]["src-tauri/Cargo.lock"] != coverage["application_lockfiles_sha256"]["src-tauri/Cargo.lock"]
    if (not isinstance(delta, dict) or set(delta) != {"baseline", "current", "change"}
            or any(not re.fullmatch(r"\d+\.\d+\.\d+", delta.get(key, "")) for key in ("baseline", "current"))
            or delta["change"] != ("aperture-root-version-only" if changed else "unchanged")
            or (delta["baseline"] != delta["current"]) != changed
            or baseline["application_lockfiles_sha256"]["pnpm-lock.yaml"] != coverage["application_lockfiles_sha256"]["pnpm-lock.yaml"]):
        raise ValueError("Prepared Cargo/npm coverage lacks its proven root-version delta")
    return coverage


def check_plan(manifest, path, sha256):
    if type(manifest.get("schema")) is not int or manifest["schema"] not in (1, 2):
        raise ValueError("Unsupported prepared source coverage schema")
    plan = source_plan(path, sha256)
    if (plan.get("application_source_sha", manifest["source_sha"]) != manifest["source_sha"]
            or plan["application_lockfiles_sha256"] != lockfile_hashes(ROOT)
            or plan["native_dependencies_sha256"][manifest["platform"]] != manifest["native_dependencies_sha256"]):
        raise ValueError("Prepared source, lockfiles or native dependencies differ from reviewed source")
    if manifest.get("schema") == 2:
        coverage = validate_coverage(manifest)
        if (plan.get("application_source_sha") != manifest["source_sha"]
                or plan["application_lockfiles_sha256"] != coverage["application_lockfiles_sha256"]
                or plan["native_dependencies_sha256"] != coverage["native_dependencies_sha256"]
                or plan.get("dependency_source") != {key: coverage["baseline"][key] for key in ("url", "sha256", "review_sha256")}):
            raise ValueError("Final frozen application or dependency source coverage differs")
    elif manifest.get("source_material") != source_material(plan, manifest["source_sha"]):
        raise ValueError("Reviewed source material differs from prepared archive coverage")


def create(args):
    report, info = read(args.automated_report), read(args.buildinfo)
    same_identity(report, identity(ROOT))
    passing(report)
    if report.get("stage") != "A" or report.get("platform") != args.platform or report.get("source_sha") != args.source_sha:
        raise ValueError("Exact-source Stage A automated evidence required")
    if info.get("source_sha") != args.source_sha or info.get("platform") != args.platform or info.get("lockfiles") != lockfile_hashes(ROOT):
        raise ValueError("Prepared BUILDINFO differs from validated source")
    if digest(args.executable) != report.get("binary_sha256"):
        raise ValueError("Prepared executable differs from automated Stage A binary")
    manifest = {"schema": 2, "stage": "A-packaging", "status": "pass", "forced_kill": False,
                "platform": args.platform, "source_sha": hex_value(args.source_sha, 40),
                "validation_run_id": args.validation_run_id, "packaging_run_id": args.packaging_run_id,
                "binary_sha256": report["binary_sha256"],
                "native_dependencies_sha256": native_identity(info["native_dependencies"]),
                "development_archive_sha256": digest(args.development_archive),
                "tool_archive_sha256": digest(args.tool_archive), "source_plan_sha256": args.source_plan_sha256,
                "node_sha256": digest(node_path(args.platform, ROOT))}
    manifest["dependency_coverage"] = development_coverage(args.development_archive, args.platform, report, args.buildinfo,
        args.source_sha, args.source_plan, args.source_plan_sha256, ROOT)
    validate_coverage(manifest)
    if args.platform == "linux":
        manifest["verifier_archive_sha256"] = digest(args.verifier_archive)
    args.archive.parent.mkdir(parents=True, exist_ok=True)
    # Dereference pnpm's junctions/symlinks so the handoff has no external store dependency.
    with tarfile.open(args.archive, "w:gz", dereference=True) as archive:
        for name in PATHS[args.platform]:
            path = ROOT / name
            if not path.exists():
                raise ValueError(f"Prepared packaging input missing: {name}")
            archive.add(path, arcname=name)
    manifest["archive_sha256"] = digest(args.archive)
    write_report(args.manifest, manifest)


def validate(manifest_path, frozen, platform):
    manifest = read(manifest_path)
    expected = frozen.get("validated_inputs", {}).get(platform, {})
    if digest(manifest_path) != expected.get("manifest_sha256"):
        raise ValueError("Prepared manifest differs from frozen reviewed bytes")
    if type(manifest.get("schema")) is not int or manifest["schema"] not in (1, 2) or (manifest.get("stage"), manifest.get("status"), manifest.get("forced_kill"), manifest.get("platform")) != ("A-packaging", "pass", False, platform):
        raise ValueError("Successful packaging preparation manifest required")
    if manifest.get("source_sha") != frozen.get("source_sha") or manifest.get("packaging_run_id") != frozen.get("packaging_run_id"):
        raise ValueError("Prepared source or packaging workflow differs from frozen inputs")
    automated = [row for row in frozen["readiness"]["automated"] if row.get("platform") == platform]
    if len(automated) != 1 or automated[0].get("source_sha") != manifest["source_sha"] or automated[0].get("binary_sha256") != manifest.get("binary_sha256"):
        raise ValueError("Prepared executable differs from frozen automated evidence")
    for key in FIELDS:
        hex_value(manifest.get(key), 64)
        if manifest[key] != expected.get(key):
            raise ValueError(f"Prepared {key} differs from frozen inputs")
    for key in ("archive_sha256", "tool_archive_sha256", "development_archive_sha256", "source_plan_sha256", "node_sha256"):
        hex_value(manifest.get(key), 64)
    if platform == "linux":
        hex_value(manifest.get("verifier_archive_sha256"), 64)
    if manifest["schema"] == 2:
        validate_coverage(manifest)
        return manifest
    material = manifest.get("source_material")
    if (not isinstance(material, dict) or set(material) != set(SOURCE_MATERIAL_FIELDS)
            or material["application_source_sha"] != manifest["source_sha"]
            or material.get("native_dependencies_sha256", {}).get(platform) != manifest["native_dependencies_sha256"]):
        raise ValueError("Prepared source material identity is missing or inconsistent")
    return manifest


def prepare_verifier(directory, archive_path):
    binary = Path(shutil.which("minisign") or "/usr/bin/minisign").resolve(strict=True)
    directory.mkdir(parents=True, exist_ok=False)
    (directory / "bin").mkdir()
    (directory / "lib").mkdir()
    shutil.copy2(binary, directory / "bin/minisign")
    dependencies = subprocess.check_output(["ldd", str(binary)], text=True, timeout=30)
    if "not found" in dependencies:
        raise ValueError("Minisign shared library closure is incomplete")
    libraries = {Path(match) for match in re.findall(r"/[^\s()]+", dependencies)}
    if not any(path.name.startswith("libsodium.so") for path in libraries):
        raise ValueError("Minisign libsodium closure missing")
    if not any(path.name == "ld-linux-x86-64.so.2" for path in libraries):
        raise ValueError("Minisign ELF loader missing")
    for library in libraries:
        shutil.copy2(library, directory / "lib" / library.name, follow_symlinks=True)
    wrapper = directory / "minisign"
    wrapper.write_text('#!/bin/sh\nset -eu\nhere=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\nexec "$here/lib/ld-linux-x86-64.so.2" --library-path "$here/lib" "$here/bin/minisign" "$@"\n')
    wrapper.chmod(0o755)
    subprocess.run([str(wrapper.resolve()), "-v"], check=True, timeout=30)
    with tarfile.open(archive_path, "w:gz", dereference=True) as archive:
        archive.add(directory, arcname="release-verifier")


def restore_verifier(frozen, manifest_path, archive_path, destination):
    preparation_run(frozen.get("packaging_run_id"))
    manifest = validate(manifest_path, frozen, "linux")
    if digest(archive_path) != manifest["verifier_archive_sha256"]:
        raise ValueError("Prepared signature verifier archive differs")
    extract(archive_path, destination, ("release-verifier",))
    executable = destination / "release-verifier/minisign"
    subprocess.run([str(executable.resolve()), "-v"], check=True, timeout=30)
    return executable.parent.resolve()


def extract(archive_path, destination, allowed):
    destination = Path(destination)
    with tarfile.open(archive_path, "r:gz") as archive:
        members = archive.getmembers()
        names = set()
        for member in members:
            path = PurePosixPath(member.name)
            if (str(path) != member.name or "\\" in member.name or ":" in member.name
                    or path.is_absolute() or ".." in path.parts or member.name in names
                    or not (member.isfile() or member.isdir()) or member.mode & 0o7000
                    or not any(member.name == prefix or member.name.startswith(prefix + "/") for prefix in allowed)):
                raise ValueError("Unsafe prepared archive entry")
            names.add(member.name)
            target = destination / member.name
            if any(parent.is_symlink() for parent in (target, *target.parents)) or target.exists() and member.isfile():
                raise ValueError("Prepared input destination is not empty")
        for member in members:
            target = destination / member.name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with target.open("xb") as stream:
                    shutil.copyfileobj(archive.extractfile(member), stream)
                target.chmod(member.mode)


def preparation_run(run_id):
    result = trusted_run(run_id, "native-validation.yml")
    sha = hex_value(result.get("head_sha"), 40)
    subprocess.run(["git", "merge-base", "--is-ancestor", sha, "origin/main"], check=True, timeout=30)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("create")
    for name in ("platform", "source-sha", "validation-run-id", "packaging-run-id", "source-plan-sha256"):
        p.add_argument("--" + name, required=True)
    for name in ("archive", "manifest", "automated-report", "buildinfo", "executable", "development-archive", "tool-archive", "source-plan"):
        p.add_argument("--" + name, required=True, type=Path)
    p.add_argument("--verifier-archive", type=Path)
    p = sub.add_parser("prepare-node")
    p.add_argument("platform", choices=PATHS)
    p = sub.add_parser("prepare-verifier")
    p.add_argument("directory", type=Path)
    p.add_argument("archive", type=Path)
    p = sub.add_parser("restore-verifier")
    p.add_argument("frozen", type=Path)
    p.add_argument("manifest", type=Path)
    p.add_argument("archive", type=Path)
    p.add_argument("destination", type=Path)
    p = sub.add_parser("bind")
    p.add_argument("frozen", type=Path)
    p.add_argument("directory", type=Path)
    p = sub.add_parser("restore")
    p.add_argument("frozen", type=Path)
    p.add_argument("manifest", type=Path)
    p.add_argument("archive", type=Path)
    p.add_argument("--platform", choices=PATHS, required=True)
    p.add_argument("--source-plan", type=Path, required=True)
    p.add_argument("--source-plan-sha256", required=True)
    p = sub.add_parser("extract-tools")
    p.add_argument("archive", type=Path)
    p.add_argument("destination", type=Path)
    p = sub.add_parser("native-run")
    p.add_argument("run_id")
    p.add_argument("source_sha")
    p = sub.add_parser("coverage", help="Validate dependency coverage inside an exact-source CI development archive")
    p.add_argument("--platform", choices=PATHS, required=True)
    for name in ("development-archive", "automated-report", "buildinfo", "source-plan"):
        p.add_argument("--" + name, type=Path, required=True)
    p.add_argument("--source-sha", required=True)
    p.add_argument("--source-plan-sha256", required=True)
    p.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    if args.command == "coverage":
        print(json.dumps(development_coverage(args.development_archive, args.platform, read(args.automated_report), args.buildinfo,
            args.source_sha, args.source_plan, args.source_plan_sha256, args.root), sort_keys=True))
    elif args.command == "create":
        create(args)
    elif args.command == "prepare-node":
        prepare_node(args.platform)
    elif args.command == "prepare-verifier":
        prepare_verifier(args.directory, args.archive)
    elif args.command == "restore-verifier":
        directory = restore_verifier(read(args.frozen), args.manifest, args.archive, args.destination)
        if os.environ.get("GITHUB_PATH"):
            with Path(os.environ["GITHUB_PATH"]).open("a") as stream:
                stream.write(str(directory) + "\n")
        print(directory)
    elif args.command == "native-run":
        trusted_run(args.run_id, "native-validation.yml", args.source_sha)
    elif args.command == "bind":
        frozen = read(args.frozen)
        preparation_run(frozen.get("packaging_run_id"))
        manifests = [validate(args.directory / f"prepared-{platform}.json", frozen, platform) for platform in PATHS]
        if manifests[0]["validation_run_id"] != manifests[1]["validation_run_id"]:
            raise ValueError("Prepared platforms reference different automated validation runs")
        trusted_run(manifests[0]["validation_run_id"], "native-validation.yml", frozen["source_sha"])
        print(f"packaging_run_id={frozen['packaging_run_id']}")
    elif args.command == "restore":
        manifest = validate(args.manifest, read(args.frozen), args.platform)
        check_plan(manifest, args.source_plan, args.source_plan_sha256)
        if digest(args.archive) != manifest["archive_sha256"]:
            raise ValueError("Prepared archive SHA256 differs")
        extract(args.archive, ROOT, PATHS[args.platform])
        executable = ROOT / ("src-tauri/target/x86_64-pc-windows-msvc/release/aperture.exe" if args.platform == "windows" else "build/prepared/linux-runtime.tar.gz")
        key = "binary_sha256" if args.platform == "windows" else "tool_archive_sha256"
        if digest(executable) != manifest[key]:
            raise ValueError("Restored prepared input differs")
        node = node_path(args.platform, ROOT)
        check_node(node, manifest["node_sha256"])
        if os.environ.get("GITHUB_PATH"):
            with Path(os.environ["GITHUB_PATH"]).open("a") as stream:
                stream.write(str(node.parent.resolve()) + "\n")
        tools = ROOT / "build/windows-packaging-tools.tar.gz"
        if args.platform == "windows" and digest(tools) != manifest["tool_archive_sha256"]:
            raise ValueError("Restored packaging tools archive differs")
    elif args.command == "extract-tools":
        extract(args.archive, args.destination, ("build/windows-packaging-tools",))


if __name__ == "__main__":
    main()
