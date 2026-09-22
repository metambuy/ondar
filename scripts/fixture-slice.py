#!/usr/bin/env python3
"""Make ondar-stations' station fixtures from the M3 Step 0 census files (2026-09-21).

    python3 scripts/fixture-slice.py _handover/m3-step0-logs src-tauri/crates/ondar-stations/fixtures

Deterministic. stations-PT-60.json = the first 50 rows of p3-PT.json as served, plus one row
each of: lastcheckok == 0, bitrate == 0, hls == 1, a folded-name duplicate pair (both rows), and
one empty url_resolved row — PT has none, so that row comes from p3-US-full.json and its
countrycode is rewritten to PT so the fixture stays one country (such rows are also
lastcheckok == 0, which the tests account for). Rows already among the first
50 are not added twice; the extras are appended in this order after the 50. countries.json,
stations-MT.json and the search-*.json files are byte-for-byte copies.
"""
import json, shutil, sys
from collections import Counter
from pathlib import Path

src, dst = Path(sys.argv[1]), Path(sys.argv[2])
dst.mkdir(parents=True, exist_ok=True)
pt = json.load(open(src / "p3-PT.json"))
rows = pt[:50]
have = {s["stationuuid"] for s in rows}

def first(pred, pool):
    for s in pool:
        if s["stationuuid"] not in have and pred(s):
            return s
    raise SystemExit("no row for a required case")

extras = [
    first(lambda s: s["lastcheckok"] == 0, pt),
    first(lambda s: s["bitrate"] == 0 and s["lastcheckok"] == 1, pt),
    first(lambda s: s["hls"] == 1 and s["lastcheckok"] == 1, pt),
]
keys = Counter((s["name"].strip().casefold(), s["url_resolved"]) for s in pt)
dupe_key = next(k for k, v in keys.items() if v > 1 and all(
    s["stationuuid"] not in have for s in pt if (s["name"].strip().casefold(), s["url_resolved"]) == k))
extras += [s for s in pt if (s["name"].strip().casefold(), s["url_resolved"]) == dupe_key][:2]
us = json.load(open(src / "p3-US-full.json"))
# An empty url_resolved is what a failed check leaves behind, so such rows are lastcheckok == 0;
# the fixture keeps the row as it is (the empty url, not the check flag, is the case under test).
empty = dict(first(lambda s: not s["url_resolved"], us))
empty["countrycode"] = "PT"; empty["country"] = "Portugal"
extras.append(empty)
for s in extras:
    if s["stationuuid"] not in have:
        rows.append(s); have.add(s["stationuuid"])
json.dump(rows, open(dst / "stations-PT-60.json", "w"), ensure_ascii=False, separators=(",", ":"))
print(f"stations-PT-60.json: {len(rows)} rows ({len(extras)} extras)")
for a, b in [("p2-countries.json", "countries.json"), ("p3-MT.json", "stations-MT.json"),
             ("p6-i1.json", "search-i1.json"), ("p6-i2.json", "search-i2.json"), ("p6-j.json", "search-j.json")]:
    shutil.copyfile(src / a, dst / b); print(f"{b}: {(dst / b).stat().st_size} B (copy of {a})")
