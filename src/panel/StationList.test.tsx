// The station list's reply rules (M3b 1b; the project's first TypeScript test — vitest under
// jsdom, `pnpm test`). Each test states what it would have to see to fail:
//
// 1. the wrong-country guard: a slow reply for the previous country landing after the fast one
//    for the current selection is dropped (fails if the late reply replaces the list);
// 2. `stations:updated` landed → the list is requested again; another country's event is not
//    ours (fails if no third request, or if PT's event triggers one);
// 3. `stations:updated` failed → `refreshing…` clears and nothing is requested (fails if the
//    flag stays, or if a request follows);
// 4. a show re-requests the selected country (fails if `showGeneration` changes nothing).
//
// `../api` is mocked whole: the list never reaches Tauri, and every request is a deferred
// promise the test resolves in the order it chooses — which is the point.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ListedStations, Station, StationsUpdated } from "../api";
import StationList from "./StationList";

type Deferred = { cc: string; resolve: (l: ListedStations) => void };

const mock = vi.hoisted(() => {
  const requests: { cc: string; resolve: (l: unknown) => void }[] = [];
  const listeners: ((u: unknown) => void)[] = [];
  return {
    requests,
    listeners,
    listStations: (countryCode: string) =>
      new Promise((resolve) => {
        requests.push({ cc: countryCode, resolve });
      }),
    onStationsUpdated: (cb: (u: unknown) => void) => {
      listeners.push(cb);
      return Promise.resolve(() => {});
    },
  };
});

vi.mock("../api", () => ({
  stations: { listStations: mock.listStations, recordPlayed: () => Promise.resolve() },
  audio: { play: () => Promise.resolve() },
  onStationsUpdated: mock.onStationsUpdated,
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

const requests = (): Deferred[] => mock.requests as unknown as Deferred[];
const resolve = (r: Deferred, l: ListedStations) => act(async () => r.resolve(l));
const emit = (u: StationsUpdated) => act(async () => mock.listeners.forEach((cb) => cb(u)));

afterEach(() => {
  cleanup();
  mock.requests.length = 0;
  mock.listeners.length = 0;
});

describe("StationList", () => {
  it("drops a reply for a country that is no longer selected", async () => {
    const view = render(<StationList selected="PT" showGeneration={0} onPlay={() => {}} />);
    expect(requests().map((r) => r.cc)).toEqual(["PT"]);
    view.rerender(<StationList selected="FR" showGeneration={0} onPlay={() => {}} />);
    expect(requests().map((r) => r.cc)).toEqual(["PT", "FR"]);
    await resolve(requests()[1], listed("FR", ["FIP"]));
    expect(screen.getByText("FIP")).toBeTruthy();
    await resolve(requests()[0], listed("PT", ["Antena 1"]));
    expect(screen.getByText("FIP")).toBeTruthy();
    expect(screen.queryByText("Antena 1")).toBeNull();
    expect(screen.getByText(/1 stations/)).toBeTruthy();
  });

  it("re-requests the selected country when its refresh landed, and ignores another's", async () => {
    render(<StationList selected="FR" showGeneration={0} onPlay={() => {}} />);
    await resolve(requests()[0], listed("FR", ["FIP"], true));
    expect(screen.getByText(/refreshing…/)).toBeTruthy();
    await emit({ country_code: "PT", outcome: "landed" });
    expect(requests()).toHaveLength(1);
    await emit({ country_code: "FR", outcome: "landed" });
    expect(requests().map((r) => r.cc)).toEqual(["FR", "FR"]);
    await resolve(requests()[1], listed("FR", ["FIP", "France Inter"]));
    expect(screen.getByText("France Inter")).toBeTruthy();
    expect(screen.queryByText(/refreshing…/)).toBeNull();
  });

  it("clears the refreshing flag on a failed refresh without asking again", async () => {
    render(<StationList selected="FR" showGeneration={0} onPlay={() => {}} />);
    await resolve(requests()[0], listed("FR", ["FIP"], true));
    expect(screen.getByText(/cached 60 min ago · refreshing…/)).toBeTruthy();
    await emit({ country_code: "FR", outcome: "failed" });
    expect(screen.getByText(/cached 60 min ago$/)).toBeTruthy();
    expect(screen.queryByText(/refreshing…/)).toBeNull();
    expect(requests()).toHaveLength(1);
  });

  it("re-requests the selected country on every show", async () => {
    const view = render(<StationList selected="FR" showGeneration={0} onPlay={() => {}} />);
    await resolve(requests()[0], listed("FR", ["FIP"]));
    view.rerender(<StationList selected="FR" showGeneration={1} onPlay={() => {}} />);
    expect(requests().map((r) => r.cc)).toEqual(["FR", "FR"]);
    view.rerender(<StationList selected="FR" showGeneration={1} onPlay={() => {}} />);
    expect(requests()).toHaveLength(2);
  });
});
