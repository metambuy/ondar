// The station list (M3b 1b): the rows Rust serves for the selected source — one country's
// ranked list, the favourites or the recents (commit 4, `source.ts`) — one line each (decision
// 2, R1: name, then codec and bitrate; a long name is clamped with an ellipsis), scrolling
// inside the collapsed pane. A click plays the station through the existing `play`. Renders
// what Rust answers and reports clicks; holds no logic beyond which reply to apply:
//
// - **The wrong-source guard** (`/code-review` finding 7, 2026-09-22; BUILD_PLAN M3b): a reply
//   is applied only if it is for the source selected at the moment it lands. A missing country
//   list can wait on the network for up to the client's 200 s budget and land after a fast
//   reply for the next selection; the promise callback captured an older render's `source`, so
//   the guard reads a ref the effect keeps current. `StationList.test.tsx` pins it.
// - **Re-request on show** (M3a acceptance item 6, carried): `showGeneration` changes on every
//   effective show, and the effect re-requests; the service refreshes only an expired list, so
//   a fresh one costs a cache read. `storeGeneration` does the same for a favourite toggled by
//   the transport, and `recents:updated` for a recorded play, each only for its own source.
// - **After a refresh:** `landed` → ask again; `failed` → Rust says the expired list stays, so
//   clear the flag it set and do not ask again (an offline page would otherwise loop).
//
// `record_played` is called here on the click, as the dev list did, until the click endpoint's
// commit moves both to the first `Playing` of the session (M3b plan, commit 5).
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { audio, onRecentsUpdated, onStationsUpdated, stations } from "../api";
import type { Station } from "../api";
import {
  measureMode,
  measureParam,
  report,
  reportBlocks,
  reportList,
  reportMount,
  sampleFrames,
} from "../measure";
import type { MountMarks } from "../measure";
import styles from "./panel.module.css";
import { describeError, provenance } from "./provenance";
import { sourceKey } from "./source";
import type { ListSource } from "./source";

/** A list as this component holds it: the rows and their provenance line. */
type Shown = { key: string; items: Station[]; status: string; bytes: number };

type Props = {
  source: ListSource;
  /** Bumped by `Panel` on every effective show; a change re-requests the list. */
  showGeneration: number;
  /** Bumped by `Panel` when it added or removed a favourite; the favourites re-request. */
  storeGeneration: number;
  /** A row was clicked: `Panel` hands the station to Now Playing. */
  onPlay: (s: Station) => void;
  /** The measurement harness's mount repetition (`?measure=perf&m=mount`); `Panel` keys on it. */
  measureRep?: number;
};

/** The harness's row count (`?measure=perf&n=`), or every row. */
function perfRows(): number | null {
  if (measureMode() !== "perf") return null;
  const n = Number(measureParam("n"));
  return Number.isFinite(n) && n >= 0 ? n : null;
}

/** The scroll sampler's programmatic step, and its arms (`&m=scroll` | `scroll-hand`). */
const SCROLL_STEP_PX = 4;
const SCROLL_START_MS = 10_000;
const SCROLL_DURATION_MS = 6_000;
const SCROLL_HAND_DURATION_MS = 12_000;

function meta(s: Station): string {
  const bitrate = s.bitrate_kbps === null ? "" : ` ${s.bitrate_kbps}k`;
  return `${s.codec}${bitrate}${s.hls ? " hls" : ""}${s.video ? " video" : ""}`;
}

function StationList({ source, showGeneration, storeGeneration, onPlay, measureRep = 0 }: Props) {
  const [list, setList] = useState<Shown | null>(null);
  const [error, setError] = useState<string | null>(null);
  const key = sourceKey(source);
  const sourceRef = useRef(source);
  const listRef = useRef<HTMLUListElement>(null);
  const autoPlayed = useRef(false);
  // The harness's mount marks (`?measure=perf&m=mount`): request → reply → commit → paint.
  const marks = useRef<Partial<MountMarks>>({});

  // One request for the source; the reply is applied only if that source is still selected.
  const load = (s: ListSource) => {
    const k = sourceKey(s);
    if (measureMode() === "perf") marks.current = { t_request: performance.now() };
    const request: Promise<Shown> =
      s.kind === "country"
        ? stations.listStations(s.cc).then((l) => ({
            key: sourceKey({ kind: "country", cc: l.country_code }),
            items: l.items,
            status: `${l.items.length} stations · ${provenance(l)}`,
            bytes: JSON.stringify(l).length,
          }))
        : (s.kind === "favourites" ? stations.listFavourites() : stations.listRecents()).then(
            (items) => ({
              key: k,
              items,
              status: `${items.length} ${s.kind}`,
              bytes: JSON.stringify(items).length,
            }),
          );
    return request.then(
      (shown) => {
        if (shown.key !== sourceKey(sourceRef.current)) return;
        if (measureMode() === "perf") {
          marks.current.t_reply = performance.now();
          marks.current.reply_bytes = shown.bytes;
        }
        setList(shown);
        setError(null);
      },
      (e) => {
        if (k === sourceKey(sourceRef.current)) setError(describeError(e));
      },
    );
  };

  useEffect(() => {
    sourceRef.current = source;
    load(source);
    // `key` stands for `source` (same list, same key), so a re-render with an equal source
    // does not re-request; `load` reads only refs and the module-level API.
  }, [key, showGeneration]);

  // A favourite was toggled: only the favourites list changes.
  useEffect(() => {
    if (storeGeneration > 0 && sourceRef.current.kind === "favourites") load(sourceRef.current);
  }, [storeGeneration]);

  useEffect(() => {
    const un1 = onStationsUpdated((u) => {
      const current = sourceRef.current;
      if (current.kind !== "country" || u.country_code !== current.cc) return;
      if (u.outcome === "landed") load(current);
      else
        setList((l) =>
          l && l.key === sourceKey(current) && l.status.endsWith(" · refreshing…")
            ? { ...l, status: l.status.slice(0, -" · refreshing…".length) }
            : l,
        );
    });
    const un2 = onRecentsUpdated(() => {
      if (sourceRef.current.kind === "recents") load(sourceRef.current);
    });
    return () => {
      un1.then((un) => un());
      un2.then((un) => un());
    };
  }, []);

  // The measurement harness (debug builds under `?measure=fit` only): after the list landed and
  // after every show, every block's box and the rows that fit — `src/measure.ts`.
  useEffect(() => {
    if (measureMode() !== "fit" || listRef.current === null) return;
    const cc = list?.key ?? "none";
    reportBlocks(document, { trigger: "list", cc });
    reportList(listRef.current, "li", `.${styles.stationName}`, { cc, show_generation: showGeneration });
  }, [list, showGeneration]);

  const shown = list !== null && list.key === key ? list : null;
  const rows = perfRows();
  const items = shown === null ? [] : rows === null ? shown.items : shown.items.slice(0, rows);

  // The harness's mount marks: the commit that rendered the rows (layout done, not painted)
  // and the first frame after it — a hidden webview runs no rAF, so this needs the panel shown.
  useLayoutEffect(() => {
    if (measureMode() !== "perf" || measureParam("m") !== "mount" || shown === null) return;
    const m = marks.current;
    if (m.t_request === undefined || m.t_reply === undefined) return;
    m.t_commit = performance.now();
    requestAnimationFrame(() => {
      reportMount({
        n: items.length,
        rep: measureRep,
        reply_bytes: m.reply_bytes ?? 0,
        t_request: m.t_request ?? 0,
        t_reply: m.t_reply ?? 0,
        t_commit: m.t_commit ?? 0,
        t_paint: performance.now(),
      });
    });
  }, [shown, items.length, measureRep]);

  // The harness's scroll sampler (`&m=scroll`: the list scrolled 4 px per frame for 6 s;
  // `&m=scroll-hand`: nothing moved by the page, the intervals sampled while a hand scrolls).
  useEffect(() => {
    const arm = measureMode() === "perf" ? measureParam("m") : null;
    if ((arm !== "scroll" && arm !== "scroll-hand") || shown === null) return;
    const el = listRef.current;
    if (el === null) return;
    const timer = setTimeout(
      () => {
        report("scroll_start", { arm, rows: items.length, scroll_height: el.scrollHeight });
        if (arm === "scroll") {
          sampleFrames("scroll", SCROLL_DURATION_MS, () => (el.scrollTop += SCROLL_STEP_PX), {
            arm,
            rows: items.length,
          });
        } else {
          sampleFrames("scroll", SCROLL_HAND_DURATION_MS, () => {}, { arm, rows: items.length });
        }
      },
      Math.max(0, SCROLL_START_MS - performance.now()),
    );
    return () => clearTimeout(timer);
  }, [shown, items.length]);

  // The measurement harness (debug builds under `?measure=…&play=first` only): play the first
  // row once the list is in, so Now Playing is measured with a real title and stream info.
  // Straight to `play`, not the row's click: no recents entry from a measurement.
  useEffect(() => {
    const first = shown?.items[0];
    if (measureParam("play") !== "first" || autoPlayed.current || first === undefined) return;
    autoPlayed.current = true;
    onPlay(first);
    void audio.play(first.url, first.uuid);
  }, [shown, onPlay]);

  const status = shown ? shown.status : (error ?? "loading…");
  const cue =
    measureMode() === "perf" && measureParam("m") === "scroll-hand" ? " · SCROLL BY HAND from +10 s to +22 s" : "";

  return (
    <section className={styles.fill} aria-label="Stations">
      <p className={`${styles.muted} ${styles.clamp}`} data-measure="stations_provenance">
        {status}
        {cue}
      </p>
      <ul ref={listRef} className={styles.list} data-measure="list_viewport">
        {shown && shown.items.length === 0 && (
          <li className={styles.muted}>
            {source.kind === "country"
              ? `No stations for ${source.cc} after filtering.`
              : source.kind === "favourites"
                ? "No favourites yet — ★ on the transport adds the playing station."
                : "Nothing played yet."}
          </li>
        )}
        {items.map((s) => (
          <li key={s.uuid}>
            <button
              type="button"
              className={styles.station}
              aria-label={`Play ${s.name}`}
              onClick={() => {
                onPlay(s);
                void audio.play(s.url, s.uuid);
                void stations.recordPlayed(s);
              }}
            >
              <span className={styles.stationName}>{s.name}</span>
              <span className={`${styles.muted} ${styles.nowrap}`}>{meta(s)}</span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

// Not memoised, by measurement (M3b commit 2, m2: 20 shows each with 750 rows plain, 750 rows
// behind `React.memo`, and an empty list — medians 8, 7 and 5 ms; the rows' re-render on a
// show is not the cost, so the wrapper was deleted rather than shipped beside this).
export default StationList;
