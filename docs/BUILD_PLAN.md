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
   `eq::tests` and `examples/eq_headroom_sweep.rs` in the meantime.)*
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

- [x] `stations::client` — `reqwest`, SRV discovery of `_api._tcp.radio-browser.info` with
      the measured fallbacks (`de1`, `all.api` — there is one server, Step 0 2026-09-21),
      `User-Agent: Ondar/<version>`, connect 10 s + a stall bound + a per-request total sized
      from the byte count, **3 attempts on the same host with backoff** and one SRV re-resolve
      between attempts 1 and 2; fetch the whole country with `hidebroken=true` and an
      **explicit high `limit`** (the API truncates silently at 1000 without one), then filter,
      dedupe, sort and cap **750** locally — never let the server cap before the filter
- [x] Endpoints: `/json/countries?hidebroken=true`,
      `/json/stations/bycountrycodeexact/{cc}?hidebroken=true&limit=…`,
      `/json/url/{uuid}` (click — **M3b commit 5, 2026-09-23**: once per `play`, on the
      session's first `Playing`, from Rust, never retried; a measurement run never votes),
      `/json/stations/search` (search only)
- [x] `stations::model` — `Station`, `Country`; normalise `url_resolved`, codec, bitrate;
      merge the 9 lowercase country codes into their uppercase rows, drop `XX`
- [x] Filtering: drop `lastcheckok == 0` (= `hidebroken`), drop empty `url_resolved`,
      **keep `bitrate == 0` sorted last among equal votes** (decided 2026-09-21, reverses
      "drop bitrate 0": a zero bitrate is unknown, not broken, and it is 16.8 % of stations),
      dedupe by folded name + `url_resolved`, sort by votes then clicktrend, cap 750
- [x] `stations::cache` — SQLite (`rusqlite`, bundled); countries TTL 7 d, station lists TTL
      24 h; serve stale on network failure **with no age ceiling**, reporting the age
- [x] `store.rs` — favourites and recently-played (SQLite), reachable from the collapsed view
- [ ] ~~Port `cities.js` → `resources/cities.json`; load into `geo`~~ → **not ported** (M1,
      2026-09-30): no city set in M4; kept for a later coordinate-inference milestone (was → M4,
      brief D4, 2026-09-21)
- [x] HLS (M3c): ADTS-AAC media playlists only — live refresh loop + ID3 strip; the MPEG-TS
      demux and audio-variant selection move to after M4 (decided 2026-09-21 from Step 0's
      sample: 5/10 ADTS, 5/10 TS of which 3 carry video). **Built 2026-09-24 on branch `m3c`**
      (`a3ec612` + docs): detection by content type, the playlist refetched on its final URL,
      audio-only variant (LC before HE, highest bandwidth), refresh on `MEDIA-SEQUENCE` from
      three segments behind live, ID3 skipped, `FFF9` → `FFF1`, format guard; TS / fMP4 / KEY /
      BYTERANGE / video-only / plain M3U refused terminally with a rendered message.
      **Acceptance** (`_handover/m3c-acceptance.md`): Antena 1 in-app 18 min at speed 1.0000, 0
      underruns, 264 advancing refreshes; the HE-AAC and `FFF9` stations at 1.0000 / 0.9998; TS
      and video-only refused in < 1 s with 3 / 2 requests exactly, no vote; click = play; Stop
      leaves no orphaned request for 31 s; Wi-Fi off 20 s (Martín, 2026-09-25) → `Buffering` →
      stall → `Reconnecting` → `Playing` in 20.7 s, one click. **217 + 15.** Defect B
      (an unbounded `Connecting` on `main`) opened for after the merge, before M4.
- [x] Commands: `list_countries`, `list_stations(country_code)`, `search_stations(query)` (+ favourites, recents; **M3a, 2026-09-22**)
- [x] UI: country dropdown (a native select, searchable by type-ahead — a custom list is a
      later commit if it proves poor by hand) wired to `list_countries`; station list wired to
      `list_stations`, click-to-play through the existing `play` command; ★ Favourites and
      Recents behind a toggle beside the select (acceptance finding C, `298c342` — they began
      as the select's first entries and were not found unaided); Now Playing; the transport
      row (**M3b commits 1b–4, 2026-09-23**). **The requirement carried from the M3a review (finding 7):** the
      real list applies a reply only for the source still selected — pinned by
      `StationList.test.tsx` (vitest, the first TS tests). The re-request on popover show
      (acceptance item 6) is built and tested the same way.
- [x] Tests: response parsing from recorded fixtures, filter/dedupe logic, cache TTL
- [x] **M3a acceptance, 2026-09-22** (`_handover/m3a-acceptance.md`): 10 items, 8 passed as
      built; item 8 (ICY → `Http` in one attempt) fixed by a retry policy by cause (`3ab7ec2`),
      item 6's stale `refreshing…` fixed by a `failed` refresh event (`4d83918`); the re-request
      of an expired list on popover show / reconnect is **M3b** (page behaviour). See ONDAR.md,
      "M3a: the station directory, built", the acceptance paragraph.
- [x] **M3a code review, 2026-09-22** (`_handover/m3a-review-findings.md`): ten findings fixed
      one commit each — the `landed` outcome derived from the write, a cache that will not open
      moved aside instead of stopping the launch, an empty countries answer refused, the retry
      policy narrowed to 401/403/404/410 on the first open with `Retry-After`, rule 3 floored at
      2 000, one non-HTTP wording table, `mms://` refused, the dev list's reply guard, `LIKE`
      escaping, the error body text — plus three cleanups; acceptance items 5, 6, 8 re-run
      2026-09-23 (`m3a-acc-07`/`08`). ONDAR.md, "Code review, 2026-09-22".
- [x] **M3b acceptance, 2026-09-23** (`_handover/m3b-acceptance.md`): 10 items, 7 passed as
      built; B (the country control unusable offline) fixed `f829b1e`, C (★ discoverability →
      a toggle beside the select) fixed `298c342`, items 2 and 9 re-run PASS. **A — the output
      keeps the first session's sample rate — is M1's defect, already on `main`, deferred by
      decision to its own measured work after the merge, before M4**; items 4 and 6 pass on
      clicks and prefetch and fail on A, not re-run. ONDAR.md, "M3b: the collapsed view…", the
      acceptance paragraph.
- [x] **M3b code review, 2026-09-23** (`_handover/code-review-2026-09-23.md`): eight findings
      fixed one commit each — the `started` race decided under one lock, the prefetch ceiling,
      the transport's reading of `reconnecting`, the error cleared on a source change, the
      reply serialised only under `?measure=perf`, the failed click's own error shape,
      `describeError` in the transport, and the three drifted document lines (this commit).
      ONDAR.md, "Code review, 2026-09-23".

**Exit:** country dropdown populated from Rust; selecting a country lists real stations;
airplane mode still shows the last cached lists (**measured 2026-09-22**, acceptance item 5);
favourites persist across restarts (by test at M3a — recents measured by hand; the favourite
control and its hand check are M3b's).

---

## M4 — Map

**Goal:** the expanded pane shows the selected country as a **drawn map**, framed and zoomable,
from a bundled resource. The satellite map was reversed 2026-09-24 and the reversal applied
2026-09-30 (ONDAR.md, "M4: the drawn map — the reversal"); M4 is split in three. Plan:
`_handover/m4a-plan.md`.

**4a — Geodata and the `ondar-map` crate (no UI)**

- [x] The build tool `ondar-map-build` (a workspace member): Natural Earth 10m v5.1.2 admin 0,
      map units and admin 1, pinned by SHA-256 and refused on a mismatch; never in CI
- [x] The frame rules: grouping at 300 km ground distance, the frame group (MY by override),
      the centre from the antimeridian-aware bbox (AQ pole-centred), the fit and the 1.5 km/pt
      floor, the inset table, subdivisions above 8 km/pt, the code aliases, the antimeridian seam
- [x] The ladder 1.5/3/6/12/24 km/pt, each unit once per level, simplified per ring to a
      measured ≤ 0.25 pt (subdivisions 0.5 pt), with a per-ring index
- [x] `src-tauri/resources/map/world.ondarmap` committed with its build report and golden tables
- [x] The `ondar-map` crate: the LAEA, the codec, a loader that never panics on bytes,
      `lookup`/`fit`/`clamp`/`frame`, and its tests on the shipped resource
- [x] The resource loaded at startup from `resource_dir()`, in dev and in the bundle
- [x] Record the installed bundle size in ONDAR.md (10 576 KiB, from 7 908; built 2026-10-01 on
      branch `m4a`, ONDAR.md "M4a: built and measured")

**4b — IPC and the SVG renderer, with pan and zoom** (Step 0 2026-10-02,
`_handover/m4b-step0-report.md`; brief `m4b-brief.md`; plan `m4b-plan.md`, reviewed P1–P6) —
**done, merged 2026-10-07** (`29f1582`, tagged `m4b-done`)

- [x] Transport `json` with `ts-rs` types (Step 0 rule C: RU at fit 14 ms invoke → commit at the
      real pane; `path` / `raw` save ≤ 4 ms for a hand-written codec) — built (commits 6–7); the
      bar re-derived at the acceptance review (a country change from idle to the painted frame
      ≤ 100 ms p90) and met, RU at 300 by 1 ms (ONDAR.md "M4b: built and measured"); `raw` parked
      at the top of M4c's brief
- [x] The view in Rust (`map::Session`, held by `MapSessionState`): inputs from the page (scroll, drag, `− fit +`) and from Rust
      (a native pinch) accumulated, never dropped; one `map_pull` reply per animation frame, every
      frame with a sequence number; `clamp_view` in Rust — built (commit 6; no Rust-born input,
      the pinch having failed)
- [x] Zoom: `− fit +` controls always (C1, a 74 × 24 pt row in the band's reserved bottom-right
      corner). The native pinch (Z1) was **a removable spike first**, to ship only if magnify events
      reached Rust with the panel key, `frontmost` unchanged, and Q1's click, scroll and drag
      unaffected — **FAILED 2026-10-05** (`b9531a0`, reverted `fab802a`): three instruments, zero
      magnify events, a scroll control of 511 in the same state; ONDAR.md "M4b: the pinch spike,
      measured". Zoom is the controls alone.
- [x] The band: the expanded panel's full extra height, 328 wide, a rounded platter (B1) — 328 ×
      178 on the ANMITE, 328 × 300 uncapped; the band rect a Rust constant from the layout; D1's
      floor a band of 140 pt (no band below it)
- [x] Insets at every band (I1): a per-band scale the tool computes and the resource stores; a
      minimum legible box; labels never clipped; one rebuild with coverage for every band from
      140 to 300 (`missing_blobs` = 0 at every band) — above 4.55 MB, stop
- [x] The SVG in both themes (Sand, Ink), drawing exactly the paths Rust sends; a theme change
      recolours without a frame — one flat land tone per theme with its hairline (the two-tone
      filter and a `<use>` edge group both went at the acceptance), Martín's palette at round 3
- [x] D7 kept: insets at the fit view only; `fit` is the return route
- [x] The list stays the navigation; the map is never the only route to a station

**4c — Stations** (brief `m4c-brief.md`; plan `m4c-plan.md`; Step 0 2026-10-07,
`_handover/m4c-step0-report.md`, decisions S1–S4 in ONDAR.md "M4c: Step 0 and its decisions") —
**done, merged 2026-10-12** (`d431568`, tagged `m4c-done`; ONDAR.md "M4c: built and measured")

- [x] The bar's levers first, every deciding row 60 runs: the neighbours one rung coarser (own land
      and insets capped at level 4) and RU's ring RDP retry in one rebuild; one path per layer
      dropped; `raw` only if the final row with dots reads RU at 300 above 90 ms p90 — built
      (`0e8ca3d`, RU 300 80 / 81); (e) dropped; `raw` not needed (the final row: RU 300 66 and 71 p90)
- [x] Dots gathered by 10 km ground distance, radius `min(6, 2.5 + 0.6·ln n)` pt, no numbers — built
      (`d42c700`; PT 31 / US 48 / BR 124 / RU 10 on the slices, by test)
- [x] The station work off the bar's path: `map_select` frames the land at once, the dots follow
      (`map:changed`); select → dots painted ≤ 150 ms p90 — built (`fb00ef5`, `935737c`); dots
      painted ≤ 60 ms p90 at 300 and 178
- [x] `map_hit`: a click on a dot **filters the list** to its stations, never plays; a chip clears
      the filter; **Esc clears the filter if one is set, otherwise hides the panel**; the playing
      dot marked — built (`935737c`; the halo at `r + 3`)
- [x] Hover shows the count and the region label (radio-browser's `state`, carried by `Station`,
      cache v3) — only if hover reaches the key panel (the positive control first) — S4 passed;
      built, then on the controls' plate (`f220084`, decision 7 amended)
- [x] Coordinates from radio-browser's `geo` only
- [x] **Step 0's R6, for its brief:** a dot more than 25 km outside every part of its country is
      not drawn (PT's two Brazilian stations and the Zürich one are the cases), and a country
      with 0 geo stations shows a "no coordinates" state (MT) — built (R6 per dot; MT's line)

**Exit:** PT shows the mainland with the Azores and Madeira insets; RU fits at ~28 km/pt with
its 86 subdivisions and zooms to 1.5 km/pt; the map never requests the network; the installed
size is recorded in ONDAR.md — **met**:
- PT's captures show both insets with their dots.
- RU fits at 28.011 km/pt (`frame_bench`); the zoom floor is unchanged since M4b.
- `map_select` reads the stored list only: `cached_stations_never_starts_a_fetch`, and no fetch after
  18 cached selects in the logged session.
- `Ondar.app` is 10 804 KiB.

**After M4:**

- [ ] Coordinate inference in Rust (city-level, drawn as approximate) — Amendment 1, M2;
      PixelRadio's heuristics are a reference, not an inheritance.

---

## M5 — the five phases (re-cut 2026-10-19)

**Goal:** the list finds any station and shows one row per station; the audio UI exists; the map
feels like a map at zoom; the app behaves on the system. Re-cut 2026-10-19 at the ranking session
(record `_handover/m5-ranking-2026-10-19.md`; decisions in ONDAR.md, "M5 re-cut (2026-10-19)").
No tester evidence yet, so Martín's list led: if only three fit before audio work, search, the cap,
favourites. Order: **M5a → M5b → M5d → M5c → M5e**; one phase per session. M5d before M5c because
M1's EQ exit criterion 4 has been open since September, and no tester has mentioned the map;
Martín may swap c and d, the phases are independent.

**Every phase:** Step 0 with the buy-before-build paragraph — the crates and OS facilities checked,
each with name, version, what it covers, what it misses against our use, why not (ONDAR.md, "Buy
before build"; "learning value beats speed" is invoked by name, never the default); the
measurement instrument defined before any bar; an exit; its own branch; `/code-review` before the
`--no-ff` merge; the `missing_docs` flip for each crate it touches — its public items documented
and `#![deny(missing_docs)]` at the crate root in that commit (counts on 2026-10-17: `ondar-map`
144, `ondar-audio` 109, `ondar-stations` 89, the shell 10, `ondar-map-build` 8; the workspace table
allows it until then, because CI's `-D warnings` would deny a warn — ONDAR.md, "The rules are the
compiler's"). Model Opus 5.5 unless the chat says why.

### M5a — the list: cap and search

"750 stations" against "France (3690)": any search over the cached list searches a fifth of
France. **The cap and search are one decision** (D-cap, D-search).

- **Step 0.** Where 750 comes from (client `limit`, rank truncation, or the cache). Buy before
  build (name, version, covers, misses, why not): radio-browser `/json/stations/search` parameters
  and whether it folds accents (measure "radio" vs "rádio" with `countrycode=FR`); Unicode folding
  crates (`unicode-normalization` NFD + strip marks, `deunicode`, `icu_normalizer`) against
  `filter.rs::fold`, which has no normalisation (audit by-product); list virtualisation — the plain
  list measured first, `@tanstack/react-virtual` only if the measurement says so; the gather budget
  at the largest country's full count. **Instrument:** list render time and map gather time versus
  row count on the current build, USA (the largest by radio-browser's count) and France; with the
  full list's SQLite size and fetch time. The bar is set from the measurement, then the cap goes.
- [ ] Lift the cap, after the measurement, never blind.
- [ ] Local search with folding ("rádio" = "radio"), debounced, over the selected country's full
      cached list as you type.
- [ ] "Search all countries" (a row, or Enter) via the API's search endpoint, debounced; results in
      the same list with the country flag; offline, that row is disabled with a state and local
      search still works.
- [ ] Favourites: the star to the right of the country dropdown (0.1.1 ships it as a hypothesis;
      M5a keeps or moves it on the testers' word).
- [ ] `missing_docs` flip: `ondar-stations`.

**Exit:** France selected, "rádio" and "radio" return the same stations from all 3690 within the
measured bound; the largest country renders and gathers within its bar; "BBC" with France selected
reaches BBC stations in one action; offline, the global row says so and local search still works.

### M5b — one row per station

The France list's "duplicates" are **distinct streams of one station** (France Info mp3 128k /
aac 192k; RFI Afrique 64k / RFI-Afrique 96k): a product rule — one row per station, best stream
chosen, fail-over to the next — not data cleaning. It changes the list's data model (D-streams).

- **Step 0.** **The grouping-key measurement first:** folded name alone vs folded name + homepage
  host, on five countries' full lists — how many groups, how many groups mix homepages (distinct
  stations sharing a name), max streams per group. Buy before build (name, version, covers, misses,
  why not): radio-browser fields that could carry a dedupe hint (`homepage`, `favicon`,
  `languagecodes`); favourites' storage (a stream uuid today).
- [ ] Group; best stream = highest bitrate, codec tie-break; fail-over to the next stream on
      error, on the reconnect path (a first-class feature already).
- [ ] Favourites keep storing a stream uuid, displayed by station; migration test.
- [ ] `missing_docs` flip: `ondar-stations`.

**Exit:** France Info appears once and plays aac 192k; a test double that kills that URL falls to
mp3 128k with no user action; existing favourites survive (test); no group mixes homepages in the
five-country sample, or the rule says what happens when one does.

### M5d — audio UI

The apprenticeship's showcase: a built feature nobody can use since M2c retired the bench.

- **Step 0.** **The CPU instrument defined** (the carried item) before the "single digits" bar;
  the spectrum's emit path through `Shared::emit_from` / `SessionCtx::emit` (**N1:** never
  `emit_from` — it would take the session lock from the audio path, and the audio callback never
  blocks; ONDAR.md, "Code review 2 and G1–G3"); **the 16 kHz band against the source's Nyquist
  measured** (the EQ runs at the sink's rate, so its 0.45 fs bypass no longer disables that band on
  a 22 050 station; ONDAR.md, the resampling side effects). Buy before build (name, version,
  covers, misses, why not): `rustfft` vs `realfft` vs `microfft`.
- [ ] `audio::spectrum` on the post-EQ buffer, ~30 Hz of log-binned magnitudes emitted as
      `spectrum-tick`; **throttled and dropped when the UI lags**.
- [ ] UI: vertical EQ sliders, preset picker, spectrum strip in the collapsed view (the biquad
      DSP and its unit tests exist from M1 — this is the UI + presets).
- [ ] Presets: Flat, Voice, Bass, Bright, Late Night; custom gains persisted per app, in Rust (not
      per station, unless decided otherwise).
- [x] EQ headroom fix — **done at M1** (`1e4d237`, 2026-09-11): a soft-clip stage *inside*
      `Equalizer`, the last operation on every sample. See ONDAR.md, "EQ output is bounded by a
      soft-clip stage".
- [ ] **M1 exit criterion 4** exercised by hand once the sliders exist, and recorded.
- [ ] `missing_docs` flip: `ondar-audio`.

**Exit:** moving a slider changes the sound immediately and silently; the spectrum reacts to
music; CPU within the bar set from the instrument.

### M5c — the map at zoom

The France/Ink exhibit (2026-10-19, `_handover/m6lite/zoom-france-ink-2026-10-19.png`): zoomed
inland, a flat land tone with dots and nothing else. **Empty by design** — coastlines and borders
only, and France is finer than 8 km/pt so no subdivisions. The fix is this phase, not a 0.1.1
change, and **not a shallower zoom limit** (it would be reversed later; no churn). Martín: make it
feel more like a map; rivers too (D-map).

- **Step 0.** B5's explanation (dots with no place name: where the region label comes from and why
  it is empty), if not done in 0.1.1. Whether the resource carries admin-1 for countries finer than
  8 km/pt at all; if not, a rebuild and the size growth. Buy before build (name, version, covers,
  misses, why not): Natural Earth 10m `rivers_lake_centerlines` and `lakes` (lines need a new record
  type: format v4, where the audit's "map tables partial" also lands); Natural Earth 10m
  `populated_places` for persistent place labels (public domain, independent of station records;
  the buy option for labels) — each with its size per ladder level after simplification. **The
  coordinate-inference measurement as a read-only agent task** (D-coordinates): of France's 3690,
  how many lack geo but carry a state/city field matching `cities.js`. **Instrument:** resource size
  and paint time per level (M4c's dots pass).
- [ ] Admin-1 past a zoom threshold.
- [ ] Water: rivers and lakes, in the style tokens, both themes.
- [ ] Persistent place labels.
- [ ] The B5 fix.
- [ ] Format v4, if the lines need it.
- [ ] `missing_docs` flip: `ondar-map` and `ondar-map-build`; `laea.rs:1-4` if not already done
      (re-dated in the 2026-10-19 docs commit).

**Exit:** France at 3 km/pt shows regions, the major rivers and the top-N place names; paint within
M4c's bars; the resource's growth stated.

### M5e — system integration and states

- **Step 0.** Buy before build, named with versions (covers, misses, why not): `objc2`
  `MPNowPlayingInfoCenter`; `tauri-plugin-global-shortcut`; `tauri-plugin-autostart`; sleep/wake and
  device-change notifications (IOKit / CoreAudio). **The four states (empty, loading, offline,
  error) designed before coded.** Instrument: the device-change path measured as defect A's was.
- [ ] Global keyboard shortcut to toggle the popover; `Space` play/pause when focused.
- [ ] Media keys and macOS **Now Playing** (`MPNowPlayingInfoCenter` via `objc2`).
- [ ] Launch at login (`tauri-plugin-autostart`).
- [ ] Sleep timer; pause/resume on system sleep/wake and on audio device change.
- [ ] Empty, loading, offline and error states designed, not defaulted (honest states are a
      feature — CLAUDE.md, known risk 3).
- [ ] `missing_docs` flip: the shell.

**Exit:** media keys and Now Playing work with the popover hidden; the popover toggles by shortcut;
sleep pauses and wake resumes; device change does not stall (measured, the defect-A path); each
state has a test.

### Parked (2026-10-19)

- **Tray animation** — neither animate nor redesign until a tester notices the playing state
  (D-tray; Martín: testers will not notice).
- **Onboarding** (first launch picks a country from the locale and plays a top station) — until a
  tester reports the first launch as confusing.
- **Coordinate inference** for the stations without `geo` (20.7 %, 2026-09-21; the `cities.js`
  reservation) — measured in M5c's Step 0 as a read-only agent task, decided after; no reopening
  of M2 without that number (D-coordinates).

---

## M6-lite (before M5)

**Goal:** a thin slice end to end: a signed, notarised build of `main` in 3–5 testers' hands before
M5, whose items they rank. Decided 2026-10-12 (ONDAR.md, "M6-lite before M5").

**M6-lite released 2026-10-14** (v0.1.0, Cloud release `01M4XF0DQR6APGTGZ0V2DKZEC2`); exit pending
testers. Merged to `main` 2026-10-16; the tag `m6lite-done` waits for the exit below.

- [x] Developer ID signing and **notarisation**, locally first (secrets never committed or logged)
      (local; second Mac pending) — signed 2026-10-13, notarisation accepted and the `.app` stapled
      2026-10-14; 0.1.0 released 2026-10-14 with the DMG notarised and stapled too (ONDAR.md,
      "Signing and notarisation", "Release 0.1.0")
- [ ] Bundle metadata check (`tauri.conf.json`)
- [x] A file log sink (`7cb845d`; release path observed 2026-10-13)
- [ ] `tauri-plugin-updater`, the feed on CrabNebula Cloud: check on launch, a prompt in the popover
      — code in commit 4b (2026-10-14; tauri 2.12.3 in 4a); the 0.1.0 feed is live (0.0.9 → 200
      with 0.1.0, 0.1.0 → 204); ticked when a tester sees the prompt for 0.1.1 (ONDAR.md, "Updater",
      "Release 0.1.0"). Spike 5, 2026-10-27, on Martín's own 0.1.0: the prompt appeared, then it
      installed and restarted into 0.1.1 (ONDAR.md, "Release 0.1.1"); a tester has not seen it yet
- [x] A tester note in EN / ES / PT, with the known gaps (`docs/testers/`, 2026-10-14; the download
      link filled at the 0.1.0 release)
- [x] Crash reporting and telemetry: **none** (decided 2026-10-12)

**0.1.1** (branch `v0.1.1` from `main`, started 2026-10-26; the release on a later day with Martín
at the Mac). Scope final 2026-10-26 (`_handover/v011-run2-brief-2026-10-26.md`; plan
`_handover/v011-plan.md`), superseding the 2026-10-19 list. Nothing else.

- [x] F1 reads (`_handover/v011-f1-reads-2026-10-26.md`): the engine thread wedges in rodio's
      `Player::clear()`, which waits without a timeout for an output that has stopped pulling; the
      trigger, observed in Martín's repro, is loss of the output device (Bluetooth headphones
      disconnected), not sleep alone. **The fix is 0.1.2** (follow the default output device; never
      block the engine thread on the output); 0.1.1 ships only the instruments below
- [x] The version-pin test (the three version fields agree) — review finding (`abde367`)
- [x] Hover plate: the place before the count (the dot-filter chip follows; it reads as the plate)
      (`64d3b08`)
- [x] The favourites-list star to the right of the country dropdown — layout only; **a hypothesis
      to check with testers, not a diagnosed fix** (`6c70bb3`)
- [x] `srv.rs` uses the SRV record's port (latent defect, small, testable) (`976ee02`)
- [x] B5 explained (where a dot's place comes from and why it is empty) and the 750 cap explained
      (where it is applied); read-only, into ONDAR.md; B5 fixed only if a one-liner; the cap is not
      lifted (M5a). Done 2026-10-26: ONDAR.md, "B5 explained" (M4c Step 0) and "The 750 cap
      explained" (M3a); B5 is not a one-liner (it needs a data source, M5c)
- [x] F1 instruments: **7a** receipt lines (INFO in the shell's `play` command, and at the top of
      `Engine::play` and `Engine::stop`, before `clear`); **7d** one WARN when the state is
      `Playing` and the output has pulled nothing for 5 s; **7c** release builds keep symbols
      (Step 0 measures the options against 0.1.0's 10 880 KiB). 7b (page errors forwarded) is out:
      see M6. Done 2026-10-27: 7a, 7d, plus Error states logged at WARN (9c); 7c chose the dSYM
      (ONDAR.md, "0.1.1: release symbols")
- [x] `countries: dropping code "XX"`: one INFO line per fetch instead of a WARN per row, so the
      `tauri:dev` gate's "no WARN line" reads literally (`f751d81`)
- [x] Tester defects: none as of 2026-10-26 13:28
- [x] Release as 0.1.0's steps (ONDAR.md, "Release 0.1.0"): version bump (three fields), tag, build
      with the env sourced in-command, the tar.gz-from-stapled-.app check, the DMG's own submit and
      staple, spike 2 on the DMG, `cn release draft/upload/publish`, feed check (0.1.0 → 200 with
      0.1.1, 0.1.1 → 204). **Spike 5 on Martín's own 0.1.0 first** ("Ondar 0.1.1 available ·
      Update", installs and restarts); then the testers, both asked explicitly for the log file (the
      `m6lite-done` exit needs one). **Since 7c:** copy `src-tauri/target/release/ondar.dSYM`
      with `cp -RL` to `_handover/v0.1.1/ondar-0.1.1.dSYM`, and check that `dwarfdump --uuid` matches the
      stapled `.app`'s binary; never upload it. `/code-review main..v0.1.1` before the merge.
      Done 2026-10-27: tag `v0.1.1` = `f60fb9c`, Cloud release `01M5YZFRHDQ6AR4E3S9M8XJSTN`, feed
      and download checked, spike 5 passed, merged to `main` (ONDAR.md, "Release 0.1.1"); the
      testers next

**Out, to M6:** the universal binary (aarch64 only here), DMG art, README screenshots, a CI release
workflow.

**Exit:** two testers open it without a Gatekeeper warning and play a station; one receives 0.1.1
through the prompt; a log file arrives.

---

## M6 — Signing, notarisation, DMG

**Goal:** ship it. What M6-lite leaves.

- [x] App icon set — landed early (`90236fd`, 2026-09-13), generated from
      `src-tauri/icons/ondar-icon-master.svg`; see ONDAR.md, "The app icon and tray glyphs"
- [ ] DMG background art
- [ ] Signing and notarisation in a CI release workflow (`APPLE_*` secrets)
- [ ] Universal binary (aarch64 + x86_64)
- [ ] README with screenshots, credits (Natural Earth, radio-browser.info), licence
- [ ] Page-error forwarding (0.1.1's 7b: `window.onerror` and `unhandledrejection` to one Rust
      command that logs at WARN): dropped from 0.1.1 on 2026-10-26 because the F1 reads showed the
      page alive; kept for testers in general

**Exit:** a notarised DMG that a stranger can open on a clean Mac without Gatekeeper warnings.

---

## Verification per milestone

Every milestone closes with the same ritual:

1. Every gate in CLAUDE.md, "Gates before any task is done", each judged by its exit status; that
   list is the only one, so this plan cannot drift from it. Bare `cargo test` runs the root package
   only and skips every crate (CLAUDE.md, "Traps").
2. A manual smoke pass against that milestone's exit criteria, with the app actually running
3. Update `ONDAR.md` / `docs/` with anything learned that contradicts this plan
4. Commit, and tag `m2-done`, `m3-done`, … following `m1-done`'s pattern

---

## Open questions

1. ~~**Name.**~~ **Settled 2026-09-13: Ondar.** "Onda" collided with Onda Cero, a national
   Spanish radio network; "Ondar" is Basque for sand, and no radio app or station was found
   under it. See ONDAR.md, "Renamed from Onda to Ondar".
2. ~~**Bundle identifier.**~~ **Settled 2026-09-21: `eu.ondar.radio`** (domain `ondar.eu`
   bought; dev overlay `eu.ondar.radio.dev`). The M3 soft deadline was met: the change landed
   before any path under the app data directory exists, so no cache is orphaned. See ONDAR.md,
   "Bundle identifier: `eu.ondar.radio`". Was: `dev.crabnebula.ondar` was a placeholder with two
   deadlines — **M3, soft** (Tauri derives the app data directory from the identifier, so a
   change after the SQLite cache lands orphans that cache) and **M6, hard** (once a signed
   artifact exists, macOS keys preferences, app support and keychain items to the identifier, so
   a change loses user settings silently on upgrade).
3. ~~**Blue Marble month**~~ — **moot (2026-09-24, drawn map).**
4. ~~**Night-lights dark mode**~~ — **moot (2026-09-24, drawn map):** the night theme is Ink.
5. ~~**Deepest zoom level**~~ — **moot (2026-09-24, drawn map):** the finest level is 1.5 km/pt,
   Step 0's ladder.
6. ~~**HLS streams**~~ **Settled 2026-09-21: supported, in Rust, split.** Measured at M3 Step 0:
   3.8 % of an eight-country sample (9.7 % in PT); of ten sampled, five ADTS-AAC media
   playlists and five MPEG-TS (three with video). ADTS ships in M3 as M3c; the TS demux moves
   to after M4. See ONDAR.md, "M3 Step 0: the live data, measured".

## Risk notes

Per-milestone risks are called out inline above. For risks that aren't tied to one milestone
(sparse station coordinates, general stream reliability, macOS-only build host), see ONDAR.md's
"Known risks" section — not restated here to avoid the two documents drifting apart.
