# Fixture provenance

Station and country data © radio-browser.info contributors, mirrored here for tests under the
API's own stated freedoms. The API documentation (`https://api.radio-browser.info/`, "Infos",
fetched 2026-09-21) says, verbatim: *"This API is completely free and open source. Your freedoms
are: You may use it in free and non free software. You may install it on your own server and
mirror all its data. You may fork it…"*. No formal data licence (CC0, ODbL, …) is published; the
server code is AGPL-3.0 (github.com/segler-alex/radiobrowser-api-rust), which covers the server,
not the data; the dump directory (`backups.radio-browser.info`) carries no terms. Recorded in
`_handover/m3a-plan.md` § F5 and accepted by Martín at the M3a plan review (2026-09-21).

Every file is a slice of a response recorded by the M3 Step 0 census on 2026-09-21
(`_handover/m3-step0-logs/`, `User-Agent: Ondar/0.1.0`, server `de1.api.radio-browser.info`,
`software_version` 0.7.45):

| file | from | how |
|---|---|---|
| `countries.json` | `p2-countries.json` (`GET /json/countries`) | byte-for-byte copy, 250 rows |
| `stations-MT.json` | `p3-MT.json` (`GET /json/stations/bycountrycodeexact/MT`) | byte-for-byte copy, 14 rows, the whole country |
| `stations-PT-60.json` | `p3-PT.json` (`…/bycountrycodeexact/PT`, 371 rows) + one row of `p3-US-full.json` | `scripts/fixture-slice.py`: the first 50 rows as served, then one `lastcheckok == 0`, one `bitrate == 0`, one `hls == 1`, a folded-name duplicate pair, and one empty `url_resolved` row (PT has none; taken from US, its country rewritten to PT; such rows are also `lastcheckok == 0`) |
| `search-i1.json`, `search-i2.json` | `p6-i1.json`, `p6-i2.json` (`/json/stations/search?name=radio&countrycode=PT&order=name&limit=5[&offset=5]`) | copies — pagination |
| `search-j.json` | `p6-j.json` (`/json/stations/search?nameExact=true&name=ORBITAL`) | copy — exactness (case-insensitive) |

Regenerate with `python3 scripts/fixture-slice.py _handover/m3-step0-logs src-tauri/crates/ondar-stations/fixtures`.
Credit in the app: "station data: radio-browser.info" (README, M6).
