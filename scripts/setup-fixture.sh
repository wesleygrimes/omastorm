#!/usr/bin/env bash
# Fixture sources (development only). Copies the archived Level II volume and
# extracts the Natural Earth files that `engine/build.rs` converts into the
# embedded geography (DESIGN.md, basemap tiles: coastline, lakes, country and
# state lines at 1:10m and 1:50m, plus populated places for map labels) and
# GeoNames cities5000 for the location picker from data/fixtures/, then
# verifies data/SHA256SUMS. data/raw is ignored, so a fresh checkout runs
# this once before `cargo build`. Launch never calls it. Live upstream URLs
# are only for `scripts/refresh-fixtures.sh` (data/README.md).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p data/raw
if sha256sum -c data/SHA256SUMS >/dev/null 2>&1; then
  echo 'Fixtures already verified.'
  exit 0
fi
[[ -d data/fixtures ]] || {
  printf 'Missing data/fixtures/ (see data/README.md).\n' >&2
  exit 1
}
while read -r _ path; do
  [[ -n ${path:-} ]] || continue
  name=${path#data/raw/}
  dest=data/raw/$name
  if [[ -f data/fixtures/$name ]]; then
    cp -f "data/fixtures/$name" "$dest"
  elif [[ -f data/fixtures/$name.gz ]]; then
    gzip -dc "data/fixtures/$name.gz" >"$dest"
  else
    printf 'Missing vendored fixture for %s (see data/README.md).\n' "$name" >&2
    exit 1
  fi
done <data/SHA256SUMS
sha256sum -c data/SHA256SUMS
