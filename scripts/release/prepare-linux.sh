#!/usr/bin/env bash
# Run as root inside the Debian 12 build container.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends ca-certificates curl git build-essential pkg-config python3 meson ninja-build cmake patchelf file squashfs-tools xz-utils libfuse2 libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libtiff-dev libtiff-tools libjpeg-dev libpng-dev libwebp-dev libheif-dev libexif-dev liblcms2-dev libexpat1-dev liborc-0.4-dev libarchive-dev
mkdir -p build/linux build/release
# Restore the reviewed build-environment versions that bookworm-security has
# since superseded. The native identity gate still requires the exact set.
snapshot=https://snapshot.debian.org/archive/debian-security/20260908T094315Z/pool/updates/main
curl --fail --location --retry 3 "$snapshot/c/ca-certificates/ca-certificates_20250419~deb12u1_all.deb" -o build/linux/ca-certificates.deb
curl --fail --location --retry 3 "$snapshot/l/linux/linux-libc-dev_6.1.187-1_amd64.deb" -o build/linux/linux-libc-dev.deb
sha256sum --check <<'SUMS'
62b08a77d985d4253894b1f69aebda5925034ca4e294add364167fad8cb64a44  build/linux/ca-certificates.deb
ebcd139c31438e19f161bcf277a38708402ddd0e3acbe208cbbb27bff1c52bb9  build/linux/linux-libc-dev.deb
SUMS
dpkg -i build/linux/ca-certificates.deb build/linux/linux-libc-dev.deb
curl --fail --location --retry 3 https://github.com/libvips/libvips/releases/download/v8.18.0/vips-8.18.0.tar.xz -o build/linux/vips.tar.xz
echo 'b85ab92280c30d22f5c8fe2f68b809cddb7eaac437d8c33474475dac84ddc574  build/linux/vips.tar.xz' | sha256sum --check
tar -xf build/linux/vips.tar.xz -C build/linux
meson setup build/linux/vips-build build/linux/vips-8.18.0 --prefix=/usr -Dintrospection=disabled -Dvapi=false -Dexamples=false -Dcplusplus=false -Dmodules=disabled -Darchive=enabled -Djpeg=enabled -Dpng=enabled -Dtiff=enabled
meson compile -C build/linux/vips-build
meson install -C build/linux/vips-build
ldconfig
pkg-config --atleast-version=8.18.0 vips
pkg-config --atleast-version=4.5.0 libtiff-4
vips -l dzsave
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
