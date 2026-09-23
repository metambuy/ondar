// The country control (M3b 1b, decision 1: a native `<select>` — 240 rows, keyboard type-ahead
// and a native height for free; a searchable list is a later commit if this proves poor by
// hand). Renders what Rust answers and reports a choice; the selection itself lives in
// `Panel.tsx`, since the station list and, at M4, the map share it. The provenance line after
// the control is Rust's `source`/`age_secs`/`refreshing` for the countries list.
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

type Props = {
  selected: string;
  onSelect: (code: string) => void;
  /** Bumped by `Panel` on every effective show; a change re-requests the list. */
  showGeneration: number;
};

export default function CountryControl({ selected, onSelect, showGeneration }: Props) {
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

  // While the list is loading the select shows the selection alone, so it is never empty.
  const items = countries?.items ?? [{ code: selected, name: selected, station_count: 0 }];
  return (
    <div className={styles.row} data-measure="country_row">
      <label className={`${styles.field} ${styles.grow}`}>
        Country
        <select
          data-measure="country_select"
          value={selected}
          onChange={(e) => onSelect(e.target.value)}
          disabled={countries === null}
        >
          {items.map((c) => (
            <option key={c.code} value={c.code}>
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
