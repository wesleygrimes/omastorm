#!/usr/bin/env bash
# Build a native Linux release asset and a candidate multi-architecture pin
# under target/dist/. --write-pin verifies the published asset before pinning.
# Does not publish, tag, or push.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }
source scripts/engine-pin.sh

write_pin=0
case ${1:-} in
  '') ;;
  --write-pin) write_pin=1 ;;
  *) die "Usage: bash scripts/build-engine-release.sh [--write-pin]" ;;
esac
[[ $# -le 1 ]] || die "Usage: bash scripts/build-engine-release.sh [--write-pin]"

if ! command -v rustc >/dev/null && [[ -x .tools/cargo/bin/rustc ]]; then
  export RUSTUP_HOME="$PWD/.tools/rustup" CARGO_HOME="$PWD/.tools/cargo"
  export PATH="$CARGO_HOME/bin:$PATH"
fi
host=$(rustc -vV | awk '/^host:/{print $2}')
case $host in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
  *) die "Build on x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu (host is $host)." ;;
esac
machine=${host%%-*}
# Explicit target avoids a Cargo config/env target silently changing the asset.
bash scripts/cargo.sh build --release --target "$host" --locked --offline
src=target/$host/release/omastorm-engine
[[ -x $src ]] || die "cargo did not produce $src"

mkdir -p target/dist
asset=omastorm-engine-$host
dest=target/dist/$asset
cp -- "$src" "$dest"
strip --strip-unneeded -- "$dest"
chmod 755 -- "$dest"
sum=$(sha256sum -- "$dest" | awk '{print $1}')
printf '%s  %s\n' "$sum" "$dest"

pin=engine/release.pin
read_engine_pin "$pin"
version=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
jq -n --arg source "$(git rev-parse HEAD)" --arg version "$version" \
  --arg asset "$asset" --arg sha256 "$sum" \
  '{source:$source, version:$version, asset:$asset, sha256:$sha256}' > "$dest.build.json"
if [[ $tag != "engine-$version" ]]; then
  # Hashes from the previous release must not follow a version bump.
  assets=() hashes=()
  tag=engine-$version
fi
assets[$machine]=$asset
hashes[$machine]=$sum
candidate=target/dist/release.pin
{
  printf '# Pinned GitHub Release for the engine binary (DESIGN.md, distribution).\n'
  printf '# Bump only after the named assets exist on %s.\n' "$repo"
  printf 'tag=%s\nrepo=%s\n' "$tag" "$repo"
  for arch in x86_64 aarch64; do
    [[ -n ${assets[$arch]:-} ]] || continue
    printf 'asset_%s=%s\nsha256_%s=%s\n' "$arch" "${assets[$arch]}" "$arch" "${hashes[$arch]}"
  done
} > "$candidate"
# Preserve the other architecture's published checksum even on a native build
# machine that has only one binary. All entries belong to the candidate tag.
{
  for arch in x86_64 aarch64; do
    [[ -n ${assets[$arch]:-} ]] || continue
    printf '%s  %s\n' "${hashes[$arch]}" "${assets[$arch]}"
  done
} > target/dist/SHA256SUMS

if (( write_pin )); then
  bash scripts/pin-engine-release.sh "$candidate"
else
  printf 'Candidate pin: %s (committed pin unchanged).\n' "$candidate"
  printf 'Publish only as a new immutable release with both architectures; follow docs/RELEASING.md.\n'
  printf 'After publication, verify with mise engine-verify and write the pin with mise engine-pin.\n'
fi
