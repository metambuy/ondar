// The map pane (M4b commit 7): a renderer, not an application. Rust owns the country, the view,
// the frame and the band; this component accumulates the page's own inputs between animation
// frames (wheel and drag deltas, the `− fit +` clicks — ephemeral input state), pulls once per
// frame while anything is pending, and draws exactly the paths Rust sends. It computes no
// geometry: the platter's size and the controls' rect come from the layout, every path is in
// pane points already, and a reply is drawn only if its sequence number is newer than the frame
// on screen, so a late reply cannot overwrite a newer one. A theme change recolours by CSS alone
// and pulls nothing.
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { map } from "../api";
import type { Frame, MapBand, MapInputs, MapReply, MapStatus, Shape } from "../api";
import { measureMode, measureParam, report, sampleFrames } from "../measure";
import styles from "./panel.module.css";

/** The click/drag threshold in points (decision F, recorded for M4c's dots). */
export const DRAG_THRESHOLD_PT = 4;

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

export default function MapPane({ band, country }: Props) {
  const [reply, setReply] = useState<MapReply | null>(null);
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
  useEffect(() => {
    void map.select(country).then(() => loop.current?.wake());
  }, [country]);

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
  const onPointerUp = () => {
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

  return (
    <div className={styles.platter} data-measure="map_band" data-seq={reply?.seq ?? -1}>
      <svg
        ref={svgRef}
        className={styles.map}
        viewBox={`0 0 ${band.width} ${band.height}`}
        role="img"
        aria-label={`Map of ${country}`}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
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
          </>
        )}
      </svg>
      {message !== null && <p className={styles.mapMessage}>{message}</p>}
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
