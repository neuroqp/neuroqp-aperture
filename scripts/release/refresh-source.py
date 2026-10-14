#!/usr/bin/env python3
"""Refresh an immutable application snapshot using reviewed, unchanged sources.

Python 3.10+, no downloads or publication. Without --write, validate inputs and
emit a local preparation receipt only. Review the completed archive before
changing corresponding-source.json or publishing under a new source-only tag.
"""

import argparse
import hashlib
import io
import json
import lzma
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile

import release

EXCLUDED = "scripts/release/corresponding-source.json"
RECORDS = ("SOURCE-MANIFEST.json", release.SOURCE_INVENTORY,
           "application/cargo-vendor-inventory.json", "application/npm-inventory.json")
NATIVE_INPUTS = ("scripts/release/prepare-linux.sh",
                "scripts/release/package-linux.sh", "scripts/release/prepare-windows.ps1",
                "scripts/release/windows-dependencies.json", "scripts/release/windows-lgpl-sources.json",
                "scripts/release/windows-license-provenance.json", "scripts/release/windows-native-licenses.txt")


def encoded(value):
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True) + "\n").encode()


def sha(value):
    return hashlib.sha256(value).hexdigest()


def cargo_packages(body):
    # Match the reviewed assembler's parser for Cargo's generated lock format.
    result = {}
    for block in body.decode().split("[[package]]")[1:]:
        fields = {}
        for line in block.splitlines():
            if line.startswith(("name = ", "version = ", "source = ", "checksum = ")):
                key, value = line.split(" = ", 1)
                fields[key] = json.loads(value)
        if fields.get("source", "").startswith("registry+"):
            result[(fields["name"], fields["version"])] = fields["checksum"]
        elif fields.get("source"):
            raise ValueError("Unsupported Cargo source requires fresh source assembly and review")
    return result


def snapshot(root, source_sha):
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise ValueError("Provide an immutable full application commit SHA")
    data = subprocess.check_output(["git", "archive", "--format=tar", source_sha], cwd=root)
    files = {}
    with tarfile.open(fileobj=io.BytesIO(data)) as tree:
        for member in tree:
            if member.isdir() or member.name == EXCLUDED:
                continue
            if not member.isfile():
                raise ValueError(f"Application snapshot requires regular files: {member.name}")
            files[member.name] = (tree.extractfile(member).read(), 0o755 if member.mode & 0o111 else 0o644)
    return files


def prepare(archive, prior_review, root, source_sha):
    if archive.name != release.SOURCE_ASSET:
        raise ValueError("Prior archive must use the fixed corresponding-source asset name")
    plan = release.source_plan(prior_review)
    release.verify_source_material(archive.parent, plan)
    records = {}
    with tarfile.open(archive, "r|xz") as source:
        for member in source:
            if member.name in RECORDS:
                if not member.isfile() or member.size > 1024 * 1024:
                    raise ValueError("Prior source inventory must be bounded and regular")
                records[member.name] = source.extractfile(member).read()
                if len(records) == len(RECORDS):
                    break
    if set(records) != set(RECORDS):
        raise ValueError("Prior archive lacks application dependency inventories")
    manifest = json.loads(records["SOURCE-MANIFEST.json"])
    for name in RECORDS[1:]:
        if manifest["inventory_sha256"].get(name) != sha(records[name]):
            raise ValueError(f"Prior inventory differs from its manifest: {name}")
    files = snapshot(root, source_sha)
    locks = {name: sha(files[name][0]) for name in release.LOCKFILES}
    if locks["pnpm-lock.yaml"] != plan["application_lockfiles_sha256"]["pnpm-lock.yaml"]:
        raise ValueError("Changed npm dependencies need fresh source assembly and review")
    old_files = {entry["path"]: entry for entry in json.loads(records[release.SOURCE_INVENTORY])["files"]}
    changed_application = sorted(name for name in set(old_files) | set(files)
                                 if name not in old_files or name not in files or sha(files[name][0]) != old_files[name]["sha256"])
    for name in NATIVE_INPUTS:
        if name not in old_files or name not in files or sha(files[name][0]) != old_files[name]["sha256"]:
            raise ValueError(f"Changed native build input requires fresh review: {name}")
    cargo = json.loads(records["application/cargo-vendor-inventory.json"])
    previous = {(entry["name"], entry["version"]): entry for entry in cargo["crates"]}
    locked = cargo_packages(files["src-tauri/Cargo.lock"][0])
    if any(key not in previous or previous[key]["packageChecksum"] != checksum for key, checksum in locked.items()):
        raise ValueError("New or changed Cargo dependencies need fresh source assembly and review")
    removed = {entry["directory"] for key, entry in previous.items() if key not in locked}
    cargo.update(cargoLockSha256=locks["src-tauri/Cargo.lock"],
                 crates=[previous[key] for key in sorted(locked)])
    inventory = {"schema": 1, "gitBaseSha": source_sha, "trackedChanges": [], "includedUntracked": [],
                 "excludedPublicationRecord": EXCLUDED, "workingTreePatchSha256": sha(b""),
                 "files": [{"path": name, "sha256": sha(body), "size": len(body), "mode": mode}
                           for name, (body, mode) in sorted(files.items())]}
    generated = {release.SOURCE_INVENTORY: encoded(inventory),
                 "application/cargo-vendor-inventory.json": encoded(cargo),
                 "application/working-tree.patch": b""}
    if len(generated[release.SOURCE_INVENTORY]) > 65536:
        raise ValueError("Application inventory exceeds the source gate's 64 KiB limit")
    manifest.update(status="assembled-awaiting-review", git_base_sha=source_sha,
                    refresh={"prior_archive_sha256": plan["sha256"], "removed_cargo_directories": sorted(removed),
                             "changed_application_files": changed_application,
                             "recipe": "application/source/scripts/release/refresh-source.py"})
    manifest["inventory_sha256"].update({name: sha(body) for name, body in generated.items() if name.endswith("inventory.json")})
    generated["SOURCE-MANIFEST.json"] = encoded(manifest)
    if len(generated["SOURCE-MANIFEST.json"]) > 65536:
        raise ValueError("Source manifest exceeds the source gate's 64 KiB limit")
    generated.update({"application/source/" + name: body for name, (body, _) in files.items()})
    review = {**plan, "status": "assembled-awaiting-review", "sha256": None,
              "url": f"https://github.com/neuroqp/neuroqp-aperture/releases/download/source-v{json.loads(files['package.json'][0])['version']}/{release.SOURCE_ASSET}",
              "application_lockfiles_sha256": locks,
              "application_source_inventory_sha256": sha(generated[release.SOURCE_INVENTORY])}
    if review["url"] == plan["url"]:
        raise ValueError("Refreshed application sources require a new source-only release tag")
    return generated, files, removed, review


def write_archive(archive, output, generated, files, removed):
    # Preserve every native/npm payload byte. Only the application snapshot,
    # its identities, and no-longer-locked Cargo directories are replaced.
    expected = {}
    temporary = output.with_suffix(".partial")
    try:
        with lzma.LZMAFile(temporary, "w", preset=3) as compressed, tarfile.open(fileobj=compressed, mode="w|", format=tarfile.GNU_FORMAT) as target:
            def add(name, body, mode=0o644):
                info = tarfile.TarInfo(name)
                info.size, info.mode = len(body), mode
                target.addfile(info, io.BytesIO(body))
                expected[name] = sha(body)
            add("SOURCE-MANIFEST.json", generated["SOURCE-MANIFEST.json"])
            # Keep both bounded gate records before the large dependency payloads.
            add(release.SOURCE_INVENTORY, generated[release.SOURCE_INVENTORY])
            prior_names = set()
            with tarfile.open(archive, "r|xz") as source:
                for member in source:
                    name = member.name
                    parts = PurePosixPath(name).parts
                    if not member.isfile() or name.startswith("/") or ".." in parts or name in prior_names:
                        raise ValueError(f"Unsafe or duplicate prior source member: {name}")
                    prior_names.add(name)
                    if name in generated or name.startswith("application/source/") or (name.startswith("application/cargo-vendor/") and parts[2] in removed):
                        continue
                    # Original source packages are bounded by the reviewed archive;
                    # streaming avoids keeping multi-gigabyte payloads in memory.
                    reader = source.extractfile(member)
                    copied = HashReader(reader)
                    target.addfile(member, copied)
                    expected[name] = copied.hash.hexdigest()
            for name, body in sorted(generated.items()):
                if name not in expected:
                    mode = files[name.removeprefix("application/source/")][1] if name.startswith("application/source/") else 0o644
                    add(name, body, mode)
        if temporary.stat().st_size >= 2 * 1024 ** 3:
            raise ValueError("Refreshed archive exceeds GitHub's 2 GiB asset limit")
        seen = set()
        with tarfile.open(temporary, "r|xz") as source:
            for member in source:
                body = HashReader(source.extractfile(member))
                while body.read(1024 * 1024):
                    pass
                if member.name in seen or body.hash.hexdigest() != expected.get(member.name):
                    raise ValueError(f"Refreshed source readback differs: {member.name}")
                seen.add(member.name)
        if seen != set(expected):
            raise ValueError("Refreshed source readback has missing members")
        temporary.replace(output)
        return {"members_verified": len(seen), "archive_bytes": output.stat().st_size,
                "sha256": release.digest(output)}
    finally:
        temporary.unlink(missing_ok=True)


class HashReader:
    def __init__(self, source):
        self.source, self.hash = source, hashlib.sha256()

    def read(self, size=-1):
        body = self.source.read(size)
        self.hash.update(body)
        return body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prior-archive", required=True, type=Path)
    parser.add_argument("--prior-review", required=True, type=Path)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--output", required=True, type=Path, help="New, empty, local material directory")
    parser.add_argument("--root", type=Path, default=release.ROOT)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    if args.prior_archive.name != release.SOURCE_ASSET or args.output.exists():
        parser.error("Use the fixed prior archive name and a new output directory; published bytes must be preserved")
    generated, files, removed, review = prepare(args.prior_archive, args.prior_review, args.root, args.source_sha)
    args.output.mkdir(parents=True)
    receipt = {"application_source_sha": args.source_sha, "prior_archive_sha256": release.digest(args.prior_archive),
               "native_payloads_unchanged": True, "removed_cargo_directories": sorted(removed),
               "changed_application_files": json.loads(generated["SOURCE-MANIFEST.json"])["refresh"]["changed_application_files"],
               "application_source_files": len(files), "archive_written": args.write}
    if args.write:
        receipt.update(write_archive(args.prior_archive, args.output / release.SOURCE_ASSET, generated, files, removed))
        review["sha256"] = receipt["sha256"]
        # The ordinary gate still requires an explicit reviewed ready plan.
        release.verify_source_material(args.output, review)
    (args.output / release.SOURCE_REVIEW).write_bytes(encoded(review))
    (args.output / "REFRESH.json").write_bytes(encoded(receipt))
    print(json.dumps(receipt, sort_keys=True))


if __name__ == "__main__":
    main()
