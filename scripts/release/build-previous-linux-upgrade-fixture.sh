#!/usr/bin/env bash
# Private prior-code fixture in the already provisioned Debian package job.
set -euo pipefail
if [[ ${SIGNED:-false} == true || -n ${TAURI_SIGNING_PRIVATE_KEY:-} || -n ${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-} ]]; then
  echo 'Previous-code fixtures require an unsigned context without signing secrets.' >&2
  exit 1
fi
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
output=$(realpath -m "${1:-$root/build/upgrade-fixture-linux}")
prior="$root/build/previous-stable-linux-checkout"
target="$root/src-tauri/target"
bundle="$target/release/bundle/appimage"
helper="$root/scripts/release/previous-upgrade-fixture.py"
# Checkout's temporary HOME trust does not reach shell/Python Git commands.
# Append exact paths to this process's config, preserving inherited entries.
git_config_count=${GIT_CONFIG_COUNT:-0}
[[ $git_config_count =~ ^[0-9]+$ ]]
git_config_count=$((10#$git_config_count))
for directory in "$root" "$prior"; do
  printf -v "GIT_CONFIG_KEY_$git_config_count" '%s' safe.directory
  printf -v "GIT_CONFIG_VALUE_$git_config_count" '%s' "$directory"
  export "GIT_CONFIG_KEY_$git_config_count" "GIT_CONFIG_VALUE_$git_config_count"
  git_config_count=$((git_config_count + 1))
done
export GIT_CONFIG_COUNT=$git_config_count
[[ ! -e "$prior" && ! -e "$output" && -z ${CARGO_TARGET_DIR:-} ]]
case "$output/" in "$root/build/release/"*) echo 'Fixture output must be separate from production assets.' >&2; exit 1 ;; esac
cd "$root"
git fetch --no-tags origin refs/tags/v0.1.1:refs/tags/v0.1.1
[[ $(git rev-parse 'v0.1.1^{commit}') == b34a2de71298e6c74980e502655dbe8ac73e0632 ]]
git worktree add --no-track -b qa/previous-stable-linux-upgrade "$prior" b34a2de71298e6c74980e502655dbe8ac73e0632
python3 "$helper" --root "$prior" --production-root "$root" --output "$output" --platform linux
[[ -d "$bundle" ]]
scratch=$(mktemp -d "$root/build/previous-appimage-bundle.XXXXXX")
production_hashes() {
  find "$root/build/release" -maxdepth 1 -type f -print0 | sort -z | xargs -0 sha256sum
}
production_hashes > "$scratch/production-before.sha256"
restore() {
  status=$?
  trap - EXIT
  if [[ -d "$scratch/original" ]]; then
    # Only the owned, newly generated prior bundle is removed. Staged normal
    # packages and the original bundle were never hardlinked to mutable files.
    if ! { rm -rf "$bundle" && mv "$scratch/original" "$bundle"; }; then
      echo "Original bundle retained for recovery: $scratch/original" >&2
      status=1
    fi
  fi
  production_hashes > "$scratch/production-after.sha256" || status=1
  cmp "$scratch/production-before.sha256" "$scratch/production-after.sha256" || status=1
  if [[ ! -e "$scratch/original" ]]; then rm -rf "$scratch"; fi
  exit "$status"
}
trap restore EXIT
mv "$bundle" "$scratch/original"
ln -s "$target" "$prior/src-tauri/target"
mkdir -p "$prior/build/linux"
cp "$root/build/linux/dependencies.json" "$root/build/linux/native-notices.txt" "$prior/build/linux/"
cd "$prior"
pnpm install --frozen-lockfile
bash scripts/release/package-linux.sh --config "$output/prior-test-overlay.json"
cp build/release/Aperture-x86_64.AppImage build/release/BUILDINFO-linux.json build/release/THIRD-PARTY-NOTICES-linux.txt "$output/"
python3 scripts/release/release.py source-material "$output" --fetch --platform linux
python3 "$helper" --root "$prior" --production-root "$root" --output "$output" --platform linux --record
echo "Previous-stable-code fixture staged separately: $output"
