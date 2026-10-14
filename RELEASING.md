# Building and releasing Aperture

Aperture ships from the public [source repository](https://github.com/neuroqp/neuroqp-aperture).
We offer Windows x64 Setup and portable executables and a Linux x86_64 AppImage.
macOS, ARM, app stores, MSI and distro packages are deferred.

Follow these steps in order. Source checks precede optimized release packages;
desktop acceptance uses the exact candidate downloads. Promote those bytes
without rebuilding. Detailed [setup and source-material instructions](scripts/release/RELEASE-REFERENCE.md)
and the [acceptance checklist and report template](scripts/release/RELEASE-ACCEPTANCE.md)
are separate references.

## 1. Prepare reviewed source

Use a feature branch and a reviewed PR to `main`. Keep unrelated work out of
release commits. Never push directly to `main`, move a released tag or replace a
published binary. Release work does not itself authorize merging a PR.

Choose an increasing `MAJOR.MINOR.PATCH` version. During `0.x`, use a minor bump for
features or incompatible changes and a patch bump for fixes. Describe any
compatibility change explicitly. Stable promotion currently accepts three-number
versions, with no prerelease suffix. For example, on the feature branch:

```sh
python3 scripts/release/release.py version 0.2.0
```

This updates `package.json`, Tauri configuration, Cargo metadata and Cargo's root
lockfile entry. Move the relevant `CHANGELOG.md` entries from `Unreleased` into a
dated `0.2.0` section. Include user-visible changes, known limitations and upgrade
actions. Refresh reviewed corresponding-source material and pins even for a
version-only Cargo lock change, as required by the current source gate.

Run the automated source suites before tagging. On a prepared Linux development
machine, with the pinned tools and native dependencies available:

```sh
pnpm install --frozen-lockfile
pnpm format:check
pnpm exec tsc --noEmit
pnpm test
python3 scripts/release/test_release.py
python3 scripts/release/test-source-windows.py
python3 scripts/release/release.py check
python3 scripts/release/release.py source-material build/release-preflight --fetch
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
pnpm build
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol --locked
APERTURE_WORKER_EXECUTABLE="$PWD/src-tauri/target/debug/aperture" cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
```

Expect all commands to exit successfully, version check to print the intended
version, and source preflight to verify the reviewed archive. Compiling the test
and worker executables is necessary before running Rust tests. The candidate jobs
repeat native source checks on both build platforms before optimized packaging.

Keep ordinary PR checks fast. Run targeted native checks for changed UI behavior
and unsigned **Native validation** when the build or packaging changes. Browser
previews and mocked dialogs do not establish native desktop behavior. Before
tagging, identify the expanded acceptance checks needed for the diff since the
previous stable version, and confirm the test environments and upgrade baseline
are ready. Do not begin a release with required environments unavailable.

## 2. Build one immutable candidate

After the PR has passed checks and review and an authorized merge has completed,
fetch `origin/main` and identify the reviewed commit. For example:

```sh
git fetch origin
git tag -a v0.2.0 <reviewed-main-commit> -m 'Aperture 0.2.0'
git push origin refs/tags/v0.2.0
```

Watch **Release candidate** in GitHub Actions. It checks version/tag/main ancestry,
source materials and build identities, runs source tests, builds packages, signs
updates and runs package smoke checks. It publishes a prerelease outside latest,
so stable clients do not see it. `BUILDINFO` records the source commit, toolchains,
lock hashes and native dependency versions. The assets include:

- Three fixed-name application downloads and their updater signatures.
- `latest.json` with immutable version-tag URLs and changelog notes.
- `SHA256SUMS`, platform `BUILDINFO`, third-party notices and reviewed source material.

Require successful jobs and verify the exact downloaded asset set on a prepared
Linux verifier with `minisign` installed. Use a new empty destination:

```sh
gh release download v0.2.0 --repo neuroqp/neuroqp-aperture --dir build/candidate-v0.2.0
python3 scripts/release/release.py verify build/candidate-v0.2.0 --version 0.2.0 --source-sha <reviewed-main-commit> --cryptographic
sha256sum build/candidate-v0.2.0/SHA256SUMS
```

Expect verification to succeed and the checksum-file digest to match the candidate
notes. Record the tag, source commit and this digest in the acceptance report.

## 3. Accept the exact downloads

Complete [routine desktop acceptance](scripts/release/RELEASE-ACCEPTANCE.md#routine-checks-for-every-candidate)
on Windows 11 standard-user Setup and portable editions, and the same AppImage
on Debian 12, Ubuntu 24, Arch/Omarchy and Fedora. Record exact OS versions and
desktop sessions. Windows 10 remains unverified and publisher signing remains
pending; do not advertise either as validated. Complete the selected expanded
checks and actual signed upgrades as well.

Record pass, fail or inconclusive results with evidence in the release PR or an
associated durable GitHub test record. The report must identify the tested bytes,
all required environments and tests, the operator, limitations and the decision.
Any required failure, inconclusive result or missing evidence blocks promotion.

If the application fails, fix it in a new PR and issue a higher version/tag. Never
replace candidate bytes or retag. A diagnosed test-tool failure permits a bounded
retry of unchanged bytes; retain the original failure and explanation.

## 4. Promote without rebuilding

Review the completed acceptance report. The operator may run **Promote release**
on `main` only when all required checks passed. Supply:

- `tag`, for example `v0.2.0`.
- `source_sha`, the full candidate commit.
- `checksums_sha256`, the digest of the entire tested `SHA256SUMS` file.
- `desktop_acceptance`, set to `accepted` after reviewing the report.

The workflow re-downloads and verifies assets, signatures, tag/main ancestry and
increasing stable versions. It preserves every candidate asset ID and byte, then
marks the release stable/latest. The acceptance report is an operator gate:
the workflow checks the typed confirmation, not the report or its test results.

## 5. Verify production and announce

After promotion, complete these checks before announcing success:

1. Finalize the GitHub release body. Remove the candidate-only warning, retain
   source/checksum identity, and link acceptance evidence and support limitations.
   Promotion changes state and title only; it does not finalize the body.
2. Download all three applications and `latest.json` through the public latest
   routes without authentication. Verify complete download hashes against the
   accepted set; confirm the feed version and immutable asset URLs.
3. In a released Windows installed app and Linux AppImage, perform a real manual
   check against the production feed and observe the up-to-date result. Verify
   the portable check uses the portable edition. Test feeds cannot prove this step.
4. Append production results to the acceptance record and stop owned QA processes,
   feeds and VMs. Restore temporary test settings.

If production verification fails, pause announcements, remove latest status or
restore the preceding stable release as latest, and investigate. Ship a
higher-version fix for product defects. Already-upgraded clients do not
automatically downgrade.

The website can link to the fixed public routes:

- [Windows Setup](https://github.com/neuroqp/neuroqp-aperture/releases/latest/download/Aperture-Setup-x64.exe)
- [Windows portable](https://github.com/neuroqp/neuroqp-aperture/releases/latest/download/Aperture-Portable-x64.exe)
- [Linux AppImage](https://github.com/neuroqp/neuroqp-aperture/releases/latest/download/Aperture-x86_64.AppImage)
- [Updater feed](https://github.com/neuroqp/neuroqp-aperture/releases/latest/download/latest.json)
- [Release notes](https://github.com/neuroqp/neuroqp-aperture/releases/latest)

GitHub hosts downloads and release notes. Updater notes come from the changelog
when candidate metadata is built; editing the GitHub body later does not change
the immutable feed. Users can subscribe to GitHub release notifications.
Website announcements and email are separate editorial actions; workflows do not
send them. The first stable release was `0.1.1`; `0.1.0` was not promoted.
