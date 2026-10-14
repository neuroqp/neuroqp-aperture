#!/usr/bin/env python3
"""Promote a reviewed GitHub candidate without uploading or rebuilding bytes."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import sys
from urllib.request import urlopen

from bounded import timestamp, parsed, write_report

from release import digest, source_plan
from bounded import identity, load_identity, read, same_identity, trusted_run, validate_acceptance, validate_deadline, deadline_remaining


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True, timeout=min(60, deadline_remaining(FROZEN)))


def main():
    tag = os.environ["RELEASE_TAG"]
    sha = os.environ["SOURCE_SHA"]
    global FROZEN
    FROZEN = load_identity(os.environ["FROZEN_IDENTITY"])
    REPORT.update({key: FROZEN[key] for key in ("source_sha", "build_inputs_sha256", "checksums_sha256", "freeze_started_at", "deadline_at")})
    REPORT["asset_sha256"] = {name: row["sha256"] for name, row in FROZEN["assets"].items()}
    same_identity(FROZEN, identity())
    acceptance = read(os.environ["DESKTOP_REPORT"])
    validate_acceptance(acceptance, FROZEN)
    expected_digest = FROZEN["checksums_sha256"]
    if os.environ.get("GITHUB_REF") != "refs/heads/main":
        raise ValueError("Run trusted main workflow after fixed native desktop smoke")
    trusted_run(os.environ["CANDIDATE_RUN_ID"], "release.yml")
    trusted_run(os.environ["SMOKE_RUN_ID"], "release-evidence.yml")
    if acceptance.get("candidate_run_id") != os.environ["CANDIDATE_RUN_ID"]:
        raise ValueError("Desktop report belongs to another frozen attempt")
    if not re.fullmatch(r"v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", tag) or not re.fullmatch(r"[0-9a-f]{40}", sha) or not re.fullmatch(r"[0-9a-f]{64}", expected_digest):
        raise ValueError("Invalid release identity")
    repo = "neuroqp/neuroqp-aperture"
    candidate = json.loads(gh("api", f"repos/{repo}/releases/tags/{tag}"))
    if not candidate["prerelease"] or candidate["draft"]:
        raise ValueError("Only a published candidate prerelease can be promoted")
    ref = json.loads(gh("api", f"repos/{repo}/git/ref/tags/{tag}"))["object"]
    while ref["type"] == "tag":
        ref = json.loads(gh("api", f"repos/{repo}/git/tags/{ref['sha']}"))["object"]
    if ref["type"] != "commit" or ref["sha"] != sha:
        raise ValueError("Tag no longer points to the reviewed commit")
    comparison = json.loads(gh("api", f"repos/{repo}/compare/{sha}...main"))
    if comparison["status"] not in ("ahead", "identical"):
        raise ValueError("Release commit must be contained in main")
    releases = json.loads(gh("api", f"repos/{repo}/releases?per_page=100"))
    stable = [r for r in releases if not r["prerelease"] and not r["draft"]]
    numbers = lambda value: tuple(map(int, value.removeprefix("v").split(".")))
    if any(re.fullmatch(r"v\d+\.\d+\.\d+", r["tag_name"]) and numbers(r["tag_name"]) >= numbers(tag) for r in stable):
        raise ValueError("Stable release versions must increase")
    with tempfile.TemporaryDirectory(prefix="aperture-promotion-") as folder:
        directory = Path(folder)
        gh("release", "download", tag, "--repo", repo, "--dir", folder)
        if digest(directory / "SHA256SUMS") != expected_digest:
            raise ValueError("Candidate checksums differ from the reviewed set")
        source_plan(directory / "Aperture-Corresponding-Source.json", FROZEN["source_review_sha256"])
        subprocess.run(["python3", str(Path(__file__).with_name("release.py")), "verify", folder, "--version", tag[1:], "--source-sha", sha, "--cryptographic"], check=True, timeout=min(60, deadline_remaining(FROZEN)))
        validate_deadline(FROZEN)
        # Leave time for state confirmation; elapsed Stage A/source prep is never reset.
        if deadline_remaining(FROZEN) < 30:
            raise ValueError("Insufficient original deadline remaining to promote and confirm")
        # This edits release state and title only. Artifact bytes never change.
        REPORT["publication_requested"] = True
        REPORT["published"] = None  # A lost response cannot establish that mutation failed.
        gh("release", "edit", tag, "--repo", repo, "--prerelease=false", "--latest", "--title", f"Aperture {tag[1:]}")
        REPORT["published"] = True
        after = json.loads(gh("api", f"repos/{repo}/releases/tags/{tag}"))
        if after["prerelease"] or after["draft"] or {(a["id"], a["name"], a["size"], a.get("digest")) for a in after["assets"]} != {(a["id"], a["name"], a["size"], a.get("digest")) for a in candidate["assets"]}:
            raise ValueError("Promotion did not preserve the candidate assets")
        latest = json.loads(gh("api", f"repos/{repo}/releases/latest"))
        if latest["id"] != candidate["id"] or latest["tag_name"] != tag:
            raise ValueError("Public latest release did not advance to this candidate")
        # Download only small public identity/feed metadata after state promotion.
        # The three applications were hashed above and retain their immutable asset IDs.
        expected_feed = (directory / "latest.json").read_bytes()
        with urlopen(f"https://github.com/{repo}/releases/download/{tag}/latest.json", timeout=min(10, deadline_remaining(FROZEN))) as public:
            public_feed = public.read(2 * 1024 * 1024)
        if public_feed != expected_feed:
            raise ValueError("Public updater feed bytes differ from verified candidate")
        REPORT["public_asset_ids"] = {a["name"]: a["id"] for a in after["assets"]}
        REPORT["public_feed_sha256"] = digest(directory / "latest.json")
        validate_deadline(FROZEN)
        print(f"Promoted {tag} within the frozen 30-minute attempt. Candidate assets retained without rebuilding.")


if __name__ == "__main__":
    REPORT = {"schema": 1, "stage": "C", "status": "fail", "forced_kill": False, "published": False, "publication_requested": False, "started_at": timestamp()}
    try:
        main()
        REPORT["status"] = "pass"
    except Exception as error:
        REPORT["error"] = str(error)
        if isinstance(error, subprocess.TimeoutExpired):
            REPORT["forced_kill"] = True
        if REPORT["publication_requested"]:
            REPORT["status"] = "inconclusive"
        raise
    finally:
        REPORT["finished_at"] = timestamp()
        REPORT["elapsed_seconds"] = (parsed(REPORT["finished_at"]) - parsed(REPORT["started_at"])).total_seconds()
        write_report(Path("build/evidence/promotion-report.json"), REPORT)
