// The page half of the dev-only measurement harness (M3b commit 1a; the Rust half is
// `src-tauri/src/measure.rs`). Active only when the popover was loaded as `panel.html?measure=…`,
// which only a debug build launched with `ONDAR_MEASURE` does; otherwise every function here is
// a no-op and no command is called. Reports are one log line each on the Rust side, stamped with
// the process clock, so a measurement is read from the app's log like every other quantity in
// this project — nothing is stored or shown on the page.
import { measure as api } from "./api";

const params: URLSearchParams | null = (() => {
  const p = new URLSearchParams(window.location.search);
  return p.has("measure") ? p : null;
})();

/** The mode (`fit`, `perf`), or `null` when the harness is inactive. */
export function measureMode(): string | null {
  return params?.get("measure") ?? null;
}

/** A further query parameter of the launch line (`?measure=fit&cc=FR` → `param("cc")`). */
export function measureParam(name: string): string | null {
  return params?.get(name) ?? null;
}

export type Fields = Record<string, string | number | boolean | null | undefined>;

function format(v: string | number | boolean | null | undefined): string {
  if (typeof v === "number") return Number.isInteger(v) ? String(v) : v.toFixed(1);
  return String(v);
}

/** One report line: `measure[<mode>] <kind> k=v k=v … t_page_ms=… t_ms=…` in the Rust log. */
export function report(kind: string, fields: Fields): void {
  if (params === null) return;
  const text = Object.entries(fields)
    .map(([k, v]) => `${k}=${format(v)}`)
    .join(" ");
  void api.report(kind, text, performance.now());
}

/**
 * Every `[data-measure]` element under `root`, as one `block` line each: its box in CSS px
 * (= points inside this webview, `tokens.css`), and whether its content is wider than its box
 * (`scroll_width > client_width` — the overflow question for a control row).
 */
export function reportBlocks(root: ParentNode, extra: Fields = {}): void {
  if (params === null) return;
  root.querySelectorAll<HTMLElement>("[data-measure]").forEach((el) => {
    const r = el.getBoundingClientRect();
    report("block", {
      block: el.dataset.measure,
      top: r.top,
      height: r.height,
      width: r.width,
      right: r.right,
      scroll_width: el.scrollWidth,
      client_width: el.clientWidth,
      overflow: el.scrollWidth > el.clientWidth,
      ...extra,
    });
  });
}

/**
 * The scrolling list: how many rows fit its viewport, two ways that must agree — `rows_full`
 * from the measured pitch (⌊(viewport + gap) / pitch⌋, the gap being what follows the last
 * full row) and `rows_by_rect` by counting rows whose bottom edge is inside the viewport — plus
 * the pitch's spread over every row (a non-uniform pitch makes the first number meaningless)
 * and how many names the one-line design clamps (`scroll_width > client_width` on the name).
 */
export function reportList(list: HTMLElement, rowSelector: string, nameSelector: string, extra: Fields = {}): void {
  if (params === null) return;
  const v = list.getBoundingClientRect();
  const rows = Array.from(list.querySelectorAll<HTMLElement>(rowSelector)).map((el) =>
    el.getBoundingClientRect(),
  );
  const pitches = rows.slice(1).map((r, i) => r.top - rows[i].top);
  const pitch = pitches.length > 0 ? pitches[0] : (rows[0]?.height ?? 0);
  const rowHeight = rows[0]?.height ?? 0;
  const gap = pitch - rowHeight;
  const rowsFull = pitch > 0 ? Math.floor((v.height + gap) / pitch) : 0;
  const rowsByRect = rows.filter((r) => r.bottom <= v.bottom + 0.5).length;
  const rowsPartial = rows.filter((r) => r.top < v.bottom && r.bottom > v.bottom + 0.5).length;
  const names = Array.from(list.querySelectorAll<HTMLElement>(nameSelector));
  const clamped = names.filter((el) => el.scrollWidth > el.clientWidth).length;
  const longest = names.reduce((m, el) => Math.max(m, el.scrollWidth), 0);
  report("list", {
    rows: rows.length,
    viewport_top: v.top,
    viewport_height: v.height,
    row_height: rowHeight,
    pitch,
    pitch_min: pitches.length > 0 ? Math.min(...pitches) : pitch,
    pitch_max: pitches.length > 0 ? Math.max(...pitches) : pitch,
    rows_full: rowsFull,
    rows_by_rect: rowsByRect,
    rows_partial: rowsPartial,
    names_clamped: clamped,
    names: names.length,
    longest_px: longest,
    ...extra,
  });
}

/**
 * Commit 2's marks and sampler. The mount marks are taken where they happen (`StationList`);
 * this is the arithmetic and the report line, so a component carries as little as possible.
 */
export type MountMarks = {
  n: number;
  rep: number;
  reply_bytes: number;
  t_request: number;
  t_reply: number;
  t_commit: number;
  t_paint: number;
};

export function reportMount(m: MountMarks): void {
  report("mount", {
    n: m.n,
    rep: m.rep,
    reply_bytes: m.reply_bytes,
    reply_ms: m.t_reply - m.t_request,
    commit_ms: m.t_commit - m.t_reply,
    paint_ms: m.t_paint - m.t_commit,
    total_ms: m.t_paint - m.t_request,
  });
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const i = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[i];
}

/**
 * Sample the interval between consecutive animation frames for `durationMs`, calling `step`
 * each frame (a programmatic scroll, or nothing for a hand-driven one), then report the
 * distribution and the raw intervals — the median is the display's effective frame period as
 * the page saw it, and "dropped" counts intervals over twice that median.
 */
export function sampleFrames(kind: string, durationMs: number, step: () => void, extra: Fields = {}): void {
  if (params === null) return;
  const intervals: number[] = [];
  let last = performance.now();
  const start = last;
  const tick = (now: number) => {
    intervals.push(now - last);
    last = now;
    step();
    if (now - start < durationMs) requestAnimationFrame(tick);
    else finish();
  };
  const finish = () => {
    const sorted = [...intervals].sort((a, b) => a - b);
    const median = percentile(sorted, 50);
    report(kind, {
      frames: intervals.length,
      duration_ms: last - start,
      median_ms: median,
      p95_ms: percentile(sorted, 95),
      max_ms: sorted[sorted.length - 1] ?? 0,
      dropped: intervals.filter((d) => d > 2 * median).length,
      ...extra,
      raw: intervals.map((d) => d.toFixed(1)).join(","),
    });
  };
  requestAnimationFrame((t0) => {
    last = t0;
    requestAnimationFrame(tick);
  });
}
