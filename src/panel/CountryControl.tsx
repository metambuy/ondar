// The country control (M3b 1b, decision 1: a native `<select>` — 240 rows, keyboard type-ahead
// and a native height for free; a searchable list is a later commit if this proves poor by
// hand). Its first two entries are the favourites and the recents (commit 4, decision 2: a
// filter on the same list, see `source.ts`). Renders what Rust answers and reports a choice;
// the selection itself lives in `Panel.tsx`, since the station list and, at M4, the map share
// it. The provenance line after the control is Rust's `source`/`age_secs`/`refreshing` for the
// countries list.
//
// Requests: on mount, on every popover show (`showGeneration`; an expired list is refreshed by
// the service only when asked for again — M3a acceptance item 6, carried) and after a refresh
// `landed`; a `failed` refresh clears the flag without asking again (an offline page would
// otherwise loop fetch → fail → event → fetch).
import { useEffect, useState } from "react";
import { onCountriesUpdated, stations } from "../api";
import type { ListedCountries } from "../api";
import styles from "./panel.module.css";
import { describeError, provenance } from "./provenance";
import { parseSourceValue, sourceValue } from "./source";
import type { ListSource } from "./source";

type Props = {
  source: ListSource;
  onSelect: (source: ListSource) => void;
  /** Bumped by `Panel` on every effective show; a change re-requests the list. */
  showGeneration: number;
};

export default function CountryControl({ source, onSelect, showGeneration }: Props) {
  const [countries, setCountries] = useState<ListedCountries | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      stations.listCountries().then(
        (c) => {
          if (cancelled) return;
          setCountries(c);
          setError(null);
        },
        (e) => {
          if (!cancelled) setError(describeError(e));
        },
      );
    load();
    const unlisten = onCountriesUpdated((u) => {
      if (u.outcome === "landed") load();
      else setCountries((c) => (c ? { ...c, refreshing: false } : c));
    });
    return () => {
      cancelled = true;
      unlisten.then((un) => un());
    };
  }, [showGeneration]);

  // While the countries are loading the select shows the selection alone, so it is never
  // empty — and the two stores, which need no network.
  const items =
    countries?.items ??
    (source.kind === "country" ? [{ code: source.cc, name: source.cc, station_count: 0 }] : []);
  return (
    <div className={styles.row} data-measure="country_row">
      <label className={`${styles.field} ${styles.grow}`}>
        Country
        <select
          data-measure="country_select"
          value={sourceValue(source)}
          onChange={(e) => onSelect(parseSourceValue(e.target.value))}
          disabled={countries === null && source.kind === "country"}
        >
          <option value="favourites">★ Favourites</option>
          <option value="recents">Recents</option>
          <hr />
          {items.map((c) => (
            <option key={c.code} value={sourceValue({ kind: "country", cc: c.code })}>
              {c.name} ({c.station_count})
            </option>
          ))}
        </select>
      </label>
      <span className={`${styles.muted} ${styles.nowrap}`} data-measure="countries_provenance">
        {countries ? provenance(countries) : (error ?? "loading…")}
      </span>
    </div>
  );
}
