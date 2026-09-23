// What the station list shows (M3b commit 4, decision 2): one country's ranked list, the
// favourites, or the recents — chosen from one control, the country select, whose first two
// entries are the two stores. A filter on the same list rather than a third pane or a segmented
// control: no height taken from the six rows the collapsed pane fits, one control to reach by
// keyboard, and the rows, the guard and the re-request rules are the same code.
export type ListSource = { kind: "country"; cc: string } | { kind: "favourites" } | { kind: "recents" };

/** The select's `value` for a source, and back. A country is `cc:PT`; the stores are bare. */
export function sourceValue(s: ListSource): string {
  return s.kind === "country" ? `cc:${s.cc}` : s.kind;
}

export function parseSourceValue(v: string): ListSource {
  if (v === "favourites" || v === "recents") return { kind: v };
  return { kind: "country", cc: v.startsWith("cc:") ? v.slice(3) : v };
}

/** Identity for the reply guard and effect dependencies: same key, same list. */
export function sourceKey(s: ListSource): string {
  return sourceValue(s);
}
