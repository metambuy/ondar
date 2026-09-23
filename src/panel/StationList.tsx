// The station list (M3b 1b): the ranked rows Rust serves for the selected country, one line
// each (decision 2, R1: name, then codec and bitrate; a long name is clamped with an ellipsis),
// scrolling inside the collapsed pane. A click plays the station through the existing `play`.
// Renders what Rust answers and reports clicks; holds no logic beyond which reply to apply:
//
// - **The wrong-country guard** (`/code-review` finding 7, 2026-09-22; BUILD_PLAN M3b): a reply
//   is applied only if its `country_code` is the selection at the moment it lands. A missing
//   list can wait on the network for up to the client's 200 s budget and land after a fast
//   reply for the next selection; the promise callback captured an older render's `selected`,
//   so the guard reads a ref the effect keeps current. `StationList.test.tsx` pins it.
// - **Re-request on show** (M3a acceptance item 6, carried): `showGeneration` changes on every
//   effective show, and the effect re-requests; the service refreshes only an expired list, so
//   a fresh one costs a cache read.
// - **After a refresh:** `landed` → ask again; `failed` → Rust says the expired list stays, so
//   clear the flag it set and do not ask again (an offline page would otherwise loop).
//
// `record_played` is called here on the click, as the dev list did, until the click endpoint's
// commit moves both to the first `Playing` of the session (M3b plan, commit 5).
import { useEffect, useRef, useState } from "react";
import { audio, onStationsUpdated, stations } from "../api";
import type { ListedStations, Station } from "../api";
import { measureMode, reportBlocks, reportList } from "../measure";
import styles from "./panel.module.css";
import { describeError, provenance } from "./provenance";

type Props = {
  selected: string;
  /** Bumped by `Panel` on every effective show; a change re-requests the list. */
  showGeneration: number;
};

function meta(s: Station): string {
  const bitrate = s.bitrate_kbps === null ? "" : ` ${s.bitrate_kbps}k`;
  return `${s.codec}${bitrate}${s.hls ? " hls" : ""}${s.video ? " video" : ""}`;
}

export default function StationList({ selected, showGeneration }: Props) {
  const [list, setList] = useState<ListedStations | null>(null);
  const [error, setError] = useState<string | null>(null);
  const selectedRef = useRef(selected);
  const listRef = useRef<HTMLUListElement>(null);

  const load = (cc: string) =>
    stations.listStations(cc).then(
      (l) => {
        if (l.country_code !== selectedRef.current) return;
        setList(l);
        setError(null);
      },
      (e) => {
        if (cc === selectedRef.current) setError(describeError(e));
      },
    );

  useEffect(() => {
    selectedRef.current = selected;
    load(selected);
  }, [selected, showGeneration]);

  useEffect(() => {
    const unlisten = onStationsUpdated((u) => {
      if (u.country_code !== selectedRef.current) return;
      if (u.outcome === "landed") load(u.country_code);
      else setList((l) => (l && l.country_code === u.country_code ? { ...l, refreshing: false } : l));
    });
    return () => {
      unlisten.then((un) => un());
    };
  }, []);

  // The measurement harness (debug builds under `?measure=fit` only): after the list landed and
  // after every show, every block's box and the rows that fit — `src/measure.ts`.
  useEffect(() => {
    if (measureMode() !== "fit" || listRef.current === null) return;
    const cc = list?.country_code ?? "none";
    reportBlocks(document, { trigger: "list", cc });
    reportList(listRef.current, "li", `.${styles.stationName}`, { cc, show_generation: showGeneration });
  }, [list, showGeneration]);

  const shown = list !== null && list.country_code === selected ? list : null;
  const status = shown
    ? `${shown.items.length} stations · ${provenance(shown)}`
    : (error ?? "loading…");

  return (
    <section className={styles.fill} aria-label="Stations">
      <p className={`${styles.muted} ${styles.clamp}`} data-measure="stations_provenance">
        {status}
      </p>
      <ul ref={listRef} className={styles.list} data-measure="list_viewport">
        {shown && shown.items.length === 0 && (
          <li className={styles.muted}>No stations for {selected} after filtering.</li>
        )}
        {(shown?.items ?? []).map((s) => (
          <li key={s.uuid}>
            <button
              type="button"
              className={styles.station}
              aria-label={`Play ${s.name}`}
              onClick={() => {
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
