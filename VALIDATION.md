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

## PR review regressions

Thirty-eight frontend and twenty-eight Rust tests pass. Ordered overlay tests
cover interleaved point/nonpoint ROIs, split groups, deletion, active-selection
demotion and repeated saves. The ImageJ interoperability script verifies the
same source-slot order and ROI metadata after export and reopen.

The TIFF reader checks the combined 32 MiB tag budget before reading either
payload, and both reader and writer bound item/container allocation. Sparse
fixtures cover oversized combined tags and millions of zero-length items;
ordinary empty items and a large individual ROI still round-trip.

Production-browser regression checks verified that save errors survive six
loading polls with dirty annotations and progress intact, then clear on retry or
opening another image. Counter controls disable during saving without adding
disabled properties to rendered SVG markers. Unit coverage includes opaque,
translucent and transparent imported ARGB colors.

Native keyboard checks verified that Tab-focused toolbar and Counter buttons
ignore E and retain Space activation while the pointer stays over the image.
Mouse selection of tool, type, size or Counter returns focus on canvas entry,
allowing E placement without an extra click; keyboard and typing focus remain
intact. Save-dialog Escape and Tab/Space cancellation retain dirty edits.
Imported TIFF marker and label attributes retain half alpha and zero alpha,
including the black label halo.

## Color connections (October 9, 2026)

The Color implementation passes 46 frontend tests and 32 Rust tests, a production
frontend build, an embedded Tauri debug build, Prettier, rustfmt, and strict
Clippy. Pixel tests cover additive many-to-many routes, independent transfer
settings, custom RGB coefficients, identity/reset behavior, and immutable input
planes. Backend tests decode five-band and five-channel ImageJ TIFF fixtures,
check distinct 16-to-8-bit PNG samples, warm-cache reuse, bounded asset paths,
unsupported layouts, and unchanged source bytes.

Native WebKitWebDriver checks used the embedded Linux app, not a browser preview.
Generated grayscale16, RGB, five-channel ImageJ and five-band TIFFs displayed
successfully. Actual canvas pixel reads verified rerouting, multiple inputs to
one output, channel-5 isolation, empty connections producing black, custom hex
colors, rapid slider changes, exact reset at an unchanged viewport, and recoloring
retained after zoom/pan/Fit. Source-file SHA-256 hashes and existing PNG/JPEG cache
contents stayed unchanged across color edits. Annotation SVG content stayed
unchanged when the image colors changed. Light/dark panels and the compact picker
were inspected in native screenshots. A native wire-drag followed by click
activation exposed stale click suppression; the fix passed the same sequence.

Cancellation during metadata loading recovered to a valid image. A five-channel
2048-by-2048 image was recolored while detail was loading, then replaced by RGB;
late requests did not corrupt the new image or report stale errors. These
focused checks invoked the normal native loading command and restored frontend
state. A separate final-build smoke check opened the five-channel ImageJ TIFF
through the real GTK file chooser, displayed it, and passed Zoom/Fit and source
integrity checks (`test-data/color-native-chooser.json`).
Local JSON and screenshot evidence stays ignored under `test-data/color-*`.

Coverage is limited to the supported TIFF layouts and these generated fixtures.
OME mapping, planar-separate multiband TIFFs, scientific alpha layouts, and
single-directory contiguous ImageJ hyperstacks report explicit errors. Color
uses 8-bit display samples; original 16-bit pixels remain intact. The original CPU compositor used UI-thread tile invalidation; the GPU follow-up
below replaces that path when WebGL2 is available. Display range still samples the original preview histogram
(first channel for scientific channel TIFFs), not the recolored composite.

## GPU color rendering (October 9, 2026)

Color now uses a custom OpenSeadragon WebGL2 drawer. Source samples are uploaded
once per tile-cache lifetime. Color edits upload a 256 × 32 RGBA float lookup
(128 KiB), then replay the last displayed tile geometry in a coalesced microtask.
Normal OpenSeadragon frames still select tiles, advance blending and update
camera transforms. Evicted textures invalidate replay; unavailable/lost GPU
contexts recover through the existing CPU compositor. No wgpu, new dependency,
Rust loading change, or pyramid-conversion optimization is included.

The first GPU implementation waited for the next OpenSeadragon frame. Native
measurement exposed that scheduling delay, so the second iteration added cached
geometry replay. On the same pre-existing 9,878 × 7,410 RGB SubIFD pyramid and
2,832 × 1,558 drawing surface (DPR 2), measured results were:

| Renderer                              | Input to viewport submission, median / p95 | Next-frame opportunity, median / p95 |
| ------------------------------------- | ------------------------------------------ | ------------------------------------ |
| CPU baseline, 30 edits                | 576 / 625 ms                               | 576 / 625 ms                         |
| GPU, scheduled OSD redraw, 20 edits   | 29 / 30 ms                                 | 60 / 60 ms                           |
| GPU, cached geometry replay, 40 edits | 2 / 2 ms                                   | 27 / 27 ms                           |

The replay run observed GPU completion at 4 / 6 ms (median / p95, including
fence polling). All three runs used the fitted view after loading. The final CPU
and GPU runs recorded zero tile fetches; the GPU run additionally recorded zero
2D pixel readbacks or source texture uploads. CPU recoloring made 3,030 readbacks
across its 30 measured edits. GPU frame intervals were 33 / 34 ms rather than the
CPU baseline's 34 / 589 ms (median / p95). These native runs were unfocused and
WebKit ran animation frames near 30 Hz. Neither submission, a fence, nor the
next animation frame proves physical display presentation or 60 Hz performance.

Run the reproducible native benchmark against an already-open, fitted pyramid:

```sh
python3 scripts/native-color-benchmark.py --session test-data/color-session.json \
  --output test-data/gpu-benchmark.json --trials 40 --warmup 8 --gpu-fence
```

The session file contains the WebKitWebDriver `sessionId`; the driver defaults
to localhost:4444. Navigate the native webview to `tauri://localhost/?colorBenchmark`
for viewport diagnostics and sustained validation. Add `colorRenderer=cpu` for
the previous CPU path. `--sustained --trials 120 --drag-interval-ms 16.6667`
requests a continuous input stream and reports the cadence actually achieved,
including coalesced inputs and a final-setting framebuffer check. Timing uses
the real Color input handler; framebuffer readback is a separate correctness
pass. GPU fences are optional because polling itself adds measurement overhead.
Raw JSON, screenshots and generated microscopy fixtures remain ignored in
`test-data/`; source test images are never modified.

A prebuilt 513 × 385, 32-channel uint16 pyramid exercised all channels on the
same drawing surface, including one-pixel-wide edge tiles. Low-intensity output
colors kept all contributions visible instead of saturating the composite.
After all display tiles were resident, 40 edits submitted at 1 / 2 ms and GPU
completion was observed at 9 / 11 ms (median / p95). An integer `texelFetch`
shader iteration changed the median completion observation from 10 to 9 ms,
with no p95 improvement; this small difference is not evidence of a substantial
speedup. Its explicit RGB branch also bounds vector indexing. The comparison
used a shader replacement in the running native WebGL context to retain exactly
the same source textures; the final embedded build was then rebuilt and checked.

A separate 120-input sustained run on that settled 32-channel view submitted all
120 changes, matched the final setting's reference framebuffer, and recorded
zero fetches, source uploads or pixel readbacks. Submission was 1 / 2 ms and the
next-frame opportunity 17 / 32 ms. Requested input spacing was 16.67 ms; WebKit's
unfocused event loop achieved 30 / 30 ms. This does not establish 60 Hz dragging.
The 32-channel fixture is a rendering stress test, not proof of large-file load
performance. Larger five-channel runs were excluded from settled-render claims
because their tile caches were still loading.

A native shader check compared 19,080 screen samples against the actual cached
32-channel planes and found zero byte differences. Comparing against the original
synthetic TIFF instead exposed some zero-valued decoded display samples; these
zeros already existed before GPU upload. That conversion discrepancy is deferred
with the loading/conversion work and must not be described as fixed here.

The final embedded Linux build passed 53 frontend tests, production/Tauri builds
and formatting. Native checks verified real pointer dragging changes pixels,
brightness/contrast and custom colors, exact Reset and return-to-Fit, black with
all routes removed, and CPU recovery after forced `WEBGL_lose_context` with
custom settings retained. EXIF orientation 6, grayscale16, the 2,051 × 1,025 odd
pyramid and the existing alpha-image display path were compared against CPU
rendering. GPU horizontal flip matched the exact mirrored framebuffer. A final
40-edit RGB run reproduced 2 / 2 ms submission and 4 / 6 ms observed completion
with only lookup uploads; a 120-input RGB run also retained the final setting.
Independent renderer and benchmark reviews found no remaining blockers. Rust
source and image conversion are unchanged by this GPU follow-up.

## Connection display ranges and compact panel

Connections now use the same minimum/maximum transfer and percentile presets as
Display range, replacing brightness/contrast. Histograms sample original input
channels once per preview, before tinting or mixing. Scientific channel
histograms reuse the decoded preview planes; ordinary RGB/gray histograms share
the existing preview histogram read. Slider edits do not resample histograms.

All 56 frontend tests, formatting and the embedded Tauri build pass. Native RGB
checks verified independent connection ranges, isolated presets/reset, unchanged
global Display range settings, exact pixel restoration after Reset, and that
Linear/Log changes the histogram without changing pixels. Native five-channel
checks matched the first and fifth channel histograms to their decoded preview
planes exactly. Light and dark screenshots verified the 350 px panel, centered
SVG arrow, and curve-following selection/focus glow in the current wire color,
including a custom cyan shade. Focused wires have no rectangular outline.

The updated maximum-slider benchmark on the settled 9,878 × 7,410 RGB pyramid
submitted frames at 2 / 3 ms and observed GPU completion at 5 / 6 ms, median / p95
over 30 edits. It recorded only lookup uploads, with zero source uploads,
fetches or 2D readbacks. Presentation timing retains the limitations above.
Native evidence remains ignored under `test-data/connection-range-*`.

## First preview loading (October 9, 2026)

The cache manifest restores dimensions, orientation, channels and native levels
before launching readers. Manifest-less caches migrate through metadata discovery
without rebuilding display pixels. Native `image-state` events deliver revisions
immediately, with the 250 ms poll retained for recovery. Ordinary pyramid loading
probes a suitable preview level first, then discovers remaining levels and prunes
the cache after publishing the preview. Scientific channel identification remains
before decoding to preserve channel/page semantics. Native detail requests wait
until discovery is complete.

The final Linux embedded debug binary passes 62 frontend tests, 35 Rust tests,
TypeScript/Vite, Prettier, rustfmt and strict Clippy. Native WebKitWebDriver checks
cover preview draws, cache reopen, five-channel metadata/rendering, cached pyramid
Zoom/Fit, cancellation before preview and during detail, replacement during
decoding, and corrupt-file recovery. The real SubIFD preview remains 617 by 463
in both versions. SHA-256, size and modification time checks pass for all five
real benchmark sources.

### Native latency comparison

Both versions use embedded assets and the real frontend, Rust commands, asset
protocol and GPU drawer. Baseline is main at `e7e3b1c`, including channel/GPU
rendering. Time starts when the harness supplies the accepted filename response
to Open and ends after the drawer submits a decoded whole-image preview. Only
the picker response is supplied; filename entry and picker interaction are
excluded. This measures native rendering submission, not physical compositor
presentation. A following animation frame and framebuffer sample are recorded.
The 10 ms diagnostic state poll is only an approximate backend timestamp.

Medians use five fresh and five completed-cache reopens per ordinary input.
Fresh means the completed app cache was cleared; OS caches were uncontrolled.
The app was already launched, the native window was unfocused, and no competing
builds or decoding ran during timed comparisons.

| Input                                           | Fresh baseline / updated | Cached baseline / updated |
| ----------------------------------------------- | -----------------------: | ------------------------: |
| 65 by 49 PNG fixture                            |             295 / 225 ms |               294 / 53 ms |
| 0.92 MB PNG, 1024 by 1024                       |             312 / 266 ms |               315 / 83 ms |
| 219.6 MB stripped RGB TIFF, 9878 by 7410        |             809 / 523 ms |               307 / 64 ms |
| 64.9 MB Deflate uint16 TIFF, 8610 by 5879       |            1072 / 765 ms |               337 / 74 ms |
| 3.1 MB RGB SubIFD pyramid, 9878 by 7410         |            1325 / 292 ms |              1091 / 69 ms |
| 2.53 GB stripped uint16 BigTIFF, 43054 by 29398 |           3370 / 2971 ms |      not compared / 56 ms |

The GB baseline has three preview-only trials, the update five. One further full
conversion verified completed-cache reuse, followed by five warm-only reopens.
The first baseline GB trial took 5718 ms, versus 3110 and 3370 ms afterward;
uncontrolled OS caching prevents attributing all of that difference to this PR.
An uncached flat GB overview still takes about three seconds. Cached GB samples
were 227 ms on the first open in a new app session, then 94, 56, 53 and 48 ms.

Small five-band and five-page ImageJ fixtures take 1869 and 2127 ms fresh and
100 and 99 ms cached over three trials each. Baseline single trials were
2684/607 and 2855/849 ms, fresh/cached. These establish correctness and direction,
but are not equivalent five-trial comparisons.

### Iteration and remaining limits

Standalone backend notification measurements over ten warm trials reached
0.57 ms for PNG, 0.81 ms for the native pyramid, and approximately 1.01 ms for
five-channel files. Preview asset reads were approximately 0.03 ms. Native warm
latency now mostly comes from IPC, small display-asset decoding/upload and frame
scheduling. Further cache lookup/notification tuning has little headroom.
Shortening subprocess checks from 10 to 2 ms helped command-heavy channel loads;
the native results above use the final interval. An early iteration chose an
overly small pyramid level; it was corrected before the final comparison.

Fresh ordinary images still incur executable startup and decoding. Scientific
previews run several sequential commands per channel. Flat TIFFs without an
overview require substantial source scanning/downsampling. Potential follow-ups
are independent persistence of a preview when detail is cancelled, combining
channel decoding operations, and a deliberately partial first view for flat GB
files. These are not implemented. Preview caches currently become reusable only
after complete display-cache promotion succeeds.

The preview workspace has a 1 GiB watchdog; the root resumes its 2 GiB watchdog
after pruning. Existing cache plus workspace may transiently approach 3 GiB,
plus writes between 500 ms checks. Neither bound is instantaneous. This changes
the previous pre-preview root-budget behavior and avoids recursively walking
every cached tile before first pixels. Active caches are pinned against eviction.

Run `python3 scripts/benchmark-loading.py --help` for the reusable native benchmark.
Start `tauri-driver` with a dedicated `XDG_CACHE_HOME` under the output directory,
for example ports 4445 and 4446. Ignored evidence under `artifacts/` includes
`baseline-final-native.json`, `final-native.json`, `baseline-big-native.json`,
`final-big-native.json`, `final-big-complete-native.json`, `final-big-warm-native.json`,
`final-channels-native.json`, screenshots and `final-loading-regressions.json`.
Windows/macOS, release-build timings, cold OS caches, peak RSS and physical
presentation latency were not measured in this follow-up.

## TIFF worker and targeted Rust previews, October 9, 2026

This follow-up supersedes the previous section's pending shared-channel decoder
and missing RSS measurements. TIFF operations now use Rust libvips 2.3 bindings
in one image-scoped child, with native handles confined to its main thread.
Metadata and subsequent native tile operations reuse compatible source handles;
sequential loaders reopen after a consumed sink. Channel previews materialize
each resized source page once before extracting its bands. `tiffinfo` remains
a separate metadata-only process. Non-TIFF images retain the CLI path.

Large ordinary uncompressed gray16 and RGB8 sRGB TIFFs use a positional-read Rust
area reducer for their overview, with at most eight available CPU threads. The
64-megapixel threshold limits the changed filter to images with measured benefit.
Every source pixel contributes; input precision is retained until the existing
display conversion. Classic/BigTIFF and both byte orders are supported. The
parser falls back for compression, tiles/SubIFDs, multipage/channel layouts,
alpha, nondefault orientation/FillOrder/predictor and incompatible sample types.
Malformed reads fail. This overview deliberately differs from Lanczos; native
detail and generated full-resolution pyramids still use libvips. The display
cache key changes once to avoid reusing earlier overviews. Sources are untouched.

### Strategy selection

Standalone optimized Rust process medians for the 2.53 GB gray16 TIFF, three
trials each, were 2329 / 1142 / 633 / 542 ms with 1 / 2 / 4 / 8 threads. Peak
RSS was approximately 14 / 16 / 24 / 40 MiB. Thread outputs were identical.
The initial unspecialized Rust loop took about 5350 ms and was rejected.
The selected loop specializes unsigned16 accumulation and batches positional
reads while partitioning the global output grid into independent row ranges.

Native gray16 first-draw medians were 956 ms with four threads and 752 ms with
eight, five trials each. Eight was selected from the tested budgets. The
existing libvips thread experiment at 2 / 4 / 8 showed no substantial flat-GB
gain; adding libvips threads alone does not remove its scanline/filter cost.

For the 219.6 MB RGB TIFF, decode-plus-JPEG time in an initialized worker was
261 ms through libvips, versus 771 / 404 / 239 / 168 ms through Rust with
1 / 2 / 4 / 8 threads, three trials each. Four offered little benefit; eight
was checked in the native app before selecting its route. Sparse nearest
sampling was rejected: although about 38 ms process time on the GB file,
its high-byte error against full-area reduction reached 174/255, p99 39.
There is no sampled production branch.

### Native measurements

Hardware remains Core Ultra 9 185H, 31 GiB RAM, Arch, libvips 8.18.6. Embedded
frontend debug binaries use optimized Aperture Rust code and unchanged dependency
profiles. The baseline is the preceding `ab6cac4` build. The native WebDriver
harness supplies the chooser's accepted result; IPC, decoding, asset fetches and
the GPU drawer remain real. Time begins at accepted selection and ends at a
draw submitting a complete image level. It is not physical display presentation.
The harness also accepts a complete detail level if it arrives before the
preview; reported runs drew previews. No completed app cache was reused for
fresh trials. OS page caches were uncontrolled, so these are local, app-cache
fresh measurements, not cold-disk or network-storage guarantees.

| TIFF input                              | Previous first draw | Selected first draw | Fresh trials, previous / selected |
| --------------------------------------- | ------------------: | ------------------: | --------------------------------: |
| RGB8 stripped, 219.6 MB                 |              488 ms |              362 ms |                             3 / 5 |
| Gray16 Deflate, 64.9 MB                 |             1060 ms |              684 ms |                             3 / 3 |
| RGB SubIFD pyramid, 3.1 MB, 9878 × 7410 |              358 ms |              184 ms |                             3 / 3 |
| Five-band interleaved, 0.80 MB          |             2832 ms |              237 ms |                             3 / 3 |
| Five-page ImageJ channels, 0.80 MB      |             2067 ms |              246 ms |                             3 / 3 |
| Gray16 stripped BigTIFF, 2.53 GB        |             2957 ms |              752 ms |                             3 / 5 |

The GB trials cancel after the overview, excluding full-pyramid completion from
first-preview timing. Other trials finish and then test cache reopen. Four-thread
and selected eight-thread GB overviews were both 1024 × 699. RGB remains
1024 × 768, and native-pyramid preview remains 617 × 463. Earlier measurements
vary with the desktop workload; these small repeated series show the gain on
this machine and should not be read as universal codec/storage guarantees.

RSS is sampled every 50 ms through first draw, summing the validation driver's
native app, WebKit and decoder descendants. Shared pages can be counted more
than once, and brief peaks can be missed; this is not unique physical memory or
a hard ceiling. GB baseline process-tree peaks were 703–706 MiB; selected eight
thread peaks were 566–618 MiB, decoder 94–154 MiB. RGB selected process-tree peaks
were 572–601 MiB, decoder 100–104 MiB. The persistent worker slightly increases
some small-image peaks compared with the CLI baseline, while avoiding GB-scale
allocation. Standalone Rust's ~40 MiB is not the complete application's memory.

### Correctness and lifecycle

44 Rust tests against the production worker executable and 62 frontend tests
pass, along with frontend build, Prettier, rustfmt and strict Clippy. Coverage
includes channel-plane intensity/high-byte checks, explicit ImageJ first-Z/T page order,
ordinary multipage behavior, native pyramid/odd-edge geometry, unsupported
OME/alpha layouts, both TIFF endian/directory variants, RGB noncontiguous strips,
fractional full-area coverage, and source/cache fingerprint validation. A
binding regression test compares gray16-to-JPEG display pixels at scale 1.
Separate binding checks compared every decoded preview PNG pixel with the CLI
for all five bands and all five ImageJ pages; the ignored reproducer is
`artifacts/worker-check/check.py`.
The binding's `get_coding` incorrectly calls the format getter in 2.3.0; the
worker reads actual coding metadata instead. Native images use thread-local
`Rc` handles, avoiding the binding's shallow native-pointer `Clone`.

Native checks passed cancellation before GB preview and during detail,
replacement with PNG, corrupted-file recovery, cached pyramid Zoom/Fit and
five-channel Color rendering. Cancellation reached the reported cancelled
state in about 24 ms and its decoder PID was reaped before replacement completed.
Malformed/oversized protocol replies and unexpected EOF also reap the child.
Completed generated pyramids release their decoder; native pyramids retain it
for on-demand tiles. Source size, mtime and SHA256 checks remain unchanged.
No generated data or native screenshots are committed.

One preliminary fast-small measurement reached backend ready without a detected
preview draw and was excluded, rather than inventing a time. A standalone five
trial rerun and the complete repeated native matrix passed. The harness now
checks complete level coverage and retains screenshots on timeout. Two transient
WebDriver connection closures are retried only for inspection, never image-open
mutations. Timings use the drawer's own timestamp, not inspection return time.

### Remaining ceiling and next work

Flat TIFFs still require source reads; compressed TIFFs still require codec
work. Full-resolution generated pyramids remain separate background work after
preview publication. More libvips threads did not solve flat TIFF latency.
The current Rust route stops at eight threads; larger budgets were not measured.
Cold storage, network disks, unusual metadata and unsupported layouts remain
outside the fast-path benchmark. A useful next experiment is independently
scheduled compressed strips with bounded buffers and area reduction, preserving
predictors/channel semantics. For small images, pre-initializing a spare
killable worker could remove library startup from the click path. Measure both
before adding dependencies or changing the reader's supported formats.

Repeat with `scripts/benchmark-loading.py`, a dedicated `XDG_CACHE_HOME` and
`--driver-pid` for RSS. Use a separate D-Bus session when the normal app is open,
so its single-instance guard and session are unaffected. Local ignored evidence
is under `artifacts/worker-native/` and `artifacts/raw-strategies/`.

## Parallel compressed strips and pyramid conversion, October 9, 2026

This follow-up starts from merged PR #6, commit `610c8fa`. It changes new-file
decoding and pyramid conversion. Workers still start on demand when an image is
opened. Existing completed display caches are not used in the fresh trials.

### Selected implementation

Ordinary single-directory unsigned grayscale and RGB8 sRGB TIFFs of at least
16 megapixels can use the existing full-coverage area reducer for uncompressed,
LZW and Deflate strips. Each compressed worker owns an independent libtiff 4.5+
handle, with mapping disabled. LibTIFF reverses predictors and returns native
endian samples. Gray16 stays 16-bit until the existing preview JPEG conversion.
The reducer uses eight available threads below 256 megapixels, up to sixteen
above that threshold. Uncompressed reads remain capped at eight. This preview
filter differs from libvips' Lanczos fallback; it includes every source pixel.

Compressed and decoded strips are each capped at 16 MiB. Each native handle has
a 16 MiB single-allocation limit. All declared directory-field payloads,
including ignored private tags, count toward a 4 MiB metadata limit before
opening handles. This last guard addresses a reproduced amplification case:
eight 4 MiB private fields retained 36.4 MiB in one native handle despite its
single-allocation limit. Unsupported layouts and larger strips/metadata fall
back to libvips. Corrupt eligible strip data fails instead of displaying it.
Existing pyramid selection, multipage/channel mapping and orientation rules
remain in force.

Pyramid sinks temporarily use up to eight libvips threads, restoring the normal
two-thread preview/native-tile budget afterward. RGB8 sRGB uses direct JPEG
region encoding at quality 85, eliminating a separate graph for every tile.
Its native `.jpeg` tile names and older `.jpg` cache names both work through the
existing asset URLs. Gray16 retains its shrink-then-convert order. Scientific
channel pyramids now lazily extract, normalize and write PNG tiles in one graph,
avoiding the full-resolution raw, normalized and final plane intermediates.
The 2 GiB cache watchdog, process cancellation and source preservation remain.

### Strategies tested

Warm-file process medians for the generated 1.58 GB Deflate TIFF, three trials
per budget, were 13.459 / 6.836 / 3.513 / 2.163 / 1.506 / 1.599 seconds for
1 / 2 / 4 / 8 / 16 / 22 preview threads. Sixteen was selected. At sixteen,
libdeflate plus a Rust predictor pass took 1.249 seconds for decoding, versus
1.268 seconds through independent libtiff handles. LibTIFF was selected for
equivalent measured speed, existing availability and LZW/predictor coverage.
The final production libtiff reducer took 1.482 seconds in a separate check;
its preview-byte checksum matched the earlier decoder experiment.

For the 76.6 MB LZW derivative, production preview medians at 1 / 4 / 8 / 16
threads were 935 / 268 / 187 / 240 ms. Smaller inputs retain eight threads.
For the original 64.9 MB Deflate image, initialized-process decoding at
1 / 2 / 4 / 8 threads took 301 / 152 / 82 / 47 ms with libdeflate and
306 / 159 / 81 / 48 ms with libtiff.

The initial RGB pyramid experiment took 1.853 seconds with the previous suffix
and two threads, versus 0.472 seconds using direct JPEG and eight threads.
All 1,548 tiles, 292.8 million decoded channel samples, matched exactly.
These screening runs overlapped corpus generation; the native quiet repeats
below are the final performance evidence. Gray16 direct encoding was rejected:
it changes conversion order and produced a maximum decoded JPEG difference of 16. High-byte disk staging was rejected for the same ordering change; preserving
full uint16 staging would exceed the large-image cache budget. No staging path
or sampled-pixel preview was shipped.

### Native measurements

Hardware is Core Ultra 9 185H, 22 logical CPUs, 31 GiB RAM, local SSD, Arch,
libvips 8.18.6 and libtiff 4.7.2. Embedded frontend debug binaries use optimized
Aperture Rust code. The baseline is merged PR #6. The native WebDriver harness
supplies the accepted chooser result; Rust IPC, reading, assets and GPU drawing
are real. First draw ends at submission of a complete preview, rather than
physical display presentation. Ready time ends at backend pyramid completion.
Each table value is the median of three fresh-app-cache trials with filesystem
pages uncontrolled. The large selected trials use separate native sessions.

| TIFF input                                                                  | Previous first draw | Selected first draw | Previous ready | Selected ready |
| --------------------------------------------------------------------------- | ------------------: | ------------------: | -------------: | -------------: |
| Gray16 Deflate, 1.58 GB file / 2.53 GB decoded, predictor 2, 64-row strips  |            14.170 s |             1.769 s |       37.943 s |       18.797 s |
| Gray16 Deflate, 64.9 MB file / 101.2 MB decoded, predictor 1, 15-row strips |             0.719 s |             0.329 s |        1.672 s |        0.874 s |
| Gray16 LZW, 76.6 MB file / 101.2 MB decoded, predictor 2, 64-row strips     |             1.067 s |             0.397 s |        2.304 s |        1.287 s |
| RGB8 uncompressed, 219.6 MB                                                 |             0.413 s |             0.410 s |        1.866 s |        0.778 s |
| Five interleaved ushort channels, generated 33.5 MB, 2048 × 1638            |             0.634 s |             0.660 s |        1.099 s |        0.867 s |

Native pyramid reuse still showed a preview in 226 ms, and the five-page ImageJ
fixture in 274 ms, three fresh trials each. Channel previews were not changed;
their measured small difference is noise, while completion improved. The
large compressed input is an owned BigTIFF derivative of the real 2.53 GB gray16
source. The smaller LZW derivative uses the original 64.9 MB Deflate source.
Neither original was changed. The channel derivative was generated from the
small fixture, with its extra planes explicitly marked unspecified channels.

Source-local page eviction was also tested on the large Deflate file using
read-only `posix_fadvise(DONTNEED)`. `fincore` reported zero resident bytes before
each of three native sessions; preview draws were 2.511 / 2.370 / 2.273 seconds.
The median is 2.370 seconds. These tests cancel after preview and do not measure
full conversion with evicted pages. They do not control SSD firmware caches,
network storage or all system memory. The reusable harness exposes
`--fresh-only`, `--evict-source-pages` and backend ready timings.

RSS was sampled every 50 ms through preview and completion. On the large file,
previous decoder peaks were 241–247 MiB versus selected 258–260 MiB; native
driver process-tree sums were 727–765 MiB versus 728–750 MiB. RGB decoder peaks
rose from 104–105 MiB to 197–216 MiB with the larger encoding budget. These are
sampled RSS values, with shared pages counted across processes, not hard limits.

One initial mixed trial hit a WebKitWebDriver connection reset after completing
the large conversion; the repeated separate sessions succeeded. Inspection and
idempotent cancellation may retry transport resets, while Open is never
replayed. An old cached-reopen trial delayed drawing despite prompt backend
readiness; cached reopen results are excluded from this new-file assessment.

### Correctness and remaining ceiling

All 48 Rust tests passed with the production worker, including 96 combinations
of Classic/BigTIFF, endian order, grayscale/RGB, 8/16-bit, LZW/Deflate and
predictors 1/2. Noncontiguous strips, final short strips, actual sixteen-thread
fractional reductions, corruption, strip bounds and metadata fallback are
covered. Direct RGB JPEG and fused channel PNG pixels match the previous graph
at every tested pyramid level. Strict Clippy, Rust formatting, 62 frontend tests,
Prettier and the frontend build passed.

Native checks passed cancellation during compressed decoding, worker reaping,
replacement, cancellation during detail conversion, error recovery, cached
native zoom/fit and five-channel Color rendering. Cancellation was observed in
47.3 ms before reopening a PNG; old workers disappeared. Screenshots were
inspected. Source hashes, sizes and modification times matched the previous
integrity records, including all seven unrelated original-checkout files.

The approximately one-second GB goal is not reached. Full-coverage decoding is
still the preview floor: sixteen threads beat eight, and twenty-two added no
benefit. Source I/O raised the native preview median from 1.769 to 2.370 seconds
after page eviction. Full gray16 pyramid generation still decodes strips through
libvips' sequential reader and rereads after the preview. Its eight encoding
threads cut overall completion, but do not make source decompression parallel.
A next experiment would feed a bounded pool of parallel decoded uint16 strips
directly into a libvips image generator, retaining shrink order and avoiding
full-image staging. A progressively filled preview would be a separate product
choice if the first visible result must arrive before a full decode completes.
Oversized compressed strips, unsupported layouts, release binaries and
Windows/macOS remain unmeasured by this fast-path benchmark.

Local ignored evidence is under `artifacts/compressed-native/` and
`artifacts/pyramid-strategies/`. Repeat with `scripts/benchmark-loading.py` and a
dedicated cache/D-Bus session. The relevant native behavior is documented in
[libtiff strip reads](https://libtiff.gitlab.io/libtiff/functions/TIFFReadEncodedStrip.html),
[libtiff open options](https://libtiff.gitlab.io/libtiff/functions/TIFFOpen.html) and
[libvips dzsave](https://github.com/libvips/libvips/blob/v8.18.6/libvips/foreign/dzsave.c).

## Filename loading progress

The filename now carries a quiet background fill and a percentage while loading.
Backend notifications advance in five-percentage-point steps, at most twenty
progress advances per load. Preview work occupies the first 20%, pyramid work
occupies the next 79%, and completion is reserved for ready assets. These are
approximate work weights, not elapsed-time estimates. Unknown metadata progress
uses a static tint; completion, cancellation and errors clear the display.

Raw TIFF progress counts completed rows across parallel partitions. Libvips
progress uses public GLib evaluation hooks scoped to each synchronous sink,
without accessing private Rust binding fields or enabling CLI stdout output.
Worker reports use coarse buckets and a 250 ms throttle. Existing image events
carry the fractions, including aggregate channel progress; no new polling or
dependencies were added. Existing early cache/preview notification order remains.

Validation passed with 52 Rust tests using the production worker, 62 frontend
tests, frontend/native builds, formatting and strict Clippy. Tests cover streaming
progress followed by terminal responses, invalid fractions, cancellation/reaping,
raw pixel parity, actual native evaluation callbacks, and monotonic multichannel
progress. Independent read-only review found no actionable defects.

Native WebKit validation exercised the 1.58 GB Deflate TIFF through preview and
pyramid generation. The observed work percentages advanced from 0 through 95%,
then ready cleared the fill. Cancellation, opening another image, worker reaping,
error recovery, cached pyramid zoom/fit and five-channel Color passed. Light/dark
screenshots and raw results remain ignored in `artifacts/loading-progress/`.

A three-trial fresh-display-cache comparison using the existing native harness
reported median first draws of 2.005 s before and 1.767 s with progress. Source
pages were not explicitly evicted. Trial variation and sequential ordering make
this a regression screen, not evidence that progress itself speeds loading.
The screenshot-instrumented full-load trial took 20.562 s and emitted fifteen
known loading percentages, followed by ready. No physical display-presentation
claim is made. Source files were opened read-only and left unchanged.

## Folder image navigation, October 12, 2026

Previous/Next chevrons keep fixed positions around a flexible filename area.
Folder entries use Open's shared extension list, with case-insensitive alphabetical
ordering, excluded subdirectories and disabled end boundaries. Navigation uses the
same discard warning and progressive loader as Open; rapid clicks advance from
the latest selected target rather than waiting for its load reply.

Validation passed 64 frontend tests and 55 Rust tests in both subprocess and
production-worker modes, strict Clippy, Prettier, Rust formatting, and frontend
and native debug builds. The native WebKit harness exercises all seven allowed
extensions, unsupported entries/directories, corrupt-image recovery, both themes
at actual 640 × 400 and 1200 × 800 CSS-pixel window sizes, and short/long filenames.
It replaces a 1.58 GB TIFF before preview and during detail loading, checks worker
reaping and stale replies, and holds ordinary/channel downloads to verify abort
and immediate release of OpenSeadragon loading slots. Real native Cancel/Discard
dialogs cover added points and deletion of the final point; saving/reopening
retains annotations and navigation after saving needs no discard warning. All
14 native checks passed, including pointer clicks within the title bar's drag
region and a delayed annotation cancellation error after standalone Cancel.

Repeat with `scripts/native-navigation.py`, a built debug binary and a dedicated
`tauri-driver` session on port 4446. Supply `--large-image` for a local large TIFF;
start the driver with `XDG_CACHE_HOME` under `artifacts/folder-navigation/cache`.
Use an owned `TMPDIR` there too on systems with temporary-directory user quotas.
Only the initial chooser result is supplied by the navigation harness; Rust IPC,
image workers, assets, rendering and warning dialogs are native. The existing
`native-smoke.py` separately exercises the real GTK Open picker. Owned test copies,
read-only aliases, reports and screenshots remain ignored under
`artifacts/folder-navigation/`; original fixture/source sizes and mtimes are checked.

The real-picker trial initially aborted in GTK's icon helper with a theme-icon
decode error reporting "Disk quota exceeded" before image selection. The `/tmp`
mount has user quotas; standalone decoding and a small temporary write succeeded
afterward, so the exact exhausted limit was not established. The picker passed
with an owned temporary directory on the home filesystem. No system configuration
or unrelated temporary files were changed. The native WebDriver harness also
retries transient proxy disconnections for read-only observations only.

Cancellation kills/reaps image workers and subprocesses, aborts active frontend
downloads/histogram loading, and releases queued backend tile requests. A bounded
annotation metadata read already inside its parser call finishes that call before
its cancellation check; canceled work cannot publish annotations or restart loading.

## Windows/Linux packaging and updates, October 13, 2026

The release branch adds a Windows native-reader port, Windows per-user and
portable packages, Linux AppImage packaging, and signed-update workflows.
The final Linux debug build passed 57 Rust tests with the production worker,
strict Clippy, 66 frontend tests, frontend build, and formatting. Release metadata
checks pass four tests; signature verification separately accepted signed test
assets and rejected modified bytes. Native reader tests compare custom file
callbacks with TIFF/BigTIFF decoding and exercise BMP decoding and TIFF conversion.

A real Linux Tauri/WebKit session verified the update dialog, displayed package
version, development-build update restriction, preference toggle/persistence,
and dialog dismissal. A further native session supplied only test release metadata,
a completed-download response, and the initial chooser result; annotation editing
and saving used the real backend. Cancel kept unsaved edits, Save persisted them,
and the native development-mode installation rejection restored editing. This
checks the UI guard and recovery, not a real download or upgrade. Native IPC opened the BMP fixture and produced its 65 × 49
preview and tiles without errors. Reports and a rendered dialog screenshot are
ignored under `artifacts/native-validation/`. A separate picker/window-controls
smoke attempt was stopped when desktop focus changed; it is not counted as a
passed picker check. No source microscopy images were modified.

Windows/MSVC and Debian 12 package builds share the manual Native validation
workflow and release build implementation. Their unsigned artifacts are validation
builds, not public releases.
Clean-machine desktop acceptance, the Linux distro matrix, and a complete signed
upgrade from a prior package remain required before stable release promotion.
See `RELEASING.md` for the checklist and the distinction between updater signing
and pending Windows publisher signing.

The Debian 12 AppImage from commit `cecb787` passed bundled vips, vipsheader,
tiffinfo, PNG/BMP/TIFF conversion, raw-preview and DeepZoom worker checks in
[package validation](https://github.com/neuroqp/neuroqp-aperture/actions/runs/38408565452).
The downloaded 102.58 MiB artifact also launched and rendered its native window
on Arch/Omarchy through `--appimage-extract-and-run`. This was a launch check,
not the full distro or image-interaction acceptance matrix. The local screenshot
is ignored at `artifacts/package-validation/appimage-native.png`.

The complete [Windows/Linux package run](https://github.com/neuroqp/neuroqp-aperture/actions/runs/38413257615)
passed at `cbc1acb`: both optimized packages, 53 Windows and 57 Linux Rust tests,
strict Clippy, frontend checks, and combined artifact metadata/checksums. Windows
produced Setup and portable executables. Its smoke checked interrupted extraction,
forced/concurrent lock contention, payload reuse and file hashes, bundled worker
conversion, per-user install/uninstall at a Unicode path, and native window handles
using app-local fixed WebView2. The portable screenshot shows the rendered toolbar;
the installed screenshot was captured before first paint, so installed rendering
remains unverified. GitHub's runner account is not a clean university standard-user
acceptance environment. The portable download is approximately 231.5 MiB and its
uncompressed payload approximately 710.5 MiB.

The real concurrency check exposed an invalid-handle/error-code handling defect
in the first launcher. The corrected launcher compares the handle numerically and
captures the Windows error in the same API call. A small production-template
regression now forces contention before expensive builds; extraction-only failures
exit without a blocking dialog and retain diagnostics. Full package smoke then
passed. Evidence is retained as `windows-package-evidence` in that workflow run
and locally under `artifacts/package-validation/windows-evidence/`.

The final documentation/CI-trigger cleanup does not change these tested binaries.
The temporary feature-branch push workflow was removed; manual Native validation
continues to invoke the same reusable package build. No release tag or public
release was created.

## PR review fixes, 2026-10-13

`scripts/native-update-recovery.py` reproduced the discarded-update dirty-state
bug in the previous native binary, then passed after the fix. Real pointer edits,
TIFF annotation saving, and the backend's development-mode install rejection
verified Cancel, failed Discard-install recovery, the subsequent Open warning,
restored edit controls, and Save-before-install. The check supplies chooser and
confirmation responses plus update metadata/download responses. It does not
exercise a real signed upgrade or native dialog rendering. The source fixture was
unchanged; only an owned copy was saved. Results are ignored under
`artifacts/update-recovery/`.

The source-material gate has six release-helper tests, including unfinished-source
blocking, accepted source bytes, archive tampering, and dependency identity
mismatch. The checked-in source plan remains `unassembled`, so signed candidate
creation is blocked until source/build material is assembled and reviewed.

The complete [review-fix package run](https://github.com/neuroqp/neuroqp-aperture/actions/runs/38421253191)
passed at `fc624ce`: 56 Windows and 59 Linux Rust tests, strict Clippy, six
release-helper tests, frontend checks, optimized Setup/portable/AppImage builds,
packaged-worker checks, Windows extraction/concurrency/reuse and per-user
install/uninstall smoke, and combined artifact metadata/checksums. The Windows
log confirms both the restored-mtime/file-replacement identity regression and
two/four-band TIFF loading regression passed. Native desktop acceptance and a
real signed upgrade remain separate first-release requirements.

A subsequent review found that unsigned CI uploads also needed the source gate.
Both platform artifact uploads now require the reviewed source archive and the
matching platform BUILDINFO identity; unsigned aggregate verification requires
the source assets too. Failure-debug binary uploads were removed. While the plan
is `unassembled`, a native-validation run can build and smoke-test packages but
will fail at the publication gate without uploading them. Seven temporary binary
artifacts from this branch's three earlier runs were deleted; run logs and
Windows screenshot/diagnostic evidence were retained. The native code and packages
are unchanged from the successful run above.
