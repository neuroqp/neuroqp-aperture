#!/usr/bin/env python3
"""Prepare and record private previous-stable-code updater fixtures.

The application version and locks stay unchanged. Only the test-only updater
transport and unsigned packaging configuration differ from the previous release.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess

import release

PRIOR_TAG = "v0.1.1"
PRIOR_SHA = "b34a2de71298e6c74980e502655dbe8ac73e0632"
PRIOR_VERSION = "0.1.1"


def validate_baseline(source_sha, version, updater, production_key, signed=False):
    if signed or os.environ.get("TAURI_SIGNING_PRIVATE_KEY") or os.environ.get("TAURI_SIGNING_PRIVATE_KEY_PASSWORD"):
        raise ValueError("Previous-code fixtures require an unsigned context without signing secrets")
    if source_sha != PRIOR_SHA or version != PRIOR_VERSION:
        raise ValueError("Previous-code fixture requires the reviewed v0.1.1 source and unchanged version")
    if not production_key or updater.get("pubkey") != production_key or updater.get("requireSignedVersion") is not True:
        raise ValueError("Previous-code fixtures must retain the production key and signed-version checks")


def fixture(root, output, platform, production_root, record=False):
    source_sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    config = json.loads((root / "src-tauri/tauri.conf.json").read_text())
    production = json.loads((production_root / "src-tauri/tauri.conf.json").read_text())
    validate_baseline(source_sha, release.current_version(root), config["plugins"]["updater"],
                      production["plugins"]["updater"]["pubkey"], os.environ.get("SIGNED") == "true")
    if subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=root):
        raise ValueError("Previous-code fixture checkout must retain its exact committed source")
    plan = release.source_plan(root / "scripts/release/corresponding-source.json")
    release.verify_application_locks(root, plan)
    endpoint = "http://127.0.0.1:" + ("18080" if platform == "windows" else "8763") + "/latest.json"
    overlay = {"bundle": {"createUpdaterArtifacts": False}, "plugins": {"updater": {
        "pubkey": config["plugins"]["updater"]["pubkey"], "requireSignedVersion": True,
        "endpoints": [endpoint], "dangerousInsecureTransportProtocol": True}}}
    if record:
        if json.loads((output / "prior-test-overlay.json").read_text()) != overlay:
            raise ValueError("Fixture overlay changed since preparation")
        info = json.loads((output / f"BUILDINFO-{platform}.json").read_text())
        if info["source_sha"] != PRIOR_SHA or info["version"] != PRIOR_VERSION:
            raise ValueError("Actual fixture BUILDINFO must identify the previous stable source and version")
        release.source_material(output, root=root, platform=platform)
        receipt = json.loads((output / "FIXTURE-PREPARE.json").read_text())
        package = output / ("Aperture-Prior-0.1.1-Setup-x64.exe" if platform == "windows" else "Aperture-x86_64.AppImage")
        if not package.is_file() or not package.stat().st_size:
            raise ValueError("Actual previous-stable fixture package is missing or empty")
        if receipt["overlay_sha256"] != release.digest(output / "prior-test-overlay.json"):
            raise ValueError("Fixture overlay bytes changed since preparation")
        receipt.update(actual_buildinfo=info, files_sha256={path.relative_to(output).as_posix(): release.digest(path)
                       for path in sorted(output.rglob("*")) if path.is_file() and path.name != "FIXTURE.json"})
        (output / "FIXTURE.json").write_text(json.dumps(receipt, indent=2) + "\n")
    else:
        if output.exists():
            raise ValueError("Fixture output must be a fresh directory")
        output.mkdir(parents=True)
        (output / "prior-test-overlay.json").write_text(json.dumps(overlay, indent=2) + "\n")
        receipt = {"purpose": "Private unsigned previous-stable-code upgrade fixture; never promote",
                   "base_source_tag": PRIOR_TAG, "base_source_sha": PRIOR_SHA, "prior_version": PRIOR_VERSION,
                   "candidate_version": release.current_version(production_root), "platform": platform,
                   "test_deltas": ["Loopback updater transport and unsigned packaging overlay only; application source, version, locks, public key and signed-version checks unchanged"],
                   "application_lockfiles_sha256": plan["application_lockfiles_sha256"],
                   "helper_sha256": release.digest(Path(__file__)),
                   "helper_source_sha": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=production_root, text=True).strip(),
                   "overlay_sha256": release.digest(output / "prior-test-overlay.json")}
        if tuple(map(int, receipt["candidate_version"].split("."))) <= tuple(map(int, PRIOR_VERSION.split("."))):
            raise ValueError("Incoming candidate must be newer than the previous stable fixture")
        (output / "FIXTURE-PREPARE.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"Previous-stable fixture {'recorded' if record else 'prepared'}: {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path, help="Isolated previous-stable checkout")
    parser.add_argument("--production-root", type=Path, default=release.ROOT)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--platform", required=True, choices=("linux", "windows"))
    parser.add_argument("--record", action="store_true", help="Verify actual BUILDINFO and ordinary corresponding-source gate, then record package hashes")
    args = parser.parse_args()
    fixture(args.root.resolve(), args.output.resolve(), args.platform, args.production_root.resolve(), args.record)


if __name__ == "__main__":
    main()
