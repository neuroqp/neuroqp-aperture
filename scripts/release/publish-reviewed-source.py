#!/usr/bin/env python3
"""Reproduce reviewed source bytes and publish only their existing source draft."""

import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

import release

REPO = "neuroqp/neuroqp-aperture"
PRIOR_SHA = "b34a2de71298e6c74980e502655dbe8ac73e0632"


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True)


def tag_commit(tag):
    ref = json.loads(gh("api", f"repos/{REPO}/git/ref/tags/{tag}"))["object"]
    while ref["type"] == "tag":
        ref = json.loads(gh("api", f"repos/{REPO}/git/tags/{ref['sha']}"))["object"]
    if ref["type"] != "commit":
        raise ValueError("Source tag must identify a commit")
    return ref["sha"]


def publication_context(source_sha, environment):
    if environment.get("GITHUB_REPOSITORY") != REPO or environment.get("GITHUB_EVENT_NAME") != "workflow_dispatch" or environment.get("PREPARE_SOURCE") != "true":
        raise ValueError("Source publication requires explicit manual source-only dispatch in the source repository")
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha) or environment.get("BUILD_UPGRADE_FIXTURE") == "true" or environment.get("TAURI_SIGNING_PRIVATE_KEY") or environment.get("TAURI_SIGNING_PRIVATE_KEY_PASSWORD"):
        raise ValueError("Source-only publication requires a full source commit and no fixture/signing context")


def source_tag(plan):
    tag = plan["url"].split("/")[-2]
    if not re.fullmatch(r"source-v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-r[1-9]\d*)?", tag):
        raise ValueError("Reviewed publication URL must identify an immutable source-only tag")
    return tag


def draft(candidate, tag):
    if candidate.get("tag_name") != tag or not candidate.get("draft") or candidate.get("published_at"):
        raise ValueError("Only the existing unpublished source draft can receive reviewed material")
    names = [asset["name"] for asset in candidate["assets"]]
    if len(set(names)) != len(names) or set(names) - {release.SOURCE_ASSET, release.SOURCE_REVIEW}:
        raise ValueError("Source draft has duplicate or unexpected assets")


def check_assets(candidate, tag, directory):
    draft(candidate, tag)
    for asset in candidate["assets"]:
        local = directory / asset["name"]
        if asset.get("state") != "uploaded" or asset["size"] != local.stat().st_size:
            raise ValueError("Existing source asset is incomplete or differs; no clobber is permitted")
        expected = release.digest(local)
        if asset["name"] == release.SOURCE_REVIEW:
            with tempfile.TemporaryDirectory(prefix="aperture-source-review-") as folder:
                gh("release", "download", tag, "--repo", REPO, "--pattern", release.SOURCE_REVIEW, "--dir", folder)
                if release.digest(Path(folder) / release.SOURCE_REVIEW) != expected:
                    raise ValueError("Existing source review JSON differs from the committed ready plan")
        elif asset.get("digest") != "sha256:" + expected:
            raise ValueError("Existing source archive differs or lacks a server digest; no clobber is permitted")


def main():
    source_sha = os.environ.get("SOURCE_SHA", "")
    publication_context(source_sha, os.environ)
    root = release.ROOT
    subprocess.run(["git", "cat-file", "-e", source_sha + "^{commit}"], cwd=root, check=True)
    subprocess.run(["git", "merge-base", "--is-ancestor", source_sha, "HEAD"], cwd=root, check=True)
    subprocess.run(["git", "diff", "--quiet", source_sha, "HEAD", "--", ".", ":(exclude)scripts/release/corresponding-source.json"], cwd=root, check=True)
    plan_path = root / "scripts/release/corresponding-source.json"
    plan = release.source_plan(plan_path)
    tag = source_tag(plan)
    if tag_commit(tag) != source_sha:
        raise ValueError("Source tag must already point to the exact reviewed application snapshot")
    if subprocess.check_output(["git", "rev-parse", "refs/tags/v0.1.1^{commit}"], cwd=root, text=True).strip() != PRIOR_SHA:
        raise ValueError("Previous stable source tag differs from the reviewed baseline")
    endpoint = f"repos/{REPO}/releases/tags/{tag}"
    draft(json.loads(gh("api", endpoint)), tag)
    work = root / "build/source-publication"
    prior_root = work / "previous-locks"
    for name in ("scripts/release/corresponding-source.json", *release.LOCKFILES):
        target = prior_root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(subprocess.check_output(["git", "show", f"{PRIOR_SHA}:{name}"], cwd=root))
    prior = work / "prior"
    release.source_material(prior, fetch=True, root=prior_root)
    spec = importlib.util.spec_from_file_location("source_refresh", root / "scripts/release/refresh-source.py")
    refresh = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(refresh)
    generated, files, removed, prepared = refresh.prepare(prior / release.SOURCE_ASSET, prior_root / "scripts/release/corresponding-source.json", root, source_sha)
    for name in ("application_source_inventory_sha256", "application_lockfiles_sha256", "native_dependencies_sha256"):
        if prepared[name] != plan[name]:
            raise ValueError(f"Requested source snapshot differs from the current ready plan: {name}")
    directory = work / "reviewed"
    directory.mkdir()
    result = refresh.write_archive(prior / release.SOURCE_ASSET, directory / release.SOURCE_ASSET, generated, files, removed, 0)
    if result["sha256"] != plan["sha256"]:
        raise ValueError("Runner archive bytes differ from the already reviewed archive; publication blocked")
    release.verify_source_material(directory, plan)
    shutil.copyfile(plan_path, directory / release.SOURCE_REVIEW)
    candidate = json.loads(gh("api", endpoint))
    if tag_commit(tag) != source_sha:
        raise ValueError("Source tag changed before publication")
    check_assets(candidate, tag, directory)
    existing = {asset["name"] for asset in candidate["assets"]}
    for name in (release.SOURCE_ASSET, release.SOURCE_REVIEW):
        if name not in existing:
            gh("release", "upload", tag, str(directory / name), "--repo", REPO)
    before = json.loads(gh("api", endpoint))
    check_assets(before, tag, directory)
    if {asset["name"] for asset in before["assets"]} != {release.SOURCE_ASSET, release.SOURCE_REVIEW}:
        raise ValueError("Verified source asset set is incomplete")
    if tag_commit(tag) != source_sha:
        raise ValueError("Source tag changed before draft publication")
    gh("release", "edit", tag, "--repo", REPO, "--draft=false", "--prerelease", "--latest=false")
    after = json.loads(gh("api", endpoint))
    identities = lambda value: {(asset["id"], asset["name"], asset["size"], asset.get("digest")) for asset in value["assets"]}
    if after["draft"] or not after["prerelease"] or identities(after) != identities(before):
        raise ValueError("Source publication did not preserve verified assets as a prerelease")
    if json.loads(gh("api", f"repos/{REPO}/releases/latest"))["tag_name"] == tag:
        raise ValueError("Source material must remain outside latest")
    print(f"Published exact reviewed source bytes under {tag}: {result['sha256']}")


if __name__ == "__main__":
    main()
