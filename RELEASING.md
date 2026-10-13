# Building and releasing Aperture

Aperture ships from the public [source repository](https://github.com/neuroqp/neuroqp-aperture).
The release process produces Windows x64 Setup and portable executables and a
Linux x86_64 AppImage. macOS, ARM, app stores, MSI, and distro packages are deferred.

## Source and version history

Use feature branches and reviewed pull requests to `main`. Keep unrelated work
out of release commits. Never push directly to `main`, move a released tag, or
replace a published binary. A release tag records the exact reviewed source;
`BUILDINFO` records that commit, toolchains, lockfile hashes, and native dependency
versions. An installer and its updater feed must agree on the version.

Prepare a release on a feature branch:

```sh
python3 scripts/release/release.py version 0.2.0
```

This updates `package.json`, Tauri configuration, Cargo metadata, and Cargo's root
lockfile entry together. Move the relevant `CHANGELOG.md` entries from
`Unreleased` into a dated `0.2.0` section. Describe user-visible improvements,
fixes, limitations, and any action needed during an upgrade. Run the checks and
open a PR; merge only after review. The first version is `0.1.0`.

## Build locations and checks

| Check              | Where                                                 | Purpose                                                                                                            |
| ------------------ | ----------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| Ordinary PR checks | GitHub Linux runner                                   | Formatting, TypeScript, frontend tests; keep these fast.                                                           |
| Native validation  | Local Linux or manual GitHub workflow                 | Rust tests/Clippy, production worker behavior, frontend build.                                                     |
| Package validation | GitHub Windows runner and Debian 12 container         | Compile the actual Windows/MSVC and Linux packages, exercise bundled native tools, retain unsigned test artifacts. |
| Desktop acceptance | Real Windows standard-user session and Linux desktops | Verify installation, image viewing, saving, and updating in the packaged applications.                             |

The package workflow shares its build implementation with release candidates.
Validation builds omit updater signatures and cannot be promoted. Release builds
use the repository's updater signing secret. Build Linux against Debian 12;
building only on the developer's newer Arch system would raise the minimum
runtime requirements unnoticed. Windows builds bundle pinned image libraries
and fixed WebView2, so they do not rely on a user's development environment or
an online runtime installer. Pinned downloads have SHA-256 checks; changing those
versions and checksums is a reviewed source change.

Full native builds are deliberately separate from normal PR checks. Run package
validation when packaging, platform-specific code, toolchains, or native
libraries change. A successful worker test is not desktop acceptance.

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

## Signing and credentials

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
completed corresponding-source distribution. Preserve and publish the applicable
sources/patches alongside downloads, or establish a suitable source-offer process.
This preparation remains a first-publication requirement.

## Candidate and stable releases

After the version PR is merged, fetch `origin/main`, identify the intended commit,
and create the corresponding annotated version tag. Push only that tag:

```sh
git fetch origin
git tag -a v0.2.0 <reviewed-main-commit> -m 'Aperture 0.2.0'
git push origin refs/tags/v0.2.0
```

The release workflow validates source/version identity, builds both platforms,
signs the update artifacts, creates release notes from `CHANGELOG.md`, and
publishes a **prerelease candidate**. It is not marked latest, so installed stable
clients do not see it. The candidate includes:

- The three fixed-name downloads and their detached updater signatures.
- `latest.json`, with immutable version-tag download URLs for all three editions.
- `SHA256SUMS`, platform `BUILDINFO` files, and third-party notices.

Download and test those exact candidate assets. Record the tested tag, source
commit, `SHA256SUMS` hash, operating systems, and acceptance results in the release
PR or associated test record. Run **Promote release** with that identity and the
completed desktop-acceptance confirmation. Promotion verifies the asset set and
checksums, then marks the same release stable/latest. It does not rebuild.

Run `promote-release.yml` on `main` with `tag` such as `v0.2.0`, `source_sha`
and `checksums_sha256` copied from the candidate release notes, and
`desktop_acceptance` set to `accepted` after completing the checks below.
`checksums_sha256` is the digest of the entire `SHA256SUMS` file. Promotion also
verifies updater signatures, the unchanged tag, main ancestry, and increasing
stable versions. It preserves every candidate asset ID and byte.

If a candidate fails, fix it in a new PR and issue a new version/tag. Never change
the bytes behind an already-tested release identity. If a stable release has a
serious defect, remove its latest status or restore the preceding stable release
as latest and ship a higher-version fix. Clients that already upgraded do not
automatically downgrade.

## Desktop acceptance before promotion

Use disposable copies of generated fixtures for saving. Never modify source
microscopy datasets. Record actual results rather than marking an untested item
as passed.

1. **Windows 10/11 x64 standard user:** install without elevation, launch from
   shortcut, open PNG/JPEG/BMP/TIFF/BigTIFF, and save/reopen point annotations.
   Test on a clean machine without libvips, Rust, Node, or an installed WebView2.
   Check non-ASCII names, spaces/brackets, paths on another drive, and an allowed
   network share. Verify worker processes exit on close and cancellation.
2. **Portable:** run offline as a standard user, check first extraction and warm
   launch, concurrent launch behavior, read-only download location with writable
   local application data, and the behavior when disk space or permissions fail.
   Confirm the app offers only a portable download and never launches Setup.
3. **Linux:** run the candidate AppImage on Debian 12/Ubuntu, Arch/Omarchy, and
   Fedora. Check available Wayland/X11 desktop paths, FUSE and extraction fallback,
   file chooser, image formats, annotation save, and cancellation. Confirm no
   system libvips tools are needed. Check a read-only AppImage fails updating
   gracefully and retains the running version.
4. **Upgrade from a previous signed test build:** check no update, update available,
   offline/error handling, notes, opt-out, deferral, and progress. Save/Discard/Cancel
   must work for unsaved annotations. Verify an altered payload is rejected,
   image workers stop before replacement, preferences survive, the new version
   launches, and uninstall leaves source images untouched. Use a test configuration
   and test signing key/feed for destructive upgrade tests; never weaken production
   signature verification or point production clients at an experimental feed.

The first release still needs these platform acceptance results. Automated
compile and worker checks are useful evidence, but cannot establish the behavior
of a Windows desktop session or an upgrade from a released package.

## Website links and announcements

After stable promotion, the website can link directly to:

- [Windows Setup](https://github.com/neuroqp/neuroqp-aperture/releases/latest/download/Aperture-Setup-x64.exe)
- [Windows portable](https://github.com/neuroqp/neuroqp-aperture/releases/latest/download/Aperture-Portable-x64.exe)
- [Linux AppImage](https://github.com/neuroqp/neuroqp-aperture/releases/latest/download/Aperture-x86_64.AppImage)
- [Release notes](https://github.com/neuroqp/neuroqp-aperture/releases/latest)

GitHub Releases is the download host and release-note record; the website does
not need its own binary storage or update service. The same release notes appear
in the app's updater. Users can subscribe to GitHub's release notifications.
Publishing a website announcement or sending email is a separate editorial step;
the workflows do not send messages on your behalf.
