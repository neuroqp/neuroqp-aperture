# Release recovery, 14 October 2026

This records the stopped attempt, not acceptance of a new release. The table records the initial recovery inspection; later development rehearsals are recorded below.

| Item                      | Recovered state                                                                                                                                                                               |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Stable release            | `v0.1.1`, commit `b34a2de71298e6c74980e502655dbe8ac73e0632`                                                                                                                                   |
| Application PR            | [#15](https://github.com/neuroqp/neuroqp-aperture/pull/15), still open                                                                                                                        |
| Latest application fix    | `1aad73307af77634501b940af91951f737b30730`; serialize cache reads with publication                                                                                                            |
| Existing 0.2.0 packages   | Unsigned native run [38607561268](https://github.com/neuroqp/neuroqp-aperture/actions/runs/38607561268), exact source `8f36d602655d2ef23470bc15068b419b0abdf2ab`; these predate the cache fix |
| Application candidate/tag | No `v0.2.0` tag or application release exists                                                                                                                                                 |
| Source publications       | `source-v0.2.0-r2` and `source-v0.2.0-r3` are immutable prereleases, not application releases                                                                                                 |
| Remaining build jobs      | No running GitHub build jobs found                                                                                                                                                            |
| Test VM                   | Owned Windows QEMU exited on systemd SIGTERM at 18:56 UTC; QEMU, TPM processes and QA ports are absent. Guest graceful closure was not observed                                               |

The multi-instance changes, advisory save choices, staged-destination race fix and cache-read fix received independent GPT-6.1-Sol source review. GitHub Codex review exhausted its account allowance; the user approved independent review. At `8f36`, automated checks passed: 70 frontend tests, 69 Windows and 74 Linux Rust tests with actual workers, release/source helpers, formatting and Clippy. The backend changed afterward, so the full Rust/native cache regression results do not apply to `1aad733`. Unchanged frontend, annotation-writing and packaging observations can be reused with their input identities; they do not establish final-package acceptance.

Focused Debian native checks passed at `8f36`. Windows installation, registered TIFF opening, independent processes/caches, all advisory save choices, changed-source handling, natural closure and update admission observations passed. Independent TIFF decoding and encoded-tile comparison confirmed unchanged pixels. Windows then exposed a real cache-publication defect: a parent tile directory cannot be moved while a descendant tile file is open, even with delete sharing. A native Windows probe reproduced error 5; the fix adds short reads to the existing publication mutex. The exact-source old-fail/new-pass harness was reported by the implementation agent, but its `/tmp` files were lost during host cleanup. A recovered receipt labels that limitation; committed regressions and native validation remain required.

Helper failures were separate: Setup/portable inner-executable hashes were incorrectly assumed equal; PowerShell JSON-array and void-task results contaminated values; a dialog provider reported its OK control as a pane; the warm-cache comparison included temporary source aliases. Linux tooling also had a mount-path error and misclassified an application boolean as a WebDriver error. Their original failures remain preserved and are not passes. The installer/uninstaller admission case was incomplete when stopped. No further release was started during recovery.

## Measured historical timings

These are observations from old workflows, not proof that the new 30-minute target is met.

| Operation                               | Measured duration                       | Evidence                                                                            |
| --------------------------------------- | --------------------------------------- | ----------------------------------------------------------------------------------- |
| 0.1.1 signed candidate workflow         | 30m 57s, before desktop acceptance      | [38489812079](https://github.com/neuroqp/neuroqp-aperture/actions/runs/38489812079) |
| 0.1.1 signed Linux package step         | 5m 00s                                  | Same run, Linux job                                                                 |
| 0.1.1 signed Windows Setup step         | 11m 14s                                 | Same run, Windows job                                                               |
| 0.1.1 portable packaging step           | 5m 58s                                  | Same run, Windows job                                                               |
| 0.2.0 unsigned Windows Setup step       | 10m 25s                                 | Run 38607561268                                                                     |
| 0.2.0 portable packaging step           | 5m 36s                                  | Run 38607561268                                                                     |
| Unchanged prior Windows fixture rebuilt | 11m 52s wasted per rebuild              | Run 38607561268                                                                     |
| 0.2.0 Windows portable first extraction | About 5m 29s on the constrained test VM | Preserved guest timestamps, 18:18:10 to 18:23:39 UTC                                |

The old CI smoke steps took roughly one second on Linux and 63–69 seconds on Windows, but those worker/package checks were not the new native desktop smoke suite. New desktop smoke timings must be measured separately. Source assembly, lab preparation, queueing and desktop smoke can prevent a 30-minute release; an expired deadline must block publication.

## Stage A helper rehearsals during process repair

No final package, source archive, candidate or release was built or published during
this process repair. Rehearsals used unchanged published `0.1.1` packages and one
65-by-49 RGB TIFF, SHA-256
`951d023f7442080f97d8201cebffae609df2ac53a45cccd0389197e9bc0090c7`.
These are helper-development observations, not release acceptance.

| Rehearsal            | Measured result                                                                                                                                                                               |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Linux on Omarchy     | Native launch observation failed after 90.15 seconds; no rendering pass. A later native Close request produced launcher exit 0, which proves cleanup only                                     |
| Windows 11 Setup     | Standard-user install returned 0, installed marker and actual WebView2 process lineage passed. Overall helper failed after 63.01 seconds because it mishandled the empty pre-load image state |
| Windows parser/seams | Passed on the licensed standard interactive account before the final observer-watchdog changes; later changes require their own Windows regression result                                     |
| Windows portable     | No new native rehearsal performed                                                                                                                                                             |

The Linux rehearsal also found OS-ID and executable-permission preflight problems.
These and the Windows image-state failure were helper/environment defects, not
new application defects. Raw reports, hashes, screenshots and cleanup observations
remain under ignored `artifacts/native-smoke-rehearsal-linux` and
`artifacts/bounded-process-resume-lab-20261014`. Failed observations remain failures.

The native smoke suite has not yet passed on the complete supported matrix.
Reusable Linux desktop bases are not yet prepared. Readiness must block any new
release until those Stage A tasks finish. The new signed build and successful
post-build smoke durations are unmeasured, and the 30-minute target remains unproven.
