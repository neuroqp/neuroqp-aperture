#!/usr/bin/env bash
# Inspect bundled files only. Full worker regressions run in Stage A.
set -euo pipefail
artifact=$(realpath "${1:-build/release/Aperture-x86_64.AppImage}")
folder=$(mktemp -d)
trap 'rm -rf "$folder"' EXIT
chmod +x "$artifact"
(cd "$folder" && "$artifact" --appimage-extract >/dev/null)
test -x "$folder/squashfs-root/AppRun"
for executable in aperture vips vipsheader tiffinfo; do
  test -x "$folder/squashfs-root/usr/bin/$executable"
done
for file in LICENSE.txt BUILDINFO.json THIRD-PARTY-NOTICES.txt; do
  test -s "$folder/squashfs-root/usr/share/aperture/$file"
done
find "$folder/squashfs-root" -name 'libvips.so*' -print -quit | grep -q .
