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
   keeps the device and `Player`.
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
   (`EngineEvent::Started`). Write, liveness and flag are decided under one lock. **Every session
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
14. **The per-sample DSP path** (`ring.rs`, `eq.rs`) has no allocation, lock, log or panic shape
    outside `#[cfg(test)]`.

**Testing this crate.** The session tests run real sockets on 127.0.0.1 and read every state off
the event stream, in order, never a sample; `ONDAR_TEST_POLL_DELAY_MS`, `ONDAR_TEST_SERVER_DELAY_MS`,
`ONDAR_TEST_LATE_REQUEST_DELAY_MS` and `ONDAR_TEST_TICK_DELAY_MS` stand in for a slow CI runner —
run them before claiming a timing test is sound. Examples: `cargo run -p ondar-audio --example
stall_bench` against `scripts/stall-server.py`; `eq_headroom_sweep` cross-checks the soft-clip.
