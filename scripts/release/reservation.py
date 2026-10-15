#!/usr/bin/env python3
"""Claim a release once, permitting only proven pre-build identity failures."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

REPOSITORY = "neuroqp/neuroqp-aperture"
CLAIM = "Package build claim"


def api(endpoint, method="GET", body=None, pages=False):
    command = ["gh", "api", endpoint, "--method", method]
    if pages:
        command += ["--paginate", "--slurp"]
    if body is not None:
        command += ["--input", "-"]
    return json.loads(subprocess.check_output(command, input=None if body is None else json.dumps(body), text=True, timeout=30))


def destination(record, tag, source_sha):
    if record.get("tag_name") != tag or record.get("target_commitish") != source_sha or record.get("draft") is not True or record.get("prerelease") is not True or record.get("assets") != []:
        raise ValueError("Reservation must be the exact frozen tag/source, draft prerelease, with zero assets")


def failed_identity_run(run_id):
    run = api(f"repos/{REPOSITORY}/actions/runs/{run_id}")
    if run.get("id") != run_id or run.get("repository", {}).get("full_name") != REPOSITORY or run.get("path") != ".github/workflows/release.yml" or run.get("event") != "workflow_dispatch" or run.get("head_branch") != "main" or run.get("run_attempt") != 1 or run.get("status") != "completed" or run.get("conclusion") != "failure":
        raise ValueError("Reservation origin must be a trusted first-attempt Stage B failure on main")
    pages = api(f"repos/{REPOSITORY}/actions/runs/{run_id}/attempts/1/jobs?per_page=100", pages=True)
    jobs = [job for page in pages for job in page["jobs"]]
    expected = {"identity": "failure", "source": "skipped", "packages": "skipped", "candidate": "skipped"}
    if len(jobs) != len(expected) or {job["name"]: job.get("conclusion") for job in jobs} != expected or any(job.get("status") != "completed" for job in jobs):
        raise ValueError("Only identity may have run; every other Stage B job must be explicitly skipped")
    return {"run": run, "jobs": jobs}


def unclaimed_origin(record, tag, source_sha, recovery=None):
    body = record.get("body") or ""
    claimed = CLAIM.lower() in body.lower()
    if claimed:
        if not recovery or recovery.get("schema") != 1 or (recovery.get("tag"), recovery.get("source_sha"), recovery.get("reservation_id"), recovery.get("alias"), recovery.get("body_sha256")) != (tag, source_sha, record.get("id"), record.get("tag_name"), hashlib.sha256(body.encode()).hexdigest()):
            raise ValueError("Unreviewed package claim or alias prevents reservation reuse")
        destination({**record, "tag_name": tag}, tag, source_sha)
    else:
        destination(record, tag, source_sha)
    origins = re.findall(r"Stage B run: ([1-9][0-9]*)\.", body)
    if len(origins) != 1:
        raise ValueError("Reservation requires one retained Stage B origin run")
    run_id = int(origins[0])
    origin = failed_identity_run(run_id)
    if claimed:
        claims = re.findall(r"Package build claim: Stage B run ([1-9][0-9]*); orchestration ([0-9a-f]{40})\.", body)
        if len(claims) != 1 or body.lower().count(CLAIM.lower()) != 1 or run_id != recovery["origin_run_id"] or (int(claims[0][0]), claims[0][1]) != (recovery["claim_run_id"], recovery["claim_orchestration_sha"]):
            raise ValueError("Recovery must identify the exact original and sole claimed run")
        origin["claim_origin"] = failed_identity_run(recovery["claim_run_id"])
        if origin["claim_origin"]["run"].get("head_sha") != recovery["claim_orchestration_sha"]:
            raise ValueError("Claim orchestration differs from its actual run")
    return {**origin, "original_body": body}


def write_reservation(tag, source_sha, body, existing_id=None):
    fields = {"tag_name": tag, "target_commitish": source_sha, "name": f"Aperture {tag[1:]} build reservation",
              "body": body, "draft": True, "prerelease": True, "make_latest": "false"}
    endpoint = f"repos/{REPOSITORY}/releases"
    record = api(endpoint if existing_id is None else f"{endpoint}/{existing_id}", "POST" if existing_id is None else "PATCH", fields)
    if existing_id is not None:
        if record.get("id") != existing_id:
            raise ValueError("Reservation identity changed")
        record = api(f"{endpoint}/{existing_id}")
    destination(record, tag, source_sha)
    if type(record.get("id")) is not int or record["id"] <= 0 or record.get("body") != body:
        raise ValueError("GitHub did not retain the exact reservation and durable package build claim")
    return record


def reserve(tag, source_sha, check_only=False, recovery=None):
    resolved = subprocess.check_output(["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"], text=True).strip()
    if resolved != source_sha:
        raise ValueError("Application tag differs from the frozen source")
    pages = api(f"repos/{REPOSITORY}/releases?per_page=100", pages=True)
    matches = [record for page in pages for record in page if record.get("tag_name") == tag or
               (recovery and recovery.get("tag") == tag and recovery.get("source_sha") == source_sha and record.get("id") == recovery.get("reservation_id"))]
    if recovery and recovery.get("tag") == tag and recovery.get("source_sha") == source_sha and (len(matches) != 1 or matches[0].get("id") != recovery.get("reservation_id")):
        raise ValueError("Reviewed recovery requires its exact existing reservation; never create a replacement")
    if len(matches) > 1:
        raise ValueError("Ambiguous existing reservation")
    existing = matches[0] if matches else None
    audit = {"tag": tag, "source_sha": source_sha, "check_only": check_only}
    if existing:
        # Re-read the exact ID before evaluating and mutating its retained body.
        existing = api(f"repos/{REPOSITORY}/releases/{existing['id']}")
        audit.update(reservation_id=existing["id"], origin=unclaimed_origin(existing, tag, source_sha, recovery))
    if check_only:
        return {**audit, "status": "eligible", "mutated": False}
    if os.environ.get("GITHUB_REPOSITORY") != REPOSITORY or os.environ.get("GITHUB_REF") != "refs/heads/main" or os.environ.get("GITHUB_RUN_ATTEMPT") != "1":
        raise ValueError("Only a first-attempt Stage B run on repository main may claim packages")
    run_id, workflow_sha = os.environ["GITHUB_RUN_ID"], os.environ["GITHUB_SHA"]
    if not run_id.isdecimal() or not re.fullmatch(r"[0-9a-f]{40}", workflow_sha):
        raise ValueError("Current workflow requires exact run and orchestration identities")
    frozen = f"Frozen attempt: {os.environ['FROZEN_RUN_ID']}/{os.environ['FROZEN_SHA256']}."
    claim = f"{CLAIM}: Stage B run {run_id}; orchestration {workflow_sha}. {frozen}"
    if existing:
        body = audit["origin"]["original_body"] + "\n\n" + claim
    else:
        body = f"{frozen} Stage B run: {run_id}.\n\n{claim}"
    record = write_reservation(tag, source_sha, body, existing["id"] if existing else None)
    return {**audit, "status": "claimed", "reservation_id": record["id"], "claim": claim, "orchestration_sha": workflow_sha}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--check-only", action="store_true")
    parser.add_argument("--recovery-record", type=Path)
    parser.add_argument("--receipt", type=Path, default=Path("build/evidence/reservation.json"))
    args = parser.parse_args()
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", args.tag) or not re.fullmatch(r"[0-9a-f]{40}", args.source_sha):
        parser.error("Exact application version tag and frozen source SHA required")
    receipt = {"status": "failed", "tag": args.tag, "source_sha": args.source_sha}
    try:
        recovery = json.loads(args.recovery_record.read_bytes()) if args.recovery_record else None
        receipt = reserve(args.tag, args.source_sha, args.check_only, recovery)
        print(json.dumps(receipt) if args.check_only else receipt["reservation_id"])
    except Exception as error:
        receipt["error"] = str(error)
        raise
    finally:
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
