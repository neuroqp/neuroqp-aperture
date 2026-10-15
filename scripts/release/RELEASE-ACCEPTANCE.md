# Validation and package smoke

Use with [RELEASING.md](../../RELEASING.md). Stage A establishes application behavior. Stage C establishes that the final packages launch and contain the expected runtime. Neither CI success nor a typed approval substitutes for observed native results.

## Stage A functional validation

Run full automated frontend, Rust, release-helper and actual-worker regressions before final packaging. Development compilation is allowed and necessary. Use the compiled native development app on the affected operating systems, with pinned native libraries/runtime. Browser previews alone do not establish native dialogs or filesystem behavior.

Run unit and backend integration suites in GitHub Actions on Linux and Windows. Reserve desktop VM time for limited native functional checks of changed behavior, using the CI development executable. Do not repeat the automated suites there or turn the behavior table below into a mandatory GUI matrix for every release.

| Behavior           | Meaningful pre-freeze observations                                                                                                                 |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Viewing            | Supported formats and relevant TIFF/BigTIFF variants render correct dimensions/pixels; navigation, pan, zoom and display controls work             |
| Saving             | Annotation save/reopen retains points; independent decoded pixels and encoded image data remain unchanged; failures preserve edits and destination |
| Unsaved work       | Real Save/Discard/Cancel branches preserve or discard the intended edits                                                                           |
| Lifecycle          | Replacement/cancellation leaves no stale results or surviving workers; normal close exits without forced cleanup                                   |
| Multiple instances | Independent processes, edits and caches; shared/changed-file advisory Cancel/Save as/Overwrite choices; one window closes without breaking others  |
| Updates/installers | Applicable installer admission, startup exclusion, signature/version rejection, preferences, worker shutdown and upgrade failure paths             |

Select changed-area tests before freeze. Expand codec, large-file, scratch-space, display/GPU or native-library coverage when those inputs change. Reuse valid unchanged-component results with their original input hashes and limitations. Do not repeat every format on every desktop merely because another package is being built.

Previous-stable upgrade fixtures are reusable development inputs. Record baseline source/version, production public key, transport/configuration deltas and fixture hashes. Do not rebuild an unchanged baseline for each attempt. Actual signed final payload signatures and feed identities remain mandatory in Stage B/C; detailed upgrade behavior belongs here.

Prepare the lab, native helpers and fixed small TIFF before readiness approval. Pin fixture SHA-256 and dimensions, helper SHA-256, native desktop backend, tools and readiness expiry for each required environment. A crash/forced-kill result fails readiness. Keep real datasets and generated evidence out of Git.

### Readiness record

`bounded.py identity` records the source and Git input identities. Combine trusted **Stage A development validation** automated records with two native-functional records, Linux and Windows, and six prepared-lab records. A native record includes `suite: native-functional`, tested `areas`, evidence references, development-binary hash, component identities, source SHA, `status` and `forced_kill`. Unchanged component evidence may reference an older source explicitly; current-source native tests must match the automated development binary.

The labs are `windows11-setup`, `windows11-portable`, `debian12`, `ubuntu24`, `arch-omarchy` and `fedora-wayland`. Each records `ready: true`, the exact reviewed `source_sha`, its `environment`, fixed TIFF/helper hashes, dimensions, native desktop/tools with exact paths and SHA-256 values, expiry and a pass. All six
environments use the same fixed TIFF bytes and dimensions. Windows 10 is unverified; publisher signing remains pending.

Submit JSON through **Release evidence**, stage A, with the trusted validation run ID. Download its `release-readiness` artifact. Freeze only after it validates against the exact reviewed checkout. Script `--help` and the workflow inputs define the executable commands; preserve exact file bytes when submitting JSON with `gh workflow run --field report_json=@file`.

## Stage B identity and source preparation

`bounded.py freeze READINESS.json frozen-attempt.json --readiness-run-id RUN_ID` starts the single 30-minute clock. Submit the original receipt through **Release evidence**, stage B. Source preparation/review takes place after this freeze; it does not alter the frozen commit or start a second clock.

Provide the reviewed corresponding-source JSON and its exact SHA-256 to **Stage B signed candidate**, together with the frozen-attempt record identity. Prepare its archive once and transfer it. The source gate compares required coverage, lock/native identities and archived application files with the frozen Git tree. Final package jobs sign updater assets and record BUILDINFO and hashes. They do not rerun this functional checklist.

## Stage C fixed desktop smoke

Use `smoke-desktop-linux.py` and `smoke-desktop-windows.ps1` with the downloaded candidate identity, exact asset/hash and prepared tiny TIFF. Supply existing observer endpoints/native screenshot tools. Missing tooling or a non-native desktop fails readiness; do not install or repair it inside the attempt.

Prepare Linux Stage C with a direct `WebKitWebDriver` listener in Stage A, running as the desktop user on the same desktop bus and display. For example, start the pinned executable with `WebKitWebDriver --host=127.0.0.1 --port=4471` and record `driver_url: http://127.0.0.1:4471` in the candidate identity. Pin its resolved executable path and SHA-256 in that lab's `tools`. The helper uses native `webkitgtk:browserOptions` and verifies the listener process itself. The `tauri-driver` proxy in [LINUX-UPGRADE-FIXTURE.md](LINUX-UPGRADE-FIXTURE.md) belongs to Stage A upgrade testing and cannot be supplied as this Stage C endpoint. Verify direct session creation, native accessibility and screenshot observation before declaring the lab ready.

The fixed cases are launch, render and clean exit. Windows Setup also performs a standard-user install as part of launch. Portable launch starts with the specific incoming payload absent. Linux uses the same AppImage bytes across all four desktops. Debian and Ubuntu add one equally small fallback launch/render/close. Fedora must prove native Wayland, not merely an environment variable.

Rendering requires actual ready image state without an error, expected dimensions and observed image pixels in an owned native window. Preview pixels alone can remain visible after loading fails. Observe normal closure and actual exit 0; a cleanup kill never passes.

Use fixed per-step timeouts and the frozen overall deadline. Record each start/finish/duration, source/input/checksum identities, asset/fixture/helper hashes, OS/user/backend, screenshot SHA-256 and process outcome. A second attempt must retain the first failed record, unchanged-byte identity and a specific transient-lab reason. No third attempt, repaired helper, weakened assertion or restarted deadline is accepted.

A required fail/inconclusive/timeout or missing environment ends the attempt. Return repair to Stage A. Preserve the failed candidate and report; published assets remain immutable. Submit six passing reports through **Release evidence**, stage C. Promotion validates this compact record and the exact candidate identities before changing Latest.

Do not perform annotation/save/pixel-preservation suites, full formats, cancellation/concurrency/performance or GUI upgrades here. Any exceptional final-byte check requires a small reviewed workflow change before freeze; keep its evidence separately named so it cannot silently expand this fixed suite.

## Evidence retention

Put the compact result and essential observations in a durable GitHub PR/release record. Link it from concise final release notes. CI artifacts expire; retain current/previous relevant package identities and compact reports independently. Record every helper/environment failure honestly. Detailed lab preparation, VM cleanup and crash recovery belong in [LAB.md](LAB.md), outside release execution.

The native adapters open the fixed TIFF through the real startup command-line path introduced in 0.2.0. Use a verified 0.2.0 development or existing validation fixture when preparing this helper. A 0.1.1 fixture does not implement that entry point. Observers only read application state; they do not replace Tauri invoke or mock image loading.
