#!/bin/sh
# Fetches the CLDR English territory names the country groups read (0.1.2, F4) into
# src-tauri/crates/ondar-map-build/input/ (gitignored) and checks the copy against cldr.tsv, the
# table the tool include_str!s and refuses on. Never run in CI: the built table
# (ondar-stations/data/countries.tsv) is committed, and the tool's input-bound tests are #[ignore]d.
#
# usage: scripts/fetch-cldr.sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
crate="$root/src-tauri/crates/ondar-map-build"
pins="$crate/cldr.tsv"
dest="$crate/input"
url="https://raw.githubusercontent.com/unicode-org/cldr-json/49.0.0/cldr-json/cldr-localenames-full/main/en/territories.json"
# Exactly one pin (groups.rs asserts the same): a loop over zero rows, or a missing table, would
# pass having checked nothing (0.1.2, E1).
[ -f "$pins" ] || { echo "FAIL $pins missing" >&2; exit 1; }
n=$(grep -v '^#' "$pins" | grep -c '[^[:space:]]' || true)
[ "$n" = 1 ] || { echo "FAIL $pins: one pin expected, $n found" >&2; exit 1; }
IFS="$(printf '\t')" read -r file bytes sha <<EOF
$(grep -v '^#' "$pins" | grep '[^[:space:]]')
EOF
mkdir -p "$dest"
if [ ! -f "$dest/$file" ] || [ "$(shasum -a 256 "$dest/$file" | cut -d' ' -f1)" != "$sha" ]; then
  curl -fsSL --retry 3 -o "$dest/$file.part" "$url"
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
