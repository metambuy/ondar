// What the station list shows: one country's ranked list, or "mine" — the favourites, then the
// recents not already among them, as one list. The ★ button beside the country select switches
// between the two (M3b acceptance item 2, finding C, decided 2026-09-23: the stores used to be
// the select's first two entries, and at acceptance they sat above ~240 countries, out of view
// in a native menu that opens at the selected one). One list rather than two: one toggle, no
// station row taken, and the rows, the guard and the re-request rules are the same code.
export type ListSource = { kind: "country"; cc: string } | { kind: "mine" };

/** Identity for the reply guard and effect dependencies: same key, same list. */
export function sourceKey(s: ListSource): string {
  return s.kind === "country" ? `cc:${s.cc}` : "mine";
}
