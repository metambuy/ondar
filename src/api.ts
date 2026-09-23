// The only file that talks to Rust. Everything else is rendering.
import { getName, getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { EqBand } from "./bindings/EqBand";
import type { IcyMetadata } from "./bindings/IcyMetadata";
import type { PanelHeight } from "./bindings/PanelHeight";
import type { PanelLayout } from "./bindings/PanelLayout";
import type { PanelView } from "./bindings/PanelView";
import type { PlaybackState } from "./bindings/PlaybackState";
import type { StreamInfo } from "./bindings/StreamInfo";
import type { CacheSource } from "./bindings/CacheSource";
import type { Codec } from "./bindings/Codec";
import type { Country } from "./bindings/Country";
import type { ListedCountries } from "./bindings/ListedCountries";
import type { ListedStations } from "./bindings/ListedStations";
import type { Station } from "./bindings/Station";
import type { StationsUpdated } from "./bindings/StationsUpdated";
import type { CountriesUpdated } from "./bindings/CountriesUpdated";
import type { RefreshOutcome } from "./bindings/RefreshOutcome";

export type { EqBand, IcyMetadata, PanelHeight, PanelLayout, PanelView, PlaybackState, StreamInfo };
export type {
  CacheSource,
  Codec,
  Country,
  CountriesUpdated,
  ListedCountries,
  ListedStations,
  RefreshOutcome,
  Station,
  StationsUpdated,
};

export type OndarError = { code: string; message: string };

export const audio = {
  play: (url: string, stationId: string) => invoke<void>("play", { url, stationId }),
  pause: () => invoke<void>("pause"),
  resume: () => invoke<void>("resume"),
  stop: () => invoke<void>("stop"),
  setVolume: (volume: number) => invoke<void>("set_volume", { volume }),
  setEqGain: (band: number, gainDb: number) => invoke<void>("set_eq_gain", { band, gainDb }),
  getEq: () => invoke<EqBand[]>("get_eq"),
  getPlaybackState: () => invoke<PlaybackState>("get_playback_state"),
};

// The popover itself. `escape` reports a key and `setExpanded` a click on the expand control;
// what either means is decided in Rust (a refused expansion changes nothing, and the page learns
// the outcome from `onPanelLayout`, not from the call). `getLayout` is what the popover last laid
// out — pane, height state, size in points, expandable — the value `onPanelLayout` delivers on
// every effective show and resize, for the page to mirror on mount, as `getPlaybackState` is for
// `onState`. The page is told its height; it never computes it.
export const panel = {
  escape: () => invoke<void>("panel_escape"),
  // Back left the About pane: reported, so the next layout event carries the pane on screen.
  viewBack: () => invoke<void>("panel_view_back"),
  setExpanded: (expanded: boolean) => invoke<void>("panel_set_expanded", { expanded }),
  getLayout: () => invoke<PanelLayout>("get_panel_layout"),
  // The page committed the DOM for this layout generation; Rust completes the visible change
  // (orders a pending show in, or resizes) on it, or on its fallback timer if this never arrives.
  layoutCommitted: (generation: number) =>
    invoke<void>("panel_layout_committed", { generation }),
};

export const onPanelLayout = (cb: (l: PanelLayout) => void): Promise<UnlistenFn> =>
  listen<PanelLayout>("panel:layout", (e) => cb(e.payload));

// Name and version from the bundle (`core:app:default` grants both).
export const app = {
  info: async (): Promise<{ name: string; version: string }> => {
    const [name, version] = await Promise.all([getName(), getVersion()]);
    return { name, version };
  },
};

export const onState = (cb: (s: PlaybackState) => void): Promise<UnlistenFn> =>
  listen<PlaybackState>("playback:state", (e) => cb(e.payload));
export const onStreamInfo = (cb: (i: StreamInfo) => void): Promise<UnlistenFn> =>
  listen<StreamInfo>("playback:stream_info", (e) => cb(e.payload));
export const onMetadata = (cb: (m: IcyMetadata) => void): Promise<UnlistenFn> =>
  listen<IcyMetadata>("playback:metadata", (e) => cb(e.payload));

// The station directory (M3a). Every call is answered by Rust's stations service from its
// SQLite cache; a list carries its provenance (`source`, `age_secs`, `refreshing`), and an
// expired list is served at once while Rust refreshes it — `onStationsUpdated` says how that
// refresh ended: `landed` (ask again) or `failed` (the expired list stays; stop showing
// "refreshing"). The page never fetches, filters or ranks anything itself.
export const stations = {
  listCountries: () => invoke<ListedCountries>("list_countries"),
  listStations: (countryCode: string) => invoke<ListedStations>("list_stations", { countryCode }),
  searchStations: (query: string) => invoke<Station[]>("search_stations", { query }),
  listFavourites: () => invoke<Station[]>("list_favourites"),
  addFavourite: (station: Station) => invoke<void>("add_favourite", { station }),
  removeFavourite: (uuid: string) => invoke<boolean>("remove_favourite", { uuid }),
  listRecents: () => invoke<Station[]>("list_recents"),
  recordPlayed: (station: Station) => invoke<void>("record_played", { station }),
};

// The dev-only measurement harness's one command (M3b 1a; `src/measure.ts`). The command is
// compiled into debug builds only, and the page calls it only when loaded with `?measure=…`,
// which a release build never is.
export const measure = {
  report: (kind: string, fields: string, tPage: number) =>
    invoke<void>("measure_report", { kind, fields, tPage }),
};

export const onStationsUpdated = (cb: (u: StationsUpdated) => void): Promise<UnlistenFn> =>
  listen<StationsUpdated>("stations:updated", (e) => cb(e.payload));
export const onCountriesUpdated = (cb: (u: CountriesUpdated) => void): Promise<UnlistenFn> =>
  listen<CountriesUpdated>("countries:updated", (e) => cb(e.payload));
