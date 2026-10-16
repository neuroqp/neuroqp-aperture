# NeuroQP Aperture

A minimal desktop image viewer built with Tauri, Rust, and OpenSeadragon.
Two compact toolbars leave the rest of the window to the image. Large images
load through a preview and progressively available detail tiles.

Color and multi-point options open independently at the viewer's top-right.
Color connects image channels to editable output colors, with a live display
range, brightness, source histogram, presets and Reset per connection. Drag wires to reroute or remove them; click an
output to pick its shade. These settings affect only the display.
WebGL2 keeps image samples on the GPU and redraws cached tiles when colors change;
unavailable or lost GPU contexts fall back to CPU rendering. No additional Rust
renderer or graphics dependency is required. Loading and pyramid generation are
separate from this rendering path.
Point type, color, and size buttons preview the current combination. The header's
sun/moon button switches light and dark modes and remembers your choice; the
initial theme follows the system.

## Multiple windows and file opening

Each Aperture window displays one image. Launch another copy or press **Ctrl+N**
to compare images independently. **Open** and folder navigation
replace only the image in their current window.

You can pass an image filename when launching Aperture:

```sh
~/Applications/Aperture-x86_64.AppImage "/path/to/cells étude.tif"
```

Overwriting warns when the image is open in another Aperture window or changed
on disk. Choose Cancel, Save as or Overwrite anyway. Other windows remain editable;
annotations are not merged automatically. Close other Aperture windows before
installing an update. When upgrading from 0.1.1, close all its windows first,
because that older version does not participate in the new coordination.

For a Linux TIFF association, create
`~/.local/share/applications/aperture.desktop` with the following content. Replace
the example executable path with your AppImage's absolute path:

```ini
[Desktop Entry]
Type=Application
Name=Aperture
Exec=/home/you/Applications/Aperture-x86_64.AppImage %f
Terminal=false
MimeType=image/tiff;
Categories=Education;Science;
```

Then set and verify the default:

```sh
xdg-mime default aperture.desktop image/tiff
xdg-mime query default image/tiff
```

Keep the AppImage at that path when updating. Windows Setup registers TIFF file
opening; choose Aperture in Windows' Open with/default-app settings if needed.

## Downloads and updates

Release packages are published on [GitHub Releases](https://github.com/neuroqp/neuroqp-aperture/releases).
Available packages are:

| Platform     | Download                    | Installation and updates                                                                                                                              |
| ------------ | --------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Windows x64  | `Aperture-Setup-x64.exe`    | Installs for your user without administrator access; updates from the app.                                                                            |
| Windows x64  | `Aperture-Portable-x64.exe` | Run the downloaded file; it extracts its bundled runtime into your local application-data directory. Download and replace the executable for updates. |
| Linux x86_64 | `Aperture-x86_64.AppImage`  | Mark executable and run from a writable folder; updates from the app.                                                                                 |

Routine release validation covers Windows 11 under a standard user. Windows 10
compatibility is unverified.

Both Windows editions include their image libraries and a fixed WebView2 runtime.
No separate runtime download is needed. Publisher signing is pending, so Windows
may display an unknown-publisher warning; institutional application policies can
still prevent execution. The portable edition also needs writable local storage,
and its first launch takes longer while it extracts its runtime.

On Linux, download to a folder you own, then run:

```sh
chmod +x Aperture-x86_64.AppImage
./Aperture-x86_64.AppImage
```

If FUSE is unavailable, use `./Aperture-x86_64.AppImage --appimage-extract-and-run`.
Packages target a Debian 12 baseline. Every release is checked on Debian 12 and
Ubuntu 24 with a native Wayland session. Arch/Omarchy and Fedora receive periodic
checks and checks when runtime dependencies or desktop integration change; they
are compatibility targets, without a per-release guarantee. macOS, ARM, distro
packages, and app stores are outside the current release scope.

The header's update button shows the installed version, release notes, and a
manual check. Automatic checks run at most once a day and can be disabled. Updates
are downloaded and signature-verified before you choose to install and restart;
unsaved annotations can be saved, discarded, or kept by cancelling. Portable
updates open the new portable download in your browser. Settings and image caches
live outside the downloaded executable and survive replacement.

See [RELEASING.md](RELEASING.md) for build, testing, signing, and publication steps.

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

For Settings and update-restart regression checks, start `tauri-driver` with
isolated `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` and `TMPDIR`, then run:

```sh
python3 scripts/native-settings.py src-tauri/target/debug/aperture
python3 scripts/native-update-recovery.py src-tauri/target/debug/aperture
```

These checks use real native image loading, annotations and saving where relevant,
with supplied file-chooser and updater responses. They never install an update.
Results and screenshots stay in ignored `artifacts/` directories.

## ImageJ annotation validation

Run the optional interoperability check against a local ImageJ installation:

```bash
python3 scripts/check-imagej-interop.py /path/to/ImageJ/ij.jar --java-home /path/to/jdk
```

It generates synthetic TIFFs, round-trips editable ROIs through Aperture and
ImageJ, and checks coordinates, metadata, and exact image pixel preservation.
No microscopy source files are modified.

## Proof-of-concept boundaries

- Linux is the primary development target. Windows and Linux release packages
  require the platform acceptance checks in [RELEASING.md](RELEASING.md).
- Save/Ctrl+S offers Overwrite, Save as…, or Cancel for TIFFs. Overwrite updates
  ImageJ-compatible annotations at the current path while preserving pixels and
  unrelated metadata. Other formats default to a sibling file with the same
  name and a .tif extension; existing destinations require confirmation.
  Save as… opens the file picker and remembers the chosen path for later saves.
  Click with Multi-point active to add
  points; closing the panel or selecting Pan leaves placement mode. Imported
  ImageJ point selections and overlays remain editable annotations.
- Select a wire in Color to adjust its Minimum/Maximum range and Brightness.
  Drag the graph's black/white cutoff lines horizontally or its right-hand
  plateau vertically for brightness. Hover highlights each target and shows
  its drag direction. Brightness runs from 0 to 100% and scales the channel's contribution without
  changing the input white point. The approximate source histogram uses 8-bit
  display samples, with linear counts and a Linear/Log toggle below the graph.
  Percentile presets change only the input range; Reset restores 0–255 and
  100% brightness for that connection. Original images and cached tiles stay
  unchanged.
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
- BMP decoding uses a 512 MiB decoded-allocation limit; the large-image streaming
  paths apply to TIFF, not BMP.
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

## License

Aperture is [MIT licensed](LICENSE). Bundled third-party components retain their
own licenses; release downloads include their notices.
