# Release setup and package reference

Use [RELEASING.md](../../RELEASING.md) for the operator procedure and
[desktop acceptance](RELEASE-ACCEPTANCE.md) for the test gates. Commands and code
paths below are relative to the repository root.

## Build environments

`native-validation.yml` is Stage A. It compiles development binaries with their
pinned native runtimes, runs the automated functional/regression suites and
produces reusable development evidence. Its release-profile native binaries warm the same dependency compilation cache used by Stage B without creating any Setup, portable or AppImage package. Prepare native Windows/Linux desktops
and perform changed-area functional validation here. Optional prior-version
fixtures are development inputs; reuse verified ones while their baseline and
configuration stay applicable.

`package-build.yml` is Stage B. It builds updater-signed final packages once,
Windows on `windows-2022` and Linux in the pinned Debian 12 container on an Ubuntu
runner. It consumes trusted Stage A and frozen-source evidence. It does not rerun
frontend/Rust/worker functional suites or build an unsigned optimized edition first.

The short CI `smoke-linux.sh` and `smoke-windows.ps1` check payload structure,
bundled runtime and build identity. The versioned `smoke-desktop-*` adapters perform
Stage C's fixed native launch/one-TIFF/render/normal-exit cases. Windows Server
payload evidence does not establish Windows 11 standard-user behavior. Observer
tools and every native desktop must already be ready before freeze.

Pinned downloads have SHA-256 checks. Changes to toolchains, library versions,
checksums, workflow code or release scripts require source review.

## Operator sequence

Use a clean reviewed checkout. Keep reports and downloads in an ignored evidence
directory. Workflow dispatches below run from `main`; their `source_sha` identifies
the exact validated source, not necessarily the current `main` tip.

1. Run `native-validation.yml` on the reviewed feature commit. Complete affected-area
   native functional tests and all lab-readiness checks. Submit their readiness
   manifest with the successful validation run ID:

   ```sh
   gh workflow run release-evidence.yml --ref main -f stage=A \
     -f source_sha="$release_sha" -f input_run_id="$validation_run" \
     -F report_json=@readiness-input.json
   ```

2. After that evidence run succeeds, download its `release-readiness` artifact.
   Freeze once and upload the exact original receipt:

   ```sh
   python3 scripts/release/bounded.py freeze release-readiness.json frozen-attempt.json \
     --readiness-run-id "$readiness_run"
   gh workflow run release-evidence.yml --ref main -f stage=B \
     -f source_sha="$release_sha" -f input_run_id="$readiness_run" \
     -F report_json=@frozen-attempt.json
   ```

3. Prepare source material once with `refresh-source.py --source-sha "$release_sha"
--prior-archive PATH --prior-review PATH --attempt frozen-attempt.json
--output FRESH_DIRECTORY --write --timeout 600`. Review the resulting external
   manifest and archive, mark the manifest ready with its immutable publication
   identity, and transfer those exact bytes under a new source-only tag. Validate
   with `bounded.py source-receipt` as described in [SOURCE-MATERIAL.md](SOURCE-MATERIAL.md).
   No runner assembles a second archive.
4. Create and push the annotated application version tag on `$release_sha` after
   that commit is contained in `main`. Dispatch `release.yml` from `main` with
   `tag`, `source_sha`, `frozen_run_id`, `frozen_sha256`,
   `source_review_json=@REVIEW.json` and `source_review_sha256`. Supply file inputs
   with `-F` and digests with `-f`. Compute digests from exact files using `sha256sum`.
   Watch the existing attempt; never rerun failed packaging or reset its clock.
5. Download that candidate into a fresh directory. Run `release.py verify DIRECTORY
--version VERSION --source-sha "$release_sha" --cryptographic`, then use the
   prepared native adapters for only the fixed smoke cases. Make an owned AppImage
   download executable with `chmod +x`; this does not change its byte hash.
6. Put the six reports in one directory, named `windows11-setup.json`,
   `windows11-portable.json`, `debian12.json`, `ubuntu24.json`, `arch-omarchy.json`
   and `fedora-wayland.json`. Run `bounded.py compact frozen-candidate.json
REPORT_DIRECTORY compact-reports.json`. Preserve raw reports.
   Submit the compact file to `release-evidence.yml`, stage C, with the candidate
   run ID. After it passes, dispatch `promote-release.yml` with `tag`, `source_sha`,
   `candidate_run_id` and `smoke_run_id`. The original deadline must still be live.

Use `gh run view RUN_ID` and `gh run download RUN_ID --name ARTIFACT` to inspect
and obtain exact workflow artifacts. A failed or inconclusive required result
ends the attempt; repair belongs to Stage A. These commands are an operator
sequence, not permission to reconstruct evidence or bypass failed assertions.

## Package behavior

- **Setup:** `Aperture-Setup-x64.exe` installs under the current user's local
  application data, creates shortcuts, and supports per-user uninstall. It must
  not request administrator elevation. Its installed marker enables the Tauri
  installer update path.
- **Portable:** `Aperture-Portable-x64.exe` contains the complete application,
  native libraries, and WebView2. The launcher extracts a version-specific payload
  locally and reuses it. Initial extraction requires time and free disk space;
  subsequent launches reuse those files. It runs with portable mode set and only
  offers browser downloads of newer portable executables. Close the old app and
  replace the downloaded launcher to update.
- **Linux:** `Aperture-x86_64.AppImage` includes the native image tools and their
  dependencies. Keep it in a writable user-owned directory for in-place updates.
  Make it executable before launch. `--appimage-extract-and-run` is the fallback
  when FUSE is unavailable. It still requires a compatible Linux desktop and GPU
  stack; AppImage is not a substitute for testing those environments.

All editions use the stable `com.neuroqp.aperture` identity. Preferences and
image display caches are outside package directories. Linux uses the XDG
application directories; Windows uses user-local application directories.
Generated display data uses a 2 GiB soft cache limit. Saving TIFF annotations can
require temporary disk space proportional to the source image. Portable means
no installed application registration; it does not mean zero local files.
Portable runtimes are retained by version to avoid deleting files used by another
running copy. After closing Aperture, old payload folders under
`%LOCALAPPDATA%\NeuroQP\Aperture\portable` can be removed; launching that version
again will extract its payload. This does not remove image caches or preferences.

## Signing and corresponding source

Tauri update signatures and Windows publisher signatures have separate purposes:

- **Tauri signing is configured.** The public key is committed in
  `src-tauri/tauri.conf.json`; `TAURI_SIGNING_PRIVATE_KEY` is a GitHub Actions
  secret. The initial private key is stored outside Git at
  `~/.config/neuroqp-aperture-release/updater.key` on the provisioning machine.
  Back it up securely before the first release. Losing it prevents existing
  clients from trusting future updates. Do not casually generate a replacement.
- **Windows publisher signing is pending.** Unsigned packages can be built and
  tested. They may show Windows unknown-publisher/SmartScreen warnings, and
  university policies may block them. No certificate purchase or signing-service
  enrollment is part of this implementation. When signing is configured, sign
  the application and final Windows download bytes before generating Tauri
  signatures and checksums. A valid publisher signature does not guarantee
  immediate SmartScreen reputation.

Never put a signing key in Git, logs, build artifacts, or a pull request. Package
validation must not receive release secrets. Only reviewed release-tag builds
use the updater key. Treat workflow and release-script changes as security-relevant.
MIT covers Aperture source; retain third-party licenses and satisfy their source
and redistribution requirements for the actual libraries bundled in each build.
Before distributing the first release candidate, assemble the corresponding native
library sources and build material. Windows inventory and exact upstream versions
are in `scripts/release/windows-lgpl-sources.json`; the build checks them against
the bundled library versions. Linux BUILDINFO records the Debian package versions
and libvips source hash. Notices and source links alone are not presented as a
completed corresponding-source distribution.

The reviewed first-release archive is published under
[`source-v0.1.1`](https://github.com/neuroqp/neuroqp-aperture/releases/tag/source-v0.1.1),
separate from application releases and excluded from latest.
`scripts/release/corresponding-source.json` pins its checksum, both native
dependency identities, exact Cargo/pnpm lock hashes and the archived application
source-inventory hash. [SOURCE-MATERIAL.md](SOURCE-MATERIAL.md)
records the review and historical build-environment limits. The workflow downloads
and verifies the archive, then compares its identities against actual BUILDINFO.
Checkout lock changes block source preflight before downloading.
An identity mismatch blocks binary uploads and requires refreshed material.

Development binaries and optional upgrade fixtures receive no updater signing
secrets. Their source-material requirements still apply before public binary
artifact uploads. Final packages use the reviewed external source plan pinned by
SHA-256 and frozen Git identity. Missing or mismatched coverage, lockfiles, native
identities or BUILDINFO blocks uploads. Do not weaken this gate to speed a release.

Corresponding-source preparation happens once after source freeze. See
[SOURCE-MATERIAL.md](SOURCE-MATERIAL.md) for the reviewed refresh command, exact-byte
transfer/cache reuse, and the separately deferred dependency/archive split. Do not
assemble locally and then assemble the same material again on a publisher runner.
An external manifest avoids changing the frozen commit merely to update source
publication metadata. Source-material tags and assets remain immutable.

## Historical bootstrap upgrade fixtures

The retained `0.0.9` recipes were first-release bootstrap tests for `0.1.1`.
They are not the previous-stable baseline for later releases and are not called
by Stage B or C. [LINUX-UPGRADE-FIXTURE.md](LINUX-UPGRADE-FIXTURE.md) and
`build-windows-upgrade-fixture.ps1` preserve that historical evidence only.

For new Stage A updater work, reuse a verified fixture based on the actual
previous stable source/version and its production key. Rebuild it only when that
baseline or test transport changes. Keep its configuration delta, source/runtime
identities and package hashes with the development record. A direct new Setup
install is not proof of an upgrade. Detailed upgrade testing never becomes an
automatic post-package matrix.
