#!/usr/bin/env bash
# Fresh-checkout setup, run through `mise setup` so the mise tools are on
# PATH. Checks the desktop packages mise does not manage, extracts verified
# fixtures, fetches crates, and builds the debug engine. Launch never calls
# this. Fixture bytes come from data/fixtures/; cargo fetch is the remaining
# network step.
set -euo pipefail
cd "$(dirname "$0")/.."

missing=()
need() { # command, package
  command -v "$1" > /dev/null 2>&1 || missing+=("$2")
}
need quickshell quickshell
need socat socat
[[ -x ${QSB:-/usr/lib/qt6/bin/qsb} ]] || missing+=(qt6-shadertools)
if (( ${#missing[@]} )); then
  printf 'Missing desktop packages: %s\n' "${missing[*]}" >&2
  printf 'Install these packages, then re-run mise setup.\n' >&2
  exit 1
fi
command -v magick > /dev/null 2>&1 || echo 'Optional: imagemagick (captures) is not installed.' >&2
command -v ffmpeg > /dev/null 2>&1 || echo 'Optional: ffmpeg (demo video) is not installed.' >&2

bash scripts/setup-fixture.sh
cargo fetch --locked
cargo build --offline --locked
echo 'Setup complete. Next: mise start'
