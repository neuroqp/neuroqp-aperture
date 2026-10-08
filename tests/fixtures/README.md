# Small image fixtures

Run `uv run scripts/collect-test-images.py` to regenerate these fixtures and
link available local images into ignored `test-data/`. The script requires only
temporary Python dependencies. The application does not use Python.
Add `--derive-pyramid` when the `vips` executable is installed to regenerate
the 2051 by 1025 odd-size native pyramid and derive a native JPEG pyramid
from the real RGB source. The latter remains in ignored `test-data/`.

Most generated valid images store width 65 and height 49. The native SubIFD
pyramid stores 129 by 97, then 65 by 49, then 33 by 25. `exif-rotated.jpg` has
EXIF orientation 6 and should display at 49 by 65 after rotation. The odd dimensions
exercise partial final tiles. RGB images contain a spatial gradient. Gray16
corners are 0, 65535, 16384, and 49152 in top-left, top-right, bottom-left,
bottom-right order. The second page of `two-pages.tif` reverses the first page's
colors, allowing a first-page behavior check. `corrupt.tif` has a valid TIFF
header followed by a truncated directory and must fail without crashing.

| Fixture | Purpose |
| --- | --- |
| `rgb-65x49.png`, `.jpg`, `.bmp` | Ordinary formats and dimensions |
| `rgb-65x49.webp` | Additional ordinary-format support check |
| `rgba-alpha.png` | Fully transparent left half, opaque right half |
| `exif-rotated.jpg` | EXIF clockwise rotation |
| `gray16-stripped.tif` | Unsigned 16-bit, multiple uncompressed strips |
| `gray16-deflate.tif` | Unsigned 16-bit, compressed strips |
| `rgb-tiled.tif` | RGB tiles, partial edge tiles |
| `gray16-tiled-bigtiff.tif` | Unsigned 16-bit tiled BigTIFF |
| `rgb-subifd-pyramid.tif` | Tiled Deflate BigTIFF, two native reduced SubIFDs |
| `rgb-odd-subifd-pyramid.tif` | 2051 by 1025, libvips native SubIFDs with floor-rounded edges |
| `two-pages.tif` | Explicit first-page display behavior |
| `corrupt.tif` | Truncated-file error path |

The local corpus manifest records layout, dimensions, compression, page count,
file size, and original modification timestamps. Its source links cover real
PNG/JPEG, RGB TIFF, 16-bit Deflate TIFF, and a 2.53 GB stripped BigTIFF. The
vendor QPTIFF examples are TIFF containers with tiled pages and existing
pyramids, retained as outside-scope examples for future work. The initial app
rejects their canonical `.qptiff` extension, including through renamed `.tif`
links. The locally derived RGB SubIFD TIFF exercises supported native pyramid
access. Keep the corpus and original
source paths out of Git.
