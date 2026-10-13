#!/usr/bin/env python3
"""Version, candidate metadata and same-byte promotion checks. No dependencies."""

import argparse
import base64
import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ASSETS = {
    "windows-x86_64": "Aperture-Setup-x64.exe",
    "windows-x86_64-portable": "Aperture-Portable-x64.exe",
    "linux-x86_64": "Aperture-x86_64.AppImage",
}
VERSION = re.compile(r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\Z")
SOURCE_ASSET = "Aperture-Corresponding-Source.tar.xz"
SOURCE_REVIEW = "Aperture-Corresponding-Source.json"
LOCKFILES = ("pnpm-lock.yaml", "src-tauri/Cargo.lock")
SOURCE_INVENTORY = "application/source-inventory.json"


def digest(path):
    with path.open("rb") as source:
        result = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def native_identity(dependencies):
    return hashlib.sha256(json.dumps(dependencies, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")).hexdigest()


def lockfile_hashes(root):
    return {name: digest(root / name) for name in LOCKFILES}


def verify_application_locks(root, plan):
    if lockfile_hashes(root) != plan["application_lockfiles_sha256"]:
        raise ValueError("Corresponding source does not match the checkout application lockfiles; review refreshed material")


def source_plan(path):
    plan = json.loads(path.read_text(encoding="utf-8"))
    if plan.get("schema") != 1 or plan.get("status") != "ready":
        raise ValueError("Corresponding-source publication blocked: assemble and review the actual source/build material first")
    hashes = plan.get("native_dependencies_sha256", {})
    if plan.get("asset") != SOURCE_ASSET or not re.fullmatch(r"[0-9a-f]{64}", plan.get("sha256") or "") or set(hashes) != {"linux", "windows"} or any(not re.fullmatch(r"[0-9a-f]{64}", value or "") for value in hashes.values()):
        raise ValueError("Corresponding-source review must pin archive bytes and both native dependency identities")
    locks = plan.get("application_lockfiles_sha256", {})
    if set(locks) != set(LOCKFILES) or any(not re.fullmatch(r"[0-9a-f]{64}", value or "") for value in locks.values()) or not re.fullmatch(r"[0-9a-f]{64}", plan.get("application_source_inventory_sha256") or ""):
        raise ValueError("Corresponding-source review must pin both application lockfiles and their archived source inventory")
    if not re.fullmatch(r"https://github.com/neuroqp/neuroqp-aperture/releases/download/[A-Za-z0-9._-]+/Aperture-Corresponding-Source\.tar\.xz", plan.get("url") or ""):
        raise ValueError("Source archive must have a versioned source-repository release URL; its downloaded bytes are verified")
    return plan


def verify_source_material(directory, plan, builds=False, platform=None, fixture_locks=None):
    archive = directory / SOURCE_ASSET
    if not archive.is_file() or digest(archive) != plan["sha256"]:
        raise ValueError("Corresponding-source archive missing or different from the reviewed bytes")
    # Read bounded identity records in place, never extract or execute content.
    inventory_bytes = None
    with tarfile.open(archive, "r|xz") as source:
        first = source.next()
        if not first or not first.isfile() or first.name != "SOURCE-MANIFEST.json" or first.size > 65536:
            raise ValueError("Source archive must start with a bounded SOURCE-MANIFEST.json identity record")
        record = json.load(source.extractfile(first))
        for member in source:
            if member.name == SOURCE_INVENTORY:
                if not member.isfile() or member.size > 65536:
                    raise ValueError("Source archive must contain a bounded application source inventory")
                inventory_bytes = source.extractfile(member).read()
                break
    if record.get("schema") != 1 or record.get("native_dependencies_sha256") != plan["native_dependencies_sha256"]:
        raise ValueError("Source archive identity differs from its reviewed dependency identities")
    if record.get("inventory_sha256", {}).get(SOURCE_INVENTORY) != plan["application_source_inventory_sha256"]:
        raise ValueError("Source archive application inventory differs from its reviewed identity")
    if inventory_bytes is None or hashlib.sha256(inventory_bytes).hexdigest() != plan["application_source_inventory_sha256"]:
        raise ValueError("Source archive application inventory bytes differ from their reviewed hash")
    inventory = json.loads(inventory_bytes)
    lock_entries = [entry for entry in inventory.get("files", []) if entry.get("path") in LOCKFILES]
    if inventory.get("schema") != 1 or len(lock_entries) != len(LOCKFILES) or {entry["path"]: entry.get("sha256") for entry in lock_entries} != plan["application_lockfiles_sha256"]:
        raise ValueError("Source archive application lockfiles differ from the reviewed plan")
    platforms = ("linux", "windows") if builds else ((platform,) if platform else ())
    for platform in platforms:
        info = json.loads((directory / f"BUILDINFO-{platform}.json").read_text(encoding="utf-8"))
        if native_identity(info["native_dependencies"]) != plan["native_dependencies_sha256"][platform]:
            raise ValueError(f"Corresponding source does not match the shipped {platform} native dependency identity")
        expected_locks = fixture_locks if fixture_locks is not None else plan["application_lockfiles_sha256"]
        if info.get("lockfiles") != expected_locks:
            raise ValueError(f"Corresponding source does not match the shipped {platform} application lockfiles")


def upgrade_fixture_locks(directory, root, plan):
    # Only this separate, unsigned acceptance fixture may change the root version.
    info = json.loads((directory / "BUILDINFO-windows.json").read_text(encoding="utf-8"))
    if info["version"] != "0.0.9" or current_version(root) != "0.1.0":
        raise ValueError("Upgrade fixture must be the separate 0.1.0 to 0.0.9 Windows acceptance build")
    production = (root / "src-tauri/Cargo.lock").read_bytes()
    replacement, count = re.subn(rb'(?m)^name = "aperture"\nversion = "[^"\n]+"$', b'name = "aperture"\nversion = "0.0.9"', production)
    delta = directory / "test-deltas/src-tauri/Cargo.lock"
    if count != 1 or not delta.is_file() or delta.read_bytes() != replacement:
        raise ValueError("Upgrade fixture Cargo.lock must differ only in the aperture root version")
    return {**plan["application_lockfiles_sha256"], "src-tauri/Cargo.lock": digest(delta)}


def source_material(directory, fetch=False, root=ROOT, platform=None, upgrade_fixture=False):
    review = root / "scripts/release/corresponding-source.json"
    plan = source_plan(review)
    verify_application_locks(root, plan)
    fixture_locks = None
    if upgrade_fixture:
        if platform != "windows":
            raise ValueError("Upgrade fixture source verification requires --platform windows")
        fixture_locks = upgrade_fixture_locks(directory, root, plan)
    directory.mkdir(parents=True, exist_ok=True)
    archive = directory / SOURCE_ASSET
    if fetch:
        temporary = archive.with_suffix(".partial")
        try:
            with urllib.request.urlopen(plan["url"], timeout=60) as source, temporary.open("wb") as output:
                if not source.geturl().startswith("https://"):
                    raise ValueError("Source material download redirected away from HTTPS")
                shutil.copyfileobj(source, output, length=1024 * 1024)
            if digest(temporary) != plan["sha256"]:
                raise ValueError("Downloaded corresponding source differs from its reviewed checksum")
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)
    verify_source_material(directory, plan, platform=platform, fixture_locks=fixture_locks)
    shutil.copyfile(review, directory / SOURCE_REVIEW)


def current_version(root=ROOT):
    package = json.loads((root / "package.json").read_text(encoding="utf-8"))["version"]
    tauri = json.loads((root / "src-tauri/tauri.conf.json").read_text(encoding="utf-8"))["version"]
    cargo = re.search(r'(?ms)^\[package\].*?^version = "([^"]+)"', (root / "src-tauri/Cargo.toml").read_text(encoding="utf-8"))[1]
    lock = re.search(r'(?m)^name = "aperture"\nversion = "([^"]+)"', (root / "src-tauri/Cargo.lock").read_text(encoding="utf-8"))[1]
    if len({package, tauri, cargo, lock}) != 1 or not VERSION.fullmatch(package):
        raise ValueError(f"Version mismatch or invalid stable version: {package}, {tauri}, {cargo}, {lock}")
    return package


def notes(version, root=ROOT):
    text = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    match = re.search(rf"(?ms)^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## |\Z)", text)
    if not match or not match[1].strip():
        raise ValueError(f"CHANGELOG.md has no notes for {version}")
    return match[1].strip() + "\n"


def set_version(version, root=ROOT):
    if not VERSION.fullmatch(version):
        raise ValueError("Use a stable version such as 0.2.0; candidates use GitHub prerelease state")
    old = current_version(root)
    # Validate before writing any file. The lock's root package has no checksum.
    for name in ("package.json", "src-tauri/tauri.conf.json"):
        path = root / name
        data = json.loads(path.read_text(encoding="utf-8"))
        data["version"] = version
        path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    path = root / "src-tauri/Cargo.toml"
    path.write_text(path.read_text(encoding="utf-8").replace(f'version = "{old}"', f'version = "{version}"', 1), encoding="utf-8")
    path = root / "src-tauri/Cargo.lock"
    # Keep lock bytes and line endings intact; Windows text writes translate LF.
    pattern = rb'(?m)(^name = "aperture"\r?\nversion = ")' + re.escape(old.encode()) + rb'(")'
    path.write_bytes(re.sub(pattern, lambda match: match[1] + version.encode() + match[2], path.read_bytes(), count=1))
    # pnpm-lock.yaml has no root version field; its importer resolves dependencies only.
    current_version(root)


def manifest(directory, version, repository, source_sha, published_at, signed=True, root=ROOT):
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise ValueError("Source SHA must be a full Git commit ID")
    if repository != "neuroqp/neuroqp-aperture":
        raise ValueError("Release assets belong in neuroqp/neuroqp-aperture")
    plan = source_plan(root / "scripts/release/corresponding-source.json")
    verify_application_locks(root, plan)
    verify_source_material(directory, plan, builds=True)
    if json.loads((directory / SOURCE_REVIEW).read_text(encoding="utf-8")) != plan:
        raise ValueError("Source material review differs from the release commit's reviewed plan")
    platforms = {}
    for target, name in ASSETS.items():
        artifact = directory / name
        if not artifact.is_file() or artifact.stat().st_size == 0:
            raise ValueError(f"Missing or empty release asset: {name}")
        if signed:
            signature = (directory / (name + ".sig")).read_text(encoding="utf-8").strip()
            if not signature:
                raise ValueError(f"Empty updater signature: {name}")
            platforms[target] = {"signature": signature, "url": f"https://github.com/{repository}/releases/download/v{version}/{name}"}
    data = {"version": version, "notes": notes(version, root), "pub_date": published_at, "platforms": platforms}
    # Unsigned validation deliberately has no updater feed.
    if signed:
        (directory / "latest.json").write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    for platform in ("windows", "linux"):
        info = json.loads((directory / f"BUILDINFO-{platform}.json").read_text(encoding="utf-8"))
        if info["version"] != version or info["source_sha"] != source_sha:
            raise ValueError(f"Wrong build identity in BUILDINFO-{platform}.json")
        if not (directory / f"THIRD-PARTY-NOTICES-{platform}.txt").stat().st_size:
            raise ValueError(f"Missing third-party notices for {platform}")
    checksums = "".join(f"{digest(path)}  {path.name}\n" for path in sorted(directory.iterdir()) if path.is_file() and path.name != "SHA256SUMS")
    (directory / "SHA256SUMS").write_text(checksums, encoding="utf-8")
    verify(directory, version, source_sha, signed)


def verify(directory, version, source_sha, signed=True):
    expected = set(ASSETS.values()) | {"BUILDINFO-linux.json", "BUILDINFO-windows.json", "THIRD-PARTY-NOTICES-linux.txt", "THIRD-PARTY-NOTICES-windows.txt", SOURCE_ASSET, SOURCE_REVIEW}
    if signed:
        expected |= {name + ".sig" for name in ASSETS.values()} | {"latest.json"}
    rows = {}
    for line in (directory / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match or match[2] in rows or match[2] == "SHA256SUMS":
            raise ValueError("Invalid or duplicate checksum entry")
        rows[match[2]] = match[1]
    if set(rows) != expected or {p.name for p in directory.iterdir() if p.is_file()} != expected | {"SHA256SUMS"}:
        raise ValueError("Release asset set differs from the required files")
    for name, checksum in rows.items():
        if digest(directory / name) != checksum:
            raise ValueError(f"Checksum mismatch: {name}")
    for platform in ("linux", "windows"):
        info = json.loads((directory / f"BUILDINFO-{platform}.json").read_text(encoding="utf-8"))
        if info["version"] != version or info["source_sha"] != source_sha:
            raise ValueError("Build identity mismatch")
    verify_source_material(directory, source_plan(directory / SOURCE_REVIEW), builds=True)
    if signed:
        feed = json.loads((directory / "latest.json").read_text(encoding="utf-8"))
        if feed["version"] != version or set(feed["platforms"]) != set(ASSETS):
            raise ValueError("Updater feed identity mismatch")
        for target, name in ASSETS.items():
            entry = feed["platforms"][target]
            if entry["signature"] != (directory / (name + ".sig")).read_text(encoding="utf-8").strip() or entry["url"] != f"https://github.com/neuroqp/neuroqp-aperture/releases/download/v{version}/{name}":
                raise ValueError(f"Updater feed mismatch: {target}")


def buildinfo(platform, directory, dependency_file, root=ROOT):
    def run(*args):
        if os.name == "nt" and args[0] == "pnpm":
            # pnpm/action-setup installs a .cmd shim, not a Windows executable.
            shim = shutil.which("pnpm.cmd")
            if not shim:
                raise ValueError("pnpm.cmd is not on PATH")
            args = (os.environ["COMSPEC"], "/d", "/c", shim, *args[1:])
        return subprocess.check_output(args, cwd=root, text=True, encoding="utf-8").strip()
    info = {"version": current_version(root), "source_sha": run("git", "-c", f"safe.directory={root}", "rev-parse", "HEAD"), "platform": platform,
            "toolchains": {"rust": run("rustc", "--version"), "cargo": run("cargo", "--version"), "node": run("node", "--version"), "pnpm": run("pnpm", "--version")},
            "lockfiles": lockfile_hashes(root),
            "native_dependencies": json.loads(dependency_file.read_text(encoding="utf-8")), "desktop_acceptance": "Required before promotion; automated worker/package checks are not GUI acceptance."}
    info["runner"] = {name: os.environ[name] for name in ("ImageOS", "ImageVersion", "RUNNER_OS", "RUNNER_ARCH") if name in os.environ}
    if platform == "windows":
        info["toolchains"].update({name: os.environ[name] for name in ("VCToolsVersion", "WindowsSDKVersion", "VSINSTALLDIR") if name in os.environ})
    else:
        info["toolchains"]["cc"] = run("cc", "--version").splitlines()[0]
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps(info, indent=2) + "\n", encoding="utf-8")


def verify_signatures(directory, root=ROOT):
    public = json.loads((root / "src-tauri/tauri.conf.json").read_text(encoding="utf-8"))["plugins"]["updater"]["pubkey"]
    with tempfile.TemporaryDirectory(prefix="aperture-signatures-") as folder:
        key = Path(folder) / "public.key"
        key.write_bytes(base64.b64decode(public, validate=True))
        for name in ASSETS.values():
            signature = Path(folder) / (name + ".minisig")
            signature.write_bytes(base64.b64decode((directory / (name + ".sig")).read_text(encoding="utf-8").strip(), validate=True))
            subprocess.run(["minisign", "-Vm", str(directory / name), "-p", str(key), "-x", str(signature)], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check")
    sources = commands.add_parser("source-material")
    sources.add_argument("directory", type=Path)
    sources.add_argument("--fetch", action="store_true")
    sources.add_argument("--platform", choices=("linux", "windows"))
    sources.add_argument("--upgrade-fixture", action="store_true", help="Verify the separate unsigned 0.0.9 Windows fixture root-version delta")
    version = commands.add_parser("version")
    version.add_argument("version")
    release_notes = commands.add_parser("notes")
    release_notes.add_argument("version", nargs="?")
    for command in ("metadata", "verify"):
        child = commands.add_parser(command)
        child.add_argument("directory", type=Path)
        child.add_argument("--version", required=True)
        child.add_argument("--source-sha", required=True)
        child.add_argument("--unsigned", action="store_true")
        if command == "verify":
            child.add_argument("--cryptographic", action="store_true")
        if command == "metadata":
            child.add_argument("--repository", default="neuroqp/neuroqp-aperture")
            child.add_argument("--published-at", default=datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z"))
    info = commands.add_parser("buildinfo")
    info.add_argument("platform", choices=("linux", "windows"))
    info.add_argument("directory", type=Path)
    info.add_argument("dependency_file", type=Path)
    args = parser.parse_args()
    if args.command == "check":
        print(current_version())
        notes(current_version())
    elif args.command == "source-material":
        source_material(args.directory, args.fetch, platform=args.platform, upgrade_fixture=args.upgrade_fixture)
    elif args.command == "version":
        set_version(args.version)
    elif args.command == "notes":
        print(notes(args.version or current_version()), end="")
    elif args.command == "metadata":
        manifest(args.directory, args.version, args.repository, args.source_sha, args.published_at, not args.unsigned)
    elif args.command == "verify":
        verify(args.directory, args.version, args.source_sha, not args.unsigned)
        if args.cryptographic:
            if args.unsigned:
                raise ValueError("Unsigned validation has no signatures to verify")
            verify_signatures(args.directory)
    elif args.command == "buildinfo":
        buildinfo(args.platform, args.directory, args.dependency_file)


if __name__ == "__main__":
    main()
