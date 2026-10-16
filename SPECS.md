# NeuroQP Aperture

A minimal desktop image viewer that launches quickly, shows the first useful
preview as soon as possible, and stays responsive while loading large images.

## Initial scope

- Develop on Linux and ship Windows x64 and Linux x86_64 packages. Windows uses
  a per-user NSIS installer and a single-file portable launcher; Linux uses
  AppImage. Defer macOS, ARM, app stores, and distro-specific packages.
- Display one image per window. Allow independent application processes for
  side-by-side viewing. Launching an image path opens it in a new process;
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
- Keep the interface and implementation small. Destructive image editing, analysis,
  and specialist microscopy formats are outside the current scope. Channel brightness, range and coloring are display-only. Multi-point
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
  conversion order. Channel pyramids fuse extraction and normalization into the
  pyramid graph to avoid full-resolution intermediate files. Windows packages bundle the native image readers and fixed WebView2 runtime;
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

1. **Image bar:** identify the currently loaded image by filename. Place Open
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
  histogram of the original source channel's 8-bit display samples.
  Drag the black-point or white-point line horizontally to adjust its input
  cutoff; drag the right-hand plateau vertically to adjust brightness. Each
  target highlights its background on hover and while dragging, with horizontal
  or vertical resize cursors. Keep brightness reachable at a white point of 255
  and at 0%/100% brightness. Capture the pointer during dragging, clamp values
  to the same bounds as the sliders, and stop on release, cancellation, panel
  closure or image/connection replacement. Sliders remain keyboard accessible.
  Provide percentile presets 0–100, 0.1–99.9, 0.5–99.5, and 1–99 beside Reset.
  The 0–100 preset uses the sampled occupied minimum and maximum. Presets change
  only the selected connection's input range and retain its brightness.
  Reset restores that connection's full 0–255 range and 100% brightness.
  The histogram defaults to linear counts; its Linear/Log toggle sits below
  the graph, clear of its drag targets, and changes only histogram heights.
  Input ranges use the 8-bit display domain, not original high-bit-depth values.
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
  and explicit ImageJ channel pages at first Z/time, up to 32 channels. Reject
  unsupported OME mapping and ambiguous alpha layouts with an explicit error.
  Channel TIFF display caches retain separate lossless 8-bit planes (16-bit
  inputs use their high byte); these are display samples, not scientific exports.
  Use WebGL2 to retain immutable channel textures and apply a small color lookup
  in the shader. Coalesce settings changes within an event-loop turn and redraw
  the last displayed tile geometry immediately; OpenSeadragon still owns tile
  selection, pan, zoom, and cache eviction. Fall back to CPU compositing if WebGL2
  is unavailable or the context is lost. GPU optimization must not change the
  image-loading or pyramid-generation pipeline.
- Use the approved T1/B2/C1/D1/S1/P2/M2 styling: recessed lavender toggles, tinted
  action buttons, solid purple checkboxes, raised dropdowns, fine round-handle
  sliders, and quiet compact panels. Use purple and orange accents in light and
  dark modes. Follow system appearance initially; Settings > Appearance selects
  System, Light or Dark and remembers the choice.

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
