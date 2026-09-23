// The transport row's reading of playback state (`/code-review` finding 3, 2026-09-23 — vitest
// under jsdom, `pnpm test`). While a session is `reconnecting` Rust owns the recovery — the
// backoff, CLAUDE.md invariant 5 — and a Play here would be a new `play` call: a new session, a
// reset backoff, and a second vote on that session's first `Playing`. The list's row already
// treats `reconnecting` as audible (StationList.test.tsx, its last test); this pins the
// transport to the same reading, one test per surface, so the two cannot drift apart again.
// Fails if `reconnecting` falls through to the Play branch (the code before this test), or if
// Stop — the one thing a person may want during a reconnect — is not offered.
//
// `../api` is mocked whole: nothing reaches Tauri; the state arrives through `onState` as the
// `playback:state` event would.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PlaybackState, Station } from "../api";
import Transport from "./Transport";

const mock = vi.hoisted(() => {
  const stateListeners: ((s: PlaybackState) => void)[] = [];
  return {
    stateListeners,
    onState: (cb: (s: PlaybackState) => void) => {
      stateListeners.push(cb);
      return Promise.resolve(() => {});
    },
  };
});

vi.mock("../api", () => ({
  audio: {
    play: () => Promise.resolve(),
    pause: () => Promise.resolve(),
    resume: () => Promise.resolve(),
    stop: () => Promise.resolve(),
    setVolume: () => Promise.resolve(),
    getPlaybackState: () => Promise.resolve({ kind: "idle" }),
  },
  onState: mock.onState,
}));

const fip: Station = {
  uuid: "FR-FIP",
  name: "FIP",
  url: "https://example.invalid/FIP",
  homepage: "",
  favicon: "",
  country_code: "FR",
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

const button = (name: string) => screen.getByRole("button", { name }) as HTMLButtonElement;
const emitState = (s: PlaybackState) => act(async () => mock.stateListeners.forEach((cb) => cb(s)));

afterEach(() => {
  cleanup();
  mock.stateListeners.length = 0;
});

describe("Transport", () => {
  it("offers no Play while reconnecting — Pause disabled and Stop enabled, as during connecting", async () => {
    render(<Transport station={fip} isFavourite={false} onToggleFavourite={() => {}} />);
    await act(async () => {});
    expect(button("Play").disabled).toBe(false);
    await emitState({ kind: "reconnecting", attempt: 1 });
    expect(screen.queryByRole("button", { name: "Play" })).toBeNull();
    expect(button("Pause").disabled).toBe(true);
    expect(button("Stop").disabled).toBe(false);
    await emitState({ kind: "connecting" });
    expect(screen.queryByRole("button", { name: "Play" })).toBeNull();
    expect(button("Pause").disabled).toBe(true);
    expect(button("Stop").disabled).toBe(false);
  });
});
