#!/usr/bin/env python3
"""Promote a reviewed GitHub candidate without uploading or rebuilding bytes."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

from release import digest, verify


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True)


def main():
    tag = os.environ["RELEASE_TAG"]
    sha = os.environ["SOURCE_SHA"]
    expected_digest = os.environ["CHECKSUMS_SHA256"]
    if os.environ.get("GITHUB_REF") != "refs/heads/main" or os.environ.get("DESKTOP_ACCEPTANCE") != "accepted":
        raise ValueError("Run trusted main workflow after desktop acceptance")
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
        verify(directory, tag[1:], sha)
        subprocess.run(["python3", str(Path(__file__).with_name("release.py")), "verify", folder, "--version", tag[1:], "--source-sha", sha, "--cryptographic"], check=True)
        # This edits release state and title only. Artifact bytes never change.
        gh("release", "edit", tag, "--repo", repo, "--prerelease=false", "--latest", "--title", f"Aperture {tag[1:]}")
        after = json.loads(gh("api", f"repos/{repo}/releases/tags/{tag}"))
        if after["prerelease"] or after["draft"] or {(a["id"], a["name"], a["size"], a.get("digest")) for a in after["assets"]} != {(a["id"], a["name"], a["size"], a.get("digest")) for a in candidate["assets"]}:
            raise ValueError("Promotion did not preserve the candidate assets")
        print(f"Promoted {tag}. Candidate assets retained without rebuilding.")


if __name__ == "__main__":
    main()
