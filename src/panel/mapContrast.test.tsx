// The map's text on the controls' plate (M4c k+4b, decision 7 amended) — vitest, `pnpm test`.
// Read from the stylesheets as text, so the claim is about what ships: the plate rule's text
// and background tokens, resolved in `tokens.css` for Sand (`:root`) and Ink (the dark block),
// the background composited over each surface a label can sit on, and the WCAG 2.x contrast
// ratio computed. Each test's comment states what it pins and what it would have to see to fail.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const tokensCss = readFileSync("src/styles/tokens.css", "utf8");
const panelCss = readFileSync("src/panel/panel.module.css", "utf8");
const darkAt = tokensCss.indexOf("prefers-color-scheme: dark");
const themes = { Sand: tokensCss.slice(0, darkAt), Ink: tokensCss.slice(darkAt) };

/** A token's value in one theme's block (Ink falls back to `:root` for a token it does not redefine). */
function token(theme: keyof typeof themes, name: string): string {
  const find = (css: string) => new RegExp(`${name}:\\s*([^;]+);`).exec(css)?.[1].trim();
  const v = find(themes[theme]) ?? find(themes.Sand);
  if (v === undefined) throw new Error(`no ${name} in ${theme}`);
  return v;
}

type Rgba = [number, number, number, number];
/** A six-digit hex colour, or the rgb function with an optional alpha; channels 0–1. */
function colour(v: string): Rgba {
  const hex = /^#([0-9a-fA-F]{6})$/.exec(v);
  if (hex) return [0, 2, 4].map((i) => parseInt(hex[1].slice(i, i + 2), 16) / 255).concat(1) as Rgba;
  const rgb = /^rgb\((\d+) (\d+) (\d+)(?: \/ ([\d.]+))?\)$/.exec(v);
  if (rgb) return [Number(rgb[1]) / 255, Number(rgb[2]) / 255, Number(rgb[3]) / 255, rgb[4] === undefined ? 1 : Number(rgb[4])];
  throw new Error(`not a colour: ${v}`);
}
const over = (top: Rgba, under: Rgba): Rgba =>
  [0, 1, 2].map((i) => top[i] * top[3] + under[i] * (1 - top[3])).concat(1) as Rgba;
const luminance = (c: Rgba) => {
  const lin = (x: number) => (x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
  return 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
};
const contrast = (a: Rgba, b: Rgba) => {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
};

/** A rule's body in panel.module.css. */
function rule(selector: string): string {
  const start = panelCss.indexOf(`${selector} {`);
  if (start < 0) throw new Error(`no rule ${selector}`);
  return panelCss.slice(start, panelCss.indexOf("}", start));
}
/** The token a declaration uses: `color: var(--x)` → `--x`. */
function uses(body: string, property: string): string {
  const m = new RegExp(`(?:^|[\\s;{])${property}:\\s*var\\((--[\\w-]+)\\)`).exec(body);
  if (m === null) throw new Error(`no ${property}: var(…) in ${body}`);
  return m[1];
}

describe("the map's plate", () => {
  // 1. The hover label's and MT's text on the plate reads at WCAG 1.4.3's 4.5 in both themes on
  //    every surface it can sit on: the plate's background composited over the sea, the land
  //    and the neighbours. The worst case is land (Sand 10.13, Ink 7.49 at 935737c's tokens);
  //    the floors 10.1 / 7.4 pin those figures, 4.5 the requirement. Fails with the text back
  //    on `--map-label` (Ink on land 1.13), a transparent plate (Ink on land 3.6), or any
  //    token change that drops a figure below its floor.
  it("1. the plate's text contrasts >= 4.5 over sea, land and neighbours, Sand and Ink", () => {
    const body = rule(".mapPlate");
    const fgName = uses(body, "color");
    const bgName = uses(body, "background");
    const floor = { Sand: 10.1, Ink: 7.4 };
    for (const theme of ["Sand", "Ink"] as const) {
      const fg = colour(token(theme, fgName));
      const bg = colour(token(theme, bgName));
      const ratios = ["--map-sea", "--map-land", "--map-neighbour"].map((s) =>
        contrast(fg, over(bg, colour(token(theme, s)))),
      );
      for (const r of ratios) {
        expect(r, theme).toBeGreaterThanOrEqual(4.5);
        expect(r, theme).toBeGreaterThanOrEqual(floor[theme]);
      }
    }
  });

  // 2. The plate's font is at least 11 px (the subheadline, 11/14), not the inset labels' 8.
  //    Fails with the font back on `--text-map-label`.
  it("2. the plate's font is >= 11 px", () => {
    const font = token("Sand", uses(rule(".mapPlate"), "font"));
    const size = /(\d+(?:\.\d+)?)px\//.exec(font);
    expect(size, font).not.toBeNull();
    expect(Number(size![1])).toBeGreaterThanOrEqual(11);
  });

  // 3. The plate never takes the pointer, and a long place ends in an ellipsis on one line:
  //    the hover label sits beside its dot and over its neighbours, so with pointer events it
  //    would fire the dots' leave and enter as it appears (flicker). Fails without either.
  it("3. the hover label takes no pointer and clips a long place to one line", () => {
    const tip = rule(".mapTip");
    expect(tip).toMatch(/pointer-events:\s*none/);
    expect(tip).toMatch(/white-space:\s*nowrap/);
    expect(tip).toMatch(/text-overflow:\s*ellipsis/);
    expect(tip).toMatch(/overflow:\s*hidden/);
    expect(rule(".mapNote")).toMatch(/pointer-events:\s*none/);
  });
});
