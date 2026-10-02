#!/bin/sh
# Fetches the Natural Earth 10m inputs of the map build tool (M4a) into
# src-tauri/crates/ondar-map-build/input/ (gitignored) and checks each against pins.tsv —
# the same table the tool include_str!s and refuses on. Never run in CI: the built resource
# is committed (decision D3), and the tool's input-bound tests are #[ignore]d.
#
# usage: scripts/fetch-natural-earth.sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
crate="$root/src-tauri/crates/ondar-map-build"
pins="$crate/pins.tsv"
dest="$crate/input"
base="https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/10m_cultural"
mkdir -p "$dest"
grep -v '^#' "$pins" | while IFS="$(printf '\t')" read -r file bytes sha; do
  [ -n "$file" ] || continue
  if [ ! -f "$dest/$file" ] || [ "$(shasum -a 256 "$dest/$file" | cut -d' ' -f1)" != "$sha" ]; then
    curl -fsSL --retry 3 -o "$dest/$file.part" "$base/$file"
    mv "$dest/$file.part" "$dest/$file"
  fi
  got=$(shasum -a 256 "$dest/$file" | cut -d' ' -f1)
  size=$(wc -c < "$dest/$file" | tr -d ' ')
  if [ "$got" = "$sha" ] && [ "$size" = "$bytes" ]; then
    echo "ok   $file  $size B  $got"
  else
    echo "FAIL $file  $size B (pinned $bytes)  $got (pinned $sha)" >&2
    exit 1
  fi
done
