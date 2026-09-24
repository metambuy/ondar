// The country control (M3b 1b, decision 1: a native `<select>` — 240 rows, keyboard type-ahead
// and a native height for free; a searchable list is a later commit if this proves poor by
// hand), and before it the ★ toggle that switches the list to the favourites and recents
// (`source.ts`; acceptance item 2, finding C — the stores left the select's menu, where they sat
// out of view). Renders what Rust answers and reports a choice; the selection itself lives in
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
  /** The selected country: the select's value whether or not ★ is on. */
  country: string;
  /** A country was chosen; `Panel` shows its list (and turns ★ off). */
  onSelect: (cc: string) => void;
  /** Whether the list shows the favourites and recents (★ on). */
  mine: boolean;
  onToggleMine: () => void;
  /** Bumped by `Panel` on every effective show; a change re-requests the list. */
  showGeneration: number;
};

export default function CountryControl({ country, onSelect, mine, onToggleMine, showGeneration }: Props) {
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

  // While the countries are loading, or when they could not be had, the select shows the
  // selection alone (its code, no count — `PT (0)` read as an empty country at acceptance
  // item 9's fit run), so it is never empty. Neither control is ever disabled: offline with
  // nothing cached the stores, behind ★, are the one thing that still works (M3b acceptance
  // item 9, finding B; `Panel.test.tsx`).
  const items = countries?.items ?? [{ code: country, name: country, station_count: 0 }];
  // An error has no countries to describe, and can be a sentence long: it takes a line of its
  // own under the row, wrapped and clamped to three lines, so the select keeps its width (on
  // the row, `nowrap`, it pushed the control off the panel — item 9). The row keeps the short
  // provenance or `loading…`.
  const failed = countries === null && error !== null;
  return (
    <div className={styles.stack}>
      <div className={styles.row} data-measure="country_row">
        <button
          type="button"
          aria-pressed={mine}
          aria-label="Favourites and recents"
          title="Favourites and recents"
          onClick={onToggleMine}
        >
          {mine ? "★" : "☆"}
        </button>
        <label className={`${styles.field} ${styles.grow}`}>
          Country
          <select
            data-measure="country_select"
            value={country}
            onChange={(e) => onSelect(e.target.value)}
          >
            {items.map((c) => (
              <option key={c.code} value={c.code}>
                {countries ? `${c.name} (${c.station_count})` : c.name}
              </option>
            ))}
          </select>
        </label>
        {!failed && (
          <span className={`${styles.muted} ${styles.nowrap}`} data-measure="countries_provenance">
            {countries ? provenance(countries) : "loading…"}
          </span>
        )}
      </div>
      {failed && (
        <p className={`${styles.muted} ${styles.clamp}`} role="alert" data-measure="countries_error">
          {error}
        </p>
      )}
    </div>
  );
}
