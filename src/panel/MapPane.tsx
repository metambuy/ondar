// The map pane (M4b commit 7): a renderer, not an application. Rust owns the country, the view,
// the frame and the band; this component accumulates the page's own inputs between animation
// frames (wheel and drag deltas, the `− fit +` clicks — ephemeral input state), pulls once per
// frame while anything is pending, and draws exactly the paths Rust sends. It computes no
// geometry: the platter's size and the controls' rect come from the layout, every path is in
// pane points already, and a reply is drawn only if its sequence number is newer than the frame
// on screen, so a late reply cannot overwrite a newer one. A theme change recolours by CSS alone
// and pulls nothing.
//
// M4c: the frame carries the country's station dots (Rust gathers, locates and projects them).
// The pane draws them, marks the one holding the playing station, labels the one under the
// pointer, and turns a click into `map.hit`, whose answer goes to `Panel` as the list's filter.
// The map never plays. Dots are not keyboard-reachable (decision 6): the list is the route.
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { map, onMapChanged } from "../api";
import type { Dot, Frame, MapBand, MapHit, MapInputs, MapReply, MapStatus, Shape } from "../api";
import { measureMode, measureParam, report, sampleFrames } from "../measure";
import { dotText } from "./dots";
import styles from "./panel.module.css";

/** The click/drag threshold in points (decision F, recorded for M4c's dots). */
export const DRAG_THRESHOLD_PT = 4;

/** The playing dot's halo: a ring this many points outside the dot (D4). */
const HALO_GAP_PT = 3;
/** The hover label's gap from the dot's edge, in points. */
export const LABEL_GAP_PT = 4;

/** A path's `d` for a shape: every ring closed, the fill rule even-odd (holes). */
export function pathOf(shape: Shape): string {
  let d = "";
  for (const ring of shape.rings) {
    for (let i = 0; i < ring.length; i++) {
      d += (i === 0 ? "M" : "L") + ring[i][0] + " " + ring[i][1];
    }
    d += "Z";
  }
  return d;
}

/** The line a status puts on the platter, or none. Exhaustive: a status Rust adds that this
 *  switch does not name fails typecheck at the `never` arm (M4b's review, latent 14). */
function statusMessage(status: MapStatus): string | null {
  switch (status) {
    case "frame":
      return null;
    case "no_map":
      return "No map for this country";
    case "unavailable":
      return "Map unavailable";
    case "no_band":
      // the band is Rust's: with none, the page draws nothing
      return null;
    default: {
      const unknown: never = status;
      return unknown;
    }
  }
}

function lineOf(line: [number, number][]): string {
  let d = "";
  for (let i = 0; i < line.length; i++) {
    d += (i === 0 ? "M" : "L") + line[i][0] + " " + line[i][1];
  }
  return d;
}

const none = (): MapInputs => ({ pan_pt: [0, 0], zoom_steps: 0, fit: false });

// The measurement harness (`?measure=map`, debug builds): `m=paint` cycles countries and reports
// each pull's round trip and commit; `m=pan` zooms in once, then feeds a wheel delta per frame.
const PAINT_COUNTRIES = ["RU", "US", "PT", "AQ"];
const PAINT_FIRST_MS = 6_000;
const PAINT_PERIOD_MS = 1_500;
const PAN_DURATION_MS = 3_000;

type Props = {
  band: MapBand;
  /** The selected country's code; a change returns the view to its fit (Rust's `map_select`). */
  country: string;
  /** Its name, from the countries list (`Panel`): the SVG's label and MT's line. */
  countryName: string;
  /** The station Now Playing names: the dot holding it gets the halo. */
  playingUuid: string | null;
  /** A click landed on a dot: its stations, for `Panel`'s list filter. A miss calls nothing. */
  onHit: (hit: MapHit) => void;
};

/// The pull loop (the page's half of decision D): input accumulated since the last pull, one pull
/// in flight, a `requestAnimationFrame` only while something is pending — no timer, no polling.
/// Not render state: a render per wheel event would be the wrong cadence, so it lives outside
/// React's render and is created once per mount.
type Loop = {
  push: (f: (i: MapInputs) => void) => void;
  wake: () => void;
  shownSeq: () => number;
  stop: () => void;
};

function makeLoop(
  onReply: (r: MapReply, invokeMs: number, parse: { ms: number; bytes: number } | null) => void,
): Loop {
  // The acceptance review's A3: under `m=paint&decomp=1` the reply's parse cost is measured by
  // proxy — `JSON.parse(JSON.stringify(reply))`, the same object re-parsed — once per drawn reply.
  // The copy sits in the heap through React's render and inflates `commit_ms` (26 ms for RU where
  // the clean figure is 7), so the decomposition and the first-paint rows are separate runs.
  const parseProxy = measureMode() === "map" && measureParam("m") === "paint" && measureParam("decomp") === "1";
  let pending = none();
  let dirty = false;
  let inFlight = false;
  let scheduled = false;
  let shownSeq = -1;
  let alive = true;
  const schedule = () => {
    if (scheduled || !alive) return;
    scheduled = true;
    requestAnimationFrame(pull);
  };
  const pull = () => {
    scheduled = false;
    if (!alive) return;
    if (inFlight) {
      schedule();
      return;
    }
    const inputs = pending;
    pending = none();
    dirty = false;
    inFlight = true;
    const sentAt = performance.now();
    void map.pull(inputs).then(
      (r) => {
        inFlight = false;
        if (!alive) return;
        // a reply is drawn only if newer than the frame on screen
        if (r !== null && r.seq > shownSeq) {
          shownSeq = r.seq;
          const invokeMs = performance.now() - sentAt;
          let parse: { ms: number; bytes: number } | null = null;
          if (parseProxy) {
            const text = JSON.stringify(r);
            const t = performance.now();
            JSON.parse(text);
            parse = { ms: performance.now() - t, bytes: text.length };
          }
          onReply(r, invokeMs, parse);
        }
        if (dirty) schedule();
      },
      () => {
        inFlight = false;
        if (dirty) schedule();
      },
    );
  };
  return {
    push: (f) => {
      f(pending);
      dirty = true;
      schedule();
    },
    wake: () => {
      dirty = true;
      schedule();
    },
    shownSeq: () => shownSeq,
    stop: () => {
      alive = false;
    },
  };
}

export default function MapPane({ band, country, countryName, playingUuid, onHit }: Props) {
  const [reply, setReply] = useState<MapReply | null>(null);
  // The dot under the pointer, by index in the frame it was entered on: a newer frame drops it
  // (the dot may have moved or gone). View state.
  const [hover, setHover] = useState<{ seq: number; i: number } | null>(null);
  const svgRef = useRef<SVGSVGElement>(null);
  // the harness's marks: the drawn reply's invoke round trip and when it arrived
  const invokeMs = useRef<{
    seq: number;
    ms: number;
    at: number;
    parse: { ms: number; bytes: number } | null;
  } | null>(null);
  const loop = useRef<Loop | null>(null);
  const push = useCallback((f: (i: MapInputs) => void) => loop.current?.push(f), []);

  useEffect(() => {
    const l = makeLoop((r, ms, parse) => {
      invokeMs.current = { seq: r.seq, ms, at: performance.now(), parse };
      setReply(r);
    });
    loop.current = l;
    return () => {
      l.stop();
      loop.current = null;
    };
  }, []);

  // A country change (and the mount: a collapse and re-expand) returns to the fit — Rust's
  // `select` — and pulls.
  const countryRef = useRef(country);
  useEffect(() => {
    countryRef.current = country;
    void map.select(country).then(() => loop.current?.wake());
  }, [country]);

  // `map:changed`: Rust installed the selection's dots (after its first frame) or regathered
  // them on a landed refresh. Pull once, at the view the pane has; never re-select (that would
  // return to the fit).
  useEffect(() => {
    const unlisten = onMapChanged(() => loop.current?.wake());
    return () => {
      unlisten.then((un) => un());
    };
  }, []);

  // A band change is Rust's to notice (the session compares the layout's band); the page only
  // needs to pull once so the fit at the new band arrives.
  useEffect(() => {
    loop.current?.wake();
  }, [band.width, band.height]);

  // The platter's size and the controls' rect, from the layout: CSS variables, not literals.
  useLayoutEffect(() => {
    const root = document.documentElement.style;
    root.setProperty("--map-band-width", `${band.width}px`);
    root.setProperty("--map-band-height", `${band.height}px`);
    const [cx, cy, cw, ch] = band.controls;
    root.setProperty("--map-controls-x", `${cx}px`);
    root.setProperty("--map-controls-y", `${cy}px`);
    root.setProperty("--map-controls-w", `${cw}px`);
    root.setProperty("--map-controls-h", `${ch}px`);
  }, [band]);

  // The wheel: non-passive, so the band's scroll never scrolls anything else; deltas in CSS px
  // are points. Two-finger scroll pans with the gesture (the content follows the fingers, as the
  // Mac's Maps does); a mouse wheel pans one axis as it arrives.
  useEffect(() => {
    const el = svgRef.current;
    if (el === null) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      push((i) => {
        i.pan_pt[0] += e.deltaX;
        i.pan_pt[1] += e.deltaY;
      });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [push]);

  // A drag pans: the pointer is captured on press, deltas accumulate once it has moved
  // `DRAG_THRESHOLD_PT` from the press (a shorter move is a click — M4c's dots), negated because
  // dragging the map right moves the view's centre left.
  const drag = useRef<{ x: number; y: number; lastX: number; lastY: number; panning: boolean } | null>(null);
  const onPointerDown = (e: React.PointerEvent<SVGSVGElement>) => {
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { x: e.clientX, y: e.clientY, lastX: e.clientX, lastY: e.clientY, panning: false };
  };
  const onPointerMove = (e: React.PointerEvent<SVGSVGElement>) => {
    const d = drag.current;
    if (d === null || (e.buttons & 1) === 0) return;
    if (!d.panning && Math.hypot(e.clientX - d.x, e.clientY - d.y) >= DRAG_THRESHOLD_PT) {
      d.panning = true;
      d.lastX = d.x;
      d.lastY = d.y;
    }
    if (!d.panning) return;
    const dx = e.clientX - d.lastX;
    const dy = e.clientY - d.lastY;
    d.lastX = e.clientX;
    d.lastY = e.clientY;
    push((i) => {
      i.pan_pt[0] -= dx;
      i.pan_pt[1] -= dy;
    });
  };
  // A release with no pan is a click (M4c, decision 3): Rust tests it against the dots on screen
  // (`map.hit`, svg-local points — the viewBox is the band's size, so CSS px are pane points) and
  // the page filters the list to the answer. A reply that lands after a country change is the old
  // country's and is dropped.
  const onPointerUp = (e: React.PointerEvent<SVGSVGElement>) => {
    const d = drag.current;
    drag.current = null;
    if (d === null || d.panning) return;
    const box = e.currentTarget.getBoundingClientRect();
    const at = countryRef.current;
    void map.hit([e.clientX - box.left, e.clientY - box.top]).then((hit) => {
      if (hit !== null && countryRef.current === at) onHit(hit);
    });
  };
  const onPointerCancel = () => {
    drag.current = null;
  };

  const zoom = useCallback(
    (steps: number) =>
      push((i) => {
        i.zoom_steps += steps;
      }),
    [push],
  );
  const fit = useCallback(
    () =>
      push((i) => {
        i.fit = true;
      }),
    [push],
  );

  // The harness's marks per drawn reply: invoke → commit (this effect runs after the render that
  // used the reply committed), then the first two animation frames.
  const seq = reply?.seq;
  useEffect(() => {
    if (measureMode() !== "map" || seq === undefined) return;
    const c = invokeMs.current;
    if (c === null || c.seq !== seq) return;
    const commitAt = performance.now();
    const vertices = reply?.frame?.stats.vertices ?? 0;
    const status = reply?.status;
    requestAnimationFrame((t1) => {
      requestAnimationFrame((t2) => {
        report("frame", {
          seq,
          invoke_ms: c.ms,
          parse_ms: c.parse?.ms,
          wire_proxy: c.parse?.bytes,
          commit_ms: commitAt - c.at,
          raf1_ms: t1 - commitAt,
          raf2_ms: t2 - t1,
          vertices,
          status,
        });
      });
    });
  }, [seq, reply]);

  // The harness's drivers (`?measure=map&m=paint|pan`).
  useEffect(() => {
    if (measureMode() !== "map") return;
    const m = measureParam("m");
    if (m === "paint") {
      const n = Number(measureParam("n") ?? "20");
      // `period=` (ms between selects, default 1 500 — a country change from idle; Step 0's probe
      // ran its frames back to back, so `period=100` reproduces its hot core — A3's decomposition)
      const period = Number(measureParam("period") ?? PAINT_PERIOD_MS);
      const codes = (measureParam("cc") ?? PAINT_COUNTRIES.join(",")).split(",");
      const timers: ReturnType<typeof setTimeout>[] = [];
      for (let k = 0; k < n * codes.length; k++) {
        timers.push(
          setTimeout(() => {
            report("paint_select", { k, code: codes[k % codes.length] });
            void map.select(codes[k % codes.length]).then(() => loop.current?.wake());
          }, PAINT_FIRST_MS + k * period),
        );
      }
      return () => timers.forEach(clearTimeout);
    }
    if (m === "pan") {
      // `cc=` selects the country first (the heavy case is RU); `steps=` zooms (default one `+`);
      // then input arrives as a trackpad's would — a wheel-sized push every 8 ms from a timer,
      // between animation frames — while `sampleFrames` reads the rAF cadence and the frames drawn.
      const cc = measureParam("cc");
      let feed: ReturnType<typeof setInterval> | undefined;
      const go = () => {
        zoom(Number(measureParam("steps") ?? "1"));
        setTimeout(() => {
          const seen = new Set<number>();
          const t0 = performance.now();
          // 0.6 pt every 8 ms = 75 pt/s: 225 pt over the 3 s, inside D6's 328 pt of room for RU at
          // one `+`, so the view moves for the whole window (at 2 pt it reached the edge at 1.8 s)
          feed = setInterval(() => {
            push((i) => {
              i.pan_pt[0] += 0.6;
              i.pan_pt[1] += 0.3;
            });
          }, 8);
          sampleFrames("pan", PAN_DURATION_MS, () => {
            seen.add(loop.current?.shownSeq() ?? -1);
          });
          setTimeout(() => {
            clearInterval(feed);
            const elapsed = performance.now() - t0;
            report("pan_cycles", {
              frames_drawn: seen.size,
              elapsed_ms: elapsed,
              cycles_per_s: (seen.size * 1000) / elapsed,
            });
          }, PAN_DURATION_MS + 200);
        }, 500);
      };
      const t = setTimeout(() => {
        if (cc !== null) {
          void map.select(cc).then(() => {
            loop.current?.wake();
            setTimeout(go, 1_000);
          });
        } else {
          go();
        }
      }, PAINT_FIRST_MS);
      return () => {
        clearTimeout(t);
        clearInterval(feed);
      };
    }
    return undefined;
  }, [push, zoom]);

  const frame: Frame | null = reply?.status === "frame" ? (reply.frame ?? null) : null;
  const message = reply ? statusMessage(reply.status) : null;
  // MT (decision 5): stations, none of them located.
  const noLocations = frame !== null && frame.stats.stations_total > 0 && frame.stats.stations_located === 0;
  const hovered: Dot | null =
    frame !== null && hover !== null && hover.seq === reply?.seq ? (frame.dots[hover.i] ?? null) : null;
  // The hover label (decision 7, amended at k+4b: the inset-label style read 1.13 to 3.91 against
  // the map, under WCAG's 4.5) is HTML on the controls' plate over the SVG. Beside the dot on the
  // side toward the pane's centre, `LABEL_GAP_PT` from its edge, its `max-width` ending at the
  // pane's edge (a long place ends in an ellipsis); centred on the dot vertically, by the label's
  // own box (line height plus padding), and clamped into the pane. Placement only, set on the
  // element before paint: the words are Rust's count and place.
  const tipRef = useRef<HTMLParagraphElement>(null);
  useLayoutEffect(() => {
    const el = tipRef.current;
    if (el === null || hovered === null) return;
    const gap = hovered.r + LABEL_GAP_PT;
    if (hovered.x <= band.width / 2) {
      const left = hovered.x + gap;
      el.style.left = `${left}px`;
      el.style.right = "";
      el.style.maxWidth = `${Math.max(0, band.width - left)}px`;
    } else {
      const edge = hovered.x - gap;
      el.style.left = "";
      el.style.right = `${band.width - edge}px`;
      el.style.maxWidth = `${Math.max(0, edge)}px`;
    }
    const h = el.offsetHeight;
    el.style.top = `${Math.min(Math.max(hovered.y - h / 2, 0), band.height - h)}px`;
  }, [hovered, band]);

  return (
    <div className={styles.platter} data-measure="map_band" data-seq={reply?.seq ?? -1}>
      <svg
        ref={svgRef}
        className={styles.map}
        viewBox={`0 0 ${band.width} ${band.height}`}
        role="img"
        aria-label={`Map of ${countryName}`}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerCancel}
      >
        {/* Three layers, one flat fill each (A1, 2026-10-06): neighbours, the land with its
            hairline edge on the same path, subdivisions above it; then the insets. No filter —
            Ink's inland tone through an erode/blur filter cost ~90 ms a paint at 300 and was
            removed at the acceptance review. No `<use>` (round 3, C1 + C2): WebKit styles a
            `<use>` clone as the original element, so commit 7's edge group — a `<use>` per land
            path — painted the land fill again over the subdivisions, with `stroke: none`. */}
        {frame !== null && (
          <>
            <g className={styles.neighbours}>
              {frame.neighbours.map((s, i) => (
                <path key={i} d={pathOf(s)} fillRule="evenodd" />
              ))}
            </g>
            <g className={styles.land}>
              {frame.land.map((s, i) => (
                <path key={i} id={`map-land-${i}`} d={pathOf(s)} fillRule="evenodd" />
              ))}
            </g>
            <g className={styles.subdivisions}>
              {frame.subdivisions.map((l, i) => (
                <path key={i} d={lineOf(l)} />
              ))}
            </g>
            <g className={styles.insets}>
              {frame.insets.map((ins) => {
                const [x, y, w, h] = ins.rect;
                return (
                  <g key={ins.label}>
                    <rect className={styles.insetBox} x={x} y={y} width={w} height={h} />
                    <g className={styles.insetLand}>
                      {ins.land.map((s, i) => (
                        <path key={i} d={pathOf(s)} fillRule="evenodd" />
                      ))}
                    </g>
                    {/* the label, whole — the tool refuses a label wider than its box (P3) */}
                    <text className={styles.insetLabel} x={x + w - 4} y={y + h - 4} textAnchor="end">
                      {ins.label}
                    </text>
                  </g>
                );
              })}
            </g>
            {/* The dots (M4c), above the insets: Rust's centres and radii, larger first, so a
                small dot is never under a large one. No text on the map but the hover label.
                Not focusable and hidden from assistive tech (decision 6: the list reaches every
                station); the SVG stays one image. */}
            <g className={styles.dots} aria-hidden="true">
              {frame.dots.map((d, i) => (
                <g key={i}>
                  <circle
                    cx={d.x}
                    cy={d.y}
                    r={d.r}
                    onPointerEnter={() => setHover({ seq: reply?.seq ?? -1, i })}
                    onPointerLeave={() => setHover(null)}
                  />
                  {playingUuid !== null && d.uuids.includes(playingUuid) && (
                    <circle className={styles.halo} cx={d.x} cy={d.y} r={d.r + HALO_GAP_PT} />
                  )}
                </g>
              ))}
            </g>
          </>
        )}
      </svg>
      {message !== null && <p className={styles.mapMessage}>{message}</p>}
      {noLocations && <p className={`${styles.mapPlate} ${styles.mapNote}`}>No station locations for {countryName}</p>}
      {hovered !== null && (
        <p ref={tipRef} className={`${styles.mapPlate} ${styles.mapTip}`} aria-hidden="true">
          {dotText(hovered.n, hovered.place)}
        </p>
      )}
      {/* C1: the `− fit +` row in the band's reserved bottom-right corner, native buttons placed
          at the rect Rust reserved (CSS variables from the layout), keyboard-reachable. */}
      <div className={styles.controls} role="group" aria-label="Zoom">
        <button type="button" aria-label="Zoom out" onClick={() => zoom(-1)}>
          −
        </button>
        <button type="button" aria-label="Fit the country" onClick={fit}>
          fit
        </button>
        <button type="button" aria-label="Zoom in" onClick={() => zoom(1)}>
          +
        </button>
      </div>
    </div>
  );
}
