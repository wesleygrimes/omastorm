#!/usr/bin/env bash
# Download current upstream fixture URLs and rewrite data/fixtures/ plus
# data/SHA256SUMS. Ordinary setup, CI, and launch never call this; it is the
# intentional bump path in data/README.md.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v unzip >/dev/null 2>&1 || {
  printf 'unzip is required to extract cities5000.zip.\n' >&2
  exit 1
}
work=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-fixtures.XXXXXX")
trap 'rm -rf "$work"' EXIT
raw=$work/raw
mkdir -p "$raw"

curl -fL --retry 2 'https://unidata-nexrad-level2.s3.amazonaws.com/2013/05/20/KTLX/KTLX20130520_201643_V06.gz' \
  -o "$raw/KTLX20130520_201643_V06.gz"
ne='https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson'
curl -fL --retry 2 "$ne/ne_10m_populated_places_simple.geojson" -o "$raw/places.geojson"
curl -fL --retry 2 'https://download.geonames.org/export/dump/cities5000.zip' -o "$work/cities5000.zip"
unzip -p "$work/cities5000.zip" cities5000.txt >"$raw/cities5000.txt"
curl -fL --retry 2 'https://download.geonames.org/export/dump/admin1CodesASCII.txt' -o "$raw/admin1CodesASCII.txt"
for scale in 10m 50m; do
  for theme in coastline lakes admin_0_boundary_lines_land admin_1_states_provinces_lines; do
    curl -fL --retry 2 "$ne/ne_${scale}_${theme}.geojson" -o "$raw/ne_${scale}_${theme}.geojson"
  done
done

sums=$work/SHA256SUMS
: >"$sums"
for name in \
  KTLX20130520_201643_V06.gz \
  places.geojson \
  cities5000.txt \
  admin1CodesASCII.txt \
  ne_10m_coastline.geojson \
  ne_10m_lakes.geojson \
  ne_10m_admin_0_boundary_lines_land.geojson \
  ne_10m_admin_1_states_provinces_lines.geojson \
  ne_50m_coastline.geojson \
  ne_50m_lakes.geojson \
  ne_50m_admin_0_boundary_lines_land.geojson \
  ne_50m_admin_1_states_provinces_lines.geojson; do
  (cd "$work" && sha256sum "raw/$name") | sed 's|raw/|data/raw/|' >>"$sums"
done

if [[ -f data/SHA256SUMS ]]; then
  echo 'Checksum changes:'
  diff -u data/SHA256SUMS "$sums" || true
fi

mkdir -p data/raw data/fixtures
cp -f "$raw/KTLX20130520_201643_V06.gz" data/raw/KTLX20130520_201643_V06.gz
cp -f "$raw/KTLX20130520_201643_V06.gz" data/fixtures/KTLX20130520_201643_V06.gz
for name in \
  places.geojson \
  cities5000.txt \
  admin1CodesASCII.txt \
  ne_10m_coastline.geojson \
  ne_10m_lakes.geojson \
  ne_10m_admin_0_boundary_lines_land.geojson \
  ne_10m_admin_1_states_provinces_lines.geojson \
  ne_50m_coastline.geojson \
  ne_50m_lakes.geojson \
  ne_50m_admin_0_boundary_lines_land.geojson \
  ne_50m_admin_1_states_provinces_lines.geojson; do
  cp -f "$raw/$name" "data/raw/$name"
  gzip -n -9 -c "$raw/$name" >"data/fixtures/${name}.gz"
done
cp -f "$sums" data/SHA256SUMS
sha256sum -c data/SHA256SUMS
records=$(wc -l <data/raw/cities5000.txt)
printf 'Vendored upstream bytes. cities5000.txt: %s records.\n' "${records// /}"
echo 'Update dates and record counts in data/README.md before committing.'
