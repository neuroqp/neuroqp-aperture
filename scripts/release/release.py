#!/usr/bin/env python3
"""Version, candidate metadata and same-byte promotion checks. No dependencies."""

import argparse
import base64
import datetime
import hashlib
import json
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ASSETS = {
    "windows-x86_64": "Aperture-Setup-x64.exe",
    "windows-x86_64-portable": "Aperture-Portable-x64.exe",
    "linux-x86_64": "Aperture-x86_64.AppImage",
}
VERSION = re.compile(r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\Z")


def digest(path):
    with path.open("rb") as source:
        result = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def current_version(root=ROOT):
    package = json.loads((root / "package.json").read_text())["version"]
    tauri = json.loads((root / "src-tauri/tauri.conf.json").read_text())["version"]
    cargo = re.search(r'(?ms)^\[package\].*?^version = "([^"]+)"', (root / "src-tauri/Cargo.toml").read_text())[1]
    lock = re.search(r'(?m)^name = "aperture"\nversion = "([^"]+)"', (root / "src-tauri/Cargo.lock").read_text())[1]
    if len({package, tauri, cargo, lock}) != 1 or not VERSION.fullmatch(package):
        raise ValueError(f"Version mismatch or invalid stable version: {package}, {tauri}, {cargo}, {lock}")
    return package


def notes(version, root=ROOT):
    text = (root / "CHANGELOG.md").read_text()
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
        data = json.loads(path.read_text())
        data["version"] = version
        path.write_text(json.dumps(data, indent=2) + "\n")
    path = root / "src-tauri/Cargo.toml"
    path.write_text(path.read_text().replace(f'version = "{old}"', f'version = "{version}"', 1))
    path = root / "src-tauri/Cargo.lock"
    path.write_text(path.read_text().replace(f'name = "aperture"\nversion = "{old}"', f'name = "aperture"\nversion = "{version}"', 1))
    # pnpm-lock.yaml has no root version field; its importer resolves dependencies only.
    current_version(root)


def manifest(directory, version, repository, source_sha, published_at, signed=True, root=ROOT):
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise ValueError("Source SHA must be a full Git commit ID")
    if repository != "neuroqp/neuroqp-aperture":
        raise ValueError("Release assets belong in neuroqp/neuroqp-aperture")
    platforms = {}
    for target, name in ASSETS.items():
        artifact = directory / name
        if not artifact.is_file() or artifact.stat().st_size == 0:
            raise ValueError(f"Missing or empty release asset: {name}")
        if signed:
            signature = (directory / (name + ".sig")).read_text().strip()
            if not signature:
                raise ValueError(f"Empty updater signature: {name}")
            platforms[target] = {"signature": signature, "url": f"https://github.com/{repository}/releases/download/v{version}/{name}"}
    data = {"version": version, "notes": notes(version, root), "pub_date": published_at, "platforms": platforms}
    # Unsigned validation deliberately has no updater feed.
    if signed:
        (directory / "latest.json").write_text(json.dumps(data, indent=2) + "\n")
    for platform in ("windows", "linux"):
        info = json.loads((directory / f"BUILDINFO-{platform}.json").read_text())
        if info["version"] != version or info["source_sha"] != source_sha:
            raise ValueError(f"Wrong build identity in BUILDINFO-{platform}.json")
        if not (directory / f"THIRD-PARTY-NOTICES-{platform}.txt").stat().st_size:
            raise ValueError(f"Missing third-party notices for {platform}")
    checksums = "".join(f"{digest(path)}  {path.name}\n" for path in sorted(directory.iterdir()) if path.is_file() and path.name != "SHA256SUMS")
    (directory / "SHA256SUMS").write_text(checksums)
    verify(directory, version, source_sha, signed)


def verify(directory, version, source_sha, signed=True):
    expected = set(ASSETS.values()) | {"BUILDINFO-linux.json", "BUILDINFO-windows.json", "THIRD-PARTY-NOTICES-linux.txt", "THIRD-PARTY-NOTICES-windows.txt"}
    if signed:
        expected |= {name + ".sig" for name in ASSETS.values()} | {"latest.json"}
    rows = {}
    for line in (directory / "SHA256SUMS").read_text().splitlines():
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
        info = json.loads((directory / f"BUILDINFO-{platform}.json").read_text())
        if info["version"] != version or info["source_sha"] != source_sha:
            raise ValueError("Build identity mismatch")
    if signed:
        feed = json.loads((directory / "latest.json").read_text())
        if feed["version"] != version or set(feed["platforms"]) != set(ASSETS):
            raise ValueError("Updater feed identity mismatch")
        for target, name in ASSETS.items():
            entry = feed["platforms"][target]
            if entry["signature"] != (directory / (name + ".sig")).read_text().strip() or entry["url"] != f"https://github.com/neuroqp/neuroqp-aperture/releases/download/v{version}/{name}":
                raise ValueError(f"Updater feed mismatch: {target}")


def buildinfo(platform, directory, dependency_file, root=ROOT):
    def run(*args):
        return subprocess.check_output(args, cwd=root, text=True).strip()
    info = {"version": current_version(root), "source_sha": run("git", "rev-parse", "HEAD"), "platform": platform,
            "toolchains": {"rust": run("rustc", "--version"), "cargo": run("cargo", "--version"), "node": run("node", "--version"), "pnpm": run("pnpm", "--version")},
            "lockfiles": {name: digest(root / name) for name in ("pnpm-lock.yaml", "src-tauri/Cargo.lock")},
            "native_dependencies": json.loads(dependency_file.read_text()), "desktop_acceptance": "Required before promotion; automated worker/package checks are not GUI acceptance."}
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f"BUILDINFO-{platform}.json").write_text(json.dumps(info, indent=2) + "\n")


def verify_signatures(directory, root=ROOT):
    public = json.loads((root / "src-tauri/tauri.conf.json").read_text())["plugins"]["updater"]["pubkey"]
    with tempfile.TemporaryDirectory(prefix="aperture-signatures-") as folder:
        key = Path(folder) / "public.key"
        key.write_bytes(base64.b64decode(public, validate=True))
        for name in ASSETS.values():
            signature = Path(folder) / (name + ".minisig")
            signature.write_bytes(base64.b64decode((directory / (name + ".sig")).read_text().strip(), validate=True))
            subprocess.run(["minisign", "-Vm", str(directory / name), "-p", str(key), "-x", str(signature)], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check")
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
