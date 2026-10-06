// The popover as a whole, offline (M3b acceptance item 9, finding B — vitest under jsdom,
// `pnpm test`). With `list_countries` failing and a favourite in the store, the country control
// stays enabled and the favourites are one choice away: the stores need no network, so the one
// thing a person can still use offline must not sit behind the countries list. Fails if the
// select is disabled while the countries are missing (the code before this test), if the ★
// toggle is (finding C moved the stores behind it), or if the favourite never reaches the list.
//
// Also (M4b commit 7): with no band in the layout no map pane is mounted, and with a band the
// platter is mounted at the band's size with the `− fit +` row at the rect Rust gave (fails if
// the pane mounts on a collapsed layout, or if the page sizes the platter itself).
//
// Also (the acceptance review's A2, 2026-10-06): the map follows the dropdown — a change of the
// country select reaches `map_select` with the new code, and back (fails if the pane's country is
// not the control's, or if a change does not re-select). The acceptance photo of US drawn under a
// PT dropdown was the `m=paint` driver, which selects RU, US, PT, AQ through `map.select` directly.
//
// `../api` is mocked whole: nothing reaches Tauri.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ListedCountries, PanelLayout, Station } from "../api";
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
      escape: () => Promise.resolve(),
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
    },
    onPanelLayout: listener,
    stations: {
      listCountries: () => (countriesReply ? Promise.resolve(countriesReply) : Promise.reject(offline)),
      listStations: () => Promise.reject(offline),
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
});
afterEach(() => {
  cleanup();
  Object.assign(layout, collapsed);
});

// Every mocked promise settles on a microtask; one flush lets the effects' replies land.
const settle = () => act(async () => {});

describe("Panel offline", () => {
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
