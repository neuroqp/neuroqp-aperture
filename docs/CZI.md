# Initial CZI support

Aperture reads Zeiss CZI through the pinned `openreadout-czi` and
`openreadout-core` 0.3.0 crates. Format parsing, tile stitching and pixel
decoding run in Rust on a loader thread. Native preview and pyramid operations
remain in isolated image workers. There is no Python, Java, TypeScript reader,
vendor SDK or libCZI in the application.

## Browsing

Open CZI through Open, folder navigation, or a filename on the command line. The
Dimensions panel works as for [ND2](ND2.md): choose Position, Z, Time or Channel
and use its range, previous/next buttons, keyboard arrows or mouse wheel.

Each image the reader exposes becomes one Position, labelled with its scene name
from the metadata. Unnamed scenes are `Scene <S index>` when a file has several
scenes, and `Position 1` otherwise. Subblocks that differ along H, I, R, V or B
(for example SIM phases) are separate positions named with their coordinates.
Super-resolved renderings are also separate images, but they usually differ in
channel count from the other images and are then rejected. All positions must
share Z, time, channel count and pixel type; otherwise opening fails with an
explicit error. XY size may differ between
positions, as in multi-scene mosaics whose scenes differ by a few rows. Each frame
keeps its own geometry and display cache. The viewport is retained in
image-relative coordinates, so a differently sized scene shows the same relative
region rather than the same pixel scale.

Mosaic tiles are stitched on read in ascending M order; later tiles win at seams
and uncovered pixels are zero. Supported inputs are unsigned 8/16-bit grayscale
channels, up to 32, and one RGB acquisition channel stored as Bgr24, Bgr48 or
Bgra32. RGB is shown in red, green, blue order; Bgra32 alpha is dropped. The
reader decodes uncompressed, LZW, Zstd (including HiLo), chunked, JPEG and
JPEG XR subblocks.

Explicitly rejected: float, int32, double and complex pixel types; mixed pixel
types; multi-file documents and any subblock that references another file part;
inconsistent scenes; files without an intact subblock directory; JPEG XR
subblocks whose codestream geometry or pixel format disagrees with the directory;
and JPEG-lossless or camera/system raw subblocks, which the
reader does not decode. Only full-resolution level 0 is read; stored pyramid
levels are not used. Airyscan fast-scan subblocks stored below their logical size
are not upsampled by the reader.

Channel names and colors come from the metadata XML and its display settings.
X/Y/Z pixel sizes come from `Scaling`, and the nominal time interval from the T
increment. The panel shows this calibration. The interval is acquisition
metadata, not a substitute for per-frame timestamps.

## Max projections

Z max, Time max, or both reduce the current position exactly as for ND2: maxima
are taken from original unsigned samples before display normalization, inputs
stream through one accumulator, cached frames are borrowed, and projections have
separate canonical cache keys. RGB keeps its sample order and shared range.
Projections are display-only and not preloaded.

## Display and limits

The loader decodes one whole position/Z/time frame, including its channels, and
writes a temporary display TIFF for the existing preview/pyramid pipeline.
Sixteen-bit channels are normalized per selected frame for display, with one
shared range for RGB, and original ranges stay in image state and the cache
manifest. Original samples are retained under the shared memory budget and
preloading follows the same order as ND2. Cache keys include the source identity,
selected coordinates and a CZI-specific decoder version.

A decoded selected frame is limited to 256 MiB. Larger stitched scenes, such as
whole-slide scans, are rejected with an error that names the frame size. Browsing
starts at the first position, so a file whose first position is too large cannot
be opened even if later positions fit. Region or pyramid-level streaming for
those scenes is future work.

Before the reader allocates, Aperture reads the file header and checks the
metadata XML size, the intact subblock directory, every full-resolution subblock
header and the entry embedded in it, which must agree with the directory, every
JPEG XR codestream header, the attachment directory, the TimeStamps/EventList
attachments the reader interprets and every segment header. Event lists are
walked as the reader walks them, so overlapping descriptions are counted at their
full expanded size.
The XML, attachment directory and interpreted attachments are capped at 32 MiB
together; the subblock directory has its own 128 MiB cap. Encoded subblock
payloads are limited to 257 MiB, decoded subblocks to 256 MiB and images to 4096.
Aperture parses the XML once under a reservation to measure its strings. Opening
then reserves the reader's XML, directory and segment-header workspaces plus, for
every image, copies of those strings, listed channels, time stamps and expanded
event records, and afterwards retains compact typed summaries.

Frame decoding reserves the reader's reopening workspace and its tile batch for
the selected scene: as many tiles as the reader decodes at once (the thread count,
limited by its 256 MiB output batch and the most subblocks one plane can use),
each with its payload, mask and decode copies, plus both the reader's plane and
the assembled frame. JPEG may decode up to four times its declared size. JPEG XR
codestreams must declare the directory's tile size and sample layout, so their
output is bounded; each JPEG XR tile also reserves the decoder's index table (at
most 8 bytes per payload byte), per-tile-column entropy and quantizer state for
the column count in its header, and macroblock-row workspace for up to 16
internal channels. This bounds accounted image data and conservative workspaces,
not total RSS. The reader is reopened for each decoded
frame and re-reads segment headers, which costs latency on files with many
subblocks.

CZI is read-only in this iteration. Annotation loading, editing and saving are
disabled in the UI and rejected by the backend. Vendor ROIs, attachments, full
metadata inspection, CZI writing and scientific export remain separate work.

## Validation and reproducibility

Mandatory Rust tests write small CZI containers in memory. They check exact
scene/Z/time/channel addressing, 16-bit display normalization, BGR24, BGR48 and
BGRA32 sample order, two-tile stitching, Z projection, scenes of different width
through the backend, unchanged source bytes, per-plane decode batches, payload
offsets from embedded subblock entries, JPEG XR header and tile-column checks,
per-image XML and overlapping event-description admission, and rejection of float and mixed
pixel types, inconsistent scenes, oversized frames and first positions,
multi-file parts, overflowing segments, oversized XML or directories and missing
directories.

The optional public-file check compares decoded samples against independent
[czifile](https://github.com/cgohlke/czifile) reference values in
`test-data/czi/expected.json`. It checks dimensions, channel names, per-plane
minimum and maximum, and exact 4×4 pixel blocks, including mosaic seams and the
JPEG XR preview. It also checks that the two whole-slide fixtures are rejected.

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked -- --test-threads=1
cargo test --manifest-path src-tauri/Cargo.toml --locked public_czi -- --ignored
# Start tauri-driver with owned XDG_CACHE_HOME and TMPDIR, then:
python3 scripts/native-czi.py src-tauri/target/debug/aperture
```

Public fixtures, their sources, licences and reference script stay in ignored
`test-data/czi/`; native evidence stays in `artifacts/czi/`. They come from Zeiss
and community samples on Zenodo and the aicsimageio test resources; see
`test-data/czi/README.txt` and `provenance.json`.

## Next decisions

Prioritize region or pyramid-level streaming for whole-slide scenes, retaining a
reader between frames to avoid repeated directory walks, and persistent
source-intensity display ranges. Multi-file documents, float pixel types and
dimension-aware annotations should follow actual sample requirements.
