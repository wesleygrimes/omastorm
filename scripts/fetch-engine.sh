#!/usr/bin/env bash
# Fetch the pinned GitHub Release asset, verify its committed sha256, and
# install it under $XDG_DATA_HOME/omastorm/bin (DESIGN.md, distribution).
# Ordinary launch never calls this. Plugin bootstrap does, through
# `run.sh --ensure`, only when the checkout has no debug engine.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }
source scripts/engine-pin.sh

machine=$(engine_machine "${OMASTORM_ENGINE_MACHINE:-$(uname -m)}")
pin_file=${OMASTORM_ENGINE_PIN:-engine/release.pin}
read_engine_pin "$pin_file"
asset=${assets[$machine]:-}
sha256=${hashes[$machine]:-}
[[ -n $asset && -n $sha256 ]] || die "No pinned $machine engine for $tag yet. Publish its release asset and add its checksum to $pin_file."

data_home=${XDG_DATA_HOME:-$HOME/.local/share}
dest_dir=$data_home/omastorm/bin
dest=$dest_dir/omastorm-engine

hash_of() { sha256sum -- "$1" | awk '{print $1}'; }

if [[ -x $dest ]] && [[ $(hash_of "$dest") == "$sha256" ]]; then
  [[ ${1:-} == --print-path ]] && printf '%s\n' "$dest"
  exit 0
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-engine.XXXXXX")
trap 'rm -rf "$work"' EXIT
tmp=$work/$asset

if [[ -n ${OMASTORM_ENGINE_ASSET:-} ]]; then
  [[ -f $OMASTORM_ENGINE_ASSET ]] || die "OMASTORM_ENGINE_ASSET is not a file: $OMASTORM_ENGINE_ASSET"
  cp -- "$OMASTORM_ENGINE_ASSET" "$tmp"
else
  url=${OMASTORM_ENGINE_URL:-https://github.com/$repo/releases/download/$tag/$asset}
  if ! curl -fsSL --retry 2 -A "omastorm/$tag (https://omastorm.com)" -o "$tmp" -- "$url"; then
    die "Could not download $asset from $url." \
      "Publish GitHub Release $tag on $repo with that asset matching $pin_file, and make the repository public so the asset is anonymous." \
      "From a checkout with Rust: bash scripts/cargo.sh build --locked && bash run.sh"
  fi
fi

got=$(hash_of "$tmp")
if [[ $got != "$sha256" ]]; then
  die "Engine sha256 mismatch for $asset." \
    "expected $sha256" \
    "got      $got" \
    "The committed pin is the source of truth; a substituted asset is refused."
fi

mkdir -p -- "$dest_dir"
chmod 755 -- "$tmp"
# Temp-and-rename so a half-written dest is never executable as the engine.
mv -f -- "$tmp" "$dest"
# The trap would otherwise try to remove a file already moved.
trap 'rm -rf "$work"' EXIT

[[ ${1:-} == --print-path ]] && printf '%s\n' "$dest"
exit 0
