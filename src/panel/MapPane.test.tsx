// The map pane's pull rules and rendering (M4b commit 7 and the acceptance review) — vitest
// under jsdom, `pnpm test`. `../api` is mocked whole: every `pull` is a deferred promise the
// test resolves, so what the pane sends, and when, is observable. Each test's comment states
// what it pins and what it would have to see to fail.
import { readFileSync } from "node:fs";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Frame, MapBand, MapInputs, MapReply, MapStatus } from "../api";
import MapPane, { DRAG_THRESHOLD_PT, pathOf } from "./MapPane";

type Deferred = { resolve: (r: MapReply | null) => void; inputs: MapInputs };
const pulls: Deferred[] = [];
const selects: string[] = [];

vi.mock("../api", () => ({
  map: {
    select: (code: string) => {
      selects.push(code);
      return Promise.resolve();
    },
    pull: (inputs: MapInputs) =>
      new Promise<MapReply | null>((resolve) => {
        pulls.push({ resolve, inputs });
      }),
  },
  measure: { report: () => Promise.resolve() },
}));

const band: MapBand = { x: 16, y: 404, width: 328, height: 178, controls: [246, 146, 74, 24] };

const frame = (n: number): Frame => ({
  view: { centre: [0, 0], scale: 2 },
  level: 1.5,
  neighbours: [{ rings: [[[0, 0], [10, 0], [10, 10]]] }],
  land: Array.from({ length: n }, (_, i) => ({ rings: [[[i, 0], [i + 5, 0], [i + 5, 5]]] })),
  subdivisions: [],
  insets: [{ label: "Antilles", rect: [260, 8, 60, 44], land: [{ rings: [[[270, 20], [280, 20], [280, 30]]] }] }],
  stats: { vertices: 3 * n, rings_considered: n, rings_skipped: 0, missing_blobs: 0, insets_dropped: 0 },
});

const reply = (seq: number, n: number): MapReply => ({
  seq,
  status: "frame",
  band,
  view: { centre: [0, 0], scale: 2 },
  frame: frame(n),
});

// jsdom's requestAnimationFrame is a timer: run it.
const frames = (k = 1) =>
  act(async () => {
    for (let i = 0; i < k; i++) {
      await new Promise((r) => setTimeout(r, 20));
    }
  });
const settle = () => act(async () => {});

const landPaths = (root: HTMLElement) => root.querySelectorAll("svg g:nth-of-type(2) path");

beforeEach(() => {
  pulls.length = 0;
  selects.length = 0;
});
afterEach(cleanup);

async function mounted() {
  const r = render(<MapPane band={band} country="FR" />);
  await settle();
  await frames();
  // the mount: select, then one pull
  expect(selects).toEqual(["FR"]);
  expect(pulls.length).toBe(1);
  return r;
}

describe("MapPane", () => {
  // 1. One pull in flight: three wheel events while a reply is held produce one pull after it
  //    resolves, carrying the summed deltas. Fails on a pull per event, or input lost.
  it("1. holds one pull in flight and sums the input that arrives meanwhile", async () => {
    const { container } = await mounted();
    const svg = container.querySelector("svg")!;
    fireEvent.wheel(svg, { deltaX: 3, deltaY: 1 });
    fireEvent.wheel(svg, { deltaX: 4, deltaY: 2 });
    fireEvent.wheel(svg, { deltaX: 5, deltaY: 3 });
    await frames(3);
    expect(pulls.length).toBe(1);
    act(() => pulls[0].resolve(reply(1, 2)));
    await settle();
    await frames();
    expect(pulls.length).toBe(2);
    expect(pulls[1].inputs).toEqual({ pan_pt: [12, 6], zoom_steps: 0, fit: false });
  });

  // 2. A reply whose `seq` is not newer than the frame on screen is not drawn, a newer one is.
  //    Fails if a stale reply replaces the paths.
  it("2. draws only a reply newer than the frame on screen", async () => {
    const { container } = await mounted();
    act(() => pulls[0].resolve(reply(5, 3)));
    await settle();
    expect(landPaths(container).length).toBe(3);
    const svg = container.querySelector("svg")!;
    fireEvent.wheel(svg, { deltaX: 1, deltaY: 0 });
    await frames();
    act(() => pulls[1].resolve(reply(4, 7)));
    await settle();
    expect(landPaths(container).length).toBe(3);
    fireEvent.wheel(svg, { deltaX: 1, deltaY: 0 });
    await frames();
    act(() => pulls[2].resolve(reply(6, 7)));
    await settle();
    expect(landPaths(container).length).toBe(7);
  });

  // 3. A `null` reply leaves the paths as they are (fails if the map clears). 4. Idle → no pull:
  //    once the pending input is sent nothing pulls until new input (fails on a pull per
  //    animation frame).
  it("3. a null reply leaves the paths; 4. idle, nothing pulls", async () => {
    const { container } = await mounted();
    act(() => pulls[0].resolve(reply(1, 4)));
    await settle();
    const svg = container.querySelector("svg")!;
    fireEvent.wheel(svg, { deltaX: 2, deltaY: 0 });
    await frames();
    act(() => pulls[1].resolve(null));
    await settle();
    expect(landPaths(container).length).toBe(4);
    await frames(5);
    expect(pulls.length).toBe(2);
  });

  // 5. A drag under 4 pt sends nothing, over it sends the delta negated. Fails with no threshold,
  //    or with the sign of a wheel pan.
  it("5. a drag pans past the threshold, negated; under it nothing", async () => {
    const { container } = await mounted();
    act(() => pulls[0].resolve(reply(1, 1)));
    await settle();
    const svg = container.querySelector("svg")!;
    svg.setPointerCapture = () => {};
    fireEvent.pointerDown(svg, { button: 0, buttons: 1, clientX: 100, clientY: 100, pointerId: 1 });
    fireEvent.pointerMove(svg, { buttons: 1, clientX: 102, clientY: 101, pointerId: 1 });
    await frames(2);
    expect(pulls.length).toBe(1);
    fireEvent.pointerMove(svg, { buttons: 1, clientX: 100 + DRAG_THRESHOLD_PT + 6, clientY: 100 - 3, pointerId: 1 });
    fireEvent.pointerUp(svg, { pointerId: 1 });
    await frames();
    expect(pulls.length).toBe(2);
    expect(pulls[1].inputs.pan_pt).toEqual([-(DRAG_THRESHOLD_PT + 6), 3]);
  });

  // 6. `+` sends `zoom_steps: 1`, `−` −1, `fit` sends `fit: true`. Fails if the controls are
  //    wired to the wrong field.
  it("6. the controls send zoom steps and fit", async () => {
    const r = await mounted();
    act(() => pulls[0].resolve(reply(1, 1)));
    await settle();
    fireEvent.click(r.getByRole("button", { name: "Zoom in" }));
    await frames();
    expect(pulls[1].inputs).toEqual({ pan_pt: [0, 0], zoom_steps: 1, fit: false });
    act(() => pulls[1].resolve(null));
    await settle();
    fireEvent.click(r.getByRole("button", { name: "Zoom out" }));
    fireEvent.click(r.getByRole("button", { name: "Zoom out" }));
    await frames();
    expect(pulls[2].inputs.zoom_steps).toBe(-2);
    act(() => pulls[2].resolve(null));
    await settle();
    fireEvent.click(r.getByRole("button", { name: "Fit the country" }));
    await frames();
    expect(pulls[3].inputs.fit).toBe(true);
  });

  // 7. One `<path>` per shape with `fill-rule="evenodd"`, every inset's label whole, the platter
  //    at the band's size and the controls at the rect Rust gave. Fails if the renderer computes
  //    any of it.
  it("7. one path per shape, even-odd, the label whole, the platter and controls at the rects given", async () => {
    const { container } = await mounted();
    act(() => pulls[0].resolve(reply(1, 2)));
    await settle();
    const paths = container.querySelectorAll("svg path");
    expect(landPaths(container).length).toBe(2);
    for (const p of Array.from(landPaths(container))) expect(p.getAttribute("fill-rule")).toBe("evenodd");
    expect(paths.length).toBeGreaterThanOrEqual(2 + 1 + 1);
    expect(container.querySelector("text")!.textContent).toBe("Antilles");
    const root = document.documentElement.style;
    // the values are the band's, as `MapPane` sets them (not literals: `check-tokens.sh`)
    const px = (v: number) => `${v}px`;
    expect(root.getPropertyValue("--map-band-width")).toBe(px(band.width));
    expect(root.getPropertyValue("--map-band-height")).toBe(px(band.height));
    expect(root.getPropertyValue("--map-controls-x")).toBe(px(band.controls[0]));
    expect(root.getPropertyValue("--map-controls-y")).toBe(px(band.controls[1]));
    expect(container.querySelector("svg")!.getAttribute("viewBox")).toBe("0 0 328 178");
    expect(pathOf({ rings: [[[1, 2], [3, 4]], [[5, 6]]] })).toBe("M1 2L3 4ZM5 6Z");
  });

  // 9. One flat land tone per theme (the acceptance review's A1): the SVG carries no `<filter>`
  //    and no element is filtered, and the stylesheets carry no `filter`, no `opacity` and no
  //    coast token — Ink's inland tone through an erode/blur filter cost ~90 ms a paint at 300
  //    and its glow over the neighbours broke the flat-neighbours spec. Fails on the code before
  //    it.
  it("9. one flat land tone per theme: no filter, no opacity, no coast token", async () => {
    const { container } = await mounted();
    act(() => pulls[0].resolve(reply(1, 2)));
    await settle();
    expect(container.querySelector("filter")).toBeNull();
    expect(container.querySelector("[filter]")).toBeNull();
    // the stylesheets as text (paths from the repo root, vitest's cwd under `pnpm test`): the
    // one-flat-tone rule is pinned on the source
    const panelCss = readFileSync("src/panel/panel.module.css", "utf8");
    const mapRules = panelCss.slice(panelCss.indexOf(".platter"));
    expect(mapRules).not.toMatch(/\bfilter\s*:/);
    expect(mapRules).not.toMatch(/\bopacity\s*:/);
    const tokensCss = readFileSync("src/styles/tokens.css", "utf8");
    expect(tokensCss).not.toContain("--map-land-coast");
    expect(tokensCss).toContain("--map-land:");
  });

  // 10. The land is drawn once, with its hairline, and the subdivisions above it (round 3,
  //    C1 + C2): the SVG carries no `<use>` — WebKit styles a `<use>` clone as the original
  //    element, so the edge group's clones of the land paths painted the land fill again, over
  //    the subdivisions, with `stroke: none`: no interior borders, no coast, in every capture —
  //    the land rule strokes `--map-edge`, and the subdivisions group follows the land group.
  //    Fails on the code before it: a `<use>` per land path and `stroke: none` on the land.
  it("10. the land is drawn once with its hairline, the subdivisions above it", async () => {
    const { container } = await mounted();
    const r = reply(1, 2);
    r.frame!.subdivisions = [
      [[1, 1], [2, 2], [3, 1]],
      [[4, 4], [5, 5]],
    ];
    act(() => pulls[0].resolve(r));
    await settle();
    // no `<use>` anywhere in the map
    expect(container.querySelector("svg use")).toBeNull();
    // the groups in draw order: neighbours, land, subdivisions, then the insets
    const groups = Array.from(container.querySelectorAll("svg > g"));
    const landIdx = groups.findIndex((g) => g.querySelector("path#map-land-0") !== null);
    const subIdx = groups.findIndex((g) => g.querySelectorAll("path").length === 2 && g.querySelector("path")!.getAttribute("d") === "M1 1L2 2L3 1");
    expect(landIdx).toBeGreaterThan(0);
    expect(subIdx).toBe(landIdx + 1);
    // the land's rule strokes the edge token; nothing strokes `none` in the map rules
    const panelCss = readFileSync("src/panel/panel.module.css", "utf8");
    const landRule = panelCss.slice(panelCss.indexOf(".land path {"), panelCss.indexOf("}", panelCss.indexOf(".land path {")));
    expect(landRule).toMatch(/stroke:\s*var\(--map-edge\)/);
    expect(landRule).not.toMatch(/stroke:\s*none/);
  });

  // 11. The controls follow the theme (round 3, C3, Martín): the `− fit +` buttons take their
  //    background, text and border from `--map-controls-*` tokens defined under both the light
  //    root and the dark block — dark translucent in Ink, light in Sand. Fails on the code
  //    before it: the native button look, no such tokens.
  it("11. the controls follow the theme", async () => {
    const panelCss = readFileSync("src/panel/panel.module.css", "utf8");
    const start = panelCss.indexOf(".controls > button {");
    const rule = panelCss.slice(start, panelCss.indexOf("}", start));
    expect(rule).toMatch(/background:\s*var\(--map-controls-bg\)/);
    expect(rule).toMatch(/color:\s*var\(--map-controls-fg\)/);
    const tokens = readFileSync("src/styles/tokens.css", "utf8");
    const dark = tokens.indexOf("prefers-color-scheme: dark");
    for (const t of ["--map-controls-bg:", "--map-controls-fg:", "--map-controls-stroke:"]) {
      expect(tokens.slice(0, dark)).toContain(t);
      expect(tokens.slice(dark)).toContain(t);
    }
  });

  // 8. A theme change pulls nothing: recolouring is CSS. Fails if the pane requests a frame on
  //    `prefers-color-scheme`.
  // 12. Each status renders its text or nothing: `frame` draws the paths and no line,
  //     `no_map` and `unavailable` their line and no path, `no_band` neither (the band is Rust's;
  //     with none the page draws nothing). Fails if a status's text changes or `no_band` shows
  //     a line. That a status the switch does not know fails typecheck is the `never` arm's, not
  //     this test's (M4b's review, latent 14).
  it("12. each status renders its text or nothing", async () => {
    const cases: [MapStatus, string | null, number][] = [
      ["frame", null, 2],
      ["no_map", "No map for this country", 0],
      ["unavailable", "Map unavailable", 0],
      ["no_band", null, 0],
    ];
    for (const [status, text, paths] of cases) {
      const { container, unmount } = await mounted();
      const r: MapReply =
        status === "frame" ? reply(1, 2) : { seq: 1, status, band: null, view: null, frame: null };
      act(() => pulls[0].resolve(r));
      await settle();
      expect(container.querySelector("p")?.textContent ?? null, status).toBe(text);
      expect(landPaths(container).length, status).toBe(paths);
      unmount();
      pulls.length = 0;
      selects.length = 0;
    }
  });

  it("8. a theme change pulls nothing", async () => {
    await mounted();
    act(() => pulls[0].resolve(reply(1, 1)));
    await settle();
    // the pane never reads the appearance: a media change is nothing to it
    window.dispatchEvent(new Event("change"));
    await frames(3);
    expect(pulls.length).toBe(1);
  });
});
