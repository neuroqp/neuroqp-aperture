#!/usr/bin/env python3
"""Verify the assembled Windows source inventory, optionally against BUILDINFO."""

import argparse
import hashlib
import json
from pathlib import Path


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def verify(directory, dependencies=None):
    inventory = json.loads((directory / "inventory.json").read_text(encoding="utf-8"))
    paths = {}
    for item in inventory["files"]:
        path = (directory / item["path"]).resolve()
        if not path.is_relative_to(directory.resolve()) or item["path"] in paths:
            raise ValueError("Unsafe or duplicate source inventory path")
        paths[item["path"]] = item
        if path.stat().st_size != item["size"] or digest(path) != item["sha256"]:
            raise ValueError(f"Source/build material differs: {item['path']}")
    def archive_verified(item):
        return (item["verified"] and item["archive"] in paths
                and item["sha256"] == paths[item["archive"]]["sha256"])

    sources = inventory["sourceArchives"]
    if not all(archive_verified(item) for item in sources):
        raise ValueError("Missing or unverified recipe source archive")
    versions = {item["nativeKey"]: item["version"] for item in sources if item["nativeKey"]}
    if versions != inventory["nativeComponents"]:
        raise ValueError("Source versions differ from the complete native component inventory")
    crates = inventory["staticSources"]["librsvgCargo"]
    if crates["nonRegistrySources"] or not all(archive_verified(item) for item in crates["packages"]):
        raise ValueError("Missing librsvg Cargo source archive")
    for component in ("gvdb", "nsis", "nsisTauriUtils"):
        if not archive_verified(inventory["staticSources"][component]):
            raise ValueError(f"Missing {component} static/runtime source archive")
    if not all(archive_verified(item) for item in
               inventory["staticSources"]["nsisTauriUtils"]["licenseEvidenceSources"]):
        raise ValueError("Missing NSIS plugin dependency source/license evidence")
    for component in ("patchedCargoLock", "compilerAndRustStd"):
        if inventory["staticSources"][component] not in paths:
            raise ValueError(f"Missing {component} build material")
    if any(not item["allApplied"] or any(
            not patch["applied"] or patch["path"] not in paths
            or patch["sha256"] != paths[patch["path"]]["sha256"]
            for patch in item["patches"]) for item in inventory["patchApplication"]):
        raise ValueError("A vendor patch did not apply to its pinned source archive")
    if any(not item["sourceArchives"] or any(path not in paths for path in item["sourceArchives"])
           for item in inventory["nativePeFiles"]):
        raise ValueError("A bundled native executable has no preserved source mapping")
    if dependencies:
        actual = json.loads(dependencies.read_text(encoding="utf-8"))
        actual = actual.get("native_dependencies", actual)
        if actual["nativeComponents"] != inventory["nativeComponents"] or actual["libvips"] != inventory["nativeVendor"]:
            raise ValueError("Windows BUILDINFO identifies different native binaries or versions")
    return inventory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--native-dependencies", type=Path)
    args = parser.parse_args()
    inventory = verify(args.directory, args.native_dependencies)
    print(f"Verified {len(inventory['files'])} source/build files; audit status={inventory['status']}")
    print("Verification does not approve publication or erase the recorded provenance limits.")


if __name__ == "__main__":
    main()
