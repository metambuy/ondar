# Ondar — Build Plan

Sequence and milestone numbering follow **ONDAR.md** (the tags follow it too — `m1-done` is
M1, not M0). This file exists for exit criteria, the verification ritual, and open questions;
decisions, versions, and findings live in ONDAR.md — see that file first if the two disagree.

**Ordering logic:** shell + audio → tray/popover → station data → map → spectrum/EQ UI/polish
→ signing. Audio comes before the popover chrome deliberately: it is the hardest, most
Rust-heavy part and the thing that makes the app a radio rather than a picture of one.

One milestone per session. Do not start the next milestone's work before the previous one's
exit criteria pass.

---

## M1 — Scaffold + audio engine — **done**, tagged `m1-done`

Tauri + Vite/React/TS scaffold; Rust audio module (`stream-download` → rodio/Symphonia decode
→ EQ adapter → output); play/pause/stop/volume/EQ commands; ICY title events; reconnect and
error states; EQ biquad unit tests. Plain test window (`src/App.tsx`, retired into the popover
at M2c), no tray.

**Exit criteria (met):**

1. Play an MP3 and an AAC stream from the bench; hear audio; `playback:stream_info` arrives.
2. ICY titles update on stations that send them.
3. Pull the network: state goes `buffering` → `reconnecting (n)` → `playing` on recovery, or
   `error [network]` after 5 attempts (1/2/4/8/16 s).
4. EQ sliders audibly change the sound; `cargo test -p ondar-audio` passes. *(Since M2c the
   bench is gone and the popover's dev transport carries no EQ sliders, so this criterion is
   **unexercisable by hand until M5** brings the EQ UI. The engine-side claim is held by
   `eq::tests` (17) and `examples/eq_headroom_sweep.rs` in the meantime.)*
5. Switching stations silences the old station immediately.

CI (fmt/clippy/test/typecheck in GitHub Actions) was not part of this milestone — a separate,
later piece of work, not a gap in M1 itself. It was set up on 2026-09-11, after M1 closed; this
paragraph said "is not yet set up" until 2026-09-13. It gates every *push*, verifying that
push's head commit, not every commit — see ONDAR.md, "CI verifies the head of each push, not
every commit".

---

## M2 — Tray + NSPanel popover

**Goal:** it behaves like a real macOS menu bar app before it does anything useful.

- [x] `ActivationPolicy::Accessory` + `LSUIElement` — no Dock icon (M2a; `lsappinfo` →
      `UIElement` on the bundled build)
- [x] Tray icon as a **template image**, light/dark correct — template image set (M2a);
      light/dark appearance checked by eye 2026-09-16
- [x] `tauri-nspanel`: a non-activating `NSPanel` — a separate `panel` window, not the main
      window converted (M2a; the M1 bench window retired at M2c, into the popover)
- [x] Position from Tauri's own `TrayIconEvent::Click { rect }` (`tauri-plugin-positioner` not
      needed — ONDAR.md, 2026-09-12) — popover anchors under the tray item, correct on
      multi-monitor and with the notch — anchoring and edge clamping (M2a); multi-monitor,
      mixed scale and the notch in logical points (M2b, measured on three displays)
- [x] Hide on resign-key / `Esc`; toggle on tray click; right-click tray → About + Quit (no
      Preferences — no preferences window exists in any milestone) — resign-key and tray toggle
      (M2a); `Esc` and the menu (M2c), every close through one logged hide path
- [x] Vibrancy background (`macos-private-api`, Tauri's own `set_effects` — `window-vibrancy`
      is not a direct dependency), rounded corners, no window chrome — vibrancy and no chrome
      (M2a; confirmed by eye 2026-09-16); rounded corners at the measured 8 pt, shadow following
      (M2c)
- [x] Collapsed/expanded height states, resized **and** repositioned in place in one frame change
      (empty panes for now) — **not animated** (M2d decision D2, measured: an animated resize is
      14 frames of empty material with the page frozen, and no duration rescues it); the expanded
      height is capped to the display (D1, 598 pt measured on the ANMITE); the page's commit is
      awaited before the frame changes or the popover is shown (D3), which also fixed M2c's
      one-frame pane flash (M2d)
- [x] `tauri-plugin-single-instance` — 2.4.4, plus `RunEvent::Reopen` for the launches that
      start no process; both show the popover (M2c)
- [x] Design tokens (`styles/tokens.css`) for both appearances — values measured on this Mac,
      literal check in `pnpm lint` (M2c)

**Exit:** popover opens and closes like Bartender/Fantastical; expand/collapse resizes in place
in one frame change with no jump — **not animated** (D2; the earlier "animates" here predated the
measurement, ONDAR.md "M2d: resize in place"); nothing flickers on a second monitor. **Met
2026-09-21:** M2d merged `2a9bae9`, tagged `m2d-done`; M2 complete. (The one M2d measurement still
open — whether the round trip removes the resize's unpainted band, acceptance item 6 — is
recorded in ONDAR.md as unmeasured, not as a failed exit.)

**Risk:** `tauri-nspanel` API drift — a git-only community crate that moves faster than any
doc here. Verify against the pinned rev before writing code and record what you find in
ONDAR.md's "Verified versions". The M2 spike (2026-09-12) passed the dependency half of its
go/no-go against Tauri 2.11.5, so the borderless always-on-top fallback is not taken; its vibrancy
measurement was of a reverted `TaoWindow`, re-measured on the real panel at M2a by view tree only. The spike's `hides_on_deactivate`
defect was resolved at M2a by dropping it and dismissing on resign-key — see ONDAR.md, "M2a: the
tray path, measured".

---

## M3 — Station API + SQLite cache + country/station UI

**Goal:** the app knows about every country and its stations, offline-tolerant, and you can
reach them from the popover.

- [ ] `stations::client` — `reqwest`, SRV discovery of `_api._tcp.radio-browser.info` with
      hardcoded fallback hosts, `User-Agent: Ondar/<version>`, timeouts, 3 retries with backoff
      across *different* hosts
- [ ] Endpoints: `/json/countries`, `/json/stations/bycountrycodeexact/{cc}`,
      `/json/url/{uuid}` (click), `/json/stations/search`
- [ ] `stations::model` — `Station`, `Country`; normalise `url_resolved`, codec, bitrate
- [ ] Filtering: drop `lastcheckok == 0`, drop bitrate 0, dedupe by name+url, sort by votes
      then clicktrend, cap per country
- [ ] `stations::cache` — SQLite (`rusqlite`, bundled); countries TTL 7 d, station lists TTL
      24 h; serve stale on network failure
- [ ] `store.rs` — favourites and recently-played (SQLite), reachable from the collapsed view
- [ ] Port `cities.js` → `resources/cities.json`; load into `geo`
- [ ] Commands: `list_countries`, `list_stations(country_code)`, `search_stations(query)`
- [ ] UI: searchable country dropdown wired to `list_countries`; station list wired to
      `list_stations`, click-to-play through the existing `play` command
- [ ] Tests: response parsing from recorded fixtures, filter/dedupe logic, cache TTL

**Exit:** country dropdown populated from Rust; selecting a country lists real stations;
airplane mode still shows the last cached lists; favourites persist across restarts.

---

## M4 — Map

**Goal:** the expanded pane shows the selected country on real satellite imagery.

**4a — Asset pipeline (build time, not runtime)**

- [ ] Download NASA Blue Marble NG: the 21600×10800 (2 km/px) monthly composite; pick one
      month — plus optionally Black Marble night lights for dark mode
- [ ] `tools/tiles/build.sh` using **libvips**:
      `vips dzsave world.tif tiles --layout google --suffix .webp[Q=80] --tile-size 256`
- [ ] Keep levels that fit the size budget; record total MB in `docs/map-pipeline.md`
- [ ] Ship as Tauri `resources`; serve through the asset protocol (CSP allowlisted)
- [ ] Simplified `countries.geojson` (Natural Earth 50 m, `mapshaper -simplify 8%`) with
      per-country bounding boxes precomputed into a JSON side-file

**4b — Renderer**

- [ ] Leaflet with `L.CRS.EPSG4326` (note: zoom-0 grid is **2×1**, not the usual 1×1), local
      `L.tileLayer` over the bundled pyramid
- [ ] `map.fitBounds(countryBbox, {padding})` on country selection
- [ ] `maxBounds` = country bbox + margin; `minZoom`/`maxZoom` clamped to available levels
- [ ] Country outline overlay: all countries faint, selected country bright + inner glow
- [ ] Station markers (from Rust, capped ~200/country), hover tooltip, click → play
- [ ] Playing station's marker pulses
- [ ] Attribution line: "Imagery: NASA Earth Observatory" in the about panel
- [ ] Dark mode: swap tile layer (night lights) or apply a tuned filter

**Exit:** pick Portugal → the country fills the frame at good resolution; pick Russia → you
can pan across it at a readable zoom without losing the frame; the map never requests the
network. Record the measured installed bundle size in ONDAR.md.

---

## M5 — Spectrum + EQ UI, tray animation, polish

**Goal:** the EQ and spectrum are visible and usable, the tray feels alive, and the rough
edges from earlier milestones get finished.

- [ ] `audio::spectrum` — `rustfft` on the post-EQ buffer, ~30 Hz of log-binned magnitudes
      emitted as `spectrum-tick`; **throttled and dropped if the UI lags**
- [ ] UI: vertical EQ sliders, preset picker, spectrum strip in the collapsed view (the
      biquad DSP and its unit tests already exist from M1 — this is the UI + presets)
- [ ] Presets: Flat, Voice, Bass, Bright, Late Night; custom gains persisted per app (not per
      station, unless decided otherwise)
- [x] EQ headroom fix — **done at M1** (`1e4d237`, 2026-09-11): a soft-clip stage *inside*
      `Equalizer`, the last operation on every sample, so the bound cannot be bypassed by
      assembling the graph differently. Not a separate adapter after it, and no makeup
      attenuation. See ONDAR.md, "EQ output is bounded by a soft-clip stage"
- [ ] Tray animation: a small frame sequence swapped on a timer via `TrayIcon::set_icon`
      (there is no animated-template-image API)
- [ ] Global keyboard shortcut to toggle the popover; `Space` play/pause when focused
- [ ] Media key support and macOS **Now Playing** integration (`MPNowPlayingInfoCenter` via
      `objc2`)
- [ ] Launch at login (`tauri-plugin-autostart`)
- [ ] Sleep timer, and pause/resume on system sleep/wake and on audio device change
- [ ] Empty, loading, offline and error states designed, not defaulted
- [ ] Onboarding: first launch picks a country from the system locale and plays a top station

**Exit:** moving a slider changes the sound immediately and silently; the spectrum reacts to
music; CPU stays low (single digits) while playing; the tray icon animates while playing.

---

## M6 — Signing, notarisation, DMG

**Goal:** ship it.

- [x] App icon set — landed early (`90236fd`, 2026-09-13), generated from
      `src-tauri/icons/ondar-icon-master.svg`; see ONDAR.md, "The app icon and tray glyphs"
- [ ] DMG background, `tauri.conf.json` bundle metadata (replace the placeholder bundle
      identifier — see open questions)
- [ ] Apple Developer ID signing + **notarisation** in CI (`APPLE_*` secrets)
- [ ] Universal binary (aarch64 + x86_64)
- [ ] `tauri-plugin-updater` with a signed update feed
- [ ] Crash/error reporting decision (opt-in or none — no silent telemetry)
- [ ] README with screenshots, credits (NASA, radio-browser.info), licence

**Exit:** a notarised DMG that a stranger can open on a clean Mac without Gatekeeper warnings.

---

## Verification per milestone

Every milestone closes with the same ritual:

1. From `src-tauri`: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings &&
   cargo test --workspace`. Plain `cargo test` (no `-p`/`--workspace`) only runs the root
   `ondar` package's tests — `ondar-audio`'s tests (the ones that matter) need `--workspace` or
   `-p ondar-audio`; see CLAUDE.md.
2. From the repo root: `pnpm typecheck && pnpm lint`
3. A manual smoke pass against that milestone's exit criteria, with the app actually running
4. Update `ONDAR.md` / `docs/` with anything learned that contradicts this plan
5. Commit, and tag `m2-done`, `m3-done`, … following `m1-done`'s pattern

---

## Open questions

1. ~~**Name.**~~ **Settled 2026-09-13: Ondar.** "Onda" collided with Onda Cero, a national
   Spanish radio network; "Ondar" is Basque for sand, and no radio app or station was found
   under it. See ONDAR.md, "Renamed from Onda to Ondar".
2. **Bundle identifier.** `dev.crabnebula.ondar` (in `src-tauri/tauri.conf.json`) is a
   placeholder — decide before the first signed artifact. Two deadlines, the second the one
   that actually bites:
   - **M3, soft.** Tauri derives the app data directory from the bundle identifier, so
     changing it after the SQLite cache lands at M3 orphans that cache. Recoverable — it just
     rebuilds. *(Reasoning recorded 2026-09-13, not a measurement: this has not been tested
     against a real Tauri build.)*
   - **M6, hard.** Once a signed artifact exists, macOS keys preferences, app support and
     keychain items to the identifier, so a change loses user settings silently on upgrade.
3. **Blue Marble month** — one fixed month, or all twelve switching with the calendar
   (twelve months multiplies the bundle; almost certainly one).
4. **Night-lights dark mode** — worth the extra tile set, or a filter on the day imagery?
5. **Deepest zoom level** — how much bundle size are you willing to spend? The single biggest
   lever on download size.
6. **HLS streams** — support them (adds `hls` handling in Rust) or exclude them from results?
   See ONDAR.md's known risks for the current state (not yet measured how many stations this
   affects).

## Risk notes

Per-milestone risks are called out inline above. For risks that aren't tied to one milestone
(sparse station coordinates, general stream reliability, macOS-only build host), see ONDAR.md's
"Known risks" section — not restated here to avoid the two documents drifting apart.
