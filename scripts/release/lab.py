#!/usr/bin/env python3
"""Inventory the owned QA lab; plan exact cleanup and prepare offline CoW runs."""

import argparse
from contextlib import contextmanager
import datetime as dt
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys

REPO = Path(__file__).resolve().parents[2]
DEFAULT_ROOT = REPO.parent / "neuroqp-aperture-release/artifacts/first-release"
GIB = 1024**3
DISK = "environments/windows11/windows11.qcow2"
OBSOLETE = ("environments/windows10/windows10.qcow2", "environments/windows10/windows10-eval.iso",
            "windows10/Windows10-LTSC-2021-eval-x64.iso")
INSTALL_ISO = "environments/windows11/windows11-eval.iso"
STATE = "environments/bounded-lab"


def now():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def write(path, value):
    path = Path(path)
    if path.exists() or path.is_symlink():
        raise ValueError(f"Receipt already exists: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def inside(root, relative):
    relative = Path(relative)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError("Only relative owned paths are allowed")
    path = root / relative
    for part in (path, *path.parents):
        if part == root:
            break
        if part.is_symlink():
            raise ValueError(f"Symlink is not an owned file: {part}")
    if not path.resolve().is_relative_to(root.resolve()):
        raise ValueError("Path leaves owned lab")
    return path


def identity(path):
    s = path.stat()
    if not stat.S_ISREG(s.st_mode):
        raise ValueError(f"Regular file required: {path}")
    if s.st_uid != os.getuid():
        raise ValueError(f"File is not owned by the lab operator: {path}")
    return {"device": s.st_dev, "inode": s.st_ino, "size": s.st_size,
            "allocated": s.st_blocks * 512, "mtime_ns": s.st_mtime_ns, "links": s.st_nlink}


def inactive():
    active = []
    for item in Path("/proc").iterdir():
        if not item.name.isdecimal():
            continue
        try:
            if item.stat().st_uid != os.getuid():
                continue
            command = (item / "cmdline").read_bytes().split(b"\0")[0]
            if Path(os.fsdecode(command)).name.startswith(("qemu-system", "swtpm")):
                active.append(int(item.name))
        except (FileNotFoundError, ProcessLookupError):
            continue
        except PermissionError as error:
            raise ValueError("Cannot inspect owned processes; mutation refused") from error
    if active:
        raise ValueError(f"Owned VM/TPM processes must stop first: {active}")


@contextmanager
def operation(root):
    # One offline mutation at a time. Launchers must use this same operation lock.
    folder = inside(root, STATE)
    folder.mkdir(parents=True, exist_ok=True)
    with (folder / "operation.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        inactive()
        yield


def inventory(root, extents=False):
    groups, seen, images, unique_files = {}, set(), [], []
    for folder, dirs, files in os.walk(root, followlinks=False):
        dirs[:] = [name for name in dirs if not (Path(folder) / name).is_symlink()]
        for name in files:
            path = Path(folder) / name
            if path.is_symlink() or not path.is_file():
                continue
            entry = identity(path)
            relative = str(path.relative_to(root))
            key = (entry["device"], entry["inode"])
            group = "/".join(Path(relative).parts[:2])
            if key not in seen:
                groups[group] = groups.get(group, 0) + entry["allocated"]
                unique_files.append({"path": relative, **entry})
                seen.add(key)
            if path.suffix.lower() in (".qcow2", ".iso", ".raw"):
                images.append({"path": relative, **entry})
    result = {"schema": 1, "at": now(), "root": str(root), "allocated_unique_inode_bytes": sum(groups.values()),
              "environment_allocated_unique_inode_bytes": sum(value for key, value in groups.items()
                                                               if key.startswith("environments/")),
              "groups": dict(sorted(groups.items(), key=lambda row: -row[1])), "images": images,
              "largest_unique_files": sorted(unique_files, key=lambda row: -row["allocated"])[:30],
              "readiness": {"windows11": "blocked: last SIGTERM requires guest recovery and graceful shutdown before base freeze",
                            "linux": "blocked: verified live ISOs exist, persistent prepared desktops/tools do not"},
              "reclaimed_bytes_claimed": 0}
    base = inside(root, STATE + "/windows11-base.json")
    if base.exists():
        record = read(base)
        disk = inside(root, DISK)
        if record.get("immutable") is True and record.get("disk_identity") == identity(disk) and not disk.stat().st_mode & 0o222:
            result["readiness"]["windows11"] = "verified immutable disk base registered; each disposable run still requires native validation"
    if extents and images:
        output = subprocess.check_output(["btrfs", "filesystem", "du", "-s", "--raw",
                                          *[str(root / row["path"]) for row in images]], text=True, timeout=180)
        result["btrfs_extents"] = output
        totals = subprocess.check_output(["btrfs", "filesystem", "du", "-s", "--raw", str(root),
                                          str(inside(root, "environments"))], text=True, timeout=180)
        result["btrfs_tree_extents"] = totals
        result["btrfs_referenced_extent_bytes"] = {}
        for line in totals.splitlines()[1:]:
            fields = line.split(maxsplit=3)
            if len(fields) == 4 and all(value.isdecimal() for value in fields[:3]):
                result["btrfs_referenced_extent_bytes"][fields[3]] = int(fields[1]) + int(fields[2])
        result["allocation_note"] = "Unique inodes do not deduplicate reflinks; use exclusive extents for reclaim estimates."
    return result


def diskboot_verified(root, receipt):
    data = read(receipt)
    if data.get("schema") != 1 or data.get("status") != "pass" or data.get("install_media_attached") is not False:
        raise ValueError("Actual verified disk-only boot receipt required before removing install ISO")
    if data.get("disk_identity") != identity(inside(root, DISK)):
        raise ValueError("Disk-only boot receipt is stale")


def cleanup_plan(root, receipt=None):
    inactive()
    names = list(OBSOLETE)
    if receipt:
        diskboot_verified(root, receipt)
        names.append(INSTALL_ISO)
    rows = [{"path": name, "identity": identity(inside(root, name))} for name in names if inside(root, name).exists()]
    selected = {row["path"] for row in rows}
    aliases = {}
    for row in rows:
        entry = row["identity"]
        key = (entry["device"], entry["inode"])
        if key not in aliases:
            aliases[key] = [str(path.relative_to(root)) for path in root.rglob("*")
                            if path.is_file() and not path.is_symlink()
                            and (path.stat().st_dev, path.stat().st_ino) == key]
    removable = {key: identity(inside(root, paths[0]))["allocated"] for key, paths in aliases.items()
                 if set(paths).issubset(selected) and len(paths) == identity(inside(root, paths[0]))["links"]}
    return {"schema": 1, "at": now(), "root": str(root), "action": "unlink exact obsolete lab files",
            "diskboot_receipt": str(Path(receipt).resolve()) if receipt else None, "files": rows,
            "estimated_unique_inode_reduction_bytes": sum(removable.values()),
            "hardlink_aliases": list(aliases.values()),
            "preserve": "licensed Win11 disk, TPM/UEFI/private credentials, canonical previous stable packages and all evidence"}


def cleanup(root, manifest):
    data = read(manifest)
    if data.get("schema") != 1 or data.get("root") != str(root) or data.get("action") != "unlink exact obsolete lab files":
        raise ValueError("Exact versioned cleanup manifest required")
    rows = data.get("files", [])
    if not rows or len({row["path"] for row in rows}) != len(rows):
        raise ValueError("Empty or duplicate cleanup plan")
    allowed = set(OBSOLETE)
    if INSTALL_ISO in {row["path"] for row in rows}:
        diskboot_verified(root, data["diskboot_receipt"])
        allowed.add(INSTALL_ISO)
    with operation(root):
        for row in rows:
            if row["path"] not in allowed or identity(inside(root, row["path"])) != row["identity"]:
                raise ValueError("Cleanup target is protected, changed, or outside exact allowlist")
        before = inventory(root)["allocated_unique_inode_bytes"]
        unlinked = {}
        for row in rows:
            inactive()
            path = inside(root, row["path"])
            expected = row["identity"]
            key = (expected["device"], expected["inode"])
            if identity(path) != {**expected, "links": expected["links"] - unlinked.get(key, 0)}:
                raise ValueError("Cleanup target changed during cleanup; remaining files preserved")
            path.unlink()
            unlinked[key] = unlinked.get(key, 0) + 1
        after = inventory(root)["allocated_unique_inode_bytes"]
    return {"schema": 1, "at": now(), "manifest": str(Path(manifest).resolve()), "removed": rows,
            "allocated_before": before, "allocated_after": after, "allocated_removed": before - after,
            "note": "Allocated inode reduction is not a filesystem-free-space claim when reflinks/snapshots share extents."}


def qemu(root, args, mutate=False, timeout=180):
    tool = shutil.which("qemu-img")
    if tool:
        command = [tool, *args]
    else:
        rootfs = inside(root, "environments/debian12/rootfs")
        if not (rootfs / "usr/bin/qemu-img").is_file() or not shutil.which("bwrap"):
            raise ValueError("Existing qemu-img tool unavailable; no downloads or lab reconstruction")
        def guest(value):
            value = str(value)
            return "/lab/" + str(Path(value).relative_to(root)) if value.startswith(str(root) + "/") else value
        command = ["bwrap", "--unshare-user", "--uid", str(os.getuid()), "--gid", str(os.getgid()),
                   "--ro-bind", str(rootfs), "/", "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
                   "--bind" if mutate else "--ro-bind", str(root), "/lab", "/usr/bin/qemu-img", *map(guest, args)]
    return subprocess.check_output(list(map(str, command)), text=True, timeout=timeout)


def recovered(root, receipt):
    data = read(receipt)
    if data.get("schema") != 1 or data.get("guest_shutdown") != "graceful" or data.get("qemu_exited") is not True:
        raise ValueError("Observed guest recovery and graceful shutdown receipt required; SIGTERM is insufficient")
    if data.get("licensed") is not True or data.get("disk_identity") != identity(inside(root, DISK)):
        raise ValueError("Clean licensed baseline receipt is missing or stale")


def freeze_base(root, receipt):
    with operation(root):
        recovered(root, receipt)
        disk = inside(root, DISK)
        info = json.loads(qemu(root, ["info", "--output=json", disk]))
        if info.get("dirty-flag") or info.get("backing-filename"):
            raise ValueError("Clean standalone qcow2 required")
        for path in root.rglob("*.qcow2"):
            if path == disk:
                continue
            child = json.loads(qemu(root, ["info", "--output=json", path]))
            if child.get("backing-filename") or child.get("full-backing-filename"):
                raise ValueError("Base freezing refused while lab backing chains exist")
        record = {"schema": 1, "at": now(), "disk": DISK, "disk_identity": identity(disk),
                  "recovery_receipt": str(Path(receipt).resolve()), "immutable": True}
        destination = inside(root, STATE + "/windows11-base.json")
        if destination.exists():
            raise ValueError("Base already registered")
        disk.chmod(disk.stat().st_mode & ~0o222)
        write(destination, record)
    return record


def overlay(root, name):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}", name):
        raise ValueError("Simple run name required")
    with operation(root):
        record = read(inside(root, STATE + "/windows11-base.json"))
        disk = inside(root, DISK)
        if record.get("immutable") is not True or identity(disk) != record["disk_identity"] or disk.stat().st_mode & 0o222:
            raise ValueError("Unchanged immutable verified base required")
        destination = inside(root, STATE + "/runs/" + name)
        destination.mkdir(parents=True, exist_ok=False)
        qemu(root, ["create", "-f", "qcow2", "-F", "qcow2", "-b", disk, destination / "disk.qcow2"], mutate=True)
        source = inside(root, "environments/windows11")
        for filename in ("uefi-vars.fd", "tpm"):
            path = inside(root, str((source / filename).relative_to(root)))
            if path.is_dir():
                if any(p.is_symlink() for p in path.rglob("*")):
                    raise ValueError("TPM state cannot contain symlinks")
                shutil.copytree(path, destination / filename)
            else:
                shutil.copy2(path, destination / filename)
        result = {"schema": 1, "at": now(), "base": str(disk), "run": str(destination),
                  "disk": str(destination / "disk.qcow2"), "readiness": "not booted or validated",
                  "maximum_overlay_allocated_bytes": 4 * GIB}
        write(destination / "run.json", result)
    return result


def compact(root, receipt):
    with operation(root):
        recovered(root, receipt)
        if inside(root, STATE + "/windows11-base.json").exists():
            raise ValueError("Compact before freezing a base; never replace a backing file used by overlays")
        disk = inside(root, DISK)
        for path in root.rglob("*.qcow2"):
            info = json.loads(qemu(root, ["info", "--output=json", path]))
            if info.get("backing-filename") or info.get("full-backing-filename"):
                raise ValueError("Compaction refused while any lab backing chain exists")
        before = identity(disk)
        if shutil.disk_usage(root).free < before["allocated"] + 4 * GIB:
            raise ValueError("Compaction requires current disk allocation plus 4 GiB free for a worst-case safe temporary copy")
        temporary = inside(root, DISK + ".compact.tmp")
        if temporary.exists():
            raise ValueError("Previous compaction temporary file preserved; inspect it first")
        try:
            qemu(root, ["convert", "-c", "-O", "qcow2", disk, temporary], mutate=True, timeout=1800)
            qemu(root, ["compare", "-f", "qcow2", "-F", "qcow2", disk, temporary], timeout=1800)
            inactive()
            if identity(disk) != before or identity(temporary)["allocated"] >= before["allocated"]:
                raise ValueError("Original changed or compaction saved no space; original retained")
            temporary.chmod(disk.stat().st_mode)
            os.replace(temporary, disk)
        except Exception:
            # Failed output stays for diagnosis; never replace an unverified image.
            raise
        return {"schema": 1, "at": now(), "disk": DISK, "before": before, "after": identity(disk),
                "disk_identity": identity(disk), "guest_shutdown": "graceful", "qemu_exited": True,
                "licensed": True, "recovery_receipt": str(Path(receipt).resolve()),
                "guest_sectors_compared": True, "atomic_replacement": True, "TPM_UEFI_unchanged": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    commands = parser.add_subparsers(dest="command", required=True)
    inv = commands.add_parser("inventory"); inv.add_argument("--btrfs", action="store_true")
    plan = commands.add_parser("plan-cleanup"); plan.add_argument("--diskboot-receipt", type=Path)
    delete = commands.add_parser("cleanup"); delete.add_argument("manifest", type=Path)
    for name in ("freeze-base", "compact"):
        commands.add_parser(name).add_argument("recovery_receipt", type=Path)
    commands.add_parser("overlay").add_argument("name")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(); root = args.root.resolve()
    if not root.is_dir() or args.root.is_symlink():
        raise ValueError("Existing owned lab directory required")
    if args.output.exists():
        raise ValueError("Fresh versioned output receipt required")
    if args.command == "inventory": result = inventory(root, args.btrfs)
    elif args.command == "plan-cleanup": result = cleanup_plan(root, args.diskboot_receipt)
    elif args.command == "cleanup": result = cleanup(root, args.manifest)
    elif args.command == "freeze-base": result = freeze_base(root, args.recovery_receipt)
    elif args.command == "overlay": result = overlay(root, args.name)
    else: result = compact(root, args.recovery_receipt)
    write(args.output, result)
    print(f"Recorded {args.command}: {args.output}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Lab operation blocked: {error}", file=sys.stderr)
        sys.exit(1)
