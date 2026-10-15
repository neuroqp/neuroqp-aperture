#!/usr/bin/env python3
"""Seal the validated Linux AppDir and tools; stage their exact bytes without APT."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import shutil
import stat
import tarfile
import tempfile

from release import digest, native_identity


REMOVED = ("QA-DRIVER-PROVENANCE.json", "QA-DRIVER-STATUS-PREFLIGHT.json",
           "usr/bin/WebKitWebDriver", "usr/share/aperture/DEVELOPMENT-SOURCE.json")


def inventory(root):
    records = {}
    for path in sorted(root.rglob("*")):
        name = path.relative_to(root).as_posix()
        mode = stat.S_IMODE(path.lstat().st_mode)
        if mode & 0o7000:
            raise ValueError(f"Special permissions: {name}")
        if path.is_symlink():
            target = os.readlink(path)
            if Path(target).is_absolute() or not path.resolve(strict=True).is_relative_to(root.resolve()):
                raise ValueError(f"Escaping symlink: {name}")
            records[name] = {"type": "symlink", "mode": mode, "target": target}
        elif path.is_file():
            records[name] = {"type": "file", "mode": mode, "sha256": digest(path)}
        elif path.is_dir():
            records[name] = {"type": "directory", "mode": mode}
        else:
            raise ValueError(f"Unsupported file: {name}")
    return records


def identities(source_sha, binary_sha256, native_sha256):
    for value, length in ((source_sha, 40), (binary_sha256, 64), (native_sha256, 64)):
        if not re.fullmatch(f"[0-9a-f]{{{length}}}", value):
            raise ValueError("Expected lowercase hexadecimal identity")
    return {"source_sha": source_sha, "binary_sha256": binary_sha256,
            "native_dependencies_sha256": native_sha256}


def inventory_sha256(records):
    return digest_bytes(json.dumps(records, sort_keys=True).encode())


def validate_payload(root, expected):
    runtime = root / "runtime"
    for path in [runtime, *runtime.rglob("*")]:
        if path.is_symlink():
            continue
        if path.is_dir() and stat.S_IMODE(path.stat().st_mode) != 0o755:
            raise ValueError(f"Runtime directory is not 0755: {path.relative_to(runtime)}")
        if path.is_file() and not path.stat().st_mode & 0o004:
            raise ValueError(f"Runtime file is not readable by other users: {path.relative_to(runtime)}")
    info = json.loads((runtime / "usr/share/aperture/BUILDINFO.json").read_text())
    if info.get("source_sha") != expected["source_sha"] or info.get("platform") != "linux":
        raise ValueError("Runtime BUILDINFO source/platform differs")
    if native_identity(info.get("native_dependencies")) != expected["native_dependencies_sha256"]:
        raise ValueError("Runtime native dependency identity differs")
    if digest(runtime / "usr/bin/aperture") != expected["binary_sha256"]:
        raise ValueError("Runtime executable differs")
    for name in ("AppRun", "AppRun.wrapped", "usr/bin/aperture", "usr/bin/vips", "usr/bin/vipsheader", "usr/bin/tiffinfo"):
        if not (runtime / name).is_file() or not os.access(runtime / name, os.X_OK):
            raise ValueError(f"Missing executable: {name}")
    for name in ("appimagetool", "mksquashfs", "desktop-file-validate"):
        path = root / "tools/usr/bin" / name
        if not path.is_file() or not os.access(path, os.X_OK):
            raise ValueError(f"Missing packaging tool: {name}")
    with (root / "tools/runtime-x86_64").open("rb") as stream:
        if stream.read(4) != b"\x7fELF":
            raise ValueError("AppImage runtime is not an ELF executable")
    for name in ("LICENSE.txt", "THIRD-PARTY-NOTICES.txt"):
        if not (runtime / "usr/share/aperture" / name).stat().st_size:
            raise ValueError(f"Empty metadata: {name}")
    if any((runtime / name).exists() for name in REMOVED):
        raise ValueError("QA/development-only files remain")


def seal(runtime, tools, development, output, expected, appimage_runtime, expected_runtime_inventory_sha256):
    if output.exists() or output.with_suffix(output.suffix + ".json").exists():
        raise ValueError("Refusing to replace an existing handoff")
    original = inventory(runtime)
    if inventory_sha256(original) != expected_runtime_inventory_sha256:
        raise ValueError("Validated runtime inventory differs from reviewed SHA256")
    inventory(tools)
    if digest(runtime / "usr/bin/aperture") != expected["binary_sha256"]:
        raise ValueError("Validated runtime executable differs")
    prior = json.loads((runtime / "usr/share/aperture/BUILDINFO.json").read_text())
    if native_identity(prior.get("native_dependencies")) != expected["native_dependencies_sha256"]:
        raise ValueError("Validated runtime native dependency identity differs")
    with tempfile.TemporaryDirectory(prefix="aperture-runtime-") as directory:
        root = Path(directory)
        shutil.copytree(runtime, root / "runtime", symlinks=True)
        if inventory_sha256(inventory(root / "runtime")) != expected_runtime_inventory_sha256:
            raise ValueError("Copied runtime inventory differs from reviewed SHA256")
        normalized = []
        copied_runtime = root / "runtime"
        for path in [copied_runtime, *copied_runtime.rglob("*")]:
            if path.is_dir() and not path.is_symlink():
                if stat.S_IMODE(path.stat().st_mode) != 0o755:
                    normalized.append(path.relative_to(copied_runtime).as_posix())
                path.chmod(0o755)
        shutil.copytree(tools, root / "tools", symlinks=True)
        shutil.copyfile(appimage_runtime, root / "tools/runtime-x86_64")
        (root / "tools/runtime-x86_64").chmod(0o755)
        removed = {name: original[name] for name in REMOVED if name in original}
        for name in removed:
            (root / "runtime" / name).unlink()
        metadata = {"BUILDINFO-linux.json": "BUILDINFO.json",
                    "THIRD-PARTY-NOTICES-linux.txt": "THIRD-PARTY-NOTICES.txt",
                    "LICENSE.txt": "LICENSE.txt"}
        with tarfile.open(development, "r:gz") as archive:
            if digest_bytes(archive.extractfile("linux/bin/aperture").read()) != expected["binary_sha256"]:
                raise ValueError("Development executable differs from validated runtime")
            for source, target in metadata.items():
                member = archive.getmember("linux/" + source)
                if not member.isfile():
                    raise ValueError("Development metadata is not a regular file")
                (root / "runtime/usr/share/aperture" / target).write_bytes(archive.extractfile(member).read())
        validate_payload(root, expected)
        manifest = {"schema": 1, **expected, "development_archive_sha256": digest(development),
                    "appimage_runtime_sha256": digest(appimage_runtime),
                    "validated_runtime_inventory_sha256": expected_runtime_inventory_sha256,
                    "directory_mode_normalization": {"mode": "0755", "paths": sorted(normalized)},
                    "removed": removed, "files": inventory(root)}
        (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        output.parent.mkdir(parents=True, exist_ok=True)
        with output.open("xb") as stream, gzip.GzipFile(filename="", fileobj=stream, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                def normalized(member):
                    member.uid = member.gid = member.mtime = 0
                    member.uname = member.gname = "root"
                    return member
                for path in sorted(root.iterdir()):
                    archive.add(path, arcname=path.name, filter=normalized)
        receipt = {"archive_sha256": digest(output), "archive_bytes": output.stat().st_size, **manifest}
        output.with_suffix(output.suffix + ".json").write_text(json.dumps(receipt, indent=2) + "\n")
        return receipt


def digest_bytes(value):
    return hashlib.sha256(value).hexdigest()


def verify_package(extracted, manifest_path):
    manifest = json.loads(manifest_path.read_text())
    expected = {name.removeprefix("runtime/"): record for name, record in manifest["files"].items()
                if name.startswith("runtime/")}
    actual = inventory(extracted)
    if not expected or actual != expected:
        changed = sorted(name for name in actual.keys() | expected.keys() if actual.get(name) != expected.get(name))
        raise ValueError(f"Packaged runtime differs from sealed inputs: {', '.join(changed[:10])}")
    return {"status": "pass", "source_sha": manifest["source_sha"],
            "binary_sha256": manifest["binary_sha256"], "entries_verified": len(actual)}


def stage(archive_path, destination, archive_sha256, expected):
    if digest(archive_path) != archive_sha256:
        raise ValueError("Runtime archive SHA256 differs")
    if destination.exists():
        raise ValueError("Staging destination already exists")
    with tarfile.open(archive_path, "r:gz") as archive:
        members = archive.getmembers()
        names = set()
        links = set()
        for member in members:
            path = PurePosixPath(member.name)
            if not path.parts or str(path) != member.name or path.is_absolute() or ".." in path.parts or member.name in names:
                raise ValueError("Unsafe or duplicate archive path")
            if path.parts[0] not in ("runtime", "tools", "manifest.json") or (path.parts[0] == "manifest.json" and (len(path.parts) != 1 or not member.isfile())):
                raise ValueError("Unexpected archive path")
            if not (member.isfile() or member.isdir() or member.issym()) or member.mode & 0o7000:
                raise ValueError("Unsupported archive entry")
            names.add(member.name)
            if member.issym():
                links.add(member.name)
        for member in members:
            path = PurePosixPath(member.name)
            if any(str(parent) in links for parent in path.parents):
                raise ValueError("Archive writes through a symlink")
            if member.issym():
                target = PurePosixPath(posixpath.normpath(str(path.parent / member.linkname)))
                if not target.parts or PurePosixPath(member.linkname).is_absolute() or target.parts[0] != path.parts[0] or ".." in target.parts:
                    raise ValueError("Escaping archive symlink")
        manifest = json.load(archive.extractfile("manifest.json"))
        if manifest.get("schema") != 1 or any(manifest.get(key) != value for key, value in expected.items()):
            raise ValueError("Runtime manifest identity differs")
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".runtime-stage-", dir=destination.parent) as temporary:
            root = Path(temporary)
            # Preflight forbids symlink parents; keep archive order for sequential gzip reads.
            for member in members:
                path = root / member.name
                path.parent.mkdir(parents=True, exist_ok=True)
                if member.isdir():
                    path.mkdir(exist_ok=True)
                elif member.issym():
                    path.symlink_to(member.linkname)
                else:
                    with path.open("xb") as stream:
                        shutil.copyfileobj(archive.extractfile(member), stream)
                if not member.issym():
                    path.chmod(member.mode)
            (root / "manifest.json").unlink()
            if inventory(root) != manifest.get("files"):
                raise ValueError("Runtime file manifest differs")
            validate_payload(root, expected)
            (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
            root.rename(destination)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    seal_parser = commands.add_parser("seal")
    for name in ("runtime", "tools", "development", "output", "appimage-runtime"):
        seal_parser.add_argument("--" + name, type=Path, required=True)
    seal_parser.add_argument("--expected-runtime-inventory-sha256", required=True)
    stage_parser = commands.add_parser("stage")
    stage_parser.add_argument("archive", type=Path)
    stage_parser.add_argument("destination", type=Path)
    stage_parser.add_argument("--archive-sha256", required=True)
    verify_parser = commands.add_parser("verify-package")
    verify_parser.add_argument("extracted", type=Path)
    verify_parser.add_argument("manifest", type=Path)
    for command in (seal_parser, stage_parser):
        for name in ("source-sha", "binary-sha256", "native-sha256"):
            command.add_argument("--" + name, required=True)
    args = parser.parse_args()
    if args.command == "verify-package":
        print(json.dumps(verify_package(args.extracted, args.manifest)))
        return
    expected = identities(args.source_sha, args.binary_sha256, args.native_sha256)
    if args.command == "seal":
        result = seal(args.runtime, args.tools, args.development, args.output, expected, args.appimage_runtime,
                      args.expected_runtime_inventory_sha256)
        print(json.dumps({key: result[key] for key in ("archive_sha256", "archive_bytes", *expected)}))
    else:
        stage(args.archive, args.destination, args.archive_sha256, expected)
        print(json.dumps(expected))


if __name__ == "__main__":
    main()
