#!/usr/bin/env python3
"""Exercise packaged image worker decoding and TIFF writing without a desktop."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib


def png(path):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    rows = b"".join(b"\0" + bytes([x * 17, y * 17, 127] * 16) for y, x in zip(range(16), range(16)))
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 16, 16, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def bmp(path):
    pixels = b"".join(bytes([127, y * 17, x * 17]) for y in range(16) for x in range(16))
    path.write_bytes(b"BM" + struct.pack("<IHHI", 54 + len(pixels), 0, 0, 54) + struct.pack("<IiiHHIIiiII", 40, 16, 16, 1, 24, 0, len(pixels), 0, 0, 0, 0) + pixels)


def smoke(command):
    with tempfile.TemporaryDirectory(prefix="aperture-package-") as folder:
        folder = Path(folder)
        source = folder / "source.png"
        png(source)
        original = hashlib.sha256(source.read_bytes()).hexdigest()
        converted = folder / "converted.tif"
        preview = folder / "preview.jpg"
        bitmap = folder / "source.bmp"
        bmp(bitmap)
        bitmap_tiff = folder / "bitmap.tif"
        requests = [{"Command": {"program": "vips", "args": ["copy", str(source), str(converted)]}}, {"RawPreview": {"input": str(converted), "output": str(preview), "max_edge": 32, "threads": 1}}, {"Command": {"program": "vips", "args": ["copy", str(bitmap), str(bitmap_tiff)]}}]
        process = subprocess.run(command + ["--image-worker"], input="".join(json.dumps(r) + "\n" for r in requests), capture_output=True, text=True, timeout=60, env=os.environ.copy())
        if process.returncode:
            raise RuntimeError(f"Packaged image worker failed: {process.stderr}")
        results = [json.loads(line)["result"] for line in process.stdout.splitlines() if "result" in json.loads(line)]
        if len(results) != len(requests) or any("Err" in result for result in results):
            raise RuntimeError(f"Packaged TIFF conversion/preview failed: {results}; {process.stderr}")
        if not converted.read_bytes().startswith((b"II*\0", b"MM\0*")) or not bitmap_tiff.read_bytes().startswith((b"II*\0", b"MM\0*")) or not preview.is_file() or preview.stat().st_size < 256:
            raise RuntimeError("Packaged worker did not produce TIFF and preview pixels")
        if hashlib.sha256(source.read_bytes()).hexdigest() != original:
            raise RuntimeError("Worker changed its source image")
        print("Packaged worker: PNG/BMP decode, TIFF write/read and preview passed. Desktop interaction still requires acceptance.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", nargs="+", help="Packaged executable and any launcher arguments")
    smoke(parser.parse_args().executable)
