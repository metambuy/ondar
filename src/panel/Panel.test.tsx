// The popover as a whole: offline, with and without a map band, and the map following the
// dropdown — vitest under jsdom, `pnpm test`. `../api` is mocked whole: nothing reaches Tauri.
// Each test's comment states what it pins and what it would have to see to fail.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ListedCountries, ListedStations, MapHit, PanelLayout, Station } from "../api";
import Panel from "./Panel";

const orbital: Station = {
  uuid: "9618a87b-0601-11e8-ae97-52543be04c81",
  name: "ORBITAL",
  url: "http://centova.radios.pt:8401/;listen.pls",
  homepage: "",
  favicon: "",
  country_code: "PT",
  codec: "mp3",
  codec_raw: "MP3",
  bitrate_kbps: 192,
  hls: false,
  video: false,
  votes: 1,
  click_count: 0,
  click_trend: 0,
  geo: null,
  state: "",
  last_check_ok: true,
};

// Mutable: the band test sets an expanded layout with a band before rendering.
const layout: PanelLayout = {
  transition: "show",
  generation: 0,
  view: "transport",
  state: "collapsed",
  width: 360,
  height: 420,
  expandable: true,
  band: null,
};
const collapsed = { ...layout };

const offline = { code: "stations", message: "radio-browser unreachable after 3 attempt(s) in 3.01s" };

// The dropdown test sets a countries reply; the offline tests leave it `null` (the list fails).
let countriesReply: ListedCountries | null = null;
// Every `map.select` the pane made, in order.
const selects: string[] = [];
// The dot-filter tests: a stations reply per country (`null`: offline), what `map.hit` answers,
// how many times Esc reached Rust, and the page's `panel:layout` listener.
const stationsReplies = new Map<string, ListedStations>();
let hitReply: MapHit | null = null;
let escapes = 0;
let layoutListener: ((l: PanelLayout) => void) | null = null;

vi.mock("../api", () => {
  const listener = () => Promise.resolve(() => {});
  return {
    audio: {
      play: () => Promise.resolve(),
      pause: () => Promise.resolve(),
      resume: () => Promise.resolve(),
      stop: () => Promise.resolve(),
      setVolume: () => Promise.resolve(),
      getPlaybackState: () => Promise.resolve({ kind: "idle" }),
    },
    panel: {
      escape: () => {
        escapes += 1;
        return Promise.resolve();
      },
      viewBack: () => Promise.resolve(),
      setExpanded: () => Promise.resolve(),
      getLayout: () => Promise.resolve(layout),
      layoutCommitted: () => Promise.resolve(),
    },
    app: { info: () => Promise.resolve({ name: "Ondar", version: "0" }) },
    measure: { report: () => Promise.resolve() },
    map: {
      select: (code: string) => {
        selects.push(code);
        return Promise.resolve();
      },
      pull: () => Promise.resolve(null),
      hit: () => Promise.resolve(hitReply),
    },
    onMapChanged: listener,
    onPanelLayout: (cb: (l: PanelLayout) => void) => {
      layoutListener = cb;
      return Promise.resolve(() => {});
    },
    stations: {
      listCountries: () => (countriesReply ? Promise.resolve(countriesReply) : Promise.reject(offline)),
      listStations: (cc: string) => {
        const l = stationsReplies.get(cc);
        return l ? Promise.resolve(l) : Promise.reject(offline);
      },
      listFavourites: () => Promise.resolve([orbital]),
      listRecents: () => Promise.resolve([]),
      addFavourite: () => Promise.resolve(),
      removeFavourite: () => Promise.resolve(true),
    },
    onState: listener,
    onStreamInfo: listener,
    onMetadata: listener,
    onStationsUpdated: listener,
    onCountriesUpdated: listener,
    onRecentsUpdated: listener,
  };
});

beforeEach(() => {
  countriesReply = null;
  selects.length = 0;
  stationsReplies.clear();
  hitReply = null;
  escapes = 0;
  layoutListener = null;
});
afterEach(() => {
  cleanup();
  Object.assign(layout, collapsed);
});

// Every mocked promise settles on a microtask; one flush lets the effects' replies land.
const settle = () => act(async () => {});

describe("Panel offline", () => {
  // Offline (M3b acceptance item 9, finding B). With `list_countries` failing and a favourite in
  // the store, the country control stays enabled and the favourites are one choice away: the
  // stores need no network, so the one thing a person can still use offline must not sit behind
  // the countries list. Fails if the select is disabled while the countries are missing (the
  // code before this test), if the ★ toggle is (finding C moved the stores behind it), or if
  // the favourite never reaches the list.
  it("keeps the country control enabled and the favourites reachable with no countries list", async () => {
    render(<Panel />);
    await settle();
    const select = screen.getByRole("combobox") as HTMLSelectElement;
    const star = screen.getByRole("button", { name: "Favourites and recents" }) as HTMLButtonElement;
    expect(select.disabled).toBe(false);
    expect(star.disabled).toBe(false);
    expect(star.getAttribute("aria-pressed")).toBe("false");
    expect(screen.getAllByText(/radio-browser unreachable/).length).toBeGreaterThan(0);
    fireEvent.click(star);
    await settle();
    expect(star.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByText("★ ORBITAL")).toBeTruthy();
    expect(screen.getByText("1 favourites · 0 recents")).toBeTruthy();
  });

  // M4b commit 7: with no band in the layout no map pane is mounted, and with a band the platter
  // is mounted at the band's size with the `− fit +` row at the rect Rust gave. Fails if the
  // pane mounts on a collapsed layout, or if the page sizes the platter itself.
  it("mounts no map pane without a band, and the platter with the controls at the band's rect with one", async () => {
    render(<Panel />);
    await settle();
    expect(document.querySelector("[data-measure='map_band']")).toBeNull();
    cleanup();
    Object.assign(layout, {
      state: "expanded",
      height: 598,
      band: { x: 16, y: 404, width: 328, height: 178, controls: [246, 146, 74, 24] },
    });
    render(<Panel />);
    await settle();
    const platter = document.querySelector("[data-measure='map_band']");
    expect(platter).not.toBeNull();
    expect(screen.getByRole("group", { name: "Zoom" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Zoom in" })).toBeTruthy();
    const root = document.documentElement.style;
    const px = (v: number) => `${v}px`;
    expect(root.getPropertyValue("--map-band-height")).toBe(px(178));
    expect(root.getPropertyValue("--map-controls-x")).toBe(px(246));
  });

  // The acceptance review's A2: the map follows the dropdown — a change of the country select
  // reaches `map_select` with the new code, and back. The acceptance photo of US drawn under a
  // PT dropdown was the `m=paint` driver, which selects RU, US, PT, AQ through `map.select`
  // directly. Fails if the pane's country is not the control's, or if a change does not
  // re-select (killed with the pane's select effect ignoring the prop).
  it("the map follows the dropdown: a country change re-selects, and back (A2)", async () => {
    countriesReply = {
      items: [
        { code: "PT", name: "Portugal", station_count: 300 },
        { code: "US", name: "United States", station_count: 5000 },
      ],
      fetched_at: 0,
      age_secs: 0,
      source: { kind: "fresh" },
      refreshing: false,
    };
    Object.assign(layout, {
      state: "expanded",
      height: 598,
      band: { x: 16, y: 404, width: 328, height: 178, controls: [246, 146, 74, 24] },
    });
    render(<Panel />);
    await settle();
    expect(selects).toEqual(["PT"]);
    const select = screen.getByRole("combobox") as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "US" } });
    await settle();
    expect(selects).toEqual(["PT", "US"]);
    fireEvent.change(select, { target: { value: "PT" } });
    await settle();
    expect(selects).toEqual(["PT", "US", "PT"]);
  });
});

// The dot filter (M4c k+4, decision 3 and the Esc rule): `Panel` holds it, `MapPane`'s click sets
// it from `map.hit`'s reply, `StationList` shows it with the chip.
describe("Panel's dot filter", () => {
  const band = { x: 16, y: 404, width: 328, height: 178, controls: [246, 146, 74, 24] as [number, number, number, number] };
  const row = (cc: string, name: string): Station => ({ ...orbital, uuid: `${cc}-${name}`, name, country_code: cc });
  const listed = (cc: string, names: string[]): ListedStations => ({
    country_code: cc,
    items: names.map((n) => row(cc, n)),
    fetched_at: 0,
    age_secs: 0,
    source: { kind: "fresh" },
    refreshing: false,
  });
  const chip = () => screen.queryByRole("button", { name: "Show all stations" });
  const rows = () =>
    screen
      .queryAllByRole("button")
      .filter((b) => b.getAttribute("aria-label")?.startsWith("Play "))
      .map((b) => b.getAttribute("aria-label")!.slice("Play ".length));
  const esc = () => fireEvent.keyDown(window, { key: "Escape" });

  async function expandedWithPT() {
    countriesReply = {
      items: [
        { code: "PT", name: "Portugal", station_count: 3 },
        { code: "US", name: "United States", station_count: 1 },
      ],
      fetched_at: 0,
      age_secs: 0,
      source: { kind: "fresh" },
      refreshing: false,
    };
    stationsReplies.set("PT", listed("PT", ["A", "B", "C"]));
    stationsReplies.set("US", listed("US", ["K"]));
    Object.assign(layout, { state: "expanded", height: 598, band });
    const r = render(<Panel />);
    await settle();
    expect(rows()).toEqual(["A", "B", "C"]);
    return r;
  }
  // a click on the map: press and release in place, answered by `hitReply`
  async function clickMap(hit: MapHit) {
    hitReply = hit;
    const svg = document.querySelector("svg")!;
    svg.setPointerCapture = () => {};
    fireEvent.pointerDown(svg, { button: 0, buttons: 1, clientX: 50, clientY: 50, pointerId: 1 });
    fireEvent.pointerUp(svg, { clientX: 50, clientY: 50, pointerId: 1 });
    await settle();
  }
  const hitB: MapHit = { uuids: ["PT-B"], n: 1, place: "Lisboa" };

  // 4. A click on a dot filters the list and shows the chip; Esc with a filter clears it and
  //    does not reach Rust; Esc again (no filter) does. The listener is registered once, at
  //    mount, when the filter was null: it must read the filter through a ref. Fails with the
  //    stale closure (the first Esc hides the panel with the filter still set), or if Esc
  //    clears and hides at once.
  it("4. Esc clears a filter without hiding; Esc with none hides", async () => {
    await expandedWithPT();
    await clickMap(hitB);
    expect(rows()).toEqual(["B"]);
    expect(chip()!.textContent).toBe("✕ 1 station · Lisboa");
    esc();
    await settle();
    expect(escapes).toBe(0);
    expect(chip()).toBeNull();
    expect(rows()).toEqual(["A", "B", "C"]);
    esc();
    await settle();
    expect(escapes).toBe(1);
  });

  // 5. Esc with no filter ever set hides at once (the M2c behaviour, unchanged). Fails if Esc is
  //    swallowed when there is nothing to clear.
  it("5. Esc without a filter reaches Rust", async () => {
    await expandedWithPT();
    esc();
    await settle();
    expect(escapes).toBe(1);
  });

  // 6. D8: in the About pane Esc keeps hiding the panel, filter or not — the filter is the
  //    transport pane's, out of sight there. Fails if Esc in About clears an unseen filter
  //    instead of hiding.
  it("6. Esc in the About pane hides even with a filter set", async () => {
    await expandedWithPT();
    await clickMap(hitB);
    expect(chip()).not.toBeNull();
    act(() => layoutListener!({ ...layout, generation: 1, transition: "show", view: "about" }));
    await settle();
    esc();
    await settle();
    expect(escapes).toBe(1);
  });

  // 7. A country change clears the filter (the new country's list shows whole); so does ★
  //    toggled on; and a dot clicked while ★ is on turns ★ off, so the filter applies to the
  //    country the map shows. Fails if a filter outlives a country change or ★, or if a hit
  //    under ★ filters the favourites (an empty list under a chip).
  it("7. a country change clears the filter; ★ on clears it; a hit under ★ returns to the country", async () => {
    await expandedWithPT();
    await clickMap(hitB);
    expect(chip()).not.toBeNull();
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "US" } });
    await settle();
    expect(chip()).toBeNull();
    expect(rows()).toEqual(["K"]);
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "PT" } });
    await settle();
    await clickMap(hitB);
    expect(chip()).not.toBeNull();
    const star = screen.getByRole("button", { name: "Favourites and recents" });
    fireEvent.click(star);
    await settle();
    expect(star.getAttribute("aria-pressed")).toBe("true");
    expect(chip()).toBeNull();
    await clickMap(hitB);
    expect(star.getAttribute("aria-pressed")).toBe("false");
    expect(rows()).toEqual(["B"]);
  });

  // 8. MT's line names the country, not its code: `Panel` passes the countries list's name.
  //    Fails if the pane shows the code.
  it("8. the map pane is given the country's name", async () => {
    await expandedWithPT();
    expect(screen.getByRole("img").getAttribute("aria-label")).toBe("Map of Portugal");
  });
});
