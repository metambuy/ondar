// Dev station list (M3a): a country select and the first rows of its ranked list, with Play,
// so the directory, the cache and the events are exercisable by hand until M3b's UI replaces
// this. Visibly a placeholder, like the dev transport. Renders what Rust answers and reports
// clicks; holds no logic — the provenance line is Rust's `source`/`age_secs`/`refreshing`.
import { useEffect, useState } from "react";
import { audio, onCountriesUpdated, onStationsUpdated, stations } from "../api";
import type { ListedCountries, ListedStations, OndarError } from "../api";
import styles from "./panel.module.css";

const ROWS_SHOWN = 50;

function describeError(e: unknown): string {
  const err = e as OndarError;
  return err && typeof err === "object" && "code" in err ? `${err.code}: ${err.message}` : String(e);
}

function age(secs: number): string {
  if (secs < 120) return `${secs} s ago`;
  if (secs < 7200) return `${Math.round(secs / 60)} min ago`;
  return `${Math.round(secs / 3600)} h ago`;
}

function provenance(l: { source: { kind: string }; age_secs: number; refreshing: boolean }): string {
  const source = l.source.kind === "fresh" ? "fresh" : `cached ${age(l.age_secs)}`;
  return l.refreshing ? `${source} · refreshing…` : source;
}

export default function DevStations() {
  const [countries, setCountries] = useState<ListedCountries | null>(null);
  const [selected, setSelected] = useState<string>("PT");
  const [list, setList] = useState<ListedStations | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadCountries = () => {
    stations.listCountries().then(setCountries, (e) => setError(describeError(e)));
  };
  const loadStations = (cc: string) => {
    stations.listStations(cc).then(
      (l) => {
        setList(l);
        setError(null);
      },
      (e) => setError(describeError(e)),
    );
  };

  useEffect(loadCountries, []);
  useEffect(() => loadStations(selected), [selected]);

  // A background refresh landed (stale-while-revalidate): ask again for what is on screen.
  useEffect(() => {
    const un1 = onStationsUpdated((u) => {
      if (u.country_code === selected) loadStations(selected);
    });
    const un2 = onCountriesUpdated(loadCountries);
    return () => {
      un1.then((un) => un());
      un2.then((un) => un());
    };
  }, [selected]);

  return (
    <section className={styles.section} aria-label="Dev stations">
      <h2 className={styles.heading}>Dev stations</h2>
      <div className={styles.row}>
        <label className={styles.field}>
          Country
          <select value={selected} onChange={(e) => setSelected(e.target.value)} disabled={countries === null}>
            {(countries?.items ?? [{ code: "PT", name: "Portugal", station_count: 0 }]).map((c) => (
              <option key={c.code} value={c.code}>
                {c.name} ({c.station_count})
              </option>
            ))}
          </select>
        </label>
        <span className={styles.muted}>{countries ? provenance(countries) : "countries: loading…"}</span>
      </div>
      {error && <p className={styles.line}>{error}</p>}
      {list && (
        <p className={styles.muted}>
          {list.country_code}: {list.items.length} stations · {provenance(list)}
        </p>
      )}
      <ul className={styles.stack} aria-label="Stations">
        {(list?.items ?? []).slice(0, ROWS_SHOWN).map((s) => (
          <li key={s.uuid} className={styles.row}>
            <button
              type="button"
              aria-label={`Play ${s.name}`}
              onClick={() => {
                void audio.play(s.url, s.uuid);
                void stations.recordPlayed(s);
              }}
            >
              Play
            </button>
            <span className={styles.clamp}>{s.name}</span>
            <span className={styles.muted}>
              {s.codec}
              {s.bitrate_kbps === null ? "" : ` ${s.bitrate_kbps}k`}
              {s.hls ? " hls" : ""}
              {s.video ? " video" : ""}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}
