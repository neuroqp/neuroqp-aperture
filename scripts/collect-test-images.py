#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "Pillow", "tifffile"]
# ///
"""Generate small fixtures and link a local, read-only validation corpus."""

import argparse
import json
from pathlib import Path
import subprocess

import numpy as np
from PIL import Image
import tifffile


def metadata(path):
    item = {"bytes": path.stat().st_size}
    if path.suffix.lower() in {".tif", ".tiff", ".qptiff"}:
        with tifffile.TiffFile(path) as image:
            page = image.pages[0]
            item.update(
                width=page.imagewidth,
                height=page.imagelength,
                dtype=str(page.dtype),
                samples=page.samplesperpixel,
                layout="tiled" if page.is_tiled else "stripped",
                bigtiff=image.is_bigtiff,
                compression=page.compression.name,
                pages=len(image.pages),
                subifd_levels=(
                    [
                        {"width": p.imagewidth, "height": p.imagelength}
                        for p in page.pages
                    ]
                    if page.subifds
                    else []
                ),
            )
    else:
        with Image.open(path) as image:
            item.update(width=image.width, height=image.height, mode=image.mode)
    return item


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--sample-root", type=Path, default=Path.home() / "src/neuroqp/sample_data"
    )
    parser.add_argument(
        "--dataset-root",
        type=Path,
        default=Path.home() / "src/neuroqp-datasets/datasets",
    )
    parser.add_argument(
        "--derive-pyramid",
        action="store_true",
        help="Use installed vips to create a local native pyramid from the real RGB TIFF",
    )
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    fixtures = repo / "tests/fixtures"
    corpus = repo / "test-data"
    fixtures.mkdir(parents=True, exist_ok=True)
    corpus.mkdir(exist_ok=True)

    # Odd dimensions expose clipping at the final display tile's right/bottom edge.
    y, x = np.indices((49, 65), dtype=np.uint32)
    gray = ((x * 65535 // 64) + y * 17).astype(np.uint16)
    gray[0, 0], gray[0, -1], gray[-1, 0], gray[-1, -1] = 0, 65535, 16384, 49152
    rgb = np.stack(
        (x * 255 // 64, y * 255 // 48, (x + y) * 255 // 112), axis=-1
    ).astype(np.uint8)
    for extension in ["png", "jpg", "bmp", "webp"]:
        Image.fromarray(rgb).save(fixtures / f"rgb-65x49.{extension}")
    alpha = np.where(x < 33, 0, 255).astype(np.uint8)
    Image.fromarray(np.dstack((rgb, alpha))).save(fixtures / "rgba-alpha.png")
    exif = Image.Exif()
    exif[274] = 6  # Stored landscape pixels should display rotated clockwise.
    Image.fromarray(rgb).save(fixtures / "exif-rotated.jpg", exif=exif)
    tifffile.imwrite(
        fixtures / "gray16-stripped.tif", gray, rowsperstrip=7, metadata=None
    )
    tifffile.imwrite(
        fixtures / "rgb-tiled.tif", rgb, tile=(16, 16), photometric="rgb", metadata=None
    )
    tifffile.imwrite(
        fixtures / "gray16-tiled-bigtiff.tif",
        gray,
        tile=(16, 16),
        bigtiff=True,
        metadata=None,
    )
    tifffile.imwrite(
        fixtures / "gray16-deflate.tif",
        gray,
        compression="deflate",
        rowsperstrip=7,
        metadata=None,
    )
    py, px = np.indices((97, 129), dtype=np.uint32)
    pyramid_rgb = np.stack(
        (px * 255 // 128, py * 255 // 96, (px + py) * 255 // 224), axis=-1
    ).astype(np.uint8)
    with tifffile.TiffWriter(
        fixtures / "rgb-subifd-pyramid.tif", bigtiff=True
    ) as image:
        image.write(
            pyramid_rgb,
            subifds=2,
            tile=(16, 16),
            compression="deflate",
            photometric="rgb",
            metadata=None,
        )
        for step in [2, 4]:
            image.write(
                pyramid_rgb[::step, ::step],
                subfiletype=1,
                tile=(16, 16),
                compression="deflate",
                photometric="rgb",
                metadata=None,
            )
    with tifffile.TiffWriter(fixtures / "two-pages.tif") as image:
        image.write(rgb, photometric="rgb", metadata=None)
        image.write(255 - rgb, photometric="rgb", metadata=None)
    (fixtures / "corrupt.tif").write_bytes(b"II\x2a\x00\x08\x00\x00\x00broken")

    if args.derive_pyramid:
        oy, ox = np.indices((1025, 2051), dtype=np.uint32)
        odd_rgb = np.stack(
            (ox * 255 // 2050, oy * 255 // 1024, (ox + oy) * 255 // 3074), axis=-1
        ).astype(np.uint8)
        odd_source = corpus / "odd-pyramid-source.png"
        Image.fromarray(odd_rgb).save(odd_source)
        subprocess.run(
            [
                "vips",
                "tiffsave",
                str(odd_source),
                str(fixtures / "rgb-odd-subifd-pyramid.tif"),
                "--tile",
                "--pyramid",
                "--subifd",
                "--bigtiff",
                "--compression=deflate",
            ],
            check=True,
        )

    entries = []
    for path in sorted(fixtures.iterdir()):
        if (
            path.suffix not in {".tif", ".png", ".jpg", ".bmp", ".webp"}
            or path.name == "corrupt.tif"
        ):
            continue
        entries.append(
            {
                "name": path.name,
                "source": str(path),
                "kind": "generated",
                **metadata(path),
            }
        )
    derived = corpus / "real-rgb-subifd-pyramid.tif"
    if args.derive_pyramid:
        subprocess.run(
            [
                "vips",
                "tiffsave",
                str(corpus / "real-rgb-stripped.tif"),
                str(derived),
                "--tile",
                "--pyramid",
                "--subifd",
                "--bigtiff",
                "--compression=jpeg",
            ],
            check=True,
        )
    if derived.exists():
        entries.append(
            {
                "name": derived.name,
                "source": str(corpus / "real-rgb-stripped.tif"),
                "kind": "derived",
                **metadata(derived),
            }
        )

    selected = [
        (
            "real-preview.png",
            args.sample_root
            / "microglia_plaque_public/normalized/inspection_exports/caredio_gpnmb_iba1/channel_png_previews/channel_1/RML6_5_TH_20x_1_c1.png",
        ),
        (
            "real-contact-sheet.jpg",
            args.dataset_root
            / "microglia/data_output/proof/qa/contact_sheet_usf_20x.jpg",
        ),
        (
            "real-rgb-stripped.tif",
            args.sample_root / "10x_whole_brain/1985_F_NAc_44_OVERLAY.tif",
        ),
        (
            "real-gray16-deflate.tif",
            args.sample_root / "rat/R1C1_4x_estimate_from_20x_downsample5.tif",
        ),
        (
            "real-gray16-stripped-bigtiff.tif",
            args.dataset_root / "NC_paper/data_input/final/R1C1.tif",
        ),
        (
            "real-rgb-tiled-jpeg-pyramid.tif",
            args.sample_root
            / "formats/qptiff/perkinelmer/HandEcompressed_Scan1.qptiff",
        ),
        (
            "real-rgb-tiled-lzw-pyramid.tif",
            args.sample_root
            / "formats/qptiff/perkinelmer/HandEuncompressed_Scan1.qptiff",
        ),
        (
            "real-gray-tiled-lzw-bigtiff.tif",
            args.sample_root / "formats/qptiff/perkinelmer/LuCa-7color_Scan1.qptiff",
        ),
    ]
    for name, source in selected:
        if not source.exists():
            print(f"Skipped unavailable source: {source}")
            continue
        target = corpus / name
        if target.is_symlink():
            target.unlink()
        if target.exists():
            raise FileExistsError(f"Refusing to replace ordinary file: {target}")
        target.symlink_to(source.resolve())
        entries.append(
            {
                "name": name,
                "source": str(source),
                "kind": "source-symlink",
                "expected": (
                    "unsupported-vendor-extension"
                    if source.suffix.lower() == ".qptiff"
                    else "ready"
                ),
                "mtime_ns": source.stat().st_mtime_ns,
                **metadata(source),
            }
        )
    (corpus / "manifest.json").write_text(json.dumps(entries, indent=2) + "\n")
    print(
        f"Prepared {len(entries)} images; local source files were only read, not copied or changed."
    )


if __name__ == "__main__":
    main()
