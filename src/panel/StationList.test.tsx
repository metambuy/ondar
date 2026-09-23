// The station list's reply rules (M3b 1b, extended at commit 4 for the three sources — vitest
// under jsdom, `pnpm test`). Each test states what it would have to see to fail:
//
// 1. the wrong-source guard: a slow reply for the previous country landing after the fast one
//    for the current selection is dropped (fails if the late reply replaces the list);
// 2. `stations:updated` landed → the list is requested again; another country's event is not
//    ours (fails if no third request, or if PT's event triggers one);
// 3. `stations:updated` failed → `refreshing…` clears and nothing is requested (fails if the
//    flag stays, or if a request follows);
// 4. a show re-requests the selected source (fails if `showGeneration` changes nothing);
// 5. the favourites source lists `list_favourites`, and a late reply for the country left
//    behind is dropped (fails if the country's rows show over the favourites);
// 6. `recents:updated` re-requests the recents only while they are shown (fails if a country
//    list is re-requested on it, or if the recents are not);
// 7. a favourite toggle (`storeGeneration`) re-requests the favourites only (fails if it
//    re-requests a country list);
// 8. a click on the playing row does nothing, on the paused row resumes, on another row plays
//    (fails if the row always plays — F6 review F2).
//
// `../api` is mocked whole: nothing reaches Tauri, and every request is a deferred promise
// the test resolves in the order it chooses — which is the point.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ListedStations, Station, StationsUpdated } from "../api";
import StationList from "./StationList";
import type { ListSource } from "./source";

type Deferred = { what: string; resolve: (l: unknown) => void };

const mock = vi.hoisted(() => {
  const requests: { what: string; resolve: (l: unknown) => void }[] = [];
  const stationsListeners: ((u: unknown) => void)[] = [];
  const recentsListeners: (() => void)[] = [];
  const stateListeners: ((s: unknown) => void)[] = [];
  const plays: [string, string][] = [];
  let resumes = 0;
  const defer = (what: string) =>
    new Promise((resolve) => {
      requests.push({ what, resolve });
    });
  return {
    requests,
    stationsListeners,
    recentsListeners,
    stateListeners,
    plays,
    resumed: () => resumes,
    play: (url: string, uuid: string) => {
      plays.push([url, uuid]);
      return Promise.resolve();
    },
    resume: () => {
      resumes += 1;
      return Promise.resolve();
    },
    onState: (cb: (s: unknown) => void) => {
      stateListeners.push(cb);
      return Promise.resolve(() => {});
    },
    listStations: (countryCode: string) => defer(`cc:${countryCode}`),
    listFavourites: () => defer("favourites"),
    listRecents: () => defer("recents"),
    onStationsUpdated: (cb: (u: unknown) => void) => {
      stationsListeners.push(cb);
      return Promise.resolve(() => {});
    },
    onRecentsUpdated: (cb: () => void) => {
      recentsListeners.push(cb);
      return Promise.resolve(() => {});
    },
  };
});

vi.mock("../api", () => ({
  stations: {
    listStations: mock.listStations,
    listFavourites: mock.listFavourites,
    listRecents: mock.listRecents,
  },
  audio: { play: mock.play, resume: mock.resume },
  onStationsUpdated: mock.onStationsUpdated,
  onRecentsUpdated: mock.onRecentsUpdated,
  onState: mock.onState,
}));

function station(name: string, cc: string): Station {
  return {
    uuid: `${cc}-${name}`,
    name,
    url: `https://example.invalid/${name}`,
    homepage: "",
    favicon: "",
    country_code: cc,
    codec: "mp3",
    codec_raw: "MP3",
    bitrate_kbps: 128,
    hls: false,
    video: false,
    votes: 1,
    click_count: 0,
    click_trend: 0,
    geo: null,
    last_check_ok: true,
  };
}

function listed(cc: string, names: string[], refreshing = false): ListedStations {
  return {
    country_code: cc,
    items: names.map((n) => station(n, cc)),
    fetched_at: 0,
    age_secs: 3600,
    source: { kind: "cached" },
    refreshing,
  };
}

const country = (cc: string): ListSource => ({ kind: "country", cc });
const requests = (): Deferred[] => mock.requests as unknown as Deferred[];
const whats = () => requests().map((r) => r.what);
const resolve = (r: Deferred, l: unknown) => act(async () => r.resolve(l));
const emit = (u: StationsUpdated) => act(async () => mock.stationsListeners.forEach((cb) => cb(u)));
const emitRecents = () => act(async () => mock.recentsListeners.forEach((cb) => cb()));
const emitState = (kind: string) => act(async () => mock.stateListeners.forEach((cb) => cb({ kind })));
const list = (source: ListSource, showGeneration = 0, storeGeneration = 0, playingUuid: string | null = null) => (
  <StationList
    source={source}
    showGeneration={showGeneration}
    storeGeneration={storeGeneration}
    onPlay={() => {}}
    playingUuid={playingUuid}
  />
);

afterEach(() => {
  cleanup();
  mock.requests.length = 0;
  mock.stationsListeners.length = 0;
  mock.recentsListeners.length = 0;
  mock.stateListeners.length = 0;
  mock.plays.length = 0;
});

describe("StationList", () => {
  it("drops a reply for a country that is no longer selected", async () => {
    const view = render(list(country("PT")));
    expect(whats()).toEqual(["cc:PT"]);
    view.rerender(list(country("FR")));
    expect(whats()).toEqual(["cc:PT", "cc:FR"]);
    await resolve(requests()[1], listed("FR", ["FIP"]));
    expect(screen.getByText("FIP")).toBeTruthy();
    await resolve(requests()[0], listed("PT", ["Antena 1"]));
    expect(screen.getByText("FIP")).toBeTruthy();
    expect(screen.queryByText("Antena 1")).toBeNull();
    expect(screen.getByText(/1 stations/)).toBeTruthy();
  });

  it("re-requests the selected country when its refresh landed, and ignores another's", async () => {
    render(list(country("FR")));
    await resolve(requests()[0], listed("FR", ["FIP"], true));
    expect(screen.getByText(/refreshing…/)).toBeTruthy();
    await emit({ country_code: "PT", outcome: "landed" });
    expect(requests()).toHaveLength(1);
    await emit({ country_code: "FR", outcome: "landed" });
    expect(whats()).toEqual(["cc:FR", "cc:FR"]);
    await resolve(requests()[1], listed("FR", ["FIP", "France Inter"]));
    expect(screen.getByText("France Inter")).toBeTruthy();
    expect(screen.queryByText(/refreshing…/)).toBeNull();
  });

  it("clears the refreshing flag on a failed refresh without asking again", async () => {
    render(list(country("FR")));
    await resolve(requests()[0], listed("FR", ["FIP"], true));
    expect(screen.getByText(/cached 60 min ago · refreshing…/)).toBeTruthy();
    await emit({ country_code: "FR", outcome: "failed" });
    expect(screen.getByText(/cached 60 min ago$/)).toBeTruthy();
    expect(screen.queryByText(/refreshing…/)).toBeNull();
    expect(requests()).toHaveLength(1);
  });

  it("re-requests the selected source on every show", async () => {
    const view = render(list(country("FR")));
    await resolve(requests()[0], listed("FR", ["FIP"]));
    view.rerender(list(country("FR"), 1));
    expect(whats()).toEqual(["cc:FR", "cc:FR"]);
    view.rerender(list(country("FR"), 1));
    expect(requests()).toHaveLength(2);
  });

  it("lists the favourites and drops the country reply left behind", async () => {
    const view = render(list(country("PT")));
    view.rerender(list({ kind: "favourites" }));
    expect(whats()).toEqual(["cc:PT", "favourites"]);
    await resolve(requests()[1], [station("Jazz FM", "GB")]);
    expect(screen.getByText("Jazz FM")).toBeTruthy();
    expect(screen.getByText(/1 favourites/)).toBeTruthy();
    await resolve(requests()[0], listed("PT", ["Antena 1"]));
    expect(screen.queryByText("Antena 1")).toBeNull();
    expect(screen.getByText("Jazz FM")).toBeTruthy();
  });

  it("re-requests the recents on recents:updated, and only the recents", async () => {
    const view = render(list(country("FR")));
    await resolve(requests()[0], listed("FR", ["FIP"]));
    await emitRecents();
    expect(requests()).toHaveLength(1);
    view.rerender(list({ kind: "recents" }));
    expect(whats()).toEqual(["cc:FR", "recents"]);
    await resolve(requests()[1], [station("FIP", "FR")]);
    await emitRecents();
    expect(whats()).toEqual(["cc:FR", "recents", "recents"]);
  });

  it("re-requests the favourites on a toggle, and only the favourites", async () => {
    const view = render(list(country("FR")));
    await resolve(requests()[0], listed("FR", ["FIP"]));
    view.rerender(list(country("FR"), 0, 1));
    expect(requests()).toHaveLength(1);
    view.rerender(list({ kind: "favourites" }, 0, 1));
    expect(whats()).toEqual(["cc:FR", "favourites"]);
    view.rerender(list({ kind: "favourites" }, 0, 2));
    expect(whats()).toEqual(["cc:FR", "favourites", "favourites"]);
  });

  it("does nothing on the playing row, resumes the paused one, plays another", async () => {
    const view = render(list(country("FR"), 0, 0, "FR-FIP"));
    await resolve(requests()[0], listed("FR", ["FIP", "France Inter"]));
    await emitState("playing");
    fireEvent.click(screen.getByRole("button", { name: "Play FIP" }));
    expect(mock.plays).toEqual([]);
    expect(mock.resumed()).toBe(0);
    await emitState("paused");
    fireEvent.click(screen.getByRole("button", { name: "Play FIP" }));
    expect(mock.plays).toEqual([]);
    expect(mock.resumed()).toBe(1);
    fireEvent.click(screen.getByRole("button", { name: "Play France Inter" }));
    expect(mock.plays).toEqual([["https://example.invalid/France Inter", "FR-France Inter"]]);
    view.unmount();
  });
});
