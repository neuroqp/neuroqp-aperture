# Local Linux upgrade fixture

Build the first-release `0.0.9` prior AppImage from the exact reviewed `0.1.1`
candidate commit. Keep the candidate AppImage, signature and BUILDINFO in their
original checkout or download directory. This recipe uses a separate local QA
feature checkout and the package workflow's Debian 12 baseline and toolchains.
It records build provenance; it does not promise byte-identical package rebuilds.

Run from the source repository with Docker available. Replace `SOURCE_SHA` with
the full candidate `BUILDINFO-linux.json` `source_sha`, already reviewed for release.
Do not use a moving branch name. The temporary QA branch is never pushed.

```bash
set -euo pipefail
unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
export SOURCE_SHA=REPLACE_WITH_REVIEWED_40_CHARACTER_COMMIT
[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]
export QA_ROOT=$(mktemp -d /tmp/aperture-linux-prior.XXXXXX)
git clone --no-checkout https://github.com/neuroqp/neuroqp-aperture.git "$QA_ROOT"
git -C "$QA_ROOT" switch --no-track -c qa/linux-upgrade-prior "$SOURCE_SHA"
docker run --rm -e SOURCE_SHA -v "$QA_ROOT:/work" -w /work \
  debian:12.12-slim@sha256:d5d3f9c23164ea16f31852f95bd5959aad1c5e854332fe00f7b3a20fcc9f635c bash -se <<'BUILD'
set -euo pipefail
unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
export APPIMAGE_EXTRACT_AND_RUN=1 CARGO_INCREMENTAL=0
apt-get update && apt-get install -y --no-install-recommends git ca-certificates curl xz-utils
bash scripts/release/prepare-linux.sh
curl --fail --location https://nodejs.org/dist/v22.22.0/node-v22.22.0-linux-x64.tar.xz -o /tmp/node.tar.xz
curl --fail --location https://nodejs.org/dist/v22.22.0/SHASUMS256.txt -o /tmp/node-shasums.txt
NODE_HASH=$(awk '$2=="node-v22.22.0-linux-x64.tar.xz" {print $1}' /tmp/node-shasums.txt)
printf '%s  /tmp/node.tar.xz\n' "$NODE_HASH" | sha256sum --check
tar -xf /tmp/node.tar.xz --strip-components=1 -C /usr/local
curl --fail --location https://sh.rustup.rs -o /tmp/rustup.sh
sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.98.0
. "$HOME/.cargo/env"
corepack enable && corepack prepare pnpm@9.15.9 --activate
test "$(git -c safe.directory="$PWD" rev-parse HEAD)" = "$SOURCE_SHA"
test "$(python3 scripts/release/release.py check)" = 0.1.1
python3 scripts/release/release.py version 0.0.9
pnpm install --frozen-lockfile
python3 - <<'PY'
import json, shutil
from pathlib import Path
config = json.loads(Path('src-tauri/tauri.conf.json').read_text())['plugins']['updater']
assert config['requireSignedVersion'] is True and config['pubkey']
updater = {**config, 'endpoints': ['http://127.0.0.1:8763/latest.json'], 'dangerousInsecureTransportProtocol': True}
out = Path('build/linux-upgrade-fixture'); out.mkdir()
(out / 'overlay.json').write_text(json.dumps({'bundle': {'createUpdaterArtifacts': False}, 'plugins': {'updater': updater}}))
for name in ('package.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock'):
    target = out / 'test-deltas' / name; target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(name, target)
PY
bash scripts/release/package-linux.sh --config build/linux-upgrade-fixture/overlay.json
python3 - <<'PY'
import hashlib, json, os
from pathlib import Path
out = Path('build/linux-upgrade-fixture')
info = json.loads(Path('build/release/BUILDINFO-linux.json').read_text())
assert info['version'] == '0.0.9' and info['source_sha'] == os.environ['SOURCE_SHA']
def sha(path):
    with path.open('rb') as source: return hashlib.file_digest(source, 'sha256').hexdigest()
paths = sorted(out.rglob('*')) + [Path('build/release') / name for name in ('Aperture-x86_64.AppImage', 'BUILDINFO-linux.json', 'THIRD-PARTY-NOTICES-linux.txt')] + [Path('build/linux/dependencies.json')]
receipt = {'purpose': 'Private unsigned Linux desktop acceptance fixture', 'base_source_sha': info['source_sha'], 'candidate_version': '0.1.1', 'prior_version': info['version'], 'toolchains': info['toolchains'], 'native_dependencies': info['native_dependencies'], 'files_sha256': {str(path): sha(path) for path in paths if path.is_file()}}
(out / 'FIXTURE.json').write_text(json.dumps(receipt, indent=2) + '\n')
PY
BUILD
```

The prior package is `$QA_ROOT/build/release/Aperture-x86_64.AppImage`; provenance,
overlay and all four replacement version files are under
`$QA_ROOT/build/linux-upgrade-fixture`. Keep those files with the acceptance result.
The package helper reads the current version directly when creating BUILDINFO;
it does not require a `0.0.9` changelog entry. Only the pre-change `0.1.1` check
above reads release notes. The existing public key and signed-version requirement
are retained; only this private fixture allows its loopback HTTP feed.

Do not upload the fixture, generate release metadata for it, or run a production
source upload gate with its altered version locks. Ordinary candidate gates stay
strict. The recipe does not change the preserved candidate packages or signature.

Run acceptance as a regular user in a real Linux desktop session, with
`tauri-driver` and a matching `WebKitWebDriver` on PATH. Use the exact signed
candidate and its detached signature, with free ports 4444 and 8763. Replace
`CANDIDATE_DIR` with the preserved signed candidate directory, then run:

```bash
dbus-run-session -- bash
```

Inside that private desktop bus session, run:

```bash
set -euo pipefail
unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
CANDIDATE_DIR=/absolute/path/to/preserved/signed-candidate
QA_SESSION=$(mktemp -d /tmp/aperture-linux-update.XXXXXX)
mkdir "$QA_SESSION/cache" "$QA_SESSION/config" "$QA_SESSION/tmp"
export XDG_CACHE_HOME="$QA_SESSION/cache" XDG_CONFIG_HOME="$QA_SESSION/config" TMPDIR="$QA_SESSION/tmp"
export APPIMAGE_EXTRACT_AND_RUN=1
tauri-driver --port 4444 --native-driver "$(command -v WebKitWebDriver)" > "$QA_SESSION/driver.log" 2>&1 &
QA_DRIVER_PID=$!
trap 'kill "$QA_DRIVER_PID" 2>/dev/null || true' EXIT
python3 - <<'PY'
import socket, time
for attempt in range(40):
    try:
        with socket.create_connection(('127.0.0.1', 4444), timeout=.25): break
    except OSError: time.sleep(.25)
else: raise TimeoutError('tauri-driver did not listen; inspect driver.log')
PY
python3 scripts/native-appimage-update.py \
  --prior "$QA_ROOT/build/release/Aperture-x86_64.AppImage" \
  --candidate "$CANDIDATE_DIR/Aperture-x86_64.AppImage" \
  --signature "$CANDIDATE_DIR/Aperture-x86_64.AppImage.sig" \
  --version 0.1.1 --prior-version 0.0.9 --output "$QA_SESSION/result.json"
```

Exit that shell after acceptance to stop its owned driver. The harness copies
the prior AppImage into an owned writable directory and serves
the candidate without changing either input. Its result and native screenshot
record signature rejection, unsaved-work handling, replacement and restart.
