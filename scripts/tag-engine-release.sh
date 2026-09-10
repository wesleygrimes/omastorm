#!/usr/bin/env bash
# Push engine-<version> from main so CI drafts the GitHub Release.
# Run as `mise engine-tag`. Does not publish, verify assets, or write the pin.
# Refuses unless on main, clean, and HEAD is the full commit currently at
# origin's main.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }

command -v git > /dev/null 2>&1 || die 'Need git on PATH; run this as mise engine-tag.'

branch=$(git rev-parse --abbrev-ref HEAD)
[[ $branch == main ]] || die "On $branch; engine tags are pushed from main."
[[ -z $(git status --porcelain) ]] || die 'The working tree is not clean.'
bash scripts/require-origin-main.sh tag

command -v gh > /dev/null 2>&1 || die 'Need gh on PATH; run this as mise engine-tag.'
gh auth status > /dev/null 2>&1 || die 'gh is not logged in (gh auth login).'

version=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
lock_version=$(awk '/^name = "omastorm-engine"$/{getline; print}' Cargo.lock | awk -F'"' '{print $2}')
[[ $version == "$lock_version" ]] \
  || die "engine/Cargo.toml says $version but Cargo.lock says $lock_version; run mise engine-bump and commit the lock."
tag=engine-$version
pinned=$(awk -F= '/^tag=/{print $2}' engine/release.pin)
[[ $tag != "$pinned" ]] || die "$tag is already the pinned release; bump with mise engine-bump first."
! git rev-parse -q --verify "refs/tags/$tag" > /dev/null || die "Tag $tag already exists locally."
! git ls-remote --exit-code --tags origin "refs/tags/$tag" > /dev/null 2>&1 || die "Tag $tag already exists on origin."
! gh release view "$tag" -R wesleygrimes/omastorm > /dev/null 2>&1 \
  || die "Release $tag already exists. Releases are immutable; bump the version instead of replacing it."

# Annotated: CI's gh release create --verify-tag rejects a lightweight tag.
git tag -a "$tag" -m "$tag"
git push origin "refs/tags/$tag"
printf 'Pushed %s. Engine builds CI will draft the GitHub Release; publish it, then mise engine-verify and mise engine-pin.\n' "$tag"
