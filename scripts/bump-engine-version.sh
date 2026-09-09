#!/usr/bin/env bash
# Bump engine/Cargo.toml and refresh Cargo.lock. Publishes nothing and does
# not touch engine/release.pin. Run as `mise engine-bump` (next patch) or
# `mise engine-bump -- <version>`.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }

current=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
[[ $current =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "engine/Cargo.toml version is not X.Y.Z: $current"

version=${1:-}
if [[ -z $version ]]; then
  IFS=. read -r major minor patch <<< "$current"
  version=$major.$minor.$((patch + 1))
fi
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "Version must be X.Y.Z: $version"
[[ $version != "$current" ]] || die "engine/Cargo.toml is already $version."

pinned=$(awk -F= '/^tag=/{print $2}' engine/release.pin)
[[ $version != "${pinned#engine-}" ]] || die "$version is already the pinned release."

bak=$(mktemp)
trap 'rm -f -- "$bak"' EXIT
cp -- engine/Cargo.toml "$bak"
awk -v v="$version" '
  BEGIN { done = 0 }
  !done && /^version = / { printf "version = \"%s\"\n", v; done = 1; next }
  { print }
' "$bak" > engine/Cargo.toml
if ! bash scripts/cargo.sh generate-lockfile --offline; then
  cp -- "$bak" engine/Cargo.toml
  die 'cargo generate-lockfile --offline failed; engine/Cargo.toml was left unchanged.'
fi
lock_version=$(awk '/^name = "omastorm-engine"$/{getline; print}' Cargo.lock | awk -F'"' '{print $2}')
[[ $version == "$lock_version" ]] || die "Cargo.lock still says $lock_version after the bump."

printf 'Bumped engine to %s. Next: mise check, then commit engine/Cargo.toml and Cargo.lock to main.\n' "$version"
