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
viewer controls, the original single-panel behavior, toggle-to-close, Escape with
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

## Selected styling and independent panels

Light/dark styling and independently opened panels were checked in the browser
at 1280 by 800 and 640 by 400. Both panels fit side by side at the minimum size;
closing either leaves the other aligned right. Escape closes the focused panel
and restores its toolbar-button focus. Point selections persist across closing
and reopening. The theme button changes the palette and stores the choice.

The blue/medium/circle selection produced medium blue type choices, differently
colored medium circles, and blue circles in five sizes, with one selected button
per row. An orange/large/dot combination also passed. Brightness changed to 100
without changing the loaded checkerboard canvas; annotation count remained zero.
Eleven frontend tests pass, including choice coordination and marker geometry.

Native Linux mouse checks verified both panels together over a loaded microscopy
TIFF, changing the marker color to blue, and switching from dark to light. The
screenshots `test-data/point-options-dark.native.png` and
`test-data/point-options-light.native.png` remain local and ignored. Image
processing and point placement remain unimplemented.

## Display range and approximate histogram

Supersedes the earlier UI-only brightness prototype. Fourteen frontend tests
pass, including range clipping, the CSS transfer formula, histogram sampling,
percentile tails, constant images, and empty histograms. Prettier, TypeScript,
Vite, and the standalone Linux debug build pass.

Browser interaction verified two sliders only, default Linear, the upper-right
hover toggle, Log changing the histogram without changing the image, moving
cutoff lines, percentile selection, Reset, and disabled controls on a new load.
Light and dark layouts were inspected. Raw canvas pixels remained identical
through display adjustments; a screenshot confirmed visible range clipping.

Earlier native Linux checks in this change verified a preview histogram and a
visible 1–99 percentile adjustment on a real microscopy TIFF, selecting 28–153.
The native evidence is `test-data/histogram.native.png` and
`test-data/histogram-preset.native.png`. The final simplified panel and hover
interaction were checked in the isolated browser only, to avoid interfering
with the user's active desktop. Native verification of that final UI remains
outstanding. All screenshots and validation fixtures remain ignored.

Histogram values are an approximation from a preview downsampled to at most
256 by 256 pixels. They describe the 8-bit display luminance, not original
16-bit intensities. The renderer filters only the image canvas; file data and
cached tiles are not rewritten.

## Editable multi-point annotations and ImageJ TIFF copies

Eighteen frontend and sixteen Rust tests pass. Frontend checks cover immutable
point additions, style grouping, outside-image rejection, and pixel-center/flip
transforms. Backend checks cover real ImageJ ROI bytes, malformed metadata,
classic/BigTIFF in both byte orders, unknown metadata retention, atomic copies,
source/symlink/hardlink overwrite rejection, and pixel-identical PNG conversion.
Prettier, rustfmt, Clippy, TypeScript/Vite, and the standalone debug build pass.

`python3 scripts/check-imagej-interop.py /path/to/ij.jar --java-home /path/to/jdk`
passed against the locally installed ImageJ 1.54g. It generates synthetic TIFFs,
uses ImageJ's own encoders/decoders, edits through Aperture's ROI/TIFF modules,
reopens with ImageJ, adds another point, and reimports the saved result. It
checks exact pixel values, original source bytes, counters, subpixels, active
selection and overlay styles, ROI properties, calibration and image metadata.
The headless check bypasses ImageJ toolbar activation, which requires a GUI.
Its float-image case tests metadata interchange only; Aperture's viewer still
rejects floating-point images.

Browser interaction with the production frontend and mocked native IPC verified
imported marks, active-only placement, outside-image rejection, separate style
groups, counters, label/all-slice configuration, save payloads, and unchanged
marker screen size through zoom. T3 covered initial interactions; after its
explicit automation-host-unavailable response, isolated Chromium completed zoom
and pan checks. Validation harnesses remain ignored under `test-data/`.

A separate Linux Tauri/WebDriver instance verified native point placement over
a loaded image, marker rendering, counter updates, Save enablement, the native
Save dialog opening, successful native save command execution, and reloading the
saved TIFF with its marker. ImageJ independently reopened that native PNG-to-TIFF
output, verified all 3,185 source RGB pixels exactly, and added another point to
the editable selection. Native dialog filename entry and confirmation were
not automated; save-command tests and browser Save-flow checks cover those
surrounding paths separately. No global desktop mouse or keyboard injection was
used. Local screenshots include `test-data/points-native-added.png` and
`test-data/points-native-imported.png`.

TIFF copies retain their layout, so stock ImageJ's existing tiled-TIFF/BigTIFF
reader limits still apply. Movement, deletion, stack navigation, and large-point-
set performance benchmarks remain outside this change.

## Counter-group editing

Supersedes the new-points-only styling described above. Twenty-three frontend
and seventeen Rust tests pass. Counter edits update existing markers, preserve
other counters, and retain source metadata when mixed-counter ROIs split.
The expanded ImageJ check verifies names/properties, counters, coordinates and
slice positions through splitting, reordering, style edits and repeated saves.

T3 browser checks verified editing both existing counter-0 markers without
changing counter 1, recalling counter-1 controls, and saving the split groups
with their original metadata references. A separate native Linux Tauri instance
verified color, type, size and label changes, plus switching away and back to
restore group controls and counts. Screenshot `test-data/counter-groups.native.png`
is local and ignored. The review's loading-state issue was fixed by disabling
point controls until annotation loading completes. Empty groups keep preferences
for the current image only; Show on all slices still controls new-point positions.

## ImageJ counter semantics and themed controls

Supersedes Counter-group editing above. Counter now selects a category inside
the active point selection. Type, size and base color are shared by that
selection; label visibility also applies to point overlays. Default counter-0
selections show point indices; selecting a higher category switches to category
labels and ImageJ colors, retained when returning to 0. Aperture switches
immediately, without reproducing ImageJ's temporary missing-label behavior
before its per-point counter array is allocated.

Twenty-nine frontend and nineteen Rust tests pass, alongside Prettier, rustfmt,
Clippy, TypeScript/Vite and the native debug build. Regression coverage includes
overlay-only imports, preserved active/nonpoint selection roles, mixed-slice
positions, category labels/colors, shared styling and immutable point addition.
The installed ImageJ interoperability check passes after the metadata changes.

The Counter menu uses an app-themed HTML popover instead of GTK's native select
popup. A dedicated T3 browser tab verified actual clicks, keyboard selection,
Escape/Tab dismissal, and readable help/menu layouts at 640 by 480 in both themes.
Native Tauri/WebDriver checks verified category switching, placement, counts,
shared appearance, labels, and Save/reopen with one ROI containing both counters.
Save used the real frontend and Rust command with only the filename-dialog
response supplied by the harness; native filename entry was not exercised.
Local ignored screenshots include counter-menu-light.native.png,
counter-menu-dark.native.png, counter-help-light.native.png and
counter-help-dark.native.png under test-data/.

## Point placement, dragging, deletion and temporary panning

Thirty-five frontend and twenty-one Rust tests pass. The new model checks cover
category 99, immutable movement/deletion, subpixel edge positions, nearest and
topmost screen-space hit testing, and pan-over-edit precedence. Deleting the
last point leaves an empty active selection and does not promote earlier ROIs
in memory or on save. ImageJ interoperability now includes category 99 and
clearing all points while retaining an unrelated rectangle overlay.

Native Tauri/WebDriver interaction verified that points appear on mouse-down
before release, with pointer drift creating no extra point. Space and Ctrl each
pan the viewport without adding points or switching off Multi-point, then
restore placement on release. Hover/drag moved an existing category-99 point
without changing its style or count; Save/reopen retained its coordinates.
Alt-hover selected the minus cursor, and Alt-click removed the point. Saving
after deleting every point reopened without point annotations. The native Save
check supplied only the filename-dialog response; it used the actual frontend
and Rust save command.

The Counter dropdown exposes 0–99, scrolls to the current choice, and accepts
two-digit keyboard selection. Native light/dark screenshots verify the help icon
to its right and the explanation followed by ImageJ compatibility text. Browser
checks at 640 by 480 cover scrolling to 99 and popup bounds. Screenshots remain
ignored under test-data/: counter-99-menu-{light,dark}.native.png,
counter-updated-help-{light,dark}.native.png, point-dragged.native.png and
point-delete-hover.native.png. WebDriver screenshots do not include the mouse
pointer, so cursor checks inspect computed CSS and interaction state.

Targeted native checks also passed with toolbar focus retained before the first
canvas press, held modifiers, modifier changes during a drag, outside release,
and actual window focus loss. Cancelled drags did not resume or alter saved
coordinates. Pointer-cancel cleanup was exercised with a synthetic event;
hardware-generated pointer cancellation was not exercised.

## Keyboard placement, navigation and tool help

Thirty-six frontend tests pass, including the fixed 5 CSS-pixel hit radius and
keyboard placement guards. Native Tauri interaction verified 3× keyboard panning:
ArrowRight/D and ArrowUp/W taps move approximately 108 CSS pixels with
OpenSeadragon's navigation setting at 120 (its minimum tap spans nine pan frames,
compared with 36 pixels at the previous setting of 40).

An extra-large marker is draggable at 4 pixels from its center but not at 6 or
9 pixels. Separate E presses each add one point at the cursor. Space, Ctrl, Alt,
input focus, Pan mode and an active mouse gesture prevent keyboard placement.
WebKit/WebDriver did not emit repeated events for a held key; repeat suppression
was verified with a synthetic repeated keydown and the unit guard test.

Native light/dark screenshots verify the Multi-point header help. Escape hides
help while retaining the panel. T3 browser checks additionally cover keyboard
focus, outside dismissal and scrollable help at 640 by 320. Local screenshots
remain ignored under test-data/multipoint-header-help-{light,dark}.native.png.
No backend code or source images changed in this follow-up.

## Faster keyboard panning and full sampled intensity range

Thirty-seven frontend tests pass, including 0–100 histogram endpoints with rare
extremes, uniform images and empty histograms. Production build and formatting
pass. Native ArrowRight/D and ArrowUp/W taps now move 540 CSS pixels, five times
the previous 108, with the navigation setting increased from 120 to 600.

A generated RGB gradient spanning 40–180 verified that 0–100 selects Minimum 40
and Maximum 180 and updates the display filter. Reset restores 0–255 and the
identity filter. The source file hash remains unchanged. All five footer buttons
fit on one row inside the panel in both themes. Local screenshots remain ignored
under test-data/brightness-full-sampled-{light,dark}.native.png.

## Confirmed overwrite and Save as

Supersedes the earlier restriction against overwriting a TIFF source. Save and
Ctrl+S now offer Overwrite, Save as…, and Cancel for TIFFs. Keyboard navigation
is halved from the previous setting: 300 instead of 600, yielding 270 CSS pixels
per native tap. The frontend's 37 tests, production build and formatting pass.

Rust coverage includes authorized same-path replacement in classic TIFF and
BigTIFF, both byte orders, retained permissions, symlink targets, read-only
destinations, failed rename cleanup, and the classic TIFF 4 GiB offset limit.
New Save-as files use normal creation permissions even when the source is
read-only. Stable per-image ROI metadata references prevent later overwrites
from attaching the wrong metadata after deletion or reordering.

All 97 TIFFs from manual_counting_pascal/tiffs passed actual backend load,
annotation addition, same-path save, reopen, deletion/reordering/editing and a
second overwrite on disposable copies. The first save retained every original
byte except the first-IFD pointer and appended annotation metadata. Every
original's SHA-256, size and modification time matched before and after testing.
No format-specific failure was found in that set.

All 24 Rust tests, strict Clippy, rustfmt and the installed ImageJ interoperability
script pass. Native desktop checks verified Save/Ctrl+S, Overwrite without a
picker, Save-as destination recall, cancellation and write-failure dirty-state
retention, and both dialog themes. Repeated native overwrites after deleting an
imported point overlay retained calibration, Info, ROI names/properties and a
rectangle overlay; ImageJ independently verified exact pixels and edited points.

PNG saving created the matching sibling TIFF directly and prompted on an
existing destination. The original PNG hash and decoded pixels stayed unchanged.
A disposable copy of 1321_BLA_20X_DAPI_-1.16_bottom_left_512px.tif was opened,
annotated, overwritten through the dialog and reopened with its new point and
identical decoded pixels. Native Escape, Tab and Space operated the dialog while
the cursor remained over the viewer. The source file hash remained unchanged.
The native filename-picker response was supplied by the test harness; native
filename entry itself was not automated. Ignored screenshots include
test-data/save-choice-{light,dark}.native.png and
test-data/save-real-copy-reopened.native.png.
