#!/usr/bin/env bash
# Pin already-built CI/local assets only after verifying their public release.
# Run as `mise engine-pin`. `mise engine-verify` is `--verify-only`.
#   --verify-only  require published bytes; leave engine/release.pin unchanged
set -euo pipefail
cd "$(dirname "$0")/.."
die() { printf '%s\n' "$@" >&2; exit 1; }
source scripts/engine-pin.sh
verify_only=0
candidate=target/dist/release.pin
for arg in "$@"; do
  case $arg in
    --verify-only) verify_only=1 ;;
    -*) die "Unknown option: $arg (use --verify-only)" ;;
    *) candidate=$arg ;;
  esac
done
work=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-release.XXXXXX")
trap 'rm -rf "$work"' EXIT
cp -- "$candidate" "$work/release.pin"
read_engine_pin "$work/release.pin"
for arch in x86_64 aarch64; do
  [[ -n ${assets[$arch]:-} ]] || continue
  url=https://github.com/$repo/releases/download/$tag/${assets[$arch]}
  curl -fsSL --retry 2 -o "$work/asset" -- "$url" \
    || die "Publish ${assets[$arch]} on $tag before updating engine/release.pin."
  got=$(sha256sum -- "$work/asset" | awk '{print $1}')
  [[ $got == "${hashes[$arch]}" ]] \
    || die "Published ${assets[$arch]} does not match the candidate checksum; engine/release.pin was not changed."
done
if (( verify_only )); then
  printf 'Verified published assets; engine/release.pin unchanged.\n'
  exit 0
fi
cp -- "$work/release.pin" engine/release.pin
printf 'Verified published assets and updated engine/release.pin\n'
