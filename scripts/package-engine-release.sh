#!/usr/bin/env bash
# Combine native CI outputs into one candidate pin. Never changes tracked pins.
set -euo pipefail
cd "$(dirname "$0")/.."
die() { printf '%s\n' "$@" >&2; exit 1; }
source scripts/engine-pin.sh
read_engine_pin engine/release.pin
version=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
source_commit=$(git rev-parse HEAD)
dist=${1:-target/dist}
# Artifacts lose their executable mode in transit. Check ELF architecture
# before restoring it, so a swapped/misnamed artifact cannot enter the bundle.
for arch in x86_64 aarch64; do
  asset=omastorm-engine-$arch-unknown-linux-gnu
  [[ -s $dist/$asset ]] || die "Missing native release asset: $dist/$asset"
  machine=$(LC_ALL=C readelf -h "$dist/$asset" | awk -F: '/Machine:/{gsub(/^[[:space:]]+/, "", $2); print $2}')
  case "$arch:$machine" in
    x86_64:'Advanced Micro Devices X86-64'|aarch64:AArch64) ;;
    *) die "Wrong ELF architecture for $asset: $machine" ;;
  esac
  assets[$arch]=$asset
  hashes[$arch]=$(sha256sum -- "$dist/$asset" | awk '{print $1}')
  jq -e --arg source "$source_commit" --arg version "$version" --arg asset "$asset" --arg hash "${hashes[$arch]}" \
    '.source == $source and .version == $version and .asset == $asset and .sha256 == $hash' \
    "$dist/$asset.build.json" >/dev/null \
    || die "Build metadata does not match $asset, engine $version, and commit $source_commit"
done
{
  printf '# Verify published assets with mise engine-pin before committing.\n'
  printf 'tag=engine-%s\nrepo=%s\n' "$version" "$repo"
  for arch in x86_64 aarch64; do
    printf 'asset_%s=%s\nsha256_%s=%s\n' "$arch" "${assets[$arch]}" "$arch" "${hashes[$arch]}"
  done
} > "$dist/release.pin"
{
  for arch in x86_64 aarch64; do
    printf '%s  %s\n' "${hashes[$arch]}" "${assets[$arch]}"
    chmod 755 -- "$dist/${assets[$arch]}"
  done
} > "$dist/SHA256SUMS"
printf 'Release bundle: %s (tracked pin unchanged)\n' "$dist"
