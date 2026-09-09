#!/usr/bin/env bash
# Local fallback: publish an engine release from a laptop (docs/RELEASING.md).
# Prefer `mise engine-tag` after CI builds both architectures. Run this as
# `mise release`. Refuses unless on main, clean, and even with origin/main,
# and unless engine/Cargo.toml names a version with no tag or release yet.
# Builds the candidate, requires it to answer hello with that version and the
# protocol ui/Engine.qml accepts, creates the GitHub Release as a draft with
# both native binaries and SHA256SUMS, asks, and publishes. Releases are immutable, so
# publishing is the point of no return. It then fetches the published asset
# back, requires it to hash to the candidate, and writes engine/release.pin.
# It does not commit: run `mise check`, then commit the pin bump.
#   --dry-run  stop after the candidate checks; create nothing
#   --yes      publish without the prompt
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }

dry_run=0
yes=0
for arg in "$@"; do
  case $arg in
    --dry-run) dry_run=1 ;;
    --yes) yes=1 ;;
    *) die "Unknown option: $arg (use --dry-run or --yes)" ;;
  esac
done

repo=wesleygrimes/omastorm
source scripts/engine-pin.sh
machine=$(engine_machine "$(uname -m)")
asset=omastorm-engine-$machine-unknown-linux-gnu

for tool in gh git curl jq rg socat sha256sum strip; do
  command -v "$tool" > /dev/null 2>&1 || die "Need $tool on PATH; run this as mise release."
done
gh auth status > /dev/null 2>&1 || die 'gh is not logged in (gh auth login).'

branch=$(git rev-parse --abbrev-ref HEAD)
[[ $branch == main ]] || die "On $branch; engine releases are cut from main."
[[ -z $(git status --porcelain) ]] || die 'The working tree is not clean.'
git fetch -q origin main
[[ $(git rev-parse HEAD) == $(git rev-parse origin/main) ]] \
  || die 'main is not even with origin/main; push or pull first so the release names a commit everyone has.'

version=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
lock_version=$(awk '/^name = "omastorm-engine"$/{getline; print}' Cargo.lock | awk -F'"' '{print $2}')
[[ $version == "$lock_version" ]] \
  || die "engine/Cargo.toml says $version but Cargo.lock says $lock_version; build with --locked and commit the lock."
tag=engine-$version
pinned=$(awk -F= '/^tag=/{print $2}' engine/release.pin)
[[ $tag != "$pinned" ]] || die "$tag is already the pinned release; bump version in engine/Cargo.toml first."
! git rev-parse -q --verify "refs/tags/$tag" > /dev/null || die "Tag $tag already exists locally."
! git ls-remote --exit-code --tags origin "refs/tags/$tag" > /dev/null 2>&1 || die "Tag $tag already exists on origin."
! gh release view "$tag" -R "$repo" > /dev/null 2>&1 \
  || die "Release $tag already exists. Releases are immutable; bump the version instead of replacing it."

# The candidate: optimized, stripped, with SHA256SUMS beside it.
bash scripts/build-engine-release.sh
bash scripts/package-engine-release.sh
dist=target/dist/$asset
sum=$(sha256sum -- "$dist" | awk '{print $1}')

# Both binaries must be from this commit; validate the native hello too.
bash scripts/check-engine-binary.sh "$dist" "$version"

# Notes: commits since the pinned release that change the binary.
notes=$(git log --no-merges --format='- %s' "$pinned..HEAD" -- engine/src engine/build.rs engine/tests engine/Cargo.toml Cargo.lock)
[[ -n $notes ]] || notes='- No engine source changes since the previous release.'

printf '\n%s at %s\n%s  %s\n\n%s\n\n' "$tag" "$(git rev-parse --short HEAD)" "$sum" "$asset" "$notes"
if (( dry_run )); then
  echo 'Dry run: the candidate passed; nothing was created.'
  exit 0
fi
if (( ! yes )); then
  read -r -p "Publish $tag? It cannot be edited or deleted afterwards. [y/N] " answer
  [[ $answer == y || $answer == Y ]] || die 'Not published.'
fi

# Draft first: assets lock at publish and cannot be added afterwards.
gh release create "$tag" -R "$repo" --draft --target "$(git rev-parse HEAD)" \
  --title "Engine $version" --notes "$notes" target/dist/omastorm-engine-* target/dist/SHA256SUMS target/dist/release.pin
gh release edit "$tag" -R "$repo" --draft=false
echo "Published https://github.com/$repo/releases/tag/$tag"

# Pin every public asset only after its checksum matches the candidate.
bash scripts/pin-engine-release.sh target/dist/release.pin
echo "Next: mise check, then commit the pin bump and push."
