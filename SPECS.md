# NeuroQP Aperture

A minimal desktop image viewer that launches quickly, shows the first useful
preview as soon as possible, and stays responsive while loading large images.

## Initial scope

- Develop on Linux and ship Windows x64 and Linux x86_64 packages. Windows uses
  a per-user NSIS installer and a single-file portable launcher; Linux uses
  AppImage. Defer macOS, ARM, app stores, and distro-specific packages.
- Display one image per window. Allow independent application processes for
  side-by-side viewing. Ctrl+N starts a new instance without a toolbar button.
  Launching an image path opens it in a new process;
  Open and folder navigation replace the image in the current window.
- Give each process a reusable cache directory with exclusive ownership. Cleanup
  must never remove another running process's working files or display cache.
- Before overwriting, warn if the destination is open in another Aperture process
  or has changed on disk. Offer Cancel, Save as and Overwrite anyway. All windows
  remain editable; presence is advisory and tracks the selected source until
  replacement or window closure. Cancelled or failed background processing can
  retain a usable preview or loaded annotations, so it does not release presence.
  Serialize only the final destination check and atomic replacement. Preserve
  edits on cancellation or failed saves.
- Install updates only after other Aperture processes close. Block new starts
  during replacement and retain the existing unsaved-work warning.
- Start with standard image formats, including JPEG/JPG, PNG, BMP, and TIFF/TIF.
- Support ordinary images and large TIFFs, including BigTIFF. Exact compression,
  pixel-type, and multi-page coverage must be documented as support is added.
- Keep the interface and implementation small. Destructive image editing and analysis
  are outside the current scope. Nikon ND2 and Zeiss CZI have initial browsing
  implementations described below. Channel brightness, range and coloring are display-only. Multi-point
  annotations are editable metadata, kept separate from source image pixels.

## Technology decisions

- Use Tauri for the desktop application, with a Rust backend and a web-based UI.
- Use OpenSeadragon for the image viewer, including pan, zoom, and tiled pyramid
  display. It is the same viewer technology used in the main NeuroQP application.
- This choice builds on our experience with OpenSeadragon's performance and
  suitability, reuses familiar technology, and allows future annotation tools.
- Keep file access, image decoding, and preview/pyramid generation behind the
  Rust backend. Supply previews and individual display tiles to the viewer.
- Use vanilla TypeScript and CSS for the initial UI, keeping dependencies small.
- The Linux proof of concept uses Rust libvips bindings in an isolated,
  image-scoped worker for TIFF reading and pyramid generation. Non-TIFF inputs
  use libvips tools. Ordinary single-page grayscale and RGB8 sRGB TIFF overviews
  of at least 16 megapixels use a full-coverage Rust area reducer for supported
  strip layouts. Independent libtiff handles decode LZW/Deflate strips with up to
  16 threads; uncompressed reads use up to eight. Encoded and decoded compressed
  strips are each limited to 16 MiB, with other layouts left to libvips. Full
  detail stays in libvips, using up to eight threads for pyramid sinks. RGB8
  pyramids use direct JPEG tile encoding; grayscale retains its existing
  conversion order; 16-bit grayscale overviews stay lossless 16-bit. Channel
  pyramids fuse band extraction into the pyramid graph to avoid full-resolution
  intermediate files. Windows packages bundle the native image readers and fixed WebView2 runtime;
  AppImages bundle their native image readers against the Debian 12 baseline.

## Development principles

- Keep all code consistently formatted. Use Prettier for TypeScript and other
  supported frontend files, and rustfmt via `cargo fmt --all` for Rust.
- Run formatting checks before committing changes.
- Favor minimal, elegant implementations. Follow KISS: keep the design simple,
  readable, and easy to maintain.
- Follow YAGNI: implement current requirements; add future features and
  abstractions only when they are needed.
- Avoid bloat, boilerplate, speculative flexibility, and unnecessary layers or
  dependencies. Reuse standard library and existing framework capabilities.
- Simplify without compromising correctness, responsiveness, or data integrity.

## Window layout

Two thin horizontal header bars sit above the image viewer.

1. **Image bar:** place the full NeuroQP logo at the far left, followed by
   Aperture as text, then identify the currently loaded image by filename. Use
   the Aperture symbol only as the app icon, outside the in-app header. Place Open
   and Save buttons on the right, alongside window controls including Close.
   Integrate window controls into this bar so the design does not need a third
   title bar. Fixed-position left/right chevrons surround the flexible,
   ellipsized filename area and select the previous/next supported image in the
   same folder. Sort filenames alphabetically, ignoring case with a deterministic
   case-sensitive tie break; disable navigation at either end. Folder navigation
   uses the same supported extensions as Open, excludes directories, and follows
   the shared unsaved-annotation warning and progressive image-loading flow.
   Repeated clicks advance from the latest selected file, even before its preview
   arrives, cancelling the previous worker and outstanding image downloads.
2. **Quick toolbar:** a compact row of icon buttons inspired by ImageJ. These
   use Lucide icons, with a matching custom multi-point icon. Color and multi-point toggles sit on the left; pan, zoom, and fit controls sit on the
   right. Remove unused placeholder tools.
3. **Image viewer:** fill all remaining window space. Show the image against a
   neutral background, preserve its aspect ratio, and support smooth pan and
   zoom. Use nearest-neighbor scaling so magnified pixels have sharp boundaries.
   Fit the image to the available space when first opened. WASD and arrow keys
   pan at 7.5 times OpenSeadragon's default speed and tap distance.

Open selects a local image and replaces the current image. Save and Ctrl+S use
the same flow. For a TIFF, offer Overwrite, Save as…, and Cancel. Overwrite saves
annotations directly at that path without a file picker. Other formats default
to the same filename with a .tif extension in the source directory and leave
the original unchanged; confirm before replacing an existing destination.
Save as… opens a file picker and remembers the chosen path for subsequent saves.
Enable Save when annotations are available or deletions need saving, including
after removing the final point. Confirm before discarding unsaved annotations
on Open, folder navigation or Close. Cancellation or save failure must retain unsaved edits.

## Settings

- A single gear beside the image bar's window controls opens a full-window
  settings view. Keep native window controls available in its title bar.
- Start on General, with General and Appearance in a left sidebar and controls
  on the right. Closing settings or pressing Escape returns focus to the gear
  and preserves the loaded image, viewport, tool panels and unsaved annotations.
  Image-editing and viewer shortcuts do not act behind settings.
- General shows the running version and package edition, manual update checking,
  automatic daily checking, release notes, download progress and deferral. Keep
  the existing package-specific update and unsaved-work restart protections.
  Mark the gear with a small dot when an update is available.
- Appearance offers System, Light and Dark. Apply the selection immediately,
  remember it across launches, and follow operating-system changes in System mode.

## Tool option panels

- Panels open independently in a row anchored to the viewer's top-right.
  Keep open panels adjacent; wrap when the window cannot fit another panel.
  Each toggle and close button controls only its own panel. Escape closes the
  focused panel, or the most recently opened panel if neither has focus.
- Multi-point: icon toggle rows for Cross/Dot/Circle, color, and size. Each row
  previews its choices using the other two current selections. Include Label
  points, Show on all slices, Counter, and the selected counter's point count.
  With the tool active, primary mouse-down inside the image adds a point using
  the current point-selection settings, even if the pointer is moving. E adds
  a point at the cursor once per press, ignoring key-repeat, text/control entry,
  held pan/delete modifiers, and active pointer gestures. Counter assigns a
  category to new points within the active selection, following ImageJ.
  Type, size and base color apply
  to the active selection; Label points also updates point overlays. Counter
  changes do not restore separate styles. A fresh selection using counter 0
  labels points sequentially. Once a higher counter is used, labels show category
  IDs and markers use ImageJ's counter colors. Returning to counter 0 retains
  this mode for that selection. The point count reports the selected category
  in the active selection. Explain these rules through a question-mark tooltip
  to the right of Counter's dropdown. Explain the behavior first, then note
  compatibility with ImageJ's multi-point tool. Categories range from 0 to 99;
  the scrollable dropdown must follow the app's light/dark theme.
  Use a fixed five-screen-pixel hit radius independent of marker size and zoom.
  Hovering a visible point in multi-point mode shows a grab cursor; dragging
  moves it. Alt-hover shows a minus cursor and Alt-click removes that point.
  Space or Ctrl held temporarily overrides the current tool with panning and
  a hand cursor; releasing restores the tool. Panning takes precedence over
  point editing. Losing window focus clears transient keys and gestures.
  Closing the panel or selecting Pan leaves point-placement mode. A question-mark
  tooltip in the Multi-point panel header explains marker appearance, categories,
  adding with the mouse or E, dragging, deletion, and temporary panning. Marker
  appearance settings do not change image pixels.
- Preserve option values while closing and reopening panels during the session.
- Color: a compact connection diagram maps source channels to seven editable
  output colors. Several inputs can feed one output, and one input can feed
  several outputs. Click or drag from an input to an output to connect them.
  Select a wire to adjust its Minimum/Maximum input range and Brightness
  independently. Brightness ranges from 0 to 100%, defaults to 100%, and scales
  the entire clamped intensity ramp before tinting and mixing. Zero contributes
  no light; 50% halves the contribution, including the white-point plateau.
  Plot this scaled ramp with vertical input cutoff lines over an approximate
  histogram of the source channel's display samples in source units.
  Drag the black-point or white-point line horizontally to adjust its input
  cutoff; drag the right-hand plateau vertically to adjust brightness. Each
  target highlights its background on hover and while dragging, with horizontal
  or vertical resize cursors. Keep brightness reachable at a white point at the
  top of the channel's domain and at 0%/100% brightness. Capture the pointer during dragging, clamp values
  to the same bounds as the sliders, and stop on release, cancellation, panel
  closure or image/connection replacement. Sliders remain keyboard accessible.
  Provide percentile presets 0–100, 0.1–99.9, 0.5–99.5, and 1–99 beside Reset.
  The 0–100 preset uses the sampled occupied minimum and maximum. Presets change
  only the selected connection's input range and retain its brightness.
  Reset restores that connection's full channel domain and 100% brightness.
  The histogram defaults to linear counts; its Linear/Log toggle sits below
  the graph, clear of its drag targets, and changes only histogram heights.
  Input ranges use source units within each channel's domain: 0–255 for 8-bit
  and 0–65535 for 16-bit samples. 8-bit channels start at 0–255. 16-bit channels
  start once per opened image at the sampled occupied minimum and maximum; RGB
  samples share one range to keep color balance. Constant planes use a
  one-unit range. Ranges persist while browsing frames.
  Selection and keyboard focus follow the curved wire with a glow in its current
  line color, without a
  rectangular outline. Keep the panel compact and center the arrow between
  source and color names in the connection header. Drag the
  wire itself to another output to reroute it; dropping in empty space deletes
  it. Escape cancels a drag. A trash button removes only the selected wire.
  Disconnected output dots use their color as an outline; connected dots are
  filled. Do not add wire handles or redundant column labels.
  Clicking an output when no connection is pending opens a compact shade, hue,
  and hex picker. Exact preset colors retain their names; custom colors use the
  nearest hue family's name with an -ish suffix and a muted hex value beneath.
  Neutral custom shades belong to Gray. The Gray preset is white so a grayscale
  channel retains its full intensity range. A header help tooltip explains the
  interactions. Support both app themes and keyboard access.
- Color settings update the GPU view live, without decoding the source
  again, rebuilding pyramids, or modifying source pixels or annotations. Each
  connection maps its source through its input range, scales it by brightness,
  tints it with its output color, and contributes additively to a clamped RGB
  composite. Color contains all image display adjustments; there is no separate
  Display range panel or composite filter. Closing Color retains its settings;
  opening another image resets them. Saving annotations never bakes display
  adjustments into pixels.
  Initially support unsigned 8/16-bit grayscale/RGB, interleaved multiband TIFF,
  and explicit ImageJ channel pages at first Z/time, up to 32 channels. OME-TIFFs
  use their OME-XML plane mapping (see OME-TIFF browsing). Reject ambiguous alpha
  layouts with an explicit error.
  Unsigned 16-bit images, including single-band grayscale, RGB16, 16-bit PNG
  and single-channel ImageJ stacks, display as channel images. Their display caches retain separate lossless 16-bit planes,
  served to the viewer as raw samples. 16-bit RGBA keeps the 8-bit path.
  Without a reusable TIFF pyramid, an image whose generated 16-bit pyramid could
  exceed half the display cache keeps the previous 8-bit high-byte display, and
  the status bar says so.
  Display caches are display samples, not scientific exports.
  Use WebGL2 to retain immutable integer channel textures and evaluate each
  connection's range, brightness and color in the shader. Coalesce settings changes within an event-loop turn and redraw
  the last displayed tile geometry immediately; OpenSeadragon still owns tile
  selection, pan, zoom, and cache eviction. Fall back to CPU compositing if WebGL2
  is unavailable or the context is lost. GPU optimization must not change the
  image-loading or pyramid-generation pipeline.
- Use the approved T1/B2/C1/D1/S1/P2/M2 styling: recessed lavender toggles, tinted
  action buttons, solid purple checkboxes, raised dropdowns, fine round-handle
  sliders, and quiet compact panels. Use purple and orange accents in light and
  dark modes. Follow system appearance initially; Settings > Appearance selects
  System, Light or Dark and remembers the choice.

## Initial Nikon ND2 browsing

- Decode ND2 in Rust with pinned `openreadout-nd2` and `openreadout-core` libraries.
  Do not require Python, Java, a vendor SDK, or a TypeScript format reader.
- Decode the selected position, Z and time frame on a Rust loader thread, then write a temporary display TIFF for the existing channel preview and
  pyramid pipeline. Display TIFFs keep original unsigned 8/16-bit samples, so
  Color ranges in source units apply consistently across frames. Keep the
  sampled original ranges in metadata. Cache keys include the source identity and selected coordinates.
  This implementation decodes a whole selected frame, not arbitrary regions. Native
  preview/pyramid operations remain in cancellable child processes. Rust decoding
  yields between frames; a new selection cancels queued optional work.
- Support unsigned 8/16-bit scalar acquisition channels, up to 32 samples, and
  one RGB acquisition channel. Preserve channel names/colors and available physical
  pixel sizes, Z spacing and nominal time interval. Keep source pixels unchanged.
- One toggleable Dimensions panel selects Position, Z, Time or Channel. A shared
  range, previous/next buttons and mouse-wheel scrolling browse the selected axis.
  Channel offers Composite followed by individual named channels. Channel selection
  changes only the display; spatial/time selection loads the corresponding frame.
  Retain the viewport and compatible Color settings between selected frames.
- ND2 is browsing-only in this iteration. Disable annotation editing and saving in
  the UI and reject their backend commands. ND2 writing, scientific export and
  dimension-aware annotation persistence require separate work.
- Reject unsupported pixel types, mixed RGB acquisition channels, ambiguous
  experiment loops and oversized frames explicitly. Limit a decoded selected frame
  to 256 MiB; this is not a total process-memory limit. Reader/codec buffers can
  coexist with the assembled frame. Bound encoded frames and metadata too.
  Validate compressed metadata expansion before parsing and include per-position
  metadata cloning in opening reservations; retain only compact typed summaries.
- Validate pixel values, dimension mapping and RGB sample order with synthetic
  fixtures; check real public acquisitions and native UI separately. Keep real
  acquisitions and generated evidence outside Git.

## ND2 maximum-intensity projections

- The Dimensions panel has independent Z max and Time max controls; enabling both
  reduces all Z/time frames at the current position. Unprojected axes and local
  channel selection remain navigable. Projected axes show their full extent and
  disable slice scrolling. Turning projection off starts at the first slice.
- Calculate component-wise maxima from original unsigned 8/16-bit intensities
  and display the projection in source units. Preserve channel/RGB order and keep
  positions separate.
- Reserve one original-depth accumulator plus the current input decode and
  display-TIFF writing under the shared budget. Borrow cached inputs; stream and
  release uncached inputs instead of retaining an entire stack. Check source
  identity and cancellation between frames and during reduction/conversion.
- Projection cache keys include mode and source identity and canonicalize the
  projected coordinates. Reuse completed display caches. Do not automatically
  preload projections in this iteration; ordinary preloading resumes when
  projection mode is off. Projections are display-only and remain read-only.

## Initial Zeiss CZI browsing

- Decode CZI in Rust with pinned `openreadout-czi` and `openreadout-core` libraries,
  through the same selected-frame, display-TIFF, Dimensions panel, projection,
  preloading and shared-budget path as ND2. CZI is browsing-only; annotation
  editing and saving are disabled in the UI and rejected by the backend.
- Each reader image is one Position labelled with its scene name, including
  H/I/R/V/B combinations; unnamed scenes use their S index, or `Position 1` for a
  single scene. Positions must share Z, time, channel count and pixel type, which
  usually excludes super-resolved renderings. XY size may differ; each frame keeps its own
  geometry and display cache, and the retained viewport is image-relative.
- Support unsigned 8/16-bit scalar channels, up to 32, and one RGB acquisition
  channel stored as Bgr24, Bgr48 or Bgra32, shown in R,G,B order without alpha.
  Mosaic tiles are stitched on read. Preserve channel names/colors, physical
  X/Y/Z pixel sizes and the nominal time increment.
- Reject float/int32/double/complex and mixed pixel types, inconsistent scenes,
  multi-file documents or part references and files without an intact subblock
  directory explicitly. Keep the 256 MiB selected-frame limit; reject larger
  stitched scenes with an error naming their size. Browsing starts at the first
  position, which must fit. Region or pyramid-level streaming is future work.
- Before the reader allocates, check the file header, metadata XML, subblock and
  attachment directories, interpreted attachments (walking event lists as the
  reader does), segment headers, each full-resolution subblock header and embedded
  entry, and each JPEG XR codestream header against its directory entry. Cap XML and attachments at 32 MiB, the subblock directory at
  128 MiB and encoded subblocks at 257 MiB. Reserve the reader's opening
  workspace, including per-image copies of XML strings, channels, time stamps and
  events. Frame decoding also reserves the reader's per-plane tile batch for the
  selected scene, including JPEG XR index tables and tile-column state, alongside
  its plane and the assembled frame.
- Validate pixel values, dimension mapping, stitching and RGB order with synthetic
  fixtures; compare public acquisitions against independent czifile values and
  check the native UI separately. See [CZI support](docs/CZI.md).

## OME-TIFF browsing

- Detect OME-TIFF by content: OME-XML at the root of the first page's
  ImageDescription, whatever the `.tif`/`.tiff` name, or a `*.companion.ome`
  chosen directly. Open's file filter adds `.companion.ome`; folder navigation
  lists member TIFFs only. Parse the bounded metadata without DTDs and resolve
  `TiffData` per the OME-TIFF specification, including `BinaryOnly` metadata in a
  companion or another OME-TIFF. Member files must stay inside the opened file's
  folder. See [OME-TIFF support](docs/OME-TIFF.md).
- Each OME `Image` is one Position. Series whose C/Z/T sizes, pixel type or
  samples differ from the first are left out with a status-bar note. Support
  unsigned 8/16-bit single-sample channels, up to 32, or one RGB channel; keep
  channel names/colors, physical sizes and the nominal time increment.
- A selection maps to TIFF pages that libvips reads in place through job-local
  member aliases: no frame is materialized and plain browsing has no frame-size
  limit. Each page is checked against its `Pixels`. Power-of-two SubIFD levels of
  each selected page are reused when all selected pages share them; otherwise a
  pyramid is generated, with the 8-bit fallback for oversized 16-bit pyramids.
  A plane the dataset does not store fails only that selection with a visible
  error; navigation continues.
- Fingerprint every member file, an opened companion and a `BinaryOnly`
  metadata file; cache keys include all of their identities. A changed or
  missing file fails the selection before cache reuse, after evaluation and
  before publishing, and on-demand native tiles check their member before reading
  and before publishing; errors name the file, and reopening reads it again.
- Z max, Time max and both work as for ND2 when the projected frame fits 256 MiB;
  otherwise the Dimensions panel disables them and shows why. Pages are reduced
  one at a time into an accumulator admitted before reading. Preloading builds
  previews and pyramids of nearby stored planes only. OME-TIFF is browsing-only:
  annotation editing and saving are disabled in the UI and rejected by the backend.

## Image memory and preloading

- Settings > General sets an image-memory upper limit, default 4 GiB and shared
  across Aperture windows/processes. Reserve decoder workspaces before allocation;
  retained original ND2/CZI frames keep their reservations. Crash-released OS leases
  reclaim accounting. This bounds accounted image data and conservative decoder
  allowances, not total application RSS, GPU memory or the separately bounded disk cache.
- A smaller budget changes cache/preload capacity, never source data or display
  resolution. Foreground work evicts optional frames before admission. If one
  operation cannot fit, report a clear error instead of exceeding the limit.
  Lowering the limit stops new admission while active reservations settle and
  optional caches are evicted. Other windows observe the shared setting.
- Finish the visible frame's preview and detail first, then preload nearby frames
  along the active dimension/direction. Generate remaining previews before their
  detail pyramids, stopping at memory or disk capacity. Keep original frame samples
  under the memory budget and previews/pyramids in the disk cache. Preserve visible
  pixels and viewport until the next selected preview is ready.
- Pause optional loading and evict optional raw frames under system memory
  pressure; resume after a larger available-memory margin. Foreground requests
  can ask other windows to release optional caches. Single-frame Rust decoding
  finishes before the next request; native work can be killed immediately.

## ImageJ annotation interchange

- Load active multi-point selections and point overlays embedded in ImageJ TIFFs.
  Store positions in original-image pixel-center coordinates with subpixel precision.
  Imported selections retain their ROI boundaries and metadata.
- Preserve marker type, size, ARGB color, labels, counters, slice positions, ROI
  names/properties, and unrelated metadata when saving. Preserve the distinction
  between ImageJ's active selection and point overlays. Adding points when only
  overlays exist creates a new active selection without changing those overlays.
- Display the first page for ordinary multipage TIFFs. Explicit ImageJ channel
  stacks display their channels at the first Z/time position; do not guess
  channel dimensions from an arbitrary page stack. Preserve annotations for other slices/channels/time
  points without displaying or reassigning them. Show on all slices sets new
  points' position to zero; it does not add stack navigation.
- TIFF saving copies the original file and appends replacement metadata without
  decoding or changing its pixels, compression, pages, or pyramid. Copying still
  costs disk space and I/O proportional to the source file size. Write to a
  temporary file and atomically replace the destination only after successful
  completion. Overwriting changes annotation metadata, not image pixels.
  Repeated saves must retain each imported ROI's original metadata association.
- Other supported image formats convert to a lossless, stripped classic TIFF.
  Display-range adjustments and marker graphics must not be baked into pixels.
- Stock ImageJ cannot directly read all tiled TIFFs or BigTIFFs. Metadata remains
  ImageJ ROI-compatible, but opening those images requires a suitable ImageJ
  reader. Classic TIFF output cannot exceed its 4 GiB addressing limit.
- Reject malformed or excessive annotation metadata with a visible error. The
  initial metadata limit is 32 MiB. ROI sets larger than 65,535 points have an
  ImageJ limitation on per-point counter/position persistence.

## Loading and performance

- Show an interactive window promptly. File reading, decoding, and pyramid
  generation must not block window interaction.
- Prioritize time to first useful preview over completion of the full load.
- Use an existing thumbnail or pyramid when available; do not rebuild an
  adequate existing pyramid unnecessarily.
- When a pyramid is needed, build it in the background and update the visible
  preview as usable image regions or resolution levels become available.
- Show loading/conversion progress. Allow cancellation or opening another image
  without waiting for the previous load to complete.
- Show a quiet percentage fill behind the filename, advancing in coarse steps.
  TIFF progress uses processed preview rows and libvips evaluation work, with
  approximate weights for preview and pyramid conversion. Do not show time
  estimates. Reserve completion for ready assets; clear the fill on completion,
  cancellation, or failure.
- For huge images, read and display the regions and resolution needed for the
  current view. Keep memory and GPU usage bounded rather than proportional to
  the full decoded image size.
- Reuse generated previews/pyramids on subsequent opens, with bounded disk-cache
  usage. Loading and cache generation must not alter the source image.
- Keep an available preview visible while sharper detail loads.
- Preview latency depends on file layout and compression. Some files require a
  substantial decode pass before a complete overview is available; show partial
  results and honest progress where possible.

Multi-GB images are an initial performance concern. Images of hundreds of GB are
a future scale target, not a verified capability. Measure cold startup, time to
first visible pixels, pan/zoom responsiveness during loading, and peak memory
using representative Linux files, including tiled and strip-based TIFFs.

## Future scope and open decisions

- Add a richer overview of marked cells and counts.
- Add specialist microscopy formats as needed, preserving the meaning of
  scenes, channels, Z slices, timepoints, and image metadata.
- Reconsider macOS and ARM packaging when there is demand and testing hardware.
- Expand annotation export formats and conversion options when needed.
- Define initial multi-page TIFF behavior and supported pixel types without
  silently interpreting pages as channels or flattening scientific dimensions.
- Set numerical performance targets after measuring representative files on
  the available hardware.

## Distribution and updates

- Publish source and releases in the public `neuroqp/neuroqp-aperture` repository.
  Reviewed feature branches merge into main; versioned tags identify release source.
- Offer one Windows Setup executable, one Windows portable executable, and one
  Linux AppImage. Windows packages work without administrator privileges and
  include WebView2 and image-library dependencies; institutional execution
  restrictions remain outside the application's control.
- Extract portable payloads into version-specific local application-data folders
  and reuse them. Keep image caches and settings separate from package payloads.
  Portable updates offer a download and manual replacement, never installation.
- Use signed Tauri updates for installed Windows and writable Linux AppImages.
  Check after startup at most daily, with opt-out, manual checking, release notes,
  download progress, and deferral. Development binaries do not auto-update.
- Download and verify before asking to restart. Preserve unsaved-work warnings;
  cancellation and save failure leave edits intact. Stop background image work
  before replacement. Failed installation must leave a recoverable application.
- Use three durable release operations: validate, package and publish. Finish
  automated regressions, changed-area native functional tests, source coverage
  and prepared desktop checks before final packaging. Retain source commit,
  executable and runtime dependency hashes so unchanged results remain reusable.
- Package the validated Windows and Linux executables without recompiling or
  resolving dependencies. After packaging, verify identity, updater signatures
  and source coverage, then run only launch, one-small-TIFF visible rendering and
  normal-close smoke checks. Windows 11 Setup and portable under a standard user,
  Debian 12 and Ubuntu 24 native Wayland form the routine matrix. Arch/Omarchy and
  Fedora are periodic compatibility targets and required when runtime dependencies
  or desktop integration change. Windows 10 remains unverified.
- Per-step timeouts preserve completed artifacts. Repair a failed operation and
  repeat affected checks; unchanged hash-bound results do not expire. Publish the
  tested bytes without rebuilding, moving published tags or replacing public
  assets. Verify public downloads and the production updater afterward. Follow
  [RELEASING.md](RELEASING.md).
- Keep PR checks fast. Full native builds and package validation are separate from
  ordinary PR checks. Record source identity, dependency versions, checksums,
  third-party notices, and release notes with each candidate.
