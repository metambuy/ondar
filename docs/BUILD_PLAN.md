# Onda — Build Plan

Sequenced so that every milestone ends with something you can run and hear or see.
Each milestone is one Claude Code session (or two). Do not start a milestone before the
previous one's exit criteria pass.

**Ordering logic:** shell → data → *audio* → EQ → map → polish. Audio comes before the map
deliberately: it is the hardest, most Rust-heavy part and the thing that makes the app a
radio rather than a picture of one. The map is the reward.

---

## M0 — Foundation

**Goal:** an empty Tauri v2 app that builds, lints, and runs.

- [ ] `pnpm create tauri-app` → React + TypeScript + Vite, pnpm
- [ ] Pin Tauri v2 versions; `rustfmt.toml`, `clippy.toml`, `.editorconfig`
- [ ] `AppError` + `thiserror`, `tracing` + `tracing-subscriber` initialised
- [ ] `tauri-specta` wired; `pnpm gen:bindings` produces `src/bindings.ts`
- [ ] Scripts: `typecheck`, `lint`, `test`, `gen:bindings`
- [ ] GitHub Actions: fmt, clippy `-D warnings`, cargo test, tsc, vitest
- [ ] `CLAUDE.md` + `docs/build-plan.md` committed

**Exit:** `pnpm tauri dev` opens a window; CI green on a trivial PR.

---

## M1 — Menu bar shell

**Goal:** it behaves like a real macOS menu bar app before it does anything useful.

- [ ] `ActivationPolicy::Accessory` + `LSUIElement` — no Dock icon
- [ ] Tray icon as a **template image**, light/dark correct
- [ ] `tauri-nspanel`: convert the main window to a non-activating `NSPanel`
- [ ] `tauri-plugin-positioner` (`TrayCenter`) — popover anchors under the tray item, correct
      on multi-monitor and with the notch
- [ ] Hide on resign-key / `Esc`; toggle on tray click; right-click tray → quit/preferences
- [ ] Vibrancy background, rounded corners, no window chrome
- [ ] Collapsed/expanded height states with a smooth resize + reposition (empty panes for now)
- [ ] `tauri-plugin-single-instance`
- [ ] Design tokens (`styles/tokens.css`) for both appearances

**Exit:** popover opens and closes like Bartender/Fantastical; expand/collapse animates with
no jump; nothing flickers on a second monitor.

**Risk:** `tauri-nspanel` API drift. Verify against its current v2 docs first; if it is
unmaintained, fall back to a borderless always-on-top window with manual blur handling and
record the decision.

---

## M2 — Station data layer (Rust)

**Goal:** the app knows about every country and its stations, offline-tolerant.

- [ ] `stations::client` — `reqwest` (rustls), SRV discovery of `_api._tcp.radio-browser.info`
      with hardcoded fallback hosts, `User-Agent: Onda/<version>`, timeouts, 3 retries with
      backoff across *different* hosts
- [ ] Endpoints: `/json/countries`, `/json/stations/bycountrycodeexact/{cc}`,
      `/json/url/{uuid}` (click), `/json/stations/search`
- [ ] `stations::model` — `Station`, `Country`; normalise `url_resolved`, codec, bitrate
- [ ] Filtering: drop `lastcheckok == 0`, drop bitrate 0, dedupe by name+url, sort by
      votes then clicktrend, cap per country
- [ ] `stations::cache` — SQLite (`rusqlite`, bundled feature); countries TTL 7 d,
      station lists TTL 24 h; serve stale on network failure
- [ ] Port `cities.js` → `resources/cities.json`; load into `geo`
- [ ] Commands: `list_countries`, `list_stations(country_code)`, `search_stations(query)`
- [ ] Tests: response parsing from recorded fixtures, filter/dedupe logic, cache TTL

**Exit:** country dropdown populated from Rust; selecting a country lists real stations;
airplane mode still shows the last cached lists.

---

## M3 — Audio engine (Rust) — the core milestone

**Goal:** it plays radio. Budget the most time here.

- [ ] `AudioEngine` owned by Tauri state; dedicated audio thread + command channel
      (`Play(station)`, `Pause`, `Resume`, `Stop`, `SetVolume`, `SetEqGains`)
- [ ] `stream-download` HTTP source with a bounded buffer, feeding `symphonia` via `rodio`
- [ ] Handle MP3, AAC/ADTS, Ogg/Vorbis, Opus; unsupported codec → clean `Decode` error
- [ ] State machine: `Idle → Connecting → Buffering → Playing → Paused`, plus
      `Reconnecting` and `Error`; emitted as `player-state-changed`
- [ ] Reconnect with exponential backoff (1/2/4/8/…30 s, 5 attempts)
- [ ] Volume with a smoothed ramp (no zipper noise); persisted
- [ ] ICY metadata via `icy-metadata` → `now-playing-changed` (title, optional artwork URL)
- [ ] Fire the radio-browser click endpoint once, on first audio frame
- [ ] Commands + events wired to the collapsed UI: play/pause, now playing, error states
- [ ] Tests: state machine transitions, backoff schedule, ICY parsing

**Exit:** click a station → audio within ~2 s; pull the network cable → `Reconnecting` →
recovery when it returns; switching stations does not click, pop, or leak a thread.

**Risk:** some streams are HLS or redirect chains that `stream-download` will not handle.
Detect and surface an honest "unsupported stream" error rather than hanging; note the
station count affected and decide later whether HLS is worth a milestone.

---

## M4 — Equalizer

**Goal:** a real EQ, in Rust, that you can hear.

- [ ] `audio::eq::EqSource` — a `rodio::Source` wrapper holding N `biquad` peaking filters
      per channel. Start with 5 bands (60 / 250 / 1k / 4k / 12k Hz), design for 10
- [ ] Coefficients recomputed on gain change and **interpolated over ~30 ms** — no clicks
- [ ] Pre-amp / soft limiter so boosted bands cannot clip
- [ ] Presets: Flat, Voice, Bass, Bright, Late Night; custom gains persisted per app (not per
      station, unless you decide otherwise)
- [ ] `audio::spectrum` — `rustfft` on the post-EQ buffer, ~30 Hz of log-binned magnitudes
      emitted as `spectrum-tick`; **throttled and dropped if the UI lags**
- [ ] UI: vertical sliders, preset picker, spectrum strip in the collapsed view
- [ ] Tests: a 1 kHz sine through a +12 dB 1 kHz band gains ~12 dB and neighbouring bands do
      not; a flat EQ is bit-transparent within tolerance

**Exit:** moving a slider changes the sound immediately and silently; the spectrum reacts to
music; CPU stays low (single digits) while playing.

---

## M5 — Satellite map

**Goal:** the expanded pane shows the selected country on real satellite imagery.

**5a — Asset pipeline (build time, not runtime)**

- [ ] Download NASA Blue Marble NG: the 21600×10800 (2 km/px) monthly composite; pick one
      month (August reads best globally) — plus optionally Black Marble night lights for dark mode
- [ ] `tools/tiles/build.sh` using **libvips**:
      `vips dzsave world.tif tiles --layout google --suffix .webp[Q=80] --tile-size 256`
- [ ] Keep levels that fit the size budget; record total MB in `docs/map-pipeline.md`
- [ ] Ship as Tauri `resources`; serve through the asset protocol (CSP allowlisted)
- [ ] Simplified `countries.geojson` (Natural Earth 50 m, `mapshaper -simplify 8%`) with
      per-country bounding boxes precomputed into a JSON side-file

**5b — Renderer**

- [ ] Leaflet with `L.CRS.EPSG4326`, local `L.tileLayer` over the bundled pyramid
- [ ] `map.fitBounds(countryBbox, {padding})` on country selection
- [ ] `maxBounds` = country bbox + margin; `minZoom`/`maxZoom` clamped to available levels
- [ ] Country outline overlay: all countries faint, selected country bright + inner glow
- [ ] Station markers (from Rust, capped ~200/country), hover tooltip, click → play
- [ ] Playing station's marker pulses
- [ ] Attribution line: "Imagery: NASA Earth Observatory" in the about panel
- [ ] Dark mode: swap tile layer (night lights) or apply a tuned filter

**Exit:** pick Portugal → the country fills the frame at good resolution; pick Russia → you
can pan across it at a readable zoom without losing the frame; the map never requests the
network.

---

## M6 — Product polish

- [ ] Favourites and Recently Played (SQLite), reachable from the collapsed view
- [ ] Global keyboard shortcut to toggle the popover; `Space` play/pause when focused
- [ ] Media key support and macOS **Now Playing** integration (`MPNowPlayingInfoCenter` via
      `objc2`) — station and ICY title on the lock screen and in Control Centre
- [ ] Launch at login (`tauri-plugin-autostart`)
- [ ] Sleep timer, and pause/resume on system sleep/wake and on audio device change
- [ ] Empty, loading, offline and error states designed, not defaulted
- [ ] Onboarding: first launch picks a country from the system locale and plays a top station

---

## M7 — Ship it

- [ ] App icon set, DMG background, `tauri.conf.json` bundle metadata
- [ ] Apple Developer ID signing + **notarisation** in CI (`APPLE_*` secrets)
- [ ] Universal binary (aarch64 + x86_64)
- [ ] `tauri-plugin-updater` with a signed update feed
- [ ] Crash/error reporting decision (opt-in or none — no silent telemetry)
- [ ] README with screenshots, credits (NASA, radio-browser.info), licence

**Exit:** a notarised DMG that a stranger can open on a clean Mac without Gatekeeper warnings.

---

## Verification per milestone

Every milestone closes with the same ritual:

1. `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
2. `pnpm typecheck && pnpm lint && pnpm test`
3. A manual smoke pass against that milestone's exit criteria, with the app actually running
4. Update `docs/` with anything learned that contradicts the plan
5. Commit and tag `m0`, `m1`, …

---

## Open questions to resolve before M5

1. **Name.** "Onda" is a placeholder. Decide before M7 (bundle identifier, signing).
2. **Blue Marble month** — one fixed month, or all twelve switching with the calendar
   (twelve months multiplies the bundle; almost certainly one).
3. **Night-lights dark mode** — worth the extra tile set, or a filter on the day imagery?
4. **Deepest zoom level** — how much bundle size are you willing to spend? This is the single
   biggest lever on download size.
5. **HLS streams** — support them (adds `hls` handling in Rust) or exclude them from results?

## Time shape (rough, part-time)

M0–M1 the first week, M2 quick, **M3 is the long pole** — expect it to take as long as M0–M2
combined. M4 is short if M3 is clean. M5 splits evenly between the asset pipeline and the
renderer. M6–M7 always take longer than they look, mostly notarisation.
