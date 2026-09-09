#!/usr/bin/env bash
# Bump engine/Cargo.toml and the matching Cargo.lock package version.
# Publishes nothing and does not touch engine/release.pin. Run as
# `mise engine-bump` (next patch) or `mise engine-bump -- <version>`.
# Refreshes the lock with `cargo update --offline -p <engine>`; refuses if
# anything other than that package's version line would change.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }

lock_without_engine_version() {
  awk -v name="$package" '
    $0 == "name = \"" name "\"" { print; getline; if ($0 ~ /^version = /) { print "version = \"\""; next } }
    { print }
  ' "$1"
}

current=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
[[ $current =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "engine/Cargo.toml version is not X.Y.Z: $current"
package=$(awk -F'"' '/^name = /{print $2; exit}' engine/Cargo.toml)
[[ -n $package ]] || die 'engine/Cargo.toml is missing name.'

version=${1:-}
if [[ -z $version ]]; then
  IFS=. read -r major minor patch <<< "$current"
  version=$major.$minor.$((patch + 1))
fi
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "Version must be X.Y.Z: $version"
[[ $version != "$current" ]] || die "engine/Cargo.toml is already $version."

pinned=$(awk -F= '/^tag=/{print $2}' engine/release.pin)
[[ $version != "${pinned#engine-}" ]] || die "$version is already the pinned release."

toml_bak=$(mktemp)
lock_bak=$(mktemp)
trap 'rm -f -- "$toml_bak" "$lock_bak"' EXIT
cp -- engine/Cargo.toml "$toml_bak"
cp -- Cargo.lock "$lock_bak"
awk -v v="$version" '
  BEGIN { done = 0 }
  !done && /^version = / { printf "version = \"%s\"\n", v; done = 1; next }
  { print }
' "$toml_bak" > engine/Cargo.toml
restore() {
  cp -- "$toml_bak" engine/Cargo.toml
  cp -- "$lock_bak" Cargo.lock
}
if ! bash scripts/cargo.sh update --offline -p "$package"; then
  restore
  die "cargo update --offline -p $package failed; engine/Cargo.toml and Cargo.lock were left unchanged."
fi
lock_version=$(awk -v name="$package" '
  $0 == "name = \"" name "\"" { getline; print }
' Cargo.lock | awk -F'"' '{print $2}')
if [[ $version != "$lock_version" ]]; then
  restore
  die "Cargo.lock still says $lock_version after the bump."
fi
if ! cmp -s <(lock_without_engine_version "$lock_bak") <(lock_without_engine_version Cargo.lock); then
  restore
  die 'cargo update changed more than the engine version in Cargo.lock; engine/Cargo.toml and Cargo.lock were left unchanged.'
fi

printf 'Bumped engine to %s. Next: mise check, then commit engine/Cargo.toml and Cargo.lock to main.\n' "$version"
