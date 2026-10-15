#!/usr/bin/env bash
set -euo pipefail
test "$#" -eq 0
runtime=$(realpath "${LINUX_RUNTIME_DIR:?Stage the verified Linux runtime first}")
test -s "$runtime/manifest.json"
mkdir -p build/release
test ! -e build/release/Aperture-x86_64.AppImage
cp "$runtime/runtime/usr/share/aperture/BUILDINFO.json" build/release/BUILDINFO-linux.json
cp "$runtime/runtime/usr/share/aperture/THIRD-PARTY-NOTICES.txt" build/release/THIRD-PARTY-NOTICES-linux.txt
export PATH="$runtime/tools/usr/bin:$PATH"
ARCH=x86_64 "$runtime/tools/usr/bin/appimagetool" --runtime-file "$runtime/tools/runtime-x86_64" "$runtime/runtime" build/release/Aperture-x86_64.AppImage
