# Linux validation

Validated the real Tauri debug binary on October 8, 2026 using WebKitWebDriver,
the native GTK file chooser, and compositor screenshots. Hardware was an Intel
Core Ultra 9 185H with approximately 31 GiB RAM, Arch/Omarchy, and libvips 8.18.6.
The application was built with embedded frontend assets. Browser-only layout
checks additionally passed at 1280 by 800 and 640 by 400 without horizontal
overflow; the two bars measured 36 and 35 CSS pixels.
Eight Rust tests, seven frontend tests, production frontend build, Prettier,
rustfmt, and Clippy passed. The Ubuntu build and checks also passed in
[GitHub Actions](https://github.com/neuroqp/neuroqp-aperture/actions/runs/37781918443).
Final native rechecks used the embedded binary built from `4c89cc8`.

## Coverage

| Check                          | Actual result                                                                                                      |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| JPEG, PNG, BMP, WebP           | Native chooser, displayed image, dimensions, Zoom and Fit passed                                                   |
| RGBA PNG                       | Transparent half flattened onto neutral display background                                                         |
| EXIF orientation 6 JPEG        | Correct clockwise portrait display, 49 by 65                                                                       |
| Classic TIFF                   | RGB tiled and stripped, uint16 stripped and Deflate passed                                                         |
| BigTIFF                        | Generated uint16 tiled and real 2.53 GB uint16 stripped image passed                                               |
| Native TIFF pyramid            | SubIFDs with both ceil and floor reductions, odd edges passed                                                      |
| Multipage TIFF                 | First page displayed; second inverse-color page not substituted                                                    |
| Filename characters            | Greek, Chinese, spaces, `#`, `?`, and brackets passed                                                              |
| Corrupt TIFF and vendor QPTIFF | Errors displayed; next valid image recovered                                                                       |
| Save and placeholder tools     | Remained disabled                                                                                                  |
| Loading interaction            | Preview visible, Zoom and pointer drag responded during BigTIFF conversion; Cancel succeeded and next image loaded |
| Source integrity               | Source sizes and modification timestamps unchanged throughout native checks                                        |

## Measurements

Times start immediately before pressing Enter to accept the chooser's location.
They include key delivery and a fixed 0.5-second chooser confirmation check.
Chooser setup and path entry are recorded separately. "Cold" means no completed
Aperture display cache; operating-system file caches were not controlled.
RSS is the largest sampled sum for the app and its descendants,
sampled approximately every 150 ms plus WebDriver/measurement overhead. It can
double-count shared pages and is not a hard memory limit or GPU-memory measure.

| Image                                             | Application-cache state | Backend fully ready | Sampled process-tree peak RSS |
| ------------------------------------------------- | ----------------------- | ------------------: | ----------------------------: |
| RGB stripped TIFF, 9878 by 7410, 219.6 MB         | Cold                    |             3.150 s |                     599.6 MiB |
| uint16 Deflate TIFF, 8610 by 5879, 64.9 MB        | Cold                    |             3.014 s |                     589.8 MiB |
| Real RGB SubIFD pyramid, 9878 by 7410, about 3 MB | Cold                    |             1.932 s |                     548.0 MiB |
| uint16 stripped BigTIFF, 43054 by 29398, 2.531 GB | Cold                    |           167.399 s |                     730.8 MiB |
| Same BigTIFF                                      | Warm reopen             |             1.199 s |                     561.4 MiB |

One WebDriver launch reached interactive UI in 0.597 seconds; this includes
WebDriver setup and is not an isolated production cold-start benchmark. On a
separate cold BigTIFF opening, actual backend `previewReady` appeared at 5.538
seconds. A native screenshot at 6.842 seconds showed the image while detail
conversion continued, after Zoom and a 70 by 35 CSS-pixel pointer drag. Their
WebDriver action round trip took 0.285 seconds; this is not a frame-rate or paint
latency measurement. Cancellation reached the UI at 7.057 seconds; a following
PNG became ready in 0.834 seconds. Canvas pixel reads were blocked by browser
origin rules, so exact first-painted-pixel time was not measured. The inspected
6.842-second screenshot provides an observed upper bound in that run.

## Evidence and limits

Run `uv run scripts/collect-test-images.py --derive-pyramid` to prepare the local
corpus and `python scripts/native-smoke.py --help` for the native harness. The
small generated fixtures are tracked. Source links, metadata manifest, native
JSON measurements, and screenshots remain ignored under `test-data/`, including
`native-matrix.json`, `native-large.json`, `native-interaction.json`, and
`cancel-loading-zoom.native.png`. Vendor QPTIFF sources remain outside supported
formats even when a local link uses a `.tif` suffix. No source images were edited.

Maximize/Restore requests were ignored by this compositor, including for a
floating window. The installed Omarchy default window rule suppresses maximize
events for every class; the button label matched Tauri's reported state. This
environment therefore does not verify successful native maximize/restore.
Minimize was not exercised. The native minimum-size request was not honored by
the compositor, returning 922 by 950; the 640 by 400 layout
is verified in the browser only. The real Close button closed the app, with no
Aperture or libvips worker processes remaining afterward. Final verification on
the last embedded build passed: the corrupt TIFF showed a friendly error in
1.098 seconds, and the next real RGB TIFF became ready in 1.165 seconds. The
application was left open displaying that real image. `native-final.json`
records these final results.
Windows, macOS, GPU memory, hundreds-of-GB images, and frame-rate guarantees are
not validated.

## Pixel rendering and app icon

Nearest-neighbor rendering was checked with a 128 by 128 alternating-pixel PNG.
The native app displayed crisp enlarged blocks, and a browser canvas sample
contained only black and white values at pixel boundaries with smoothing disabled.
The selected six-blade aperture icon was verified in the native header and included
in a successful standalone debug build. The seven frontend tests and formatting
check passed. JPEG display tiles remain lossy; nearest-neighbor scaling does not
make them pixel-exact scientific data.

## Lucide toolbar and tool panels

The toolbar now uses Lucide icons plus a matching custom multi-point symbol.
Browser interaction checks passed at 1280 by 800 and 640 by 400: right-aligned
viewer controls, mutually exclusive tool panels, toggle-to-close, Escape with
focus restoration, slider changes and Reset, point-type and checkbox changes,
and retained options when switching tools. Both panels fit the smaller viewport
without internal scrolling. A loaded checkerboard canvas and its viewport zoom
remained identical after brightness and point-option changes; the point count
stayed zero.

Direct mouse interaction in the running Linux Tauri app verified both palettes
and a brightness slider change from 0 to 54 over a loaded microscopy TIFF.
Native screenshots are ignored under `test-data/brightness-panel.native.png`,
`brightness-adjusted.native.png`, and `points-panel.native.png`. These controls
only manage UI state: no image processing, annotation placement, or source-file
writes were added. Nine frontend tests pass, including display-range bounds and
independent adjustment state.
