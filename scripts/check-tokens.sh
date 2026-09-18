#!/bin/sh
# Fails if a colour, size or spacing literal appears in the renderer outside
# `src/styles/tokens.css`, or if a component styles itself inline. Wired into `pnpm lint`,
# which CI runs.
#
# What counts as a literal: a hex colour (`#fff`, `#8884`), an `rgb()`/`rgba()`/`hsl()`/`hsla()`
# call, or a number with a length unit (`13px`, `1.5rem`, `6pt`, `2em`). `0`, percentages and
# keywords (`transparent`, `auto`) do not match, so `panel.html`'s transparent-background
# invariant is not a hit. `var(--space-2)` is what a size looks like when it is allowed.
#
# Inline style objects (`style={{ padding: 16 }}`) are forbidden outright: their unitless
# numbers are sizes the unit regex cannot see, and they were exactly the M1 bench's pattern.
# All styling goes through CSS modules.
#
# Scope: `panel.html`, and every `.css`, `.ts` and `.tsx` under `src/` except the tokens file.
# `src/bindings/` is ts-rs output and is scanned like everything else; it contains no styles.
#
# Known false positive, accepted: a comment containing `#abc`-shaped text (an issue number,
# say) fails the check — rewrite the comment. Better a rare rewrite than a regex that lets a
# colour through.
#
# grep's exit status is checked, not just its output: BSD grep exits 2 on a bad pattern, and
# an unchecked status turned that — or a missing file — into a silent pass
# (`/code-review` finding 5, 2026-09-17).
set -u
cd "$(dirname "$0")/.." || exit 2

literal='#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?)\(|\b[0-9]+(\.[0-9]+)?(px|pt|rem|em)\b'
inline='style=\{\{'

files=$(find src -type f \( -name '*.css' -o -name '*.ts' -o -name '*.tsx' \) ! -path 'src/styles/tokens.css'; echo panel.html)
for f in $files; do
  [ -f "$f" ] || { echo "check-tokens: missing file $f" >&2; exit 2; }
done

status=0
# Not through xargs: BSD xargs folds a utility's exit 2 into its own 1, which reads as "no hits".
# shellcheck disable=SC2086 # $files is a newline-separated list of paths without spaces
hits=$(grep -nHE "$literal" $files); rc=$?
case $rc in
  0) echo "check-tokens: style literals outside src/styles/tokens.css:" >&2; printf '%s\n' "$hits" >&2; status=1 ;;
  1) ;;
  *) echo "check-tokens: grep failed (exit $rc) while scanning for literals" >&2; exit 2 ;;
esac

# shellcheck disable=SC2086
hits=$(grep -nHE "$inline" $files); rc=$?
case $rc in
  0) echo "check-tokens: inline style objects (use a CSS module):" >&2; printf '%s\n' "$hits" >&2; status=1 ;;
  1) ;;
  *) echo "check-tokens: grep failed (exit $rc) while scanning for inline styles" >&2; exit 2 ;;
esac

[ $status -eq 0 ] && echo "check-tokens: ok (no style literals or inline styles outside src/styles/tokens.css)"
exit $status
