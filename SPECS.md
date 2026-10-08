# NeuroQP Aperture

A minimal desktop image viewer that launches quickly, shows the first useful
preview as soon as possible, and stays responsive while loading large images.

## Initial scope

- Develop and test on Linux first. Windows and macOS remain required future
  targets; validation on those platforms is deferred until available.
- Display one image at a time.
- Start with standard image formats, including JPEG/JPG, PNG, BMP, and TIFF/TIF.
- Support ordinary images and large TIFFs, including BigTIFF. Exact compression,
  pixel-type, and multi-page coverage must be documented as support is added.
- Keep the interface and implementation small. Image editing, analysis,
  and specialist microscopy formats are outside the current scope. Display-range adjustment is display-only. Multi-point
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
- The Linux proof of concept uses libvips command-line workers for image
  reading and pyramid generation. Packaging image readers for other operating
  systems remains future work.

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
   title bar.
2. **Quick toolbar:** a compact row of icon buttons inspired by ImageJ. These
   use Lucide icons, with a matching custom multi-point icon. Display range and
   multi-point toggles sit on the left; pan, zoom, and fit controls sit on the
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
on Open or Close. Cancellation or save failure must retain unsaved edits.

## Tool option panels

- Panels open independently in a row anchored to the viewer's top-right.
  Keep open panels adjacent; wrap when the window cannot fit another panel.
  Each toggle and close button controls only its own panel. Escape closes the
  focused panel, or the most recently opened panel if neither has focus.
- Display range: an approximate histogram sampled from the image preview,
  Minimum and Maximum sliders with labels on the left and values on the right.
  Adjust the displayed image live without modifying source pixels or cached tiles.
  Show vertical cutoff lines at the selected minimum and maximum.
  Provide percentile presets 0–100, 0.1–99.9, 0.5–99.5, and 1–99 beside Reset.
  The 0–100 preset uses the sampled histogram's occupied minimum and maximum;
  Reset restores the full 0–255 display range.
  The histogram defaults to linear counts. A Linear/Log toggle overlays its
  upper-right corner on hover or keyboard focus and changes only histogram heights.
  Use the current 8-bit display preview domain, not original high-bit-depth values.
  Reset the display range when opening another image.
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
- Use the approved T1/B2/C1/D1/S1/P2/M2 styling: recessed lavender toggles, tinted
  action buttons, solid purple checkboxes, raised dropdowns, fine round-handle
  sliders, and quiet compact panels. Use purple and orange accents in light and
  dark modes. Follow system appearance initially; a header theme button switches
  modes and remembers the choice.

## ImageJ annotation interchange

- Load active multi-point selections and point overlays embedded in ImageJ TIFFs.
  Store positions in original-image pixel-center coordinates with subpixel precision.
  Imported selections retain their ROI boundaries and metadata.
- Preserve marker type, size, ARGB color, labels, counters, slice positions, ROI
  names/properties, and unrelated metadata when saving. Preserve the distinction
  between ImageJ's active selection and point overlays. Adding points when only
  overlays exist creates a new active selection without changing those overlays.
- Display the first page only. Preserve annotations for other slices/channels/time
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
- Validate and package the application for Windows and macOS.
- Expand annotation export formats and conversion options when needed.
- Define initial multi-page TIFF behavior and supported pixel types without
  silently interpreting pages as channels or flattening scientific dimensions.
- Set numerical performance targets after measuring representative files on
  the available hardware.
