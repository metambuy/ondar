// A map dot's words, shared by the hover label (`MapPane`) and the list's chip (`StationList`),
// so the two read the same (decision 7, decision 3).

/** "n station(s)", then " · place" when the place is non-empty. */
export function dotText(n: number, place: string): string {
  return `${n} ${n === 1 ? "station" : "stations"}${place === "" ? "" : ` · ${place}`}`;
}
