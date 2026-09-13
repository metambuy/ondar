// M1 dev harness. Deliberately plain: this is a test bench for the audio engine, not the
// product UI (that starts at M2). It holds no logic beyond rendering state and sending
// commands.
import { useEffect, useState } from "react";
import { audio, onMetadata, onState, onStreamInfo } from "./api";
import type { EqBand, PlaybackState, StreamInfo } from "./api";

// Known-good public streams for testing. URLs rot; swap freely.
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

export default function App() {
  const [url, setUrl] = useState(PRESETS[0].url);
  const [state, setState] = useState<PlaybackState>({ kind: "idle" });
  const [info, setInfo] = useState<StreamInfo | null>(null);
  const [title, setTitle] = useState<string | null>(null);
  const [volume, setVolume] = useState(1);
  const [eq, setEq] = useState<EqBand[]>([]);
  const [log, setLog] = useState<string[]>([]);

  const push = (line: string) =>
    setLog((l) => [`${new Date().toLocaleTimeString()}  ${line}`, ...l].slice(0, 40));

  useEffect(() => {
    const unlisteners: Promise<() => void>[] = [
      onState((s) => {
        setState(s);
        push(`state → ${describe(s)}`);
        if (s.kind === "connecting") {
          setInfo(null);
          setTitle(null);
        }
      }),
      onStreamInfo((i) => {
        setInfo(i);
        push(`stream ${i.content_type ?? "?"} ${i.sample_rate} Hz ×${i.channels}`);
      }),
      onMetadata((m) => {
        setTitle(m.title);
        push(`title → ${m.title ?? "(none)"}`);
      }),
    ];
    audio.getPlaybackState().then(setState);
    audio.getEq().then(setEq);
    return () => {
      unlisteners.forEach((p) => p.then((un) => un()));
    };
  }, []);

  const play = () => audio.play(url, "manual").catch((e) => push(`play failed: ${JSON.stringify(e)}`));

  const onGain = (band: number, gainDb: number) => {
    setEq((bands) => bands.map((b) => (b.index === band ? { ...b, gain_db: gainDb } : b)));
    audio.setEqGain(band, gainDb).catch((e) => push(`eq failed: ${JSON.stringify(e)}`));
  };

  const busy = state.kind === "connecting" || state.kind === "buffering" || state.kind === "reconnecting";

  return (
    <div style={{ fontFamily: "system-ui, sans-serif", padding: 16, maxWidth: 420, fontSize: 13 }}>
      <h2 style={{ margin: "0 0 12px" }}>Ondar — audio engine bench</h2>

      <label style={{ display: "block", marginBottom: 6 }}>
        Preset{" "}
        <select value={url} onChange={(e) => setUrl(e.target.value)}>
          {PRESETS.map((p) => (
            <option key={p.url} value={p.url}>
              {p.name}
            </option>
          ))}
          {!PRESETS.some((p) => p.url === url) && <option value={url}>(custom)</option>}
        </select>
      </label>
      <input
        style={{ width: "100%", boxSizing: "border-box", marginBottom: 8 }}
        value={url}
        onChange={(e) => setUrl(e.target.value)}
        placeholder="https://…/stream"
      />

      <div style={{ display: "flex", gap: 6, marginBottom: 12 }}>
        <button onClick={play}>Play</button>
        <button onClick={() => audio.pause()} disabled={state.kind !== "playing"}>
          Pause
        </button>
        <button onClick={() => audio.resume()} disabled={state.kind !== "paused"}>
          Resume
        </button>
        <button onClick={() => audio.stop()} disabled={state.kind === "idle"}>
          Stop
        </button>
        <label style={{ marginLeft: "auto" }}>
          Vol{" "}
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={volume}
            onChange={(e) => {
              const v = Number(e.target.value);
              setVolume(v);
              audio.setVolume(v);
            }}
          />
        </label>
      </div>

      <div style={{ padding: 10, border: "1px solid #8884", borderRadius: 6, marginBottom: 12 }}>
        <div>
          <b>State:</b> {describe(state)} {busy && "…"}
        </div>
        <div>
          <b>Station:</b> {info?.station_name ?? "—"}
        </div>
        <div>
          <b>Now:</b> {title ?? "—"}
        </div>
        <div style={{ opacity: 0.7 }}>
          {info
            ? `${info.content_type ?? "?"} · ${info.bitrate_kbps ? `${info.bitrate_kbps} kbps · ` : ""}${info.sample_rate} Hz · ${info.channels === 1 ? "mono" : "stereo"}`
            : "no stream"}
        </div>
      </div>

      <div style={{ marginBottom: 12 }}>
        <b>EQ</b>{" "}
        <button style={{ fontSize: 11 }} onClick={() => eq.forEach((b) => onGain(b.index, 0))}>
          flat
        </button>
        <div style={{ display: "flex", gap: 4, marginTop: 6 }}>
          {eq.map((b) => (
            <div key={b.index} style={{ textAlign: "center", flex: 1 }}>
              <input
                type="range"
                min={-12}
                max={12}
                step={0.5}
                value={b.gain_db}
                onChange={(e) => onGain(b.index, Number(e.target.value))}
                style={{ writingMode: "vertical-lr", direction: "rtl", height: 90, width: 20 }}
              />
              <div style={{ fontSize: 10 }}>{b.center_hz >= 1000 ? `${b.center_hz / 1000}k` : Math.round(b.center_hz)}</div>
              <div style={{ fontSize: 10, opacity: 0.7 }}>{b.gain_db > 0 ? `+${b.gain_db}` : b.gain_db}</div>
            </div>
          ))}
        </div>
      </div>

      <pre style={{ fontSize: 11, maxHeight: 200, overflow: "auto", background: "#8881", padding: 8, borderRadius: 6 }}>
        {log.join("\n") || "waiting for events…"}
      </pre>
    </div>
  );
}
