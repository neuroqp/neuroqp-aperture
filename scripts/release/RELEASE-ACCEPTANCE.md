# Native validation and package smoke

Use with [RELEASING.md](../../RELEASING.md). Test application behavior in feature
PRs. A release reuses that accepted evidence by exact hashes and does not rerun
functional suites. Final smoke proves that the delivered package launches, renders and
closes normally. A typed approval, browser preview or forced cleanup is not a
native test result.

## Functional validation in feature PRs

Run automated frontend, Rust, release-helper and actual-worker regressions in CI
on Linux and Windows. Use the resulting release-profile development executable
for changed-area native checks. Desktop VMs need not repeat automated suites.

| Changed behavior   | Check in the feature PR                                                                                                             |
| ------------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| Viewing            | Relevant TIFF/BigTIFF formats, dimensions and pixels; navigation, pan, zoom and display controls                                    |
| Saving             | Annotation save/reopen; independently decoded pixels and encoded image data remain unchanged; errors preserve edits and destination |
| Unsaved work       | Real Save, Discard and Cancel branches                                                                                              |
| Lifecycle          | Replacement/cancellation leaves no stale results or workers; ordinary close exits normally                                          |
| Multiple instances | Independent images/edits/caches; changed-file choices; closing one window preserves others                                          |
| Updates/installers | Relevant admission, startup exclusion, signature/version rejection, preferences, shutdown, upgrade and failure paths                |

Expand codec, large-file, scratch-space, display/GPU and native-library coverage
when those inputs change. Reuse unchanged-path evidence with its original hashes
and limitations. Prior-version upgrade fixtures remain reusable while their
baseline, production key and transport/configuration remain applicable. Retain
fixture identities and deltas. A direct Setup install does not prove an upgrade.

The same native helpers support development validation. Linux identities add
`appdir_path` and `binary_sha256`, point `asset_path` at the tested AppDir's
`AppRun`, and hash that launcher in `asset_sha256`. Windows uses
`-Environment windows11`, `runtime_directory`, `version`, direct `aperture.exe`
`asset_path` and `binary_sha256`. Development identities need no package checksum
file. These reports have `operation: "validate"`. At release, the validation seal
needs only launch, small-TIFF render and normal-close reports for the exact
release executables; reuse matching development-binary reports.

Record source commit, tested areas, executable and dependency hashes, OS/user,
native backend, command, outcome and evidence paths. Keep fixtures and reports
outside Git. Never change a source microscopy image.

## Prepare desktop smoke

Prepare Windows 11 as a standard user, Debian 12 and Ubuntu 24 native Wayland.
Pin the small TIFF's SHA-256 and dimensions, observer/helper hashes and paths.
Rehearse the same transfer, native capture and close commands with a verified
executable. Verify a ready image state, actual pixels and normal process exit.
The observer must inspect the owned application window.

Run Python helpers with an interpreter from the
[command reference](RELEASE-REFERENCE.md). Use the existing native adapters, `smoke-desktop-windows.ps1` and
`smoke-desktop-linux.py`. Supply `source_sha`, `checksums_sha256`, exact asset path/hash and tiny-TIFF
path/hash/dimensions in `identity.json`. Linux identities also name `environment`
as `debian12` or `ubuntu24-wayland`, loopback `driver_url`, and the prepared
`screenshot_command` with `{output}` and `{pid}` placeholders. Windows identities
add `version`, `devtools_port` and `install_directory` for Setup, or
`portable_payload_directory` and `portable_payload_id` for portable.

Linux `linux-smoke.py` handles byte-stream transfer,
execution without replacing the desktop's environment:

```sh
python3 scripts/release/linux-smoke.py copy-check tiny.tif /prepared/tiny.tif returned.tif transfer.json -- ssh PREPARED_ARGUMENTS
python3 scripts/release/linux-smoke.py copy-to PACKAGE /prepared/PACKAGE SHA256 -- ssh PREPARED_ARGUMENTS
python3 scripts/release/linux-smoke.py run identity.json report.json --driver-plan driver-plan.json
```

Windows runs one native observer process with a 12-minute outer timeout:

```powershell
$helper=(Resolve-Path scripts/release/smoke-desktop-windows.ps1).Path
$identity=(Resolve-Path identity.json).Path
$report=[IO.Path]::GetFullPath('smoke.json')
$arguments='-NoProfile -STA -File "'+$helper+'" -Identity "'+$identity+'" -Environment windows11-setup -Report "'+$report+'"'
$observer=Start-Process powershell -ArgumentList $arguments -PassThru
if (!$observer.WaitForExit(720000)) { throw 'Native observer timed out; preserve the app and observer for diagnosis' }
if ($observer.ExitCode -ne 0 -or !(Test-Path -LiteralPath $report)) { throw 'Native smoke failed; retain its report and diagnostics' }
# Use -Environment windows11-portable with its separate identity/report.
```

The helper keeps its launch, render and close deadlines and writes one final
report. Read that report after the process exits. A timeout or missing report
fails the operation; preserve available diagnostics before retrying.

Use strict SSH host-key verification and an existing identity. Prepare owned
writable destinations. The Linux driver plan retains desktop variables in its `environment` object,
including the observed display, desktop bus and backend. Linux observers require a direct `WebKitWebDriver` listener on the same display
and desktop bus, native accessibility and screenshot capture. A `tauri-driver`
proxy used for upgrade testing is not that endpoint. Record observed window
backend; a `WAYLAND_DISPLAY` variable alone does not prove native Wayland.

If a prepared lab uses an idle guard, keep its supervised process and driver plan
through smoke, then stop only that owned guard and retain its closure result.
Never change host preferences or unlock a desktop silently to make a check pass.
Desktop or observer repairs require a successful rehearsal before reuse. Make
them outside a release; during one, follow the attempt limit below.

## Final package smoke

Verify package hashes and signatures first. Run exactly the four cases in
[RELEASING.md](../../RELEASING.md). Windows portable begins with the incoming
version's payload absent. All environments use the same tiny TIFF. Both Linux
cases use the same AppImage bytes. Make an owned AppImage executable before
launching; this does not change its SHA-256.

A pass requires a ready image without error, expected dimensions, observed image
pixels in the native owned window, normal close and actual exit code 0. Preview
pixels can remain after loading fails, so a screenshot alone is insufficient.
A crash, timeout, forced kill, unavailable desktop or inconclusive observation
is not a pass. Keep failures and diagnostics before retrying. Never weaken an
assertion or relabel a cleanup kill as normal closure.

Use one attempt and at most one bounded retry for an obvious setup issue, as in
[RELEASING.md](../../RELEASING.md#smoke-attempts-and-unverified-targets). Classify
the result: an observed application crash, error, wrong render or abnormal exit is
an application failure and blocks publication. Automation failure or an
unavailable OS leaves that target unverified with a recorded follow-up.

Retain source/package/fixture/helper hashes, OS/user/backend, start/end/duration,
screenshot hashes and process outcomes. Combine the four reports with
`linux-smoke.py collect`, as shown in the command reference. It accepts unrun
`unverified` reports only with `--accept-unverified`. Submit that compact result
to `publish.yml`; retain the raw reports separately. After a package or source
change, rerun all affected checks against the new hashes.
