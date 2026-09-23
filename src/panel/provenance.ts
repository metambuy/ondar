// The provenance line a list carries (`source`, `age_secs`, `refreshing` — Rust's words for
// where the rows came from), rendered as text. Shared by the country control and the station
// list; the numbers are Rust's, the formatting is the only thing here.
export function age(secs: number): string {
  if (secs < 120) return `${secs} s ago`;
  if (secs < 7200) return `${Math.round(secs / 60)} min ago`;
  if (secs < 172800) return `${Math.round(secs / 3600)} h ago`;
  return `${Math.round(secs / 86400)} d ago`;
}

export function provenance(l: { source: { kind: string }; age_secs: number; refreshing: boolean }): string {
  const source = l.source.kind === "fresh" ? "fresh" : `cached ${age(l.age_secs)}`;
  return l.refreshing ? `${source} · refreshing…` : source;
}

export type OndarErrorLike = { code: string; message: string };

export function describeError(e: unknown): string {
  const err = e as OndarErrorLike;
  return err && typeof err === "object" && "code" in err ? `${err.code}: ${err.message}` : String(e);
}
