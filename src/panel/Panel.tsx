// The popover's root view. Mirrors Rust's `panel:layout` — which pane is showing, which height
// state the popover is in, how tall it is in points, and whether it may expand — hosts the expand
// control and the placeholder for the expanded pane, and reports Escape to Rust. It decides none
// of it: the height comes from Rust (decision D1 — "expanded" is a function of the display), and
// a click on the control is a report, answered by the next `panel:layout`.
import { useEffect, useState } from "react";
import { onPanelLayout, panel } from "../api";
import type { PanelLayout, PanelView } from "../api";
import About from "./About";
import styles from "./panel.module.css";
import Transport from "./Transport";

export default function Panel() {
  // What Rust last laid out. `null` until the getter answers; the control is disabled meanwhile.
  const [layout, setLayout] = useState<PanelLayout | null>(null);
  // Rust asserts the view on every show (About for the tray menu's About item, the transport
  // for everything else), so About never outlives a hide. Back is the one local transition: a
  // choice made inside an already-shown popover, re-asserted by Rust on the next show anyway.
  const [view, setView] = useState<PanelView>("transport");
  // The window's own height, as the webview sees it — view state, read on `resize`.
  const [windowHeight, setWindowHeight] = useState(window.innerHeight);

  useEffect(() => {
    const unlisten = onPanelLayout((l) => {
      setLayout(l);
      setView(l.view);
    });
    // An emit before this listener existed was dropped by Tauri, so ask for the current layout.
    panel.getLayout().then((l) => {
      setLayout(l);
      setView(l.view);
    });
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
  useEffect(() => {
    document.documentElement.style.setProperty("--panel-height", `${rootHeight}px`);
  }, [rootHeight]);

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
  // unmounting it reset them on every return (`/code-review` finding 1, 2026-09-17).
  return (
    <main className={styles.panel}>
      <div hidden={view === "about"}>
        <Transport />
      </div>
      {view === "about" && <About onBack={() => setView("transport")} />}

      {/* Decision D4: when expansion is refused (D1's floor) the control stays, disabled, so the
          chrome is the same on every display. Rust refuses regardless of this attribute. */}
      <div className={styles.row}>
        <button
          type="button"
          aria-expanded={expanded}
          disabled={layout === null || !layout.expandable}
          onClick={() => void panel.setExpanded(!expanded)}
        >
          {expanded ? "Collapse" : "Expand"}
        </button>
      </div>

      {showExpandedPane && (
        <section aria-label="Expanded pane" className={styles.section}>
          <p className={styles.muted}>Expanded pane. The map (M4) and the equalizer (M5) go here.</p>
        </section>
      )}
    </main>
  );
}
