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
  annotation placement, and specialist microscopy formats are outside the initial
  scope. Brightness and multi-point option panels are UI-only prototypes.

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
   use Lucide icons, with a matching custom multi-point icon. Brightness and
   multi-point toggles sit on the left; pan, zoom, and fit controls sit on the
   right. Remove unused placeholder tools.
3. **Image viewer:** fill all remaining window space. Show the image against a
   neutral background, preserve its aspect ratio, and support smooth pan and
   zoom. Use nearest-neighbor scaling so magnified pixels have sharp boundaries.
   Fit the image to the available space when first opened.

Open selects a local image and replaces the current image. Save is a visible
control; its initial behavior and supported output formats remain to be decided.
Disable Save until an image and a supported save operation are available.

## Tool option panels

- Panels open independently in a row anchored to the viewer's top-right.
  Keep open panels adjacent; wrap when the window cannot fit another panel.
  Each toggle and close button controls only its own panel. Escape closes the
  focused panel, or the most recently opened panel if neither has focus.
- Brightness: display-range graph, Minimum, Maximum, Brightness, and Contrast
  sliders with each label to the left and its value to the right, plus Reset.
  Controls currently update local UI state only.
  Future brightness adjustments must affect display only, never source pixels.
- Multi-point: icon toggle rows for Cross/Dot/Circle, color, and size. Each row
  previews its choices using the other two current selections. Include Label
  points, Show on all slices, Counter, and point count. Annotation placement
  and counting are not implemented yet.
- Preserve option values while closing and reopening panels during the session.
- Use the approved T1/B2/C1/D1/S1/P2/M2 styling: recessed lavender toggles, tinted
  action buttons, solid purple checkboxes, raised dropdowns, fine round-handle
  sliders, and quiet compact panels. Use purple and orange accents in light and
  dark modes. Follow system appearance initially; a header theme button switches
  modes and remembers the choice.

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

- Add cell annotation tools with click-to-mark points, labels/categories, and
  an overview of marked cells and counts. Store annotations in image coordinates
  separately from source pixels so they remain aligned through pan and zoom.
- Add specialist microscopy formats as needed, preserving the meaning of
  scenes, channels, Z slices, timepoints, and image metadata.
- Validate and package the application for Windows and macOS.
- Define Save behavior, output formats, and any conversion settings before
  implementing saving.
- Define initial multi-page TIFF behavior and supported pixel types without
  silently interpreting pages as channels or flattening scientific dimensions.
- Set numerical performance targets after measuring representative files on
  the available hardware.
