---
paths:
  - "src-tauri/crates/ondar-stations/**"
  - "src-tauri/src/commands/stations.rs"
---

# Station directory rules

- **API etiquette (non-negotiable):** a `User-Agent` of `Ondar/<version>` on every request; servers
  from the `_api._tcp.radio-browser.info` SRV record with the measured fallbacks; the click endpoint
  once per play, **never retried** (a retry could be a second vote); cache countries 7 days and
  station lists 24 h, and serve the cache offline. The page's row does nothing on the station
  already playing and resumes a paused one, so a double click is not two votes.
- **The DB thread never awaits the network.** Fetches run on the service's own 2-worker runtime,
  coalesced per country; stale-while-revalidate. Every fetch ends with exactly one event, and
  `landed` means the cache write succeeded, not only the fetch. `cached_stations(cc)` (the map's
  read, M4c) answers the stored list, expired or not, or `None`, and never starts a fetch.
- **The client:** same-host retries under a 200 s budget; an explicit `limit=` on every list; the
  truncation guard; an empty countries answer is an error.
- **The cache** (rusqlite, bundled): `user_version` migrations, idempotent; expired lists are kept
  with their age; a corrupt database is moved aside and recreated, and the app launches regardless.
- **Ranking is Rust's:** drop broken and empty-url rows, dedupe, sort votes → known bitrate first →
  clicktrend, cap 750. The page never fetches, filters or ranks.
- With the measurement harness active the click is suppressed and the recent still recorded.
