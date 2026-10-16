#!/usr/bin/env python3
"""Version, candidate metadata and same-byte promotion checks. No dependencies."""

import argparse
import base64
import datetime
import hashlib
import io
import json
import os
import posixpath
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.request
from pathlib import Path, PurePosixPath


ROOT = Path(os.environ.get("APERTURE_SOURCE_ROOT", Path(__file__).resolve().parents[2]))
ASSETS = {
    "windows-x86_64": "Aperture-Setup-x64.exe",
    "windows-x86_64-portable": "Aperture-Portable-x64.exe",
    "linux-x86_64": "Aperture-x86_64.AppImage",
}
VERSION = re.compile(r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\Z")
SOURCE_ASSET = "Aperture-Application-Source.tar.xz"
DEPENDENCY_ASSET = "Aperture-Corresponding-Source.tar.xz"
DEPENDENCY_INVENTORIES = ("application/cargo-vendor-inventory.json", "application/npm-inventory.json")
SOURCE_REVIEW = "Aperture-Corresponding-Source.json"
LOCKFILES = ("pnpm-lock.yaml", "src-tauri/Cargo.lock")
SOURCE_INVENTORY = "application/source-inventory.json"
SOURCE_PUBLICATION = "scripts/release/corresponding-source.json"
SOURCE_COMPOSITION = "Extract the immutable dependency bundle, replace its application/source tree entirely with this application archive, and use the current application/source build recipes and lockfiles. The dependency bundle historical application snapshot is superseded."


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


def source_plan(path, trusted_sha256=None):
    body = path.read_bytes()
    if trusted_sha256 is not None and sha(body) != trusted_sha256:
        raise ValueError("Source plan differs from its trusted SHA-256")
    plan = json.loads(body)
    if plan.get("schema") not in (1, 2) or plan.get("status") != "ready":
        raise ValueError("Corresponding source requires reviewed, ready material")
    expected_asset = DEPENDENCY_ASSET if plan["schema"] == 1 else SOURCE_ASSET
    hashes = plan.get("native_dependencies_sha256", {})
    locks = plan.get("application_lockfiles_sha256", {})
    values = [plan.get("sha256"), plan.get("application_source_inventory_sha256"), *hashes.values(), *locks.values()]
    if (plan.get("asset") != expected_asset or set(hashes) != {"linux", "windows"}
            or set(locks) != set(LOCKFILES) or any(not re.fullmatch(r"[0-9a-f]{64}", value or "") for value in values)):
        raise ValueError("Source plan must pin archive, inventory, locks and both native identities")
    if plan["schema"] == 1:
        url = r"https://github.com/neuroqp/neuroqp-aperture/releases/download/[A-Za-z0-9._-]+/Aperture-Corresponding-Source\.tar\.xz"
        if not re.fullmatch(url, plan.get("url", "")):
            raise ValueError("Dependency source requires an immutable repository release URL")
    elif not re.fullmatch(r"[0-9a-f]{40}", plan.get("application_source_sha", "")):
        raise ValueError("Application source requires a full Git commit ID")
    return plan


def sha(body):
    return hashlib.sha256(body).hexdigest()


def encoded(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def cargo_packages(body):
    packages = {}
    for entry in tomllib.loads(body.decode())["package"]:
        source = entry.get("source")
        if source == "registry+https://github.com/rust-lang/crates.io-index":
            key = entry["name"] + "@" + entry["version"]
            if key in packages or not re.fullmatch(r"[0-9a-f]{64}", entry.get("checksum", "")):
                raise ValueError("Invalid or duplicate Cargo registry dependency")
            packages[key] = entry["checksum"]
        elif source:
            raise ValueError("Unreviewed Cargo source requires dependency source review")
    return dict(sorted(packages.items()))


def local_cargo_sources(files):
    manifests = {name: tomllib.loads(body.decode()) for name, (body, _) in files.items() if name.endswith("Cargo.toml")}
    local = {(value["package"]["name"], value["package"]["version"]) for value in manifests.values() if "package" in value}
    for entry in tomllib.loads(files["src-tauri/Cargo.lock"][0].decode())["package"]:
        if "source" not in entry and (entry["name"], entry["version"]) not in local:
            raise ValueError("Local Cargo package is missing from the application Git tree")
    def check_paths(table, directory):
        for key, value in table.items():
            if isinstance(value, dict):
                if key.endswith("dependencies"):
                    for dependency in value.values():
                        if isinstance(dependency, dict) and "path" in dependency:
                            path = posixpath.normpath(str(directory / dependency["path"] / "Cargo.toml"))
                            if path not in manifests:
                                raise ValueError("Local Cargo dependency path leaves the application Git tree")
                else:
                    check_paths(value, directory)
    for name, value in manifests.items():
        check_paths(value, PurePosixPath(name).parent)


def dependency_closure(files, baseline, records):
    local_cargo_sources(files)
    if set(records) != set(DEPENDENCY_INVENTORIES):
        raise ValueError("Missing reviewed dependency inventories")
    pinned = baseline.get("dependency_inventories_sha256", {})
    if set(pinned) != set(records) or any(sha(body) != pinned[name] for name, body in records.items()):
        raise ValueError("Dependency inventories differ from the reviewed source plan")
    npm = json.loads(records[DEPENDENCY_INVENTORIES[1]])
    npm_hash = sha(files["pnpm-lock.yaml"][0])
    if npm_hash != baseline["application_lockfiles_sha256"]["pnpm-lock.yaml"] or npm.get("lockfile_sha256") != npm_hash:
        raise ValueError("Changed npm dependencies require dependency source review")
    vendor = json.loads(records[DEPENDENCY_INVENTORIES[0]])
    covered = {entry["name"] + "@" + entry["version"]: entry["packageChecksum"] for entry in vendor["crates"]}
    locked = cargo_packages(files["src-tauri/Cargo.lock"][0])
    if any(covered.get(name) != checksum for name, checksum in locked.items()):
        raise ValueError("New or changed Cargo dependencies require dependency source review")
    return {"cargo_registry_packages": locked, "npm_lockfile_sha256": npm_hash,
            "local_sources": "application/source (the complete tracked Git tree)"}


def reviewed_plan(root, plan_path=None, plan_sha256=None, source_sha=None):
    if (plan_path is None) != (plan_sha256 is None):
        raise ValueError("External source plan requires both --plan and --plan-sha256")
    path = plan_path if plan_path is not None else root / SOURCE_PUBLICATION
    plan = source_plan(path, plan_sha256)
    if source_sha is not None and plan.get("application_source_sha", source_sha) != source_sha:
        raise ValueError("Source plan differs from the requested Git commit")
    return path, plan


def verify_source_material(directory, plan, builds=False, platform=None, root=None, source_sha=None):
    if plan.get("schema") != 2:
        raise ValueError("Release requires a composed application and dependency source plan")
    baseline = plan.get("dependency_source", {})
    if baseline.get("schema") != 1 or baseline.get("native_dependencies_sha256") != plan["native_dependencies_sha256"]:
        raise ValueError("Composition differs from its reviewed native source identities")
    if root is not None and baseline != source_plan(root / SOURCE_PUBLICATION):
        raise ValueError("Composition differs from the checkout's reviewed dependency source")
    archive = directory / SOURCE_ASSET
    if not archive.is_file() or digest(archive) != plan["sha256"]:
        raise ValueError("Application source archive missing or different from the composed bytes")
    files, records, seen, inventory_body = {}, {}, set(), None
    with tarfile.open(archive, "r|xz") as source:
        for member in source:
            name = member.name
            if (not member.isfile() or name.startswith("/") or ".." in PurePosixPath(name).parts
                    or str(PurePosixPath(name)) != name or name in seen or member.size > 64 * 1024 * 1024):
                raise ValueError(f"Unsafe or duplicate application source member: {name}")
            seen.add(name)
            body = source.extractfile(member).read()
            if name == SOURCE_INVENTORY:
                inventory_body = body
            elif name in DEPENDENCY_INVENTORIES:
                records[name] = body
            elif name.startswith("application/source/"):
                files[name.removeprefix("application/source/")] = (body, 0o755 if member.mode & 0o111 else 0o644)
            else:
                raise ValueError(f"Unexpected application source member: {name}")
    if inventory_body is None or sha(inventory_body) != plan["application_source_inventory_sha256"]:
        raise ValueError("Application inventory differs from its composed hash")
    inventory = json.loads(inventory_body)
    expected = {"schema": 2, "source_sha": plan["application_source_sha"], "files": source_inventory(files)}
    if inventory != expected or (source_sha is not None and source_sha != inventory["source_sha"]):
        raise ValueError("Application archive differs from the exact source inventory")
    if root is not None and files != application_snapshot(root, inventory["source_sha"]):
        raise ValueError("Application source archive differs from its Git tree")
    if baseline != json.loads(files[SOURCE_PUBLICATION][0]):
        raise ValueError("Dependency source differs from its review in the archived Git tree")
    if plan.get("composition") != SOURCE_COMPOSITION:
        raise ValueError("Composition must select the current application tree and build recipes")
    locks = {name: sha(files[name][0]) for name in LOCKFILES}
    if locks != plan["application_lockfiles_sha256"] or dependency_closure(files, baseline, records) != plan.get("dependency_closure"):
        raise ValueError("Application dependency closure differs from the composition")
    for target in (("linux", "windows") if builds else ((platform,) if platform else ())):
        info = json.loads((directory / f"BUILDINFO-{target}.json").read_text(encoding="utf-8"))
        if (native_identity(info["native_dependencies"]) != plan["native_dependencies_sha256"][target]
                or info.get("source_sha") != inventory["source_sha"] or info.get("lockfiles") != locks):
            raise ValueError(f"Corresponding source differs from the shipped {target} build")


def source_inventory(files):
    return [{"path": name, "sha256": sha(body), "size": len(body), "mode": mode}
            for name, (body, mode) in sorted(files.items())]


def dependency_records(root, baseline, fetch=False, cache=None):
    """Reuse immutable bytes and small hash-verified inventory cache; never copy the bundle."""
    cache = cache if cache is not None else root / "build/source-cache"
    cache.mkdir(parents=True, exist_ok=True)
    pinned = baseline.get("dependency_inventories_sha256", {})
    if set(pinned) != set(DEPENDENCY_INVENTORIES):
        raise ValueError("Reviewed dependency source plan must pin both dependency inventories")
    records = {}
    for name in DEPENDENCY_INVENTORIES:
        cached = cache / (pinned[name] + ".json")
        if cached.is_file() and digest(cached) == pinned[name]:
            records[name] = cached.read_bytes()
    archive = cache / (baseline["sha256"] + ".tar.xz")
    if not archive.is_file() or digest(archive) != baseline["sha256"]:
        if not fetch:
            raise ValueError("Verified immutable dependency source bundle missing from cache; use --fetch")
        temporary = archive.with_suffix(".partial")
        try:
            with urllib.request.urlopen(baseline["url"], timeout=60) as source, temporary.open("wb") as output:
                if not source.geturl().startswith("https://"):
                    raise ValueError("Dependency source download redirected away from HTTPS")
                shutil.copyfileobj(source, output, length=1024 * 1024)
            if digest(temporary) != baseline["sha256"]:
                raise ValueError("Dependency source archive differs from its reviewed checksum")
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)
    if len(records) != len(DEPENDENCY_INVENTORIES):
        with tarfile.open(archive, "r|xz") as source:
            first = source.next()
            if not first or first.name != "SOURCE-MANIFEST.json" or not first.isfile() or first.size > 65536:
                raise ValueError("Dependency source lacks its bounded original manifest")
            manifest = json.load(source.extractfile(first))
            if manifest.get("native_dependencies_sha256") != baseline["native_dependencies_sha256"]:
                raise ValueError("Dependency archive native identities differ from review")
            if any(manifest.get("inventory_sha256", {}).get(name) != pinned[name] for name in DEPENDENCY_INVENTORIES):
                raise ValueError("Dependency archive inventories differ from review")
            for member in source:
                if member.name in DEPENDENCY_INVENTORIES:
                    if not member.isfile() or member.size > 1024 * 1024:
                        raise ValueError("Dependency inventory must be bounded and regular")
                    body = source.extractfile(member).read()
                    if sha(body) != pinned[member.name]:
                        raise ValueError("Dependency inventory bytes differ from review")
                    records[member.name] = body
                    (cache / (pinned[member.name] + ".json")).write_bytes(body)
                if len(records) == len(DEPENDENCY_INVENTORIES):
                    break
    if len(records) != len(DEPENDENCY_INVENTORIES):
        raise ValueError("Dependency archive is missing reviewed inventories")
    return records


def source_material(directory, fetch=False, root=ROOT, platform=None, plan_path=None, plan_sha256=None, source_sha=None, cache=None):
    source_sha = source_sha or subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    if plan_path is not None:
        _, plan = reviewed_plan(root, plan_path, plan_sha256, source_sha)
        verify_source_material(directory, plan, platform=platform, root=root, source_sha=source_sha)
    else:
        baseline = source_plan(root / SOURCE_PUBLICATION)
        files = application_snapshot(root, source_sha)
        if sha(files["pnpm-lock.yaml"][0]) != baseline["application_lockfiles_sha256"]["pnpm-lock.yaml"]:
            raise ValueError("Changed npm dependencies require dependency source review")
        records = dependency_records(root, baseline, fetch, cache)
        closure = dependency_closure(files, baseline, records)
        inventory = encoded({"schema": 2, "source_sha": source_sha, "files": source_inventory(files)})
        directory.mkdir(parents=True, exist_ok=True)
        archive = directory / SOURCE_ASSET
        temporary = archive.with_suffix(".partial")
        try:
            with tarfile.open(temporary, "w:xz", format=tarfile.GNU_FORMAT, preset=3) as target:
                for name, body, mode in [(SOURCE_INVENTORY, inventory, 0o644),
                        *((name, body, 0o644) for name, body in sorted(records.items())),
                        *(("application/source/" + name, body, mode) for name, (body, mode) in sorted(files.items()))]:
                    entry = tarfile.TarInfo(name)
                    entry.size, entry.mode = len(body), mode
                    target.addfile(entry, io.BytesIO(body))
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)
        plan = {"schema": 2, "status": "ready", "asset": SOURCE_ASSET, "sha256": digest(archive),
                "application_source_sha": source_sha, "application_source_inventory_sha256": sha(inventory),
                "application_lockfiles_sha256": {name: sha(files[name][0]) for name in LOCKFILES},
                "native_dependencies_sha256": baseline["native_dependencies_sha256"],
                "dependency_source": baseline, "dependency_closure": closure, "composition": SOURCE_COMPOSITION}
        verify_source_material(directory, plan, root=root, source_sha=source_sha, platform=platform)
        (directory / SOURCE_REVIEW).write_bytes(encoded(plan))
    return plan


def development_source(directory, baseline_directory, platform, source_sha, fetch=False, cache=None, root=ROOT):
    review = baseline_directory / SOURCE_REVIEW
    if review.is_file():
        plan = source_material(baseline_directory, root=root, source_sha=source_sha,
                               plan_path=review, plan_sha256=digest(review))
    else:
        plan = source_material(baseline_directory, fetch, root, source_sha=source_sha, cache=cache)
    verify_source_material(baseline_directory, plan, root=root, source_sha=source_sha)
    info_path = directory / f"BUILDINFO-{platform}.json"
    info = json.loads(info_path.read_text(encoding="utf-8"))
    notices = directory / f"THIRD-PARTY-NOTICES-{platform}.txt"
    if (info.get("source_sha") != source_sha or info.get("platform") != platform
            or info.get("version") != current_version(root) or info.get("lockfiles") != plan["application_lockfiles_sha256"]
            or native_identity(info["native_dependencies"]) != plan["native_dependencies_sha256"][platform]
            or not notices.is_file() or not notices.stat().st_size):
        raise ValueError("Development runtime source, native dependencies, locks or notices differ")
    record = {"schema": 2, "platform": platform, "source_sha": source_sha,
              "source_material_sha256": digest(baseline_directory / SOURCE_REVIEW),
              "dependency_source": plan["dependency_source"], "dependency_closure": plan["dependency_closure"],
              "application_lockfiles_sha256": plan["application_lockfiles_sha256"],
              "native_dependencies_sha256": plan["native_dependencies_sha256"][platform],
              "buildinfo": {"file": info_path.name, "sha256": digest(info_path)},
              "notices": {"file": notices.name, "sha256": digest(notices)}}
    (directory / "DEVELOPMENT-SOURCE.json").write_bytes(encoded(record))


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


def manifest(directory, version, repository, source_sha, published_at, signed=True, root=ROOT, plan_path=None, plan_sha256=None):
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise ValueError("Source SHA must be a full Git commit ID")
    if repository != "neuroqp/neuroqp-aperture":
        raise ValueError("Release assets belong in neuroqp/neuroqp-aperture")
    _, plan = reviewed_plan(root, plan_path or directory / SOURCE_REVIEW, plan_sha256 or digest(directory / SOURCE_REVIEW), source_sha)
    verify_application_locks(root, plan)
    verify_source_material(directory, plan, builds=True, root=root, source_sha=source_sha)
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
    sources.add_argument("--cache", type=Path, help="Optional local cache keyed by the reviewed archive SHA-256")
    sources.add_argument("--platform", choices=("linux", "windows"))
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
        source_material(args.directory, args.fetch, platform=args.platform, plan_path=args.plan, plan_sha256=args.plan_sha256, source_sha=args.source_sha, cache=args.cache)
    elif args.command == "development-source":
        development_source(args.directory, args.baseline_directory, args.platform, args.source_sha, args.fetch, args.cache)
    elif args.command == "version":
        set_version(args.version)
    elif args.command == "notes":
        print(notes(args.version or current_version()), end="")
    elif args.command == "metadata":
        manifest(args.directory, args.version, args.repository, args.source_sha, args.published_at, not args.unsigned, plan_path=args.plan, plan_sha256=args.plan_sha256)
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
