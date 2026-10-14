# Release setup and package reference

Use [RELEASING.md](../../RELEASING.md) for the operator procedure and
[desktop acceptance](RELEASE-ACCEPTANCE.md) for the test gates. Commands and code
paths below are relative to the repository root.

## Build environments

`native-validation.yml` is Stage A. It compiles development binaries with their
pinned native runtimes, runs the automated functional/regression suites and
produces reusable development evidence. Prepare native Windows/Linux desktops
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
