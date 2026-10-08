# NeuroQP Aperture

A minimal Linux desktop image viewer built with Tauri, Rust, and OpenSeadragon.
Two compact toolbars leave the rest of the window to the image. Large images
load through a preview and progressively available detail tiles.

Brightness and multi-point options open independently at the viewer's top-right.
Point type, color, and size buttons preview the current combination. The header's
sun/moon button switches light and dark modes and remembers your choice; the
initial theme follows the system.

## Run locally

Install Node.js 22.18 or newer, pnpm 9, a current Rust toolchain, and the Tauri
Linux build dependencies. `vips` and `vipsheader` must be on PATH.

On Arch Linux:

```sh
sudo pacman -S --needed base-devel gtk3 webkit2gtk-4.1 librsvg libvips
```

On Ubuntu 24.04:

```sh
sudo apt-get install build-essential libgtk-3-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev libvips-tools libtiff-tools
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

```sh
pnpm format:check
pnpm test
pnpm build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
```

Use `pnpm format` and `cargo fmt --manifest-path src-tauri/Cargo.toml --all` to
format changes. Linux CI runs the checks above.

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
- Save creates an annotated TIFF copy with ImageJ-compatible point ROI metadata.
  It never overwrites the opened source. Click with Multi-point active to add
  points; closing the panel or selecting Pan leaves placement mode. Imported
  ImageJ point selections and overlays remain editable annotations.
- Display-range sliders and percentile presets adjust only
  the displayed image. The approximate histogram samples the 8-bit preview, with
  linear counts by default and an optional Log toggle on hover. Original images
  and cached tiles stay unchanged.
- TIFF copies preserve their original layout. Stock ImageJ cannot directly open
  all tiled TIFFs or BigTIFFs; those need a suitable image reader. Other formats
  convert to a classic stripped TIFF, subject to its 4 GiB addressing limit.
- Counter assigns an ImageJ category to new points in the active selection.
  Type, size and base color apply to that selection; Label points also updates
  point overlays. Fresh counter-0 selections show point numbers; using a higher
  counter switches to category labels and ImageJ's counter colors. The question
  mark beside Counter explains this behavior. Point movement and deletion are
  not implemented. Saved ROIs use ImageJ's active selection and overlays.
- Multi-page images display their first page. Specialist microscopy metadata,
  channels, Z stacks, and timepoints are not interpreted.
- Supported display pixels are unsigned 8/16-bit grayscale, RGB, and RGBA in
  grayscale/RGB/sRGB color spaces. Signed, floating-point, CMYK, and other
  channel layouts report an unsupported-image error.
- Display previews and tiles are separate from the source pixels. They are not
  scientific data exports. Transparent images are composited against the canvas;
  unsigned 16-bit values are mapped to an 8-bit display range.
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
