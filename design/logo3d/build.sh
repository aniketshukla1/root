#!/bin/bash
# Renders the 3D root mark and writes website/media/logo-3d-{dark,light}.webp. About ten minutes.
# Needs Chrome, Node 22 and Python with Pillow and numpy.
set -e
cd "$(dirname "$0")"
RAW="${TMPDIR:-/tmp}/root-logo3d"
for theme in dark light; do
  light=0; [ "$theme" = light ] && light=1
  rm -rf "$RAW/$theme"
  SUB=4 node capture.mjs "$RAW/$theme" 960 frames:120 "$light"
done
python3 encode.py "$RAW" "${OUT:-../../website/media}" 320 50 55 80
rm -rf "$RAW"
