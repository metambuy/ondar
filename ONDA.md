# Onda — project document

*Last updated: 2026-09-08 (M1 buffering-supervision fix — see README "M1 deviations").*

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
| Core language | **Rust** (MSRV 1.85, edition 2024) | All logic: networking, cache, audio, DSP, tray, window |
| UI | **Vite + React 18 + TypeScript** | Thin view layer only; keeps map work tractable |
| Popover window | **`tauri-nspanel`** (git dep, branch `v2.1`, **pinned to a commit rev**) | Not on crates.io; no releases. `v2.1` API = `PanelBuilder` + `tauri_panel!` macro. Do not use the older `v2` branch (`to_panel()` API). |
| Popover positioning | **Tauri `TrayIconEvent::Click { rect }`** first; `tauri-plugin-positioner` 2.3.x (`tray-icon` feature) as fallback | Tauri 2 already gives the tray icon rect; positioner only if its Position enum saves real work. Decide at M2. |
| Vibrancy | **`window-vibrancy`** (tauri-apps) + `transparent: true` | Applies `NSVisualEffectView` material to the panel |
| Map rendering | **Leaflet**, `L.CRS.EPSG4326` | Pan/zoom/markers for free; Blue Marble is already plate carrée. **Tile grid at zoom 0 is 2×1** (360°×180°), so the slicer must emit that layout or a custom `L.CRS` must be defined. |
| Map imagery | **NASA Blue Marble NG**, 2 km/px (21600×10800), sliced to a WebP tile pyramid, bundled | Public domain, offline, no API key. Full level shipped; see bundle size below. |
| Audio | **Rust**: `stream-download` → `IcyReader` → `rodio 0.22` `Decoder` (Symphonia inside) → **`rtrb` ring buffer** → EQ `Source` adapter → `Player` → `MixerDeviceSink` | Real EQ, ICY metadata, no CORS, survives webview reload. rodio 0.22 terms: *Sink→Player*, *OutputStream→MixerDeviceSink*. Symphonia is rodio's default decoder, not a separate stage. **Decoding happens on its own thread** and can block for up to `read_timeout` (20 s) on a dead connection, so **buffering supervision lives on the engine thread** (100 ms poll of shared `RingStats`, not the decode loop) — a stalled stream still reports `Buffering` even while decode itself is stuck in a read. |
| Equalizer | **Rust**, `biquad` peaking filters as a `rodio::Source` adapter | Genuine DSP; unit-testable without audio hardware |
| Spectrum | **Rust**, `rustfft`, pushed to UI as events | UI never touches audio |
| Station API | **Rust** `reqwest` client for radio-browser.info; **`hickory-resolver`** for the SRV lookup | `reqwest` cannot do SRV; a resolver crate is required |
| Cache | **SQLite** (`rusqlite`, bundled) | Offline country/station lists, favourites, recents |
| TS types | **`ts-rs`** | Stable. `tauri-specta` is still 2.0.0-rc.x (rc.25; docs.rs build failing) — revisit when it ships 2.0. |

### Verified versions (2026-09-07)

- `tauri` 2.11.5, `tauri-build` 2.6.3, `wry` 0.56, `tao` 0.36
- `tauri-nspanel`: branch `v2.1`, git only — record the pinned rev in `Cargo.toml` and here
- `tauri-plugin-positioner` 2.3.4, `window-vibrancy` 0.8.0, `hickory-resolver` 0.26.2
- `rodio` 0.22.2, `stream-download` 0.24.4 (no ICY support — we strip it ourselves), `rtrb` 0.4.0, `biquad` 0.6.0
- `ts-rs` 12.0.1
- `tauri-specta` 2.0.0-rc.25 (not adopted)

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
- **Signing/notarisation** requires a paid Apple Developer ID certificate. Assumed yes;
  decision deferred to M6. Tauri's bundler handles it from env vars once the cert exists.

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
