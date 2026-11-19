---
paths:
  - "src-tauri/crates/ondar-audio/**"
  - "src-tauri/src/commands/audio.rs"
---

# Audio engine rules

```
HTTP (stream-download, bounded; on_progress → Arrivals) → ClockedReader → IcyReader
  → [AdtsReader: HTTP audio/aac*, audio/x-aac*] → rodio::Decoder (Symphonia)   [decode thread]
  → rtrb ring (RING_SECONDS = 2) → UniformSourceIterator (to the output's format)
  → Equalizer (10 × biquad peaking + soft-clip) → Player → MixerDeviceSink
```

Threads: the engine on its own thread (a `std::sync::mpsc` channel and a 100 ms tick); tokio only
for HTTP; decode on a per-session thread. Commands never block on audio. Each invariant's reason is
in ONDAR.md under the section named.

1. **One `AudioEngine`**, created once at startup. Switching stations tears down the session and
   keeps the device. **A `Player` per attach** (0.1.2, F1): the decode thread connects a new one
   to the output's mixer at each attach, and a session's end drops it. **Never
   `Player::clear()`**: it waits without a timeout for the output to pull (the F1 wedge); pinned
   by `no_player_clear_scan` (`ondar-audio/src/lib.rs`). **One output, owned by UID** (beta item 1,
   merge A, 2026-11-19; `output.rs`): opened **pinned** (`device_by_id` + `from_device`, cpal's
   `HalOutput` unit, never `from_default_device()`, whose own follow leaves the process silent on
   the jack's codec: F9), the default watched by the owner's thread (`DefaultWatcher`, 100 ms, run
   from the first open to the idle close), every transition through the pure `decide_output`
   (`output::owner`, the sibling of rule 13). **A switch** = release the old output → its drop
   thread reports `Dropped` → wait 3 s for a device that appeared at the change, none for one that
   existed → open the new default by UID; the old unit is always gone before the new one is created.
   While the owner is not `Open` nothing writes `Playing` (`output_switching`: the attach arm and
   `decide_tick` hold `Buffering`). **The session re-attaches** on the new output without a
   reconnect: its `Player` dropped, a new ring at the new output's format, the stream and its
   buffered bytes kept, so the audio falls `away + 2 s` further behind live at each switch; past
   30 s (`MAX_LAG`) the next switch reconnects live. **A failed start** (`PlayError`, which blocks the
   engine thread ~1.5 s, measured 7 of 7 2026-11-11; commands queue meanwhile) is retried once after
   1 s, then `Error { code: Device }` naming the device, never `Playing`; no output device at all is
   that Error at once. cpal's `DeviceNotAvailable` on the pinned stream is a loss: **until merge B**
   it is a switch to the current default with no wait (F1's outcome); merge B makes it the pause.
   `StreamInvalidated` (a nominal-rate change; cpal pauses the stream) reopens the same device.
   7d's stall WARN (5 s `Playing` with nothing pulled) still reopens, once per episode
   (`stall_reopen`'s latch, cleared only by a pull), through the same path. **Closed after 30 s
   with no session** (`Idle`, `Error`; `OUTPUT_IDLE_CLOSE`, elapsed time, not ticks), the watcher
   stopped with it, so an idle Ondar holds no sleep assertion and polls nothing; the next play
   reopens it. **A session `Paused` for 30 s is ended** (`cancel_session` before the slot empties)
   and its `Play` parked; it stays `Paused`, and Resume replays it as a fresh session
   (`Connecting` → `Buffering` → `Playing`); Stop or a play forgets it. The close writes no state.
   (M5e1; "Beta item 1 A: the output owner")
2. **Ondar owns all reconnects** (own `Backoff` + a fresh `stream::open()`): stream-download's
   internal reconnect splices byte 0 onto a live mount. The one bounded exception: it stays live
   during the decoder build. ("Reconnect ownership and stream timeouts")
3. **HTTP path: `read_timeout` > `retry_timeout`, strictly** (20 s / 5 s), or the download loop
   spins; `stream.rs` clamps and warns. **HLS sets `retry_timeout` above `read_timeout` on
   purpose** (`hls::retry_timeout_for`): `HlsSource` never yields `Err`. Overrides:
   `ONDAR_READ_TIMEOUT_SECS`, `ONDAR_RETRY_TIMEOUT_SECS`, `ONDAR_PREFETCH_BYTES`.
4. **Backoff** 1/2/4/8/16 s, 5 attempts, reset after 30 s stable, then `Error` with the last code;
   each delay stretched to `Retry-After` (cap 30 s). **By cause:** a terminal open error (a non-HTTP
   answer, 401/403/404/410) fails at once only while the session has never opened;
   `UnrecognizedFormat` is terminal only while it has never produced audio.
5. Absent ICY metadata is normal, not an error.
6. **EQ:** 10 ISO bands, Q 1.414, ±12 dB; gains are atomics read every 64 frames; new coefficients
   keep the filter state (no click). No makeup gain; the soft-clip (identity below 0.95, ceiling
   1.0) is the last operation inside `Equalizer` and cannot be bypassed.
7. **The click endpoint, once per session**, on the first `Playing` of the session a `play` began
   (`EngineEvent::Started`). A resume's replay after a paused close is a resume: no `Started`
   (`begin_session(…, true)`). Write, liveness and flag are decided under one lock. **Every session
   event goes through `SessionCtx::emit` → `Shared::emit_from`** (generation-gated); a new event
   must choose, and an event from the audio path (M5's spectrum) must **not** take that lock.
8. **Prefetch from bitrate** (`stream::prefetch_for`): the knee `RING_SECONDS × bitrate / 8`,
   floored at one decoder read, capped at half of `BUFFER_BYTES`.
9. **One output format per process:** every session converts to the sink's format before the EQ
   (`output_chain`). Never give `RingSource` a finite span. ("Defect A")
10. **The decoder build is bounded from its first byte, on network arrival** (`build.rs`):
    `no_bytes` = max(60 s, the prefetch at 10 kbit/s × 1.1); `format` = max(20 s,
    3 × `retry_timeout`) after the first byte → `Starved` if a reconnect completed or the longest
    gap reached `retry_timeout`, else `Format`. One compare-and-swap from the word read to a
    `BOUND_*` phase. ("Defect B")
11. **The ADTS front end** applies to HTTP `audio/aac*` / `audio/x-aac*` only, chosen by
    `OpenedStream::kind`, after `IcyReader`. It realigns to three chained headers below 16 KiB,
    never refuses, and never passes through after an alignment.
12. **HLS is ADTS media playlists only.** MPEG-TS, fMP4, encrypted, byte-range, video-only masters
    and plain M3U are refused terminally as `unsupported_format` after one chain of requests.
    Shoutcast v1 (`ICY 200 OK`) surfaces as `http`.
13. **State changes go through `decide_tick`** (pure, at the bottom of `engine.rs`); do not route
    transitions around it. Buffering supervision lives on the engine thread, never the decode loop.
    **The output's transitions go through `decide_output`** (pure, `output::owner`), the same way.
14. **The per-sample DSP path** (`ring.rs`, `eq.rs`) has no allocation, lock, log or panic shape
    outside `#[cfg(test)]`.
15. **No `log::` / `tracing::` call site in the Source chain the audio callback pulls** (ring
    consumer, EQ adapter, converter): both log sinks (stderr and the daily file) write synchronously.
    Pinned by the source scan `callback_log_scan::the_callback_chain_has_no_log_call_site`
    (`ondar-audio/src/lib.rs`): `ring.rs`, `eq.rs` and `engine.rs::output_chain` outside tests.

**Testing this crate.** The session tests run real sockets on 127.0.0.1 and read every state off
the event stream, in order, never a sample; `ONDAR_TEST_POLL_DELAY_MS`, `ONDAR_TEST_SERVER_DELAY_MS`,
`ONDAR_TEST_LATE_REQUEST_DELAY_MS` and `ONDAR_TEST_TICK_DELAY_MS` stand in for a slow CI runner —
run them before claiming a timing test is sound. Examples: `cargo run -p ondar-audio --example
stall_bench` against `scripts/stall-server.py`; `eq_headroom_sweep` cross-checks the soft-clip.
