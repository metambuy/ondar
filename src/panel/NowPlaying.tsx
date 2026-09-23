// Now Playing (M3b 1c, the product shape): the station's name, the live ICY title, and one line
// of `flag · codec · bitrate` with the playback state where it is not simply "playing" — an
// error as text a person can act on, clamped to three lines (the full text is in Rust's log).
// Mirrors Rust's `playback:*` events; the station is the one the page last asked to play (view
// state, handed down by `Panel`), never a parallel model of what is audible: Rust says whether
// anything is playing, this only names it.
//
// The title line is reserved at body height while no title has arrived (decision 5, F4): a
// title arriving mid-stream must not move the blocks below it, at the cost of 16 pt of the list
// band. ` ` keeps the line a line.
import { useEffect, useState } from "react";
import { audio, onMetadata, onState, onStreamInfo } from "../api";
import type { PlaybackState, Station, StreamInfo } from "../api";
import styles from "./panel.module.css";

/** `FR` → 🇫🇷: the two regional-indicator symbols; anything but two ASCII letters → none. */
export function flag(countryCode: string): string {
  if (!/^[A-Z]{2}$/.test(countryCode)) return "";
  return String.fromCodePoint(...[...countryCode].map((c) => 0x1f1e6 + c.charCodeAt(0) - 65));
}

function stateText(s: PlaybackState): string | null {
  switch (s.kind) {
    case "idle":
      return "nothing playing";
    case "connecting":
      return "connecting…";
    case "buffering":
      return "buffering…";
    case "paused":
      return "paused";
    case "reconnecting":
      return `reconnecting (attempt ${s.attempt})`;
    case "error":
      return `error [${s.code}]: ${s.message}`;
    case "playing":
      return null;
  }
}

function metaText(station: Station | null, info: StreamInfo | null, state: PlaybackState): string {
  const parts: string[] = [];
  if (station) {
    const f = flag(station.country_code);
    if (f) parts.push(f);
    parts.push(station.codec);
    const bitrate = info?.bitrate_kbps ?? station.bitrate_kbps;
    if (bitrate !== null) parts.push(`${bitrate} kbps`);
  } else if (info) {
    parts.push(info.content_type ?? "unknown codec");
    if (info.bitrate_kbps !== null) parts.push(`${info.bitrate_kbps} kbps`);
  }
  const s = stateText(state);
  if (s !== null) parts.push(s);
  return parts.join(" · ");
}

export default function NowPlaying({ station }: { station: Station | null }) {
  const [state, setState] = useState<PlaybackState>({ kind: "idle" });
  const [info, setInfo] = useState<StreamInfo | null>(null);
  const [title, setTitle] = useState<string | null>(null);

  useEffect(() => {
    const unlisteners = [
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

  // The directory's name for a station the list played; the server's `icy-name` for anything
  // else (a preset), whose absence is normal; nothing while idle.
  const name = state.kind === "idle" ? "—" : (station?.name ?? info?.station_name ?? "—");

  return (
    <section aria-label="Now Playing" className={styles.stack} data-measure="now_playing">
      <p className={`${styles.heading} ${styles.nowrapClamp}`} data-measure="now_playing_name">
        {name}
      </p>
      <p className={`${styles.line} ${styles.nowrapClamp}`} data-measure="now_playing_title">
        {title ?? " "}
      </p>
      <p
        className={`${styles.muted} ${styles.clamp}`}
        role={state.kind === "error" ? "alert" : undefined}
        data-measure="now_playing_meta"
      >
        {metaText(station, info, state)}
      </p>
    </section>
  );
}
