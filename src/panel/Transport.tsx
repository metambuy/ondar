// The transport row (M3b commit 4; the M1 bench's controls folded in at M2c, the presets
// retired here): play or pause, stop, the favourite toggle, the volume. Renders state and
// sends commands; holds no logic. Rust is the source of playback state (`playback:state`);
// whether the playing station is a favourite comes from `Panel`, which asks Rust.
//
// Not carried over from the bench, deliberately: the custom-URL field, the on-page event log
// (Rust's log has every event) and the EQ sliders (EQ UI is M5; the engine claim is held by
// `eq::tests` and `eq_headroom_sweep`).
import { useEffect, useState } from "react";
import { audio, onState } from "../api";
import type { PlaybackState, Station } from "../api";
import styles from "./panel.module.css";

type Props = {
  /** The station Now Playing names (`Panel`'s `playing`); Play replays it. */
  station: Station | null;
  isFavourite: boolean;
  onToggleFavourite: () => void;
};

export default function Transport({ station, isFavourite, onToggleFavourite }: Props) {
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
  const active = state.kind !== "idle" && state.kind !== "error";

  return (
    <section aria-label="Transport" className={styles.section} data-measure="transport_controls">
      {lastError && (
        <p className={`${styles.muted} ${styles.clamp}`} role="alert">
          {lastError}
        </p>
      )}
      <div className={styles.row}>
        {state.kind === "playing" || state.kind === "buffering" || state.kind === "connecting" ? (
          <button type="button" onClick={() => audio.pause()} disabled={state.kind !== "playing"}>
            Pause
          </button>
        ) : state.kind === "paused" ? (
          <button type="button" onClick={() => audio.resume()}>
            Resume
          </button>
        ) : (
          <button
            type="button"
            disabled={station === null}
            onClick={() => station && audio.play(station.url, station.uuid).catch(report("play"))}
          >
            Play
          </button>
        )}
        <button type="button" onClick={() => audio.stop()} disabled={!active}>
          Stop
        </button>
        <button
          type="button"
          aria-pressed={isFavourite}
          aria-label={isFavourite ? "Remove from favourites" : "Add to favourites"}
          disabled={station === null}
          onClick={onToggleFavourite}
        >
          {isFavourite ? "★" : "☆"}
        </button>
        <label className={`${styles.field} ${styles.grow}`}>
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
      </div>
    </section>
  );
}
