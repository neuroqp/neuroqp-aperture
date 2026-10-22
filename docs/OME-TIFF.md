# OME-TIFF support

Aperture browses OME-TIFF with its own bounded OME-XML reader
(`src-tauri/src/ome_tiff.rs`, `roxmltree` without DTDs) and reads pixels with
the same libvips worker as ordinary TIFFs. OME-XML only maps each series and
(C, Z, T) plane to a member file and TIFF page. There is no Java, Bio-Formats,
Python or vendor library in the application.

## Opening

A TIFF is an OME-TIFF when its first page's ImageDescription has an `OME` root
element, so `.tif`, `.tiff`, `.ome.tif` and `.ome.tiff` names all work. A
`*.companion.ome` file can be chosen in Open (its filter lists `companion.ome`)
or given on the command line. Folder navigation lists TIFF members but not
companions, so a multi-file set is not listed twice; opening any member with the
full metadata, or with `BinaryOnly` metadata, opens the whole dataset.

`TiffData` follows the OME-TIFF specification: `IFD`, `FirstC/Z/T`,
`PlaneCount` and their defaults in every `DimensionOrder`, `UUID` with
`FileName`, and `BinaryOnly` whose `MetadataFile` is a companion XML file or
another OME-TIFF. Member files must be relative names that stay inside the
opened file's folder after resolving links; a member's own OME `UUID` must
match its reference. A `UUID` without `FileName` may only name the opened file.
Contradictory mappings, pages beyond a member's page count and unknown members
reject the dataset. Planes that no `TiffData` maps are allowed.

Metadata is limited to 32 MiB of XML, 2 M markup nodes and attributes, 64
namespace declarations, 4096 images and member files, 32 channels per image and
1 M planes. Raw markup is counted and the parser's memory reserved before
parsing; only compact summaries are kept.

## Browsing

Each OME `Image` is one Position labelled with its name. Positions must share
the first series' C/Z/T sizes, pixel type and samples; others (for example
label or macro images) are left out and the status bar notes how many. XY size
may differ between positions. The Dimensions panel works as for
[ND2](ND2.md): Position, Z, Time or Channel, with the range, previous/next
buttons, keyboard arrows or mouse wheel.

Supported pixels are unsigned 8/16-bit channels with one sample each, up to 32,
or one RGB channel (`SamplesPerPixel="3"`) stored as one three-band page per
Z/T, shown as Red, Green and Blue. 16-bit samples stay lossless 16-bit display
planes. Channel names and colors (OME's signed RGBA integer), physical X/Y/Z
sizes in any OME length unit and the time increment are kept. Signed,
floating-point and other groupings fail with a specific message.

A selection resolves to pages that libvips reads in place through job-local
aliases (`member-<i>.tif`) of the member files. Nothing is decoded into a whole
frame, so plain browsing has no frame-size limit. Each selected page is checked
against its `Pixels` size, samples and pixel type. Selecting a plane the
dataset does not store shows `OME-TIFF plane C …, Z …, T … is not stored in this
dataset.`; the previous plane is cleared and navigation continues.

Pyramids are per page. When every selected page has the same power-of-two
SubIFD reductions, they serve detail on demand, also for 16-bit samples. Other
factors, such as 3, and pages without SubIFDs get a generated pyramid, which
keeps the 8-bit fallback for 16-bit pyramids that would exceed half the display
cache. Preloading builds previews, then pyramids, of nearby stored planes in
the navigation direction within the disk-cache bounds; it never decodes frames.

## Changed files

Every member file is fingerprinted when the dataset opens, and display-cache
keys include all member identities. Before reusing a cache, after generating
previews and before publishing a cache, the members are checked again. A
changed or missing member fails the selection with `OME-TIFF dataset changed
on disk; reopen it.` and the file name. Reopening reads the metadata and
fingerprints again. Members are not held open, because datasets can have more
member files than common open-file limits.

## Max projections

Z max, Time max or both take maxima of the original samples, as for ND2, when
the projected frame (width × height × channels × bytes per sample) fits 256 MiB.
Larger frames disable the projection controls and show the reason in the
Dimensions panel. Pages are copied one at a time to a scratch file and reduced
into the accumulator; the accumulator, two pages and the read chunk are
reserved under the memory budget before any page is read. Cancellation is
checked between pages and every 64 Ki samples. An unstored plane in the
projected range fails the projection explicitly. Results use the ND2/CZI
display-TIFF path, with canonical projection cache keys, and are not preloaded.

OME-TIFF is read-only in this iteration: annotation editing and saving are
disabled in the UI and rejected by the backend, so OME-XML is never rewritten.

## Validation

Rust tests write small OME-TIFFs: 8/16-bit planes across C/Z/T, plain `.tif`
names, multi-file sets, `BinaryOnly` with a companion or an OME-TIFF metadata
file, RGB16 across Z/T with a skipped series, unstored planes, power-of-two and
factor-3 SubIFD pyramids, a changed secondary member after caching, read-only
rejection, projection maxima, admission and cancellation.

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked -- --test-threads=1
# Native check (writes fixtures to ignored test-data/ome/; see VALIDATION.md):
scripts/headless-session --driver 4446 -- python3 scripts/native-ome.py \
  src-tauri/target/debug/aperture --renderer gpu --public test-data/ome/public
```

`native-ome.py` opens files through the real GTK chooser, navigates through the
Dimensions panel and compares `.u16`/PNG previews and tiles with the generated
planes, projections with maxima computed in the script, and, with `--public`,
planes of public samples from
`downloads.openmicroscopy.org/images/OME-TIFF/2016-06/` (`tubhiswt-3D`,
`companion`, `binaryonly`, `bioformats-artificial`) with independent libvips page
reads. Reports go to `artifacts/ome/`.

## Next decisions

Signed, floating-point and `SignificantBits` display domains are Phase 3 TIFF
coverage. Pyramids that differ between the selected pages, ImageJ hyperstack
navigation, OME ROIs and annotations remain separate work.
