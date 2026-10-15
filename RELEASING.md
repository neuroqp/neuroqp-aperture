# Building and releasing Aperture

Full application validation happens **before final OS packages are built**. A routine release has three stages. After packaging, run only the fixed package smoke checks below, then publish the same verified bytes. Lab preparation and failure repair are development work.

## Execution contract and readiness

A deployment request first checks existing readiness evidence. Missing tooling, source coverage, validated inputs or prepared labs blocks packaging. Continue prerequisite development when the user has authorized that scope; a blocked packaging step does not cancel already-authorized development. Routine deployment alone does not authorize infrastructure implementation, VM provisioning or pipeline repair. Development and validation can iterate in scoped Stage A work before packaging; an agent must not repeatedly rename release failures as preparation and restart execution without a new instruction.

One agent owns execution. All subagents receive the root `AGENTS.md` release contract, a bounded task and explicit ownership; they must not independently launch builds or VMs. Readiness must derive from successful CI and native adapter results, not an agent-written claim that checks passed. Before dispatch, verify that the executable workflow actually supports the contract below. Documentation alone does not establish readiness.

Target about 30 minutes from validated source to publication. Freeze starts one **60-minute hard deadline** covering source preparation, packaging, transfer, smoke and publication. The target is not a timeout. Record actual timings. A required Stage B/C failure or timeout stops the attempt and produces a compact report tied to the source and available artifact hashes. Preserve the original clock and evidence. Do not automatically make repair PRs, rebuild, create another frozen attempt or redispatch. A new user instruction may authorize prerequisite repair and another attempt after readiness passes. The only retry within execution is the unchanged-byte transient smoke retry specified in Stage C.

Use these generous limits. They are ceilings, not expected durations. Short API and desktop observation timeouts remain appropriate; avoid one-to-three-minute wrappers around large artifact transfers. Job caps and the overall deadline still apply if several individual steps run slowly.

| Operation                               | Hard limit      |
| --------------------------------------- | --------------- |
| Entire attempt                          | 60 minutes      |
| Windows packaging job                   | 45 minutes      |
| Windows Setup / portable packaging      | 15 minutes each |
| Linux packaging job / AppImage wrapping | 30 / 10 minutes |
| Corresponding-source job / transfer     | 15 / 10 minutes |
| Candidate publication job / upload      | 20 / 10 minutes |
| Promotion job / full release download   | 15 / 10 minutes |
| Evidence submission                     | 10 minutes      |

Windows x64 Setup and portable, and Linux x86_64 AppImage remain supported. The desktop matrix is Windows 11 standard user, Debian 12, Ubuntu 24, Arch/Omarchy and Fedora with native Wayland coverage. Windows 10 is unverified; publisher signing is pending. macOS, ARM, stores and distro packages remain deferred.

## Stage A: development and functional validation

Finish application changes and valid review fixes on a feature branch. Follow `AGENTS.md` and `SPECS.md`. Choose an increasing `MAJOR.MINOR.PATCH` version and keep the changelog short. During `0.x`, features use a minor bump and fixes use a patch bump. An authorized merge is separate from running a release; never push directly to `main`.

Run **Stage A development validation** for the source that will be frozen. It compiles executables with the final target, release profile, frontend assets and application feature flags, and runs frontend, Rust, release-helper and worker regressions on Linux and Windows. Compilation is allowed here; final installers, portable executables and AppImages are not produced. Use those executables with the prepared native runtimes for functional desktop validation. Retain their exact bytes and hashes for packaging. They remain development editions without installed or AppImage identity, and do not auto-update. Stage B must package those validated executables instead of compiling them again; a compilation cache alone is not executable identity.

Complete the [Stage A functional checklist](scripts/release/RELEASE-ACCEPTANCE.md). Test changed behavior on affected operating systems, including saving, annotations, independent pixel preservation, cancellation and multi-instance coordination. Test installer/updater behavior and rejection paths here when those components change. Detailed GUI upgrades are development tests, not a mandatory matrix for every routine release. Reuse a verified prior-version fixture until its baseline or transport/configuration changes.

GitHub's Linux and Windows runners own unit and backend integration testing. Do not repeat those suites in the desktop VMs. Use Windows 11 and the Linux desktop for limited changed-area checks that require the running native application; retain valid unchanged-path evidence. GitHub's Windows Server runner does not establish Windows 11 desktop behavior.

Prepare the native smoke helpers, fixed small TIFF and all six desktop environments here. Exercise helpers during development and record their revisions. Existing checks may be reused only when their relevant source, dependency and packaging inputs match; name the original source and evidence. A new source revision is never silently accepted as the old one.

Exercise changed release helpers through their actual command entry points and small fixtures here. Validate GitHub metadata operations with an isolated disposable draft when those operations change; mocks alone do not prove the API path. Prepare signing configuration, release notes and source coverage before final packaging. No helper implementation or lab repair belongs after freeze.

Use the same immutable prepared Linux build environment and exact Windows runtime bundle for validation and packaging. Record source commit, executable hashes and dependency bundle identities separately. Check the actual prepared dependencies against the approved source material before any final packaging command, and report individual dependency differences. An unpinned `apt-get update/install` in a fresh release job does not preserve the tested environment even when its base container digest is pinned. Keep the full environment inventory for provenance; changing a gate to focus on shipped runtime dependencies requires separate review of the actual payload and source coverage, never ignoring an unexplained mismatch.

Upload the completed functional and lab-readiness records through **Release evidence**, stage A. It combines them with the trusted automated validation run. Missing, failed, inconclusive or expired readiness blocks source freeze. See the script commands and report fields in [the acceptance reference](scripts/release/RELEASE-ACCEPTANCE.md).

## Stage B: freeze and build final packages once

Freeze the exact reviewed source commit only after Stage A passes and the validated executables and dependencies are available by immutable artifact identity. `bounded.py freeze` records source/input identities, the readiness run and the original deadline. For bounded commands, use `bounded.py run --seconds N ID REPORT -- COMMAND`; place `--seconds` before the positional paths. Record the reviewed orchestration SHA and verify the candidate run's `head_sha`.

Before freeze, run **Stage A development validation** in `packaging-readiness` mode using the successful automated validation run, the sealed tested Linux runtime and the reviewed source plan. Download its `prepared-manifest-linux` and `prepared-manifest-windows` artifacts into one directory. The freeze command validates the successful preparation run, its reviewed ancestry and both executable identities, then records the manifest hashes automatically:

For new schema 2 preparations, `source_review_url` identifies the reviewed dependency baseline named in the CI development archive's `DEVELOPMENT-SOURCE.json`. Preparation verifies that embedded coverage, executable, metadata, current application locks and native identities. It pins baseline coverage separately from the final application archive. After freeze, assemble the new application archive once; its final review must name the exact frozen source and locks and retain the prepared `dependency_source` baseline. The final archive hash, source receipt and full Git-inventory verification remain mandatory. Historical schema 1 manifests retain their original exact archive binding.

```sh
python3 scripts/release/bounded.py freeze READINESS_JSON FROZEN_JSON \
  --readiness-run-id READINESS_RUN_ID \
  --prepared-directory PREPARED_MANIFEST_DIRECTORY \
  --packaging-run-id PACKAGING_PREPARATION_RUN_ID \
  --source-root CLEAN_VALIDATED_SOURCE_CHECKOUT
```

Use the reviewed orchestration checkout for this command when it differs from the application source checkout. Do not edit the frozen JSON afterward. A missing or mismatched prepared manifest blocks freeze.

Check reservation eligibility read-only before dispatch: `python3 scripts/release/reservation.py --tag vX.Y.Z --source-sha FULL_SHA --check-only`. Supply `--recovery-record PATH` only for a separately reviewed recovery record. Retain failed reservations and their audit trail. Existing assets or a candidate publication job that started block recovery. A new explicitly authorized attempt with repaired, validated inputs may reuse an empty unpublished reservation only with exact reviewed prior-run/body identities, completed stopped runs and a different prepared-input freeze. Never move a published tag, replace release assets or weaken a source gate. Recovery support does not authorize an automatic repair/retry loop.

### Finish an interrupted candidate publication without rebuilding

When package jobs passed and publication alone stopped, an explicitly authorized completion attempt can reuse those exact packages. The normal build workflow deliberately rejects a partially populated draft. Use the separate reviewed `resume-release.yml` workflow, with a pinned recovery record, instead of rerunning packaging or clearing the draft.

Before dispatch, review the prior stopped run, successful package jobs, original frozen receipts, source manifest and every retained asset ID/hash. Prepare fresh lab readiness and upload it through **Release evidence**, stage A, retaining unchanged functional validation. Run the resume helper's read-only `check` entry point against those records. Only then dispatch the resume workflow with `readiness_run_id`. Its fresh 60-minute completion receipt links the stopped attempt and preserves the historical source proof. It verifies the existing assets, uploads only the missing corresponding-source archive, and publishes the candidate outside Latest. It never deletes or replaces an asset, recompiles an application, or assembles source material again. A mismatch stops recovery.

Use the resulting successful resume run's frozen candidate for the same fixed Stage C smoke checks and promotion. The cancelled run remains cancelled, its deadline remains expired, and the original package/source identities stay unchanged. No functional matrix is repeated.

### Revalidate an already-public candidate after a stopped smoke attempt

An explicitly authorized new attempt may reuse the complete public v0.2.0
candidate through the same workflow's `complete-public` mode. This is separate
from `complete`, which still requires the original incomplete draft. Do not
rerun a stopped workflow or edit its frozen receipts.

The reviewed `public-candidate-recovery.json` pins the previous successful
completion run, its frozen receipt hashes, the release body and all 14 asset
IDs, sizes and hashes. Prepare fresh Stage A lab readiness with the reviewed
helpers while retaining valid unchanged application checks. Exercise the
read-only `resume_publication.py check-public INPUTS` entry point first; its
inputs include the original attempt, the previous completed candidate and fresh
trusted readiness. Then dispatch:

```sh
gh workflow run resume-release.yml --ref main \
  --field mode=complete-public --field readiness_run_id=READINESS_RUN_ID
```

This mode verifies the previous successful run and immutable candidate, starts
a separate 60-minute completion receipt linked to that history, and downloads
the 14 assets once for hash, signature and corresponding-source verification.
It does not compile, assemble source, upload assets or edit the release. Use its
new trusted frozen candidate for the fixed Stage C smoke tests and normal
promotion. A mismatch or required failure stops this attempt; it does not
authorize another recovery or reset the clock.

Prepare and review corresponding-source material **once** for that frozen commit. Transfer those verified bytes instead of assembling the same archive again on a runner. A pinned external review manifest lets source material be prepared without editing the frozen commit. Keep required dependency coverage, notices, hashes and native identities. The source gate remains mandatory. See [source material](scripts/release/SOURCE-MATERIAL.md) for cache reuse and the remaining archive-size limitation.

Create an annotated application tag on the exact validated commit, which must be contained in `main`. This may be the reviewed feature commit after merge; do not substitute a merge commit with different source identity. Never move a published tag or replace an asset. Dispatch **Stage B signed candidate** with the trusted readiness/frozen-attempt records and reviewed source-manifest digest. A tag push alone does not begin packaging.

The Windows and Linux jobs create the final updater-signed packages in parallel from the exact validated executable and runtime artifacts. Each package is built once. They must not compile application code, resolve fresh dependencies or run the functional suites. Short CI payload checks compare bundled executable/runtime hashes with Stage A, verify package structure and record build identity, signatures and asset hashes. These are not desktop acceptance. If the current workflow recompiles or reconstructs dependencies instead, deployment is blocked pending separately scoped workflow development.

CI orchestration uses the standard hosted runner's preinstalled Python, Git and PowerShell. Bootstrap tools must be available without downloading an installer during Stage B. The Windows job checks its cached Python version and records the selected path and version. Node, Cargo, NSIS, AppImage tools and the signature verifier come from the sealed packaging inputs; sealing the entire CI operating system is outside this procedure.

The candidate remains a prerelease outside Latest. A source or dependency change invalidates the frozen attempt and affected evidence. A package/build defect stops the deployment request; report the defect for separately scoped Stage A repair instead of starting another attempt automatically. Preserve existing public tags and assets, and use a higher application version after a published candidate defect.

## Stage C: small package smoke and publication

Download the exact candidate assets into a fresh directory. Verify the complete asset set, hashes, updater signatures and feed version/immutable URLs before launching them. The native adapters require prepared environments and the Stage A pinned helper/TIFF identities.

| Target                 | Entire required desktop smoke                                                                                                                                              |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Windows Setup          | Standard-user install without elevation; launch; open the fixed small TIFF; observe ready state, visible pixels and expected dimensions; close normally and observe exit 0 |
| Windows portable       | Launch the actual portable executable with its incoming payload absent; open the same TIFF and observe rendering; close normally and observe exit 0                        |
| Linux AppImage         | Launch the same verified bytes on Debian, Ubuntu, Arch/Omarchy and Fedora; open the same TIFF and observe rendering; close normally and observe exit 0                     |
| Debian/Ubuntu fallback | Repeat only that small launch/render/close check with extraction fallback                                                                                                  |

**Do not repeat** annotation editing, save/reopen or pixel-preservation suites, concurrency scenarios, extensive cancellation, performance tests, full codec coverage or a GUI upgrade matrix after packaging. If a changed packaging/updater component needs one extra final-byte check, define its exact scope and timeout during Stage A, before freeze; never enlarge the routine suite during execution.

Record pass, fail or inconclusive with helper revision, OS/user/backend, source SHA, artifact hash, screenshot identity, dimensions and actual process exit. PID absence or a forced kill does not prove successful closure. Each step has a fixed timeout, and the original overall deadline still applies.

A required failure, unavailable lab or timeout stops the deployment request. Retain the failure and diagnostics; application defects and helper repairs need separately scoped Stage A work. At most one bounded retry across the entire release is allowed for unchanged bytes after an identified transient environment failure, with both attempts retained and inside the original deadline. Assertion changes and helper repairs do not qualify. An expired attempt is **blocked, not published**.

Upload the six compact smoke reports through **Release evidence**, stage C. **Stage C promote exact bytes** verifies the trusted candidate and smoke records, deadline, hashes and signatures, and promotes those exact assets without rebuilding or replacing them.

After promotion, verify public download identity and `latest.json` version/URLs. Use public hashes and server asset identities; record the results. Do not open another functional or upgrade campaign. Finalize concise GitHub notes with the source/checksum identity, smoke report link and existing support limitations. Website/email announcements are separate actions.

## Timing and maintenance

The target is about 30 minutes from validated source freeze to publication, with a 60-minute hard stop. Measure actual source/build/smoke/transfer durations. The last packaging attempt measured Setup 5m38s, portable 5m32s and Linux wrapping 14s. Candidate input download took 2m55s before verification and upload, so five minutes was insufficient for that whole job. The [recovery report](docs/release-recovery-2026-10-14.md) shows the older signed build alone took 30m57s, and cold portable extraction took about 5m29s on the test VM. A successful end-to-end 30-minute release has not yet been demonstrated.

Maintain and recover VMs separately using the [lab procedure](scripts/release/LAB.md). Keep reusable sparse bases and small disposable overlays, current/previous relevant inputs and compact durable reports. Do not provision operating systems, create users, repair automation or modify desktop settings in Stage C.
