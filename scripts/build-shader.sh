#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
for shader in radar tile grid; do
  "${QSB:-/usr/lib/qt6/bin/qsb}" --glsl '100 es,120,150' --hlsl 50 --msl 12 \
    -o "ui/shaders/$shader.frag.qsb" "ui/shaders/$shader.frag"
done
# textureSize needs ES 3 (desktop GLSL 120 uses GL_EXT_gpu_shader4).
# Backends unable to execute this check reject the grid instead of resampling it.
"${QSB:-/usr/lib/qt6/bin/qsb}" --glsl '120,150,300 es' --hlsl 50 --msl 12 \
  -o ui/shaders/texture-check.frag.qsb ui/shaders/texture-check.frag
