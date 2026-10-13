#!/usr/bin/env bash
# Package worker smoke only; this does not prove native desktop acceptance.
set -euo pipefail
artifact=$(realpath "${1:-build/release/Aperture-x86_64.AppImage}")
folder=$(mktemp -d)
trap 'rm -rf "$folder"' EXIT
chmod +x "$artifact"
(cd "$folder" && "$artifact" --appimage-extract >/dev/null)
test -x "$folder/squashfs-root/AppRun"
test -f "$folder/squashfs-root/usr/lib/libvips.so.42" || find "$folder/squashfs-root" -name 'libvips.so*' -print -quit | grep -q .
export PATH="$folder/squashfs-root/usr/bin:/usr/bin:/bin"
export LD_LIBRARY_PATH="$folder/squashfs-root/usr/lib"
# The normal backend uses these subprocesses for non-TIFF inputs and metadata.
python3 - "$folder" <<'PY'
import importlib.util, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location('smoke', 'scripts/release/smoke-worker.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.png(Path(sys.argv[1]) / 'input.png')
PY
"$folder/squashfs-root/usr/bin/vips" copy "$folder/input.png" "$folder/output.tif"
"$folder/squashfs-root/usr/bin/vipsheader" -a "$folder/output.tif" | grep -q width
"$folder/squashfs-root/usr/bin/tiffinfo" "$folder/output.tif" | grep -q 'Image Width'
python3 scripts/release/smoke-worker.py "$folder/squashfs-root/AppRun"
