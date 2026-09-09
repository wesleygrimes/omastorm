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
# Success and upgrade-refusal paths use a stub cargo so this check does not
# need the mise toolchain or a registry. The stub must only rewrite the
# engine package version; a stub that touches another lock line must fail.
package=$(awk -F'"' '/^name = /{print $2; exit}' engine/Cargo.toml)
IFS=. read -r major minor patch <<< "$current"
next=$major.$minor.$((patch + 1))
write_bump_tree() {
  local root=$1
  mkdir -p "$root"/{engine,scripts}
  cp Cargo.toml Cargo.lock "$root/"
  cp engine/Cargo.toml engine/release.pin "$root/engine/"
  cp scripts/bump-engine-version.sh "$root/scripts/"
  cat > "$root/scripts/cargo.sh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
package=$(awk -F'"' '/^name = /{print $2; exit}' engine/Cargo.toml)
version=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
[[ $1 == update ]] || exit 1
extra=
[[ -f scripts/cargo.extra ]] && extra=$(cat scripts/cargo.extra)
awk -v name="$package" -v v="$version" -v extra="$extra" '
  $0 == "name = \"" name "\"" { print; getline; if ($0 ~ /^version = /) { printf "version = \"%s\"\n", v; next } }
  extra != "" && /^version = / && !done { printf "version = \"%s\"\n", extra; done = 1; next }
  { print }
' Cargo.lock > Cargo.lock.next
mv -- Cargo.lock.next Cargo.lock
STUB
  chmod +x "$root/scripts/cargo.sh"
}
write_bump_tree "$scratch/bump-ok"
if ! bash "$scratch/bump-ok/scripts/bump-engine-version.sh" "$next" > "$scratch/bump-ok.log" 2>&1; then
  fail "bump-engine-version.sh failed a version-only lock refresh: $(cat "$scratch/bump-ok.log")"
fi
[[ $(awk -F'"' '/^version = /{print $2; exit}' "$scratch/bump-ok/engine/Cargo.toml") == "$next" ]] \
  || fail 'Successful bump did not write engine/Cargo.toml'
ok_lock=$(awk -v name="$package" '
  $0 == "name = \"" name "\"" { getline; print }
' "$scratch/bump-ok/Cargo.lock" | awk -F'"' '{print $2}')
[[ $ok_lock == "$next" ]] || fail "Successful bump did not write the $package version in Cargo.lock"
toml_changed=$(diff -u engine/Cargo.toml "$scratch/bump-ok/engine/Cargo.toml" | awk '/^[+-][^+-]/ {c++} END {print c+0}' || true)
lock_changed=$(diff -u Cargo.lock "$scratch/bump-ok/Cargo.lock" | awk '/^[+-][^+-]/ {c++} END {print c+0}' || true)
[[ $toml_changed == 2 && $lock_changed == 2 ]] \
  || fail "Successful bump must change only the engine version lines (toml=$toml_changed lock=$lock_changed)"
rg -q "^-version = \"$current\"\$" <(diff -u engine/Cargo.toml "$scratch/bump-ok/engine/Cargo.toml") \
  || fail 'Successful bump did not replace the Cargo.toml version line'
rg -q "^\+version = \"$next\"\$" <(diff -u engine/Cargo.toml "$scratch/bump-ok/engine/Cargo.toml") \
  || fail 'Successful bump did not write the new Cargo.toml version'
rg -q "^-version = \"$current\"\$" <(diff -u Cargo.lock "$scratch/bump-ok/Cargo.lock") \
  || fail 'Successful bump did not replace the Cargo.lock version line'
rg -q "^\+version = \"$next\"\$" <(diff -u Cargo.lock "$scratch/bump-ok/Cargo.lock") \
  || fail 'Successful bump did not write the new Cargo.lock version'
write_bump_tree "$scratch/bump-bad"
printf '9.9.9\n' > "$scratch/bump-bad/scripts/cargo.extra"
before_bad=$(sha256sum "$scratch/bump-bad/engine/Cargo.toml" "$scratch/bump-bad/Cargo.lock")
if bash "$scratch/bump-bad/scripts/bump-engine-version.sh" "$next" > "$scratch/bump-bad.log" 2>&1; then
  fail 'bump-engine-version.sh accepted a lock refresh that upgraded another package'
fi
rg -q 'more than the engine version' "$scratch/bump-bad.log" \
  || fail "unclear extra-lock-change error: $(cat "$scratch/bump-bad.log")"
[[ $(sha256sum "$scratch/bump-bad/engine/Cargo.toml" "$scratch/bump-bad/Cargo.lock") == "$before_bad" ]] \
  || fail 'Rejected bump left Cargo.toml or Cargo.lock dirty'
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
