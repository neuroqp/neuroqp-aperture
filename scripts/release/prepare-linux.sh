#!/usr/bin/env bash
# Run as root inside the Debian 12 build container.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends ca-certificates curl git build-essential pkg-config python3 meson ninja-build cmake patchelf file squashfs-tools xz-utils libfuse2 libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libtiff-dev libtiff-tools libjpeg-dev libpng-dev libwebp-dev libheif-dev libexif-dev liblcms2-dev libexpat1-dev liborc-0.4-dev
mkdir -p build/linux build/release
curl --fail --location --retry 3 https://github.com/libvips/libvips/releases/download/v8.18.0/vips-8.18.0.tar.xz -o build/linux/vips.tar.xz
echo 'b85ab92280c30d22f5c8fe2f68b809cddb7eaac437d8c33474475dac84ddc574  build/linux/vips.tar.xz' | sha256sum --check
tar -xf build/linux/vips.tar.xz -C build/linux
meson setup build/linux/vips-build build/linux/vips-8.18.0 --prefix=/usr -Dintrospection=disabled -Dvapi=false -Dexamples=false -Dcplusplus=false -Dmodules=disabled
meson compile -C build/linux/vips-build
meson install -C build/linux/vips-build
ldconfig
pkg-config --atleast-version=8.18.0 vips
pkg-config --atleast-version=4.5.0 libtiff-4
python3 - <<'PY'
import json, subprocess
from pathlib import Path
run = lambda *a: subprocess.check_output(a, text=True).strip()
Path('build/linux/dependencies.json').write_text(json.dumps({'baseline': run('cat','/etc/debian_version'), 'libvips':run('pkg-config','--modversion','vips'), 'libtiff':run('pkg-config','--modversion','libtiff-4'), 'vips_source_sha256':'b85ab92280c30d22f5c8fe2f68b809cddb7eaac437d8c33474475dac84ddc574', 'debian_packages':run('dpkg-query','-W','-f=${Package} ${Version}\n')}))
with Path('build/linux/native-notices.txt').open('w') as output:
    output.write('libvips 8.18.0\n' + Path('build/linux/vips-8.18.0/LICENSE').read_text())
    for path in sorted(Path('/usr/share/doc').glob('*/copyright')):
        output.write('\n\n' + str(path) + '\n' + path.read_text(errors='replace'))
PY
