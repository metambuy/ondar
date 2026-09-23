# Ondar — project document

*Last updated: 2026-09-22 (M3a acceptance run and its two fixes — see "M3a: the station
directory, built", the acceptance paragraph; the Shoutcast v1 constraint corrected; instrument
instance thirteen). Previously 2026-09-22 (M3a built on branch `m3a`; rusqlite 0.40.2 verified). Previously 2026-09-21 (M3 Step 0 measured — see "M3 Step 0: the live data, measured"; the geo
share, the API etiquette line and the verified `hickory-resolver` version updated from it; four
instrument instances added). Previously 2026-09-21 (bundle identifier settled: `eu.ondar.radio` — see "Bundle identifier:
`eu.ondar.radio`"; socket names re-measured). Previously 2026-09-21 (M2 complete — M2d merged
`2a9bae9`, tagged `m2d-done`; the milestone list updated). Previously 2026-09-21 (M2d acceptance results, the About-pane decision, item 6
recorded as unmeasured, and instrument instance eight — see "M2d: resize in place"). Previously 2026-09-18
(M2d Step 0 measured and decisions D2–D4 recorded beside D1 — see
"M2d: resize in place — Step 0 measured, D2–D4 decided" and "M2d: the expanded height is capped to
the work area"). Previously 2026-09-17 (M2c — chrome
and input: Esc, tray menu, rounded corners, single instance, design tokens, the M1 bench retired
into the popover; Step 0 measurements and the gate-2 decisions — see "M2c: chrome and input,
measured").*

## What Ondar is

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

**Expanded popover** (~360×720 nominal, **subject to the D1 cap** — the height is capped to the
work area of the display the tray icon is on, so "expanded" is a function of the display; see
"M2d: the expanded height is capped to the work area". Grows *in place* — it stays a menu bar
popover, never a separate window):

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
| Shell | **Tauri v2** (2.11.x) | The point of the exercise. `macos-private-api` (needed for transparency/vibrancy) and `tray-icon` **enabled at M2a** (2026-09-15, `36d50b8`), with `"macOSPrivateApi": true` in `tauri.conf.json` — tauri-build requires the key to match the feature. |
| Core language | **Rust** (edition 2024; MSRV 1.91 in `Cargo.toml`, matching `stream-download` 0.24.4's own declared requirement) | All logic: networking, cache, audio, DSP, tray, window |
| UI | **Vite + React 18 + TypeScript** | Thin view layer only; keeps map work tractable |
| Popover window | **`tauri-nspanel`** (git dep, branch `v2.1`, **pinned to a commit rev**) | Not on crates.io; no releases. `v2.1` API = `PanelBuilder` + `tauri_panel!` macro. Do not use the older `v2` branch (`to_panel()` API). Pinned `rev = c9ec213…` since M2a; see "Verified versions". |
| Popover positioning | **Tauri `TrayIconEvent::Click { rect }`** — decided 2026-09-12, `tauri-plugin-positioner` **not needed** | `rect.position` is already the top-left corner in top-left-origin physical pixels, matching Tauri's own convention: no flip, no conversion. Since M2a the centred position is clamped into the work area (`NSScreen.visibleFrame`) of the display under the icon. See "M2a: the tray path, measured". |
| Vibrancy | **Tauri's own `set_effects`** + `PanelBuilder::transparent(true)` *and* `with_window(\|w\| w.transparent(true))` | `window-vibrancy` is **not** a direct dependency: Tauri wraps it. Applying to the real `OndarPanel` **measured by view tree on 2026-09-15; no visual confirmation.** The spike's measurement was on a window already converted back to a `TaoWindow` — see "The spike measured a reverted `TaoWindow`". |
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
  used directly by `ondar-audio` and pulled in identically by `stream-download`. No
  duplicate-major bloat.
- `thiserror` — two majors present: `2.0.20` (used directly by `ondar`, `ondar-audio`, `tauri`,
  `rodio`, `stream-download`, `ts-rs`) and `1.0.69` (transitive, via `json-patch` ←
  `tauri-utils`). Not a problem — 1.x/2.x coexist fine — noted for completeness.
- `window-vibrancy` 0.6.0 — **already in the tree, transitively, via `tauri` itself.** Nothing
  in Ondar's own `Cargo.toml` depends on it yet. This list previously recorded `0.8.0` here,
  from a crates.io lookup never checked against a lockfile. M2 does not add it — Tauri's
  `set_effects` wraps it; see the `tauri-nspanel` spike note below.
- `tracing-subscriber` 0.3.23 (env-filter; replaces `env_logger` — its `init()` installs
  a `LogTracer` itself, so `tracing-log` is not a direct dependency)

**Not yet a dependency** (M2+; last-checked crates.io/GitHub state, *not* locked — re-verify
before actually adding):

- `tauri-plugin-positioner` — **not needed.** `TrayIconEvent::Click` carries `rect`, whose
  `position` is already the tray icon's top-left corner in top-left-origin physical pixels
  (`tray-icon` 0.24.2 `platform_impl/macos/mod.rs:515` `get_tray_rect` flips macOS's
  bottom-left origin and subtracts the icon height before handing it over). That is the
  convention Tauri's `PhysicalPosition` already uses, so no flip and no unit conversion are
  required. The M2 fallback in the stack table is therefore not taken.
- `hickory-resolver` **0.26.3** (2026-09-10; verified by compiling against it in the M3 Step 0
  census, 2026-09-21): `TokioResolver::builder_tokio()?.build()?`, `srv_lookup(name) ->
  Lookup` (no `SrvLookup` type), `Lookup::answers() -> &[Record]`, `Lookup::valid_until()`;
  `Record` and `SRV` expose **public fields** (`ttl`, `data`, `name`; `priority`, `weight`,
  `port`, `target`), not accessor methods. Default features include `tokio` + `system-config`.
  Was 0.26.2 (verified 2026-09-08).
- `tauri-specta` 2.0.0-rc.25 (not adopted)
- `rusqlite` **0.40.2** (2026-08-08; `bundled`), in the tree since M3a (2026-09-22): `Connection`,
  `transaction()`, `pragma_query_value`/`pragma_update` for `user_version`, `OptionalExtension`.

**`tauri-nspanel` — pinned by the M2 spike (2026-09-12), in the tree since M2a (2026-09-15, `36d50b8` on branch `m2`):**

- `tauri-nspanel` **2.1.0**, git only, pinned
  `rev = "c9ec2130422200f0863b23dfdad02b133a529b07"`. **Go/no-go: the dependency half PASSED; the
  vibrancy half was not actually measured until 2026-09-15.** The spike measured vibrancy on a
  window `Panel::to_window()` had already converted back to a `TaoWindow` (see "The spike measured
  a reverted `TaoWindow`"). Step 0 of M2a re-measured it on the real `OndarPanel` — **by view tree
  only, with no visual confirmation**, the same kind of evidence as before. It builds against
  `tauri` 2.11.5 with no patching, and the dependency graph unifies cleanly — single copies of
  `objc2` 0.6.4, `objc2-app-kit` 0.3.2, `objc2-foundation` 0.3.2 (it asks for `^0.6.1`/`^0.3.1`).
  It also enables `macos-private-api` on `tauri` itself. **The recorded fallback (borderless
  always-on-top window) is not triggered.**
- `window-vibrancy` stays **not a direct dependency.** Tauri provides `set_effects` on
  `Window`/`WebviewWindow` itself and calls `window_vibrancy` internally, so nothing needs to
  be added to `Cargo.toml` and the crates.io version of `window-vibrancy` is irrelevant to us.
  The note above about re-verifying its version at M2 is resolved: there is nothing to add.
- API shape confirmed against the pinned source: `PanelBuilder::<R, P>::new(&AppHandle, label)`
  is generic over a panel class declared with the `tauri_panel!` macro, `build()` returns
  `tauri::Result<Arc<dyn Panel<R>>>`, and `Panel` has **no** positioning or effects methods —
  both are reached through the Tauri window, `app.get_webview_window(label)`.
  **Correction (2026-09-15): not through `Panel::to_window()`**, as this entry previously said.
  `to_window()` is a conversion *back* (`panel.rs:255-287`, doc comment "Convert panel back to a
  regular Tauri window"): it removes the panel from the plugin store, clears the delegate, sets
  `releasedWhenClosed`, and `object_setClass`es the NSWindow back to `TaoWindow`. Upstream calls it
  only right before `close()`. The `nspanel`
  plugin must be registered (`.plugin(tauri_nspanel::init())`): `build()` calls `to_panel()`
  internally, which `unwrap()`s on the plugin's managed state.
- Ordering that is not optional: `ActivationPolicy` must be set **before**
  `PanelBuilder::build()`. With `no_activate(true)` the builder forces
  `NSApplicationActivationPolicy::Prohibited` around window creation and then restores the
  policy that was in effect beforehand, so setting `Accessory` afterwards is undone.
- Two separate transparencies are both required. `PanelBuilder::transparent(true)` acts on the
  NSWindow (`backgroundColor = clearColor`, `opaque = false`); the webview's own opacity is
  decided by wry when it creates the `WKWebView`, so it needs
  `.with_window(|w| w.transparent(true))` as well. The window-level call cannot reach it
  retroactively.
- **Verified at M2a (2026-09-15), against the pinned source and the bundled build:**
  - `no_activate(true)` does **not** make the panel non-activating. It only sets the activation
    policy to `Prohibited` around window creation (`builder.rs:815-926`). Non-activating is the
    style mask `NSWindowStyleMask::NonactivatingPanel`. `PanelBuilder::style_mask` / `set_style_mask`
    *replace* the mask; Ondar ORs the bit onto tao's (measured `0x8004` → `0x8084`).
  - `Panel::show()` is `orderFrontRegardless` alone (`panel.rs:242-246`) and never makes the panel
    key, so it can never resign key. Ondar calls `make_key_window()` after it — not
    `show_and_make_key()` (`panel.rs:409-418`), which also makes the content view
    (`WryWebViewParent`) first responder.
  - `Panel::set_event_handler` + `panel_event!` exist as recorded (`event.rs`, `panel.rs:303-335`),
    with two details the earlier note left out: `panel_event!` must be invoked inside
    `tauri_panel!`, which emits the imports it needs (`common.rs:19-30`); and `set_event_handler`
    **replaces** the NSWindow delegate. It stores the original delegate but restores it only when
    the handler is set back to `None` — nothing forwards to it while a handler is installed — so
    it silences tao's window events for that window. Not used; see "M2a: the tray path, measured".
  - tao's own delegate turns `windowDidResignKey:` into `WindowEvent::Focused(false)` (tao 0.35.3
    `window_delegate.rs:384-411`), passed through unchanged on macOS (tauri-runtime-wry 2.11.4
    `lib.rs:522-524`). **Measured end to end on the bundled build**, not only read.
  - `tray-icon` 0.24.2 builds the status item image from **one** PNG — one representation — and
    forces 18 pt height (`platform_impl/macos/mod.rs:283-311`); its `set_icon` hard-codes
    `is_template = false` (`mod.rs:115-123`). Swap with `TrayIcon::set_icon_with_as_template`
    (tauri 2.11.5 `tray/mod.rs:569-591`), one main-thread task; `set_icon` then
    `set_icon_as_template` is two, and can draw a flat black glyph in between. **Measured on the
    bundled build** (temporary probe, 2026-09-15): after the atomic swap the button image reads
    `isTemplate=true` at 18×18 pt; a plain `set_icon`, as negative control, reads `false`.
  - `tauri::include_image!` decodes PNGs at compile time (tauri-codegen `image.rs`), so no
    `image-png` feature is needed.
  - `Monitor::work_area()` is `NSScreen.visibleFrame` in top-left-origin physical pixels
    (tauri-runtime-wry `src/monitor/macos.rs:8-28`). `monitor_from_point` tests against
    `CGDisplayBounds`, which is in **points** (tao `platform_impl/macos/monitor.rs:163-170`).

**Added at M2c (2026-09-17):**

- `tauri-plugin-single-instance` **2.4.4** (the current 2.x; 3.0.0-alpha.0 exists and is not
  taken), registered **first**. macOS mechanism, read in `src/platform_impl/macos.rs` and
  measured end to end: a Unix socket at `/tmp/<identifier with `.` and `-` replaced by
  `_`>_si.sock` (`:62`) — `/tmp/eu_ondar_radio_si.sock` since the identifier change (measured
  2026-09-21; `/tmp/dev_crabnebula_ondar_si.sock` before it). A starting process
  connects; on success it writes cwd + argv and `exit(0)`s inside plugin setup (`:27-29`), before
  anything else is built; on `NotFound`/`ConnectionRefused` it removes the path and binds, which
  is why a socket left behind by `kill -9` recovers (measured, R7); on any *other* connect error
  it launches normally with **no** protection. The callback runs on a tokio worker
  (`tauri::async_runtime::spawn`, `:100`) — hop to main before touching AppKit. The socket is
  removed on `RunEvent::Exit`. It covers a real second process only (`open -n`, the inner binary,
  a copy of the bundle at another path); `open Ondar.app` and a Finder double-click against the
  running app start no process and arrive as `RunEvent::Reopen`, wired in `lib.rs`.
- Tauri CLI **2.11.4** (`@tauri-apps/cli`; the `tauri` crate is 2.11.5). `tauri dev --config
  <file>` merges the file into the configuration `generate_context!` bakes in — tauri-codegen
  reads `TAURI_CONFIG` (`lib.rs:83-87`) and tauri-build reruns on it (`lib.rs:472`). The CLI
  source that sets `TAURI_CONFIG` is not readable here (npm ships a binary), so that link is
  **measured**, not read: `pnpm tauri:dev` creates `/tmp/eu_ondar_radio_dev_si.sock` (re-measured
  2026-09-21 after the identifier change; it was `/tmp/dev_crabnebula_ondar_dev_si.sock` on
  2026-09-17), and a bundle built without the overlay keeps `eu.ondar.radio` (its `Info.plist`
  `CFBundleIdentifier`, read 2026-09-21).
- **Dependency risk, not a dependency:** `EffectsBuilder::radius` (tauri 2.11.5
  `window/mod.rs:2453`) reaches window-vibrancy 0.6.0's `setCornerRadius:` on the effect view,
  which that crate's own source calls "not listed in Apple documentation, might be private, but
  it works" (`ns_visual_effect_view_tagged.rs:92-99`). Ondar sends no private selector; Tauri
  does. A Tauri or window-vibrancy bump could drop it, so the corners are part of every
  milestone's by-eye acceptance. The `wants_layer=false` lead recorded during Step 0 is **void**:
  rounding works with no layer backing.

**MSRV correction (found and closed 2026-09-10, `7bbe332`):** the workspace `Cargo.toml` used
to declare `rust-version = "1.85"`, but `stream-download` 0.24.4's own manifest declares
`rust-version = "1.91.0"`, so 1.85 had never been buildable since `stream-download` was added —
it went unnoticed because the toolchain here is 1.98. `7bbe332` bumped the workspace
`rust-version` to 1.91 and dropped `README.md`'s caveat that `Cargo.toml` still said 1.85, so
its "Rust ≥ 1.91" prerequisite now stands alone; `clippy.toml`'s `msrv` matches. Nothing
outstanding.

Re-verify at the start of each milestone that touches these; update this list.

**Frontend test runner (added M3b 1b, 2026-09-23, from `pnpm-lock.yaml`):** vitest 5.0.1,
@testing-library/react 16.3.3, @testing-library/dom 10.4.2 (a peer the second requires and
pnpm does not add on its own — four packages where the decision named three), jsdom 30.1.1.
`pnpm test` runs `src/**/*.test.tsx`; its count is reported beside the Rust count, never summed.

### The boundary rule

The webview is a **renderer and an input device**. It holds no business logic, no audio, no
network calls, no persistence. Every meaningful action is a Tauri command; every state change
arrives as a Tauri event. If you find yourself writing a `fetch()` or an `<audio>` element in
TypeScript, stop — it belongs in Rust.

## Repo tooling

- **Remote:** private GitHub repo `metambuy/ondar` (created 2026-09-10, visibility `PRIVATE`,
  default branch `main`). `gh` 2.100.0 is installed on the build machine and authenticated as
  `metambuy` over HTTPS; git operations use the same credential. The `m1-done` tag is pushed
  and dereferences to `4b4ee3d`.
- **CI:** `.github/workflows/ci.yml`, on push and pull_request, `macos-latest` only (CoreAudio
  is a hard dependency; there is no Linux/Windows path to test). **It gates every *push*,
  verifying that push's head commit — not every commit**; see "CI verifies the head of each
  push, not every commit" below. It runs, in order:
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
  [run 34495743288](https://github.com/metambuy/ondar/actions/runs/34495743288). Its
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
- **20.7 % of radio-browser stations have coordinates** — measured 2026-09-21 over 25 236
  stations in eight countries (7 % RU to 38 % BR; `_handover/m3-step0-logs/p3-census.tsv`),
  an eight-country sample, not a global figure. The inherited "~30 %" is retired. Map markers
  are therefore sparse; the country dropdown, not the map, is the primary navigation. The map
  is context and delight. The PixelRadio supplementary coordinate DB will raise coverage (M4).
- ~~**BLOCKER: `src-tauri/icons/icon.png` is a 1×1 placeholder.**~~ **Resolved 2026-09-13.**
  It had stopped being cosmetic: the bundler failed with `Failed to create app icon: No
  matching IconType` and produced nothing, so no bundle could be built at all and
  `LSUIElement`, signing, notarisation and the DMG were all unreachable. The M2 spike worked
  around it only by passing an out-of-tree `.icns` via `tauri build --config`. See "The app
  icon and tray glyphs" below for what replaced it and how it was verified.
- **Popover size limits.** Anything that wants a big canvas is the wrong feature for this app.
- **Stream reliability varies.** Reconnect logic and honest error states are a first-class
  feature, not polish.
- **Shoutcast v1 servers** (`ICY 200 OK` status line) are rejected by hyper/reqwest and surface
  as an `Http` error **on the first attempt, since M3a's acceptance fix (2026-09-22)**. The
  history has two steps, and the first was recorded here as if it were the whole fix. (1) The
  classifier read the top-level `Display` ("error sending request…") while hyper's parse error
  sat in the `source()` chain — measured 2026-09-21 (M3 Step 0, a synthetic ICY server through
  `stall_bench`: `Reconnecting { attempt: 4 }` after 12 s); `e51f3ea` walked the chain
  (`stream.rs`, `classify_open_error`, pinned by `icy_status_line_is_http_not_network` against a
  real socket). (2) But the engine's `retry_or_fail` ignored the cause: **acceptance item 8
  re-measured the same `Reconnecting { attempt: 4 }` at 12 s**, and `Error { code: Http }` only
  at 31.4 s after five attempts and six requests — the classification was right and the loop
  was still there; this paragraph's earlier "since M3a" described behaviour the code did not
  have (instrument instance eleven's class: a derived claim recorded as measured — the socket
  tests pinned `stream::open`'s code, not the runtime). Now `StreamError::terminal` carries the
  cause and the session fails at once on a parse error or a 401/403/404/410 — **on its first
  open only** — keeping the backoff for network errors, 5xx, 408/429 (their `Retry-After`
  honoured, capped at 30 s) and for every answer on a reconnect (the review's finding 4,
  2026-09-22, narrowed the acceptance fix's "any 4xx, any attempt": a rate-limited first open
  and a mount mid-restart both deserve the backoff); `engine::session_tests` pins the request
  counts (ICY 1, 404 1, 503 > 1, 429 > 1 after its `Retry-After`, a 404 on a reconnect
  → `Reconnecting { 2 }`) with `run_session` driven against real sockets. **Rare:** 0 of 148 reachable
  stations in the census answered `ICY 200 OK` (upper bound ~2 %), so no raw-socket fallback is
  built. `scripts/stall-server.py --mode icy200` is the manual check (`scripts/icy-server.py`,
  which duplicated that mode, was removed 2026-09-22).
- **EQ output is bounded by a soft-clip stage** (added 2026-09-11, Phase 1 item 4; **exit
  criterion 3 met**). Below 0.95 the stage is the identity bit for bit; above it a rational
  knee, `T + W*(1 - 1/(1+s))` with `s = (|x|-T)/W` and `W = 1-T`, asymptotic to 1.0 and
  continuous in value and slope at the threshold. Rational rather than `tanh`: one divide
  against a transcendental in the audio callback, for the same C1 shape.

  `t = 0.95` is the broadcast peak level, chosen from the sweep below
  (`examples/eq_headroom_sweep.rs` at `9ad668f`, before the stage existed — it cannot be
  reproduced from a later revision, because `Equalizer` now bounds its own output):

```
TABLE 1 — transparency cost (flat EQ, so the shaper is the only stage acting)
  signal                                            t     peak dB    resid dB
                                                                  (THD proxy)
  A  62.5 Hz sine, peak 0.95                     0.80   -0.6086  -31.4121
  A  62.5 Hz sine, peak 0.95                     0.85   -0.3736  -35.7350
  A  62.5 Hz sine, peak 0.95                     0.90   -0.1537  -44.0993
  A  62.5 Hz sine, peak 0.95                     0.95    0.0000      -inf
  B  62.5 Hz sine, peak 1.00                     0.80   -0.9152  -28.0078
  B  62.5 Hz sine, peak 1.00                     0.85   -0.6772  -30.4938
  B  62.5 Hz sine, peak 1.00                     0.90   -0.4455  -34.2164
  B  62.5 Hz sine, peak 1.00                     0.95   -0.2199  -40.9601
  C  multi-tone 5 x sine, peak 0.95              0.80   -0.6086  -40.0254
  C  multi-tone 5 x sine, peak 0.95              0.85   -0.3736  -45.3995
  C  multi-tone 5 x sine, peak 0.95              0.90   -0.1537  -54.4529
  C  multi-tone 5 x sine, peak 0.95              0.95    0.0000      -inf
  D  multi-tone 5 x sine, peak 1.00              0.80   -0.9152  -35.5063
  D  multi-tone 5 x sine, peak 1.00              0.85   -0.6772  -39.2151
  D  multi-tone 5 x sine, peak 1.00              0.90   -0.4455  -43.9189
  D  multi-tone 5 x sine, peak 1.00              0.95   -0.2199  -51.3238

TABLE 2 — what it bounds (EQ engaged, shaper on the EQ output)
  case                                     pre-shaper     t=0.80     t=0.85     t=0.90     t=0.95
  1  62.5 Hz @0.70, band 1 +12 dB             2.78707   0.98171   0.98922   0.99497   0.99868
  2  62.5 Hz @0.95, band 1 +4 dB              1.50583   0.95584   0.97208   0.98583   0.99587
  3  multi-tone @0.95, all 10 bands +12 dB    7.45484   0.99416   0.99667   0.99850   0.99962
```

  At 0.95 the `-inf` rows are literal: material sitting at the broadcast peak passes through
  **bit-exact**, which is the point of putting the threshold there rather than lower. The
  bound holds at 0.99868 / 0.99587 / 0.99962 for the three overdriven cases.

  **What the tests pin is the pre-shaper column, not those figures** (2026-09-14). `eq.rs`'s
  tests invert each measured output peak through `soft_clip`'s algebraic inverse
  (`implied_pre_shaper`, itself round-trip tested against the shipped curve) and assert the
  implied peak is within `PRE_SHAPER_TOLERANCE` = ±0.1 % of 2.78707 / 1.50583 / 7.45484. The
  post-shaper figures above are **informative, not load-bearing**: they are rounded to five
  decimals, and near the ceiling the curve is so flat that the rounding is as large as any
  useful tolerance — case 1 measures 0.998675, on the rounding edge of 0.99868 — while a
  post-shaper tolerance that looks tight is loose in gain terms (5e-4 on case 1 admitted a
  pre-shaper peak anywhere in 2.27–3.95). Measured drift at the time of writing: −0.0014 % /
  +0.0002 % / −0.0029 %. The floor under the tolerance is one f32 output step through the
  knee's inverse slope, ~0.014 % at case 3.

  **Limitation of the measurement, not of the code:** the residual column is total error
  energy and cannot distinguish harmonic order, so it under-reports how harsh a narrow knee
  sounds once the shaper is heavily engaged. That regime was not measured. Accepted, because
  heavy engagement only happens when the user has asked for a large boost on content that has
  energy in that band, and because what exit criterion 3 requires is the bound — not the
  timbre of deliberate overdrive.

  **By-ear check (subjective, not a measurement).** On 2026-09-11 Martín listened to a music
  station with band 1 at +12 dB — the heavily-engaged case — and reported no audible issue.
  Recorded because it is the *only* evidence covering the regime the residual metric cannot
  see, and labelled as what it is: one person, one station, one sitting, no instrumentation.
  It is not a substitute for a harmonic-order measurement, and it should not be cited as one.

  **Placement:** the stage is inside `Equalizer`, not a separate `Source` adapter, so the
  bound is a property of the EQ stage and cannot be bypassed by assembling the graph
  differently.

  **Why a shaper was needed at all, and why it is now the only bound in the path.** Nothing
  downstream clamps: `Player::append` adds only `.amplify()` as a value transform (rodio
  `amplify.rs:63-65`, a pure multiply), and `biquad` 0.6's `DirectForm2Transposed` step
  (`lib.rs:175-181`) is a pure IIR multiply-accumulate. Ondar opens the sink without
  `.with_sample_format()` (`engine.rs`, `DeviceSinkBuilder::open_default_sink()`), which does
  **not** mean rodio defaults to `f32`: `from_device` calls `.with_supported_config()` (rodio `stream.rs:339-352`), taking whatever
  format CoreAudio reports. macOS's HAL is natively float32 so this is `f32` in practice, but
  that is a runtime fact, not a guarantee in Ondar's or rodio's source. If a device did report
  `I16`, the cast (rodio `stream.rs:531`) is the last step before the device callback, still
  downstream of `.amplify()`. Ondar's own code does no int cast either way. Two observations
  that still hold: the `Vol` slider is applied *after* the EQ and `set_volume` clamps to
  `0.0..=1.0` (`engine.rs:523`), so bounding the EQ output bounds the whole chain to the
  device; and clipping only ever required the boosted band to contain real energy — a
  high-passed talk stream has almost nothing at 63 Hz, so +12 dB there was near-inaudible on
  it while music at the same setting was not.

- **The audible grit reported on 2026-09-08 was external to Ondar** (resolved 2026-09-09).
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

### The M2 spike: vibrancy applies and the panel does not composite — measured on a `TaoWindow` (2026-09-12)

> **Read "The spike measured a reverted `TaoWindow`" (below) before relying on this section.**
> Every measurement here was taken after `Panel::to_window()` had turned the panel back into a
> `TaoWindow`. The results stand as measurements of that window; they were not measurements of an
> NSPanel, and the vibrancy claim is still view-tree evidence only.

Branch `m2-spike`, **not merged** — it is a spike, and the recorded plan was to abandon rather
than revert a dependency off `main` if it failed. It did not fail. Version pins from it are in
"Verified versions" above; this section records what was measured.

**The question it existed to answer.** Does `WebviewWindow::set_effects` still apply vibrancy
after `tauri-nspanel` subclasses the NSWindow? The published docs cannot say, because
`set_effects` → `crate::vibrancy::set_window_effects` → `macos::apply_effects` operates on the
**NSView** (`window_vibrancy` 0.6.0 `lib.rs:218` passes `handle.ns_view`), while the conversion
changes the **window** class. Prediction was yes; it needed proof.

**Answer as recorded: yes, and it renders** — on the reverted `TaoWindow`, not the subclassed
panel. With the window shown, the content view's tree is:

```
NSNextStepFrame @0,0 360x420
  WryWebViewParent @0,0 360x420
    [ NSVisualEffectViewTagged #tag=91376254 @0,0 360x420,
      NSKVONotifying_WryWebView @0,0 360x420 ]
```

Tag 91376254 is `window_vibrancy`'s own `NS_VIEW_TAG_BLUR_VIEW`, so the view is unambiguously
the one `apply_vibrancy` inserted — below the webview, sized to the content view. It also
*paints*: both the window background (`clearColor`) and `panel.html` are transparent, so the
dark translucent fill in the screenshot has no other possible source.

**The screenshot is weak evidence and the elimination argument is the strong one.** It was taken
over a dark backdrop, which is the worst case for telling a translucent material apart from a
flat fill — the two look nearly identical there. What actually establishes the claim is that a
`clearColor` window plus a transparent page leaves nothing else that could paint those pixels.
Anyone repeating this check should put a bright, colourful, high-frequency backdrop behind the
panel, where blur is unmistakable.

`EffectState::Active`, not `FollowsWindowActiveState`. **Reason re-derived 2026-09-15:** the
recorded reason — "the panel never becomes key, so 'follows' resolves to permanently inactive" —
is contradicted: with `make_key_window()` the panel *is* key while shown (measured, M2a). Whether
AppKit renders a key non-activating panel in an inactive app as "active" was never measured.
`Active` stays because the popover should look active whenever it is on screen, and `Active`
guarantees that without depending on the unmeasured answer.

**Two failure modes here look exactly like success.** `apply_effects` scans the effect list
for a macOS `Effect` variant and returns with a bare `return` if it finds none — no error, no
log. And an opaque page body paints over the `NSVisualEffectView` while `set_effects` still
returns `Ok`. Neither can be caught by checking a return value; check the view tree.

**`LSUIElement` works, and `tauri dev` cannot test it.** `src-tauri/Info.plist` is merged by
the CLI at `tauri build`: `plutil -p` on the bundled `Info.plist` shows `"LSUIElement" => true`,
and `lsappinfo info -only ApplicationType` on the running bundle reports `"UIElement"` — no
Dock icon, confirmed without needing to look at the Dock. The docs describe this merge for
`tauri build` only and dev runs a bare binary rather than a bundle, so the question cannot be
answered from `tauri dev` at all. A bundled build is required.

#### Defect, resolved at M2a by dropping it: `hides_on_deactivate` keeps the panel off screen

With `PanelBuilder::hides_on_deactivate(true)`, the panel **never composites**. AppKit reports
`isVisible == true` while `occlusionState` keeps the `Visible` bit clear. Setting it to `false`
flips `occlusionState` from `8192` to `8194` and the panel renders immediately.

The app was never active — nothing activated it: an `Accessory` app launched from a terminal,
never clicked — so "hides on deactivate" was permanently satisfied. (Recorded originally as "a
non-activating panel"; the window was neither a panel nor non-activating, see below. The
mechanism does not depend on the class.) Bisected: the activation policy itself is **not**
involved — `ActivationPolicy::Regular` changes nothing.

**Re-measured on the real `OndarPanel`, 2026-09-15** (M2a Step 0): with
`hides_on_deactivate(true)`, `key=true visible=true` but the `Visible` bit stayed clear (raw 8192)
at +60, +319 and +1322 ms; the control run with it `false` and the same frontmost app had the bit
set (8194) by +50 ms. The defect holds on the right class.

**Decided 2026-09-15:** `hides_on_deactivate` is dropped; the popover hides when it resigns key.
The hook is `WindowEvent::Focused(false)`, not `Panel::set_event_handler` + `panel_event!` as
first recorded here — see "M2a: the tray path, measured".

What the measurements eliminated before the bisect found it, all from the instrumented log:

| Ruled out | Evidence |
|---|---|
| Geometry | 720×840 physical at (1152, 562), inside the 3024×1964 main display |
| Layout / effect sizing | every frame in the tree 360×420; not a zero-size effect view |
| Alpha | `alphaValue == 1.00` |
| Window level | 101 (`PanelLevel::PopUpMenu`) |
| Spaces | `isOnActiveSpace == true` |
| Process liveness | watch thread logged a stable state across 40 s |
| Wrong display | no panel pixels on either display, by histogram, not by eye |
| The page failing to load | `on_page_load` logs `Finished`, `tauri://localhost/panel.html` |
| Vibrancy being the cause | a solid-colour opaque page is equally invisible |

**`occlusionState` must be decoded, not read as a number.** `NSWindowOcclusionStateVisible` is
`1 << 1` (`objc2-app-kit` 0.3.2, `generated/NSWindow.rs:251`, `const Visible = 1<<1`). The
observed `8192` is `1 << 13`, an undocumented high bit, and `8192 & 2 == 0` — so the raw value
is non-zero while the window is telling you it is *not* visible. Reading that as "AppKit says
visible" manufactured a paradox that cost real time. The instrumentation now prints the decoded
bit beside the raw value.

**`tray-icon`'s `y` flip: proven correct and unreachable, not untested (measured 2026-09-16, M2b
Step 0).** This entry has carried it as an open risk since 2026-09-12: the flip uses
`CGDisplayPixelsHigh(CGMainDisplayID())` — the *main* display's height, not the height of the
display the tray icon is on (`mod.rs:610-612`) — so a status item on a non-main display whose top
edge is not aligned with the main display's would be vertically wrong.

**That configuration cannot arise here.** `NSScreen::screensHaveSeparateSpaces()` is `false`, so
there is exactly one menu bar and macOS puts it on the main display; the status item lives on that
bar. The flip is therefore always handed the height of the display the icon is on. Measured in three
arrangements (menu bar on the BenQ, then the built-in, then the BenQ again), each time against two
independent instruments — the status item's own NSWindow frame in Cocoa points, and Tauri's `rect`:

| Menu bar on | Status item frame (Cocoa pt) | Main height (pt) | Flip → | Tauri `rect.position.y` |
|---|---|---|---|---|
| BenQ (1×) | `[1216,1050 24×30]` | 1080 | 1080 − 1050 − 30 = 0 | 0 ✓ |
| Built-in (2×) | `[880,949 24×33]` | 982 | 982 − 949 − 33 = 0 | 0 ✓ |
| BenQ (1×) | `[1286,1050 24×30]` | 1080 | 1080 − 1050 − 30 = 0 | 0 ✓ |

Two further notes from the same measurements. `CGDisplayPixelsHigh` returns the mode's **point**
height, not pixels: the built-in arm only reconciles with 982, not 1964. And the risk would return if
"Displays have separate Spaces" were ever on *and* macOS placed a status item on a non-main bar —
neither observed. So: upstream defect, real in principle, **unreachable in this configuration**, and
no longer something M2b has to work around.

**Correction (2026-09-15): "only one display is available" was never measured.** It was inherited —
from the brief and the pending ledger — and repeated here without a check. Nothing had called
`available_monitors()`, and `primary_monitor()` / `monitor_from_point()` cannot reveal a second
display. The record already hinted otherwise: the spike's elimination table above says "no panel
pixels on **either** display". Measured 2026-09-15 with `system_profiler SPDisplaysDataType` and
`available_monitors()` from the bundled app (temporary probe):

| Display | Primary | Position (physical) | Size (physical) | Scale | Work area | In points |
|---|---|---|---|---|---|---|
| Built-in Liquid Retina XDR | yes | (0, 0) | 3024×1964 | **2** | (0,66) 3024×1770 | 1512×982 at (0,0) |
| BenQ GW2470 | no | (−243, −1080) | 1920×1080 | **1** | full frame | 1920×1080 at (−243,−1080) |
| ANMITE | no | (3354, −1280) | 1920×1280 | **2** | full frame | 960×640 at (1677,−640) |

tao reports each monitor's position as its point origin × *that monitor's* scale, so the "physical"
positions are not one pixel space (ANMITE's 3354 is 1677 pt). Both externals' work areas equal their
full frames, consistent with the menu bar and Dock being on the built-in only (setting not read).

**Mixed-scale positioning error (pre-merge `/code-review`, finding 2) — the case exists on this
machine and is untested.** `tray-icon`'s `get_tray_rect` (`mod.rs:515-528`) makes the rect
"physical" with the **status item's** display scale. `panel::anchor` treats it with the **panel
window's** scale: it divides by that scale for `monitor_from_point` (which tests `CGDisplayBounds`
in points, tao `monitor.rs:163-170`), and `set_position` converts back with it again (tao
`window.rs:728-734`). `work_area` is converted with the chosen monitor's own scale (tauri-runtime-wry
`monitor/macos.rs:16-27`), so up to three scales meet in one clamp. On one scale this is invisible.
Read from the source, not observed: with the tray on the BenQ (1×) and the panel window at 2×, the
monitor lookup would get the wrong point and the panel would land on the wrong display. This is independent of the upstream `y` flip: a
top-aligned 1× display would get `y` right and `x` wrong. Not fixed at M2a. It changes the
anchor/scale logic and belongs to the multi-monitor pass, measured on these displays. Open
question for that pass: moving the menu bar in Displays → Arrange makes that display *main*, which
may mask the `y` bug; whether a status item can be clicked on a non-main menu bar needs measuring.

**Correction (2026-09-15): the tray-click path *was* exercised on 2026-09-12, and failed on every
click.** `_handover/m2-spike-app.log` lines 22-27: each left click logs `Down` and `Up`, each `Up` is
followed within 2 ms by `WARN tray toggle failed: window not found`, and no `tray anchor:` line
ever appears. It was recorded as "never exercised". It first worked, and was verified from the
log, at M2a — see "M2a: the tray path, measured".

### The spike measured a reverted `TaoWindow` (found 2026-09-15)

`Panel::to_window()` (`tauri-nspanel` `panel.rs:255-287` at the pinned rev) removes the panel from
the plugin store, clears the delegate, sets `releasedWhenClosed`, and `object_setClass`es the
NSWindow back to the class it had before conversion — `TaoWindow`, a plain `NSWindow` subclass
(tao 0.35.3 `window.rs:408-427`). The spike's `setup()` ran `build()` → `hide()` →
`panel_window()` (= `to_window()`) → `set_effects` → tray → optional show and watch; the same order
is already in `7134215`, whose log format matches the recorded log. From `panel_window()` onward
the window was a `TaoWindow` and the store was empty. The retained `PanelHandle` still pointed at
the same object, so every `msg_send` kept working, and no log line printed the class. Instance
state set before the revert survived (level 101, `hidesOnDeactivate`, shadow, `opaque = false`,
`clearColor`); the `tauri_panel!` class overrides did not, and `NonactivatingPanel` was never set.

What that does to the recorded conclusions:

- **Vibrancy.** The view-tree result is real, but of a `TaoWindow`, so the spike did not answer
  its own question. The mechanism argument (vibrancy acts on the NSView; `object_setClass` does not
  touch views) predicted it would carry over. Step 0 measured it on the real `OndarPanel`:
  `NSVisualEffectViewTagged@360x420` (tag 91376254) inside `WryWebViewParent`, frame view still
  `NSNextStepFrame` after the style-mask change, `opaque=false`. **View tree only; no visual
  confirmation.**
- **`hides_on_deactivate`.** A one-variable bisect on the reverted window, with the revert constant
  across both arms, so it held for that window; re-measured on the real panel (above) and it holds
  there too. The recorded *explanation* needed rewording, not reversing.
- **The dead tray click.** `to_window()` alone explains it: `toggle()` called
  `get_webview_panel` first, and the store had been emptied in `setup()`. Both `?` sites in the
  spike's `toggle()` produced the same message, so the log could not say which fired — source order
  does. Had the `setup()` call not been there, the path would still have failed: `toggle()` itself
  called `to_window()` before positioning (reverting on the first click), `hides_on_deactivate`
  was still set, and `show()` never made the panel key.

M2a's `panel shown` log line printed `class=`, so a revert cannot go unnoticed again; since M2c
the line is `panel show reason=… effective=true class=… key=…` (the tripwire is the `class=`
field, whatever the line is called).

### M3b: the collapsed view, the click endpoint, prefetch from bitrate — built (2026-09-23)

Branch `m3b` from `92cfe3f`, seven commits each pushed alone (plan `_handover/m3b-plan.md`;
the Step 0 probe plan was reviewed and **folded into the first commits** — its two measurements
were made on the real components as they were built, and the harness stayed in the repo for M4:
`_handover/m3b-step0-plan-review-2026-09-23.md`). Reports: `_handover/m3b-measure.md` (every
number cited to a run in `_handover/m3b-measure/`), `_handover/last-report-2026-09-23.md`.

- **1a `fbb0a79` — the measurement harness**, debug builds only (`src-tauri/src/measure.rs`,
  `src/measure.ts`): `ONDAR_MEASURE=<mode>` loads `panel.html?measure=…`, `_KEEP_OPEN` skips
  the resign-key hide, `_SEQ=show|shows:<n>` drives the production show/hide paths with no
  click, `measure_report` writes `measure[<mode>] …` lines on the process clock. A release
  binary has no `measure[` string (checked; the debug binary has 7).
- **1b `de87005` — the country control and the station list**, replacing the dev list: a
  native `<select>` (decision 1; a searchable list is a later commit if it proves poor by
  hand), the ranked rows one line each (decision 2, R1: name, then codec and bitrate, clamped
  with an ellipsis) scrolling inside the collapsed pane, the wrong-source guard, re-request on
  every show (M3a acceptance item 6's carried half), `landed` re-requests / `failed` clears.
  **The first TypeScript tests** (decision F2, Martín 2026-09-23: vitest + testing-library +
  jsdom; `pnpm test`, its own CI step; **two counts, never summed**): 4 then, 8 now.
- **1c `b4bbfbc` — Now Playing**: name, the ICY title line **reserved** at body height while
  empty (decision 5: a title arriving mid-stream moves nothing, at 16 pt of the band), `flag ·
  codec · bitrate` with the state as text where it is not "playing".
- **2 `d1129a3` — the list measured at 50 / 327 / 750 rows**: plain, **no virtualisation** —
  the rule written before the numbers did not fire (below). The `React.memo` arm lost and was
  deleted.
- **4 `bb2452d` — favourites and recents** as a filter on the same list (decision 2 of the
  brief, taken by Code and flagged: ★ Favourites and Recents are the select's first entries —
  no height taken, one control); the transport row is Play/Pause/Resume, Stop, ★, Volume in
  one row; the presets retired; `recents:updated` from the service on a recorded play.
  **Reversed after acceptance (2026-09-23, Martín, finding C):** by hand the two entries sat
  above ~240 countries, out of view in a native menu that opens at the selected country, and
  were not found unaided (acceptance item 2). They left the menu for a **★ toggle before the
  select**: ★ on shows one list — the favourites, then the recents not among them, favourite
  rows marked ★, status `N favourites · M recents`; choosing a country turns it off. Chosen by
  Martín over a select that swaps its contents, a three-state cycle, and two buttons. Same
  collapsed geometry (fit `m3b-bc-fit-02`: 8 rows, `country_row overflow=false`).
- **5 `33400f5` — the click endpoint** (plan § F6, reviewed with F1–F4): the rule lives in the
  engine's `Shared::set_state`, where all four `Playing` sites converge — `begin_session`
  (from `play`) lowers a flag, the first `Playing` that finds it down sends
  `EngineEvent::Started`; keyed on the session, not the previous state, so an underrun's
  refill, a resume and a reconnect of a session that already played do not fire, while a
  session that reconnected before ever playing, or was paused while buffering, fires on its
  first `Playing`. The shell hands the id to the stations service: the recent from the cached
  snapshot (schema v2's `stations(uuid)` index), `RecentsUpdated`, then **one** `GET
  /json/url/{uuid}` on the fetch runtime, `TOTAL_CLICK` 10 s, never retried (a retry could be
  a second vote), its outcome one log line and nothing else. A measurement run never votes
  (F1). The page's row does nothing on the station already playing (F2). `record_played` left
  the page. **Found by the session-level test:** the device-less test harness never drops a
  queued source, so `Player::clear()` on a second open waited forever — the harness now drains
  the mixer on a thread. The click's real request/response is an acceptance item (the first
  online play's log line; Step 0 P6's "one recorded request").
- **6 `155d14d` — prefetch from bitrate**: `play(url, stationId, bitrateKbps)`;
  `stream::prefetch_for = max(one decoder read, RING_SECONDS × bitrate / 8)`, pure and pinned
  (see "The prefetch knee"). **Capped after the review** (finding 2, 2026-09-23) at half the
  stream buffer, 131 072 B: the record's `bitrate` is user-entered (1411, 1536, a `128000`
  typo), and a prefetch at or over the 256 KB buffer is met only when the buffer is full —
  startup would wait for the whole window. The knee crosses the cap at 525 kbit/s.
- Tests 162 → **177** (audio 73, shell 40, stations 64) **+ 8** TypeScript.

**Measured, what fits** (`m3b-measure.md` §§ 1b, 1c, 4; runs `m3b-m-01`…`-06`, `-16`; stills):
the block heights against the plan's derived table, and the rows the collapsed pane holds:

| commit | now_playing | transport | list band | rows (R1, pitch 24) |
|---|---|---|---|---|
| 1b (dev Now Playing, dev controls) | 86 | 83 | 118 | **5** |
| 1c (the real Now Playing) | **62** = derived | 83 | 142 | **6** (as 1b predicted) |
| 4 (presets retired, one transport row) | 62 | **29** | 196 | **8** (1c predicted 7 — the volume shares the row) |

Country select 20, provenance 14, expand row 18, gaps 8, all countries alike; `rows_full` =
`rows_by_rect` in every run, pitch uniform over 750 rows, the stills read the same by eye.
Names clamped by the one-line row: FR 13/750, PT 2/327. The M3a control-row overflow is
closed: the provenance span's right edge is the content edge. The hidden webview's layout
equals the visible one. A first build without the body gap was caught by the measurement
(controls touching the country row) and fixed before 1b's commit.

**Measured, list performance** (§ commit 2; runs `m3b-m-07`…`-15`; the webview presents at
60 Hz on the 120 Hz built-in, sampler median 17 ms):

| | 50 rows | 327 rows | 750 rows |
|---|---|---|---|
| fresh mount, median (reply / commit / paint) | 18 (15 / 2 / 2) | 28 (17 / 3 / 8) | **51 (31 / 5 / 16)**, 75 first |
| show with the list mounted, 20 shows, median / max | — | — | **8 / 9** (memo 7 / 10; empty list 5 / 7) |
| programmatic scroll, dropped frames (sampler) | 0 | — | **0** |
| presentations a frame late (60 fps capture, start-up excluded) | 0 / 152 | — | 7 / 248 |
| WebContent RSS | 51 MB shown | — | 81 MB mounted, 102 MB after five cycles |

Commit + paint is linear, **22.9 µs/row** (residuals ≤ 0.2 ms); the reply is by bytes,
0.088 ms/KB (319 KB at the cap). The rows' re-render is not the show's cost (memo moves the
median 1 ms); the carried 51–83 ms first show did not reproduce — max 27 ms over 66 shows. The
rule: (i) the show is 0.10 of `LAYOUT_FALLBACK`, (ii) 0 dropped frames against 0, (iii) 3.2 ms
of commit at the cap — none fired. **F5's outcome fired** (the reply dominates the commit at
750) and the leaner payload / paged command was **deferred** (chat, 2026-09-23: a boundary
change is better made once the UI is settled; 31 ms of a 51 ms mount at one mount per
country change is real but not felt). **`LAYOUT_FALLBACK` stays 250 ms**: the combined
distribution is n = 74 (M2d's 8, one of them 100 ms, and these 66, max 27), 250 ms is 2.5× the
max; recorded in the constant's comment rather than re-derived downward from the newer,
luckier sample. The hand-driven scroll runs once at M3b acceptance.

**Observed, not criteria:** one `stream_download` DEBUG line per play under `RUST_LOG=info`
(M3a saw six) — cause still open; the Web Inspector was not used (a GUI session Code cannot
drive; the screen capture stood in); the selected country is not persisted (a launch starts
on PT).

### M3a: the station directory, built (2026-09-22)

Branch `m3a` from `0e4b5d0`, ten commits each pushed alone (plan `_handover/m3a-plan.md`,
reviewed twice; gate 1 and the plan review's F1–F6, S1–S3 all applied). What exists now, and
the decisions Martín took at the plan review (2026-09-21):

- **`crates/ondar-stations`**, no Tauri dependency: `model` (boundary types; `i64`/`u64` fields
  exported to TypeScript as `number` — ts-rs's default `bigint` broke the page's arithmetic,
  and serde sends a JSON number anyway), `normalise` (the census rules with their counts: 9
  lowercase country codes merged, `XX` dropped, bitrate 0 → unknown, geo null or (0,0) → none,
  `AAC,H.264` flagged video), `filter::rank` (drop broken and empty-url, dedupe folded name +
  url keeping the higher votes, sort votes then known-bitrate-first then clicktrend, **cap 750**),
  `srv` (hickory-resolver 0.26.3; fallbacks `de1`, `all.api` — the measured set), `client`,
  `cache`, `store`, `service`.
- **Client rules, all measured:** `bycountrycodeexact/{cc}?hidebroken=true&limit=100000` (the
  endpoint measured returning a whole > 1000 country); three attempts on the same host with
  backoff 1/2 s and one SRV re-resolve before attempt 2 (there is one server); a **200 s
  wall-clock budget** for the sequence; connect 10 s, 15 s without a byte, a per-request total
  of 30 s (countries, search) or 180 s (a list: 9.5 MB at 0.5 Mbit/s is 152 s); no gzip. The
  **truncation guard** on the quantity that moved in the census: a full page; exactly 1000 rows
  against a larger limit when the country's `station_count` is unknown or ≥ 1172
  (1000 / (1 − 0.146), the largest measured broken share, so a real 1000-station country is not
  refused forever); fewer than half the published `station_count` (the broken share measured
  4.6–14.6 %) — **for countries of 2 000 stations and up only** (`RULE3_MIN_EXPECTED`, 1000 / 0.5):
  the plan had said rule 3 "cannot fire under 2 000 by construction", and it could — the count
  it compares against is up to seven days old, or older on an expired countries list, so a
  small country whose stations went broken since was refused forever (Malta, count 3, one
  working station: 1 < 1.5; review finding 6, 2026-09-22). An empty countries answer (`200 []`) is refused as `EmptyCountries` rather than
  stored: stored, it left no row for the re-read, dropped every waiter unanswered and was
  announced as `landed`, which made the page fetch it again on every event (review finding 3,
  2026-09-22).
- **Cache** (rusqlite, `bundled`): hand-rolled `PRAGMA user_version` migrations (decision 1 —
  one table set, no dependency); TTL 24 h for a list, 7 d for countries, from radio-browser's
  own recheck cadence; an expired list is never dropped — it is served at once as `cached` with
  `refreshing: true` while a refresh runs (**stale-while-revalidate**, F5), and with no age
  ceiling when the network is down (G3). `stations:updated { country_code, outcome }` tells the page
  when a refresh landed. Only a missing list makes a caller wait. A file that will not open or
  migrate is moved aside as `ondar.sqlite.corrupt-<unix seconds>` and recreated; if that fails
  too the directory is **unavailable** (`code: "stations"` on every call) and the app still
  launches — until the review fix of 2026-09-22 (finding 2) the open error propagated out of
  `setup` and a corrupt cache stopped the tray, the popover and audio from coming up at all.
- **Service** (decision 2 as amended by F3): one thread owns the connection and never awaits the
  network; fetches run as tasks on a two-worker runtime and report back through the same
  channel; concurrent callers for one country share one fetch. Measured by test: a held fetch
  does not delay `list_favourites`.
- **Store:** favourites; recents **20, a replay moves the entry to the top** (decision 3).
- **Fixtures** (decision 4, F5): ~120 KB of census slices committed under
  `crates/ondar-stations/fixtures/` with `PROVENANCE.md` quoting the API's stated freedom to
  "mirror all its data" (no formal data licence exists; the server's AGPL covers the server);
  `scripts/fixture-slice.py` regenerates the PT slice; the 1000-row truncation body is built in
  the test.
- **Two `ondar-audio` fixes that Step 0 surfaced** (decision 6): the ICY status line is now
  `Http` (the classifier walks the error's `source()` chain to hyper's parse error; `hyper` is a
  direct dependency for that downcast only) — **and, from acceptance, terminal on the first
  attempt** (`StreamError::terminal`, `retry_or_fail` by cause; see the acceptance paragraph and
  the Constraints entry; narrowed by the review's finding 4 to 401/403/404/410 and to the first
  open of a session) — and the DNS claim was **measured and falsified** before any bound was
  added — reqwest's `connect_timeout` covers DNS on the audio path (see "Reconnect ownership and
  stream timeouts").
- **Measured on the dev loop, 2026-09-22:** schema migrated to v1; the database at
  `~/Library/Application Support/eu.ondar.radio.dev/ondar.sqlite` (the bundle's is under
  `eu.ondar.radio/`); one SRV record; countries 240 rows; PT fetched 344 rows (`hidebroken`),
  327 kept after the filter (the census day had 345/328 — live churn); no error lines.
- **CI on a branch supersedes the older run** (`ci.yml`, `cancel-in-progress` off `main` by
  design): re-running an older commit's cancelled run cancels the tip's. The rule for a branch
  built one-commit-per-push is therefore **wait for green before the next push**, and never
  re-run an older run while the tip's is in flight (learned on commits 5 and 7).
- Tests 88 → **141** (124 hand-written + 17 generated; audio 54, shell 39, stations 48); after
  the acceptance fixes **148** (129 + 19; audio 58, shell 40, stations 50).

**Acceptance, run 2026-09-22 14:00–15:47 UTC** (`_handover/m3a-acceptance.md`, ten items from the
plan's Verification section against the debug bundle at `b52e61c`, logs `m3a-acc-*` beside it;
8 of 10 passed first time, the two failures fixed on `m3a` and re-run):

- **Passed as built:** S2 offline with no cache → `code: "stations"` after three same-host attempts
  in 3.02 s, nothing retries afterwards (`m3a-acc-01b`); the bundle's database is its own file
  (`eu.ondar.radio/`, the dev loop's under `.dev/`); first launch online → 240 countries, PT 344
  rows → 327 kept, `PT|327|344` in SQLite; a second open within 24 h → **zero** fetch or SRV lines;
  an expired list offline → served **immediately** at the click, `cached 25 h ago · refreshing…`,
  the refresh fails in 3.0 s with `(expired list stays)`; recents survive a restart and a replay
  moves the row without a duplicate; G4(b) by its test; 0 `ERROR`/`panicked` across every run.
  Also measured, unplanned: **France, 3 650 rows → 750 kept** through the live client (the
  > 1 000-station country the plan had deferred to M3b: `limit=100000` honoured, the guard silent
  with `expected` known, the cap applied).
- **Item 8 failed, then fixed (`3ab7ec2`).** `stall_bench` against `scripts/icy-server.py` (since
  removed for `stall-server.py --mode icy200`) read
  `Reconnecting { attempt: 4 }` after 12 s with four requests — the shape `e51f3ea` had been
  written to remove — and `Error { code: Http }` only at 31.4 s, six requests. The classification
  was right; `retry_or_fail` ignored the cause. Now a terminal open error (hyper parse error, or
  401/403/404/410 on the session's first open — the review's finding 4 narrowed "any 4xx")
  fails the session at once; 5xx, 408/429 and network keep the backoff; `engine::session_tests` drive
  `run_session` against counting servers (ICY 1, 404 1, 503 > 1 requests; mutation-checked). Re-run:
  `Error { code: Http }` at **0.138 s**, one request, no `Reconnecting`. The Constraints entry that
  had recorded the classification as the whole fix is corrected (instance thirteen below).
- **Item 6 failed as written, half fixed (`4d83918`), half carried.** After the network came back
  the page kept `cached 25 h ago · refreshing…` through four opens with no fetch in the log. Two
  causes: (a) the service's failure arm emitted nothing, so the flag never cleared — now every
  fetch ends with one event carrying a `RefreshOutcome` (`landed` | `failed`), and the page clears
  the flag on `failed` without re-requesting (re-run `m3a-acc-06`: `cached 26 h ago`, no
  `refreshing…`, 10 s after the failed refresh); (b) the page requests a list on mount and on a
  country change only, so a popover open after a reconnect never re-requests — **carried to M3b**
  as page behaviour (the dev list retires there; the service already restarts a refresh on any
  repeat request for an expired list, shown by the country-change variant: PT refetched in
  0.46 s, the event reached the page, the cache replaced). The acceptance item's premise "each
  open starts a refresh" was Code's assumption about the page, not a measurement.
- **Method.** The "done" handshake cannot span a Wi-Fi-off step: the first offline run launched
  with the network still up (void), and a blocking script that waited for the default route to
  drop aborted twice at 240 s with Wi-Fi off — the route test never fired on this Mac, cause not
  found — while the harness's permission classifier timed out on a launch attempt. Offline items
  are now run by Martín himself from Terminal (Code lists the commands, ends its turn), online
  ones by Code's launcher with the handshake.
- **Observations, not criteria:** six `stream_download` DEBUG lines per play under `RUST_LOG=info`
  (the shell's filter should not pass them; cause not identified); the first popover show of a
  session with the 327-row list mounted takes 51–83 ms against M2d's 2–13 ms, later shows 2–35 ms
  (`LAYOUT_FALLBACK` 250 ms still 3× the worst); the dev list's country `<select>` is wider than
  the panel (its label is a flex item that cannot shrink below the select's intrinsic width), which
  also pushes the countries provenance line off the right edge — dev list, retires at M3b.
  Favourites persist by test only (`store::tests`); the dev list has no favourite control, so the
  hand check moves to M3b.

**Code review, 2026-09-22** (`/code-review 0e4b5d0..f023084`, the whole branch; findings in
`_handover/m3a-review-findings.md`, triage in `m3a-code-review-review-2026-09-22.md`; 8 finders
+ 4 verifiers, no run lost to a 429): ten findings, all fixed, one commit each, each pushed alone
and green, each with a test that fails on the previous commit and a mutation check (exceptions
stated below). Tests 148 → **162** (audio 65, shell 40, stations 57).

- **`landed` meant "the fetch landed", not "the list was stored"** (`f3de220`, finding 1). With
  a read-only or full disk the page's re-request on `landed` started another full-country fetch,
  forever — the loop `4d83918` had been meant to close. One exit path per fetch (`finish`),
  outcome derived from the write; the post-put re-read went with it.
- **A cache that would not open stopped the app** (`693f12b`, finding 2): moved aside as
  `ondar.sqlite.corrupt-<ts>` and recreated, else a degraded handle (`Unavailable`); nothing in
  `setup` can refuse the launch. Measured on the bundle: the existing database opens normally
  (`m3a-acc-07`/`08`, no `.corrupt-` file).
- **`200 []` from `/json/countries`** (`d0b4202`, finding 3) dropped every waiter unanswered
  (`Closed`) and announced `landed`; now refused by the client (`EmptyCountries`) → `failed`.
- **Retry policy narrowed** (`5431a70`, finding 4): terminal for 401/403/404/410 and the ICY case
  only, and only before a session's first open; `Retry-After` honoured (delta-seconds, ≤ 30 s).
  Session-level tests: a 429 waits out its header; a 404 on the reconnect after a WAV stream
  ended keeps the backoff.
- **Rule 3 floored at 2 000** (`c6974f0`, finding 6): the plan's "cannot fire under 2 000 by
  construction" was false — Malta, count 3, one working station, `1 < 1.5`.
- **One non-HTTP wording table** (`6bf8114`, finding 8): "status" out of it; the 5xx branch is
  reachable again. Its test does not compile on the prior commit; the mutation is the check.
- Small: `mms://` → `invalid_url` before any request (`f19e776`); the dev list applies a reply
  only for the country still selected (`eeeeb97`, **no test** — no TS runner, the list retires at
  M3b; BUILD_PLAN carries the guard for M3b's list); `LIKE` metacharacters escaped in the offline
  search, ASCII-only folding stated (`d5795fb`); the 4xx/5xx body text back in the message,
  bounded to 200 chars (`73020d3`).
- Cleanups: `CacheSource::StaleAfterFailure` removed from the IPC contract (`bacca7f`);
  `scripts/icy-server.py` removed for `stall-server.py --mode icy200` (`e016a5d`); the `expect`
  census in CLAUDE.md corrected to three sites (`44099b0`); `.claude/settings.local.json`
  ignored (`6bab153`). Recorded for M3b, not fixed: the 750 cap is applied before storage, so the
  offline search sees only the top 750 of each list.
- **Acceptance re-run on the touched paths** (`m3a-acceptance.md`, "Re-run after the review
  fixes"): item 8 through the replacement server (`Http` at 0.175 s, one request); items 5 and 6
  as `m3a-acc-07` (expired PT served at once offline, refresh failed at +3.3 s and the `failed`
  event had cleared `refreshing…` before the first open at +6.2 s; online, FR 3 652 → 750 in
  1.28 s and PT 344 → 327 in 0.45 s through the new exit path) and `m3a-acc-08` (countries
  240 rows refreshed at launch, `fetched_at` current). 0 `ERROR`/panic across the three runs.

### M3 Step 0: the live data, measured (2026-09-21)

Before M3a, a census of radio-browser.info from this Mac with the real `User-Agent` — plan
`_handover/m3-step0-plan.md`, report `_handover/m3-step0-report.md`, raw responses and scripts
in `_handover/m3-step0-logs/` (the probe crate is uncommitted, kept as `probe.patch`). Every
number below is **measured** there unless tagged otherwise. It changed four premises M3 had
inherited and produced four decisions.

**The API.**

- **One server.** The SRV record `_api._tcp.radio-browser.info` has a single target,
  `de1.api.radio-browser.info` (priority 1, weight 1, port 443), by two instruments
  (`hickory-resolver` and `dig`). `de2` still resolves and answers but to the **same address**
  (`91.98.4.78`); `fi1`, `nl1`, `at1`, `fr1` do not resolve; `all.api.radio-browser.info` is the
  same address again; `api.radio-browser.info` is the Netlify-hosted docs site, not an API host.
  `/json/servers` lists `de1` twice (v4, v6). Consequence: "3 retries across *different* hosts"
  has no object — the client retries the same host with backoff, and the offline cache carries
  resilience. The fallback list is the measured set: `de1`, `all.api`.
- **SRV TTL ~5 min observed, zone TTL not answered.** A fresh answer carried `ttl=300` and
  decremented 300 → 270 over 30 s; the "authoritative" `dig @<Cloudflare NS> +norecurse`
  answer *also* decremented (270 → 265 in 5 s), so port 53 is intercepted by the local
  resolver on this network and the zone value cannot be read from here. Re-resolve per launch
  and on failover; persisting the result is pointless.
- **`bycountrycodeexact` truncates silently at 1000.** With no `limit`, six of eight countries
  came back with exactly 1000 records against `stationcount` 1 456–8 190, status 200, no
  header saying so. `?limit=100000` returned `stationcount` ±1 (US 8 191 rows, 9 457 048 B in
  3.2 s). The client must send an explicit limit and guard against a 1000-row answer.
- **`hidebroken=true` equals the `lastcheckok == 1` subset exactly** (PT: 345 = 345, symmetric
  difference 0, fetched back to back on one host). The countries list keeps its 250 rows under
  it; per-country counts drop (US −955).
- **`search`** honours `countrycode` (case-insensitive), `hidebroken`, `order=votes|
  clickcount|clicktrend` with `reverse`, `limit` and `offset` (disjoint consecutive pages);
  `search?countrycode=PT` was byte-for-byte the `bycountrycodeexact/PT` list (371 rows).
  `nameExact` is case-insensitive (`ORBITAL` and `Orbital`), `order=name` uses a collation
  the client cannot reproduce, and `name` matches a case- and diacritic-folded substring.
  Whether `search` applies the 1000 default on a country larger than 1000 is **not
  measured** (needs a > 1000 country through `search`).
- **No compression.** `Accept-Encoding: gzip` is ignored (`tiny-http`, chunked). Some *station*
  servers gzip playlists without being asked (Wowza) — M3c's playlist fetch decodes; the
  stations client needs nothing.
- Latency on this line: ttfb 139–971 ms; the 9.5 MB US list in 3.2 s. The census client
  once sat 12 min before its first fetch of a station with no socket open; the report
  *derived* "DNS is outside reqwest's `connect_timeout`" from it. **Falsified on the audio
  path at M3a** (see "Reconnect ownership and stream timeouts": a stalled resolver is
  bounded at 10.01 s); the census hang's cause is unexplained. The stations client still
  gets a stall bound and a per-request total (M3a plan, F4) for its own reasons.
- `/json/stats`: 59 411 stations, 6 647 broken, 241 countries; the countries list's
  `stationcount` sums to 64 791 — unexplained, recorded (G7); the app shows its own counts.

**The data (eight countries: US DE PT ES FR BR RU MT, 25 236 stations).**

- Countries: 250 rows; **9 lowercase codes** (`ch de fr gr nz ru tr us uy`, one station each,
  duplicating their uppercase row's name) and **`XX`** (empty name, 1 station); no empty codes.
- Stations, pooled: `lastcheckok == 0` 8.7 %; `bitrate == 0` **16.8 %** (30.8 % in DE); `hls == 1`
  **3.8 %** (9.7 % PT, 2.4 % DE); geo present **20.7 %**; empty `url_resolved` 210 rows; https
  60.7 %; folded name + url duplicates 1–22 % per country (FR 832 rows); codec strings verbatim
  `MP3` 16 678, `AAC+` 3 982, `AAC` 3 682, `UNKNOWN` 310, `OGG` 308, empty 210, `AAC,H.264` 41
  (video), `MP4` 13. radio-browser rechecks every station roughly daily (`lastchecktime` age
  p50 13–18 h, p90 24–71 h, max 120 h = its retention), which is what a 24 h list TTL
  implicitly assumes — it holds.
- **HLS shape (sample of 10 `hls == 1` stations, 7 countries; MT has none):** 5 are ADTS-AAC
  media playlists with a leading ID3 tag (the timed-metadata carrier), 5 are MPEG-TS — 2
  audio-only, **3 carrying H.264 video** (TV feeds listed as radio; one master's first variant is
  video-only). fMP4: none. All live sliding windows, target durations 4–13 s, sequence
  advancing on refresh. The two `.m3u8` URLs flagged `hls == 0` were `302`s onto plain ADTS
  streams — the flag was right, the file name was not.
- **Shoutcast v1: 0 of 148 reachable stations** answered `ICY 200 OK` (153 probed on a raw
  socket, redirects followed; 22 % of `url_resolved` redirect; 93 % offer `icy-metaint`). Rule
  of three: ≤ 2 %. See the Constraints entry for what the engine does with one.

**The per-country cap, decided 750.** After the filter (drop `lastcheckok == 0` and empty
`url_resolved`, dedupe folded name + url, keep `bitrate == 0` sorted last among equal votes,
sort votes then clicktrend), the share of a country's total `clickcount` carried by its top N
(`_handover/m3-step0-logs/p3-cap.tsv`, and the 750 column recomputed under the decided rules):

| cc | filtered n | clicks @500 | **clicks @750** | clicks @1000 | votes @750 |
|---|---|---|---|---|---|
| US | 6 832 | 50.6 % | **56.9 %** | 61.9 % | 91.4 % |
| DE | 5 735 | 52.0 % | **59.2 %** | 64.1 % | 85.6 % |
| FR | 2 844 | 76.7 % | **84.0 %** | 88.5 % | 97.5 % |
| RU | 2 650 | 63.4 % | **70.4 %** | 76.1 % | 96.1 % |
| BR | 1 366 | 73.4 % | **83.9 %** | 93.4 % | 99.3 % |
| ES | 1 236 | 88.9 % | **94.0 %** | 97.2 % | 99.5 % |
| PT | 328 | 100 % | **100 %** | 100 % | 100 % |

The curve is flat in the long tail; 750 is Martín's call (2026-09-21) between the report's 500
and the next row.

**Decisions (Martín, 2026-09-21, gate 1).** (1) Cap **750** per country. (2) **`bitrate == 0` is
kept, sorted last** among equal votes: a zero bitrate is "unknown", not "broken" (`lastcheckok`
covers broken), and dropping it costs 16.8 % pooled, 30.8 % in DE; the prefetch formula falls
back to `one_decoder_read` when the bitrate is unknown (M3b). Reverses BUILD_PLAN's "drop
bitrate 0". (3) **HLS is split:** ADTS-AAC media playlists (live refresh loop + ID3 strip) ship
in M3 as M3c; the MPEG-TS demux and audio-variant selection move to after M4; until then a TS
station fails with an honest error, not a reconnect loop. (4) The two `ondar-audio` defects
Step 0 surfaced — the ICY status line surfacing as a reconnect loop, and DNS unbounded on the
connect path — are fixed in M3a, each its own commit.

### Bundle identifier: `eu.ondar.radio` (decided 2026-09-21)

Martín bought the domain **`ondar.eu`** and settled the identifier as its reverse-DNS form,
**`eu.ondar.radio`**, with the dev overlay **`eu.ondar.radio.dev`** (`src-tauri/tauri.conf.json`,
`src-tauri/tauri.dev.conf.json`; the shell test `dev_identifier_is_the_real_identifier_plus_dev`
pins the derivation). It replaces the `dev.crabnebula.ondar` placeholder before M3 creates any
path under the app data directory, so BUILD_PLAN's M3 soft deadline is met with nothing to
migrate.

- **Why `radio` and not `app`:** an identifier ending in `.app` (`eu.ondar.app`) collides with the
  bundle extension on macOS — `Ondar.app` versus an identifier whose last label is `app` — and is
  the kind of ambiguity Finder, LaunchServices and humans reading a `defaults` domain trip on.
- **Measured 2026-09-21** (`_handover/identifier-logs/sockets.txt`, `id-01-bundle.log`,
  `id-02-dev.log`): the rebuilt debug bundle (`Info.plist` `CFBundleIdentifier` =
  `eu.ondar.radio`, `lsappinfo` agrees) launched detached (`ppid=1`) and created
  `/tmp/eu_ondar_radio_si.sock`; `pnpm tauri:dev` beside it created
  `/tmp/eu_ondar_radio_dev_si.sock`, and both processes ran at once — the two-identifier
  coexistence M2c case (f) needs. Both names follow the plugin's `.`→`_` rule exactly as
  predicted.
- **Old data is orphaned and harmless.** `~/Library/WebKit/dev.crabnebula.ondar` and
  `~/Library/Caches/dev.crabnebula.ondar` (and the `dev.crabnebula.onda` pair from before the
  rename) stay on disk; nothing reads them, they hold only WebKit's own website data and cache,
  and there is **no** `~/Library/Application Support/dev.crabnebula.ondar` — the app never wrote
  a file of its own under the old identifier (listed 2026-09-21, `sockets.txt`). The first launch
  under the new identifier created the `eu.ondar.radio` pair in the same two places. Delete the
  old ones by hand or leave them.
- **Quitting with `pkill` (SIGTERM) leaves the socket file behind** — both `dev_crabnebula_*`
  sockets were still in `/tmp` after their processes were gone, and so were the new pair after
  this measurement. Harmless by design: the plugin removes a stale path on `ConnectionRefused`
  and binds (measured at M2c, R7). Only `RunEvent::Exit` (Quit from the tray menu) removes it.

### M2d: resize in place — Step 0 measured, D2–D4 decided (2026-09-18)

Branch `m2d`. Step 0 was an uncommitted probe on `8901021` (`_handover/m2d-step0-logs/probe.patch`
is its byte-exact record; report `_handover/m2d-step0-report.md`, gate-1 review
`m2d-step0-review-2026-09-18.md` — both gitignored, which is why the results live here). Bundled
debug build, macOS 26.6.2 (25G83), launched detached; every show driven by `tray::rect()` +
`panel::show_at`. Every figure below is a **measurement** unless marked derived or decided.

**A derived claim about AppKit was wrong, and the first run caught it.** The probe plan read tao
0.35.3's source and derived that Tauri's `set_size` reaches `setContentSize:`, which "keeps the
Cocoa bottom-left origin", so a naive height change would grow the panel upward into the menu bar.
Measured: on a **visible** panel `setContentSize:` keeps the **top-left** — Cocoa frame
`[537,1312 360×420]` → `[537,1012 360×720]`, top-left unchanged, in both directions over ten
cycles. On a **hidden** panel it kept the **bottom-left** (top-left y 39 → 339 after a hidden
shrink). The hidden case is measured and **unexplained**. Reading the source told us which
selector is called, not what the selector does. Moot while every resize recomputes the anchor
(below); a trap for any path that sets a size while hidden and then trusts the old position.

**P5 — the M2b latent defect, reached on purpose.** With the icon on the 2× built-in and the panel
forced onto the 1× BenQ with `setFrameOrigin`, a size-only change (`set_size` 360×720) left the
panel on the BenQ, 221 pt left of and 783 pt above the icon; re-entering production's
`anchor_points` with the new size landed it at (758,39) under the icon, in 0.85 ms. On the same
display the naive and recomputed paths agree — because `setContentSize:` keeps the top-left and the
clamp is idle there — so a same-display pass proves nothing; the other-display arm is the one that
could fail, and did. **Rule: every expand and collapse re-enters the anchor computation with the
new size; there is no size-only path.**

**R3 — which frame a runtime check uses.** The probe logged two forms. The **own-screen** form
(`panel.frame` inside `panel.screen().visibleFrame`) read `true` on every row *including the two
failures* — a panel on the wrong display entirely, inside that display's visible frame, passes it.
The **tray-screen** form (inside the visible frame of the `NSScreen` containing the icon's centre)
failed where it should. **Any runtime assertion or log check about the panel's placement uses the
tray-screen visible frame.** The own-screen form is not to be used for this.

**P1 — what reaches the screen on a resize.** Two instant routes were measured on the 60 Hz ANMITE
with a calibrated capture (179 of 180 frames resolved; a one-frame event is captured with p ≈ 0.99):
one synchronous `setFrame:display:` on the NSPanel (route S, 0.6–1.7 ms, `Resized` delivered
inside the call) and Tauri's `set_size` (route T, returns in 0.1 ms, frame changes 2.4–5.2 ms
later on a further main-queue turn). **Both show exactly one 60 Hz frame of unpainted material at
the new size on expand, and one frame of stale content clipped by the smaller window on
collapse.** The page knows its new size within 2–6 ms (its `resize` report reaches Rust 2.0–5.7 ms
after route S, 4.0–8.7 ms after route T), but WebKit's first paint at the new size lands one
display period after the window server composites the new frame — the same mechanism as M2c's
pane flash, with a frame change as the trigger. The animated route (`setFrame:display:animate:`,
AppKit's default 0.231 s for this delta) showed **14 frames** of material sliding open with the
page frozen at its old layout throughout, then the same one-frame pop.

**Decision D2, 2026-09-18: the resize is not animated.** One synchronous `setFrame:display:`
carrying size and position together (route S). Animation hides nothing and adds a quarter second
of empty material; and **no shorter duration can rescue it**, because the page stayed frozen through
all 14 frames — shortening only shortens the frozen window until it degenerates into the instant
case. The question is shut on the measurement, not on taste. `docs/BUILD_PLAN.md`'s "smooth" and
"animates" wording is amended accordingly.

**P3 — hidden vs visible resize.** Both land in the same frame, anchor and occlusion state. They
differ in one thing: a hidden WKWebView **does not lay out at all** — no `resize` report until the
window is shown, then 23–111 ms after the show — and under capture the hide→resize→show path
composites no wrong-size frame but replays the M2c pane flash on JS-driven content (CSS layout
current in the first composite, React-rendered content 1–2 frames behind). It also blinks the
popover off and on, which is not "grows in place".

**Decision D3, Martín, 2026-09-18: the page-commit round trip is in M2d.** M2c review finding 8
(the one-frame pane flash) had been deferred because "M3 replaces that page". Step 0 measured the
same mechanism as M2d's **own** resize artefact, which M3 does not remove, so the deferral's reason
expired and the decision was reopened per "Principle: a measurement that contradicts a recorded
justification reopens the decision". Design: Rust emits the layout the page should render (view,
height state, width, height, expandable flag, a generation counter); the page reports its React
commit for that generation; Rust completes the visible change then — `setFrame` for a resize,
`orderFrontRegardless` + `makeKeyWindow` for a show — with a **fallback timer** so a dead or slow
page cannot wedge the popover, a stale report a logged no-op, and a hide cancelling anything
pending. "Commit" is the DOM commit, not a paint: a hidden page runs no rendering updates (P3), so
a report from `requestAnimationFrame` would never arrive for a show. This fixes finding 8 by
ordering; whether it also removes the unpainted band on a visible expand depends on WebKit having
rasterised the page's pre-laid-out overflow before the frame change — a hypothesis M2d measures at
acceptance, not a result. The timer's value and the measured round-trip latency are recorded with
the acceptance results.

**Decision D4, Martín, 2026-09-18: when expansion is refused (D1's floor), the expand control stays
visible and disabled.** The chrome does not differ between displays. The short-display configuration
is a fringe case and is accepted rough: implement the shape, spend no polish budget on it. Rust
refuses regardless of the control's state; the page never decides.

**Decision, 2026-09-18: the height state persists across hides within a run.** A hide has no side
effect (the property the one-hide-path consolidation at M2c was protecting), and the next show
recomputes the frame for the display the icon is then on, so an expanded panel hidden on the ANMITE
and reopened on the built-in gets 720 pt, not 598. An earlier doc comment in `panel.rs` said "M2d
collapses the expanded state" — that was a prediction, not a decision.

**P2 — is the shadow recomputed on a frame change? Yes, and the result is UNFALSIFIED.** On a
uniform backdrop with a 0-level threshold (the two absent-reference captures were pixel-identical),
the shadow's bottom edge moved 948 → 1108 px with the body's 160 px growth and stayed 47 px below
it, **without** `invalidateShadow()`; calling it changed nothing. The control — halving the blur
view with no frame change, the content-alpha case `invalidateShadow` is documented for — was
recomputed automatically too, so the probe never demonstrated it could see a stale shadow.
Verdict: no staleness observed, no control demonstrated. **The implementation calls
`invalidateShadow()` unconditionally after every frame change as insurance, not as a fix**
(58–107 µs on the main thread). The corner **contour** fit read 29.98 pt, rms 2.30 px, in all five
captures including the control: it is not a weak instrument, it is an instrument pointed at a
constant — the corner radius convolved with a fixed blur kernel is a property of the rendering,
not of the state — and it is recorded as such, not as corroboration. Closes the Loose-ends item.

**Instrument lessons, the sixth and seventh instances of "verify the instrument":** the
`setContentSize:` derivation above (a source-derived claim about what a selector *does*), and an
opaque probe overlay that kept the window's alpha opaque everywhere and so masked the very control
it existed to enable — a direct repeat of M2c's lesson that an instrument which changes the
quantity it measures reports its own shape. Both are appended to the principle section.

**Open, watched at acceptance:** in three P3 runs the panel resigned key 1.5 s / 1.5 s / 62 ms
after its first show; one was the probe's own helper launching, two are unexplained. No P3
quantity depends on key status. This is the re-check M2c P7's closure asked for ("if a second
window ever returns, re-check"); a recurrence with hands off is a defect.

**Acceptance, 2026-09-21 (Martín; `m2d-acc-02` on the ANMITE, `m2d-acc-03` on the built-in,
both `57bc490`, detached, `.err` empty; the chat's reading in `_handover/OPEN.md`).** Every
figure measured from those logs.

- **Item 1, the built-in hosting the menu bar — passed.** 16 expands to 360×720 and 16
  collapses, some ~230 ms apart; every one `trigger=commit`; `inside_tray_screen_visible=true
  gap_below_icon=6` on all 39 `panel placed` lines; no fallback, no WARN. By eye: grows and
  shrinks in place, no jump, shadow at both sizes.
- **Item 2, the ANMITE hosting the menu bar — passed, and an independent fixture.** Expand →
  `size_points=(360, 598) capped=true expandable=true`, `apply_frame … cocoa=[254,6 360x598]` —
  bottom 6 pt above the display's edge, gap 6 on show, expand and collapse. The icon sat at
  x = 844 physical, not P4's 788, so this is not a replay of the unit test's fixture.
- **Item 5, the round trip — measured, n stated.** `after_ms` on the **visible** path
  (resizes): 1–4 ms, n = 32 (`m2d-acc-03`; two more in `m2d-acc-02` at 0 and 1). On the
  **hidden** path (shows): 2–13 ms and one **100 ms** (generation 34, straight after the resize
  burst), n = 8 across both runs. Short of the planned ≥ 20 shows. **`LAYOUT_FALLBACK` stays
  provisional at 250 ms** — 2.5× the observed maximum, the rule its doc comment states — until a
  run with ≥ 20 hidden shows exists; the constant's comment carries this n. No `trigger=fallback`
  on a healthy page in either run. The dead-page step (a stopped WebContent process) was not run.
- **Item 6, the unpainted band on a visible expand — UNMEASURED, not passed.** The plan's
  hypothesis — that laying the page out at the target height before the frame changes lets WebKit
  paint the new band in the same frame the window grows — has no capture behind it. The
  measurement needs the P1b instrument on the ANMITE (review F3) and a hand on the Expand control
  at a known moment; it could not be driven unattended (System Events keystrokes are not
  authorised from the harness's shell) and Martín's runs did not include a capture. Recorded as an
  open measurement, not as a result: by eye Martín saw no artefact at item 1, which is what a
  16.7 ms frame looks like to an eye and is therefore not evidence either way.
- **Item 7, M2c review finding 8 — closed by eye.** About → Back logs `panel view back
  from=About to=Transport`, the next show is `view=Transport`, and Martín saw **no pane flash**
  where he had seen one on 2026-09-18. Closed by eye, not by capture; the mechanism (the show is
  ordered in after the page's DOM commit) is the round trip's, measured at item 5.
- **Item 8, regression — passed.** Esc: `reason=esc effective=true` + one no-op; right-click:
  `reason=menu effective=true` + one no-op; every close one effective hide + one no-op.
- **Items 3 and 4** (the refusal floor; a visible panel on another display than the icon's) —
  not reachable by hand on this hardware and stated so in the plan; covered by the unit tests and
  by construction (no size-only path exists) respectively.
- **G3 watch.** Four `resign_key effective=true` in `m2d-acc-03`, all hands-on dismissals —
  including one at +1.48 s after a show, the interval P3's unexplained runs showed, which Martín
  confirmed was his click outside the popover. No hands-off resign was produced, so P3 runs 10
  (+1.53 s) and 11 (+62 ms) stay unexplained; the item stays open as a watch into M3.
- **One defect, found by eye:** the Expand/Collapse control rendered on the About pane. **Decided
  2026-09-21 (Martín): it does not belong there.** Fixed in `a5f6d2e`: the page renders the
  control on the transport only; Rust refuses a resize from About regardless (`reason=view`);
  About opens at the collapsed height whatever the user chose, the choice survives (`chosen`,
  set only by a successful resize), and Back restores it through the resize path. Three tests pin
  it. This reverses the plan's "chrome on both panes", which was Code's choice, not a decision.
- **The review's C1 fix verified at item 7:** the `panel view back` line is the report that keeps
  a later resize from re-asserting About.

### M2d: the expanded height is capped to the work area (decided 2026-09-18)

**Decision D1, Martín, 2026-09-18.** The expanded popover's height is
`min(NOMINAL_EXPANDED, usable height)`, where `NOMINAL_EXPANDED` is the ~720 pt of the product
shape above and the usable height is what the existing M2b point-space code can give a panel
anchored under the tray icon. **As implemented (`panel.rs`, `layout`, gate 2 push-back 3, review
finding 1):** the height between the panel's top edge — `TRAY_GAP` below the icon's bottom edge,
where `centred_below` puts it — and `EDGE_MARGIN` above the bottom of the work area of the display
the icon is on: `usable = (work_area.bottom − EDGE_MARGIN) − (icon_bottom + TRAY_GAP)`, 6 pt each.
This **reduces to the form first recorded here** — the work-area height minus `TRAY_GAP` and
`EDGE_MARGIN` — whenever the icon's bottom edge coincides with the work-area top, which is true on
every arrangement measured (ANMITE: tray rect 60 px / 2 = 30 pt = `work_area.y` 30; built-in:
66 / 2 = 33 = 33), so the number is the same, 598 on the ANMITE. The general form was preferred
because the panel hangs from the *icon*, not from the work-area top: should the two ever differ,
the recorded form plus `clamp_into` gives a panel pulled up to gap 0 — the readout that means "the
clamp fired" — where this one gives a shorter panel with the clamp idle by construction. The
decision is unchanged; only its expression is. No second positioning path: the cap feeds the same
`anchor_points` / `clamp_into` that places the collapsed panel.

**Floor: expansion is refused only when the capped height would not exceed the collapsed height**
(420 pt today). That is the only bound justifiable now, and it is recorded as **PROVISIONAL**. The
real product floor — how short a map pane stops being worth showing — is derived at **M4** from the
map's minimum legible pane, and M2d must not invent a number for it. A named constant with a
fabricated justification is exactly what this project's principles forbid ("Principle: an
assertion must be able to fail on the quantity it pins"). Owner of the floor: **M4**.

**Consequence to design for: "expanded" is a function of the display, not a constant.** Pane
layout, any future screenshot, and the event the webview receives all tolerate a variable expanded
height. The webview is told the height; it never computes it (CLAUDE.md, "The one rule").

**Why cap, and not the other two options.** Measured 2026-09-16 while the ANMITE hosted the menu
bar: it is 960×640 pt with a work area of (0,30) 960×610 pt, so a 720 pt panel cannot fit while a
420 pt one does. Scrolling inside a fixed 720 pt content area keeps the product constant but puts
a scroll view inside a popover on a short display, and the map pane at M4 scrolls badly. Refusing
to expand on a short display is honest and simple, but silently disables a feature on one of the
three displays attached here. Capping keeps the map, which wants area rather than a specific
height, and costs only that the expanded height varies.

**On the ANMITE this gives 598 pt (610 − 6 − 6) — measured 2026-09-18, M2d Step 0 P4** (an
earlier version of this paragraph recorded the figure as derived, which under this project's tag
discipline was a correctness bug in the document once the measurement existed). The ANMITE hosted
the menu bar, work area measured (0,30) 960×610 pt at launch; the probe computed
`work_area_height_pt=610 usable=598 nominal=720 capped=598 refuse_expand=false` and the panel
landed at `[226,36 360×598]`, 6 pt below the icon with its bottom 6 pt above the display's edge.
The pre-cap control — 720 pt through the unchanged show path — was pulled to the work-area top by
`clamp_into` (gap 0) and still ran 110 pt off the bottom of the display: the clamp cannot fit a
720 pt panel into 610 pt; the cap is what does. **The gap below the icon is the sensitive
readout: 6 pt means the clamp was idle, 0 pt means it fired.**

**The refusal branch was not exercised by any attached display** (no work area here makes
`capped ≤ 420` true), so it is covered by a unit test driven from a synthetic work area on both
sides of the boundary — the comparison is one that an inverted or off-by-one version would ship
unnoticed, and an inverted one refuses on *every* display, not just the short one.

D2, D3 and D4 are recorded in "M2d: resize in place — Step 0 measured, D2–D4 decided" above.

### M2c: chrome and input, measured (2026-09-16/17)

Branch `m2c`. Step 0 was an uncommitted probe (`_handover/m2c-step0-logs/probe.patch` is its
byte-exact record; report `_handover/m2c-step0-report.md`), on the bundled debug build, macOS
26.6.2 (25G83), built-in display at 2×. Martín performed every gesture. Every number here is a
measurement unless marked as a decision.

**What was measured, and what it decided.**

1. **Escape reaches the webview's JS in both phases** — panel key with Safari still the active
   app and no click inside the panel, and after one click inside — 3/3 each, and the WKWebView
   is first responder from `t+0` after plain `make_key_window()` (the `panel.rs` claim written at
   M2a, now tested). So Esc is a page `keydown` → `panel_escape` → `panel::hide(reason=esc)`;
   no native subclass or key monitor. The unhandled key beeped in the probe, louder after an
   in-panel click; the shipped listener calls `preventDefault()`, checked by ear at acceptance.
2. **`show_menu_on_left_click(false)` is required.** Run A left tray-icon's default `true`:
   7 left-clicks → 7 `Click{Left,Down}`, 0 `Up`, 0 toggles — the menu's modal tracking loop
   swallows `mouseUp:` and the toggle keys off `Up`, so the tray icon merely showed the menu.
   **Right-click emits `Down` only** (5/5, never an `Up`): right-click logic keys off `Down`.
   Opening the menu does **not** resign a visible panel's key status, so the hide on right-`Down`
   is explicit — measured to run on main in 5.6 ms and to land visually before the menu (R5).
3. **The standard About panel opens behind the frontmost app**: created, `visible=true`,
   `level=0` (`NSNormalWindowLevel`) while the app is inactive. Decided: About is a pane inside
   the popover, and `PredefinedMenuItem::about` is not used. A 3 s window sampler first missed it
   entirely — the menu was still open — hence the instrument lesson below.
4. **`EffectsBuilder::radius` works, and the window shadow follows the corners.** An earlier
   reading of the same probe said `radius` was inert and recommended the documented `maskImage`;
   it was **retracted** (see the null-test lesson below). Calibrated against known radii, `radius`
   and `maskImage` produce identical arcs (0.75 / 9.95 / 12.35 pt measured for 0 / 8.5 / 12 pt
   set), as do a layer on the effect view and a layer on `contentView`. Shadow contour: **20.40 pt
   for a square body, 29.40 pt for a 17 pt body** — the shadow derives from the window's
   composited alpha, so whatever rounds the material rounds the shadow, and `invalidateShadow()`
   adds nothing. The evidence is the *difference*, not the absolute: blur rounds a square corner
   on its own. **Radius token 8 pt** — Control Center measured 7.6 pt by a calibrated threshold
   fit (−0.8 pt) and ≈ 8.3 pt by a differential match; the spread is backdrop-contrast
   dependent (repeatable to 0.00 pt within a backdrop, ~0.6 pt across). Confirmed at acceptance
   by one capture holding both the panel and Control Center. Re-measure if the OS major version
   changes.
5. **Single instance, six cases.** (a) `open Ondar.app` and (d) a Finder double-click →
   `RunEvent::Reopen` on the main thread, no plugin callback — LaunchServices starts no process
   ((d) fires twice). (b) `open -n`, (c) the inner binary, (e) **a copy of the bundle at another
   path**, (f) `pnpm tauri dev` → the plugin callback on `tokio-rt-worker`. (e) is what
   justifies the plugin: LaunchServices does not dedupe by identifier across paths. (f) was real
   dev friction — the dev instance handed off and exited — fixed by the dev identifier
   (`tauri.dev.conf.json`, `pnpm tauri:dev`). After `kill -9` the socket survives, and the next
   launch removes and rebinds it; the callback fires again (R7).
6. **`TrayIcon::rect()` equals `Click { rect }`**: 11/11 clicks, on the main thread in
   21–198 µs, plus 71–80 µs from the `Reopen` handler. Both are tray-icon 0.24.2's one
   `get_tray_rect`; the measurement closed the step source could not (same `NSWindow`).
7. **Quit with a stream playing**: `PredefinedMenuItem::quit` is `terminate:`; the process
   exited 0 (against a 143/143 SIGTERM control), no panic, `pgrep -x ondar` empty. **The engine
   is not shut down; the process is**, and the OS reclaims the device. Recorded as the behaviour.
8. **The setup-time self-resign is gone with `main`**: 3 launches × 5 samples, 15/15 key.
   The mechanism was never identified; the condition was removed, not explained.
9. **The double-hide re-measured** at +2.2 / +2.4 / +3.7 ms, and consolidated: one
   `panel::hide(reason)` and one `panel::show_at(rect, reason)`, each logging `effective=`, so a
   close reads as one effective hide and one no-op.

**Decisions (Martín, 2026-09-16/17).** Right-click with the popover open hides it first,
`reason=menu`. About lives in the popover. `tauri dev` gets `<id>.dev` through a dev-only config
overlay; bundles keep the real identifier. A second launch with the popover already visible
leaves it visible (logged no-op). The page's view follows the show reason (`panel:view`,
`"about"` for the About item, `"transport"` otherwise), so About never outlives a hide. Esc calls
`preventDefault()`. Quit stays `terminate:`, recorded rather than replaced. The M1 bench retired
into the popover as a dev transport — presets, play/pause/stop, volume, no EQ sliders (M1 exit
criterion 4 is unexercisable by hand until M5; the engine claim is held by `eq::tests` and
`eq_headroom_sweep`).

**Design tokens.** `src/styles/tokens.css` values are measurements on this Mac (macOS 26.6.2,
2026-09-17T18:18:44Z): `NSFont.preferredFont(forTextStyle:)` sizes and line heights, and the
semantic `NSColor`s resolved in sRGB under both appearances
(`_handover/m2c-step0-logs/measure-tokens.swift`, raw stdout in `tokens-measured.txt`). They
coincide with Apple's published values, which is what a correct measurement of a system value
looks like. The 8 pt rhythm and the 1 px hairline are decisions, not HIG citations — the HIG
prescribes no grid. `scripts/check-tokens.sh` fails `pnpm lint` on a colour or length literal
anywhere else in the renderer; the panel radius token is pinned to `PANEL_CORNER_RADIUS` by a
shell test that reads the stylesheet.

**Instrument lessons, promoted from the Step 0 candidates.**

- **A sampler shorter than the human it waits for measures the wait.** Item 3's first sampler
  expired while the menu was still open and reported the menu, not the About panel.
- **An instrument that occludes the quantity it measures reports its own shape.** Three times in
  item 4: opaque corner markers on the arc, hollow markers still on the arc, and an edge detector
  that found the shadow's straight offset and called the body square.
- **Calibrate a method against known values and state the conditions it holds under.** The
  corner fit was repeatable to 0.00 pt within a backdrop and drifted 0.6 pt across backdrops;
  consistency is not accuracy, and a calibration carries its conditions with it.
- **A null test reads exactly like a negative result.** One environment variable fed two
  rounding mechanisms, so the run meant to isolate `EffectsBuilder::radius` applied no radius at
  all, and "inert" survived a review on the strength of it. When a probe reports no effect, prove
  the mechanism was switched on. It was caught by accident — an unrelated capture set the variable
  the null test had omitted.
- **Never present a summariser's rendering as a verbatim quote.** The `maskImage` header text a
  review quoted came from a WebFetch summary; the header on this Mac adds "(It does not also mask
  subviews.)", the sentence that decided what the option was worth.

**Acceptance, 2026-09-18 (Martín on the bundled build, detached launch; logs and captures in
`_handover/m2c-acceptance/`).** All eight items passed or were measured:

- **Esc does not beep**, in either phase — `preventDefault()` on the page's `keydown` stops the
  key from reaching `cancelOperation:`. The probe without it beeped; this is the difference.
- Right-click with the popover open: gone before the menu. About → Back → transport; About, Esc,
  tray click → transport. Quit with a stream playing: audio stopped, no instance left.
- Second launch: `open`, `open -n`, Finder double-click and the `/tmp` copy all showed the
  popover (three `reopen`, three `second_instance` in the log). The already-visible no-op path
  was not exercised.
- Every close in both logs is one `effective=true` and one resign-key `effective=false`.
- **Control Center and the popover cannot be on screen together** — opening Control Center
  resigns the panel's key status and the dismissal design hides it — so the planned
  single-capture comparison is impossible on this OS. Instead, two captures per appearance on
  one desktop, each the other's absent reference (the R8 differential): the popover's top-left
  arc fits **9.18 pt (light, 0.17 px rms) / 9.40 pt (dark)** with the same threshold-and-fit
  family that read an 8.5 pt setting as 9.35–9.95 pt at Step 0 — i.e. the 8 pt setting renders
  as intended. Control Center's outer glass corner could not be fitted: it differs from the
  backdrop by less than its own shadow does at every threshold. Step 0's dark-appearance
  measurement stays the token's provenance; by eye the two corners are comparable.
- **A one-frame flash of the previous pane** on About from the menu, and on the first tray click
  after Back or Esc: the view event is async (JS, then a React commit) and `orderFrontRegardless`
  is not, so the retained WKWebView layer is composited once before the new pane. Predicted by
  `/code-review` as plausible, now measured. **Deferred** at M2c: the fix is a round trip (the page
  reports its commit, Rust orders in then, with a fallback timer), owed to whichever milestone
  keeps the About pane — M3 replaces this page. **Reopened and taken into M2d as decision D3**
  (2026-09-18): M2d Step 0 measured the same mechanism as the resize's own artefact, which M3 does
  not remove. See "M2d: resize in place — Step 0 measured, D2–D4 decided".

### M2b: coordinates are logical points (decided and measured 2026-09-16)

Branch `m2b`. `4f14213` recorded the Step 0 instrument findings; `0331ace` is the fix.

**Decision: route B — global logical points throughout, top-left origin.** The reason is measured,
not aesthetic. Tauri reports each monitor's `position`, `size` and `work_area` as global points
multiplied by **that monitor's own** scale (tao `monitor.rs:225-231`; tauri-runtime-wry
`monitor/macos.rs:16-27`), verified against the Cocoa frames in both arrangements, 6 of 6 exact. So
on a mixed-scale layout **there is no common physical space**: two monitors' values are not
comparable, and a tray rect (physical at the *status item display's* scale, tray-icon
`mod.rs:515-528`) cannot be compared with a work area built at another monitor's scale. Route A —
resolve the monitor, keep working in physical — has to convert to points to compare anything, so it
is route B applied per comparison. Points are also what `monitor_from_point` and
`set_outer_position` already want.

**What the defect actually was.** `anchor()` divided by `window.scale_factor()` — the panel
window's own scale, which is the scale of whichever display the panel happens to be **sitting on**
(`NSWindow::backingScaleFactor`, tao `window.rs:885-887`). That is a better description than the
`/code-review` finding's: the finding said the panel would land on the *wrong display*. Measured
2026-09-16 with the panel forced onto the 2× built-in and the icon on the 1× BenQ, it landed on the
**right** display, 649 pt left of the icon and 12 pt inside the menu bar band — so a
display-identity assertion would have passed it. The tests assert the panel lies inside the chosen
display's work area instead.

**Why the tray path had never shown it.** Three arrangements, three correct landings, for three
different geometric reasons (measured, `_handover/m2b-step0-logs/`): the panel is only ever shown
under the tray icon, the tray is on the menu-bar display, the menu-bar display is at the Cocoa
origin, and a hidden window keeps its numeric Cocoa frame while the displays move around it — so
the panel is dragged onto whichever display becomes the new menu-bar display, and the two scales
agree by geometry. Forcing the panel onto a chosen display with `setFrameOrigin` is what produced
the failure on demand. **Latent but reachable**: M2d's resize-and-reposition will move a panel that
is already showing on another display.

**How point space is entered, and why that way.** The rect does not carry its own scale, so it is
recovered by dividing the icon's centre by each monitor's scale and keeping the monitors whose point
bounds then contain it. The alternative was the status item's own `NSWindow.screen()` through
`tray-icon`'s internals — exact, but it needs a live AppKit window and the main thread, which would
put the whole decision outside unit tests, and the bounds test is needed for clamping anyway.
`PointRect::contains` is half-open so a display seam belongs to exactly one display; more than one
acceptor, or none, is **logged** rather than silently resolved.

**Same-class defect fixed with it: `TRAY_GAP` and `EDGE_MARGIN` were physical pixels.** The same
constant was 3 pt of visible gap on the 2× built-in and 6 pt on the 1× BenQ (measured both ways).
They are points now, at 6 pt — the value the 1× display has been showing, and in the range macOS's
own status item menus leave. That number is a judgement; the unit is not.

**Verified against the case that failed** (2026-09-16, `probe-05-fix-verified-mixed-scale.log`):
panel forced onto the 2× built-in (`panel_scale=2` at the click), icon on the 1× BenQ, rect
`(1314,0) 24×30` → `display=Some(0) accepted=[0] position_points=(1146,36)`, landed
`[1146,624 360×420]` on the BenQ, `inside_visible_frame=true`. Before the fix the same case gave
`(469,18)` and `inside_visible_frame=false`. The panel's own scale still read 2 and no longer
changed the result.

**Post-review fixes (`7ce7869`, `/code-review` on `main...734eb9b`, all five findings confirmed
against the source before changing anything; `_handover/m2b-review-findings.md`).**

- **Ambiguous acceptance is resolved towards the primary display, not towards list order.** Two
  displays of different scale can both accept one rect — a 2× primary with a 1× display to its right
  does so for essentially every icon position — and `accepted.first()` meant trusting
  `CGGetActiveDisplayList`'s order, which nothing here relies on deliberately. In all six measured
  runs the primary was index 0, so this had never shown; that is an inherited assumption, not a
  measurement.
- **"The status item is on the main display" is a user setting, and the code no longer assumes it.**
  It holds here because `NSScreen::screensHaveSeparateSpaces()` measured **false** — and that is
  System Settings → Desktop & Dock → "Displays have separate Spaces", which gives *every* display
  its own menu bar when on, and then a status item can sit on a non-primary display. On this Mac
  `defaults read com.apple.spaces spans-displays` returns `1`, and the key's presence means someone
  turned the setting off explicitly; the shipped default is the setting **on** (inherited, not
  verified here — Apple's documentation was not available offline). So the primary is a *preference*
  among the acceptors: when it is not among them, `anchor_points` falls back to the first acceptor,
  still clamped, and logs `AmbiguousWithoutPrimary`. Same class of error as "one display here":
  an environment-dependent fact treated as a property of the platform.
- **The no-acceptor fallback clamps again.** It had reinterpreted a physical rect as points and
  skipped clamping — off screen on a 2× display, and worse than the `primary_monitor()` fallback it
  replaced. It now uses the primary's scale and work area, clamped, and only assumes points when
  there is no primary at all.
- **The panel-scale division moved into the pure function**, because in `anchor()` no test could
  reach it — see the principle candidate below.
- A fixture figure was corrected: the built-in's arrangement-1 work area is **950 pt**
  (`3024x1900` in the log), not 949; the 32 pt inset there is the notch band, not a menu bar.

**The notch needs nothing.** The built-in reports `safeAreaInsets.top = 32` and an
`auxiliaryTopLeftArea` of `[·,−32 663×32]` whether or not it hosts the menu bar, and its
`work_area` already excludes that band; the panel hangs below the icon and is clamped to the work
area, and at 360 pt wide on a 1512 pt display it cannot reach either side of the notch.

### M2a: the tray path, measured (2026-09-15)

Branch `m2`: `36d50b8` (dependency), `e1f252f` (tray icon), `9f29edf` (panel, anchor, clamp),
`791fd66` (resign-key dismissal). Each pushed alone; CI green on each.

**Decision: the resign-key hook is `WindowEvent::Focused(false)`, not a `panel_event!` delegate.**
Decision 1's substance — the popover hides when it resigns key, replacing `hides_on_deactivate` —
is unchanged; only the hook moves. tao's window delegate already emits `Focused(false)` from
`windowDidResignKey:`, so listening on the panel's `WebviewWindow` gets the same AppKit callback
with no new `unsafe`. `Panel::set_event_handler` would replace tao's delegate: it stores the
original but restores it only when the handler is set back to `None`, and forwards nothing while
installed, which silences tao's `Resized`, `Moved`, `Focused`, `ScaleFactorChanged`, `ThemeChanged`
and `Destroyed` for the panel window — events M2b's resize-in-place work needs.

**Step 0 — the gate (probe, never committed).** The panel was shown without a tray click, from an
env-driven hook calling the same show path, on the bundled debug build.

| Question | Result |
|---|---|
| Real subclass | `class=OndarPanel` in every run |
| Becomes key | `key=true` straight after `make_key_window()`; still `true` at +1323 ms when shown after launch |
| Steals focus | No: `lsappinfo front` unchanged across show (Finder; TextEdit; Firefox in later runs) |
| `Focused(false)` fires | 33 ms after `open -a TextEdit`; again on `open -a Finder` |
| Vibrancy on the real panel | effect view present, 360×420, by view tree; no visual confirmation |
| `hides_on_deactivate(true)` on the real panel | `Visible` bit clear at +60/+319/+1322 ms; control with it `false`: set by +50 ms |

**A synchronous occlusion read after show is stale.** The `Visible` bit read straight after
`orderFrontRegardless` + `makeKeyWindow` was clear (raw 8192) in 14 of 14 probe runs; in the 13
without `hides_on_deactivate` it was set at the first later sample. Sampled every 10 ms in 6 runs,
the bit flipped within (12, 24], (12, 27], (14, 30], (15, 23], (21, 34] and (23, 35] ms. The
product reads it 100 ms after show (~2.9× the worst upper bound, 35 ms) and logs the elapsed time
actually observed.

**Showing the popover during `setup()` makes it resign key by itself — bisected to the M1 bench
window, mechanism unidentified.** Shown from inside `setup()`, the panel was key at show and had
lost key by the first sample (+51 to +107 ms), with no focus change and never regained it:

| `main` (M1 bench) window | Runs | Panel kept key |
|---|---|---|
| created visible (as on `main`) | 3 | no, 3 of 3 |
| created visible, `hide()` inside `setup()` | 1 | no |
| `"visible": false` in `tauri.conf.json` (temporary build) | **1 — n = 1, not repeated** | **yes** |

- `main` itself was **never key** in any run, so this is not another window taking key. What
  removes the effect is `main` never being ordered in; hiding it after creation does not. **Why
  that clears the panel's key status is not identified.** The 4-vs-1 comparison rests on a single
  run for the `visible: false` arm and is recorded as suggestive, not settled.
- The "became key" and "resigned key" lines logged ~20 µs apart at +51/+56 ms are event delivery,
  not a hand-off: `setup()` holds the main thread, so `Focused(true)` is queued and delivered with
  `Focused(false)` once the loop runs. Shown 3 s after launch, `became key` logs at show.
- Every loss was within ~100 ms of launch; shown later with `main` visible, the panel kept key for
  >1.3 s until another app was activated. No tray click can arrive during `setup()`.
- **Prediction, untested:** clicking the bench window while the popover is open would activate
  Ondar, make `main` key, and dismiss the popover. It lives until M2b retires `main`.

**Positioning.** `centred_below(tray_pos, tray_size, panel_size)` and `clamp_into` are pure and
unit-tested (shell tests 3 → 9; workspace 53 → 59). Fixture: the 2026-09-12 rect `(1932, 0)`
`48x66`, panel 360×420 logical at scale 2 → `(1596, 72)`. Near the right edge the centred panel
runs off screen (3024 px display, 720 px panel: past icon x ≈ 2640); **clamp chosen over flipping
to right-aligned**, so the panel stays centred whenever it fits and slides only as far as needed,
6 px from the work area's edge. Each test was mutated to confirm it fails: `y` height-term sign,
`tray_pos.y` sign (caught only by the `y = 100` case — the measured rect has `y = 0`), centring
dropped, icon half-width dropped, clamp removed, margin dropped.

**Manual tray check, bundled build `791fd66`** (Martín's clicks; this shell cannot post Apple
Events, `-1743`). Two sequences — open/close/open/close, then open/close/open/click another app:

- 7 physical clicks → 14 `tray click` lines, one `Down` and one `Up` each.
- Tray rect `(1816, 0)` `48x66` — a different x from 2026-09-12's 1932, because the menu bar's
  other items differ. Anchor `(1480, 72)`; work area `(0, 66)` `3024x1770`; clamp a no-op.
- 4 shows: all `class=OndarPanel key=true`, all `settled_visible=true settled_raw=8194` at
  105–106 ms.
- 3 tray closes: `panel toggle -> hide`, then ~6 ms later `panel resigned key -> hide
  visible_before=false` — ordering out the key window resigns key; the second hide is a no-op.
- Click-away: `panel resigned key -> hide visible_before=true`.
- **No resign between a `Down` and its `Up`**, so the feared re-open race did not occur and the
  pre-agreed timestamp guard was not added.

**Pre-merge `/code-review` (2026-09-15): two low-severity findings, both confirmed against source.**
Verified line by line in `_handover/m2a-review-findings.md`. (1) The tray swap was `set_icon` +
`set_icon_as_template`, two main-thread tasks with a possible flat-black frame between, repeated on
every state change — fixed in `ede2dd1` (atomic `set_icon_with_as_template`, swap only on an
idle/playing flip). (2) Mixed-scale positioning — recorded under "Multi-monitor caveat" above, not
fixed at M2a.
- `lsappinfo` on the running bundle: `"ApplicationType"="UIElement"`.

### Reconnect ownership and stream timeouts (measured 2026-09-08, M1)

**Ondar owns all reconnects.** `stream-download`'s internal reconnect is a file-download
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
Both values are env-overridable (`ONDAR_READ_TIMEOUT_SECS`, `ONDAR_RETRY_TIMEOUT_SECS`),
so `stream.rs` clamps `read_timeout` to `retry_timeout * 2` and warns if the invariant
is violated.

**The connect bound covers DNS (measured 2026-09-21, M3a G4b).** The M3 Step 0 report
*derived* from a 12-minute hang of its own census client that reqwest's `connect_timeout`
does not bound DNS resolution, and gate 1 asked M3a to measure it on the audio path or
bound the connect phase outright. Measured first, with a resolver injected through
`ClientBuilder::dns_resolver` whose future never completes: `stream::open` against the
production client (`connect_timeout` 10 s) returned `Network` at **10.01 s**; against a
200 ms bound it returns within the second. So on the engine's path a stalled resolver is
bounded by `connect_timeout` and no extra bound is needed; the test
`dns_resolution_is_inside_connect_timeout` pins it (a hang there would trip the test's 5 s
guard). The census client's own hang therefore has an **unexplained** cause — it had the
same 10 s `connect_timeout` — and is recorded as such, not as "DNS". Instrument instance
eleven below is corrected accordingly.

**Two upstream bugs in `stream-download` 0.24.4, not reported upstream** (decided
2026-09-09 — the findings are recorded here rather than filed; revisit if either
starts costing us): `handle_reconnect` tests only the outer `timeout` result, so a
failed reconnect (e.g. a 416 to a retried range request) still fires `on_reconnect`
and leaves the loop polling a dead stream — a spin, measured at 125,253 log lines /
28.8 MB over 14 s; and the fast-`Err` path above, which is jointly `reqwest`
0.13.4's `ReadTimeoutBody` not clearing its elapsed sleep on the error return
(`async_impl/body.rs:351-353` skips the reset at `:358`) and `stream-download`'s
`handle_bytes` returning `Continue` with no backoff on repeated `Err` — measured at
4.15M log lines in 40 s. Neither is fixed in Ondar. A post-M1 pass should add an
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
there was an artefact of the harness, not of Ondar.

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

The previous version of that table could not be trusted, and the reason was not in Ondar.
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

**M3 refinement — built at M3b commit 6 (2026-09-23):** radio-browser's station record carries
`bitrate`, and `play` now carries it to the engine, which sizes the prefetch as
`prefetch_bytes = max(one_decoder_read, RING_SECONDS × bitrate / 8)` before `open`, capped at
half the stream buffer since `/code-review` finding 2 (2026-09-23; the M3b section has the reasoning)
(`stream::prefetch_for`, pure and pinned: the floor wins up to 131 kbit/s, 192 kbit/s is
48 000 B, 320 kbit/s is 80 000 B, no bitrate is the floor). The `max` is load-bearing — the
knee alone starves the decoder at 64 kbit/s, and the test fails without it.

**The first term is pinned to a dependency's internal behaviour.** `ondar-audio` never
constructs a `MediaSourceStream`; rodio 0.22.2 does it internally over symphonia-core 0.5.5 and
chooses the 32768 B read size. It is not a property of decoding and not ours to set, so it
**must be re-verified on any rodio or symphonia bump**. A bump that raises it reintroduces the
spontaneous underruns above, silently. `crates/ondar-audio/src/icy.rs` records the largest
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
retry and never returns it to the decode thread, so **nothing in Ondar could fail the
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


### The app icon and tray glyphs (2026-09-13)

**`src-tauri/icons/ondar-icon-master.svg` is the source of truth.** Vector, 3.5 KB, and it
regenerates any size. Every PNG in `src-tauri/icons/` **derives from it** — the 1024 px master
was rendered from this SVG, and `pnpm tauri icon <master>.png` produced the rest. Verified, not
assumed: `rsvg-convert` at 256 px against the 1024 px master downscaled to 256 px gives
RMSE 0.0085, which is anti-aliasing and nothing else. Regenerate from the SVG rather than
upscaling any PNG.

> Rendering the SVG needs `rsvg-convert`. ImageMagick on this machine has **no `rsvg`
> delegate**, so `magick file.svg` silently falls back to its internal MSVG parser, which
> mangles the gradient — it reported RMSE 0.19 for an image that is actually identical. A
> comparison made that way proves nothing.

**The blocker is gone, proven the only way that counts:** `pnpm tauri build --debug` with **no
`--config` override** completes, exit 0, no `No matching IconType`, and the bundle carries
`Contents/Resources/icon.icns` with `CFBundleIconFile = icon.icns`.

`tauri.conf.json`'s `bundle.icon` now lists the real set (`32x32.png`, `128x128.png`,
`128x128@2x.png`, `icon.icns`, `icon.ico`) instead of the lone placeholder.

#### Tray template glyphs

Four in `src-tauri/icons/tray/`: `ondar-tray-{22,44}-{idle,playing}.png`, authored as 22 px for
@1x and 44 px for @2x. **Only the 44 px pair is used, and the 22 px pair can never render** (found
2026-09-15): `tray-icon` builds the status item's `NSImage` from one PNG — one representation, no
way to supply a second — and forces its height to 18 pt (tray-icon 0.24.2
`platform_impl/macos/mod.rs:283-311`). So the shipped glyph is the 44 px file resampled by AppKit
to **36 px at @2x and 18 px at @1x**. The glyph sits on a 22 pt canvas (alpha bounding box at 44 px:
x 4–39, y 6–41), so it draws at about 29 px at @2x.

- **They are pure black on alpha — measured, max RGB channel value 0 across all four — so they
  must be set with `icon_as_template(true)`.** macOS then reads shape from the alpha channel
  alone and tints for light/dark menu bars and the highlight state. Setting them without that
  flag renders them as flat black artwork that disappears on a dark menu bar. `tray-icon`'s
  `set_icon` resets template mode, so the playing swap uses `set_icon_with_as_template`, and only
  on an idle/playing flip (`ede2dd1`).
- **The playing state is a SHAPE change, never a colour change:** the cap dot above the stem is
  *hollow* when idle and *filled* when playing. A template image has no colour to change — that
  is the constraint the design is built around, not an accident of these files.

**The state difference, measured on what renders (re-derived 2026-09-15).** The earlier table
measured the source files — a 22 px file that is never drawn, and a 44 px source rather than its
36 px result — so it described neither shipped size and was withdrawn, not annotated.

*What is measured, and why, written before the numbers were taken.* The quantity is the alpha
difference between the idle and playing renders at the backing sizes the menu bar draws, 36 px and
18 px, using the earlier table's two thresholds (any difference; more than 25 %). Alpha, because a
template image is tinted through its alpha as a mask, so coverage is what reaches the screen. The
instrument is AppKit's own path: `NSImage(data:)` from the 44 px PNG, `size` set to 18 pt, drawn into
a bitmap at 1× and 2×. The status bar button's interpolation setting is not visible from outside, so
every `NSImageInterpolation` level is reported as a range. Second instrument: `sips -z` (ImageIO).
Calibration: the same counting code on the source files must reproduce the earlier 16/4 and 38/16 —
it does, exactly.

| Render (from the 44 px file) | Pixels differing at all | Differing by >25 % | Σ\|Δα\| (post hoc) | max \|Δα\| |
|---|---|---|---|---|
| 36 px (@2x), AppKit, 5 interpolation levels | 26–42 of 1296 | 10–12 | 2084–2511 | 247–255 |
| 36 px, `sips` | 42 | 10 | 2163 | 255 |
| 18 px (@1x), AppKit, 5 interpolation levels | 11–17 of 324 | 4 | 348–533 | 84–159 |
| 18 px, `sips` | 17 | 4 | 498 | 148 |
| *reference: 44 px source file* | *38 of 1936* | *16* | *3120* | *255* |
| *reference: 22 px source file (never drawn)* | *16 of 484* | *4* | *944* | *178* |

`sips` agrees exactly with AppKit's `default`/`high` interpolation on every column.

**Σ|Δα| was added after seeing the counts, and is labelled so.** The two count thresholds alone make
@1x look no worse than the authored 22 px file (4 strong pixels either way). They cannot distinguish
a crisp change from a blurred one, which is exactly what a 44 → 18 px downsample of a hollow dot
produces. Total alpha change shows it: **the @1x render carries 37–56 % of the contrast the 22 px
file would have** (348–533 against 944), and its strongest pixel changes by at most 84–159 of 255.
At @2x it carries 67–80 % of the 44 px source's contrast, with a full-strength (255) pixel still
present.

**Known and accepted, re-derived:** the state difference is clear at @2x and marginal at @1x — and
at @1x it is worse than the earlier table said, because the shipped render is a blurred downsample
rather than the authored 22 px glyph. Not a defect to be fixed by tweaking the dot. The two real
options are a two-representation `NSImage` set directly on the status item (bypassing `tray-icon`'s
`set_icon`), which would let the 22 px glyph render at @1x, or a different idle/playing distinction
at that size. Neither is taken; it is an open decision.

### Renamed from Onda to Ondar (2026-09-13)

**Old name:** Onda. **New name:** Ondar — Basque for sand.

**Why:** "Onda" collides with [Onda Cero](https://www.ondacero.es/), a national Spanish radio
network. For an internet radio player that is not a distant collision, it is the same product
category in a market the app will be used in. No radio app or station was found under "Ondar".

Done as one mechanical commit: a tree half-renamed between commits is worse than either state
and useless to bisect.

**How to read an "Onda" you find in this repo.** The rule applied was: does the string *name the
thing as it is now*, or does it *reproduce something literally emitted or recorded at a past
moment*? Reproduced literals are quotations and were kept verbatim — command output such as
`running 34 tests` / `34 passed`, log lines, commit messages, tag and commit SHAs. Everything
else referring to the product was renamed, **including inside dated findings above**: those are
load-bearing present-tense engineering claims that merely carry a date, and freezing them would
make this document read as though it described a different program.

So an "Onda" in this repo is one of exactly three things:

| Where | Why it survived |
|---|---|
| A reproduced literal | It is a quotation of something recorded before 2026-09-13. |
| `~/Developer/Onda` | The working directory is deliberately **not** renamed: it would break the working directory and the folder grant Martín's Claude session uses, and buys only tidiness. This is why CLAUDE.md's layout diagram still has an `onda/` root. |
| A miss | Report it. |

**Follow-ups Martín owns.** The GitHub repository is being renamed `metambuy/onda` →
`metambuy/ondar` in the GitHub UI, after this commit lands, followed by `git remote set-url`.
The docs in this commit already say `metambuy/ondar`, including the Actions run link in "Repo
tooling" — so between this commit and that rename those URLs are ahead of reality. They are
live pointers, not records, which is why they were renamed rather than frozen; left alone they
would survive only on GitHub's redirect and rot quietly.

**Not renamed at all:** branch `m2-spike`, which stays at `3f923cb` as a reference
implementation for M2 proper and still uses the old name throughout.

### CI verifies the head of each push, not every commit (found 2026-09-12)

A GitHub Actions `push` trigger fires **once per push**, and the run checks out that push's head
commit. Every earlier commit in a multi-commit push is never built. The workflow's
`cancel-in-progress` split (added 2026-09-11) fixes a different problem — runs cancelling each
other on `main` — and says nothing about batching.

So the standing rule in `CLAUDE.md`, "small commits, each building and passing checks on its
own", is an **authoring** rule that CI does not enforce. The consequence, stated plainly:

> **A commit that has to stand on its own has to be pushed on its own.**

Found the hard way on 2026-09-12, twice in one session:

| Commit | What happened |
|---|---|
| `f7f04c3` | Pushed batched behind `3f923cb` on `m2-spike`. No run. Its state was established only by pushing it to a throwaway branch (`ci-check-f7f04c3`, since deleted) to force one — it passed. |
| `38c6410` | Pushed batched behind `0927e6c` on `main`. No run. Argued green instead of measured: its code tree is byte-identical to `c45c02f`, which was green, and the only delta is a Markdown file. Sound, but an argument is not a run. |

The `m1-done` tag and every "CI green on X" claim in this document predating 2026-09-12 should be
read against this: green means *that push's head* was green. Where a claim names a commit that was
not a push head, it rests on the same kind of argument as `38c6410`.

**No fix is recorded here on purpose.** Making CI verify every commit is a real change with real
costs — a matrix over `${{ github.event.commits }}`, or a merge-queue, or a pre-push hook — and
it is a separate decision, not something to slip in under a documentation correction.

### Bare `cargo test` skips the engine (found 2026-09-10)

`src-tauri/Cargo.toml` declares a `[workspace]` *and* a real `[package]` at the same root.
For that layout cargo's default scope is the root package alone, not all members — the
"defaults to every member" behaviour belongs to *virtual* manifests (a `[workspace]` with no
`[package]`). So from `src-tauri`:

| Invocation | What actually runs |
|---|---|
| `cargo test` | the `ondar` package only — its own 3 tests, exit 0, no warning |
| `cargo test --workspace` | 53 tests — 50 in `ondar-audio`, 3 in the shell |
| `cargo test -p ondar-audio` | the 50 that matter for the engine |

It reports success either way, which is what made it survive this long — and as of block 2
it is **more** dangerous, not less: the shell crate gained its own tests, so a bare run now
prints a plausible-looking `3 passed` rather than an obviously-empty `0 passed`.

**The mitigation is a test name.** A bare run prints nothing but the three shell test names, so
one of them says what happened:
`log_rate_limit::tests::bare_cargo_test_runs_only_the_shell_crate_see_claude_md`. It is a real
test of the rate limiter's burst behaviour, not a placeholder — the name is carrying a second
job. If that test is ever renamed or removed, the trap goes back to being silent. **Implication worth
stating plainly: any "cargo test passes" claim made before 2026-09-10 needs re-reading against
which invocation was used.** `README.md`'s instructions were fine — they have always said
`cargo test --workspace` and `cargo test -p ondar-audio`. The *verification ritual* in
`CLAUDE.md` and `docs/BUILD_PLAN.md` was not: it said bare `cargo test`, so any milestone
check that followed the ritual as written — M1's included — proved nothing about the audio
engine. Both files are corrected as of 2026-09-10 and CI uses `--workspace`.

Corollary: the count is itself worth pinning down, because 47 is the number you get counting
`#[test]` in source against a reported 53. The other 6 are generated — ts-rs's `#[ts(export)]` expands to an
`export_bindings_<type>` test per exported type, which is the mechanism that writes
`src/bindings/`. `cargo test -p ondar-audio -- --list` is the authority.

### Loose ends

- **Shadow after resize — resolved 2026-09-18 (M2d Step 0, P2).** Carried from M2c R8: a shadow
  derived from the window's composited alpha might keep the collapsed shape after a resize.
  Measured: the shadow's extent follows the frame change without `invalidateShadow()`; the control
  could not manufacture staleness, so the result is unfalsified and the call is made
  unconditionally as insurance. See "M2d: resize in place — Step 0 measured, D2–D4 decided".
- **One `pnpm tauri build --debug` failure, not reproducible (2026-09-15).** On branch `m2`
  with the M2a code uncommitted, the build produced `Ondar.app` and then failed at the DMG step:
  `` failed to bundle project: error running bundle_dmg.sh: `failed to run /Users/mv/Developer/Onda/src-tauri/target/debug/bundle/dmg/bundle_dmg.sh` ``
  (verbatim; the CLI printed it twice, the second time prefixed `Error`). Then 3 of 3
  passes on identical code (`--bundles dmg -v` once, the full `pnpm tauri build --debug -v`
  twice), and the acceptance build at `791fd66` passed too. **Cause unknown.** The failing run was
  not verbose. No stale Ondar volume was mounted (`/Volumes` held only two unrelated user
  installers). The unified log could not help: `log show` returned 0 lines from that shell even
  for a 10 s window in which a build was certainly running, so it was unreadable, not empty. The
  2026-09-13 claim that the build completes unaided stands; this is recorded because an
  unexplained intermittent in the release path should not live only in a chat. Run release-path
  builds with `-v` so a recurrence leaves detail.

  **Narrowed 2026-09-16, by the leftover scratch image.** The failed run left
  `rw.17605.Ondar_0.1.0_aarch64.dmg` (80,779,776 bytes) in `bundle/macos/` — mtime 12:40 local,
  the same instant as the 11:40 UTC in that day's report; this entry is UTC, a file listing is
  local. `bundle_dmg.sh` names that scratch image at line 317, creates it at 382-386, then
  **resizes** (410-416), **attaches** (431), runs the Finder AppleScript, blesses, detaches,
  **converts** to the final image (555-559), and removes the scratch file only at line 561. Its
  survival therefore places the failure **after creation and before line 561** — creation, resize,
  attach, AppleScript, bless, detach or convert. It rules out "at the start", and no more than
  that: the failing run was not verbose, and `/Volumes` being clean two hours later is consistent
  with either "never attached" or "the script's own detach trap (line 52) ran". `DMG_DIR` is
  `bundle/macos/`, not `bundle/dmg/` — the bundler runs the script there and moves the finished
  image afterwards, which is why the leftover and the `rw.*` images from later successful runs all
  appeared there. The leftover was deleted 2026-09-16.

## API etiquette (non-negotiable)

- Send a descriptive `User-Agent` (`Ondar/<version>`) on every radio-browser request.
- Discover servers via the `_api._tcp.radio-browser.info` SRV record (`hickory-resolver`),
  with hardcoded fallbacks; do not hammer a single host. **There is one host** (measured
  2026-09-21: the SRV record has a single target, `de1`; `de2` is the same address; the older
  mirror names no longer resolve), so retries are same-host with backoff, three attempts, and
  **the cache is the resilience story, not the retry loop**.
- Call the station-click endpoint when playback actually starts, once per play.
- Cache aggressively (countries: 7 days, station lists: 24 h) and respect the cache offline.

## Milestones (sequenced; one at a time)

1. **M1 — Scaffold + audio engine.** Tauri + Vite/React/TS; Rust audio module
   (`stream-download` → rodio decoder → EQ adapter → output); play/pause/stop/volume
   commands; ICY title events; reconnect and error states; EQ biquad unit tests. Plain
   test window, no tray.
2. **M2 — Tray + NSPanel popover.** `tauri-nspanel` pinned rev, vibrancy, template tray
   icon, collapsed/expanded resize in place, positioning from tray rect. **Spiked
   2026-09-12** on `m2-spike` (not merged; its measurements were of a reverted `TaoWindow`).
   **M2a done and merged 2026-09-15** (`b553737`, tagged `m2a-done`): tray, non-activating panel,
   clamped positioning, resign-key dismissal — see "M2a: the tray path, measured". **M2b
   (coordinates: multi-monitor, mixed scale, the notch) done 2026-09-16** — see "M2b: coordinates
   are logical points". **M2c (Esc, tray menu, rounded corners, single instance, tokens, the M1
   bench retired) done 2026-09-17** on branch `m2c` — see "M2c: chrome and input, measured".
   **M2d (collapsed/expanded resize: two heights, the D1 cap, the page-commit round trip) done
   2026-09-21, merged `2a9bae9`, tagged `m2d-done`** — see "M2d: resize in place". **M2 complete.**
3. **M3 — Station API + SQLite cache + country/station UI.** SRV discovery, `User-Agent`,
   click endpoint, cache TTLs, favourites/recents. **M3a built 2026-09-22 on branch `m3a`** (the
   crate, cache, store, commands, a dev list) — see "M3a: the station directory, built"; M3b (UI,
   click, prefetch from bitrate) and M3c (HLS, ADTS only) follow.
4. **M4 — Map.** Tile slicing, Leaflet CRS, country outlines, markers, PixelRadio
   coordinate DB merge. Record measured bundle size.
5. **M5 — Spectrum + EQ UI, tray animation, polish.**
6. **M6 — Signing, notarisation, DMG.**

## Principle: verify the instrument before trusting a surprising measurement

When a measurement is surprising, **check the measuring tool before you start explaining the
result.** This project has now hit the same failure three times, in three unrelated tools, and
each time the instrument was wrong rather than the thing being measured.

| Instrument | What it reported | What was actually true |
|---|---|---|
| `cargo test` (bare, no `--workspace`) | `3 passed`, exit 0 | It ran only the shell crate and silently skipped all 48 engine tests. A plausible small number reads as success; `0 passed` would have looked obviously empty. |
| `NSWindowOcclusionState` read as a boolean | `8192`, non-zero, "so the window is visible" | `Visible` is `1 << 1`, and `8192 & 2 == 0`. The window was reporting that it does **not** reach the screen. The non-zero value was undocumented high bits. |
| `magick file.svg` | RMSE **0.19** against the PNG master — a real mismatch | ImageMagick had no `rsvg` delegate and fell back to its internal MSVG parser, which mangled the gradient. With `rsvg-convert`: RMSE **0.0085**. The images were identical. |

**The common shape: a tool that degrades quietly instead of erroring.** None of the three
failed loudly. Each returned a well-formed, plausible answer — a passing test count, a non-zero
integer, a rendered image — with no warning that it had silently narrowed its scope, changed
units, or swapped implementations. That is exactly the class of failure that survives review,
because nothing in the output looks wrong.

**The transferable part: in all three the fix was to check the measuring tool, not the thing
measured.** The occlusion case is the clearest — a day went into explaining a paradox ("AppKit
says visible but nothing is on screen") that did not exist, because the instrument was never
questioned. The `magick` case was nearly written into this document as a genuine SVG/PNG
mismatch.

In practice, before reasoning from a surprising number:

- **Decode it.** A non-zero integer is not a boolean; a bitfield needs its named constant, read
  from the source. Print the decoded field beside the raw value in any log that will be read
  later.
- **Confirm the scope.** Did the command run over what you think it did? `cargo test` vs
  `cargo test --workspace` differ by every engine test and both exit 0.
- **Confirm the implementation.** Which backend actually serviced the call? Optional delegates,
  feature flags and fallbacks change the answer without changing the command.
- **Get a second instrument.** Agreement between two independent tools is cheap; a long
  explanation of one tool's output is not.

Related findings, each an instance of this: "Bare `cargo test` skips the engine", "The M2 spike"
(occlusion), "The app icon and tray glyphs" (`rsvg`). The harness-pacing error in "The harness
paced 3.57% slow" is a fourth of the same family — the measuring harness, not the engine, was
wrong, and it invalidated a whole table.

Three more, 2026-09-15, all in M2a: `Panel::to_window()` silently changed the class of the window
the spike was measuring, with nothing in the log to show it ("The spike measured a reverted
`TaoWindow`"); a synchronous occlusion read after `show` reports "not visible" for a panel that is
on screen ~35 ms later ("M2a: the tray path, measured"); and `log show` returned 0 lines from this
shell even for a window in which a build was certainly running — the unified log was unreadable,
not empty ("Loose ends").

A fifth instrument, 2026-09-16 (M2b Step 0): **`NSScreen::mainScreen` is not the menu-bar display.**
It is the screen with the key window, and it read `Some("BenQ GW2470")` while the menu bar was
measurably on the built-in (`screens()[0]` = "Built-in Retina Display", and the status item's own
window was on the built-in). The menu-bar display is `screens()[0]`, or `CGMainDisplayID()`, which is
what `tray-icon` uses. The name is the trap: "main screen" and "main display" are different things in
AppKit, and a positioning fix built on `mainScreen` would be wrong only on multi-monitor setups where
focus is on another display — silently, and in exactly the configuration it was written for.

A sixth and a seventh, 2026-09-18 (M2d Step 0). **Sixth: a claim derived from source about what a
selector does.** tao's source shows `set_size` reaching `setContentSize:`; the plan then asserted
that the selector keeps the Cocoa bottom-left origin. Measured, a visible window keeps the
top-left. The source is authoritative about *which* call is made and silent about what AppKit does
with it — the instrument here was the reader, treating a call site as a behaviour. **Seventh: an
opaque instrument overlay masked its own control.** The probe's opaque page fill kept the window's
alpha opaque everywhere, so the blur-view control that was supposed to manufacture a stale shadow
could not change the silhouette at all — the run read as a pass because the instrument had removed
the quantity it was measuring. A direct repeat of M2c's lesson (an instrument that changes the
quantity it measures reports its own shape), caught by looking at the crops rather than the
numbers. Neither is a "verify the instrument" instance in the quiet-degradation sense of the first
five; both are the reader's step being skipped — a derivation, or an overlay, taken as the thing.

An eighth, 2026-09-18/21 (M2d `/code-review`): **a tool that ran out of budget reported it as an
HTTP 429, not as a partial result, and miscounted its own verifiers.** The review agent was cut
off by the monthly spend limit after dispatching twelve verifiers; nothing in its output said so,
so the candidate list had to be mined from its transcript, and its last message counted "seven"
returned verdicts while listing six — the two that completed after it died (C8, C9) reached this
session as task notifications, not the agent. The salvage produced a full findings file only
because the review's shape — finders, then one self-contained verifier per candidate, each
carrying its files, lines and the question to settle — left every candidate usable on its own; a
monolithic reviewer dying at the same point would have left nothing. The instrument lesson is the
same family as the first five (quiet degradation: a 429 in a subagent's log is not a result in the
review's output), with a second half: **count a tool's claims about itself from its artefacts, not
from its summary** — the provenance of every verdict was re-derived from the twelve transcripts
before the findings file called eight of nine independently verified.

A ninth to a twelfth, 2026-09-21 (M3 Step 0 census), all of the quiet-degradation kind. **Ninth:
a 200 that is not the whole answer.** `bycountrycodeexact` with no `limit` returned exactly 1000
rows for six countries whose `stationcount` was 1 456–8 190, with no header saying so; the
instrument is the response itself, and the check was the record count against a second source
(the countries list). **Tenth: a body that is not what was asked for.** A station server
(Wowza) gzip-compressed a playlist although the request sent no `Accept-Encoding`; the probe
parsed compressed bytes as playlist lines and requested a garbage segment URL. The check was
the `content-encoding` header and the magic bytes. **Eleventh — corrected at M3a: a derived cause
recorded as a measured one.** The census client sat 12 minutes with no socket open (`lsof`
against the log — that part was measured), and the report wrote the *cause* as "reqwest's
`connect_timeout` bounds the TCP connect, not the DNS resolution before it" without measuring
it. Measured at M3a with an injected stalled resolver, the production client's
`connect_timeout` bounds DNS too (`open` returned at 10.01 s). The instrument lesson is the
opposite of the one first written: the hang was real, its explanation was a guess, and a
guess tagged *derived* must be checked before it becomes a fix. The hang's cause stays
unexplained. **Twelfth: the parser of the second instrument.** The `dig`
output parser matched the script's own `##` headings that contained "SRV" and reported a
disagreement between two instruments that in fact agreed; the run stopped, correctly, and the
parser was the defect. All four are in `_handover/m3-step0-report.md`.

**Thirteenth, 2026-09-22 (M3a acceptance, item 8): a unit test on the wrong level, recorded as
the fix.** `e51f3ea` made the ICY status line classify as `Http`, pinned it with socket tests on
`stream::open`, and this document's Constraints entry then said Shoutcast v1 servers "surface as
an `Http` error since M3a" — but the engine's `retry_or_fail` never read the cause, so the
runtime still ran five attempts, 31 s and six requests before the `Http` appeared. Acceptance
measured it with the same instrument Step 0 had used (`stall_bench` against
`scripts/icy-server.py`) and got Step 0's number back. The lesson: a test that pins a function's
output proves that function; the claim was about the session, and only a test at the session's
level (`engine::session_tests`, counting requests) can carry it. Same family as eleven — a
derived claim written as a measured one — with the added step that the derivation had a green
test beside it.

## Principle candidate: mutation testing proves sensitivity only where a test can reach (2026-09-16)

**A surviving mutation says a test is missing; a mutation you never thought to write says nothing at
all. Mutation testing measures the tests you have against the code they already touch.**

The instance is `/code-review` finding 3 on M2b. The M2b fix moved every coordinate into points, and
the one quantity the whole change was about — dividing the panel window's `outer_size` by that
window's own scale — sat in `anchor()`, which needs a live `WebviewWindow` and so has no test. The
pure function next to it had twelve. The mutation pass covered the pure function, ten mutations, nine
caught, one fixed by adding a test — and reported a clean bill while the central division was
untestable and unmutated. `mixed_scale_does_not_change_the_answer` claimed to pin exactly that
property and had inputs identical to its neighbour, so it could not fail on its own.

The fix was structural, not more tests: give the pure function the physical size *and* the scale, do
the division there, and the same mutation pass then kills it (dropping the division fails 7 tests,
multiplying instead of dividing fails the same 7). Two working rules:

- **Ask what the change is about, then ask whether a test can reach it** — before trusting a
  mutation score. A quantity in glue code that only integration can exercise needs moving, not
  covering.
- **A test whose inputs match its neighbour's pins nothing extra.** If two tests differ only in
  their prose, one of them is decoration.

A third, from running the pass itself: two of the five mutations silently failed to apply, because
`rustfmt` had reflowed the line the patch matched on, and both runs reported a **passing** suite —
"verify the instrument" applied to the mutation harness. The patch now asserts its own match count.

## Principle: a measurement that contradicts a recorded justification reopens the decision (recorded 2026-09-14)

**A measurement that contradicts a recorded justification reopens the decision, not just the
comment.** Never re-word the justification to fit the number; re-derive it, and if the number
turns out to be right for a different reason, say so. A recorded reason a constant *cannot* be
measured is a valid answer; a fabricated measurement is not. Applied since M1 but, until this
entry, written down only in the Claude Project's instructions field. Two instances, both
2026-09-11: `043e289` — the floor-versus-knee analysis contradicted 32 KB's recorded
justification (freshness parity with 16 KB, margin over `fill_target`), and re-deriving it
showed 32 KB correct by coincidence, because the one-decoder-read floor and the
`RING_SECONDS × byte_rate` ceiling coincide at 128 kbit/s ("The prefetch knee"); and `4f088b3`
— pruning out of `UNDERRUN_WINDOW_TICKS` mid-wait collapsed the recorded 40-tick unstable dwell
to 10, so the constants stayed and the mechanism was fixed by latching the dwell ("The unstable
dwell was selected and then cancelled").

## Principle: an assertion must be able to fail on the quantity it pins (2026-09-14)

**State what an assertion would have to see to fail, and check that it would.** A tolerance is
a claim about sensitivity. When a test observes the quantity it cares about *through* a
transform, its real sensitivity is the transform's slope at that point, not the number written.

This is a different class from "Principle: verify the instrument before trusting a surprising
measurement" and "Principle: a measurement that contradicts a recorded justification reopens
the decision", both above. Both of those need a trigger — a
surprising result, or a contradiction. An assertion pointed at the wrong quantity produces
neither: it stays green, nothing disagrees with anything, and it is wrong the whole time.

**The case.** From `1e4d237` (2026-09-11) to `73e0803` (2026-09-14), the soft-clip bound tests
compared the post-shaper output peak against a five-decimal literal with a round `5e-4`
tolerance. The shaper is nearly flat near its ceiling, so in the quantity that matters — the
EQ's own pre-shaper peak — those windows were:

| Case | Asserted | Pre-shaper window it admitted | True pre-shaper peak |
|---|---|---|---|
| 1 | 0.99868 ± 5e-4 | 2.27–3.95 (−18 % / +42 %) | 2.78707 |
| 2 | 0.99587 ± 5e-4 | 1.44–1.59 (−4.4 % / +5.5 %) | 1.50583 |
| 3 | *hypothetical* 0.99962 ± 5e-4 — it was unasserted until `97caf3c`, which used 5e-6 | 3.74 upward (−49.8 % / unbounded) | 7.45484 |

Windows are centred on the asserted literal, not the measured value, because that is what the
shipped assertion compared against. Case 3's lower edge is 50.2 % of the true peak: a gain loss
of just under half would have passed. (Exactly half, 3.727, would have failed by 0.014 —
`97caf3c`'s own comment said "half its gain", which overstated it.) The literals also carried
rounding error the size of a useful tolerance: case 1 measures 0.998675168, on the edge of
rounding to 0.99868, so even `5e-6` would have left it 1.7e-7 from failing, with a lopsided
window.

**The fix (`73e0803`).** Stop asserting on the post-shaper literal. `implied_pre_shaper`, the
algebraic inverse of `soft_clip` in f64, turns each measured peak back into the band bank's peak,
asserted within `PRE_SHAPER_TOLERANCE` = ±0.1 % of the sweep's pre-shaper column. The inverse is
itself round-trip tested against the shipped curve (within one f32 output step scaled by
`(1+s)^2`; worst measured 0.56 of a step). Measured drift: −0.0014 % / +0.0002 % / −0.0029 %.
The conversion is **executed**, not described: a comment saying "derived from a pre-shaper
allowance" could drift from the number beside it, and the helper cannot. Same reasoning as the
test named `bare_cargo_test_runs_only_the_shell_crate_see_claude_md`.

**How it was found — the detail that matters most.** Not by a failing test; none failed. Twice
it surfaced while *writing a justification*: choosing a tolerance for case 3 and saying why
exposed that `5e-4` was weak, and declining to write a comment the code did not implement ("the
tolerance is chosen in pre-shaper terms and converted") exposed the rounding edge. So the next
one is most likely hiding wherever a tolerance, threshold or margin has no written reason — or a
reason nobody has checked against the arithmetic. Writing the sentence "this would fail if…" and
then checking it is the search method.

**Not covered by the weekly drift audit.** The audit checks whether a documented figure matches
the code. It cannot judge whether a tolerance is appropriate for what it pins: that needs the
transform's slope, the arithmetic, and the intent of the test, none of which a figure-vs-code
comparison sees. Every test above would have passed that audit throughout. This gap is recorded
rather than assumed closed.

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
