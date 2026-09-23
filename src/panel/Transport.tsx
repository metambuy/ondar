// Dev transport (M2c, decision 1): the M1 bench folded into the popover so the audio path and
// the tray's playing glyph stay exercisable by hand. Visibly a placeholder: the presets retire
// with M3b's later commits, the buttons and the volume become the product's transport row. Its
// Now Playing lines moved to `NowPlaying.tsx` (M3b 1c). Renders state and sends commands; holds
// no logic.
//
// Not carried over from the bench, deliberately: the custom-URL field, the on-page event log
// (Rust's log has every event) and the EQ sliders (EQ UI is M5; the engine claim is held by
// `eq::tests` and `eq_headroom_sweep`).
import { useEffect, useState } from "react";
import { audio, onState } from "../api";
import type { PlaybackState } from "../api";
import styles from "./panel.module.css";

// Known-good public streams. URLs rot; swap freely.
const PRESETS: { name: string; url: string }[] = [
  { name: "Radio Swiss Jazz (MP3 128k)", url: "https://stream.srg-ssr.ch/m/rsj/mp3_128" },
  { name: "FIP (AAC)", url: "https://icecast.radiofrance.fr/fip-hifi.aac" },
  { name: "SomaFM Groove Salad (MP3)", url: "https://ice1.somafm.com/groovesalad-128-mp3" },
];

export default function Transport({ onPlayPreset }: { onPlayPreset: () => void }) {
  const [url, setUrl] = useState(PRESETS[0].url);
  const [state, setState] = useState<PlaybackState>({ kind: "idle" });
  const [volume, setVolume] = useState(1);
  const [lastError, setLastError] = useState<string | null>(null);

  useEffect(() => {
    const unlisten = onState(setState);
    audio.getPlaybackState().then(setState);
    return () => {
      unlisten.then((un) => un());
    };
  }, []);

  // A command's rejection is argument validation only (CLAUDE.md, IPC contract); playback
  // outcomes arrive as `playback:state` events.
  const report = (what: string) => (e: unknown) => setLastError(`${what}: ${JSON.stringify(e)}`);
  const play = () => {
    // A preset is not a station record: Now Playing falls back to the server's `icy-name`.
    onPlayPreset();
    return audio.play(url, "manual").catch(report("play"));
  };

  return (
    <section aria-label="Dev transport" className={styles.section} data-measure="transport_controls">
      {lastError && (
        <p className={`${styles.muted} ${styles.clamp}`} role="alert">
          {lastError}
        </p>
      )}

        <label className={styles.field}>
          Preset
          <select value={url} onChange={(e) => setUrl(e.target.value)}>
            {PRESETS.map((p) => (
              <option key={p.url} value={p.url}>
                {p.name}
              </option>
            ))}
          </select>
        </label>

        <div className={styles.row}>
          <button type="button" onClick={play}>
            Play
          </button>
          {state.kind === "paused" ? (
            <button type="button" onClick={() => audio.resume()}>
              Resume
            </button>
          ) : (
            <button type="button" onClick={() => audio.pause()} disabled={state.kind !== "playing"}>
              Pause
            </button>
          )}
          <button type="button" onClick={() => audio.stop()} disabled={state.kind === "idle"}>
            Stop
          </button>
        </div>

        <label className={styles.field}>
          Volume
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={volume}
            aria-label="Volume"
            onChange={(e) => {
              const v = Number(e.target.value);
              setVolume(v);
              audio.setVolume(v).catch(report("volume"));
            }}
          />
        </label>
    </section>
  );
}
