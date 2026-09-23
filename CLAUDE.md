# CLAUDE.md — Ondar

> Repository root. Claude Code reads this at session start.
> This file describes **the repo as it is**. Decisions, versions and milestone numbering live
> in `ONDAR.md`; per-milestone exit criteria and open questions live in `docs/BUILD_PLAN.md`.
> If this file and the code disagree, the code is right — fix this file in the same commit.
> **`_handover/OPEN.md` is the pending ledger** — what is outstanding, who owns it, what state
> it is in. Update it when an item's state changes. (`_handover/` is gitignored.)

## What this repo is

**Ondar** — a macOS menu bar internet radio player. Tauri v2 shell, Rust core, thin
React/TypeScript view layer. Rust audio pipeline with a real 10-band equalizer, station data
from radio-browser.info, a bundled NASA Blue Marble satellite map.

Target platform is **macOS only** (Apple Silicon first, universal binary at release). Do not
add Windows/Linux code paths.

## The one rule

**The webview is a renderer, not an application.**

TypeScript may: render, animate, handle input, hold ephemeral view state.
TypeScript may **not**: make network requests, decode or play audio, persist data, hold domain
state that outlives a render.

Every action → `invoke()` a Tauri command. Every state change → a Tauri event from Rust.
If a feature seems to need `fetch`, `<audio>`, `localStorage`, or a `setInterval` polling loop
in TS, the design is wrong; move it to Rust and emit an event.

`src/api.ts` is the **only** file that imports from `@tauri-apps/api`. Keep it that way.

## Milestones (ONDAR.md numbering — the tags follow this, not any other list)

| | | |
|---|---|---|
| M1 | Scaffold + audio engine | **done**, tagged `m1-done` |
| M2 | Tray + NSPanel popover | **done** — M2a merged 2026-09-15 (`b553737`, tagged `m2a-done`); M2b (coordinates: multi-monitor, mixed scale, notch) merged 2026-09-16 (`3b4614c`, tagged `m2b-done`); M2c (Esc, tray menu, rounded corners, single-instance, tokens, retire the M1 bench window) merged 2026-09-18 (`032fc8a`, tagged `m2c-done`); M2d (collapsed/expanded resize, D1–D4 in ONDAR.md) merged 2026-09-21 (`2a9bae9`, tagged `m2d-done`) |
| M3 | Station API + SQLite cache + country/station UI | **in progress** — **M3b built** on branch `m3b` from 2026-09-23 (plan `_handover/m3b-plan.md`; Step 0 folded into the commits: 1a `fbb0a79` harness · 1b `de87005` country control + station list + the TS runner · 1c `b4bbfbc` Now Playing · 2 `d1129a3` the list measured at 50/327/750, no virtualisation · 4 `bb2452d` favourites/recents, presets retired · 5 `33400f5` the click endpoint · 6 `155d14d` prefetch from bitrate · 7 docs; **acceptance pending** — ONDAR.md, "M3b: the collapsed view…"); Step 0 live-data census done 2026-09-21 (`_handover/m3-step0-report.md`; one API server, silent 1000-row default, 20.7 % geo, HLS 3.8 %, Shoutcast v1 0/148); M3a (crate, cache, commands) on branch `m3a`, **acceptance run 2026-09-22** (10 items; two fixes `3ab7ec2` retry policy by cause, `4d83918` failed-refresh event; the re-request on show/reconnect carried to M3b); **`/code-review` 2026-09-22**: ten findings fixed in ten commits `f3de220`…`73020d3` plus three cleanups, acceptance 5/6/8 re-run 2026-09-23 (ONDAR.md, "Code review, 2026-09-22"); awaiting merge |
| M4 | Map (tile pyramid, Leaflet, markers) | |
| M5 | Spectrum + EQ UI, tray animation, polish | |
| M6 | Signing, notarisation, DMG | |

One milestone per session. Do not start the next milestone's work early. Do not leave a
milestone with failing checks.

## Layout (actual)

```
onda/
├── CLAUDE.md                     this file
├── ONDAR.md                      project document — decisions, verified versions, findings
├── README.md                     prerequisites, first run, M1 exit criteria, stall testing
├── docs/
│   ├── BUILD_PLAN.md             exit criteria + open questions per milestone
│   └── PROJECT_INSTRUCTIONS.md   stub; the real text now lives in the Claude Project's
│                                 instructions field, not this repo
├── scripts/stall-server.py       local Icecast-alike for stall/reconnect testing
├── scripts/check-tokens.sh       fails `pnpm lint` on a style literal outside tokens.css
├── vite.config.ts, tsconfig.json vite builds one entry, panel.html — the explicit input map is
│                                 what makes the bundle ship it
├── panel.html                    the popover page: transparent root (load-bearing) + the entry
├── package.json                  pnpm; pnpm-workspace.yaml carries `allowBuilds: esbuild`
├── src/                          React renderer for the popover (renderer only)
│   ├── panel.tsx, vite-env.d.ts  entry (mounts panel/Panel.tsx); Vite's client types for CSS modules
│   ├── panel/                    Panel.tsx (root: mirrors the layout from Rust — pane, height state,
│   │                             height in points, expandable — sets the root height from it, owns the
│   │                             selected country and the show counter the lists re-request on, hosts
│   │                             the expand control (disabled when refused, D4) and the placeholder for
│   │                             the expanded pane, reports Esc),
│   │                             NowPlaying.tsx (name, the reserved ICY title line, `flag · codec ·
│   │                             bitrate` + the state as text; mirrors `playback:*`; M3b 1c),
│   │                             CountryControl.tsx (the ★ toggle for favourites and recents, then the
│   │                             native country select; its provenance line, an error on a line of its
│   │                             own; never disabled; M3b 1b, fixes B/C), StationList.tsx (the rows
│   │                             of the selected source — a country's ranked list, or with ★ on the
│   │                             favourites then the recents not among them (source.ts) — one line
│   │                             each, scrolling in the collapsed pane; click → play; the wrong-source guard and the re-request rules,
│   │                             pinned by StationList.test.tsx — vitest, jsdom), source.ts (the
│   │                             ListSource type and its key), provenance.ts (the
│   │                             `cached N h ago · refreshing…` text), Transport.tsx (play/pause,
│   │                             stop, the ★ favourite toggle, volume — no EQ; the presets retired at
│   │                             M3b commit 4; `reconnecting` offers no Play, as the row reads it —
│   │                             pinned by Transport.test.tsx), About.tsx (name, version, credits),
│   │                             panel.module.css
│   ├── styles/tokens.css         THE only file with colour/size literals, light + dark together
│   ├── measure.ts                the page half of the dev-only measurement harness (M3b 1a): inert
│   │                             unless the page was loaded as `panel.html?measure=…`
│   ├── api.ts                    THE Rust boundary: invoke wrappers + event listeners
│   └── bindings/                 GENERATED by ts-rs — do not edit by hand
└── src-tauri/
    ├── Cargo.toml                workspace: ".", "crates/ondar-audio", "crates/ondar-stations"
    ├── tauri.conf.json
    ├── tauri.dev.conf.json       dev-only overlay: identifier `<id>.dev`; a shell test pins the
    │                             derivation, so renaming the real id without it fails the build
    ├── Info.plist                merged at `tauri build`: LSUIElement (dev cannot test it)
    ├── capabilities/default.json scoped to `panel`, the only window; `core:default` only
    ├── icons/                    ondar-icon-master.svg is the source; the PNGs/icns derive
    │                             from it. tray/ holds the 4 template glyphs (22/44 ×
    │                             idle/playing) — pure black on alpha, icon_as_template(true).
    │                             Only the 44 px pair renders; see ONDAR.md, tray glyphs
    ├── src/                      Tauri shell only. No domain logic.
    │   ├── main.rs               calls ondar_lib::run()
    │   ├── lib.rs                AppState, `events` module, tracing init, event forwarder
    │   │                         (also drives the tray's idle/playing glyph); single-instance
    │   │                         callback and `RunEvent::Reopen` → the panel's show path
    │   ├── panel.rs              the NSPanel popover: build, toggle, resign-key dismissal; the pure
    │   │                         geometry (display resolution, anchor + clamp, the D1 cap and floor,
    │   │                         the Cocoa frame conversion) and the D3 round trip's pure bookkeeping
    │   │                         (`RoundTrip`), both unit-tested; route S `apply_frame`; show, resize,
    │   │                         commit and fallback paths; the tray-screen placement log
    │   ├── tray.rs               template tray icon, click logging, idle/playing swap
    │   ├── error.rs              OndarError → `{ code, message }`
    │   ├── log_rate_limit.rs     tracing filter bounding the `stream_download::source` ERROR
    │   │                         flood; holds 3 of the shell's 38 tests, including the
    │   │                         bare-`cargo test` tripwire (see Commands)
    │   ├── measure.rs            dev-only measurement harness (M3b 1a), `#[cfg(debug_assertions)]`
    │   │                         whole: `ONDAR_MEASURE` → `panel.html?measure=…`, `_KEEP_OPEN`,
    │   │                         `_SEQ=show|shows:<n>` through the production show/hide paths, and
    │   │                         the `measure_report` command → `measure[<mode>] …` log lines.
    │   │                         `strings` on a release binary finds no `measure[`
    │   ├── commands/audio.rs     8 thin commands; validate args, send, return
    │   ├── commands/stations.rs  7 thin async commands (list_countries, list_stations, search_stations,
    │   │                         favourites, recents): forward to the stations service's handle and
    │   │                         map the error; none blocks main (M3a; `record_played` left with M3b
    │   │                         commit 5 — Rust records a play itself)
    │   └── commands/panel.rs     panel_escape (the page reports Esc, Rust hides, reason=esc),
    │                             panel_set_expanded (the page reports a click on the expand control;
    │                             Rust lays out, applies or refuses), panel_layout_committed (the page
    │                             reports its DOM commit for a layout generation; Rust completes the
    │                             show or resize then — D3), panel_view_back (the page's Back left
    │                             About; recorded so later layouts carry the pane on screen) and
    │                             get_panel_layout (the layout last emitted, for the page to mirror
    │                             on mount)
    └── crates/ondar-audio/       the engine. No Tauri dependency — unit-testable standalone.
        ├── engine.rs             engine thread, session lifecycle, `decide_tick` state logic
        ├── stream.rs             stream-download open, ICY headers, timeout invariant
        ├── icy.rs                in-band ICY title stripping
        ├── ring.rs               rtrb ring → rodio Source (never blocks the audio callback)
        ├── eq.rs                 10-band biquad peaking EQ + soft-clip, as a rodio Source adapter
        ├── reconnect.rs          Backoff: 1/2/4/8/16 s, 5 attempts, reset after 30 s stable
        ├── types.rs              IPC types (ts-rs `#[ts(export)]`)
        └── examples/
            ├── stall_bench.rs
            └── eq_headroom_sweep.rs  cross-checks the shipped soft-clip against the swept curve
    └── crates/ondar-stations/    the station directory (M3a). No Tauri dependency.
        ├── model.rs              boundary types: Country, Station, Codec, CacheSource, ListedCountries,
        │                         ListedStations (ts-rs; i64/u64 fields exported as `number`)
        ├── normalise.rs          radio-browser JSON → the types, with the census's rules and their counts
        ├── filter.rs             rank: drop broken / empty-url, dedupe folded name+url, sort votes then
        │                         known-bitrate-first then clicktrend, cap 750
        ├── srv.rs                SRV lookup (hickory-resolver), measured fallbacks de1 + all.api
        ├── client.rs             Transport/HostSource/Timing traits (fakes in tests); same-host retries
        │                         under a 200 s budget; stall + per-request totals; explicit limit and
        │                         the three-rule truncation guard (rule 3 from 2 000 stations up); an
        │                         empty countries answer refused
        ├── cache.rs              rusqlite (bundled), user_version migrations, TTL 24 h / 7 d, expired
        │                         lists kept with their age, local search (ASCII case folding,
        │                         LIKE metacharacters escaped)
        ├── store.rs              favourites; recents (20, replay moves to the top)
        ├── service.rs            the DB thread + a 2-worker fetch runtime: never awaits the network on
        │                         the DB thread; coalesces fetches per country; stale-while-revalidate;
        │                         emits StationsUpdated/CountriesUpdated (with a RefreshOutcome:
        │                         landed | failed — every fetch ends with one event, and `landed`
        │                         means the write succeeded, not just the fetch) through a sink
        ├── fixtures/             census slices + PROVENANCE.md (the data's stated freedoms)
        └── (scripts/fixture-slice.py regenerates the PT slice)
```

That root `onda/` is **not** a missed rename. The project is Ondar, but the working directory
on disk is still `~/Developer/Onda` — renaming it would break the working directory and the
folder grant Martín's Claude session uses, for tidiness alone. It is the one place the old name
survives on purpose. See ONDAR.md, "Renamed from Onda to Ondar".

`src/bindings/*.ts` is generated by **ts-rs**, not tauri-specta. `.cargo/config.toml` sets
`TS_RS_EXPORT_DIR = src/bindings` (relative to the repo root), so **`cargo test` in the
`ondar-audio` package is what regenerates the bindings** — see the note on workspace test
scoping below. Commit them.

That regeneration *is* a test run: `#[ts(export)]` expands to a `#[test] fn
export_bindings_<type>` that writes the `.ts` file. So the 180 tests `cargo test --workspace`
reports break down as **161 hand-written + 19 ts-rs-generated** (audio 76, shell 40, stations 64):

| | |
|---|---|
| `engine::tick_tests` | 20 |
| `engine::started_tests` | 7 — the click rule's pure part on `Shared::write_state` (M3b 5): once per session whatever the route back to `Playing` (fails on a per-`Playing` or previous-state rule); a second `play` for the same station starts again; a reconnect before ever playing starts on its first `Playing`; paused while buffering starts on resume, `Started` after `State(Playing)`; a repeated `Playing` is a no-op; **a stale session's `Playing` cannot take the new session's `Started`** (`/code-review` finding 1, 2026-09-23 — the write and its liveness are decided under one lock; fails on a flag read before the lock, which was the code: mutation-checked, the gate disabled sends `Started { u2 }` for a station that has not opened); a cancelled session's late write is dropped before any successor (fails if only `begin_session` moves the generation) |
| `engine::session_tests` | 6 — a WAV played twice across a reconnect is one session: one `Started`, with the id (fails if per-`Playing`; found the harness needed a mixer drain thread, since `Player::clear()` waits for a queued source); the retry policy at the level `stream::open`'s tests could not reach: `run_session` against counting servers on 127.0.0.1, a device-less `rodio::mixer` under the `Player`. `ICY 200 OK` and 404 → `Error { Http }` with **one** request and no `Reconnecting`; 503 → a second request through the backoff. Mutation-checked 2026-09-22: with the terminal branch disabled the first two fail at `Reconnecting { attempt: 2 }`. Review finding 4: a 429 with `Retry-After: 3` → `Reconnecting { 1 }` and no second request inside 2 s (fails if every 4xx is terminal, or if the header is ignored); a 404 on the reconnect after a 1.5 s WAV stream ended → `Reconnecting { 2 }`, no `Error` (fails if a reconnect's 4xx is terminal) |
| `stream::tests` | 13 — the prefetch is the larger of the floor and the knee (M3b 6; fails if the `max` is dropped — 64 kbit/s would get 16 000 — or the knee's arithmetic is off), **capped at half the buffer** (`/code-review` finding 2, 2026-09-23: 10 000 kbit/s and FLAC's 1411 give the ceiling, 524/525 kbit/s straddle it; fails if the upper bound is dropped, which was the code), the env override replaces the whole value; a 404's body text reaches the message, bounded to 200 chars (finding 10); `parse_url` refuses a non-http scheme as `invalid_url` before any request (finding 5); real sockets on 127.0.0.1, asserting `(code, terminal)`, plus one on the shared `NON_HTTP_WORDING` table (a 503's "status" wording is not terminal, hyper's version wording is — finding 8): an `ICY 200 OK` answer is `Http` and terminal (mutation-checked against the old rule), a 500 is `Http` and retriable, a 404 and a 403 are `Http` and terminal, a 429 is `Http`, retriable and carries its `Retry-After` (finding 4), a refused connect is `Network`, and DNS resolution is inside `connect_timeout` (a stalled resolver, 200 ms bound, 5 s guard — M3a G4a/G4b) |
| `eq::tests` | 17 |
| `icy::tests` | 3 |
| `ring::tests` | 3 |
| `reconnect::tests` | 1 |
| `types::export_bindings_*` | 6 — generated, one per `#[ts(export)]` type |
| `normalise::tests` | 6 — **stations** crate, from here to `service`: the countries fixture parses 250 → 240 with DE's merged count; the PT-60 slice's edge rows pinned by an independent Python pass; codec mapping; the geo rule |
| `filter::tests` | 5 — bitrate 0 sorts last among equal votes (fails on `Option`'s natural order); dedupe keeps the higher votes; broken/empty-url dropped; the cap cuts after sorting; the PT-60 slice ranks to 44 |
| `srv::tests` | 2 — priority/weight order; no records → the measured fallbacks only |
| `client::tests` | 15 — a click is one `transport.get` on `/json/url/<uuid>` with `TOTAL_CLICK`, never retried (fails if `fetch_with_retries` is reused); `limit=` always sent; an empty countries answer refused (`EmptyCountries`); the three truncation rules with the F6 boundary pair (1171/1172) and rule 3's floor (expected 3, rows 1 accepted; the 1999/2000 pair — finding 6); three same-host attempts with one re-resolve; the wall-clock budget stops a slow sequence at two attempts; 404 not retried, 503/429 retried; list vs small totals; the guard through the client; the countries fixture; `Rádio &` encoded and ranked |
| `cache::tests` | 8 — schema v2's `stations_uuid` index: a v1 database migrates to 2, a second `migrate` is a no-op, `station_by_uuid` finds a row under any country (M3b 5, F3); `LIKE` metacharacters in a search query match literally (`Radio_1`, `%`; finding 9); migrations versioned and idempotent; fresh at TTL−1 s, expired at TTL and TTL+1 s; a nine-day-old list kept with its age; atomic replace; countries round trip; local search |
| `store::tests` | 4 — replay to top without duplicate; the recents cap; a favourite survives its list's replacement; idempotent add |
| `service::tests` | 17 — `started(uuid)` records the recent from the cached snapshot, emits `RecentsUpdated` once and clicks once (M3b 5); a failed click is one log line (recent kept, no retry, nothing on the sink); `"manual"` neither records nor clicks and an uncached uuid clicks without a recent; a held click does not delay `list_favourites`; a measurement run (`clicks_suppressed`) records and does not vote (F1); an empty `200 []` countries answer is an error for every waiter and a `failed` refresh, never `Closed` (finding 3); a corrupt database is moved aside and the service starts on a fresh one, an unopenable path degrades the handle instead of aborting (finding 2); a failed refresh ends with exactly one `Failed` event (fails if the failure arm emits nothing); a refused cache write (`PRAGMA query_only`) ends as `Failed`, not `Landed`, and a waiter gets the cache error (fails if the outcome is assumed from the fetch — `/code-review` finding 1); a held fetch does not delay `list_favourites`; three callers one fetch; an expired list served before the refresh completes; `stations:updated` fires once; a missing list errors after three attempts and an expired one is kept; offline search fallback; countries |
| `model::export_bindings_*` | 7 — generated, stations crate (`RefreshOutcome` since 2026-09-22) |
| `log_rate_limit::tests` | 3 — in the **shell** crate, not `ondar-audio` |
| `panel::tests` | 30 — in the **shell** crate; three pin the About decision (About shows collapsed, the choice survives it, a resize from About is refused); one reads `tokens.css` and pins the radius; two pin the top-left → Cocoa frame conversion against measured frames; five pin the round trip's bookkeeping (stale commit, supersede, hide cancels, fallback once, show-pending window); five pin D1's cap (598 measured on the ANMITE, idle where 720 fits, clamp idle under the cap) and its floor (refusing and expanding sides, synthetic display). (16 until M2d retired the mixed-scale test whose quantity no longer exists — see the 1x test's comment) |
| `panel::export_bindings_*` | 4 — generated, in the **shell** crate: `panelview`, `panelheight`, `paneltransition`, `panellayout` |
| `tests::dev_identifier_is_the_real_identifier_plus_dev` | 1 — shell crate, `lib.rs`; pins `tauri.dev.conf.json` |
| `export_bindings_{stationsupdated,countriesupdated}` | 2 — generated, shell crate: the `stations:updated` and `countries:updated` payloads |

Counting `#[test]` attributes in source gives 161 and will not reconcile with the runner's 180
until those 19 are accounted for. `cargo test --workspace -- --list | grep -c ': test$'` is the
authority — the expression is part of the number, since `--list` also prints a summary line.

**The TypeScript tests are a second count, kept apart** (M3b 1b, decided 2026-09-23): `pnpm test`
(vitest, jsdom) runs `src/**/*.test.tsx` — **12** today: 10 in `StationList.test.tsx` (the
wrong-source guard, `landed` re-requests, `failed` clears `refreshing` without a request, a show
re-requests, ★ on lists favourites then recents with the country reply left behind dropped, a ★
reply landing after ★ off dropped, `recents:updated` and a favourite toggle re-request only the ★
list, a click on the playing row does nothing and on the paused row resumes — M3b 5, F2; and
does nothing while `reconnecting`, the row's reading pinned beside the transport's —
`/code-review` finding 3, 2026-09-23), 1 in `Transport.test.tsx` (`reconnecting` offers no
Play — Pause disabled, Stop enabled, as `connecting` does; a Play there would be a new session,
a reset backoff and a second vote — finding 3; fails on the code before it) and 1
in `Panel.test.tsx` (offline with no countries list and a favourite stored, the select and the ★
toggle are enabled and ★ lists the favourite — acceptance findings B and C).
Every "tests" figure in this project is written as the two numbers, `180 + 12`, never their sum:
the two runners count different things and neither can see the other's.

## Commands

```bash
pnpm install                 # frontend deps (pnpm only — do not use npm)
pnpm tauri:dev               # the dev loop: `tauri dev` with src-tauri/tauri.dev.conf.json merged,
                              # which gives the dev instance the identifier `<id>.dev` — its own
                              # single-instance socket (and, from M3, its own data dir), so it runs
                              # beside a bundled build. Bare `pnpm tauri dev` still works but shares
                              # the real identifier and hands off to a running bundle (M2c, case f).
                              # `pnpm tauri build` never merges the overlay: bundles keep the real id.
pnpm tauri build             # release bundle (macOS host only)
pnpm typecheck               # tsc --noEmit
pnpm test                    # vitest under jsdom, `src/**/*.test.tsx` (M3b 1b): the renderer's own
                              # tests, 12 today (StationList + Transport + Panel). Its count is reported BESIDE
                              # the Rust count — "180 + 12", never "192" — and CI runs it as its own step
pnpm lint                    # eslint, then scripts/check-tokens.sh (no style literal outside tokens.css)
pnpm gen:bindings            # alias for `cargo test --workspace` (ts-rs writes src/bindings/ from
                              # all three crates: the engine's IPC types, the shell's panel types
                              # and StationsUpdated, the stations crate's model)

cd src-tauri
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --workspace       # 180 tests: 76 in the ondar_audio binary, 64 in ondar_stations and
                              # 40 in the shell's ondar_lib; the remaining targets have 0. Plain
                              # `cargo test` with no `-p`/`--workspace` only runs the root
                              # `ondar` package (40 tests) and silently skips both crates; this
                              # workspace has a real [package] at the root, so cargo doesn't
                              # default to "all members" the way a virtual workspace would.
                              # Use `--workspace` or `-p ondar-audio` explicitly. A bare run
                              # prints only the shell's forty test names, and one of them —
                              # bare_cargo_test_runs_only_the_shell_crate_see_claude_md — says
                              # so. That name is the signal; it is a real test, and renaming it
                              # makes the trap silent again.
cargo run -p ondar-audio --example stall_bench    # against scripts/stall-server.py
```

Before declaring any task done: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --workspace`, `pnpm typecheck`, `pnpm test`, `pnpm lint`, and `pnpm tauri:dev` (not
the bare form, which hands off to a running bundle and exits — a vacuous pass) launching without
a console error.

## The IPC contract

Commands (`src-tauri/src/commands/audio.rs`, wrapped in `src/api.ts`):
`play(url, stationId, bitrateKbps)`, `pause()`, `resume()`, `stop()`, `set_volume(volume)`,
`set_eq_gain(band, gainDb)`, `get_eq()`, `get_playback_state()`. Plus two **panel** commands
(`commands/panel.rs`): `panel_escape()` (`panel.escape()`) — the page reports an Escape `keydown`
and Rust hides the popover through `panel::hide` with `reason=esc`; and `panel_set_expanded(expanded)`
(`panel.setExpanded()`) — the page reports a click on the expand control and Rust lays the panel
out for the new height against a fresh tray rect, applies it, or refuses it (D1's floor, or
`reason=view` on the About pane, which has no control and always shows at the collapsed height;
the user's choice survives it and Back restores it — decided 2026-09-21), logging which. A third, `panel_layout_committed(generation)` (`panel.layoutCommitted()`), is the
**round trip** (D3): every `panel:layout` carries a generation; the page reports it from an
effect after the render that used it, and Rust completes the visible change then — orders a
pending show in, or changes the visible panel's frame — if that generation is still pending. A
stale, superseded or cancelled generation is a logged no-op; a hide cancels; and a fallback
timer (`LAYOUT_FALLBACK`, 250 ms, kept at n = 74 on 2026-09-23 — provenance in its doc comment) completes without
the report so a dead page cannot wedge the popover — `trigger=fallback` on a healthy page is a
defect. A fourth, `panel_view_back()` (`panel.viewBack()`): the page's Back button left the About
pane — the one page-local transition — and reports it, so the pane a later layout event carries
is the one on screen (`/code-review` C1). And one panel getter, `get_panel_layout()`
(`panel.getLayout()`), the counterpart of the `panel:layout` event as `get_playback_state` is of
`playback:state`. All five are outside the three groups below — they never touch the engine.

**Stations** commands (`commands/stations.rs`, wrapped in `src/api.ts`'s `stations` object, M3a):
`list_countries()`, `list_stations(countryCode)`, `search_stations(query)`, `list_favourites()`,
`add_favourite(station)`, `remove_favourite(uuid)`, `list_recents()`. (`record_played` is gone
since M3b commit 5: a play is recorded by Rust, on the session's first `Playing`, with the click.)
All `async`: each sends a message to the `ondar-stations` service's DB thread and awaits a
`oneshot` reply — a fetch in flight never delays a cache read or a store call. A list comes back
as `ListedCountries` / `ListedStations` with its provenance: `source` (`fresh` | `cached`),
`fetched_at`, `age_secs`, `refreshing`. An **expired** list is served at
once as `cached` with `refreshing: true` while Rust refreshes it in the background
(stale-while-revalidate); only a **missing** list makes the caller wait, and that wait is bounded
by the client's 200 s retry budget. Errors: `{ code: "stations", message }` (network exhausted
with nothing cached, a truncated list, a cache failure, or the directory **unavailable** because
its database could not be opened even after being moved aside as `ondar.sqlite.corrupt-<ts>` and
recreated — the app launches regardless, review finding 2), or `invalid_argument` for a bad country
code. The page never fetches, filters or ranks.

Events (names defined once, in `src-tauri/src/lib.rs::events`):
`playback:state`, `playback:stream_info`, `playback:metadata`, `playback:reconnect`,
`stations:updated` (a `StationsUpdated { country_code, outcome }`: a background refresh of that
country's list ended — `outcome` `"landed"`: re-request it; `"failed"`: the expired list stays,
clear `refreshing` and do **not** re-request, since a re-request starts another refresh),
`countries:updated` (a `CountriesUpdated { outcome }`, same rule for the countries list),
`recents:updated` (no payload: a play was recorded, so a page showing the recents re-requests
`list_recents`; M3b commit 4), and
`panel:layout` (a `PanelLayout`: `transition` `"show"` | `"resize"` — on a show the hidden frame is
already at the size, on a resize it changes after the page's commit — `generation`, `view`
`"about"` | `"transport"`, `state` `"collapsed"` | `"expanded"`, `width`/`height` in points,
`expandable`; emitted on every effective show — the view from the show reason — and on every
resize. The page mirrors it and decides none of it: it is told its **target** height and never
computes that; its root is the larger of the target and the window's own height only while a
resize is in flight, so nothing is unpainted inside a still-tall window. Superseded M2c's
`panel:view`).

"Every command is a message to the engine" is **not** true here. The eight audio commands fall into
three groups, and which group a command is in determines what its return value means:

| Group | Commands | Mechanism |
|---|---|---|
| Channel message, returns `Result` | `play`, `set_volume` | Validate args, send an `AudioCommand`, return. The `Result` reports **argument validation only** — never a playback outcome, which arrives later as an event. |
| Channel message, returns `()` | `pause`, `resume`, `stop` | Nothing to validate, so no `Result` at all. |
| Direct engine access, never touches the channel | `set_eq_gain`, `get_eq`, `get_playback_state` | Reach into `AudioEngine` through a shared handle. `set_eq_gain` validates and returns `Result`; the two getters return data synchronously with no `Result`. |

The third group is the one that surprises. `set_eq_gain` *looks* like a setter that should be
sequenced with playback, but it calls `state.engine.eq().set(..)` — a `Relaxed` atomic store
into `EqGains`, picked up by the EQ adapter on the audio thread at its next frame-boundary
check (every 64 frames). It never reaches the engine thread or the command channel. `get_eq`
and `get_playback_state` likewise read `AudioEngine::eq()` / `AudioEngine::state()` directly
(an atomic-array snapshot and a `Mutex` lock respectively).

Practical consequence: EQ changes are **not** ordered against `play`/`stop`. A `set_eq_gain`
issued just before a `play` applies to the new session immediately, because gains live on the
engine handle and outlive any one session — they are not part of the command stream.

Types crossing the boundary derive `Serialize, Deserialize, TS` with `#[ts(export)]`: the
engine's in `crates/ondar-audio/src/types.rs`, the shell's beside the module that owns them
(`panel.rs`'s `PanelView`, `PanelHeight` and `PanelLayout`). Adding one means adding it there, running
`cargo test --workspace` (plain `cargo test` skips the engine — see above; `pnpm gen:bindings`
is the alias), and committing the generated `.ts`. No boundary type is typed by hand on the TS
side.

## Rust conventions

- **No `unwrap()` / `expect()` / `panic!` on *fallible runtime operations* in code reachable
  from a command or the audio thread.** Two exemptions, each with the reason it cannot fire:
  `Mutex::lock().unwrap()` (poison propagation only — a poisoned mutex means another thread
  already panicked, and the audio callback takes no locks), and `expect()` on thread spawn and
  tokio runtime construction, where failure means the OS refused a thread and the app cannot
  run at all. Spawns are not all at startup: the decode thread is spawned per session, from
  `play`. The per-sample DSP path (`ring.rs`, `eq.rs`) has none of any kind outside
  `#[cfg(test)]` and must stay that way. Nothing enforces this — `clippy.toml` sets only
  `msrv`.

  **Three sites sit outside both exemptions, by decision (2026-09-14; the third added at M3a
  and counted by the 2026-09-22 review):**
  - `stream.rs::build_client`'s `.build().expect(..)`, called once from `Engine::new`. It
    fails only if the native-tls connector (Security.framework) cannot initialise or the
    user-agent is not a valid header value.
  - `NonZeroUsize::new(BUFFER_BYTES).expect(..)` in `stream.rs`, reached from `play` on every
    open. `BUFFER_BYTES` is a non-zero `const`, so it cannot fire.
  - `client.rs::ReqwestTransport::new`'s `.build().expect(..)` in `ondar-stations`, called once
    from `StationsService::start` at setup — the same reqwest builder with the same two ways
    to fail as the first site.

  The documented resolution is to *describe* them here rather than change them. Converting
  them to real error handling is an open option nobody has taken.
- One shell error type, `OndarError` (`thiserror`), serialised as `{ code, message }` with a
  stable `code` discriminant so the UI branches on it without parsing strings. Engine-side
  failure reasons are `types::ErrorCode` (`network`, `http`, `unsupported_format`, `decode`,
  `device`, `invalid_url` — also a non-http scheme such as `mms://`, refused before any request)
  carried inside `PlaybackState::Error`.
- Logging: `log::` inside `ondar-audio`; the shell installs `tracing_subscriber::fmt` (its
  `tracing-log` feature bridges `log` call sites), so one `RUST_LOG` drives both — including
  `stream-download`'s internal `tracing` output. Never `println!`. Audio-thread logging is
  rate-limited.
- The engine runs on its **own thread**, driven by a `std::sync::mpsc` command channel and a
  100 ms tick (`TICK_INTERVAL`). Tokio exists only for `stream-download`'s HTTP. Decode runs
  on a further per-session thread. Commands never block on audio.
- No allocation, locking, or logging inside the per-sample DSP path.
- Buffering supervision lives on the **engine thread**, not the decode loop — a stalled read
  blocks `decoder.next()` indefinitely, so a decode-cadence supervisor cannot see a stall.
- State-machine changes go through `decide_tick`, the pure function at the bottom of
  `engine.rs`, so they stay unit-testable without an audio device. It has 20 tests. Add to
  them; do not route new transitions around it.
- Commands are thin: validate → send → map the error. Domain logic lives in `ondar-audio`.
- Never add a dependency without saying what it does and why std or an existing crate is not
  enough.

## Testing conventions

- **State what an assertion would have to see to fail, and check that it would.** A tolerance
  is a claim about sensitivity, not a round number. When a test measures the thing it cares
  about *through* a transform, its real sensitivity is the transform's slope at that point, not
  the tolerance written. Assert on the quantity of interest, inverting the transform if
  necessary. (`eq.rs`: `implied_pre_shaper` + `PRE_SHAPER_TOLERANCE`.)
- **Prefer a justification the code executes to one written beside it.** A comment saying a
  tolerance was derived from a pre-shaper allowance can drift out of agreement with the number;
  a helper that performs the conversion cannot. Same reasoning as the test named
  `bare_cargo_test_runs_only_the_shell_crate_see_claude_md` — an explanation that is
  load-bearing cannot rot silently.

See ONDAR.md, "Principle: an assertion must be able to fail on the quantity it pins". The
weekly drift audit does not cover this class.

## TypeScript conventions

- Strict mode. No `any`. Import IPC types from `src/bindings/`.
- Function components + hooks. Local state by default; a single small store only for state
  genuinely shared across panes (popover expansion, selected country) — introduce it at M2,
  not before.
- Rust is the source of truth for player state; the UI mirrors events and never maintains an
  optimistic parallel model.
- CSS modules, no framework. Colours and spacing from CSS custom properties; both light and
  dark values defined together.
- Every interactive element keyboard reachable and labelled.

## Audio pipeline invariants

```
HTTP (stream-download, bounded) → IcyReader → rodio::Decoder (Symphonia)   [decode thread]
  → rtrb ring (RING_SECONDS = 2) → Equalizer (10 × biquad peaking) → Player → MixerDeviceSink
```

1. One `AudioEngine`, owned by Tauri state, created once at startup.
2. Switching stations tears down the session but keeps the device and `Player` alive.
3. **Ondar owns all reconnects.** `stream-download`'s internal reconnect fires only on a hang
   and, on a live Icecast mount, splices a plain GET's byte 0 onto the writer's position — an
   audible jump with no state change. Real recovery is our own `Backoff` + a fresh
   `stream::open()`. See ONDAR.md, "Reconnect ownership and stream timeouts".
4. **`read_timeout` must stay strictly greater than `retry_timeout`** (20 s / 5 s). Inverted,
   the download loop spins forever. `stream.rs` clamps and warns. Both are env-overridable
   (`ONDAR_READ_TIMEOUT_SECS`, `ONDAR_RETRY_TIMEOUT_SECS`, `ONDAR_PREFETCH_BYTES`).
5. Backoff is 1/2/4/8/16 s, 5 attempts, counter reset after 30 s of stable playback; then
   `PlaybackState::Error` with the last attempt's code. **The policy is by cause** (2026-09-22,
   M3a acceptance item 8, narrowed the same day by review finding 4): a **terminal** open error
   — a non-HTTP answer such as `ICY 200 OK`, or a 401/403/404/410 — fails the session on the
   first attempt with `code: http` and no `Reconnecting`, **but only while the session has never
   opened**; on a reconnect every answer keeps the backoff (a mount that was playing can be 404
   while its source restarts). Network errors, 5xx, 408/429, a decoder failure and a stream that
   ended keep the backoff, each delay stretched to the server's `Retry-After` (delta-seconds,
   capped at 30 s). `StreamError::terminal` and `retry_after` carry the decision from the
   classifier to `retry_or_fail`; `run_session`'s `opened_once` confines it to the first open.
6. ICY metadata absence is normal, not an error.
7. EQ: 10 ISO-266 octave bands, Q = 1.414, ±12 dB. Gains are atomics read at frame boundaries
   every 64 frames; changed bands get new coefficients while **filter state is preserved** —
   that is what avoids the click. There is no makeup gain, and no gain ramp or interpolation
   — but the adapter bounds its own output with a soft-clip stage, identity bit-for-bit below
   `SOFT_CLIP_THRESHOLD` = 0.95 and asymptotic to `SOFT_CLIP_CEILING` = 1.0, applied inside
   `Equalizer` as the last operation on every sample so it cannot be bypassed. See ONDAR.md,
   "EQ output is bounded by a soft-clip stage".
9. **Prefetch from bitrate** (M3b commit 6): `play` carries the station record's
   `bitrate_kbps` (or none) and the engine sizes the stream's prefetch as the knee,
   `RING_SECONDS × bitrate / 8`, bounded below by `PREFETCH_FLOOR_BYTES` (one decoder read) and
   above by `PREFETCH_CEILING_BYTES` (half of `BUFFER_BYTES`, 131 072 — a prefetch at or over
   the buffer is met only when the buffer is full, and the record's `bitrate` is user-entered:
   1411, 1536 and a `128000` typo exist; `/code-review` finding 2, 2026-09-23) —
   `stream::prefetch_for`, pure and tested (the floor wins up to 131 kbit/s; 320 kbit/s is
   80 000 B; the ceiling from 525 kbit/s; no bitrate is the floor). `ONDAR_PREFETCH_BYTES`
   still overrides the whole value.
   Logged per play: `play station_id=… bitrate_kbps=… prefetch_bytes=…`.
8. Call the radio-browser click endpoint exactly once, when playback actually starts — built at
   M3b commit 5: `Shared::write_state` in the engine sends `EngineEvent::Started { station_id }`
   on the **first `Playing` of the session a `play` began** (`begin_session` resets the flag;
   an underrun's refill, a resume and a reconnect of a session that already played find it
   set; a session that reconnected before ever playing, or was paused while buffering, fires
   on its first `Playing`). **The write, its liveness and the flag are decided under one lock**
   (`/code-review` finding 1, 2026-09-23): every `begin_session` and every session end move a
   generation, a `SessionCtx` carries the one it was born with, and a write from a stale
   decode thread is dropped there — a flag checked before the lock left a window for the engine
   thread's `cancel` + `begin_session`, in which the stale `Playing` took the new session's
   `Started` (a vote and a recent for a station that had not opened, and nothing on its real
   first `Playing`). The shell's forwarder hands the id to the stations service, whose DB
   thread records the recent from the cached snapshot (`station_by_uuid`, schema v2's index)
   and spawns **one** `GET /json/url/{uuid}` (`Client::click`, `TOTAL_CLICK` 10 s, never
   retried — a retry could be a second vote) whose outcome is one log line and nothing else.
   The page's row does nothing on the station already playing (a paused one resumes), so a
   double click is not two votes; a replay is stop, then the row. With the measurement harness
   active the click is suppressed (`suppressed=measurement`) and the recent still recorded.

## macOS specifics (as built through M2b)

- Activation policy `Accessory` (set in `panel::setup`, **before** `PanelBuilder::build()`) +
  `LSUIElement` in `src-tauri/Info.plist` — no Dock icon, no menu bar menus. Only a bundled
  build can show it: `lsappinfo info -only ApplicationType` → `"UIElement"`.
- The popover is an `OndarPanel` (`tauri_panel!`) built with `PanelBuilder`, `tauri-nspanel`
  pinned by `rev` (c9ec213) in `Cargo.toml`, recorded in ONDAR.md. The plugin must be registered
  (`tauri_nspanel::init()`).
- **Never call `Panel::to_window()`** — it converts the panel back to a `TaoWindow` and empties the
  plugin store (the cause of the spike's dead tray click). Reach the Tauri window with
  `get_webview_window(label)`.
- Non-activating is the style mask: `NonactivatingPanel` ORed onto tao's mask. `no_activate(true)`
  only keeps window *creation* from activating the app.
- Showing is: lay out, `apply_frame` (one synchronous `setFrame:display:` with origin **and**
  size, while still hidden — M2d route S, D2), then emit `panel:layout` with `transition=show`
  (the order is load-bearing for that field), **wait for the page's commit**
  (or the 250 ms fallback), then `show()`, then `make_key_window()`; `show()` alone never makes
  the panel key. Measured on the dev loop 2026-09-18: the hidden-page commit arrives 2–8 ms
  after the request. A resize is the same round trip with `apply_frame` at the end instead of
  `show()`. `invalidateShadow()` follows every frame change as insurance (P2,
  unfalsified). After every show and resize the log carries `panel placed
  inside_tray_screen_visible=… gap_below_icon=…` — the **tray-screen** form by rule (R3): the
  own-screen form passed both of Step 0's forced failures. Gap 6 = clamp idle, 0 = clamp fired.
- Dismissal: hide on `WindowEvent::Focused(false)` (tao's `windowDidResignKey:`). **Not**
  `Panel::set_event_handler`, which replaces tao's delegate and silences its window events.
  `hides_on_deactivate` is not set — it keeps the panel off screen.
- Position from Tauri's own `TrayIconEvent::Click { rect }` (physical at the *status item
  display's* scale), converted to points, centred under the icon and clamped into that display's
  work area (`panel.rs`, `anchor_points` / `centred_below` / `clamp_into`, unit-tested against
  measured fixtures). `tauri-plugin-positioner` is **not needed**.
- Vibrancy is Tauri's own `set_effects` (`Effect::Popover`, `EffectState::Active`) plus
  `PanelBuilder::transparent(true)` *and* `with_window(|w| w.transparent(true))`, with
  `macos-private-api` enabled (`Cargo.toml` feature + `"macOSPrivateApi": true`).
  `window-vibrancy` is **not** a direct dependency. Measured by view tree at M2a and **checked by
  eye 2026-09-16** over a bright, busy backdrop (ONDAR.md, "The spike measured a reverted
  `TaoWindow`", for why the view tree alone was not enough).
- Tray icon: template image, 44 px glyphs via `include_image!`. `tray-icon`'s `set_icon` resets
  template mode, so the swap uses `set_icon_with_as_template` (one main-thread task), and only
  on an idle/playing flip.
- Tray menu (M2c): About + Quit. **`show_menu_on_left_click(false)` is required** — with
  `tray-icon`'s default the left click opens the menu, whose tracking loop swallows `mouseUp:`,
  and the toggle (keyed off `Up`) is dead: measured 7 clicks → 7 `Down`, 0 `Up`, 0 toggles. The
  same loop swallows the right `mouseUp:`, so **right-click logic keys off `Down`**; on
  `Click{Right, Down}` the popover hides first (`reason=menu`), measured to land before the menu.
  About is a pane inside the popover (the standard About panel opens behind the frontmost app in
  an `Accessory` app); Quit is `PredefinedMenuItem::quit` = `terminate:`, which ends the process
  without shutting the engine down — measured clean with audio playing.
- Single instance (M2c): `tauri-plugin-single-instance` 2.4.4, registered **first**. Its macOS
  mechanism is a Unix socket, `/tmp/<identifier with `.` and `-` → `_`>_si.sock`: a second
  process connects, writes cwd + argv and exits during plugin setup; the first gets the callback
  on a tokio worker and hops to main. It covers `open -n`, the inner binary and **a copy of the
  bundle at another path** (LaunchServices does not dedupe by identifier across paths —
  measured). It cannot see `open Ondar.app` or a Finder double-click against the running app:
  those start no process and arrive as `RunEvent::Reopen`, which `lib.rs` handles by running
  `.build()` then `.run(|handle, event| …)`. Both feed `panel::show_at` (`reason=second_instance`
  / `reopen`); a popover that is already up stays up (logged no-op).
- One hide path, one show path (`panel::hide` / `panel::show_at`, M2c): every caller logs a
  `reason=` and an `effective=`, so the measured double-hide on every close (toggle, then
  resign-key 2–4 ms later) reads as one effective hide and one no-op. Both hop to the main
  thread themselves — `PanelHandle` is `Send` but its methods are bare `msg_send!`.
- Occlusion: decode `NSWindowOcclusionState::Visible`, never read the raw number, and never read it
  synchronously after show — it lagged up to 35 ms when measured. The log reads it 100 ms after.
- The setup-time self-resign seen at M2a (the popover resigned key by itself while the M1 bench
  window was created visible) is **gone with `main`**: M2c Step 0, P7, 3 launches × 5 samples,
  15/15 key. The mechanism was never identified — the condition was removed, not explained. If a
  second window ever returns, re-check.
- **Coordinates are global logical points, top-left origin** (`panel.rs`, M2b): there is no common
  physical space on a mixed-scale layout, because Tauri gives each monitor's values in that
  monitor's own scale. Convert at the boundary, never divide by the panel window's scale — that is
  the scale of whatever display the panel is sitting on. `TRAY_GAP`/`EDGE_MARGIN` are points, since
  a visual spacing has to be. See ONDAR.md, "M2b: coordinates are logical points".
- M2d: expanding resizes **and** repositions against the tray anchor in the same frame — no jump.

## Map invariants (M4 — none of this exists yet)

- Tiles are generated at build time and shipped as app resources. They are **never** fetched
  from the network at runtime.
- Blue Marble is equirectangular, so lat/lng → pixel is linear. Use Leaflet's `EPSG4326` CRS;
  do not reimplement the projection. Note its zoom-0 grid is **2×1**.
- The map is framed by the selected country's bbox with padding; panning is clamped to that
  bbox plus a margin.
- Markers come from Rust, deduplicated and capped (~200 per country).
- Installed size above 100 MB is accepted (ONDAR.md, 2026-09-07). If it must be cut, drop the
  deepest zoom level before dropping quality.

## Known risks — check these before trusting this file

1. **`tauri-nspanel` API drift.** It is a git dependency with no releases. Verify against the
   pinned rev before writing code, and record what you find in ONDAR.md's "Verified versions".
   Its method names are not a guide to what they do: `to_window()` is destructive, `no_activate`
   does not make a panel non-activating, and `show()` does not make it key.
2. **HLS and redirect chains.** `stream-download` handles plain HTTP/Icecast, not `.m3u8`.
   Detect and surface `unsupported_format` rather than hanging. Also: Shoutcast v1 servers
   (`ICY 200 OK` status line) are rejected by hyper and surface as `http`.
3. **Sparse station coordinates.** 20.7 % of radio-browser stations have lat/lng (measured
   2026-09-21 over 25 236 stations in eight countries, 7–38 % by country; the inherited "~30 %"
   is retired). The country dropdown is the primary navigation; the map must never be the only
   route to a station.
4. **Stream reliability.** Dead and mislabelled streams are common. Honest error states and
   reconnect behaviour are a feature, not polish — do not paper over them with spinners.
5. **Build host.** Building, signing and notarising all require macOS.

## Working style

- **Plan first.** Anything beyond a one-file fix: propose the plan and wait.
- **Small commits**, conventional style (`feat(audio):`, `fix(map):`, `docs:`), each building
  and passing checks on its own. Docs commits stay separate from code commits — **except where a
  document line describes the behaviour the commit changes**: that hunk ships with the code and
  the commit message says so, because separating them guarantees one pushed state in which the
  document and the code disagree (2026-09-21, from M2d `7e19a1a`: the plan review asked for
  ONDAR.md's D1 formula line in the commit that changed the formula, and `/code-review` V3 then
  flagged the same commit for breaking this rule as it was written). CLAUDE.md's own same-commit
  rule above is the special case of this one.
- **CI gates every *push*, verifying that push's head commit — not every commit.** The rule
  above is yours to keep, not something CI enforces: a multi-commit push leaves every commit
  but the last unverified. **So a commit that has to stand on its own has to be pushed on its
  own.** See ONDAR.md, "CI verifies the head of each push, not every commit".
- When a decision is made or reversed, it goes into **ONDAR.md**, not just the chat.
- If a documented approach turns out to be wrong, stop and say so before improvising.
- **The author of a block is frequently wrong about the code — verify before applying, and say
  so rather than improvising.** On 2026-09-14 a dictated replacement for this file's unwrap
  rule was wrong in four places: the decode thread spawns on every `play` rather than at
  startup, `.lock().unwrap()` carries no message, `NonZeroUsize::new(CONST)` is outside the
  rule rather than an exemption to it, and the non-test site count was 20, not 19. All four
  were caught by checking the source before applying. A later item was stopped outright: it
  rested on measured values being "exact to five decimals", case 1 was not, and the comment it
  asked for would have been false. Declining to write a justification you cannot stand behind
  is the cheapest defect-finding mechanism this project has.
- **A plan goes to `_handover/<task>-plan.md` before it is reviewed.** Plan mode blocks writes to
  every file but its own plan file, so present the plan in the terminal as usual, and on approval
  write it to that file **first** and stop — the planning chat reviews the file, not the paste.
  Same mechanism as the report rule below, and the same failure at the other end of the task: on
  2026-09-15 the M2a plan existed only in the terminal, and the paste into the planning chat
  truncated mid-sentence inside its first pushback item — the one questioning whether a recorded
  spike conclusion still stood. A plan that lives only in a terminal is one paste away from being
  reviewed in part. The same applies to `/code-review` findings, which hit this failure on
  2026-09-15.
- **End a task by writing the report to `_handover/last-report-<YYYY-MM-DD>.md` as well as to
  the terminal** — same content: what changed, the commits, the checks, the measured figures,
  and anything you disagreed with. The planning chat reads that file instead of a hand-copied
  paste: on 2026-09-14 four pastes truncated in transit (a mangled word, a table cut mid-row,
  a figure clipped from 0.99962 to 0.99), each a chance to misread a number in a project where
  numbers are the point. If the file already exists for the day, append a new dated section
  rather than overwrite it.
- Verify crate claims against docs.rs or the source before writing code against them.
- Martín prefers concise, factual answers with sources. Skip the preamble.
