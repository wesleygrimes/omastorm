#!/usr/bin/env bash
# Exercise a native candidate's actual hello in an isolated runtime.
set -euo pipefail
cd "$(dirname "$0")/.."
binary=$(realpath "${1:?Usage: check-engine-binary.sh binary [version]}")
version=${2:-$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)}
protocol=$(sed -n 's/^pub const VERSION: u32 = \([0-9][0-9]*\);$/\1/p' engine/src/protocol.rs)
[[ -n $protocol ]] || { echo 'Could not read engine protocol version' >&2; exit 1; }
scratch=$(mktemp -d /tmp/omastorm-binary-check.XXXXXX)
export XDG_RUNTIME_DIR=$scratch/runtime XDG_CACHE_HOME=$scratch/cache XDG_DATA_HOME=$scratch/data
unset OMASTORM_ARCHIVE
mkdir -p "$XDG_RUNTIME_DIR"
trap '"$binary" stop >/dev/null 2>&1 || true; rm -rf "$scratch"' EXIT
"$binary" ensure
hello=$(timeout 2 socat -t0.2 - "UNIX-CONNECT:$XDG_RUNTIME_DIR/omastorm/engine.sock" < /dev/null | head -n1 || true)
jq -e --arg version "$version" --argjson protocol "$protocol" \
  '.type == "hello" and .engine == $version and .v == $protocol' <<< "$hello" >/dev/null \
  || { echo "Candidate hello does not match engine $version / protocol $protocol: $hello" >&2; exit 1; }
echo "Native candidate: engine $version / protocol $protocol PASS"
