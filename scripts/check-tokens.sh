#!/bin/sh
# Fails if a colour, size or spacing literal appears in the renderer outside
# `src/styles/tokens.css`. Wired into `pnpm lint`, which CI runs.
#
# What counts as a literal: a hex colour (`#fff`, `#8884`), an `rgb()`/`rgba()`/`hsl()`/`hsla()`
# call, or a number with a length unit (`13px`, `1.5rem`, `6pt`, `2em`). `0`, percentages and
# keywords (`transparent`, `auto`) do not match, so `panel.html`'s transparent-background
# invariant is not a hit. `var(--space-2)` is what a size looks like when it is allowed.
#
# Scope: `panel.html`, and every `.css` and `.tsx` under `src/` except the tokens file itself.
# `src/bindings/` is ts-rs output and contains no styles; it is not excluded because it has
# nothing to exclude, and an accidental style literal there should fail too.
set -u
cd "$(dirname "$0")/.." || exit 2

pattern='#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?)\(|\b[0-9]+(\.[0-9]+)?(px|pt|rem|em)\b'

files=$(find src -type f \( -name '*.css' -o -name '*.tsx' \) ! -path 'src/styles/tokens.css'; echo panel.html)

hits=$(printf '%s\n' "$files" | xargs grep -nHE "$pattern" 2>/dev/null)
if [ -n "$hits" ]; then
  echo "check-tokens: style literals outside src/styles/tokens.css:" >&2
  printf '%s\n' "$hits" >&2
  exit 1
fi
echo "check-tokens: ok (no style literals outside src/styles/tokens.css)"
