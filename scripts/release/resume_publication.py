#!/usr/bin/env python3
"""Complete the one reviewed interrupted v0.2.0 publication without rebuilding."""

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

from bounded import (ATTEMPT_SECONDS, ENVIRONMENTS, ROOT, candidate_identity, deadline_remaining,
                     identity, parsed, read, same_identity, timestamp, trusted_run,
                     validate_deadline, validate_readiness, write_report)
from release import ASSETS, SOURCE_ASSET, SOURCE_REVIEW, digest, source_plan

REPOSITORY = "neuroqp/neuroqp-aperture"
RECORD = Path(__file__).with_name("resume-publication.json")
FROZEN = None


def gh(*args, seconds=120):
    limit = seconds if FROZEN is None else min(seconds, deadline_remaining(FROZEN))
    return subprocess.check_output(["gh", *args], text=True, timeout=limit)


def api(endpoint):
    return json.loads(gh("api", f"repos/{REPOSITORY}/{endpoint}"))


def asset_rows(assets):
    rows = [{key: asset.get(key) for key in ("id", "name", "size", "digest")} for asset in assets]
    if len({row["name"] for row in rows}) != len(rows):
        raise ValueError("Duplicate release assets")
    return sorted(rows, key=lambda row: row["name"])


def validate_draft(draft, record, added=None, published=False):
    if (draft.get("id") != record["release_id"] or draft.get("tag_name") != record["tag"]
            or draft.get("target_commitish") != record["source_sha"]
            or draft.get("draft") is not (not published) or draft.get("prerelease") is not True
            or not published and draft.get("published_at") is not None):
        raise ValueError("Existing draft release identity or state changed")
    expected = record["assets"] + ([] if added is None else [added])
    if asset_rows(draft.get("assets", [])) != asset_rows(expected):
        raise ValueError("Existing asset IDs, sizes, digests or required asset set changed")
    if hashlib.sha256(draft.get("body", "").encode()).hexdigest() != record["body_sha256"]:
        raise ValueError("Original durable package build claim changed")


def validate_prior(run, jobs, record):
    if (run.get("id") != record["prior_run_id"] or run.get("repository", {}).get("full_name") != REPOSITORY
            or run.get("path") != ".github/workflows/release.yml" or run.get("event") != "workflow_dispatch"
            or run.get("head_branch") != "main" or run.get("head_sha") != record["prior_orchestration_sha"]
            or run.get("run_attempt") != 1 or run.get("status") != "completed"
            or run.get("conclusion") != record["prior_conclusion"]):
        raise ValueError("Prior run must be the exact completed cancelled Stage B attempt")
    expected = {"identity": "success", "source": "success", "packages / linux": "success", "packages / windows": "success", "candidate": "cancelled"}
    if len(jobs) != len(expected) or {job.get("name"): job.get("conclusion") for job in jobs} != expected or any(job.get("status") != "completed" for job in jobs):
        raise ValueError("Prior source and both package jobs must have succeeded")
    candidate = next(job for job in jobs if job["name"] == "candidate")
    steps = {step.get("name"): step.get("conclusion") for step in candidate.get("steps", [])}
    if steps.get("Assemble and verify candidate") != "success" or steps.get("Publish candidate outside latest") != "cancelled":
        raise ValueError("Prior verification must pass before interrupted publication")


def validate_lineage(candidate, run):
    record = read(RECORD)
    recovery = candidate.get("recovery", {})
    expected = {key: record[key] for key in ("prior_run_id", "prior_orchestration_sha", "prior_conclusion", "prior_frozen_sha256", "prior_candidate_sha256", "prior_freeze_started_at", "prior_deadline_at", "release_id")}
    expected.update(record_sha256=digest(RECORD), completion_run_id=str(run.get("id")), orchestration_sha=run.get("head_sha"))
    if recovery != expected or candidate.get("source_sha") != record["source_sha"] or candidate.get("build_inputs_sha256") != record["build_inputs_sha256"]:
        raise ValueError("Reviewed completion lineage differs from exact interrupted attempt")
    if candidate.get("checksums_sha256") != record["checksums_sha256"] or candidate.get("source_review_sha256") != record["source_review_sha256"]:
        raise ValueError("Completion changed historical candidate metadata")
    if (parsed(candidate["deadline_at"]) - parsed(candidate["freeze_started_at"])).total_seconds() != ATTEMPT_SECONDS or parsed(candidate["freeze_started_at"]) <= parsed(record["prior_deadline_at"]):
        raise ValueError("Completion requires its own new 60-minute deadline")
    retained = candidate.get("release_assets", [])
    source = [row for row in retained if row.get("name") == SOURCE_ASSET]
    if (len(source) != 1 or type(source[0].get("id")) is not int or source[0]["id"] <= 0
            or source[0].get("digest") != "sha256:" + record["source_archive_sha256"]
            or type(source[0].get("size")) is not int or source[0]["size"] <= 0
            or asset_rows(retained) != asset_rows(record["assets"] + source)):
        raise ValueError("Completion must retain every original asset ID and exact added source")
    pinned = {row["name"]: row["digest"].removeprefix("sha256:") for row in record["assets"]}
    assets = candidate.get("assets", {})
    if set(assets) != set(ENVIRONMENTS):
        raise ValueError("Completion requires the same six frozen package environments")
    for environment, row in assets.items():
        name = ASSETS["windows-x86_64" if environment == "windows11-setup" else "windows-x86_64-portable" if environment == "windows11-portable" else "linux-x86_64"]
        if row.get("name") != name or row.get("sha256") != pinned.get(name):
            raise ValueError("Completion changed a frozen package")


def preflight(inputs, record):
    old_path, candidate_path = inputs / "prior/frozen-attempt.json", inputs / "prior/frozen-candidate.json"
    if digest(old_path) != record["prior_frozen_sha256"] or digest(candidate_path) != record["prior_candidate_sha256"]:
        raise ValueError("Historical frozen evidence bytes changed")
    old, candidate = read(old_path), read(candidate_path)
    same_identity(old, identity())
    same_identity(candidate, old)
    if any(old.get(key) != candidate.get(key) or old.get(key) != record[f"prior_{key}"] for key in ("freeze_started_at", "deadline_at")):
        raise ValueError("Historical frozen deadline changed")
    if old.get("packaging_run_id") != record["packaging_run_id"]:
        raise ValueError("Historical verifier preparation changed")
    readiness = read(inputs / "readiness/release-readiness.json")
    trusted_run(os.environ["READINESS_RUN_ID"], "release-evidence.yml")
    validate_readiness(readiness, old)
    if parsed(readiness["completed_at"]) <= parsed(old["readiness"]["completed_at"]):
        raise ValueError("Completion needs refreshed Stage A readiness")
    for row in readiness["automated"]:
        if row["binary_sha256"] != old["validated_inputs"][row["platform"]]["binary_sha256"]:
            raise ValueError("Refreshed readiness changed the packaged executable identity")
    run = api(f"actions/runs/{record['prior_run_id']}")
    jobs = api(f"actions/runs/{record['prior_run_id']}/attempts/1/jobs?per_page=100")["jobs"]
    validate_prior(run, jobs, record)
    draft = api(f"releases/{record['release_id']}")
    validate_draft(draft, record)
    subprocess.run(["git", "merge-base", "--is-ancestor", record["source_sha"], "origin/main"], cwd=ROOT, check=True, timeout=30)
    tag = subprocess.check_output(["git", "rev-parse", f"refs/tags/{record['tag']}^{{commit}}"], cwd=ROOT, text=True, timeout=30).strip()
    remote = api(f"git/ref/tags/{record['tag']}")["object"]
    while remote["type"] == "tag":
        remote = api(f"git/tags/{remote['sha']}")["object"]
    if tag != record["source_sha"] or (remote.get("type") != "commit" or remote.get("sha") != record["source_sha"]):
        raise ValueError("Immutable application tag changed")
    return old, candidate, readiness


def complete(inputs, output, record):
    global FROZEN
    if (os.environ.get("GITHUB_REPOSITORY") != REPOSITORY or os.environ.get("GITHUB_REF") != "refs/heads/main"
            or os.environ.get("GITHUB_RUN_ATTEMPT") != "1"):
        raise ValueError("Only a first-attempt reviewed main completion workflow may publish")
    current = api(f"actions/runs/{os.environ['GITHUB_RUN_ID']}")
    if (current.get("id") != int(os.environ["GITHUB_RUN_ID"]) or current.get("head_sha") != os.environ["GITHUB_SHA"] or current.get("head_branch") != "main"
            or current.get("path") != ".github/workflows/resume-release.yml" or current.get("event") != "workflow_dispatch"
            or current.get("repository", {}).get("full_name") != REPOSITORY or current.get("run_attempt") != 1):
        raise ValueError("Current completion workflow identity differs")
    old, historical, readiness = preflight(inputs, record)
    start = timestamp()
    recovery = {key: record[key] for key in ("prior_run_id", "prior_orchestration_sha", "prior_conclusion", "prior_frozen_sha256", "prior_candidate_sha256", "prior_freeze_started_at", "prior_deadline_at", "release_id")}
    recovery.update(record_sha256=digest(RECORD), completion_run_id=os.environ["GITHUB_RUN_ID"], orchestration_sha=os.environ["GITHUB_SHA"])
    FROZEN = {**old, "readiness": readiness, "readiness_run_id": os.environ["READINESS_RUN_ID"], "freeze_started_at": start,
              "deadline_at": (parsed(start) + dt.timedelta(seconds=ATTEMPT_SECONDS)).isoformat().replace("+00:00", "Z"), "recovery": recovery}
    write_report(output / "frozen-attempt.json", FROZEN)
    directory = output / "release"
    directory.mkdir(parents=True, exist_ok=False)
    gh("release", "download", record["tag"], "--repo", REPOSITORY, "--dir", str(directory), seconds=600)
    for row in record["assets"]:
        path = directory / row["name"]
        if path.stat().st_size != row["size"] or "sha256:" + digest(path) != row["digest"]:
            raise ValueError("Downloaded existing asset changed")
    plan = source_plan(directory / SOURCE_REVIEW, record["source_review_sha256"])
    if plan["sha256"] != record["source_archive_sha256"] or plan["url"] != f"https://github.com/{REPOSITORY}/releases/download/{record['source_release_tag']}/{SOURCE_ASSET}":
        raise ValueError("Historical corresponding source identity changed")
    gh("release", "download", record["source_release_tag"], "--repo", REPOSITORY, "--pattern", SOURCE_ASSET, "--dir", str(directory), seconds=600)
    if digest(directory / SOURCE_ASSET) != record["source_archive_sha256"]:
        raise ValueError("Corresponding source archive changed")
    subprocess.run([sys.executable, str(Path(__file__).with_name("release.py")), "verify", str(directory), "--version", record["tag"][1:], "--source-sha", record["source_sha"], "--cryptographic"], check=True, timeout=min(300, deadline_remaining(FROZEN)))
    candidate = candidate_identity(FROZEN, directory)
    if candidate["checksums_sha256"] != historical["checksums_sha256"] or candidate["source_review_sha256"] != historical["source_review_sha256"]:
        raise ValueError("Candidate differs from historical verified bytes")
    # Re-read immediately before the only upload. No overwrite or delete option exists.
    validate_draft(api(f"releases/{record['release_id']}"), record)
    gh("release", "upload", record["tag"], str(directory / SOURCE_ASSET), "--repo", REPOSITORY, seconds=600)
    uploaded = api(f"releases/{record['release_id']}")
    rows = [row for row in uploaded["assets"] if row["name"] == SOURCE_ASSET]
    if len(rows) != 1 or rows[0].get("digest") != "sha256:" + record["source_archive_sha256"] or rows[0]["size"] != (directory / SOURCE_ASSET).stat().st_size:
        raise ValueError("Uploaded source archive identity differs")
    added = asset_rows(rows)[0]
    validate_draft(uploaded, record, added)
    candidate["release_assets"] = asset_rows(uploaded["assets"])
    validate_lineage(candidate, current)
    validate_deadline(FROZEN)
    gh("release", "edit", record["tag"], "--repo", REPOSITORY, "--draft=false", "--prerelease", "--latest=false", "--title", f"Aperture {record['tag'][1:]} candidate")
    validate_draft(api(f"releases/{record['release_id']}"), record, added, published=True)
    validate_deadline(FROZEN)
    write_report(output / "frozen-candidate.json", candidate)
    return {"status": "pass", "forced_kill": False, "recovery": recovery, "uploaded_asset": added, "checksums_sha256": candidate["checksums_sha256"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "complete"))
    parser.add_argument("inputs", type=Path)
    parser.add_argument("--output", type=Path, default=Path("build/evidence"))
    args = parser.parse_args()
    report = {"status": "fail", "forced_kill": False, "started_at": timestamp()}
    try:
        record = read(RECORD)
        report.update(source_sha=record["source_sha"], prior_run_id=record["prior_run_id"], release_id=record["release_id"], record_sha256=digest(RECORD))
        if args.command == "check":
            preflight(args.inputs, record)
            report["status"] = "pass"
        else:
            report.update(complete(args.inputs, args.output, record))
    except Exception as error:
        report.update(error=str(error), forced_kill=isinstance(error, subprocess.TimeoutExpired))
        raise
    finally:
        if FROZEN is not None:
            report.update({key: FROZEN[key] for key in ("source_sha", "freeze_started_at", "deadline_at", "recovery")})
        report["finished_at"] = timestamp()
        write_report(args.output / "completion-report.json", report)


if __name__ == "__main__":
    main()
