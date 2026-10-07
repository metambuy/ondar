---
paths:
  - "src-tauri/src/commands/**"
  - "src-tauri/src/lib.rs"
  - "src/api.ts"
  - "src/bindings/**"
  - "src-tauri/crates/ondar-audio/src/types.rs"
  - "src-tauri/crates/ondar-stations/src/model.rs"
---

# The IPC contract

`src/api.ts` wraps every command and event; nothing else imports `@tauri-apps/api`. Event names are
defined once, in `lib.rs::events`, and the shell's forwarder emits them (and hands a `Started`
station id to the stations service). No boundary type is typed by hand in TS: ts-rs generates them.

**Audio** (`commands/audio.rs`): three groups, and the group decides what a return value means.

| Group | Commands | Return |
|---|---|---|
| Channel message | `play(url, stationId, bitrateKbps)`, `set_volume` | `Result` = argument validation only, never a playback outcome (that is an event) |
| Channel message | `pause`, `resume`, `stop` | `()` |
| Direct engine access | `set_eq_gain`, `get_eq`, `get_playback_state` | `set_eq_gain` validates; the getters return data |

EQ changes are **not** ordered against `play`/`stop`: gains live on the engine handle and outlive
any session.

**Panel** (`commands/panel.rs`), never touching the engine: `panel_escape`, `panel_set_expanded`
(Rust lays out, applies or refuses), `panel_layout_committed(generation)` (the D3 round trip: Rust
completes a show or resize only when the page reports the generation; a 250 ms fallback, and
`trigger=fallback` on a healthy page is a defect), `panel_view_back`, `get_panel_layout`.

**Stations** (`commands/stations.rs`), all `async` messages to the service's DB thread:
`list_countries`, `list_stations`, `search_stations`, `list_favourites`, `add_favourite`,
`remove_favourite`, `list_recents`. Lists carry provenance (`source`, `fetched_at`, `age_secs`,
`refreshing`); an expired list is served at once while Rust refreshes it, and only a missing one
waits, bounded by the client's 200 s budget. Errors are `{ code: "stations", message }` (nothing
cached, a truncated list, a cache failure, the directory unavailable) or `invalid_argument`. Rust
records a play itself; the page never fetches, filters or ranks.

**Map** (`commands/map.rs`): `map_select(code)` and `map_pull(inputs)`. Replies, not events; every
reply carries `seq`, and the page draws one only if it is newer than the frame on screen.
`no_map`, `unavailable`, `no_band` are replies, not errors; `map_pull` is always `Ok`. The band is
the one Rust laid out, never the page's.

**Events:** `playback:state`, `playback:stream_info`, `playback:metadata`, `playback:reconnect`,
`stations:updated` and `countries:updated` (`outcome` `landed` → re-request; `failed` → keep the
list, clear `refreshing`, do **not** re-request), `recents:updated`, `panel:layout` (a
`PanelLayout`; the page mirrors it and computes none of it).
