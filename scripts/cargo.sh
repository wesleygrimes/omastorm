#!/usr/bin/env bash
# Prefer an installed toolchain; also support an isolated checkout-local one.
set -euo pipefail
cd "$(dirname "$0")/.."
if ! command -v cargo >/dev/null && [[ -x .tools/cargo/bin/cargo ]]; then
  export RUSTUP_HOME="$PWD/.tools/rustup" CARGO_HOME="$PWD/.tools/cargo"
  export PATH="$CARGO_HOME/bin:$PATH"
fi
# Integration isolates XDG paths and blocks live providers. A mise cargo shim
# would look for tools in that scratch home and try installing them again.
# Resolve rustup's installed toolchain without downloading, including rustc.
rustup_bin="${CARGO_HOME:-$HOME/.cargo}/bin/rustup"
if [[ ! -x "$rustup_bin" ]]; then
  rustup_bin=$(command -v rustup || true)
fi
if [[ -n "$rustup_bin" ]]; then
  cargo_bin=$("$rustup_bin" which cargo)
  export PATH="${cargo_bin%/*}:$PATH"
fi
# Build through mbx (shared build cache) when it is installed; it runs the
# Cargo found first on PATH. A mise shim is resolved to the real binary, and
# an unresolvable one falls back to plain Cargo. MBX_DISABLE=1 skips mbx.
mbx_bin=$(command -v mbx || true)
if [[ $mbx_bin == */mise/shims/* ]]; then
  mbx_bin=$(mise which mbx 2>/dev/null || true)
fi
if [[ -n "$mbx_bin" && -z "${MBX_DISABLE:-}" ]]; then
  # Keep the shared cache and managed target/ out of a caller's scratch
  # XDG_CACHE_HOME; otherwise target/ would point into a deleted temp dir.
  export MBX_CACHE_DIR="${MBX_CACHE_DIR:-$HOME/.cache/mbx}"
  # Protocol tests bind Unix sockets under target/; a managed target's long
  # cache path would exceed SUN_LEN, so target/ stays a real directory.
  export MBX_TARGET_VIEWS="${MBX_TARGET_VIEWS:-0}"
  exec "$mbx_bin" "$@"
fi
exec "${cargo_bin:-cargo}" "$@"
