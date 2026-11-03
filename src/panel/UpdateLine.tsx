// The update line (M6-lite commit 4b; 0.1.2, F2). Rust checked the feed once after launch; if it
// found a newer version it said so with `update:available`, and this renders one small button in
// the system accent: "Update to 0.1.3" (until 0.1.2, grey text "Ondar 0.1.1 available · Update",
// which did not stand out: F2). A click is a report (`updater.install`): Rust downloads, verifies,
// swaps the bundle and restarts, so a success never answers; a rejection is shown as "Update
// failed, see log" beside the button, which stays for a retry. Nothing here checks, compares or persists anything:
// the version shown is the event's, and the three phases are view state.
import { useEffect, useRef, useState } from "react";
import { onUpdateAvailable, updater } from "../api";
import type { UpdateAvailable } from "../api";
import styles from "./panel.module.css";

type Phase = "available" | "installing" | "failed";

export default function UpdateLine() {
  const [update, setUpdate] = useState<UpdateAvailable | null>(null);
  const [phase, setPhase] = useState<Phase>("available");
  // A rejection that lands after an unmount must not set state on a gone component.
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    const unlisten = onUpdateAvailable((u) => {
      setUpdate(u);
      setPhase("available");
    });
    return () => {
      mounted.current = false;
      unlisten.then((un) => un());
    };
  }, []);

  if (update === null) return null;

  const install = () => {
    setPhase("installing");
    updater.install().then(
      // A success restarts the app; there is nothing to render.
      () => {},
      () => {
        if (mounted.current) setPhase("failed");
      },
    );
  };

  return (
    <p className={styles.updateLine} data-measure="update_line" aria-live="polite">
      <button
        type="button"
        className={styles.updateButton}
        disabled={phase === "installing"}
        onClick={install}
      >
        {phase === "installing" ? "Updating…" : `Update to ${update.version}`}
      </button>
      {phase === "failed" && <span>Update failed, see log</span>}
    </p>
  );
}
