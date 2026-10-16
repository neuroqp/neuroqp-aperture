# Release command reference

Follow [RELEASING.md](../../RELEASING.md). Run commands from the repository root
in a reviewed checkout. Keep downloads and reports in an ignored owned evidence
directory. `source_sha` always means the exact validated application commit.
Release helpers require Python 3.11 or newer. Local examples use `python3.12`;
this host's unqualified `python3` resolves to 3.10. Debian 12's system `python3`
is suitable, and Windows CI selects Python 3.12.

## Workflows

- `validate.yml` compiles development executables, runs automated regressions and
  prepares reusable tested application/runtime inputs and corresponding source.
  Complete changed-area native checks before accepting those inputs.
- `package.yml` consumes the validated inputs and wraps/signs the final packages.
  Standard pinned packaging tools may be installed here. Application executables
  and runtime dependencies must match validation exactly.
- `publish.yml` consumes package output and four passing native smoke reports,
  verifies their identities and publishes without rebuilding.

Inspect a run with `gh run view RUN_ID`. Download exact artifacts with
`gh run download RUN_ID --name ARTIFACT --dir OWNED_DIRECTORY`. Use the workflow
input definitions and helper `--help` for the accepted file/report schema. Preserve
JSON file bytes when computing SHA-256 and submitting them with `-F field=@file`.

## Operator commands

Choose `source_sha` and `version` from the reviewed release commit. After that
commit is in `main`, dispatch from `main` and retain each successful run ID:

```sh
gh workflow run validate.yml --ref main -f mode=build -f source_sha="$source_sha"
```

Download `development-linux`, `development-windows` and `corresponding-source`
from that build run. Perform changed-area native validation with those executables
and prepare the tested Linux AppDir archive. `linux-runtime.py seal --help` defines
its exact runtime/inventory inputs. Store the archive at an owned HTTPS location
and record its SHA-256. Submit native results as `native.json` with `schema: 1`,
`operation: "validate"`, `source_sha` and `reports`. The functional environment names
are `windows11`, `debian12` and `ubuntu24-wayland`.
Each row identifies its tested `binary_sha256`, `status: "pass"`,
`forced_kill: false` and evidence. Then record these validated inputs:

```sh
gh workflow run validate.yml --ref main -f mode=seal -f source_sha="$source_sha" \
  -f build_run_id="$build_run_id" -f linux_runtime_url="$linux_runtime_url" \
  -f linux_runtime_sha256="$linux_runtime_sha256" -F native_json=@native.json
gh workflow run package.yml --ref main -f source_sha="$source_sha" \
  -f validation_run_id="$validation_run_id"
gh run download "$package_run_id" --name release-files --dir "$package_directory"
gh run download "$package_run_id" --name package-manifest --dir "$evidence_directory"
```

Verify and smoke the downloaded packages. Combine the four actual native reports
with the checksum file, then create the annotated version tag at the exact source
commit once. If the tag already exists, verify its target and reuse it.

```sh
python3.12 scripts/release/linux-smoke.py collect "$source_sha" "$package_directory/SHA256SUMS" smoke.json \
  windows11-setup.json windows11-portable.json debian12.json ubuntu24-wayland.json
```

```sh
git tag -a "v$version" "$source_sha" -m "Aperture $version"
git push origin "v$version"
gh workflow run publish.yml --ref main -f source_sha="$source_sha" \
  -f package_run_id="$package_run_id" -F smoke_json=@smoke.json
```

`validated-inputs` contains `validation.json`, native results, tested runtime
archives and source files. `release-files` and `package-manifest` contain the
package operation's complete output. Actions retains these artifacts for 90 days;
download them for reuse beyond that hosting retention period. Successful evidence
has no separate clock expiry.

## Verify package bytes

```sh
python3.12 scripts/release/release.py verify PACKAGE_DIRECTORY --version VERSION --source-sha FULL_SHA --cryptographic
```

Use the exact `package.json` and asset hashes when preparing desktop identities.
Final smoke commands and required observations are in
[RELEASE-ACCEPTANCE.md](RELEASE-ACCEPTANCE.md). Source commands and coverage are in
[SOURCE-MATERIAL.md](SOURCE-MATERIAL.md). Keep full reports alongside their compact
submission. Public tags and assets remain immutable.

## Package behavior

- `Aperture-Setup-x64.exe` installs under the current user's local application
  data, creates shortcuts and supports per-user uninstall without elevation. Its
  installed marker enables the installer update path.
- `Aperture-Portable-x64.exe` contains the app, image libraries and fixed WebView2.
  It extracts a version-specific payload locally, then reuses it. First launch
  needs time and free space. Updates open a download for manual replacement.
- `Aperture-x86_64.AppImage` contains native image tools and their dependencies.
  Make it executable in a writable user-owned directory. Use
  `--appimage-extract-and-run` if FUSE is unavailable.

All editions use `com.neuroqp.aperture`. Preferences and image caches live outside
package directories. Linux uses XDG directories; Windows uses user-local app data.
The display cache has a 2 GiB soft limit. Saving annotations may require temporary
space proportional to the image size. Portable runtime folders remain by version
so another running copy keeps its files. After closing Aperture, obsolete folders
under `%LOCALAPPDATA%\NeuroQP\Aperture\portable` may be removed; the next launch
of that version extracts them again. Image caches and preferences are separate.

## Signing

Tauri updater signatures are configured. The public key is in
`src-tauri/tauri.conf.json`; `TAURI_SIGNING_PRIVATE_KEY` is a GitHub Actions secret.
The provisioning machine's backup is outside Git at
`~/.config/neuroqp-aperture-release/updater.key`. Keep a secure backup. Losing the
key prevents installed clients from trusting future updates. Do not casually
replace it or put it in logs, artifacts, reports or Git. Validation receives no
release signing secrets; only reviewed package jobs use them.

Windows publisher signing remains pending. Packages may show unknown-publisher
or SmartScreen warnings, and institutional policy can block them. When publisher
signing is configured, sign the app and final Windows bytes before creating Tauri
signatures and checksums. A valid publisher signature does not guarantee immediate
SmartScreen reputation. Workflow and signing-script changes require careful review.
