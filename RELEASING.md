# Releases

A release has three operations: validate, package, publish. One operator owns the
sequence and its reports. Finish behavior testing and corresponding-source
preparation first. After packaging, check only package identity and the small
native smoke matrix, then publish the same bytes.

## Validate

1. Finish changes on a feature branch and review them. Keep the version in
   `MAJOR.MINOR.PATCH` form and write short release notes. During `0.x`, features
   use a minor bump and fixes use a patch bump. Use a higher version after a public
   release defect. Merge only with user authorization; never push `main` directly.
2. Run `validate.yml` in build mode for the exact reviewed commit. It runs the
   frontend, Rust, release-helper and actual-worker regressions, compiles the
   release-profile Windows/Linux executables and records their runtime inputs.
3. Run the affected-area [native functional checks](scripts/release/RELEASE-ACCEPTANCE.md)
   with those executables. Restore and check desktop sessions, observers and the fixed small
   TIFF. Record actual results, executable/runtime hashes and relevant screenshots.
4. Prepare and verify [corresponding source](scripts/release/SOURCE-MATERIAL.md).
   Dependencies, patches, recipes and notices must cover the actual shipped
   runtime. Dependency changes require refreshed coverage.
5. Record the validated inputs with `validate.yml`. Its prepared archives contain
   the tested executables and runtime dependencies. Retain the reports and artifact
   hashes. Packaging consumes those exact bytes; standard pinned packaging tools
   may be installed during packaging.

See [the command reference](scripts/release/RELEASE-REFERENCE.md) for inputs and
commands. A passing CI job does not establish native desktop behavior.

## Package

Run `package.yml` with the successful validation run and exact source
commit. It wraps the validated executables, creates updater signatures, verifies
payload identities and emits the release files plus `package.json`. It must not
compile application code, resolve fresh dependencies or repeat functional suites.
Keep that output even if a later operation fails.

Verify the complete asset set, SHA-256 values, updater signatures, source manifest,
version and immutable download URLs before launching anything. Download into an
owned directory. The desktop smoke uses only these verified bytes:

| Environment                        | Required check                                                                                                                                              |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Windows 11 Setup, standard user    | Install without elevation; launch; open the fixed small TIFF; observe ready state, visible image pixels and expected dimensions; close normally with exit 0 |
| Windows 11 portable, standard user | Launch the actual downloaded executable with its incoming version payload absent; perform the same render and normal-close check                            |
| Debian 12                          | Launch the AppImage; perform the same render and normal-close check                                                                                         |
| Ubuntu 24, native Wayland          | Launch the same AppImage bytes; prove the native Wayland window and perform the same render and normal-close check                                          |

Arch/Omarchy and Fedora are compatibility targets. Check them periodically and
when runtime dependencies, package launchers or desktop integration change.
They are outside the routine release matrix and carry no per-release guarantee.
Windows 10 is unverified. Windows publisher signing is pending. macOS, ARM,
stores and distro packages are outside the current scope.

Test AppImage extraction fallback, upgrades and detailed installer behavior during
validation when those paths change. Do not expand final smoke into format,
annotation, save, concurrency, cancellation, performance or upgrade suites.

## Publish

Run `publish.yml` with the successful package run, exact source commit and the
four passing native smoke reports. It rechecks source/package/report identities
and updater signatures, then publishes those bytes with their notices, checksums,
source composition and release notes. Existing matching assets may be reused;
different bytes under an existing public asset identity fail. Never move a
published tag or replace a public asset.

Verify the public download routes, version, checksum files and production updater
feed. Retain the compact validation/package/smoke reports in a durable PR or
release record. Keep current and previous relevant packages and source inputs;
GitHub Actions artifact retention is not permanent storage.

## Timeouts and interrupted work

Use a generous timeout for each build, transfer, package and desktop operation.
A timeout stops that operation and retains its outputs and diagnostics. It does
not expire successful validation, source archives or completed packages. Check
source, executable, runtime dependency and package hashes before reusing them.
Changes invalidate affected results, so rerun only those checks.

Continue authorized repairs and retry the failed operation. Do not create a new
release reservation, receipt deadline or special recovery workflow. A failure or
inconclusive required check blocks publication until fixed and revalidated. The
same release authorization covers this work unless the user limits it. Maintain
VMs separately using [LAB.md](scripts/release/LAB.md); keep real datasets untouched.
