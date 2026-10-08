// The map pane's pull rules and rendering (M4b commit 7 and the acceptance review) — vitest
// under jsdom, `pnpm test`. `../api` is mocked whole: every `pull` is a deferred promise the
// test resolves, so what the pane sends, and when, is observable. Each test's comment states
// what it pins and what it would have to see to fail.
import { readFileSync } from "node:fs";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Dot, Frame, MapBand, MapHit, MapInputs, MapReply, MapStatus } from "../api";
import MapPane, { DRAG_THRESHOLD_PT, pathOf } from "./MapPane";
import styles from "./panel.module.css";

type Deferred = { resolve: (r: MapReply | null) => void; inputs: MapInputs };
const pulls: Deferred[] = [];
const selects: string[] = [];
// Every `map.hit` point, and what the next one answers.
const hits: [number, number][] = [];
let hitReply: MapHit | null = null;
// Test 21 holds the hit's reply: the resolver of the last `map.hit`, when deferred.
let deferHits = false;
let heldHit: ((h: MapHit | null) => void) | null = null;
// `map:changed` listeners the pane registered.
const changed: (() => void)[] = [];

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
    hit: (pt: [number, number]) => {
      hits.push(pt);
      if (deferHits)
        return new Promise<MapHit | null>((resolve) => {
          heldHit = resolve;
        });
      return Promise.resolve(hitReply);
    },
  },
  onMapChanged: (cb: () => void) => {
    changed.push(cb);
    return Promise.resolve(() => {});
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
  dots: [],
  stats: {
    vertices: 3 * n,
    rings_considered: n,
    rings_skipped: 0,
    missing_blobs: 0,
    insets_dropped: 0,
    dots_hidden: 0,
    dots_outside: 0,
    stations_located: 0,
    stations_total: 0,
  },
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
  hits.length = 0;
  hitReply = null;
  changed.length = 0;
  deferHits = false;
  heldHit = null;
});
afterEach(cleanup);

const noop = () => {};

async function mounted(
  props: { playingUuid?: string | null; onHit?: (h: MapHit) => void; countryName?: string } = {},
) {
  const r = render(
    <MapPane
      band={band}
      country="FR"
      countryName={props.countryName ?? "France"}
      playingUuid={props.playingUuid ?? null}
      onHit={props.onHit ?? noop}
    />,
  );
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
  // ---- M4c k+4: the dots layer ----

  const dot = (x: number, y: number, n: number, uuids: string[], place = ""): Dot => ({
    x,
    y,
    r: Math.min(6, 2.5 + 0.6 * Math.log(n)),
    n,
    uuids,
    place,
  });
  const withDots = (seq: number, dots: Dot[], stats: Partial<Frame["stats"]> = {}): MapReply => {
    const r = reply(seq, 1);
    r.frame!.dots = dots;
    Object.assign(r.frame!.stats, stats);
    return r;
  };
  const dotCircles = (root: HTMLElement) =>
    Array.from(root.querySelectorAll(`svg g.${styles.dots} circle:not(.${styles.halo})`));
  const halos = (root: HTMLElement) => Array.from(root.querySelectorAll(`svg circle.${styles.halo}`));
  const three = [dot(40, 50, 64, ["a", "b", "c"], "Lisboa"), dot(200, 90, 3, ["d", "e", "f"]), dot(300, 1, 1, ["g"])];

  // 13. One `<circle>` per dot at the frame's centre and radius, in the frame's order (larger
  //     first, Rust's), drawn after the insets so a dot is never under a box's sea. Fails if a
  //     dot is dropped, re-sized or re-ordered by the page, or the layer moves under the insets.
  it("13. one circle per dot at the frame's x, y, r, in the frame's order, after the insets", async () => {
    const { container } = await mounted();
    act(() => pulls[0].resolve(withDots(1, three)));
    await settle();
    const c = dotCircles(container);
    expect(c.map((e) => [e.getAttribute("cx"), e.getAttribute("cy"), e.getAttribute("r")])).toEqual(
      three.map((d) => [String(d.x), String(d.y), String(d.r)]),
    );
    const groups = Array.from(container.querySelectorAll("svg > g"));
    const dotsIdx = groups.findIndex((g) => g.classList.contains(styles.dots));
    const insetsIdx = groups.findIndex((g) => g.classList.contains(styles.insets));
    expect(insetsIdx).toBeGreaterThanOrEqual(0);
    expect(dotsIdx).toBeGreaterThan(insetsIdx);
  });

  // 14. The playing dot (D4): a second circle at `r + 3` on the dot whose uuids hold the playing
  //     uuid, and on no other; none with nothing playing or the station not on the map. Fails if
  //     the halo is on every dot, on the wrong one, or at another radius.
  it("14. the playing dot's halo at r + 3, on that dot only", async () => {
    const { container, rerender } = await mounted({ playingUuid: "e" });
    act(() => pulls[0].resolve(withDots(1, three)));
    await settle();
    let h = halos(container);
    expect(h.length).toBe(1);
    expect([h[0].getAttribute("cx"), h[0].getAttribute("cy"), h[0].getAttribute("r")]).toEqual([
      "200",
      "90",
      String(three[1].r + 3),
    ]);
    rerender(<MapPane band={band} country="FR" countryName="France" playingUuid="zz" onHit={noop} />);
    expect(halos(container).length).toBe(0);
    rerender(<MapPane band={band} country="FR" countryName="France" playingUuid={null} onHit={noop} />);
    expect(halos(container).length).toBe(0);
    rerender(<MapPane band={band} country="FR" countryName="France" playingUuid="a" onHit={noop} />);
    h = halos(container);
    expect(h.length).toBe(1);
    expect(h[0].getAttribute("cx")).toBe("40");
  });

  // 15. A click is a press and a release under `DRAG_THRESHOLD_PT`: it calls `map.hit` once with
  //     the svg-local point; a drag past the threshold does not, and neither does a cancelled
  //     press. jsdom's `getBoundingClientRect` is stubbed at an offset, so the point must be
  //     the client point minus the svg's origin. Fails if the drag clicks, if the point is the
  //     client point, or if a cancel hits.
  it("15. a press and release under the threshold hits at the svg-local point; a drag does not", async () => {
    const { container } = await mounted();
    act(() => pulls[0].resolve(withDots(1, three)));
    await settle();
    const svg = container.querySelector("svg")!;
    svg.setPointerCapture = () => {};
    svg.getBoundingClientRect = () => ({ left: 16, top: 404, right: 344, bottom: 582, width: 328, height: 178, x: 16, y: 404, toJSON: () => ({}) });
    fireEvent.pointerDown(svg, { button: 0, buttons: 1, clientX: 56, clientY: 454, pointerId: 1 });
    fireEvent.pointerMove(svg, { buttons: 1, clientX: 58, clientY: 455, pointerId: 1 });
    fireEvent.pointerUp(svg, { clientX: 58, clientY: 455, pointerId: 1 });
    await settle();
    expect(hits).toEqual([[42, 51]]);
    fireEvent.pointerDown(svg, { button: 0, buttons: 1, clientX: 56, clientY: 454, pointerId: 1 });
    fireEvent.pointerMove(svg, { buttons: 1, clientX: 56 + DRAG_THRESHOLD_PT + 1, clientY: 454, pointerId: 1 });
    fireEvent.pointerUp(svg, { clientX: 56 + DRAG_THRESHOLD_PT + 1, clientY: 454, pointerId: 1 });
    await settle();
    fireEvent.pointerDown(svg, { button: 0, buttons: 1, clientX: 56, clientY: 454, pointerId: 1 });
    fireEvent.pointerCancel(svg, { clientX: 56, clientY: 454, pointerId: 1 });
    await settle();
    expect(hits).toEqual([[42, 51]]);
  });

  // 16. The hit's reply reaches `onHit` whole; a miss (`null`) calls nothing. Fails if the reply
  //     is dropped, or a miss clears or sets anything.
  it("16. a hit reaches onHit, a miss does nothing", async () => {
    const got: MapHit[] = [];
    const { container } = await mounted({ onHit: (h) => got.push(h) });
    act(() => pulls[0].resolve(withDots(1, three)));
    await settle();
    const svg = container.querySelector("svg")!;
    svg.setPointerCapture = () => {};
    const click = async (x: number, y: number) => {
      fireEvent.pointerDown(svg, { button: 0, buttons: 1, clientX: x, clientY: y, pointerId: 1 });
      fireEvent.pointerUp(svg, { clientX: x, clientY: y, pointerId: 1 });
      await settle();
    };
    hitReply = null;
    await click(5, 5);
    expect(got).toEqual([]);
    hitReply = { uuids: ["d", "e", "f"], n: 3, place: "" };
    await click(200, 90);
    expect(got).toEqual([{ uuids: ["d", "e", "f"], n: 3, place: "" }]);
  });

  // 17. MT's line (decision 5): "No station locations for {name}" on the platter when the
  //     country has stations and none located; not when it has none (a missing list, or the
  //     dots still on their way) and not when any is located. Fails on a condition that reads
  //     either count alone, or on the code (not the name) in the text.
  it("17. MT's line for stations with no locations, and only then", async () => {
    const cases: [number, number, string | null][] = [
      [13, 0, "No station locations for Malta"],
      [0, 0, null],
      [13, 2, null],
    ];
    for (const [total, located, text] of cases) {
      const { container, unmount } = await mounted({ countryName: "Malta" });
      act(() => pulls[0].resolve(withDots(1, [], { stations_total: total, stations_located: located })));
      await settle();
      expect(container.querySelector(`p.${styles.mapNote}`)?.textContent ?? null, `${total}/${located}`).toBe(text);
      unmount();
      pulls.length = 0;
      selects.length = 0;
    }
  });

  // 18. Decision 6: dots are not keyboard-reachable — no circle carries `tabIndex`, the layer is
  //     `aria-hidden`, the SVG stays `role="img"`. Fails if a dot is made focusable or exposed.
  it("18. dots carry no tabIndex; the layer is aria-hidden; the svg stays an img", async () => {
    const { container } = await mounted({ playingUuid: "a" });
    act(() => pulls[0].resolve(withDots(1, three)));
    await settle();
    expect(container.querySelectorAll("svg circle").length).toBe(4);
    expect(container.querySelector("svg circle[tabindex]")).toBeNull();
    expect(container.querySelector(`svg g.${styles.dots}`)!.getAttribute("aria-hidden")).toBe("true");
    expect(container.querySelector("svg")!.getAttribute("role")).toBe("img");
  });

  // 19. Hover (decision 7, S4 passed): entering a dot shows "n station(s) · place" beside it
  //     (the count alone when the place is empty, "station" for one); a dot in the pane's right
  //     half anchors the label to its left (`end`), one in the left half to its right, so the
  //     label stays inside the pane; leaving clears it; a new frame drops it (the dot may have
  //     moved). Fails on the wrong text, a label anchored outward, or one that outlives its dot.
  it("19. hover: the count and place beside the dot, inside the pane, cleared on leave", async () => {
    const { container } = await mounted();
    act(() => pulls[0].resolve(withDots(1, three)));
    await settle();
    const label = () => container.querySelector(`svg text.${styles.dotLabel}`);
    const c = dotCircles(container);
    fireEvent.pointerEnter(c[0]);
    expect(label()!.textContent).toBe("64 stations · Lisboa");
    expect(label()!.getAttribute("text-anchor")).toBe("start");
    expect(Number(label()!.getAttribute("x"))).toBeGreaterThan(three[0].x + three[0].r);
    fireEvent.pointerLeave(c[0]);
    expect(label()).toBeNull();
    fireEvent.pointerEnter(c[1]);
    expect(label()!.textContent).toBe("3 stations");
    expect(label()!.getAttribute("text-anchor")).toBe("end");
    expect(Number(label()!.getAttribute("x"))).toBeLessThan(three[1].x - three[1].r);
    fireEvent.pointerLeave(c[1]);
    fireEvent.pointerEnter(c[2]);
    expect(label()!.textContent).toBe("1 station");
    // at the top edge (y = 1): the baseline sits at least the label's 8 pt size down, so the
    // text's top is inside the pane
    expect(Number(label()!.getAttribute("y"))).toBeGreaterThanOrEqual(8);
    const svg = container.querySelector("svg")!;
    fireEvent.wheel(svg, { deltaX: 1, deltaY: 0 });
    await frames();
    act(() => pulls[1].resolve(withDots(2, three)));
    await settle();
    expect(label()).toBeNull();
  });

  // 20. `map:changed` (the dots installed, or a landed refresh regathered) wakes the pull loop:
  //     one pull with no input, so the frame comes at the view the pane has. Fails if the event
  //     is not listened to, or if it re-selects (which would reset the view to the fit).
  it("20. map:changed pulls once at the current view and does not re-select", async () => {
    await mounted();
    act(() => pulls[0].resolve(reply(1, 1)));
    await settle();
    await frames(2);
    expect(pulls.length).toBe(1);
    expect(changed.length).toBe(1);
    act(() => changed[0]());
    await frames();
    expect(pulls.length).toBe(2);
    expect(pulls[1].inputs).toEqual({ pan_pt: [0, 0], zoom_steps: 0, fit: false });
    expect(selects).toEqual(["FR"]);
  });
  // 21. A hit's reply that lands after a country change is the old country's dot: dropped, so
  //     the new country's list is not narrowed to stations it does not hold. Fails if the pane
  //     hands any reply to `onHit`.
  it("21. a hit reply landing after a country change is dropped", async () => {
    const got: MapHit[] = [];
    const onHit = (h: MapHit) => got.push(h);
    const { container, rerender } = await mounted({ onHit });
    act(() => pulls[0].resolve(withDots(1, three)));
    await settle();
    const svg = container.querySelector("svg")!;
    svg.setPointerCapture = () => {};
    deferHits = true;
    fireEvent.pointerDown(svg, { button: 0, buttons: 1, clientX: 40, clientY: 50, pointerId: 1 });
    fireEvent.pointerUp(svg, { clientX: 40, clientY: 50, pointerId: 1 });
    rerender(<MapPane band={band} country="ES" countryName="Spain" playingUuid={null} onHit={onHit} />);
    await settle();
    act(() => heldHit!({ uuids: ["a", "b", "c"], n: 3, place: "Lisboa" }));
    await settle();
    expect(got).toEqual([]);
    // the same click with no change lands
    fireEvent.pointerDown(svg, { button: 0, buttons: 1, clientX: 40, clientY: 50, pointerId: 1 });
    fireEvent.pointerUp(svg, { clientX: 40, clientY: 50, pointerId: 1 });
    act(() => heldHit!({ uuids: ["x"], n: 1, place: "" }));
    await settle();
    expect(got).toEqual([{ uuids: ["x"], n: 1, place: "" }]);
  });
});
