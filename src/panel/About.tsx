// The About pane (decision 2, 2026-09-16): inside the popover, not the standard About panel,
// which an Accessory app opens behind the frontmost app. Name and version come from the bundle
// through Rust; the credits are static text.
import { useEffect, useState } from "react";
import { app } from "../api";
import styles from "./panel.module.css";

export default function About({ onBack }: { onBack: () => void }) {
  const [info, setInfo] = useState<{ name: string; version: string } | null>(null);

  useEffect(() => {
    let cancelled = false;
    app.info().then((i) => {
      if (!cancelled) setInfo(i);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <section aria-label="About" className={styles.stack}>
      <h1 className={styles.heading}>{info ? `${info.name} ${info.version}` : "Ondar"}</h1>
      <p className={styles.line}>A macOS menu bar radio player.</p>
      <p className={styles.muted}>
        Station data from radio-browser.info. Map imagery: NASA Blue Marble. Audio: rodio and
        Symphonia.
      </p>
      <div className={styles.row}>
        <button type="button" onClick={onBack}>
          Back
        </button>
      </div>
    </section>
  );
}
