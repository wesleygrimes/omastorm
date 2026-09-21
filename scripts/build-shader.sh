#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
for shader in radar tile grid; do
  "${QSB:-/usr/lib/qt6/bin/qsb}" --glsl '100 es,120,150' --hlsl 50 --msl 12 \
    -o "ui/shaders/$shader.frag.qsb" "ui/shaders/$shader.frag"
done
