# Release setup and package reference

Use [RELEASING.md](../../RELEASING.md) for the operator procedure and
[desktop acceptance](RELEASE-ACCEPTANCE.md) for the test gates. Commands and code
paths below are relative to the repository root.

## Build environments

The reusable `package-build.yml` builds Windows on `windows-2022` and Linux in the
pinned Debian 12 container on an Ubuntu runner. It runs frontend build/tests,
Rust tests with a real worker executable, and Clippy before optimized packaging.
Windows Server runner evidence does not establish Windows 11 standard-user
behavior. Prepare native desktop environments separately.

`native-validation.yml` calls the same package workflow without updater secrets
or signatures. Its packages cannot be promoted. Use it before release when
packaging, platform code, toolchains or native libraries change. A routine release
need not duplicate the full unsigned build immediately before the signed build;
the candidate workflow runs the source checks again before packaging.

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

Unsigned native builds and package smoke tests remain available. Immediately
before either platform uploads binary artifacts, the workflow downloads and
verifies reviewed source material against that platform's actual BUILDINFO.
If source material is missing or fails verification, the job stops at this upload
gate after builds and smoke checks; no binary artifacts, including failed packages,
are uploaded. Windows screenshots, process JSON, and text diagnostics can still
be retained. Public binary CI downloads require source material too; signing or
calling a build validation does not establish redistribution compliance.

Unsigned package sets include the verified source archive and its review record
in `SHA256SUMS`, and aggregate metadata checks both native identities and each
build's application lock hashes.
The gate does not assemble sources or certify their distribution.

The current gate pins exact lockfile bytes. A version-only bump changes
`src-tauri/Cargo.lock` and therefore also requires refreshed reviewed material and
pins. Do this before tagging. Archive assembly is a reviewed manual step; the gate
does not assemble it, and unchanged native dependencies do not bypass it.

To refresh the source artifact when version or dependency locks change:

1. Assemble `Aperture-Corresponding-Source.tar.xz` with the applicable exact-version
   sources, build recipes/configuration, patches, and license material for the
   libraries actually bundled on both platforms, including relevant statically
   incorporated dependencies. The Windows inventory is a starting point, not a
   complete source archive. Check Linux's actual bundled libraries against the
   Debian versions in BUILDINFO. This completeness review remains human work.
2. Put `SOURCE-MANIFEST.json` first in the tar archive. It must be a regular file
   of at most 64 KiB with `schema: 1` and `native_dependencies_sha256` containing
   `linux` and `windows` identity hashes. Each hash is SHA-256 of that platform's
   BUILDINFO `native_dependencies` serialized with Python
   `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`
   and encoded as UTF-8. Dependency changes require fresh material and review.
3. Publish the reviewed archive under a fixed source-material tag in this source
   repository, outside latest, then update `corresponding-source.json` through a
   PR with `status: "ready"`, its version-tag asset URL, the archive's SHA-256,
   both native identity hashes, `application_lockfiles_sha256` for `pnpm-lock.yaml`
   and `src-tauri/Cargo.lock`, and `application_source_inventory_sha256`. Verify
   those exact lock bytes against the archived application source inventory,
   which the first manifest must pin under `inventory_sha256`. Use a new
   source-material tag and preserve published
   bytes. The workflow downloads actual bytes, checks their pinned hash and identity
   record, then compares native identities and lock hashes against both completed
   binary builds.

The separate unsigned prior-version fixture must use `source-material --platform
windows --upgrade-fixture`. Its preserved `test-deltas/src-tauri/Cargo.lock` may
change only the aperture root version from `0.1.1` to `0.0.9`; its actual BUILDINFO must hash
that exact file. Production upload and metadata gates have no fixture exception.

The gate checks reviewed byte/dependency identities. It does not determine
license obligations or prove that the archive's contents are complete or
rebuildable; those claims require the source/material review above.

## First-release upgrade fixtures

These are historical bootstrap recipes for `0.1.1`, not a command for upgrading
from every later stable version. The Windows helper is version-specific.
For later releases, arrange the previous stable baseline and any isolated test
feed fixture before tagging, following [upgrade acceptance](RELEASE-ACCEPTANCE.md#upgrade-acceptance).

For first-release Windows upgrade testing, dispatch **Native validation** with
`build_upgrade_fixture: true`. After preserving the normal unsigned packages and
verifying corresponding source, the Windows job builds a separate `0.0.9` Setup
fixture. Its loopback feed retains the production public key and signed-version
checks. The `windows-upgrade-fixture` artifact contains the installer, exact test
deltas, build identity, notices and verified source material. It receives no
signing secrets and cannot be enabled in a signed release build. Use it only in
an isolated desktop VM with the actual candidate installer and signature.

For the first-release Linux upgrade test, follow the
[local prior-AppImage recipe](LINUX-UPGRADE-FIXTURE.md). It builds
an unsigned `0.0.9` fixture in a separate QA checkout with the production public
key and a test-only loopback feed, records its provenance and version deltas,
then runs the native updater harness against the exact signed candidate.
Keep this fixture local; it is not a release or CI download asset.
