// The popover's root view. Mirrors Rust's `panel:layout` — which pane is showing, which height
// state the popover is in, how tall it is in points, and whether it may expand — hosts the expand
// control and the placeholder for the expanded pane, and reports Escape to Rust. It decides none
// of it: the height comes from Rust (decision D1 — "expanded" is a function of the display), and
// a click on the control is a report, answered by the next `panel:layout`.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { onPanelLayout, panel } from "../api";
import type { PanelLayout, PanelView, Station } from "../api";
import { measureMode, measureParam, report, reportBlocks } from "../measure";
import About from "./About";
import CountryControl from "./CountryControl";
import NowPlaying from "./NowPlaying";
import styles from "./panel.module.css";
import StationList from "./StationList";
import Transport from "./Transport";

// The measurement harness's mount cycle (`?measure=perf&m=mount`): the list mounted fresh
// every 3 s from +10 s, five times, so `StationList` reports its four marks per mount.
const MOUNT_REPS = 5;
const MOUNT_FIRST_MS = 10_000;
const MOUNT_PERIOD_MS = 3_000;
const MOUNT_UP_MS = 2_000;

export default function Panel() {
  // What Rust last laid out. `null` until the getter answers; the control is disabled meanwhile.
  const [layout, setLayout] = useState<PanelLayout | null>(null);
  // Rust asserts the view on every show (About for the tray menu's About item, the transport
  // for everything else), so About never outlives a hide. Back is the one local transition: a
  // choice made inside an already-shown popover, re-asserted by Rust on the next show anyway —
  // and reported to Rust, because every later layout event carries the pane too
  // (`/code-review` C1, 2026-09-18: without the report, Expand after Back re-asserted About).
  const [view, setView] = useState<PanelView>("transport");
  // The window's own height, as the webview sees it — view state, read on `resize`.
  const [windowHeight, setWindowHeight] = useState(window.innerHeight);
  // The newest generation applied, so an older layout arriving late is ignored (below).
  const newestGeneration = useRef(-1);
  // The selected country: shared by the country control, the station list and (M4) the map,
  // so it lives here, not in either. Not persisted yet — a launch starts on PT (the dev list's
  // default, kept until Rust remembers the choice). The measurement harness may name one.
  const [selected, setSelected] = useState<string>(measureParam("cc") ?? "PT");
  // Counts effective shows. The lists re-request on every show (an expired list is refreshed
  // by the service only when asked for again — M3a acceptance item 6, carried to M3b). The
  // getter's answer on mount is generation 0 and is not a show, so the mount request is the
  // effects' own first run, not a bump.
  const [showGeneration, setShowGeneration] = useState(0);
  // The station the page last asked to play — what Now Playing names. View state: whether
  // anything is audible is Rust's (`playback:state`), and a preset play clears this.
  const [playing, setPlaying] = useState<Station | null>(null);
  // The harness (perf mode): which mount repetition is up (0 = the list is mounted normally;
  // in `m=mount` it starts unmounted and cycles), and when the last layout event arrived.
  const [mountRep, setMountRep] = useState(0);
  const [listMounted, setListMounted] = useState(!(measureMode() === "perf" && measureParam("m") === "mount"));
  const layoutAt = useRef<{ generation: number; at: number } | null>(null);

  useEffect(() => {
    // One entry point for both channels. The getter's answer and the event are separate IPC
    // channels with no ordering between them, so a getter answered before a request but delivered
    // after that request's event would roll the page back to the older layout, and its commit
    // report would be a no-op while the newer generation waited for the fallback
    // (`/code-review` C4). Generations only grow, so a layout older than the one held is ignored.
    const apply = (l: PanelLayout) => {
      if (l.generation < newestGeneration.current) return;
      newestGeneration.current = l.generation;
      layoutAt.current = { generation: l.generation, at: performance.now() };
      setLayout(l);
      setView(l.view);
      // On a show the hidden panel's frame is already at `l.height`, and a hidden WKWebView fires
      // no `resize`, so the last reading is whatever the window was when it was last visible —
      // taller, after a show on a shorter display or after a cancelled collapse. Take Rust's word
      // for it (`/code-review` C2). On a resize the window has not changed yet; leave it.
      if (l.transition === "show") {
        setWindowHeight(l.height);
        if (l.generation > 0) setShowGeneration((g) => g + 1);
      }
    };
    const unlisten = onPanelLayout(apply);
    // An emit before this listener existed was dropped by Tauri, so ask for the current layout.
    panel.getLayout().then(apply);
    return () => {
      unlisten.then((un) => un());
    };
  }, []);

  useEffect(() => {
    const onResize = () => setWindowHeight(window.innerHeight);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // The root's height is the larger of the height Rust laid out and the window's own — not
  // `100%`, and not the event alone. On an expand the root grows to the target before the window
  // does, so the expanded pane is laid out when the new band appears; on a collapse it stays at
  // the window's height until the window has shrunk, so no band of bare material opens inside a
  // still-tall window. The value is Rust's, so it is not a style literal (`check-tokens.sh`).
  const rootHeight = Math.max(layout?.height ?? 0, windowHeight);
  // A layout effect, so the variable is in place before anything can paint this render.
  useLayoutEffect(() => {
    document.documentElement.style.setProperty("--panel-height", `${rootHeight}px`);
  }, [rootHeight]);

  // The round trip's report (decision D3): once the render that used this layout is committed,
  // tell Rust, which orders a pending show in or changes the frame then. An effect keyed on the
  // generation, not `requestAnimationFrame`: a hidden WKWebView runs no rendering updates, so an
  // rAF report would never arrive for a show. Rust ignores a generation that is no longer pending.
  // Generation 0 is the getter's answer before any show — never pending, so not reported
  // (`/code-review` C7: it logged an `effective=false` line at every mount).
  const generation = layout?.generation;
  useEffect(() => {
    if (generation !== undefined && generation > 0) void panel.layoutCommitted(generation);
    // The measurement harness (debug builds under `?measure=…` only), `src/measure.ts`: under
    // `fit`, every block's box at this commit; under `perf`, how long this commit took from the
    // layout event's arrival — the page's share of the show's `after_ms`.
    if (measureMode() === "fit") reportBlocks(document, { trigger: "layout", generation });
    if (measureMode() === "perf" && generation !== undefined && generation > 0) {
      const at = layoutAt.current;
      report("layout_commit", {
        generation,
        commit_ms: at !== null && at.generation === generation ? performance.now() - at.at : -1,
      });
    }
  }, [generation]);

  // The harness's mount cycle: five fresh mounts, two seconds up and one down each.
  useEffect(() => {
    if (measureMode() !== "perf" || measureParam("m") !== "mount") return;
    const timers: ReturnType<typeof setTimeout>[] = [];
    for (let rep = 1; rep <= MOUNT_REPS; rep++) {
      const up = MOUNT_FIRST_MS + (rep - 1) * MOUNT_PERIOD_MS - performance.now();
      timers.push(
        setTimeout(() => {
          setMountRep(rep);
          setListMounted(true);
        }, up),
        setTimeout(() => setListMounted(false), up + MOUNT_UP_MS),
      );
    }
    return () => timers.forEach(clearTimeout);
  }, []);

  useEffect(() => {
    // Escape → Rust, which hides the popover (`reason=esc`). `preventDefault()` because the key
    // is handled: left to its default it continues as `cancelOperation:` up the responder
    // chain, which is what beeps (M2c Step 0, item 1: the beep was audible, and louder after an
    // in-panel click). Whether this silences it is checked by ear at acceptance, both phases.
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        void panel.escape();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const expanded = layout?.state === "expanded";
  // The expanded pane is shown while there is room for it: Rust says expanded, or the window is
  // still taller than the collapsed target (mid-collapse, before the frame has shrunk).
  const showExpandedPane = layout !== null && (expanded || windowHeight > layout.height);

  // The transport stays mounted while About is up (`hidden`, not unmounted): its stream info,
  // title, volume and selected preset are event-driven or local state with no Rust getter, and
  // unmounting it reset them on every return (`/code-review` finding 1, 2026-09-17). The lists
  // stay mounted for the same reason.
  return (
    <main className={styles.panel} data-measure="panel">
      <div className={styles.body} hidden={view === "about"}>
        <NowPlaying station={playing} />
        <Transport onPlayPreset={() => setPlaying(null)} />
        <CountryControl selected={selected} onSelect={setSelected} showGeneration={showGeneration} />
        {listMounted && (
          <StationList
            key={mountRep}
            selected={selected}
            showGeneration={showGeneration}
            onPlay={setPlaying}
            measureRep={mountRep}
          />
        )}
      </div>
      {view === "about" && (
        <About
          onBack={() => {
            setView("transport");
            void panel.viewBack();
          }}
        />
      )}

      {/* The control belongs to the transport, not to About (decided 2026-09-21; Rust refuses a
          resize from About regardless, `reason=view`). Decision D4: when expansion is refused
          (D1's floor) it stays, disabled, so the chrome is the same on every display. */}
      {view === "transport" && (
        <div className={styles.row} data-measure="expand_row">
          <button
            type="button"
            aria-expanded={expanded}
            disabled={layout === null || !layout.expandable}
            onClick={() => void panel.setExpanded(!expanded)}
          >
            {expanded ? "Collapse" : "Expand"}
          </button>
        </div>
      )}

      {showExpandedPane && (
        <section aria-label="Expanded pane" className={styles.section}>
          <p className={styles.muted}>Expanded pane. The map (M4) and the equalizer (M5) go here.</p>
        </section>
      )}
    </main>
  );
}
