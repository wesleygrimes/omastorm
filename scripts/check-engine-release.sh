#!/usr/bin/env bash
# Bundle and pin in a scratch repository, with no network or real pin writes.
set -euo pipefail
cd "$(dirname "$0")/.."
fail() { printf '%s\n' "$@" >&2; exit 1; }
scratch=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-release-check.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
for task in engine-bump engine-tag engine-verify engine-pin build-release release; do
  rg -q "\[tasks\.$task\]" mise.toml || fail "mise.toml is missing $task"
done
toml=$(sha256sum engine/Cargo.toml Cargo.lock)
current=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
if bash scripts/bump-engine-version.sh not-a-version > "$scratch/bump.err" 2>&1; then
  fail 'bump-engine-version.sh accepted a non X.Y.Z version'
fi
rg -q 'Version must be X.Y.Z' "$scratch/bump.err" || fail "unclear bump error: $(cat "$scratch/bump.err")"
if bash scripts/bump-engine-version.sh "$current" > "$scratch/bump.err" 2>&1; then
  fail 'bump-engine-version.sh rewrote the current version'
fi
rg -q "already $current" "$scratch/bump.err" || fail "unclear current-version bump error: $(cat "$scratch/bump.err")"
[[ $(sha256sum engine/Cargo.toml Cargo.lock) == "$toml" ]] || fail 'Failed bump changed Cargo.toml or Cargo.lock'
mkdir -p "$scratch"/{scripts,engine,target/dist,bin,published}
cp scripts/{engine-pin,package-engine-release,pin-engine-release,tag-engine-release}.sh "$scratch/scripts/"
cp engine/{Cargo.toml,release.pin} "$scratch/engine/"
git -C "$scratch" init -q
git -C "$scratch" -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false commit -qm fixture --allow-empty
git -C "$scratch" branch -m topic
if bash "$scratch/scripts/tag-engine-release.sh" > "$scratch/tag.err" 2>&1; then
  fail 'tag-engine-release.sh tagged from a topic branch'
fi
rg -q 'engine tags are pushed from main' "$scratch/tag.err" \
  || fail "unclear tag refusal: $(cat "$scratch/tag.err")"
source_commit=$(git -C "$scratch" rev-parse HEAD)
version=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
# Header fixtures exercise architecture checks; these files are never executed.
# Both supported CPUs use little-endian ELF64, with e_machine at offset 18.
for arch in x86_64 aarch64; do
  asset=omastorm-engine-$arch-unknown-linux-gnu
  cp target/debug/omastorm-engine "$scratch/target/dist/$asset"
  code='\076\000'
  [[ $arch == aarch64 ]] && code='\267\000'
  printf '%b' "$code" | dd of="$scratch/target/dist/$asset" bs=1 seek=18 conv=notrunc status=none
  hash=$(sha256sum "$scratch/target/dist/$asset" | awk '{print $1}')
  jq -n --arg source "$source_commit" --arg version "$version" --arg asset "$asset" --arg sha256 "$hash" \
    '{source:$source, version:$version, asset:$asset, sha256:$sha256}' > "$scratch/target/dist/$asset.build.json"
done
cd "$scratch"
before=$(sha256sum engine/release.pin)
bash scripts/package-engine-release.sh
[[ $(sha256sum engine/release.pin) == "$before" ]] || fail 'Bundling changed the tracked pin'
(cd target/dist && sha256sum --check SHA256SUMS)
[[ $(wc -l < target/dist/SHA256SUMS) == 2 ]] || fail 'Bundle did not include both architectures'
cp target/dist/omastorm-engine-* published/

arm=omastorm-engine-aarch64-unknown-linux-gnu
mv "target/dist/$arm" "target/dist/$arm.saved"
if bash scripts/package-engine-release.sh > failure.log 2>&1; then fail 'Accepted a missing architecture'; fi
cp target/dist/omastorm-engine-x86_64-unknown-linux-gnu "target/dist/$arm"
if bash scripts/package-engine-release.sh > failure.log 2>&1; then fail 'Accepted a mislabeled architecture'; fi
mv "target/dist/$arm.saved" "target/dist/$arm"
cp "target/dist/$arm.build.json" metadata.saved
jq '.source = "stale"' metadata.saved > "target/dist/$arm.build.json"
if bash scripts/package-engine-release.sh > failure.log 2>&1; then fail 'Accepted a stale source commit'; fi
mv metadata.saved "target/dist/$arm.build.json"

cat > bin/curl <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
while [[ $# -gt 0 ]]; do
  case $1 in
    -o) out=$2; shift 2 ;;
    --) url=$2; break ;;
    *) shift ;;
  esac
done
asset=${url##*/}
[[ $asset != "${FAIL_ASSET:-}" ]] || exit 22
if [[ $asset == "${CORRUPT_ASSET:-}" ]]; then printf 'wrong bytes' > "$out";
else cp "${PUBLISHED_DIR:?}/$asset" "$out"; fi
CURL
chmod +x bin/curl
export PATH="$scratch/bin:$PATH" PUBLISHED_DIR="$scratch/published"
if FAIL_ASSET=$arm bash scripts/pin-engine-release.sh > failure.log 2>&1; then fail 'Pinned a missing published asset'; fi
[[ $(sha256sum engine/release.pin) == "$before" ]] || fail 'Missing asset changed the tracked pin'
if CORRUPT_ASSET=$arm bash scripts/pin-engine-release.sh > failure.log 2>&1; then fail 'Pinned a mismatching published asset'; fi
[[ $(sha256sum engine/release.pin) == "$before" ]] || fail 'Wrong checksum changed the tracked pin'
if FAIL_ASSET=$arm bash scripts/pin-engine-release.sh --verify-only > failure.log 2>&1; then fail 'Verified a missing published asset'; fi
[[ $(sha256sum engine/release.pin) == "$before" ]] || fail 'Failed verify-only changed the tracked pin'
bash scripts/pin-engine-release.sh --verify-only
[[ $(sha256sum engine/release.pin) == "$before" ]] || fail 'verify-only changed the tracked pin'
bash scripts/pin-engine-release.sh
cmp engine/release.pin target/dist/release.pin
echo 'Engine release: both architectures, ELF labels, exact checksums, and publication gate PASS'
