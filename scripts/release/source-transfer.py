#!/usr/bin/env python3
"""Reconstruct reviewed source bytes in CI and upload them to an existing draft."""

import argparse
import json
import os
import re
import subprocess
from pathlib import Path

import release

REPOSITORY = "neuroqp/neuroqp-aperture"


def command(arguments, timeout=60):
    return subprocess.check_output(arguments, text=True, timeout=timeout)


def validate_manifest(path, trusted_sha256, root):
    if not re.fullmatch(r"[0-9a-f]{64}", trusted_sha256) or release.digest(path) != trusted_sha256:
        raise ValueError("Transport manifest differs from its reviewed SHA256")
    record = json.loads(path.read_bytes())
    baseline = release.source_plan(root / release.SOURCE_PUBLICATION)
    if record.get("schema") != 1 or record.get("patch_asset") != "source-transfer.zst":
        raise ValueError("Unsupported source transport")
    if not re.fullmatch(r"[0-9a-f]{40}", record.get("source_sha", "")):
        raise ValueError("Transport requires a full frozen source SHA")
    if not re.fullmatch(r"source-[A-Za-z0-9._-]+", record.get("source_release_tag", "")):
        raise ValueError("Transport requires a source-only release tag")
    for key in ("patch_sha256", "target_sha256", "source_review_sha256"):
        if not re.fullmatch(r"[0-9a-f]{64}", record.get(key, "")):
            raise ValueError("Transport requires exact SHA256 identities")
    for key in ("patch_bytes", "target_bytes"):
        if type(record.get(key)) is not int or record[key] <= 0:
            raise ValueError("Transport requires positive exact byte sizes")
    if record.get("baseline_url") != baseline["url"] or record.get("baseline_sha256") != baseline["sha256"]:
        raise ValueError("Transport baseline differs from the tracked public source")
    return record


def draft_release(tag):
    pages = json.loads(command(["gh", "api", "--paginate", "--slurp", f"repos/{REPOSITORY}/releases?per_page=100"]))
    matches = [record for page in pages for record in page if record.get("tag_name") == tag]
    if len(matches) != 1:
        raise ValueError("Source destination must match exactly one release")
    record = matches[0]
    if record.get("draft") is not True:
        raise ValueError("Source destination must remain an existing draft")
    if any(asset["name"] == release.SOURCE_ASSET for asset in record["assets"]):
        raise ValueError("Source target asset already exists; never overwrite it")
    return record


def checked_file(path, sha256, size=None):
    if size is not None and path.stat().st_size != size:
        raise ValueError(f"Source transport size differs: {path.name}")
    if release.digest(path) != sha256:
        raise ValueError(f"Source transport SHA256 differs: {path.name}")


def reconstruct(baseline, patch, archive):
    command(["zstd", "--decompress", "--long=31", "--patch-from=" + str(baseline),
             str(patch), "-o", str(archive)])


def transfer(root, manifest, directory, receipt):
    source_sha, tag = manifest["source_sha"], manifest["source_release_tag"]
    command(["git", "merge-base", "--is-ancestor", source_sha, "origin/main"])
    if command(["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"]).strip() != source_sha:
        raise ValueError("Source release tag differs from the frozen source commit")
    destination = draft_release(tag)
    receipt["destination_release_id"] = destination["id"]
    directory.mkdir(parents=True, exist_ok=False)
    command(["gh", "release", "download", tag, "--repo", REPOSITORY,
             "--pattern", manifest["patch_asset"], "--pattern", release.SOURCE_REVIEW,
             "--dir", str(directory)])
    patch = directory / manifest["patch_asset"]
    checked_file(patch, manifest["patch_sha256"], manifest["patch_bytes"])
    review = release.source_plan(directory / release.SOURCE_REVIEW, manifest["source_review_sha256"])
    expected_url = f"https://github.com/{REPOSITORY}/releases/download/{tag}/{release.SOURCE_ASSET}"
    if review.get("application_source_sha") != source_sha or review["sha256"] != manifest["target_sha256"] or review["url"] != expected_url:
        raise ValueError("Reviewed source plan differs from the pinned transport target")
    baseline = directory / "baseline.tar.xz"
    command(["curl", "--fail", "--location", "--max-time", "120", "--output", str(baseline),
             manifest["baseline_url"]], timeout=125)
    checked_file(baseline, manifest["baseline_sha256"])
    archive = directory / release.SOURCE_ASSET
    reconstruct(baseline, patch, archive)
    checked_file(archive, manifest["target_sha256"], manifest["target_bytes"])
    # This full gate checks archived locks against the plan and exact frozen Git
    # tree, rather than comparing them with the newer orchestration checkout.
    release.verify_source_material(directory, review, root=root, source_sha=source_sha)
    receipt["full_source_verification"] = "passed"
    if draft_release(tag)["id"] != destination["id"]:
        raise ValueError("Source draft identity changed before upload")
    command(["gh", "release", "upload", tag, str(archive), "--repo", REPOSITORY], timeout=180)
    uploaded = json.loads(command(["gh", "api", f"repos/{REPOSITORY}/releases/{destination['id']}"]))
    assets = [asset for asset in uploaded["assets"] if asset["name"] == release.SOURCE_ASSET]
    if uploaded.get("draft") is not True or uploaded.get("tag_name") != tag or len(assets) != 1:
        raise ValueError("Uploaded source must remain the unique asset in the original draft")
    asset = assets[0]
    if asset.get("size") != manifest["target_bytes"] or asset.get("digest") != "sha256:" + manifest["target_sha256"] or asset.get("state") != "uploaded":
        raise ValueError("GitHub source asset size/digest differs from the verified archive")
    receipt.update(status="passed", uploaded_asset_id=asset["id"], github_digest=asset["digest"],
                   target_bytes=asset["size"], source_published=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--output", type=Path, default=Path("build/source-transport"))
    args = parser.parse_args()
    root = release.ROOT
    receipt_path = root / "build/source-transport-receipt.json"
    receipt_path.parent.mkdir(parents=True, exist_ok=True)
    receipt = {"status": "failed", "transport_manifest_sha256": args.sha256,
               "orchestration_sha": command(["git", "rev-parse", "HEAD"]).strip()}
    try:
        if os.environ.get("GITHUB_REPOSITORY") != REPOSITORY or os.environ.get("GITHUB_REF") != "refs/heads/main":
            raise ValueError("Source transport runs only on repository main")
        manifest = validate_manifest(args.manifest, args.sha256, root)
        receipt["transport"] = manifest
        transfer(root, manifest, args.output, receipt)
    except Exception as error:
        receipt["error"] = str(error)
        raise
    finally:
        receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
