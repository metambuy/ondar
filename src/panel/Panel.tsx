// The popover's root view. Holds the only state shared across panes — at M2c that is nothing
// yet — renders the dev transport, and reports Escape to Rust.
import { useEffect } from "react";
import { panel } from "../api";
import styles from "./panel.module.css";
import Transport from "./Transport";

export default function Panel() {
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

  return (
    <main className={styles.panel}>
      <Transport />
    </main>
  );
}
