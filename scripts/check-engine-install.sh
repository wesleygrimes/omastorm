#!/usr/bin/env bash
# Installer and pin (DESIGN.md, distribution): hash verify, refuse a
# mismatch, install under a scratch XDG_DATA_HOME, skip a current dest,
# reject an unsupported machine, keep ordinary launch and a checkout
# --ensure off the installer. Uses a scratch pin and the debug engine so
# check.sh does not need a release rebuild. When target/dist matches the
# committed pin, that asset is installed too.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { printf '%s\n' "$@" >&2; exit 1; }
[[ -x target/debug/omastorm-engine ]] || fail 'Need target/debug/omastorm-engine (check.sh builds it).'

scratch=$(mktemp -d /tmp/omastorm-engine-install.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
export XDG_DATA_HOME="$scratch/data" XDG_CACHE_HOME="$scratch/cache" XDG_RUNTIME_DIR="$scratch/runtime"
mkdir -p "$XDG_RUNTIME_DIR"
debug=$PWD/target/debug/omastorm-engine
machine=$(uname -m)
export OMASTORM_ENGINE_MACHINE=$machine
sum=$(sha256sum -- "$debug" | awk '{print $1}')
pin=$scratch/release.pin
cat > "$pin" <<PIN
tag=engine-test
repo=wesleygrimes/omastorm
asset=omastorm-engine-$machine-unknown-linux-gnu
sha256=$sum
PIN
export OMASTORM_ENGINE_PIN=$pin
dest=$XDG_DATA_HOME/omastorm/bin/omastorm-engine
install_cmd=(bash scripts/install-engine.sh)

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
path=$(OMASTORM_ENGINE_ASSET="$debug" bash scripts/install-engine.sh --print-path)
[[ $path == "$dest" ]] || fail "--print-path: $path"

# A dest that already matches is left alone; no asset and no download.
unset OMASTORM_ENGINE_ASSET
bash scripts/install-engine.sh

# The curl path (file://, no GitHub) verifies and installs too.
rm -f "$dest"
OMASTORM_ENGINE_URL="file://$debug" bash scripts/install-engine.sh
[[ -x $dest && $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail 'file:// install did not match the pin'

# A stale dest is replaced when a matching asset is supplied.
printf 'stale' > "$dest"
chmod 755 -- "$dest"
OMASTORM_ENGINE_ASSET="$debug" bash scripts/install-engine.sh
[[ $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail 'Stale dest was not replaced'

# Both architectures select and verify their own asset. These are installer
# fixtures, not cross-compiled executables; only the native debug engine runs.
for arch in x86_64 aarch64; do
  arch_pin=$scratch/$arch.pin
  sed "s/^asset=.*/asset=omastorm-engine-$arch-unknown-linux-gnu/" "$pin" > "$arch_pin"
  rm -f "$dest"
  OMASTORM_ENGINE_MACHINE=$arch OMASTORM_ENGINE_PIN=$arch_pin OMASTORM_ENGINE_ASSET=$debug bash scripts/install-engine.sh
  [[ $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail "$arch install did not match"
  if OMASTORM_ENGINE_MACHINE=$arch OMASTORM_ENGINE_PIN=$arch_pin OMASTORM_ENGINE_ASSET="$scratch/bogus" bash scripts/install-engine.sh 2>"$scratch/current.err"; then
    : # Already current: no asset read.
  else
    fail "$arch current install was not skipped"
  fi
  printf 'stale' > "$dest"
  if OMASTORM_ENGINE_MACHINE=$arch OMASTORM_ENGINE_PIN=$arch_pin OMASTORM_ENGINE_ASSET="$scratch/bogus" bash scripts/install-engine.sh 2>"$scratch/mismatch.err"; then
    fail "$arch accepted a substituted asset"
  fi
  rg -q 'sha256 mismatch' "$scratch/mismatch.err" || fail "$arch mismatch error unclear"
  [[ $(cat "$dest") == stale ]] || fail "$arch mismatch replaced existing engine"
done

# Refuse a pin for the wrong architecture before using even a current dest.
if OMASTORM_ENGINE_MACHINE=aarch64 OMASTORM_ENGINE_PIN=$scratch/x86_64.pin bash scripts/install-engine.sh 2>"$scratch/arch.err"; then
  fail 'Installer accepted an x86_64 asset on aarch64'
fi
rg -q 'does not match architecture' "$scratch/arch.err" || fail 'Wrong-architecture error was unclear'
if OMASTORM_ENGINE_MACHINE=riscv64 bash scripts/install-engine.sh 2>"$scratch/arch.err"; then
  fail 'Installer accepted an unsupported architecture'
fi
rg -q 'Unsupported engine architecture: riscv64' "$scratch/arch.err" || fail "Arch error was unclear: $(cat "$scratch/arch.err")"

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
# Working tree: this check runs before the packaging commit is on HEAD.
mkdir -p "$clone/scripts" "$clone/engine"
cp -- run.sh "$clone/run.sh"
cp -- scripts/install-engine.sh "$clone/scripts/install-engine.sh"
# Exercise default pin selection separately from the explicit pin override.
install -D -m 644 "$scratch/aarch64.pin" "$clone/engine/release-aarch64.pin"
install -D -m 644 "$scratch/x86_64.pin" "$clone/engine/release.pin"
for arch in x86_64 aarch64; do
  rm -f "$dest"
  (cd "$clone" && env -u OMASTORM_ENGINE_PIN OMASTORM_ENGINE_MACHINE=$arch OMASTORM_ENGINE_ASSET=$debug bash scripts/install-engine.sh)
  [[ -x $dest ]] || fail "$arch default pin did not install"
done
mv "$clone/engine/release-aarch64.pin" "$clone/engine/release-aarch64.pin.saved"
if (cd "$clone" && env -u OMASTORM_ENGINE_PIN OMASTORM_ENGINE_MACHINE=aarch64 bash scripts/install-engine.sh) 2>"$scratch/missing.err"; then
  fail 'Installer accepted a missing ARM64 pin'
fi
rg -q 'No pinned aarch64 engine release' "$scratch/missing.err" || fail 'Missing ARM64 pin error was unclear'
rg -q 'cargo.sh build --locked' "$scratch/missing.err" || fail 'Missing ARM64 pin omitted source-build guidance'
mv "$clone/engine/release-aarch64.pin.saved" "$clone/engine/release-aarch64.pin"
rm -rf "$clone/target"
export OMASTORM_ENGINE_ASSET=$debug OMASTORM_ENGINE_PIN=$pin
(cd "$clone" && bash run.sh --ensure)
[[ -x $dest ]] || fail 'Clone --ensure did not install the engine'
timeout 2 socat -t0.2 - "UNIX-CONNECT:$XDG_RUNTIME_DIR/omastorm/engine.sock" < /dev/null | rg -q '"type":"hello"' \
  || fail 'Clone --ensure did not produce a hello'
"$dest" stop >/dev/null

# Every committed pin is well formed; its dist asset must match when present.
for arch in x86_64 aarch64; do
  committed_pin=engine/release.pin
  [[ $arch == aarch64 ]] && committed_pin=engine/release-aarch64.pin
  [[ -f $committed_pin ]] || continue
  committed=$(awk -F= '/^sha256=/{print $2}' "$committed_pin")
  [[ $committed =~ ^[a-f0-9]{64}$ ]] || fail "$committed_pin sha256 is not 64 lowercase hex digits"
  dist=target/dist/omastorm-engine-$arch-unknown-linux-gnu
  if [[ -f $dist ]]; then
    [[ $(sha256sum -- "$dist" | awk '{print $1}') == "$committed" ]] \
      || fail "$dist does not match $committed_pin"
    unset OMASTORM_ENGINE_PIN
    export OMASTORM_ENGINE_ASSET=$dist OMASTORM_ENGINE_PIN=$PWD/$committed_pin OMASTORM_ENGINE_MACHINE=$arch
    rm -f "$dest"
    bash scripts/install-engine.sh
    [[ $(sha256sum -- "$dest" | awk '{print $1}') == "$committed" ]] \
      || fail 'Committed-pin install did not match'
  fi
done

echo 'Engine install: pin verify, mismatch refuse, dest install, skip current, replace stale, arch, checkout --ensure, clone --ensure PASS'
