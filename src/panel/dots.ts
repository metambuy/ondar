// A map dot's words, shared by the hover label (`MapPane`) and the list's chip (`StationList`),
// so the two read the same (decision 7, decision 3).

/** "place · " when the place is non-empty, then "n station(s)": the place first (0.1.1). */
export function dotText(n: number, place: string): string {
  return `${place === "" ? "" : `${place} · `}${n} ${n === 1 ? "station" : "stations"}`;
}
