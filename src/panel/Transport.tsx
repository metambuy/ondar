// Dev transport (M2c, decision 1): the M1 bench folded into the popover so the audio path and
// the tray's playing glyph stay exercisable by hand until the M3 country/station UI replaces
// this. Visibly a placeholder. Renders state and sends commands; holds no logic.
//
// Not carried over from the bench, deliberately: the custom-URL field, the on-page event log
// (Rust's log has every event) and the EQ sliders (EQ UI is M5; the engine claim is held by
// `eq::tests` and `eq_headroom_sweep`).
import { useEffect, useState } from "react";
import { audio, onMetadata, onState, onStreamInfo } from "../api";
import type { PlaybackState, StreamInfo } from "../api";
import styles from "./panel.module.css";

// Known-good public streams. URLs rot; swap freely.
const PRESETS: { name: string; url: string }[] = [
  { name: "Radio Swiss Jazz (MP3 128k)", url: "https://stream.srg-ssr.ch/m/rsj/mp3_128" },
  { name: "FIP (AAC)", url: "https://icecast.radiofrance.fr/fip-hifi.aac" },
  { name: "SomaFM Groove Salad (MP3)", url: "https://ice1.somafm.com/groovesalad-128-mp3" },
];

function describe(s: PlaybackState): string {
  switch (s.kind) {
    case "reconnecting":
      return `reconnecting (attempt ${s.attempt})`;
    case "error":
      return `error [${s.code}]: ${s.message}`;
    default:
      return s.kind;
  }
}

function describeStream(i: StreamInfo | null): string {
  if (!i) return "no stream open";
  const codec = i.content_type ?? "unknown codec";
  const rate = `${i.sample_rate} Hz`;
  const channels = i.channels === 1 ? "mono" : i.channels === 2 ? "stereo" : `${i.channels} ch`;
  const bitrate = i.bitrate_kbps === null ? "" : ` · ${i.bitrate_kbps} kbps`;
  return `${codec} · ${rate} · ${channels}${bitrate}`;
}

export default function Transport() {
  const [url, setUrl] = useState(PRESETS[0].url);
  // The URL handed to `play`, kept apart from the dropdown: the dropdown can change without a
  // Play, and the Now Playing name must describe what is audible, not what is selected
  // (`/code-review` finding 4, 2026-09-17).
  const [playingUrl, setPlayingUrl] = useState<string | null>(null);
  const [state, setState] = useState<PlaybackState>({ kind: "idle" });
  const [info, setInfo] = useState<StreamInfo | null>(null);
  const [title, setTitle] = useState<string | null>(null);
  const [volume, setVolume] = useState(1);
  const [lastError, setLastError] = useState<string | null>(null);

  useEffect(() => {
    const unlisteners: Promise<() => void>[] = [
      onState((s) => {
        setState(s);
        if (s.kind === "connecting") {
          setInfo(null);
          setTitle(null);
        }
      }),
      onStreamInfo(setInfo),
      onMetadata((m) => setTitle(m.title)),
    ];
    audio.getPlaybackState().then(setState);
    return () => {
      unlisteners.forEach((p) => p.then((un) => un()));
    };
  }, []);

  // A command's rejection is argument validation only (CLAUDE.md, IPC contract); playback
  // outcomes arrive as `playback:state` events.
  const report = (what: string) => (e: unknown) => setLastError(`${what}: ${JSON.stringify(e)}`);
  const play = () => {
    setPlayingUrl(url);
    return audio.play(url, "manual").catch(report("play"));
  };

  // `icy-name` when the server sends one (its absence is normal); otherwise the preset that was
  // actually played; otherwise nothing is playing.
  const stationName =
    info?.station_name ??
    (playingUrl === null
      ? "nothing playing"
      : (PRESETS.find((p) => p.url === playingUrl)?.name ?? playingUrl));

  return (
    <section aria-label="Dev transport" className={styles.stack}>
      {/* `data-measure`: the dev Now Playing block, measured as such by M3b 1b and replaced by
          `NowPlaying.tsx` in 1c. Same `.stack`, so the gaps inside equal the gaps around. */}
      <div className={styles.stack} data-measure="now_playing">
        <h1 className={styles.heading}>Now Playing</h1>
        <p className={styles.line}>{stationName}</p>
        <p className={styles.line}>{title ?? "—"}</p>
        <p className={`${styles.muted} ${styles.clamp}`}>
          {describe(state)} · {describeStream(info)}
        </p>
        {lastError && (
          <p className={`${styles.muted} ${styles.clamp}`} role="alert">
            {lastError}
          </p>
        )}
      </div>

      <div className={styles.section} data-measure="transport_controls">
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
      </div>
    </section>
  );
}
