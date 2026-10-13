#!/usr/bin/env bash
set -euo pipefail
mkdir -p build/release
python3 scripts/release/release.py buildinfo linux build/release build/linux/dependencies.json
python3 scripts/release/notices.py linux build/linux/native-notices.txt build/release/THIRD-PARTY-NOTICES-linux.txt
python3 - <<'PY'
import json
from pathlib import Path
files = {'usr/bin/vips':'/usr/bin/vips', 'usr/bin/vipsheader':'/usr/bin/vipsheader', 'usr/bin/tiffinfo':'/usr/bin/tiffinfo', 'usr/share/aperture/LICENSE.txt':str(Path('LICENSE').resolve()), 'usr/share/aperture/BUILDINFO.json':str(Path('build/release/BUILDINFO-linux.json').resolve()), 'usr/share/aperture/THIRD-PARTY-NOTICES.txt':str(Path('build/release/THIRD-PARTY-NOTICES-linux.txt').resolve())}
Path('build/linux/bundle.json').write_text(json.dumps({'bundle':{'linux':{'appimage':{'files':files}}}}))
PY
pnpm exec tauri build --bundles appimage --config build/linux/bundle.json "$@" -- --locked
shopt -s nullglob
images=(src-tauri/target/release/bundle/appimage/*.AppImage)
test "${#images[@]}" -eq 1
cp "${images[0]}" build/release/Aperture-x86_64.AppImage
if test -f "${images[0]}.sig"; then cp "${images[0]}.sig" build/release/Aperture-x86_64.AppImage.sig; fi
