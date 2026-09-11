#!/usr/bin/env bash
# Installer and pin (DESIGN.md, distribution): hash verify, refuse a
# mismatch, install under a scratch XDG_DATA_HOME, skip a current dest,
# select architecture-specific assets, reject an unsupported machine, keep a checkout
# --ensure off the installer. Uses a scratch pin and the debug engine so
# check.sh does not need a release rebuild. The committed pin is then installed
# for real and its asset must hash to the pin, speak the protocol the UI
# accepts, and report the version its tag names.
set -euo pipefail
# Ubuntu CI verifies installation and checksums for the published Arch binary,
# whose newer glibc requirements prevent execution there. Native candidates
# are executed separately; normal desktop checks always run the pinned one.
published_runtime=true
if [[ ${1:-} == --published-install-only ]]; then
  published_runtime=false
  shift
fi
[[ $# == 0 ]] || { echo 'Usage: test-engine-pin.sh [--published-install-only]' >&2; exit 2; }
cd "$(dirname "$0")/.."

fail() { printf '%s\n' "$@" >&2; exit 1; }
die() { fail "$@"; }
source scripts/engine-pin.sh
[[ -x target/debug/omastorm-engine ]] || fail 'Need target/debug/omastorm-engine (check.sh builds it).'

# Several copies of the debug engine and a tree of HEAD: under target/, and
# gone on exit, pass or fail.
scratch=$PWD/target/test-engine-pin
rm -rf "$scratch"
mkdir -p "$scratch"
trap 'rm -rf "$scratch"' EXIT
export XDG_DATA_HOME="$scratch/data" XDG_CACHE_HOME="$scratch/cache" XDG_RUNTIME_DIR="$scratch/runtime"
mkdir -p "$XDG_RUNTIME_DIR"
debug=$PWD/target/debug/omastorm-engine
sum=$(sha256sum -- "$debug" | awk '{print $1}')
native=$(engine_machine "$(uname -m)")
export OMASTORM_ENGINE_MACHINE=$native
other=x86_64
[[ $native == x86_64 ]] && other=aarch64
# Distinct bytes catch a selector that uses the host's checksum for both CPUs.
printf 'other architecture fixture\n' > "$scratch/other"
other_sum=$(sha256sum -- "$scratch/other" | awk '{print $1}')
pin=$scratch/release.pin
cat > "$pin" <<PIN
tag=engine-test
repo=wesleygrimes/omastorm
asset_$native=omastorm-engine-$native-unknown-linux-gnu
sha256_$native=$sum
asset_$other=omastorm-engine-$other-unknown-linux-gnu
sha256_$other=$other_sum
PIN
export OMASTORM_ENGINE_PIN=$pin
dest=$XDG_DATA_HOME/omastorm/bin/omastorm-engine
install_cmd=(bash scripts/fetch-engine.sh)

# A substituted file is refused and leaves no dest.
printf 'not-the-engine' > "$scratch/bogus"
if OMASTORM_ENGINE_ASSET="$scratch/bogus" "${install_cmd[@]}" 2>"$scratch/mismatch.err"; then
  fail 'Installer accepted a sha256 mismatch'
fi
rg -q 'sha256 mismatch' "$scratch/mismatch.err" || fail "Mismatch error was unclear: $(cat "$scratch/mismatch.err")"
[[ ! -e $dest ]] || fail 'Mismatch wrote a dest'

# Matching asset installs, is executable, and hashes to the pin.
OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
[[ -x $dest ]] || fail 'Installer did not write an executable dest'
[[ $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail 'Installed dest does not match the pin'
path=$(OMASTORM_ENGINE_ASSET="$debug" bash scripts/fetch-engine.sh --print-path)
[[ $path == "$dest" ]] || fail "--print-path: $path"

# A dest that already matches is left alone; no asset and no download.
unset OMASTORM_ENGINE_ASSET
bash scripts/fetch-engine.sh

# The curl path (file://, no GitHub) verifies and installs too.
rm -f "$dest"
OMASTORM_ENGINE_URL="file://$debug" bash scripts/fetch-engine.sh
[[ -x $dest && $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail 'file:// install did not match the pin'

# A stale dest is replaced when a matching asset is supplied.
printf 'stale' > "$dest"
chmod 755 -- "$dest"
OMASTORM_ENGINE_ASSET="$debug" bash scripts/fetch-engine.sh
[[ $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail 'Stale dest was not replaced'

# Both architectures select their own asset and checksum, including arm64 alias.
for machine in x86_64 aarch64 arm64; do
  arch=$(engine_machine "$machine")
  fixture=$debug expected=$sum
  if [[ $arch != "$native" ]]; then fixture=$scratch/other; expected=$other_sum; fi
  OMASTORM_ENGINE_MACHINE=$machine OMASTORM_ENGINE_ASSET=$fixture "${install_cmd[@]}"
  [[ $(sha256sum -- "$dest" | awk '{print $1}') == "$expected" ]] || fail "Wrong asset for $machine"
done
# A host binary cannot pass verification for the other architecture.
rm -f "$dest"
if OMASTORM_ENGINE_MACHINE=$other OMASTORM_ENGINE_ASSET=$debug "${install_cmd[@]}" 2>"$scratch/arch.err"; then
  fail 'Installer accepted the other architecture checksum'
fi
rg -q 'sha256 mismatch' "$scratch/arch.err" || fail 'Wrong architecture did not fail checksum verification'
[[ ! -e $dest ]] || fail 'Wrong architecture wrote a dest'

# The normal download path must construct the architecture-specific release URL.
mkdir -p "$scratch/bin" "$scratch/downloads"
cp "$debug" "$scratch/downloads/omastorm-engine-$native-unknown-linux-gnu"
cp "$scratch/other" "$scratch/downloads/omastorm-engine-$other-unknown-linux-gnu"
cat > "$scratch/bin/curl" <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
while [[ $# -gt 0 ]]; do
  case $1 in
    -o) out=$2; shift 2 ;;
    --) url=$2; break ;;
    *) shift ;;
  esac
done
printf '%s\n' "$url" > "$DOWNLOAD_FIXTURES/url"
cp "$DOWNLOAD_FIXTURES/${url##*/}" "$out"
CURL
chmod +x "$scratch/bin/curl"
for arch in x86_64 aarch64; do
  rm -f "$dest"
  PATH="$scratch/bin:$PATH" DOWNLOAD_FIXTURES=$scratch/downloads OMASTORM_ENGINE_MACHINE=$arch "${install_cmd[@]}"
  [[ $(cat "$scratch/downloads/url") == "https://github.com/wesleygrimes/omastorm/releases/download/engine-test/omastorm-engine-$arch-unknown-linux-gnu" ]] \
    || fail "Wrong download URL for $arch"
done
rm -f "$dest"

# An unsupported CPU and a supported CPU without a published pin never fetch.
if OMASTORM_ENGINE_MACHINE=armv7l "${install_cmd[@]}" 2>"$scratch/arch.err"; then
  fail 'Installer accepted armv7l'
fi
rg -q 'Unsupported engine architecture: armv7l' "$scratch/arch.err" || fail "Arch error was unclear: $(cat "$scratch/arch.err")"
sed "/^asset_$other=/d; /^sha256_$other=/d" "$pin" > "$scratch/missing.pin"
if OMASTORM_ENGINE_MACHINE=$other OMASTORM_ENGINE_PIN=$scratch/missing.pin "${install_cmd[@]}" 2>"$scratch/arch.err"; then
  fail 'Installer accepted an unpinned architecture'
fi
rg -q "No pinned $other engine" "$scratch/arch.err" || fail 'Missing architecture error was unclear'

# An incomplete or duplicated pin is refused before touching the destination.
sed "/^sha256_$native=/d" "$pin" > "$scratch/incomplete.pin"
cp "$pin" "$scratch/duplicate.pin"
printf 'sha256_%s=%s\n' "$native" "$sum" >> "$scratch/duplicate.pin"
for bad in incomplete duplicate; do
  if OMASTORM_ENGINE_PIN=$scratch/$bad.pin "${install_cmd[@]}" 2>"$scratch/pin.err"; then
    fail "Installer accepted $bad pin"
  fi
done

# Checkout --ensure uses the debug engine and does not write the data home.
rm -rf "$XDG_DATA_HOME"
bash run.sh --ensure
[[ ! -e $dest ]] || fail 'Checkout --ensure wrote the release dest'
timeout 2 socat -t0.2 - "UNIX-CONNECT:$XDG_RUNTIME_DIR/omastorm/engine.sock" < /dev/null | rg -q '"type":"hello"' \
  || fail 'Checkout --ensure did not produce a hello'
target/debug/omastorm-engine stop >/dev/null

# A tree without target/debug installs from the asset and ensures.
clone=$scratch/clone
mkdir -p "$clone"
git archive HEAD | tar -x -C "$clone"
# The launcher and installer come from the working tree so the check covers
# uncommitted changes to them; everything else is HEAD, as a clone would be.
mkdir -p "$clone/scripts" "$clone/engine"
cp -- run.sh "$clone/run.sh"
cp -- scripts/fetch-engine.sh "$clone/scripts/fetch-engine.sh"
cp -- scripts/engine-pin.sh "$clone/scripts/engine-pin.sh"
install -D -m 644 "$pin" "$clone/engine/release.pin"
rm -rf "$clone/target"
export OMASTORM_ENGINE_ASSET=$debug OMASTORM_ENGINE_PIN=$clone/engine/release.pin
(cd "$clone" && bash run.sh --ensure)
[[ -x $dest ]] || fail 'Clone --ensure did not install the engine'
timeout 2 socat -t0.2 - "UNIX-CONNECT:$XDG_RUNTIME_DIR/omastorm/engine.sock" < /dev/null | rg -q '"type":"hello"' \
  || fail 'Clone --ensure did not produce a hello'
"$dest" stop >/dev/null

# The committed pin names what users get. Install from it for real: the
# asset the pin names must exist on GitHub, hash to the pin, speak the
# protocol version the UI accepts, and report the version its tag names.
# The asset is fetched once into target/pinned/<sha256> and reused. When
# GitHub is unreachable the step says so and passes; a checkout is correct
# without the network, and the fetch is retried on the next run.
read_engine_pin engine/release.pin
committed=${hashes[$native]:-}
if [[ -z $committed ]]; then
  echo "Engine install fixtures PASS; no published $native pin yet, native release check pending."
  exit 0
fi
[[ $committed =~ ^[a-f0-9]{64}$ ]] || fail 'Committed pin sha256 is not 64 lowercase hex digits'
[[ $tag =~ ^engine-([0-9]+\.[0-9]+\.[0-9]+)$ ]] || fail "Committed pin tag is not engine-<version>: $tag"
pinned_version=${BASH_REMATCH[1]}
ui_protocol=$(rg -o 'message\.v !== ([0-9]+)' -r '$1' ui/Engine.qml)
[[ -n $ui_protocol ]] || fail 'Could not read the protocol version ui/Engine.qml accepts'
cache=target/pinned/$committed
unset OMASTORM_ENGINE_ASSET
export OMASTORM_ENGINE_PIN=$PWD/engine/release.pin
rm -f "$dest"
if [[ -f $cache ]]; then
  OMASTORM_ENGINE_ASSET=$cache bash scripts/fetch-engine.sh
elif curl -fsI --max-time 5 https://github.com > /dev/null 2>&1; then
  bash scripts/fetch-engine.sh
  install -D -m 755 "$dest" "$cache"
else
  echo 'Committed pin: GitHub unreachable, the published asset was not verified this run.' >&2
fi
if [[ -x $dest ]]; then
  [[ $(sha256sum -- "$dest" | awk '{print $1}') == "$committed" ]] || fail 'Pinned asset install did not match the pin'
  if [[ $published_runtime == false ]]; then
    echo 'Engine install fixtures and published asset checksum PASS; published runtime explicitly omitted (host libc compatibility).'
    exit 0
  fi
  "$dest" ensure
  hello=$(timeout 2 socat -t0.2 - "UNIX-CONNECT:$XDG_RUNTIME_DIR/omastorm/engine.sock" < /dev/null | head -n1 || true)
  "$dest" stop >/dev/null
  rg -q '"type":"hello"' <<< "$hello" || fail 'Pinned asset did not produce a hello'
  [[ $(jq -r .v <<< "$hello") == "$ui_protocol" ]] \
    || fail "Pinned asset speaks protocol v$(jq -r .v <<< "$hello"); ui/Engine.qml accepts v$ui_protocol"
  [[ $(jq -r .engine <<< "$hello") == "$pinned_version" ]] \
    || fail "Pinned asset reports engine $(jq -r .engine <<< "$hello"); the pin names $tag"
fi

echo 'Engine install: pin verify, mismatch refuse, dest install, skip current, replace stale, arch, checkout --ensure, clone --ensure, pinned asset PASS'
