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

    python3 scripts/fixture-slice.py --geo <CC> <census file> <fixtures dir>

M4c (plan § 5, A4): stations-<CC>-geo.json = the rows of one census list that survive
ondar-stations' `filter::rank` (cap 750) **and** carry a position, as served (census order, the
rows unchanged). The rank is re-implemented here; the Rust test
`the_geo_slices_normalise_to_their_row_counts` checks the slice against the real one.
"""
import json, shutil, sys
from collections import Counter
from pathlib import Path

def geo_slice(cc, census, dst):
    rows = json.load(open(census))
    # filter::rank: drop !lastcheckok and an empty url; dedupe on (folded trimmed name, url),
    # the first row kept unless a later one has more votes; sort votes desc, known bitrate
    # first, click trend desc, uuid; cap 750. normalise trims name and url_resolved.
    best = {}
    for r in rows:
        url = (r.get("url_resolved") or "").strip()
        if r.get("lastcheckok") != 1 or not url:
            continue
        key = (r["name"].strip().lower(), url)
        if key not in best or best[key]["votes"] < r["votes"]:
            best[key] = r
    ranked = sorted(best.values(), key=lambda r: (-r["votes"], (r.get("bitrate") or 0) == 0,
                                                  -r["clicktrend"], r["stationuuid"]))[:750]
    def geo(r):
        lat, lng = r.get("geo_lat"), r.get("geo_long")
        return lat is not None and lng is not None and not (lat == 0 and lng == 0)
    keep = {r["stationuuid"] for r in ranked if geo(r)}
    out = [r for r in rows if r["stationuuid"] in keep and r is best.get(
        (r["name"].strip().lower(), (r.get("url_resolved") or "").strip()))]
    assert len(out) == len(keep), "a uuid served twice"
    assert all(r["countrycode"].upper() == cc for r in out), "a row of another country"
    name = f"stations-{cc}-geo.json"
    json.dump(out, open(Path(dst) / name, "w"), ensure_ascii=False, separators=(",", ":"))
    print(f"{name}: {len(out)} rows ({len(rows)} served, {len(ranked)} ranked), "
          f"{(Path(dst) / name).stat().st_size} B")

if sys.argv[1] == "--geo":
    geo_slice(sys.argv[2], Path(sys.argv[3]), sys.argv[4])
    sys.exit(0)

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
