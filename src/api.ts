// The only file that talks to Rust. Everything else is rendering.
import { getName, getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { EqBand } from "./bindings/EqBand";
import type { IcyMetadata } from "./bindings/IcyMetadata";
import type { PlaybackState } from "./bindings/PlaybackState";
import type { StreamInfo } from "./bindings/StreamInfo";

export type { EqBand, IcyMetadata, PlaybackState, StreamInfo };

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

// The popover itself. `escape` reports a key; what it means is decided in Rust.
export const panel = {
  escape: () => invoke<void>("panel_escape"),
};

// Which pane the popover shows. Emitted by Rust on every show, from the show reason; the page
// mirrors it and never decides it.
export type PanelView = "about" | "transport";
export const onPanelView = (cb: (v: PanelView) => void): Promise<UnlistenFn> =>
  listen<PanelView>("panel:view", (e) => cb(e.payload));

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
