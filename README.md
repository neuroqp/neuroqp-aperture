# NeuroQP Aperture

A minimal Linux desktop image viewer built with Tauri, Rust, and OpenSeadragon.
Two compact toolbars leave the rest of the window to the image. Large images
load through a preview and progressively available detail tiles.

Display range, Color, and multi-point options open independently at the viewer's top-right.
Color connects image channels to editable output colors, with a live display
range, source histogram, presets and Reset per connection. Drag wires to reroute or remove them; click an
output to pick its shade. These settings affect only the display.
WebGL2 keeps image samples on the GPU and redraws cached tiles when colors change;
unavailable or lost GPU contexts fall back to CPU rendering. No additional Rust
renderer or graphics dependency is required. Loading and pyramid generation are
separate from this rendering path.
Point type, color, and size buttons preview the current combination. The header's
sun/moon button switches light and dark modes and remembers your choice; the
initial theme follows the system.

## Run locally

Install Node.js 22.18 or newer, pnpm 9, a current Rust toolchain, and the Tauri
Linux build dependencies. `vips` and `vipsheader` must be on PATH.
Parallel strip previews link libtiff 4.5 or newer.

On Arch Linux:

```sh
sudo pacman -S --needed base-devel gtk3 webkit2gtk-4.1 librsvg libvips libtiff
```

On Ubuntu 24.04:

```sh
sudo apt-get install build-essential libgtk-3-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev libvips-dev libvips-tools libtiff-dev libtiff-tools
```

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

`pnpm dev` opens the frontend development server only. Native file loading and
window controls require the Tauri application.

To build a standalone development binary with the frontend embedded:

```sh
pnpm tauri build --debug --no-bundle
./src-tauri/target/debug/aperture
```

## Checks

PR checks run Prettier, Rust formatting, TypeScript type checking, and frontend
unit tests. They run once per PR update and cancel superseded runs. They do not
install native dependencies or build the application.

Run full validation locally with:

```sh
pnpm format:check
pnpm test
pnpm build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol --locked
APERTURE_WORKER_EXECUTABLE="$PWD/src-tauri/target/debug/aperture" \
  cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
```

Use `pnpm format` and `cargo fmt --manifest-path src-tauri/Cargo.toml --all` to
format changes. The **Native validation** workflow runs the full checks above
manually from GitHub Actions. It becomes available after the workflow lands on
`main`; choose the branch to validate with **Run workflow**.
The executable override makes decoder tests use the production child worker.
Without it, isolated unit runs retain the CLI fixture path.

## Test images

Small generated images live in `tests/fixtures/`. To regenerate them and collect
available local NeuroQP images without copying or modifying the originals:

```sh
uv run scripts/collect-test-images.py
```

The ignored `test-data/` directory contains source links and a metadata
manifest. The application itself does not use Python. See
[the fixture notes](tests/fixtures/README.md) for coverage and
[SPECS.md](SPECS.md) for product requirements and future scope.
Measured native application results and limitations are recorded in
[VALIDATION.md](VALIDATION.md).

For direct application checks in an active Linux desktop session, install
`tauri-driver` and WebKitWebDriver, start `tauri-driver`, then run:

```sh
python scripts/native-smoke.py src-tauri/target/debug/aperture \
  tests/fixtures/rgb-65x49.png test-data/real-gray16-stripped-bigtiff.tif
```

The current native harness uses Hyprland, `ydotool`, `wl-copy`, and `grim` to
operate the real file chooser and capture the application window. It uses the
keyboard, clipboard, and window focus while running. Results and screenshots
stay in ignored `test-data/`.

## ImageJ annotation validation

Run the optional interoperability check against a local ImageJ installation:

```bash
python3 scripts/check-imagej-interop.py /path/to/ImageJ/ij.jar --java-home /path/to/jdk
```

It generates synthetic TIFFs, round-trips editable ROIs through Aperture and
ImageJ, and checks coordinates, metadata, and exact image pixel preservation.
No microscopy source files are modified.

## Proof-of-concept boundaries

- Linux is the current development and validation target. Windows and macOS
  packaging and testing are pending.
- Save/Ctrl+S offers Overwrite, Save as…, or Cancel for TIFFs. Overwrite updates
  ImageJ-compatible annotations at the current path while preserving pixels and
  unrelated metadata. Other formats default to a sibling file with the same
  name and a .tif extension; existing destinations require confirmation.
  Save as… opens the file picker and remembers the chosen path for later saves.
  Click with Multi-point active to add
  points; closing the panel or selecting Pan leaves placement mode. Imported
  ImageJ point selections and overlays remain editable annotations.
- Display-range sliders and percentile presets adjust only
  the displayed image. The approximate histogram samples the 8-bit preview, with
  linear counts by default and an optional Log toggle on hover. The 0–100 preset
  uses the sampled minimum and maximum; Reset restores 0–255. Original images
  and cached tiles stay unchanged.
- TIFF copies preserve their original layout. Stock ImageJ cannot directly open
  all tiled TIFFs or BigTIFFs; those need a suitable image reader. Other formats
  convert to a classic stripped TIFF, subject to its 4 GiB addressing limit.
- Counter assigns an ImageJ category to new points in the active selection.
  Type, size and base color apply to that selection; Label points also updates
  point overlays. Fresh counter-0 selections show point numbers; using a higher
  counter switches to category labels and ImageJ's counter colors. The question
  mark to the right of Counter explains this behavior. Categories range from
  0 to 99. In multi-point mode, mouse-down or E adds a point at the cursor,
  dragging an existing point moves it, and Alt-click removes it. The hit radius
  is five screen pixels regardless of marker size. Hold Space or Ctrl to
  temporarily pan without leaving the tool; WASD/arrows move at 7.5 times
  the viewer's default speed. The panel header's help explains the controls.
  Saved ROIs use ImageJ's active selection and overlays.
- Ordinary multipage images display their first page. Color supports up to 32
  unsigned 8/16-bit channels in interleaved multiband TIFFs and explicit ImageJ
  channel stacks (first Z/time position). OME-TIFF mapping and contiguous
  single-directory ImageJ hyperstacks are not supported. Ambiguous grayscale
  alpha layouts are rejected rather than interpreted as scientific channels.
- Supported display pixels are unsigned 8/16-bit grayscale, RGB, and RGBA in
  grayscale/RGB/sRGB color spaces. Signed, floating-point, CMYK, and other
  unsupported channel layouts report an unsupported-image error.
- Display previews and tiles are separate from the source pixels. They are not
  scientific data exports. Transparent images are composited against the canvas;
  unsigned 16-bit values are mapped to an 8-bit display range.
- TIFF decoding uses Rust libvips bindings in one image-scoped, killable worker.
  Channel previews resize each source page once before extracting its bands.
  Ordinary uncompressed gray16 and RGB8 sRGB TIFFs of at least 64 megapixels use a bounded
  Rust reader with up to eight threads for the overview. Its area filter includes every source
  pixel and differs from the general Lanczos preview; full detail uses libvips.
  Compressed, tiled, oriented, alpha and multipage layouts retain the general reader.
  The changed overview filter invalidates existing display caches once.
- Display files use the application's cache directory. Completed entries are
  reused, old entries are evicted, and generation stops when its 2 GiB soft disk
  limit is reached. libvips cache settings limit working storage but are not a
  hard process-memory ceiling.
- Existing tiled TIFF pyramids can be read on demand. `tiffinfo` from libtiff
  tools enables recognition of pyramids stored as separate TIFF pages and supplies
  tile-layout metadata for libvips versions before 8.18. With libvips 8.18 or newer,
  SubIFD pyramids do not require it.
- Hundreds-of-GB images remain a future scale target. Initial validation uses
  locally available images, including multi-GB TIFFs.
