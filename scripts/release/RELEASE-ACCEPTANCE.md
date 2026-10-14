# Desktop acceptance for release candidates

Use this checklist with [RELEASING.md](../../RELEASING.md). Test the exact candidate
assets, with hashes recorded before launch. A required check passes only when its
expected behavior is observed. Automated source and worker tests remain mandatory,
but do not replace native desktop acceptance.

## Prepare the test lab separately

Keep licensed Windows 11 and Linux VM snapshots ready with initialized desktops,
a standard-user account, sufficient RAM/disk and no pending restart. Record the
snapshot date and exact OS build. Update and maintain these environments between
releases. Revert to a clean baseline for installation, portable extraction and
upgrade tests; keep development tools and system image libraries out of it.
Install observer tools separately and never elevate Aperture to make a test pass.

Use disposable generated fixtures and owned output folders. Never save into real
microscopy source datasets. Keep VM disks, caches and generated evidence out of
Git; place the summarized report and essential evidence in a durable release PR
comment or associated GitHub record. CI artifacts expire after 14 days.

Aim for 30 to 60 minutes of routine acceptance after candidates and environments
are ready. This is a planning target, excluding builds, source-material assembly,
lab maintenance and expanded checks. Do not drop required checks to meet it.
If environments or observer tools are unavailable, report the block before
spending hours repairing the lab during a release.

## Routine checks for every candidate

Run these shared desktop checks for Windows Setup, Windows portable and each
listed Linux distribution. Use small fixtures with known dimensions and pixels.

| Check                    | Required observation                                                                                                                                                                                       |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Launch and native open   | The default launch reaches a usable viewer. The native file chooser opens a representative PNG and TIFF, including a filename with non-ASCII text, spaces and brackets.                                    |
| Viewing and navigation   | Actual image pixels appear with correct dimensions; pan, zoom, fit, folder navigation and display adjustment respond.                                                                                      |
| Annotation save          | Add points to an owned TIFF copy, save and reopen it. Independently verify retained points and unchanged image pixels. A screenshot or reported file size alone is insufficient.                           |
| Unsaved-work guard       | Cancel a replacement/close with edits and observe retained edits. Discard through the real dialog and observe the requested operation complete. Exercise Save during the upgrade check.                    |
| Cancellation and closure | Close naturally and observe the application exit. Exercise processing cancellation and close during processing on Windows and Linux, and verify owned workers exit. Do not count a forced kill as success. |
| Restart and persistence  | Relaunch the same bytes and verify saved preferences and normal warm-cache operation.                                                                                                                      |

Complete the full PNG/JPEG/BMP/TIFF/BigTIFF opening set once on Windows Setup and
once on Linux. Portable and the remaining Linux desktops still require the
representative PNG/TIFF and all shared checks above. The editions share viewer
code; this avoids repeating the entire format matrix on identical payloads.
Shared heavy-worker checks may use one Linux desktop with those same AppImage
bytes, with the environment and reason recorded. Each desktop still requires
natural closure and its ordinary cancellation/unsaved-work checks.

| Edition/environment                    | Additional required checks                                                                                                                                                                                                                                                                                                                            |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Windows 11 x64 Setup, standard user    | Clean per-user install, shortcut launch and uninstall without elevation; image files remain untouched. Verify bundled Fixed WebView2 processes. Disconnect networking for launch and image viewing. Record any stock Evergreen runtime.                                                                                                               |
| Windows 11 x64 portable, standard user | With no extracted payload, launch the public executable without arguments while offline and observe first extraction plus rendered pixels. Test warm launch and writable local data with the executable in a read-only location. Check its browser download action after promotion using the previous stable portable executable, as described below. |
| Debian 12 and Ubuntu 24                | Same AppImage, native desktop session, normal FUSE launch and `--appimage-extract-and-run` fallback. Observe viewer functionality on both launch paths.                                                                                                                                                                                               |
| Arch/Omarchy and Fedora                | Same AppImage and native desktop checks. Record X11 or Wayland; cover both session types across the matrix, including native Wayland on Fedora. Exercise FUSE/fallback where supported and record the actual path.                                                                                                                                    |

Record exact current distro versions rather than claiming support for every
version of a distribution. Windows 10 is unverified; adding it to the supported
matrix needs a licensed test environment and acceptance evidence. Windows
publisher signing is pending and is a disclosed limitation, not a passed check.

## Upgrade acceptance

Every candidate requires an actual signed Windows installed upgrade and Linux
AppImage upgrade, with retained preferences, annotations and successful restart.
One representative Linux desktop is sufficient for the routine upgrade; expand
that coverage when the updater, runtime or native dependencies change.

Use the previous stable version as the baseline after the first release. Because
candidates are outside latest, the normal production feed cannot offer them.
Arrange an isolated test transport before tagging. If this requires rebuilding
the prior application with a loopback feed, use the previous stable source,
retain its version, production public key and signed-version checks, and record
all configuration deltas and provenance. Call that evidence a prior-code fixture
upgrade, not an upgrade from the untouched published prior binary. Deliver the
exact incoming candidate bytes and signature. Do not alter public downloads,
production client configuration or production feeds for this test.

For `0.2.0`, the optional unsigned Windows fixture job and
`previous-upgrade-fixture.py` use the pinned released `0.1.1` source with its
version and lockfiles unchanged. See the
[fixture reference](RELEASE-REFERENCE.md#previous-stable-upgrade-fixtures).
Future baseline changes require review of that pinned tag and commit. The
[0.0.9 recipe](LINUX-UPGRADE-FIXTURE.md) is historical bootstrap evidence.
Direct installation of the new Setup is not updater evidence.

Observe update availability, notes and progress; save pending annotation edits
through the real dialog; install and restart into the candidate version. Verify
workers stop before replacement/installer start and preferences survive. Check
the previous release reports no update against its ordinary production feed.
Portable remains browser-download-only; it does not run the installer updater.
Its download action requires a newer version in the stable feed, so it cannot be
exercised from the candidate before promotion. After promotion, launch the
previous stable portable executable unchanged, check the production feed, and
verify its action opens the new release's exact portable URL without starting
an installer. This uses the published prior executable, with no additional
fixture build. Record it with the other post-promotion production-feed checks
in [RELEASING.md](../../RELEASING.md#5-verify-production-and-announce).

## Expanded checks selected before tagging

Review the diff since the previous stable version and record the applicable rows.
When multiple areas change, use the union of their checks. Give a reason for each
non-applicable row. A broad runtime/library change can require the full matrix.

| Changed area                                                                           | Additional acceptance                                                                                                                                                                                                                                                                                                                            |
| -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| TIFF reading, writing or annotation persistence                                        | Relevant compression, byte order, tiled/stripped, pixel type, BigTIFF and multipage cases; ImageJ interoperability; independent pixel preservation; corrupt-file recovery; a representative large-file save and scratch-space failure.                                                                                                           |
| Updater, signing, installation, portable launcher, Tauri, WebView2 or native libraries | Save/Discard/Cancel, opt-out/deferral/no-update/offline/error paths, tampered payload and signed-version rejection, read-only/permission failures, concurrent extraction, full cold offline launch, worker-before-replacement ordering and upgrades across affected environments. Include second-drive/network-share paths and uninstall safety. |
| Rendering, caching or performance                                                      | Representative changed image types, display/GPU/session paths, cold/warm cache and measured preview/save behavior against the previous stable baseline.                                                                                                                                                                                          |
| File dialogs, navigation or worker lifecycle                                           | Real chooser/dialog behavior, unsaved-work branches, processing replacement/cancellation and absence of stale results or surviving workers on affected platforms.                                                                                                                                                                                |

For multi-instance changes, add focused checks on Windows and Linux: two real
processes with independent images/edits and cache directories; closing one leaves
the other usable; shared-image and changed-file warnings exercise Cancel, Save as
and Overwrite anyway with saved pixels independently verified. Confirm an update
cannot replace files while another initialized window runs, and startup waits
through replacement. On Windows also test the installer/uninstaller admission
hooks, since Tauri exits before Setup replaces files. Existing `0.1.1` processes
lack these locks, so close legacy windows before the first upgrade.

## Automation and evidence rules

Codex should reuse reviewed scripts and existing native tools, then inspect the
critical rendered screens and independent file/process results. Every result
must identify the helper revision and any mocked input or observation limitation.

- `scripts/release/smoke-linux.sh` proves bundled tools/worker behavior, not the desktop.
- `scripts/release/smoke-windows.ps1` provides package/runtime evidence on Windows
  Server. Its window cleanup can force-kill after a timeout, so it does not prove
  natural closure or standard-user Windows 11 behavior.
- `scripts/native-appimage-update.py` exercises real signatures and replacement,
  but supplies chooser/confirmation responses. Combine it with actual native
  dialog checks; it is not complete native-dialog acceptance by itself.
- Other local first-release helpers in ignored `artifacts/` are optional evidence,
  not a portable committed test suite. Record and review them before reuse.

Use pass, fail or inconclusive. A missing observation is inconclusive. If a helper
fails, diagnose it and allow at most one retry of the unchanged bytes after a
specific tool/environment correction. Retain the failed result and correction.
Persistent failure blocks the release. Product failures require a new reviewed
fix and higher candidate version; never weaken assertions or edit binaries.

Use this template in the release PR or associated durable GitHub record:

```text
Candidate tag:
Source commit:
SHA256SUMS SHA-256:
Individual application hashes:
Previous stable version and upgrade fixture/transport provenance:
Diff reviewed and expanded rows selected, with non-applicable reasons:
Operator/date:

Environment | Edition/launch path | Test | Pass/fail/inconclusive | Evidence
Include exact OS build, X11/Wayland, user privilege, fixture/helper revision,
source/saved-file verification and natural process/worker exit observations.

Limitations and retained tool-failure/retry records:
Acceptance decision and report reviewer:
Promotion run:
Post-promotion public-download hashes and production-update observations:
QA cleanup:
```

All required pre-promotion checks need a pass and evidence before the operator
sets `desktop_acceptance=accepted`. Complete and record the separate production
checks after promotion and before announcing success. The workflow verifies
artifact identity, not this report. CI success, a typed confirmation or an expiring
screenshot alone does not establish acceptance. Keep the record linked from the finalized release notes.
