// The popover as a whole, offline (M3b acceptance item 9, finding B — vitest under jsdom,
// `pnpm test`). With `list_countries` failing and a favourite in the store, the country control
// stays enabled and the favourites are one choice away: the stores need no network, so the one
// thing a person can still use offline must not sit behind the countries list. Fails if the
// select is disabled while the countries are missing (the code before this test), if the ★
// toggle is (finding C moved the stores behind it), or if the favourite never reaches the list.
//
// `../api` is mocked whole: nothing reaches Tauri.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PanelLayout, Station } from "../api";
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

const layout: PanelLayout = {
  transition: "show",
  generation: 0,
  view: "transport",
  state: "collapsed",
  width: 360,
  height: 420,
  expandable: true,
};

const offline = { code: "stations", message: "radio-browser unreachable after 3 attempt(s) in 3.01s" };

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
    stations: {
      listCountries: () => Promise.reject(offline),
      listStations: () => Promise.reject(offline),
      listFavourites: () => Promise.resolve([orbital]),
      listRecents: () => Promise.resolve([]),
      addFavourite: () => Promise.resolve(),
      removeFavourite: () => Promise.resolve(true),
    },
    onPanelLayout: listener,
    onState: listener,
    onStreamInfo: listener,
    onMetadata: listener,
    onStationsUpdated: listener,
    onCountriesUpdated: listener,
    onRecentsUpdated: listener,
  };
});

afterEach(cleanup);

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
});
