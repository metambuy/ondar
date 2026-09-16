# Ondar — project document

*Last updated: 2026-09-11 (Phase 1 block 2 — harness pacing fix, dwell latch, engine
watchdog, prefetch knee, and a re-measured latency table).*

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
- `hickory-resolver` 0.26.2
- `tauri-specta` 2.0.0-rc.25 (not adopted)

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

**MSRV correction (found and closed 2026-09-10, `7bbe332`):** the workspace `Cargo.toml` used
to declare `rust-version = "1.85"`, but `stream-download` 0.24.4's own manifest declares
`rust-version = "1.91.0"`, so 1.85 had never been buildable since `stream-download` was added —
it went unnoticed because the toolchain here is 1.98. `7bbe332` bumped the workspace
`rust-version` to 1.91 and dropped `README.md`'s caveat that `Cargo.toml` still said 1.85, so
its "Rust ≥ 1.91" prerequisite now stands alone; `clippy.toml`'s `msrv` matches. Nothing
outstanding.

Re-verify at the start of each milestone that touches these; update this list.

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
- **~30% of radio-browser stations have coordinates.** Map markers are therefore sparse;
  the country dropdown, not the map, is the primary navigation. The map is context and
  delight. The PixelRadio supplementary coordinate DB will raise coverage (M4).
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
  as an `Http` error. Rare via radio-browser's `url_resolved`; measure at M3 before deciding
  whether a raw-socket fallback is worth it.
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

M2a's `panel shown` log line prints `class=`, so a revert cannot go unnoticed again.

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

**M3 refinement:** radio-browser's station record carries `bitrate`, making
`prefetch_bytes = max(one_decoder_read, RING_SECONDS × bitrate / 8)` computable before `open`.
The `max` is load-bearing — the knee alone starves the decoder at 64 kbit/s.

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
  with hardcoded fallbacks; do not hammer a single host.
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
   are logical points". M2c (Esc, tray menu, rounded corners, single-instance, tokens, retiring the
   M1 bench window) and M2d (collapsed/expanded resize) remain.
3. **M3 — Station API + SQLite cache + country/station UI.** SRV discovery, `User-Agent`,
   click endpoint, cache TTLs, favourites/recents.
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
