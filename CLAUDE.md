# CLAUDE.md — Onda

> Place at the repository root. Claude Code reads this automatically at session start.
> Keep it under ~250 lines; move detail into `docs/` and link it.

## What this repo is

**Onda** — a macOS menu bar internet radio player. Tauri v2 shell, Rust core, thin
React/TypeScript view layer. Satellite map (bundled NASA Blue Marble), Rust audio pipeline
with a real equalizer, station data from radio-browser.info.

Target platform is **macOS only** (Apple Silicon first, Intel via universal binary at
release). Do not add Windows/Linux code paths.

## The one rule

**The webview is a renderer, not an application.**

TypeScript may: render, animate, handle input, hold ephemeral view state.
TypeScript may **not**: make network requests, decode or play audio, persist data, hold
domain state that outlives a render.

Every action → `invoke()` a Tauri command. Every state change → a Tauri event from Rust.
If a feature seems to need `fetch`, `<audio>`, `localStorage`, or a `setInterval` polling
loop in TS, the design is wrong; move it to Rust and emit an event.

## Layout

```
onda/
├── src-tauri/
│   ├── src/
│   │   ├── main.rs              # entry, tray setup, activation policy
│   │   ├── app/                 # window/panel lifecycle, tray, positioning
│   │   ├── audio/
│   │   │   ├── mod.rs           # AudioEngine: owns the output stream + control channel
│   │   │   ├── stream.rs        # stream-download + reconnect/backoff
│   │   │   ├── decode.rs        # symphonia wiring
│   │   │   ├── eq.rs            # biquad band bank as a rodio Source adapter
│   │   │   ├── spectrum.rs      # rustfft analysis -> event payloads
│   │   │   └── icy.rs           # ICY metadata -> NowPlaying
│   │   ├── stations/
│   │   │   ├── client.rs        # radio-browser HTTP client (SRV discovery, UA, retries)
│   │   │   ├── model.rs         # Station, Country, filters
│   │   │   └── cache.rs         # SQLite cache + TTL
│   │   ├── geo/                 # country bboxes, city db, marker projection
│   │   ├── store.rs             # favourites, recents, settings (SQLite)
│   │   ├── commands.rs          # #[tauri::command] surface — thin, no logic
│   │   ├── events.rs            # event names + payload structs (single source of truth)
│   │   └── error.rs             # AppError + thiserror
│   ├── resources/
│   │   ├── tiles/               # generated Blue Marble pyramid (gitignored, built by xtask)
│   │   ├── countries.geojson    # simplified outlines
│   │   └── cities.json          # ported from PixelRadio
│   ├── Cargo.toml
│   └── tauri.conf.json
├── src/                         # React + TS
│   ├── main.tsx
│   ├── bindings.ts              # GENERATED — do not edit by hand
│   ├── ipc.ts                   # typed wrappers over invoke/listen
│   ├── components/
│   ├── map/                     # Leaflet setup, CRS, tile layer, markers
│   └── styles/
├── tools/
│   └── tiles/                   # vips-based tile pyramid build script
└── docs/
    ├── build-plan.md
    ├── audio-pipeline.md
    └── map-pipeline.md
```

## Commands

```bash
pnpm install                 # frontend deps
pnpm tauri dev               # run the app (this is the dev loop)
pnpm tauri build             # release bundle
pnpm typecheck               # tsc --noEmit
pnpm lint                    # eslint
cargo fmt --all              # from src-tauri/
cargo clippy --all-targets -- -D warnings
cargo test                   # Rust unit tests
pnpm test                    # vitest (view logic only)
pnpm gen:bindings            # regenerate src/bindings.ts from Rust types
./tools/tiles/build.sh       # regenerate the map tile pyramid (needs libvips)
```

Before declaring any task done: `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`,
`pnpm typecheck`, and `pnpm tauri dev` launching without a console error.

## Rust conventions

- **No `unwrap()` / `expect()` / `panic!` in any code reachable from a command or the audio
  thread.** Return `Result<T, AppError>`. `expect()` is acceptable only in `main.rs` setup
  where failure means the app genuinely cannot run.
- One error type, `AppError` (`thiserror`), serialisable to the frontend with a stable
  `kind` discriminant so the UI can branch on it (`Network`, `StreamUnavailable`,
  `Decode`, `Cache`, `Internal`).
- `tracing` for logging, never `println!`. Audio-thread logging is rate-limited.
- Async I/O on Tokio; **audio runs on its own dedicated thread**, never on the async runtime.
  The engine is driven by a `crossbeam`/`std::sync::mpsc` command channel — the command
  handler sends a message and returns immediately, it never blocks on audio.
- No allocation, locking, or logging inside the per-sample DSP path.
- Commands are thin: parse → call a module function → map the error. Domain logic lives in
  modules and is unit-testable without Tauri.
- Public types crossing the IPC boundary derive `Serialize`, `Deserialize`, `specta::Type`,
  and use `#[serde(rename_all = "camelCase")]`.

## TypeScript conventions

- Strict mode on. No `any`. Import IPC types from `bindings.ts`.
- Function components + hooks. Local state by default; a single small Zustand store only for
  state genuinely shared across panes (popover expansion, selected country).
- Rust is the source of truth for player state — the UI mirrors events, it does not
  optimistically maintain a parallel model.
- CSS modules, no framework. Colours and spacing come from CSS custom properties in
  `styles/tokens.css`; both light and dark values must be defined there.
- Every interactive element is keyboard reachable and labelled.

## macOS specifics

- Activation policy `Accessory` and `LSUIElement` in Info.plist — no Dock icon, no menu bar
  menus.
- The popover is an `NSPanel` (via `tauri-nspanel`), non-activating, hides on resign-key,
  positioned under the tray item (`tauri-plugin-positioner`, `TrayCenter`).
- Tray icon must be a **template image** (`set_icon_as_template(true)`) so it tints with the
  menu bar.
- Window vibrancy via Tauri's `macos_private_api` / effects config; the popover background is
  never a flat opaque colour.
- Expanding the popover resizes the window and repositions it against the tray anchor in the
  same frame — no visible jump.
- These community crates move fast. **Check the current API on docs.rs before writing against
  `tauri-nspanel` or `tauri-plugin-positioner`**, and flag it if their API differs from what
  this file assumes.

## Audio pipeline invariants

1. One `AudioEngine`, owned by Tauri state, created once at startup.
2. Playback graph: `stream-download` (HTTP, buffered) → `symphonia` decode →
   `EqSource` (biquad bank) → volume → `rodio` sink → `cpal` device.
3. Switching stations tears down the source but keeps the output stream and device alive.
4. Network failures use exponential backoff (1s, 2s, 4s, 8s, cap 30s) with a visible
   `Reconnecting` state; give up after 5 attempts and surface a `StreamUnavailable` error.
5. ICY metadata is parsed from the same stream (`icy-metadata`) and emitted as
   `now-playing-changed`; absence of metadata is normal, not an error.
6. EQ gains are applied without clicks (smooth coefficient interpolation) and a pre-amp
   guards against clipping when multiple bands are boosted.
7. Call the radio-browser click endpoint exactly once, when the first audio frame plays.

## Map invariants

- Tiles are generated at build time by `tools/tiles/build.sh` and shipped as app resources;
  they are **never** fetched from the network at runtime.
- Blue Marble is equirectangular (plate carrée), so lat/lng → pixel is linear. Use Leaflet's
  `EPSG4326` CRS; do not reimplement the projection.
- The map is always framed by the selected country's bounding box (from
  `countries.geojson`), fitted with padding; zoom is clamped so a country can never be
  smaller than 40% of the viewport nor zoomed past the available tile resolution.
- Panning is clamped to the country bbox plus a small margin — the user cannot get lost.
- Markers come from Rust (station lat/lng), already deduplicated and capped (~200 per country).

## Known risks — check these before trusting this file

1. **`tauri-nspanel` / `tauri-plugin-positioner` API drift.** Both are community crates that
   move faster than this document. Verify the current API on docs.rs/GitHub before writing
   against them (M1). If `tauri-nspanel` is unmaintained for Tauri v2, fall back to a
   borderless always-on-top window with manual blur handling and record the decision in
   `docs/`.
2. **HLS and redirect-chain streams.** `stream-download` handles plain HTTP/Icecast streams,
   not HLS playlists. Some radio-browser entries are `.m3u8`. Detect these and surface a
   clear `StreamUnavailable`/unsupported error rather than hanging. Count how many stations
   this affects before deciding whether HLS support earns its own milestone.
3. **Bundle size vs map depth.** The tile pyramid is the largest single asset. If the app
   exceeds ~80 MB installed, drop the deepest zoom level before dropping image quality.
4. **Sparse station coordinates.** Only ~30% of radio-browser stations have lat/lng, so map
   markers are thin in some countries. The country dropdown is the primary navigation; the
   map must never become the only way to reach a station.
5. **Stream reliability.** Dead and mislabelled streams are common. Honest error states and
   reconnect behaviour are a feature, not polish — do not paper over them with spinners.
6. **Build host.** macOS-only targets: building, signing and notarising all require macOS.
   Do not attempt to produce a release bundle from Linux.

## Working style

- **Plan first.** For anything beyond a one-file fix, propose the plan and wait.
- **One milestone per session** (see `docs/build-plan.md`). Do not start the next milestone's
  work early; do not leave a milestone with failing checks.
- **Small commits**, conventional-commit style (`feat(audio): …`, `fix(map): …`), each one
  building and passing checks on its own.
- Never add a dependency without saying what it does and why the std/existing option is not
  enough.
- Do not add features that are not in the current milestone. Write them down instead.
- If a documented approach turns out to be wrong, stop and say so before improvising.
