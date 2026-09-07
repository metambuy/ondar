# Onda — Project Instructions

> Paste this into the **Project instructions** field of a new Claude Project (name it "Onda").
> Do not reuse the PixelRadio project — this is a different product.
> "Onda" is a working name; change every occurrence when you settle on the real one.

---

## What Onda is

A macOS **menu bar radio player**. It lives in the system tray, opens as a popover, and plays
live internet radio streams from around the world. It is minimalist, native-feeling, and
audio-first.

It is **not** a port of PixelRadio. The only inheritance from PixelRadio is:

- the **station data source** (radio-browser.info) and the query/filter logic learned there,
- the **city database** (`cities.js`, ~500 cities with lat/lng, grouped by ISO alpha-2),
- the **country/region groupings** and the API-etiquette rules (`clickStation` on play).

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
animated state when audio is playing.

## Decided stack (do not relitigate without saying why)

| Layer | Choice | Rationale |
|---|---|---|
| Shell | **Tauri v2** | The point of the exercise |
| Core language | **Rust** | All logic: networking, cache, audio, DSP, tray, window |
| UI | **Vite + React 18 + TypeScript** | Thin view layer only; keeps map work tractable |
| Map rendering | **Leaflet** with a local tile pyramid, `EPSG4326`/equirectangular CRS | Pan/zoom/markers for free; Blue Marble is already plate carrée |
| Map imagery | **NASA Blue Marble NG**, sliced to a WebP tile pyramid, bundled | Public domain, offline, no API key |
| Audio | **Rust**: `stream-download` → `symphonia` decode → `rodio`/`cpal` output | Real EQ, ICY metadata, no CORS, survives webview reload |
| Equalizer | **Rust**, `biquad` peaking filters as a `rodio::Source` adapter | Genuine DSP; testable |
| Spectrum | **Rust**, `rustfft`, pushed to UI as events | UI never touches audio |
| Station API | **Rust** `reqwest` client for radio-browser.info | Server-side etiquette, retries, caching |
| Cache | **SQLite** (`rusqlite`, bundled) | Offline country/station lists, favourites, recents |
| TS types | Generated from Rust (`tauri-specta` / `ts-rs`) | One source of truth for the IPC contract |

### The boundary rule

The webview is a **renderer and an input device**. It holds no business logic, no audio, no
network calls, no persistence. Every meaningful action is a Tauri command; every state change
arrives as a Tauri event. If you find yourself writing a `fetch()` or an `<audio>` element in
TypeScript, stop — it belongs in Rust.

## Constraints and known tradeoffs

- **Offline map resolution is capped.** Blue Marble NG tops out at 500 m/px (21600×21600 tiles)
  and we will likely ship the 2 km/px 21600×10800 composite. Small countries will be shown at
  native resolution and upscale gently past that; this is accepted, not a bug.
- **Bundle size.** The tile pyramid is tens of MB. Target: full app under ~80 MB installed.
  If it exceeds that, drop the deepest zoom level before dropping quality.
- **~30% of radio-browser stations have coordinates.** Map markers are therefore sparse;
  the country dropdown, not the map, is the primary navigation. The map is context and delight.
- **Popover size limits.** Anything that wants a big canvas is the wrong feature for this app.
- **Stream reliability varies.** Reconnect logic and honest error states are a first-class
  feature, not polish.

## API etiquette (non-negotiable)

- Send a descriptive `User-Agent` (`Onda/<version>`) on every radio-browser request.
- Discover servers via the `_api._tcp.radio-browser.info` SRV record, with hardcoded fallbacks;
  do not hammer a single host.
- Call the station-click endpoint when playback actually starts, once per play.
- Cache aggressively (countries: 7 days, station lists: 24 h) and respect the cache offline.

## How to work in this project

- **Plan before code.** For anything larger than a bug fix, produce a short plan and wait for
  approval before writing files.
- **One milestone at a time.** The build plan is sequenced deliberately; do not jump ahead to
  the map before the audio engine works.
- **Ask when the answer changes the architecture.** Do not guess at product decisions.
- **Verify claims.** Community Tauri crates (`tauri-nspanel`, `tauri-plugin-positioner`) move
  fast — check the current version and API against docs.rs/GitHub before writing code against
  them, and say so if reality differs from this document.
- **Keep this document current.** When a decision is made or reversed, update the relevant
  section here rather than burying it in a chat.
- Martín prefers concise, factual answers with reputable sources. Skip the preamble.

## Glossary

- **Popover** — the tray-anchored `NSPanel` window; the whole app UI.
- **Collapsed / Expanded** — the two popover heights.
- **Tile pyramid** — the pre-sliced Blue Marble WebP levels shipped as app resources.
- **Station** — a radio-browser record: uuid, name, url_resolved, codec, bitrate, country, geo.
- **EQ band** — one biquad peaking filter with a fixed centre frequency and adjustable gain.
