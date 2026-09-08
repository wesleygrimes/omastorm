#!/usr/bin/env bash
# Build the native Linux x86_64 or aarch64 GitHub Release asset and SHA256SUMS
# under target/dist/. Pass --write-pin to copy the hash into the host's pin
# after a successful build. Does not publish, tag, or push.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }

write_pin=0
[[ ${1:-} == --write-pin ]] && write_pin=1

if ! command -v rustc >/dev/null && [[ -x .tools/cargo/bin/rustc ]]; then
  export RUSTUP_HOME="$PWD/.tools/rustup" CARGO_HOME="$PWD/.tools/cargo"
  export PATH="$CARGO_HOME/bin:$PATH"
fi
host=$(rustc -vV | awk '/^host:/{print $2}')
case $host in
  x86_64-unknown-linux-gnu) pin=engine/release.pin ;;
  aarch64-unknown-linux-gnu) pin=engine/release-aarch64.pin ;;
  *) die "Unsupported release host: $host (use native Linux x86_64 or aarch64)." ;;
esac

# Explicit target keeps Cargo configuration from selecting another architecture.
bash scripts/cargo.sh build --release --locked --offline --target "$host"
src=target/$host/release/omastorm-engine
[[ -x $src ]] || die "cargo did not produce $src"

mkdir -p target/dist
asset=omastorm-engine-$host
dest=target/dist/$asset
cp -- "$src" "$dest"
strip --strip-unneeded -- "$dest"
chmod 755 -- "$dest"

sum=$(sha256sum -- "$dest" | awk '{print $1}')
# sha256sum -c format, names as they appear on the Release.
(cd target/dist && sha256sum -- omastorm-engine-*-unknown-linux-gnu > SHA256SUMS)
printf '%s  %s\n' "$sum" "$dest"

version=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
if (( write_pin )); then
  cat > "$pin" <<PIN
# Pinned GitHub Release for the engine binary (DESIGN.md, distribution).
# Bump only after the named release exists on wesleygrimes/omastorm.
tag=engine-$version
repo=wesleygrimes/omastorm
asset=$asset
sha256=$sum
PIN
fi

if [[ -f $pin ]]; then
  expected=$(awk -F= '/^sha256=/{print $2}' "$pin")
  [[ $sum == "$expected" ]] || die "Built $dest ($sum) does not match $pin ($expected)." \
    "Re-run with --write-pin only when preparing a new engine release."
fi
