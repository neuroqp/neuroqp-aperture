# Initial ND2 support

Aperture reads Nikon ND2 through the pinned `openreadout-nd2` and
`openreadout-core` 0.3.0 crates. Format parsing and pixel decoding run in Rust
on a loader thread. Native preview and pyramid operations remain in isolated
image workers. There is no Python, Java, TypeScript reader,
vendor SDK, or whole Bio-Formats checkout in the application.

## Browsing

Open ND2 through Open, folder navigation, or a filename on the command line.
The Dimensions toolbar button toggles one panel. Choose Position, Z, Time or
Channel and use its range, previous/next buttons, keyboard arrows, or mouse
wheel over the range. Channel cycles through Composite and named channels.
Rapid input keeps only the latest requested coordinates while native requests
run in sequence. Compatible Color settings and viewport bounds survive frame
changes. These controls do not add Z/time navigation to ordinary TIFF stacks.

Supported inputs are unsigned 8/16-bit scalar acquisition channels, up to 32
samples, and a single RGB acquisition channel. The reader handles modern
uncompressed and zlib ND2 plus legacy JPEG 2000 ND2. Unsupported pixel types,
modern lossy compression, mixed RGB acquisition channels, custom experiment
loops and time sub-loops return explicit errors.

Channel names/colors and position names are retained. Available X/Y pixel sizes,
Z spacing and nominal time interval are retained in image state and display-cache
metadata; the panel shows this calibration. The nominal interval is acquisition
metadata, not a substitute for per-frame timestamps.

## Display and limits

The loader decodes whole position/Z/time frames, including their channels,
and writes temporary display TIFFs for the existing preview/pyramid pipeline.
Original unsigned samples remain in a shared-budget LRU, so revisiting a frame
can avoid ND2 decoding. The current preview and detail finish first. Idle loading
prioritizes nearby frames in the last navigation direction, then remaining
previews, then remaining detail pyramids. Native optional work is cancelled on
navigation; a single Rust frame decode finishes before yielding.
Cache identity includes the source fingerprint and selected coordinates. Completed
display caches and preloaded previews are reused within the existing bounded disk cache. Changing the
displayed channel does not reread ND2 or regenerate tiles.

For 16-bit data, taking only the high byte would erase low-valued fluorescence.
The disposable display cache instead maps each selected scalar channel's minimum
and maximum to 0–255. RGB uses one shared range. Constant zero planes stay black;
constant nonzero planes become white. The original ranges remain in image state
and the cache manifest. Eight-bit samples keep their values. This is display
normalization; it does not modify the ND2 source or provide a scientific export.
Brightness across frames or channels is not quantitatively comparable after this
automatic normalization.

Settings > General sets the image cache and decoding budget, default 4 GiB,
shared across all Aperture windows. Retained raw frames, parser/codec workspaces,
native worker allowances and a fixed browser-image allowance are admitted before
allocation. Source handles stay pinned; changed sources invalidate retained data.
Optional caches are reclaimed before foreground work, including across windows.
Lower limits block new admission while existing operations settle. Under system
memory pressure, optional raw frames are dropped and preloading pauses until a
larger free-memory margin returns. Display resolution and source pixels do not
change with this setting.

This is an upper limit for accounted image data and conservative workspace
allowances, not an OS-enforced total RSS cap. WebView/runtime overhead and GPU
memory are separate. OSD's channel-aware tile-cache target remains soft because
visible tiles must stay available. The disk cache has its own 2 GiB limit.
A decoded selected frame is additionally limited to 256 MiB. Encoded frames are
limited to 257 MiB, positions to 4096, and the serialized position/channel summary
to 48 KiB. Encoded and recursively expanded metadata are capped at 32 MiB
before reader parsing. Opening reserves JSON/position-cloning workspaces, then
retains compact typed summaries instead of full vendor trees. The integration
validates storage geometry and actual chunk headers
before pixel decoding. Whole-frame decoding remains the principal scaling limit;
region decoding and time playback are not implemented.

ND2 is read-only in this iteration. Annotation loading, editing and saving are
disabled in the UI and rejected by the backend. Vendor ROIs, acquisition events,
full vendor metadata inspection, ND2 writing, dimension-aware annotation
persistence and scientific export remain separate work. No ND2 installer/default
application association was added.

## Validation and reproducibility

Mandatory Rust tests generate small ND2 containers in memory. They check exact
P/Z/T/channel addressing, unsigned 8/16-bit values, RGB sample order, display
normalization, malformed actual chunk lengths, unchanged source bytes, backend
navigation, invalid selections and coordinate-specific cache reuse. Budget tests
cover shared admission, shrinking/lowering, crash cleanup, pressure hysteresis,
raw-cache eviction, source replacement and chunk-map validation. Native memory
checks exercise Settings persistence, preload completion, plane hashes, visible
continuity and injected preview failure recovery. Frontend
tests cover axis indexing, channel display filtering, compatible color layouts,
stale replies, cancellation, request coalescing and source replacement.

The optional public-file Rust check compares exact raw pixels against the
independent expectations in the BSD-licensed
[Python nd2 tests](https://github.com/tlambert03/nd2/tree/249e585706f948d24a66459facb07d541a31c9fd/tests).
The native harness supplies only the file-picker response. UI handlers, Rust IPC,
reader, display caches and rendered pixels use the real Linux application.

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked -- --test-threads=1
cargo test --manifest-path src-tauri/Cargo.toml --locked public_nd2_metadata -- --ignored
pnpm test
pnpm build
cargo build --manifest-path src-tauri/Cargo.toml --locked --features custom-protocol
# Start tauri-driver with owned XDG_CACHE_HOME and TMPDIR, then:
python3 scripts/native-nd2.py src-tauri/target/debug/aperture
python3 scripts/native-nd2.py src-tauri/target/debug/aperture --renderer cpu
python3 scripts/native-memory.py src-tauri/target/debug/aperture
```

Public acquisitions and generated evidence stay in ignored `test-data/nd2/` and
`artifacts/nd2/`. The required native sample is
`dims_p4z5t3c2y32x32.nd2`, also available as
[AICS ND2_dims_p4z5t3c2y32x32.nd2](https://aics-modeling-packages-test-resources.s3.us-west-2.amazonaws.com/aicsimageio/test_resources/resources/ND2_dims_p4z5t3c2y32x32.nd2).
Additional samples come from the
[upstream public test corpus](https://github.com/tlambert03/nd2/blob/249e585706f948d24a66459facb07d541a31c9fd/scripts/download_samples.py):
`dims_t3c2y32x32.nd2`, `compressed_lossless.nd2`, `1.2audrosophila.nd2`,
`dims_p4z5t3c2y32x32-tile2x3.nd2`, `aryeh_but3_cont200-1.nd2`, and the explicitly
unsupported `dims_rgb_t3p2c2z3x64y64.nd2`. The separate
[AICS RGB fixture](https://aics-modeling-packages-test-resources.s3.us-west-2.amazonaws.com/aicsimageio/test_resources/resources/ND2_dims_rgb.nd2)
is saved locally as `rgb.nd2`.

## Next decisions

Prioritize persistent source-intensity display ranges across frames, large-frame
memory/latency measurements on representative laboratory acquisitions, and a
dimension-aware annotation format before calling this complete ND2 support.
Broader vendor metadata, ROIs and irregular experiment loops should follow actual
sample requirements. The dependency currently enables codecs for other formats
through its supporting codec crate; an upstream feature split could reduce this
dependency footprint without maintaining our own ND2 parser.
