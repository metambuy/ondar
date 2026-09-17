// The popover's root view. Holds the one piece of state shared across panes — which pane is
// showing, mirrored from Rust's `panel:view` — and reports Escape to Rust.
import { useEffect, useState } from "react";
import { onPanelView, panel } from "../api";
import type { PanelView } from "../api";
import About from "./About";
import styles from "./panel.module.css";
import Transport from "./Transport";

export default function Panel() {
  // Rust asserts the view on every show (About for the tray menu's About item, the transport
  // for everything else), so About never outlives a hide. Back is the one local transition: a
  // choice made inside an already-shown popover, re-asserted by Rust on the next show anyway.
  const [view, setView] = useState<PanelView>("transport");

  useEffect(() => {
    const unlisten = onPanelView(setView);
    // An emit before this listener existed was dropped by Tauri, so ask for the current pane.
    panel.getView().then(setView);
    return () => {
      unlisten.then((un) => un());
    };
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

  // The transport stays mounted while About is up (`hidden`, not unmounted): its stream info,
  // title, volume and selected preset are event-driven or local state with no Rust getter, and
  // unmounting it reset them on every return (`/code-review` finding 1, 2026-09-17).
  return (
    <main className={styles.panel}>
      <div hidden={view === "about"}>
        <Transport />
      </div>
      {view === "about" && <About onBack={() => setView("transport")} />}
    </main>
  );
}
