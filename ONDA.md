# Onda — project document

*Last updated: 2026-09-11 (Phase 1 block 2 — harness pacing fix, dwell latch, engine
watchdog, prefetch knee, and a re-measured latency table).*

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
| Shell | **Tauri v2** (2.11.x) | The point of the exercise. `macos-private-api` (needed for transparency/vibrancy) is **to enable at M2** — as of 2026-09-10 `Cargo.toml` has `features = []` and `tauri.conf.json` has no `macOSPrivateApi` key. |
| Core language | **Rust** (edition 2024; MSRV 1.91 in `Cargo.toml`, matching `stream-download` 0.24.4's own declared requirement) | All logic: networking, cache, audio, DSP, tray, window |
| UI | **Vite + React 18 + TypeScript** | Thin view layer only; keeps map work tractable |
| Popover window | **`tauri-nspanel`** (git dep, branch `v2.1`, **pinned to a commit rev**) | Not on crates.io; no releases. `v2.1` API = `PanelBuilder` + `tauri_panel!` macro. Do not use the older `v2` branch (`to_panel()` API). |
| Popover positioning | **Tauri `TrayIconEvent::Click { rect }`** first; `tauri-plugin-positioner` 2.3.x (`tray-icon` feature) as fallback | Tauri 2 already gives the tray icon rect; positioner only if its Position enum saves real work. Decide at M2. |
| Vibrancy | **`window-vibrancy`** (tauri-apps) + `transparent: true` | Applies `NSVisualEffectView` material to the panel |
| Map rendering | **Leaflet**, `L.CRS.EPSG4326` | Pan/zoom/markers for free; Blue Marble is already plate carrée. **Tile grid at zoom 0 is 2×1** (360°×180°), so the slicer must emit that layout or a custom `L.CRS` must be defined. |
| Map imagery | **NASA Blue Marble NG**, 2 km/px (21600×10800), sliced to a WebP tile pyramid, bundled | Public domain, offline, no API key. Full level shipped; see bundle size below. |
| Audio | **Rust**: `stream-download` → `IcyReader` → `rodio 0.22` `Decoder` (Symphonia inside) → **`rtrb` ring buffer** → EQ `Source` adapter → `Player` → `MixerDeviceSink` | Real EQ, ICY metadata, no CORS, survives webview reload. rodio 0.22 terms: *Sink→Player*, *OutputStream→MixerDeviceSink*. Symphonia is rodio's default decoder, not a separate stage. **Decoding happens on its own thread** and blocks on a stalled read, so buffering supervision lives on the engine thread (100 ms poll of shared `RingStats`, not the decode loop). Stall recovery is layered: `stream-download` re-requests after `retry_timeout` (default 5 s — set explicitly, do not rely on the default) of no new data; the `reqwest` `read_timeout` (20 s) is a backstop for a reconnect that connects and then hangs; the session-level `Backoff` covers failed connects. **`read_timeout` must stay > `retry_timeout`** — see "Reconnect ownership and stream timeouts". Resume hysteresis is measured as of 2026-09-11: the dwell is latched on entry to `Buffering` (it was previously being cancelled mid-wait), and an engine-level watchdog bounds `Buffering` with no decode progress. |
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

## Repo tooling

- **Remote:** private GitHub repo `metambuy/onda` (created 2026-09-10, visibility `PRIVATE`,
  default branch `main`). `gh` 2.100.0 is installed on the build machine and authenticated as
  `metambuy` over HTTPS; git operations use the same credential. The `m1-done` tag is pushed
  and dereferences to `4b4ee3d`.
- **CI:** `.github/workflows/ci.yml`, on push and pull_request, `macos-latest` only (CoreAudio
  is a hard dependency; there is no Linux/Windows path to test). It runs, in order:
  `pnpm install --frozen-lockfile`; `cargo fmt --all --check`;
  `cargo clippy --all-targets -- -D warnings`; `cargo test --workspace`;
  `git diff --exit-code src/bindings`; `pnpm typecheck`; `pnpm lint`; `cargo build`.
  The bindings check runs immediately after the tests because ts-rs regenerates
  `src/bindings/` during the test run — drifted committed bindings fail there. It is
  `cargo build`, not `pnpm tauri build`: a full bundle is slow and pointless before M6, and
  the tile pyramid must never enter CI. Node and pnpm are pinned to the development
  machine's majors (Node 26; pnpm from `package.json`'s `packageManager`, so the lockfile,
  local installs and CI cannot drift apart).
  **First run green** on `fba1133`, 5m6s cold-cache:
  [run 34495743288](https://github.com/metambuy/onda/actions/runs/34495743288). Its
  `cargo test` step logged `running 34 tests` / `34 passed` — the `--workspace` finding below
  is therefore confirmed on a machine other than the one that wrote it, not just locally.
- **Formatting and MSRV are pinned, not toolchain-dependent:** `src-tauri/rustfmt.toml`
  (`edition = "2024"`, `max_width = 100` — rustfmt's own defaults, written down so a future
  toolchain change cannot silently restyle the tree) and `src-tauri/clippy.toml`
  (`msrv = "1.91"`, matching the workspace `rust-version`).

## Constraints and known tradeoffs

- **ICY metadata lags ~2 s on 64 kbit/s streams, structurally.** The decoder reads 32768 B at
  a time, which is 4.10 s of audio at 64 kbit/s, while the ring holds 2.0 s — so prefetch must
  sit above the knee and the surplus becomes standing lag behind live. Not a tuning miss and
  not fixable by tuning `prefetch_bytes`: the floor is set by a dependency's read size and the
  ceiling by `RING_SECONDS`. Raising `RING_SECONDS` to 4 would close it at the cost of memory
  and of a longer worst-case resume everywhere else. Titles are late on these stations; audio
  is unaffected. See "The prefetch knee".
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

| prefetch | burst | max() | ring_occupancy | predicted lag | **measured lag** | spread | TTFA |
|---|---|---|---|---|---|---|---|
| 0.51 s (8 KB) | 0 | 0.51 s | 0.51 s | 0.000 s | **+0.025 s** | 0.007 | 1.285 s |
| 1.02 s (16 KB) | 0 | 1.02 s | 1.02 s | 0.000 s | **+0.026 s** | 0.004 | 1.281 s |
| 2.05 s (32 KB) | 0 | 2.05 s | 2.00 s | 0.048 s | **+0.194 s** | 0.025 | 2.240 s |
| 3.07 s (48 KB) | 0 | 3.07 s | 2.00 s | 1.072 s | **+1.214 s** | 0.025 | 3.247 s |
| 2.05 s | 4.10 s | 4.10 s | 2.00 s | 2.096 s | **+2.202 s** | 0.032 | 0.136 s |
| 3.07 s | 4.10 s | 4.10 s | 2.00 s | 2.096 s | **+2.196 s** | 0.029 | 0.117 s |
| 3.07 s | 8.19 s | 8.19 s | 2.00 s | 6.192 s | **+6.309 s** | 0.031 | 0.150 s |

Re-measured 2026-09-11 on the corrected harness, five samples per cell over 55 s, every
figure a mean with its spread. The model fits to within 0.15 s at every point, with a
**two regimes, and they split exactly at the `fill_target` floor**: ~0.025 s where TTFA is
fill_target-limited (8 and 16 KB) and ~0.10–0.15 s where it is prefetch-limited (32 KB and
up). Describing both as one constant offset hides the split. **The old special case is gone**: the
small-prefetch point no longer misses by 0.49 s, and the previous "unexplained" residual
there was an artefact of the harness, not of Onda.

**Time-to-first-audio is a separate quantity and does not follow `max(prefetch, burst)`.**
It is the time until `fill_target` (1.0 s, half the ring) has been *decoded* into the ring,
gated by whichever is slower — bytes arriving or decoding:

- **Burst-less:** bytes arrive at 1×, so TTFA ≈ `max(prefetch_secs, 1.0 s) + ~0.25 s`.
  It floors at ~1.28 s: below 16 KB, prefetch stops being the constraint and `fill_target`
  takes over, which is why 8 KB and 16 KB give the same 1.28 s.
- **With a burst:** the bytes are already present, so only decode time remains — 0.12–0.15 s
  regardless of prefetch, against a `max()` of 4.10 s. A burst does not gate startup at all.

Practical read: against a bursting Icecast (64 KB ≈ 4.1 s) the burst dominates `max()` and
prefetch is free either way, so this choice only bites on burst-less servers. There, prefetch
alone sets latency-to-live, and it is bounded below by one decoder read and above by the knee
at `RING_SECONDS × byte_rate` — which at 128 kbit/s are the same ~32 KB. See "The prefetch
knee" for why the lower bound is the binding one and why it is not ours to choose.

### The harness paced 3.57% slow, and it invalidated the table above (found 2026-09-11)

The previous version of that table could not be trusted, and the reason was not in Onda.
`scripts/stall-server.py` paced at **15,429 B/s against a nominal 16,000** — 3.57% slow.
The loop already compensated for `sendall`; the fault was that every deadline was computed
from *"now"*, so `time.sleep` overshoot (1–3 ms on macOS) plus the uncompensated loop head
accumulated against no fixed reference.

The symptom was that measured freshness **decayed monotonically through every run**, ~0.3 s
per 10 s — the client playing at true rate while the server under-delivered, eating the
buffer. So every freshness figure depended on when in the run it was sampled, and the
earlier four-point fit was fitting a moving target. Ruled out at the time: the engine is not
outrunning real time — in steady state the ring sits at 97.2% mean fill with 73% of ticks
at ≥99%, i.e. the decode thread is parked on a full ring as designed.

Fixed by anchoring every deadline to an absolute schedule keyed on bytes sent. Measured
after: **15,996.4 B/s, −0.022%**, and spread within a run fell from ~0.3 s per 10 s to
±0.017 s over 40 s.

### The prefetch knee (measured 2026-09-11)

`prefetch_bytes` is 32 KB, bounded from **both** sides — and at 128 kbit/s the two bounds
coincide, which is the only reason a single constant works at all:

- **Floor: one decoder read, 32768 B.** Below it the decode thread asks for a full read,
  `stream-download` holds only part of it, and the thread blocks for the remainder at 1× while
  the ring drains. Measured at 16 KB, burst-less: the stream underruns **2.0 s into playback
  with no network fault at all** (`Playing` at 2.118 s, `Buffering` at 4.133 s, six seconds
  before the injected stall), then repeats — an audible dropout after every station start. A
  fixed byte count; it does not scale with bitrate.
- **Ceiling: the knee, `RING_SECONDS × byte_rate`,** ~31.25 KB at 128 kbit/s. Above it the
  surplus head start cannot fit in the ring and becomes a standing offset behind live: +0.194 s
  at 32 KB against +1.214 s at 48 KB.

An earlier version of this note justified 32 KB by freshness parity with 16 KB and by margin
over `fill_target`. Both were wrong: the freshness figures were pre-harness-fix, and dropping
below `fill_target` costs nothing — 8 KB and 16 KB give identical TTFA (1.285 / 1.281 s)
because the fill target simply takes over as the constraint. The right number for the wrong
reason, and it survives only because floor and ceiling coincide here.

Jitter tolerance is the third axis and it favours more prefetch — time from stall to underrun
is +0.41 s at 32 KB against +1.40 s at 48 KB. 32 KB is the deliberate trade at the knee, not
the maximum.

**The two bounds scale differently, so no fixed value is right everywhere:**

| bitrate | one decoder read | knee | with a fixed 32 KB |
|---|---|---|---|
| 64 kbit/s | 4.10 s | 16 KB | floor **exceeds** the knee; ~2.1 s of lag is structural |
| 128 kbit/s | 2.05 s | 31 KB | they coincide; optimal |
| 320 kbit/s | 0.82 s | 78 KB | safe, but 0.82 s of buffer where the ring holds 2.0 s |

**M3 refinement:** radio-browser's station record carries `bitrate`, making
`prefetch_bytes = max(one_decoder_read, RING_SECONDS × bitrate / 8)` computable before `open`.
The `max` is load-bearing — the knee alone starves the decoder at 64 kbit/s.

**The first term is pinned to a dependency's internal behaviour.** `onda-audio` never
constructs a `MediaSourceStream`; rodio 0.22.2 does it internally over symphonia-core 0.5.5 and
chooses the 32768 B read size. It is not a property of decoding and not ours to set, so it
**must be re-verified on any rodio or symphonia bump**. A bump that raises it reintroduces the
spontaneous underruns above, silently. `crates/onda-audio/src/icy.rs` records the largest
observed read (`MAX_OBSERVED_READ`) and `examples/stall_bench.rs` fails loudly when it exceeds
the effective prefetch — verified to fire at 16 KB and stay quiet at 32 KB.

### The unstable dwell was selected and then cancelled (found and fixed 2026-09-11)

`DWELL_TICKS_UNSTABLE` never took effect. `decide_tick` derived the dwell from
`recent_underruns` on every tick, and the underrun window prunes on a rolling basis, so an
underrun ageing out of `UNDERRUN_WINDOW_TICKS` *midway through a wait* dropped the count
below `UNDERRUN_PANIC_COUNT` and collapsed the dwell from 40 ticks to 10. `ready_ticks` had
been accumulating throughout, so the resume fired immediately. Instrumented at a 13.3 s
stall cadence:

```
t=316 Playing   recent=3 dwell=40 ready_ticks=0    <- underrun, 40 selected
t=357 Buffering recent=3 dwell=40 ready_ticks=1    <- data back, dwell counting
t=360 Buffering recent=3 dwell=40 ready_ticks=4
t=361 Buffering recent=2 dwell=10 ready_ticks=5    <- prune. dwell collapses
t=367 Playing   recent=2 dwell=10 ready_ticks=11   <- resumes at 11, not 40
```

Effective dwell 1.1 s against the 4.0 s chosen. At that cadence **every** eligible wait was
cut short — the unstable path was 100% ineffective — and at ~5 s cadence it was intermittent
*within a single run*, so the effective dwell was 10 or 40 by coincidence of where the
window boundary fell. Fixed by latching the dwell on entry to `Buffering` and holding it for
that wait (`dwell_for_tick`). `UNDERRUN_WINDOW_TICKS` stays 300 and `UNDERRUN_PANIC_COUNT`
stays 3 — they were never the problem, and tuning them without the latch would only move the
cadence at which the collapse appears. Verified after: resumes at the 3rd/4th/5th underruns
take 4.63/4.86/4.93 s, previously 2.08/2.60/3.10 s.

`RESUME_FILL_NUM/DEN` (3/4) is **observed, not optimised**: refilling to 75% cost 0.36–2.1 s
depending on residual buffer. It is a `const` rather than env-overridable, so sweeping it
needs a rebuild per value, and the cost of being slightly wrong is a marginally longer or
shorter silence after recovery — a taste judgement, not a correctness one, with nothing
downstream depending on the value. Not swept, deliberately.

### The engine-level watchdog (added 2026-09-11)

`Buffering` had no upper bound, and one reachable case never ended. Against `--range reject`
— a 416 to a retried range request — `stream-download` swallows the error into an infinite
retry and never returns it to the decode thread, so **nothing in Onda could fail the
session**: measured 1,585,143 log lines / 382 MB in a 40 s run, 1,585,101 of them identical,
stuck in `Buffering` for the whole run with no `Reconnecting` and no `Error`. The external
`Backoff` was unreachable.

The watchdog bounds time in `Buffering` with no decode progress, then fails the session into
the existing `Backoff` + a fresh `stream::open()`. Progress is `RingStats::pushed`, advanced
by the decode thread at a point only reached after a successful push — not `fill`, which the
audio callback zeroes on underrun and the decode thread stops updating while blocked, making
a stalled network and a healthy-but-starved ring indistinguishable.

Threshold is `max(3 × retry_timeout, 15 s)`, derived at runtime because `retry_timeout` is
env-overridable and a constant would silently become wrong when raised. The bound to clear is
the longest *legitimate* no-progress interval — bytes stop, the idle reconnect fires after
`retry_timeout`, the new connection delivers — measured at ~5.0 s.

Paused sessions are **exempt outright**, not given a longer threshold: a paused session stops
pulling, the ring fills, the decode thread parks on a full ring, and `pushed` stops advancing
on a perfectly healthy connection. A pause is unbounded, so a longer threshold postpones a
false positive without removing one.

Verified on the identical scenario: **64 lines / 10 KB**, session failed at 15.0 s of no
progress, `Reconnecting { attempt: 1 }`, recovery, cycle repeating under `Backoff`. The spin
does not start at all, because the watchdog fires before the point at which it got going.
That is better than the worst case rather than a guarantee of it — a spin that starts earlier
is still bounded only by the window, ~15 s at ~39,600 lines/sec, five times over under
`Backoff`.

### `Buffering` fires before the buffer is exhausted (measured 2026-09-11)

At the instant of underrun, **1.59 s and 1.63 s of already-delivered audio had not been
played** (two configurations, repeatable to 0.04 s). The underrun is a *ring* event, not a
pipeline-empty event: the decode thread is parked in a read while `stream-download` still
holds data. So `Buffering` is more responsive than buffer arithmetic predicts, and the
watchdog's progress signal must not be confused by it — which is why it counts pushes rather
than inferring from `fill`.


### Bare `cargo test` runs nothing (found 2026-09-10)

`src-tauri/Cargo.toml` declares a `[workspace]` *and* a real `[package]` at the same root.
For that layout cargo's default scope is the root package alone, not all members — the
"defaults to every member" behaviour belongs to *virtual* manifests (a `[workspace]` with no
`[package]`). So from `src-tauri`:

| Invocation | What actually runs |
|---|---|
| `cargo test` | the `onda` package only — **0 tests**, exit 0, no warning |
| `cargo test --workspace` | 34 tests (all in `onda-audio`) |
| `cargo test -p onda-audio` | the same 34 |

It reports success either way, which is what made it survive this long. **Implication worth
stating plainly: any "cargo test passes" claim made before 2026-09-10 needs re-reading against
which invocation was used.** `README.md`'s instructions were fine — they have always said
`cargo test --workspace` and `cargo test -p onda-audio`. The *verification ritual* in
`CLAUDE.md` and `docs/BUILD_PLAN.md` was not: it said bare `cargo test`, so any milestone
check that followed the ritual as written — M1's included — proved nothing about the audio
engine. Both files are corrected as of 2026-09-10 and CI uses `--workspace`.

Corollary: the 34 is itself worth pinning down, because 28 is the number you get counting
`#[test]` in source. The other 6 are generated — ts-rs's `#[ts(export)]` expands to an
`export_bindings_<type>` test per exported type, which is the mechanism that writes
`src/bindings/`. `cargo test -p onda-audio -- --list` is the authority.

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
