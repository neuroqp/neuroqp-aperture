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
from pathlib import Path, PurePosixPath

import bounded

ROOT = Path(os.environ.get("APERTURE_SOURCE_ROOT", Path(__file__).resolve().parents[2]))
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
SOURCE_PUBLICATION = "scripts/release/corresponding-source.json"


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


def source_plan(path, trusted_sha256=None):
    body = path.read_bytes()
    if trusted_sha256 is not None:
        if not re.fullmatch(r"[0-9a-f]{64}", trusted_sha256) or hashlib.sha256(body).hexdigest() != trusted_sha256:
            raise ValueError("External corresponding-source review differs from its trusted SHA-256")
    plan = json.loads(body)
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
    if "application_source_sha" in plan and not re.fullmatch(r"[0-9a-f]{40}", plan["application_source_sha"] or ""):
        raise ValueError("Reviewed application source SHA must be a full Git commit ID")
    return plan


def application_snapshot(root, source_sha):
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha or ""):
        raise ValueError("Source SHA must be a full Git commit ID")
    if subprocess.check_output(["git", "-c", f"safe.directory={root}", "cat-file", "-t", source_sha], cwd=root).strip() != b"commit":
        raise ValueError("Frozen source SHA must identify a Git commit")
    # Read raw Git objects so export-ignore/export-subst cannot alter the tree.
    tree = subprocess.check_output(["git", "-c", f"safe.directory={root}", "ls-tree", "-rz", source_sha], cwd=root)
    entries = []
    for row in tree.split(b"\0"):
        if not row:
            continue
        identity, name = row.split(b"\t", 1)
        mode, kind, object_sha = identity.split()
        name = name.decode("utf-8")
        if name == SOURCE_PUBLICATION:
            continue
        if kind != b"blob" or mode not in (b"100644", b"100755"):
            raise ValueError(f"Frozen application tree requires regular files: {name}")
        entries.append((name, int(mode[-3:], 8), object_sha))
    objects = subprocess.check_output(["git", "-c", f"safe.directory={root}", "cat-file", "--batch"], cwd=root,
                                      input=b"".join(object_sha + b"\n" for _, _, object_sha in entries))
    files, offset = {}, 0
    for name, mode, object_sha in entries:
        end = objects.index(b"\n", offset)
        found_sha, kind, size = objects[offset:end].split()
        if found_sha != object_sha or kind != b"blob":
            raise ValueError("Git application snapshot returned a different object")
        start, size = end + 1, int(size)
        files[name] = (objects[start:start + size], mode)
        offset = start + size + 1
    return files


def reviewed_plan(root, plan_path=None, plan_sha256=None, source_sha=None, attempt_path=None):
    if (plan_path is None) != (plan_sha256 is None):
        raise ValueError("External review requires both --plan and --plan-sha256")
    if plan_path is not None and not re.fullmatch(r"[0-9a-f]{40}", source_sha or ""):
        raise ValueError("External review requires a frozen full --source-sha")
    path = plan_path if plan_path is not None else root / SOURCE_PUBLICATION
    plan = source_plan(path, plan_sha256)
    if plan_path is not None and plan.get("application_source_sha") != source_sha:
        raise ValueError("External review must pin the frozen application source SHA")
    if plan_path is not None:
        if attempt_path is None:
            raise ValueError("External review requires the original frozen --attempt")
        body = attempt_path.read_bytes()
        expected = json.loads(body.decode("utf-8-sig"))
        bounded.same_identity(expected, bounded.identity(root))
        if expected.get("stage") != "B":
            raise ValueError("External review requires the original Stage B frozen attempt")
        bounded.validate_source_receipt(plan, expected, hashlib.sha256(body).hexdigest())
    elif attempt_path is not None:
        raise ValueError("--attempt requires an external reviewed --plan")
    return path, plan


def verify_source_material(directory, plan, builds=False, platform=None, fixture_locks=None, root=None, source_sha=None, application_locks=None):
    archive = directory / SOURCE_ASSET
    if not archive.is_file() or digest(archive) != plan["sha256"]:
        raise ValueError("Corresponding-source archive missing or different from the reviewed bytes")
    inventory_bytes, members, seen = None, {}, set()
    # Stream the full reviewed archive; never extract or execute its contents.
    with tarfile.open(archive, "r|xz") as source:
        first = source.next()
        if not first or not first.isfile() or first.name != "SOURCE-MANIFEST.json" or first.size > 65536:
            raise ValueError("Source archive must start with a bounded SOURCE-MANIFEST.json identity record")
        record = json.load(source.extractfile(first))
        if record.get("schema") != 1 or record.get("native_dependencies_sha256") != plan["native_dependencies_sha256"]:
            raise ValueError("Source archive identity differs from its reviewed dependency identities")
        if record.get("inventory_sha256", {}).get(SOURCE_INVENTORY) != plan["application_source_inventory_sha256"]:
            raise ValueError("Source archive application inventory differs from its reviewed identity")
        seen.add(first.name)
        for member in iter(source.next, None):
            name = member.name
            if name.startswith("/") or ".." in PurePosixPath(name).parts or str(PurePosixPath(name)) != name or name in seen or not member.isfile():
                raise ValueError(f"Unsafe or duplicate source archive member: {name}")
            seen.add(name)
            if name == SOURCE_INVENTORY:
                if member.size > 65536:
                    raise ValueError("Source archive must contain a bounded application source inventory")
                inventory_bytes = source.extractfile(member).read()
            elif name.startswith("application/source/"):
                result = hashlib.sha256()
                reader = source.extractfile(member)
                lock = name.removeprefix("application/source/") if application_locks is not None else None
                contents = [] if lock in LOCKFILES else None
                if contents is not None and member.size > 16 * 1024 * 1024:
                    raise ValueError("Archived application lockfile exceeds 16 MiB")
                for block in iter(lambda: reader.read(1024 * 1024), b""):
                    result.update(block)
                    if contents is not None:
                        contents.append(block)
                if contents is not None:
                    application_locks[lock] = b"".join(contents)
                members[name.removeprefix("application/source/")] = {
                    "sha256": result.hexdigest(), "size": member.size,
                    "mode": 0o755 if member.mode & 0o111 else 0o644}
    if inventory_bytes is None or hashlib.sha256(inventory_bytes).hexdigest() != plan["application_source_inventory_sha256"]:
        raise ValueError("Source archive application inventory bytes differ from their reviewed hash")
    inventory = json.loads(inventory_bytes)
    lock_entries = [entry for entry in inventory.get("files", []) if entry.get("path") in LOCKFILES]
    if inventory.get("schema") != 1 or len(lock_entries) != len(LOCKFILES) or {entry["path"]: entry.get("sha256") for entry in lock_entries} != plan["application_lockfiles_sha256"]:
        raise ValueError("Source archive application lockfiles differ from the reviewed plan")
    entries = {}
    for entry in inventory.get("files", []):
        name = entry.get("path", "")
        if not name or name.startswith("/") or ".." in PurePosixPath(name).parts or str(PurePosixPath(name)) != name or name in entries or not re.fullmatch(r"[0-9a-f]{64}", entry.get("sha256") or ""):
            raise ValueError("Unsafe or duplicate application source inventory entry")
        entries[name] = {key: entry[key] for key in ("sha256", "size", "mode") if key in entry}
    if set(entries) != set(members) or any(any(members[name][key] != value for key, value in entry.items()) for name, entry in entries.items()):
        raise ValueError("Source archive application members differ from their inventory")
    frozen_sha = source_sha or plan.get("application_source_sha")
    if frozen_sha is not None:
        if not re.fullmatch(r"[0-9a-f]{40}", frozen_sha) or plan.get("application_source_sha", frozen_sha) != frozen_sha or inventory.get("gitBaseSha") != frozen_sha or inventory.get("trackedChanges") != [] or inventory.get("includedUntracked") != [] or inventory.get("excludedPublicationRecord") != SOURCE_PUBLICATION or inventory.get("workingTreePatchSha256") != hashlib.sha256(b"").hexdigest():
            raise ValueError("Source archive is not a clean snapshot of the frozen source SHA")
        if root is not None:
            expected = {name: {"sha256": hashlib.sha256(body).hexdigest(), "size": len(body), "mode": mode}
                        for name, (body, mode) in application_snapshot(root, frozen_sha).items()}
            if entries != expected:
                raise ValueError("Source archive application inventory differs from the frozen Git tree")
    platforms = ("linux", "windows") if builds else ((platform,) if platform else ())
    for platform in platforms:
        info = json.loads((directory / f"BUILDINFO-{platform}.json").read_text(encoding="utf-8"))
        if native_identity(info["native_dependencies"]) != plan["native_dependencies_sha256"][platform]:
            raise ValueError(f"Corresponding source does not match the shipped {platform} native dependency identity")
        if frozen_sha is not None and info.get("source_sha") != frozen_sha:
            raise ValueError(f"Corresponding source does not match the shipped {platform} frozen source SHA")
        expected_locks = fixture_locks if fixture_locks is not None else plan["application_lockfiles_sha256"]
        if info.get("lockfiles") != expected_locks:
            raise ValueError(f"Corresponding source does not match the shipped {platform} application lockfiles")


def upgrade_fixture_locks(directory, root, plan):
    # Only this separate, unsigned acceptance fixture may change the root version.
    info = json.loads((directory / "BUILDINFO-windows.json").read_text(encoding="utf-8"))
    if info["version"] != "0.0.9" or current_version(root) != "0.1.1":
        raise ValueError("Upgrade fixture must be the separate 0.1.1 to 0.0.9 Windows acceptance build")
    production = (root / "src-tauri/Cargo.lock").read_bytes()
    replacement, count = re.subn(rb'(?m)^name = "aperture"\nversion = "[^"\n]+"$', b'name = "aperture"\nversion = "0.0.9"', production)
    delta = directory / "test-deltas/src-tauri/Cargo.lock"
    if count != 1 or not delta.is_file() or delta.read_bytes() != replacement:
        raise ValueError("Upgrade fixture Cargo.lock must differ only in the aperture root version")
    return {**plan["application_lockfiles_sha256"], "src-tauri/Cargo.lock": digest(delta)}


def obtain_source_archive(directory, plan, fetch=False, cache=None):
    directory.mkdir(parents=True, exist_ok=True)
    archive = directory / SOURCE_ASSET
    cached = cache / (plan["sha256"] + ".tar.xz") if cache is not None else None
    if (not archive.is_file() or digest(archive) != plan["sha256"]) and cached is not None and cached.is_file() and digest(cached) == plan["sha256"]:
        shutil.copyfile(cached, archive)
    if fetch and (not archive.is_file() or digest(archive) != plan["sha256"]):
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
    return cached


def store_source_cache(directory, plan, cached):
    if cached is not None and (not cached.is_file() or digest(cached) != plan["sha256"]):
        cached.parent.mkdir(parents=True, exist_ok=True)
        temporary = cached.with_suffix(".partial")
        try:
            shutil.copyfile(directory / SOURCE_ASSET, temporary)
            temporary.replace(cached)
        finally:
            temporary.unlink(missing_ok=True)


def source_material(directory, fetch=False, root=ROOT, platform=None, upgrade_fixture=False, plan_path=None, plan_sha256=None, source_sha=None, cache=None, attempt_path=None):
    review, plan = reviewed_plan(root, plan_path, plan_sha256, source_sha, attempt_path)
    verify_application_locks(root, plan)
    fixture_locks = None
    if upgrade_fixture:
        if platform != "windows":
            raise ValueError("Upgrade fixture source verification requires --platform windows")
        fixture_locks = upgrade_fixture_locks(directory, root, plan)
    cached = obtain_source_archive(directory, plan, fetch, cache)
    verify_source_material(directory, plan, platform=platform, fixture_locks=fixture_locks,
                           root=root if plan_path is not None else None, source_sha=source_sha)
    store_source_cache(directory, plan, cached)
    if review.resolve() != (directory / SOURCE_REVIEW).resolve():
        shutil.copyfile(review, directory / SOURCE_REVIEW)


def development_source(directory, baseline_directory, platform, source_sha, fetch=False, cache=None, root=ROOT):
    """Reference reviewed dependency source plus the public Stage A application commit."""
    if baseline_directory.resolve().is_relative_to(directory.resolve()) or cache is not None and cache.resolve().is_relative_to(directory.resolve()):
        raise ValueError("Development baseline/cache must remain outside the uploaded runtime")
    current = bounded.identity(root)
    if source_sha != current["source_sha"]:
        raise ValueError("Development source must identify the current public Git commit")
    review = root / SOURCE_PUBLICATION
    plan = source_plan(review)
    info_path = directory / f"BUILDINFO-{platform}.json"
    info = json.loads(info_path.read_text(encoding="utf-8"))
    locks = lockfile_hashes(root)
    version = current_version(root)
    if info.get("platform") != platform or info.get("source_sha") != source_sha or info.get("version") != version or info.get("lockfiles") != locks:
        raise ValueError("Development BUILDINFO differs from the current platform/source/version/lockfiles")
    if native_identity(info.get("native_dependencies")) != plan["native_dependencies_sha256"][platform]:
        raise ValueError("Development native dependencies require separately reviewed corresponding source")
    if locks["pnpm-lock.yaml"] != plan["application_lockfiles_sha256"]["pnpm-lock.yaml"]:
        raise ValueError("Development npm dependencies require separately reviewed corresponding source")
    notices = directory / f"THIRD-PARTY-NOTICES-{platform}.txt"
    if not notices.is_file() or not notices.stat().st_size:
        raise ValueError("Development runtime requires third-party notices")
    cached = obtain_source_archive(baseline_directory, plan, fetch, cache)
    original = {}
    verify_source_material(baseline_directory, plan, application_locks=original)
    archived = original["src-tauri/Cargo.lock"]
    pattern = rb'(?m)^name = "aperture"\r?\nversion = "([^"\r\n]+)"\r?$'
    found = re.findall(pattern, archived)
    if len(found) != 1 or not VERSION.fullmatch(found[0].decode()):
        raise ValueError("Reviewed Cargo lock requires exactly one stable aperture root version")
    replacement = re.sub(pattern, lambda match: match[0].replace(match[1], version.encode()), archived)
    if (root / "src-tauri/Cargo.lock").read_bytes() != replacement or (root / "pnpm-lock.yaml").read_bytes() != original["pnpm-lock.yaml"]:
        raise ValueError("Development Cargo/npm dependencies changed; only the exact aperture root-version delta may reuse reviewed source")
    store_source_cache(baseline_directory, plan, cached)
    record = {"schema": 1, "stage": "A", "status": "ready", "platform": platform, "source_sha": source_sha,
              "application_source_url": f"https://github.com/neuroqp/neuroqp-aperture/tree/{source_sha}",
              "build_inputs_sha256": current["build_inputs_sha256"],
              "baseline": {"url": plan["url"], "sha256": plan["sha256"], "review_sha256": digest(review),
                           "application_lockfiles_sha256": plan["application_lockfiles_sha256"]},
              "application_lockfiles_sha256": locks, "native_dependencies_sha256": plan["native_dependencies_sha256"][platform],
              "cargo_root_version": {"baseline": found[0].decode(), "current": version,
                                     "change": "unchanged" if archived == replacement else "aperture-root-version-only"},
              "buildinfo": {"file": info_path.name, "sha256": digest(info_path)},
              "notices": {"file": notices.name, "sha256": digest(notices)}}
    bounded.write_report(directory / "DEVELOPMENT-SOURCE.json", record)


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


def manifest(directory, version, repository, source_sha, published_at, signed=True, root=ROOT, plan_path=None, plan_sha256=None, attempt_path=None):
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise ValueError("Source SHA must be a full Git commit ID")
    if repository != "neuroqp/neuroqp-aperture":
        raise ValueError("Release assets belong in neuroqp/neuroqp-aperture")
    _, plan = reviewed_plan(root, plan_path, plan_sha256, source_sha, attempt_path)
    verify_application_locks(root, plan)
    verify_source_material(directory, plan, builds=True, root=root if plan_path is not None else None,
                           source_sha=source_sha if plan_path is not None else None)
    if plan_path is not None and digest(directory / SOURCE_REVIEW) != plan_sha256:
        raise ValueError("Transferred source review differs from its trusted SHA-256")
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
    verify(directory, version, source_sha, signed, root=root)


def verify(directory, version, source_sha, signed=True, root=None):
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
    plan = source_plan(directory / SOURCE_REVIEW)
    if plan.get("application_source_sha") not in (None, source_sha):
        raise ValueError("Reviewed application source SHA differs from the build identity")
    verify_source_material(directory, plan, builds=True, root=root)
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
    sources.add_argument("--plan", type=Path)
    sources.add_argument("--plan-sha256")
    sources.add_argument("--source-sha")
    sources.add_argument("--attempt", type=Path, help="Original frozen-attempt.json; required with an external review")
    sources.add_argument("--cache", type=Path, help="Optional local cache keyed by the reviewed archive SHA-256")
    sources.add_argument("--platform", choices=("linux", "windows"))
    sources.add_argument("--upgrade-fixture", action="store_true", help="Verify the separate unsigned 0.0.9 Windows fixture root-version delta")
    development = commands.add_parser("development-source")
    development.add_argument("--directory", required=True, type=Path)
    development.add_argument("--baseline-directory", required=True, type=Path)
    development.add_argument("--platform", required=True, choices=("linux", "windows"))
    development.add_argument("--source-sha", required=True)
    development.add_argument("--fetch", action="store_true")
    development.add_argument("--cache", type=Path)
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
            child.add_argument("--plan", type=Path)
            child.add_argument("--plan-sha256")
            child.add_argument("--attempt", type=Path, help="Original frozen-attempt.json; required with an external review")
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
        source_material(args.directory, args.fetch, platform=args.platform, upgrade_fixture=args.upgrade_fixture, plan_path=args.plan, plan_sha256=args.plan_sha256, source_sha=args.source_sha, cache=args.cache, attempt_path=args.attempt)
    elif args.command == "development-source":
        development_source(args.directory, args.baseline_directory, args.platform, args.source_sha, args.fetch, args.cache)
    elif args.command == "version":
        set_version(args.version)
    elif args.command == "notes":
        print(notes(args.version or current_version()), end="")
    elif args.command == "metadata":
        manifest(args.directory, args.version, args.repository, args.source_sha, args.published_at, not args.unsigned, plan_path=args.plan, plan_sha256=args.plan_sha256, attempt_path=args.attempt)
    elif args.command == "verify":
        verify(args.directory, args.version, args.source_sha, not args.unsigned, root=ROOT)
        if args.cryptographic:
            if args.unsigned:
                raise ValueError("Unsigned validation has no signatures to verify")
            verify_signatures(args.directory)
    elif args.command == "buildinfo":
        buildinfo(args.platform, args.directory, args.dependency_file)


if __name__ == "__main__":
    main()
