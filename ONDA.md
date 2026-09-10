# Onda — project document

*Last updated: 2026-09-08 (M1 stall/reconnect testing pass — see "Reconnect ownership
and stream timeouts").*

## What Onda is

A macOS **menu bar radio player**. It lives in the system tray, opens as a popover, and plays
live internet radio streams from around the world. It is minimalist, native-feeling, and
audio-first.

It is **not** a port of PixelRadio. The only inheritance from PixelRadio is:

- the **station data source** (radio-browser.info) and the query/filter logic learned there,
- the **city database** (`cities.js`, ~500 cities with lat/lng, grouped by ISO alpha-2),
- the **country/region groupings** and the API-etiquette rules (`clickStation` on play),
- a **supplementary coordinate database** for stations that radio-browser leaves ungeolocated
  (files to be supplied by Martín at M4).

Everything else — rendering, aesthetic, audio pipeline, state model — is rebuilt from scratch.

## Why it exists

Practical exercise for Martín's apprenticeship at CrabNebula (official Tauri partner). The
project must therefore be *representative of real Tauri work*: Rust-heavy core, correct
cross-boundary design, a signed and notarised macOS build at the end. Learning value beats
speed. When there is a choice between "quick JS hack" and "correct Rust with a clear
boundary", choose the Rust.

## Product shape

**Collapsed popover** (default, ~360×420):

- Now Playing: station name, country flag, bitrate/codec, live ICY title when available
- Transport: play/pause, volume, favourite
- Country dropdown (searchable) → station list for that country
- Small level meter / spectrum strip
- Expand affordance

**Expanded popover** (~360×720, grows *in place* — it stays a menu bar popover, never a
separate window):

- Satellite map section revealed above the station list
- Map is framed on the currently selected country
- Pan and zoom inside the frame; for large countries (Russia, USA, Brazil) the viewport
  moves rather than shrinking the country to illegibility
- Station markers on the map; click a marker to play
- Equalizer panel (toggle between map and EQ, or EQ as a second expanded pane)

## Aesthetic

Satellite imagery, not pixel art. The reference points are Apple's own menu bar surfaces:
translucent material (`NSVisualEffectView` vibrancy), SF Symbols or a matching icon set,
SF Pro type, 8pt spacing rhythm, subtle depth, no drop shadows on flat elements, full
light/dark support driven by the system appearance.

Map imagery is **NASA Blue Marble Next Generation**, bundled offline. Country outlines are
drawn as thin translucent strokes over the imagery; the selected country gets a brighter
stroke and a faint inner glow. Consider shipping the **Black Marble (night lights)** variant
as the dark-mode map — city lights are a natural fit for a radio product and it solves dark
mode elegantly.

The tray icon is a monochrome template image so macOS tints it correctly; it gets a subtle
animated state when audio is playing. Note: there is no animated-template-image API; this is
a small frame sequence swapped on a timer via `TrayIcon::set_icon`.

## Decided stack (do not relitigate without saying why)

| Layer | Choice | Rationale / notes |
|---|---|---|
| Shell | **Tauri v2** (2.11.x) | The point of the exercise. `macos-private-api` feature enabled (needed for transparency/vibrancy). |
| Core language | **Rust** (edition 2024; MSRV nominally 1.85 in `Cargo.toml`, **not actually achievable** — see Verified versions) | All logic: networking, cache, audio, DSP, tray, window |
| UI | **Vite + React 18 + TypeScript** | Thin view layer only; keeps map work tractable |
| Popover window | **`tauri-nspanel`** (git dep, branch `v2.1`, **pinned to a commit rev**) | Not on crates.io; no releases. `v2.1` API = `PanelBuilder` + `tauri_panel!` macro. Do not use the older `v2` branch (`to_panel()` API). |
| Popover positioning | **Tauri `TrayIconEvent::Click { rect }`** first; `tauri-plugin-positioner` 2.3.x (`tray-icon` feature) as fallback | Tauri 2 already gives the tray icon rect; positioner only if its Position enum saves real work. Decide at M2. |
| Vibrancy | **`window-vibrancy`** (tauri-apps) + `transparent: true` | Applies `NSVisualEffectView` material to the panel |
| Map rendering | **Leaflet**, `L.CRS.EPSG4326` | Pan/zoom/markers for free; Blue Marble is already plate carrée. **Tile grid at zoom 0 is 2×1** (360°×180°), so the slicer must emit that layout or a custom `L.CRS` must be defined. |
| Map imagery | **NASA Blue Marble NG**, 2 km/px (21600×10800), sliced to a WebP tile pyramid, bundled | Public domain, offline, no API key. Full level shipped; see bundle size below. |
| Audio | **Rust**: `stream-download` → `IcyReader` → `rodio 0.22` `Decoder` (Symphonia inside) → **`rtrb` ring buffer** → EQ `Source` adapter → `Player` → `MixerDeviceSink` | Real EQ, ICY metadata, no CORS, survives webview reload. rodio 0.22 terms: *Sink→Player*, *OutputStream→MixerDeviceSink*. Symphonia is rodio's default decoder, not a separate stage. **Decoding happens on its own thread** and blocks on a stalled read, so buffering supervision lives on the engine thread (100 ms poll of shared `RingStats`, not the decode loop). Stall recovery is layered: `stream-download` re-requests after `retry_timeout` (default 5 s — set explicitly, do not rely on the default) of no new data; the `reqwest` `read_timeout` (20 s) is a backstop for a reconnect that connects and then hangs; the session-level `Backoff` covers failed connects. **`read_timeout` must stay > `retry_timeout`** — see "Reconnect ownership and stream timeouts". Resume hysteresis (fill threshold in seconds of audio + dwell, longer dwell after repeated underruns): values TBD; `scripts/stall-server.py` now exists — tuning is its own pass. |
| Equalizer | **Rust**, `biquad` peaking filters as a `rodio::Source` adapter | Genuine DSP; unit-testable without audio hardware |
| Spectrum | **Rust**, `rustfft`, pushed to UI as events | UI never touches audio |
| Station API | **Rust** `reqwest` client for radio-browser.info; **`hickory-resolver`** for the SRV lookup | `reqwest` cannot do SRV; a resolver crate is required |
| Cache | **SQLite** (`rusqlite`, bundled) | Offline country/station lists, favourites, recents |
| TS types | **`ts-rs`** | Stable. `tauri-specta` is still 2.0.0-rc.x (rc.25; docs.rs build failing) — revisit when it ships 2.0. |

### Verified versions (2026-09-08, from `Cargo.lock`)

**Locked today** (M1 actually depends on these — exact versions from `cargo tree`/`Cargo.lock`,
not crates.io lookups):

- `tauri` 2.11.5, `tauri-build` 2.6.3
- `wry` 0.55.1, `tao` 0.35.3 — **correction:** previously recorded here as 0.56/0.36; those
  were never checked against a lockfile.
- `rodio` 0.22.2, `stream-download` 0.24.4 (no ICY support — we strip it ourselves),
  `rtrb` 0.4.0, `biquad` 0.6.0
- `ts-rs` 12.0.1
- `reqwest` 0.13.4 — **single version in the tree** (`cargo tree -d`, checked 2026-09-08):
  used directly by `onda-audio` and pulled in identically by `stream-download`. No
  duplicate-major bloat.
- `thiserror` — two majors present: `2.0.20` (used directly by `onda`, `onda-audio`, `tauri`,
  `rodio`, `stream-download`, `ts-rs`) and `1.0.69` (transitive, via `json-patch` ←
  `tauri-utils`). Not a problem — 1.x/2.x coexist fine — noted for completeness.
- `window-vibrancy` 0.6.0 — **already in the tree, transitively, via `tauri` itself.** Nothing
  in Onda's own `Cargo.toml` depends on it yet. This list previously recorded `0.8.0` here,
  from a crates.io lookup never checked against a lockfile — when M2 adds it explicitly,
  re-verify the current crates.io version rather than trusting either number.
- `tracing-subscriber` 0.3.23 (env-filter; replaces `env_logger` — its `init()` installs
  a `LogTracer` itself, so `tracing-log` is not a direct dependency)

**Not yet a dependency** (M2+; last-checked crates.io/GitHub state, *not* locked — re-verify
before actually adding):

- `tauri-nspanel`: branch `v2.1`, git only, no releases. Pinned commit rev not yet
  researched — do this when M2 actually starts, not before (a rev pinned now would likely be
  stale by then).
- `tauri-plugin-positioner` 2.3.4, `hickory-resolver` 0.26.2
- `tauri-specta` 2.0.0-rc.25 (not adopted)

**MSRV correction:** `Cargo.toml` declares `rust-version = "1.85"` at the workspace level, but
`stream-download` 0.24.4's own manifest declares `rust-version = "1.91.0"`. So 1.85 has never
actually been buildable since `stream-download` was added — it only went unnoticed because the
toolchain here is 1.98. `Cargo.toml` itself is unchanged in this pass (docs-only commit); the
workspace `rust-version` should be bumped (or the constraint reconsidered) before anyone tries
to build with an older toolchain. `README.md`'s "Rust ≥ 1.85" prerequisite line has the same
issue and needs the same correction.

Re-verify at the start of each milestone that touches these; update this list.

### The boundary rule

The webview is a **renderer and an input device**. It holds no business logic, no audio, no
network calls, no persistence. Every meaningful action is a Tauri command; every state change
arrives as a Tauri event. If you find yourself writing a `fetch()` or an `<audio>` element in
TypeScript, stop — it belongs in Rust.

## Constraints and known tradeoffs

- **Offline map resolution is capped.** Blue Marble NG tops out at 500 m/px (eight 21600×21600
  tiles) and we ship the 2 km/px 21600×10800 composite. Small countries will be shown at
  native resolution and upscale gently past that; this is accepted, not a bug.
- **Bundle size.** The full 2 km/px level is ~233 Mpx; as lossy WebP that is roughly 45–90 MB
  for the base level plus ~33% for the rest of the pyramid. **Decision (2026-09-07): an
  installed size above 100 MB is acceptable.** Measure real numbers at M4 and record them
  here. If Black Marble is also shipped, expect roughly double.
- **~30% of radio-browser stations have coordinates.** Map markers are therefore sparse;
  the country dropdown, not the map, is the primary navigation. The map is context and
  delight. The PixelRadio supplementary coordinate DB will raise coverage (M4).
- **Popover size limits.** Anything that wants a big canvas is the wrong feature for this app.
- **Stream reliability varies.** Reconnect logic and honest error states are a first-class
  feature, not polish.
- **Shoutcast v1 servers** (`ICY 200 OK` status line) are rejected by hyper/reqwest and surface
  as an `Http` error. Rare via radio-browser's `url_resolved`; measure at M3 before deciding
  whether a raw-socket fallback is worth it.
- **EQ has no headroom management.** A `+12 dB` band boost is ×4 linear gain with no
  limiter or soft-clip in `Equalizer`. Measured headless: 0.7 peak input, band 1
  (62.5 Hz) at +12 dB → output peak 2.787
  (`eq.rs::boost_near_full_scale_exceeds_unity_no_limiting`); 0.95 peak at +4 dB →
  1.5058 (`boost_at_broadcast_realistic_level_also_exceeds_unity`); a flat EQ passes a
  1.5-peak input through unchanged (`already_above_unity_input_passes_through_unclamped`).
  No clamp or saturating cast exists between the EQ and the device: `Player::append`
  adds only `.amplify()` as a value transform (rodio `amplify.rs:63-65`, a pure
  multiply), and `biquad` 0.6's `DirectForm2Transposed` step (`lib.rs:175-181`) is a
  pure IIR multiply-accumulate. Onda opens the sink without `.with_sample_format()`
  (`engine.rs:387`), but that does **not** mean rodio defaults to `f32`:
  `from_device` calls `.with_supported_config()` (rodio `stream.rs:339-352`), which
  takes whatever format CoreAudio reports for the device. macOS's HAL is natively
  float32 so this is `f32` in practice, but it is a runtime fact, not a guarantee in
  Onda's or rodio's source. If a device did report `I16`, the cast (rodio
  `stream.rs:531`) is the last step before the device callback — still downstream of
  `.amplify()`, so it changes nothing about the volume argument below. Onda's own code
  does no int cast either way. Two consequences worth stating: the app's `Vol` slider is applied *after*
  the EQ, so lowering it scales the entire EQ output and can hold peaks under ±1.0; and
  clipping requires the boosted band to contain real energy — a high-passed talk stream
  has almost nothing at 63 Hz, so +12 dB there is near-inaudible on it while music at
  the same setting is not. Still not shippable: broadcast radio is limited to sit near
  full scale, so a boosted band that does match the content will clip it. Likely fixes
  for M5: makeup attenuation scaled to the summed positive band gains, or a soft-clip
  stage after the EQ `Source` adapter — decide then.

- **The audible grit reported on 2026-09-08 was external to Onda** (resolved 2026-09-09).
  It was heard through a WiFi speaker, i.e. downstream of a 48→44.1 kHz resample, a
  float-to-16-bit conversion, and the speaker's own DSP and bass protection. Two
  independent results rule out the signal path. Arithmetic: the grit persisted at ¼
    `Vol`, where the +4 dB case peaks at 0.376 and cannot clip at any stage. By ear:
  re-tested through wired headphones at 63 Hz +12 dB on both the same 128 kbps talk
  stream (no audible change — a high-passed talk signal has almost no energy at 63 Hz,
  so this test alone proves little) and a music station (bass clearly boosted, no grit). The clipping measured above is real, but it was never the explanation for
  what was heard, and the earlier claim that near-full-scale content "clips downstream
  at the sink" is withdrawn as a description of an audible fault.
- **Signing/notarisation** requires a paid Apple Developer ID certificate. Assumed yes;
  decision deferred to M6. Tauri's bundler handles it from env vars once the cert exists.

### Reconnect ownership and stream timeouts (measured 2026-09-08, M1)

**Onda owns all reconnects.** `stream-download`'s internal reconnect is a file-download
feature: it decides Range-vs-plain-GET from the `Accept-Ranges` of the *first* response,
cached at `HttpStream::new()`. A live Icecast mount doesn't send that header, so every
internal reconnect is a bare GET whose byte 0 is spliced onto the writer's current
position — an audible content jump with no state change to explain it (measured: the
engine never left `Playing`). It is reached only on a *hang*, never on an error or a
clean EOF, so in practice `close`/`reset` recover through our own Backoff and a fresh
`stream::open()`, which restarts at live and surfaces `Reconnecting`/`Buffering` properly.

**`read_timeout` must stay strictly greater than `retry_timeout`.** Defaults 20 s / 5 s.
If `read_timeout` fires first, `reqwest`'s `ReadTimeoutBody` never clears its elapsed
sleep (`body.rs` ~336-360), so the body yields `Err(TimedOut)` on every poll forever;
`stream-download`'s `handle_bytes` logs and returns `Continue` with no backoff, giving a
CPU-bound spin (measured: 4.15M log lines in 40 s, stuck in `Buffering`, no recovery).
Both values are env-overridable (`ONDA_READ_TIMEOUT_SECS`, `ONDA_RETRY_TIMEOUT_SECS`),
so `stream.rs` clamps `read_timeout` to `retry_timeout * 2` and warns if the invariant
is violated.

**Two upstream bugs in `stream-download` 0.24.4, not reported upstream** (decided
2026-09-09 — the findings are recorded here rather than filed; revisit if either
starts costing us): `handle_reconnect` tests only the outer `timeout` result, so a
failed reconnect (e.g. a 416 to a retried range request) still fires `on_reconnect`
and leaves the loop polling a dead stream — a spin, measured at 125,253 log lines /
28.8 MB over 14 s; and the fast-`Err` path above, which is jointly `reqwest`
0.13.4's `ReadTimeoutBody` not clearing its elapsed sleep on the error return
(`async_impl/body.rs:351-353` skips the reset at `:358`) and `stream-download`'s
`handle_bytes` returning `Continue` with no backoff on repeated `Err` — measured at
4.15M log lines in 40 s. Neither is fixed in Onda. A post-M1 pass should add an
engine-level watchdog (max time in `Buffering` with no bytes arriving → fail the
session → external reconnect), which covers both and anything upstream breaks next.

**Latency-to-live ≈ max(prefetch_secs, burst_secs)** — how far behind the live
broadcast the audio actually is, *not* how long until playback starts, and *not*
bounded by `RING_SECONDS` (contrary to the old `ring.rs` comment). Mechanism: prefetch
and/or burst hand the decoder a head start of already-downloaded audio; once playback
runs at 1× and the network settles into real-time pacing, that head start is never
clawed back, so it becomes a standing offset behind live. `IcyMetadata` is measured
earlier than the corresponding audio is heard, because the decode thread can read
ahead of the audio callback by whatever fits in the ring —
`ring_occupancy = min(RING_SECONDS, max(prefetch_secs, burst_secs))`, not always the
full 2 s. So `IcyMetadata` lag (what the harness actually measures) is latency-to-live
minus that occupancy, and adding `ring_occupancy` back to the measured `IcyMetadata`
lag reconstructs the audible figure:

| prefetch | burst | max(prefetch,burst) | ring_occupancy | measured `IcyMetadata` lag | audible (reconstructed) |
|---|---|---|---|---|---|
| 3.07 s | 0 | 3.07 s | 2.0 s | 1.26–1.46 s | 3.3–3.5 s |
| 3.07 s | 4.10 s | 4.10 s | 2.0 s | 1.8–2.0 s | 3.8–4.0 s |
| 3.07 s | 8.19 s | 8.19 s | 2.0 s | 6.2–6.3 s | 8.2–8.3 s |
| 0.51 s | 0 | 0.51 s | 0.51 s | 0.49–0.50 s | ~1.0 s |

The reconstructed audible figure tracks `max(prefetch_secs, burst_secs)` across all
four points with no special case. The `IcyMetadata`-lag formula alone
(`max(...) − ring_occupancy`) needs one: at the fourth point it floors at 0 against a
measured 0.49–0.50 s — a 0.49 s miss, the same order as an earlier flat-floor reading's
0.8 s miss on that same point, so this point alone doesn't cleanly favour either model.
That the miss (0.49 s) is close to `prefetch_secs` itself (0.51 s) at that point is
unexplained — not attributed to connect/decode-startup overhead or anything else here.

Harness resolution: `--icy-metaint 4000` at 16000 B/s quantises title-boundary timing
to 0.25 s steps, and (measured − predicted) across the four `IcyMetadata`-lag points
runs −0.3 s to +0.49 s. Nothing finer than ~0.5 s is resolvable with this harness as
configured.

Practical read: against a bursting Icecast (64 KB ≈ 4.1 s), the 48 KB prefetch (3.07 s)
is free — the burst already dominates `max()`. Against a burst-less server, prefetch
alone sets latency-to-live, making `prefetch_bytes` a direct dial there: 16 KB would
buy back roughly 2 s in that case. Input for the hysteresis tuning pass, not a change
now.

## API etiquette (non-negotiable)

- Send a descriptive `User-Agent` (`Onda/<version>`) on every radio-browser request.
- Discover servers via the `_api._tcp.radio-browser.info` SRV record (`hickory-resolver`),
  with hardcoded fallbacks; do not hammer a single host.
- Call the station-click endpoint when playback actually starts, once per play.
- Cache aggressively (countries: 7 days, station lists: 24 h) and respect the cache offline.

## Milestones (sequenced; one at a time)

1. **M1 — Scaffold + audio engine.** Tauri + Vite/React/TS; Rust audio module
   (`stream-download` → rodio decoder → EQ adapter → output); play/pause/stop/volume
   commands; ICY title events; reconnect and error states; EQ biquad unit tests. Plain
   test window, no tray.
2. **M2 — Tray + NSPanel popover.** `tauri-nspanel` pinned rev, vibrancy, template tray
   icon, collapsed/expanded resize in place, positioning from tray rect.
3. **M3 — Station API + SQLite cache + country/station UI.** SRV discovery, `User-Agent`,
   click endpoint, cache TTLs, favourites/recents.
4. **M4 — Map.** Tile slicing, Leaflet CRS, country outlines, markers, PixelRadio
   coordinate DB merge. Record measured bundle size.
5. **M5 — Spectrum + EQ UI, tray animation, polish.**
6. **M6 — Signing, notarisation, DMG.**

## How to work in this project

- **Plan before code.** For anything larger than a bug fix, produce a short plan and wait for
  approval before writing files.
- **One milestone at a time.** The build plan is sequenced deliberately; do not jump ahead to
  the map before the audio engine works.
- **Ask when the answer changes the architecture.** Do not guess at product decisions.
- **Verify claims.** Community Tauri crates move fast — check the current version and API
  against docs.rs/GitHub before writing code against them, and say so if reality differs
  from this document. Update "Verified versions" when you do.
- **Keep this document current.** When a decision is made or reversed, update the relevant
  section here rather than burying it in a chat.
- Martín prefers concise, factual answers with reputable sources. Skip the preamble.

## Glossary

- **Popover** — the tray-anchored `NSPanel` window; the whole app UI.
- **Collapsed / Expanded** — the two popover heights.
- **Tile pyramid** — the pre-sliced Blue Marble WebP levels shipped as app resources.
- **Station** — a radio-browser record: uuid, name, url_resolved, codec, bitrate, country, geo.
- **EQ band** — one biquad peaking filter with a fixed centre frequency and adjustable gain.
