# world.ondarmap — build report

Built by `ondar-map-build` at `56c35156d91ef2f97b56fe6a549e51017f62ab1d` from Natural Earth 10m v5.1.2 (the 12 pinned inputs below). Format v1, Deflate per blob.

| | |
|---|---|
| resource | 2712599 B (2.713 MB) |
| SHA-256 | `e2775f811c08f5bc9d52f4653741e8fede00b7d0a1238bd095e306e60f52dc3a` |
| units / countries / blobs | 267 / 248 / 1131 |
| build time | 19.7 s total (store 4.2 s, 14 threads) — the ~10 min budget |

## Bytes, vertices and bounds per layer and level

Bounds are the exact measure (every original vertex to the simplified line), points at the level; the codec adds at most 0.0354 pt (half a quantum's diagonal). Land's limit is 0.25 pt, subdivisions' 0.5 pt.

| layer | level km/pt | blobs | vertices in | vertices out | raw B | deflated B | max bound pt | + codec |
|---|---|---|---|---|---|---|---|---|
| Land | 1.5 | 267 | 547394 | 324794 | 1352168 | 871139 | 0.2500 | 0.2854 |
| Land | 3 | 256 | 544849 | 253690 | 1066556 | 636161 | 0.2500 | 0.2853 |
| Land | 6 | 252 | 543479 | 215181 | 911868 | 482362 | 0.2500 | 0.2854 |
| Land | 12 | 203 | 496049 | 160563 | 689876 | 322050 | 0.2500 | 0.2854 |
| Land | 24 | 124 | 373958 | 93194 | 407460 | 164492 | 0.2500 | 0.2853 |
| Subdivisions | 6 | 18 | 133691 | 15021 | 71352 | 45638 | 0.5000 | 0.5353 |
| Subdivisions | 12 | 10 | 100239 | 6306 | 32116 | 19559 | 0.4998 | 0.5352 |
| Subdivisions | 24 | 1 | 48679 | 1541 | 8568 | 4662 | 0.4998 | 0.5352 |

Total blob bytes: 4539964 B raw (4.540 MB), 2546063 B deflated per blob (2.546 MB). Neighbour-only blobs (a unit at a level only other countries' frames need): 1015404 B raw, 536436 B deflated = 21.1 % of the deflated total. Max bound: land 0.2500 pt, subdivisions 0.5000 pt (spec 0.25 / 0.5).

## Coverage

D6 (decided 2026-10-01): the view stays inside the fit rectangle (the pane at the widest scale, centred on the frame bbox). A unit is stored at level k when a ring's cap, grown by the level's tolerance (bound + codec), meets some country's reach at k — its fit rectangle grown by the 2 pt clip margin at the coarsest scale that uses k — or it is in an inset at the inset's level.

| level km/pt | units stored (of 267) |
|---|---|
| 1.5 | 267 |
| 3 | 256 |
| 6 | 252 |
| 12 | 203 |
| 24 | 124 |

## P4 — the simplifier

Stored: **Hybrid**. The hybrid (decided 2026-10-01): per ring and level, RDP at the level's tolerance, kept if simple (no self-intersection, ≥ 3 distinct vertices, non-zero area) and its exact measure is within the bound; else that ring's per-ring VW. No repair. The rule: ship the hybrid if every bound holds and the build stays under ~10 min, else VW.

RU land at 24 km/pt (open rings, RU's frame LAEA, after the seam stitch): **36503 in; per-ring VW 13664, RDP 3904, stored 12883** (commit 4 measured VW 13 664, RDP 3 904; Q2b one ε per country 16 667 against RDP 4 128). In that blob 15 ring(s) fell back to VW; the largest has 24183 vertices in, RDP 2023 (not simple, or over the bound), VW 11114.

Open vertices before quantisation, summed over the stored blobs:

| layer | level km/pt | rings | per-ring VW | RDP | stored | stored / RDP | rings that fell back to VW |
|---|---|---|---|---|---|---|---|
| Land | 1.5 | 4327 | 450653 | 310744 | 324830 | 1.045 | 16 |
| Land | 3 | 4231 | 379615 | 213554 | 253803 | 1.188 | 41 |
| Land | 6 | 4178 | 302697 | 135358 | 215572 | 1.593 | 89 |
| Land | 12 | 3901 | 200572 | 77223 | 161447 | 2.091 | 154 |
| Land | 24 | 2849 | 109523 | 42349 | 96044 | 2.268 | 216 |
| Subdivisions | 6 | 933 | 20795 | 15003 | 15003 | 1.000 | 0 |
| Subdivisions | 12 | 571 | 8897 | 6300 | 6300 | 1.000 | 0 |
| Subdivisions | 24 | 200 | 2057 | 1560 | 1560 | 1.000 | 0 |

## P2 — neighbours drawn in another projection

Per level, the largest stretch of a displacement between a ring's storage LAEA and a projection that draws it — k'(c_storage) × k'(c_drawing) over the ring's vertices inside that frame's reach (LAEA's scale factors are k' and 1/k'), × level / scale for an inset — and the worst displacement that makes: (ring bound + codec) × stretch, points. A report, not a gate; above ~0.5 pt the remedy (neighbours simplified at the stricter scale) is taken before commit 5.

| level km/pt | max ratio | worst displacement pt | where |
|---|---|---|---|
| 1.5 | 1.3971 | 0.3663 | FRA ring 8 in MG |
| 3 | 1.3971 | 0.3758 | UMI ring 10 in US |
| 6 | 1.3971 | 0.3455 | USA ring 242 in RU |
| 12 | 1.3971 | 0.3420 | PRT ring 0 in RU |
| 24 | 1.2221 | 0.3413 | ESP ring 2 in RU |

## R9 — the seam

- **RUS**: 4 parts on the seam; parts 214 → 212; 4 vertices removed; 4 notches closed (longest 0.147 km)
- **ATA**: 1 parts on the seam; parts 179 → 179; 749 vertices removed; 0 notches closed (longest 0.000 km)
- **FJI**: 6 parts on the seam; parts 44 → 41; 6 vertices removed; 6 notches closed (longest 0.154 km)

Seam edges left in any unit: 0 (must be 0).

## D2 — subdivisions as interior borders

The gate (decided 2026-10-01): every once-found edge whose midpoint lies inside the country's admin-0 land — an interior border found on one side only — within 375 m of the admin-0 rings, and no edge found 3+ times; a country failing it is stored as the prototype's polygons. `inside` counts the once-edges more than 1 m from the rings whose midpoint is inside the land (closer ones lie on the rings); `far once` counts every once-edge more than 1 m away (coast differences between NE's two layers, islets admin 0 lacks).

| code | admin 1 | edges | once | twice (borders) | 3+ | seam | lines | far once (> 1 m) | farthest m | inside | inside farthest m | gate | stored as |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| AQ | 2 | 22887 | 22887 | 0 | 0 | 0 | 0 | 2 | 3.6 | 2 | 3.6 | PASS | borders |
| AR | 24 | 7687 | 4669 | 3018 | 0 | 0 | 43 | 0 | 0.7 | 0 | 0.0 | PASS | borders |
| AU | 15 | 14803 | 13707 | 1096 | 0 | 0 | 11 | 91 | > 10 000 | 0 | 0.0 | PASS | borders |
| BR | 27 | 22720 | 11083 | 11637 | 0 | 0 | 51 | 2 | 5.6 | 0 | 0.0 | PASS | borders |
| CA | 13 | 72035 | 67785 | 4250 | 0 | 0 | 26 | 4 | 1.8 | 2 | 1.8 | PASS | borders |
| CD | 11 | 5834 | 2874 | 2960 | 0 | 0 | 20 | 4 | 2.4 | 0 | 0.0 | PASS | borders |
| CL | 16 | 17878 | 17041 | 837 | 0 | 0 | 16 | 0 | 0.7 | 0 | 0.0 | PASS | borders |
| CN | 32 | 33610 | 14053 | 19557 | 0 | 0 | 72 | 18 | 5.0 | 4 | 4.1 | PASS | borders |
| GL | 6 | 21743 | 19994 | 1749 | 0 | 0 | 10 | 0 | 0.8 | 0 | 0.0 | PASS | borders |
| ID | 33 | 21468 | 19317 | 2151 | 0 | 0 | 31 | 0 | 0.3 | 0 | 0.0 | PASS | borders |
| IN | 36 | 20254 | 7699 | 12555 | 0 | 0 | 73 | 31 | 143.4 | 6 | 28.4 | PASS | borders |
| JP | 47 | 9700 | 6888 | 2812 | 0 | 0 | 86 | 30 | > 10 000 | 0 | 0.0 | PASS | borders |
| KZ | 17 | 5257 | 4143 | 1114 | 0 | 0 | 29 | 6 | 8.6 | 2 | 4.7 | PASS | borders |
| MM | 14 | 7387 | 4567 | 2820 | 0 | 0 | 27 | 10 | 4.0 | 10 | 4.0 | PASS | borders |
| MN | 22 | 3603 | 1493 | 2110 | 0 | 0 | 45 | 0 | 0.0 | 0 | 0.0 | PASS | borders |
| MX | 33 | 14357 | 7387 | 6970 | 0 | 0 | 72 | 6 | 13.1 | 0 | 0.0 | PASS | borders |
| RU | 86 | 85103 | 36602 | 48479 | 0 | 22 | 200 | 121 | > 10 000 | 25 | 70.7 | PASS | borders |
| US | 51 | 44305 | 35662 | 8643 | 0 | 0 | 121 | 12 | 79.3 | 12 | 79.3 | PASS | borders |

Edge-match shares over the 18: 132758 edges found twice (interior borders), 297851 once (the outline), 22 along the seam; the gate passes for 18 of 18.

Subdivision bytes: 69859 B deflated (112036 B raw), against 527 856 B deflated when 13 of the 18 were stored as polygons (commit 4, VW).

## D1 — raw or deflated

`Store::load` from a file (read + parse + inflate + CRC), release build, 10 warm-ups + 100 runs; the clock reads in 14 ns.

| encoding | bytes | load median ms | p90 ms |
|---|---|---|---|
| raw | 4706500 | 0.60 | 0.63 |
| deflated per blob | 2712599 | 17.70 | 18.22 |

The rule: deflate unless its load exceeds 50 ms → **Deflate**.

## Insets (S6)

| code | label | box (x, y, w, h) | km/pt | level | clearance pt |
|---|---|---|---|---|---|
| EC | Galápagos | 240, 232, 80, 60 | 7.749 | 6 | 53.5 |
| ES | Canary Islands | 240, 232, 80, 60 | 6.446 | 6 | 48.7 |
| FR | French Guiana | 8, 8, 60, 44 | 14.431 | 12 | 15.0 |
| FR | Guadeloupe & Martinique | 260, 8, 60, 44 | 8.358 | 6 | 20.3 |
| FR | Réunion | 8, 248, 60, 44 | 2.023 | 1.5 | 17.4 |
| IN | Andaman & Nicobar | 240, 232, 80, 60 | 17.521 | 12 | 76.5 |
| MY | Sabah & Sarawak | 240, 8, 80, 60 | 16.432 | 12 | 26.6 |
| NO | Svalbard | 8, 8, 80, 60 | 16.185 | 12 | 61.1 |
| PF | Marquesas | 240, 8, 80, 60 | 6.572 | 6 | 21.4 |
| PT | Azores | 10, 24, 92, 52 | 8.615 | 6 | 24.8 |
| PT | Madeira | 10, 84, 52, 40 | 14.273 | 12 | 55.2 |
| US | Alaska | 8, 236, 84, 56 | 51.131 | 24 | 35.3 |
| US | Hawaii | 98, 258, 60, 32 | 32.642 | 24 | 19.5 |
| YE | Socotra | 240, 232, 80, 60 | 3.735 | 3 | 55.2 |

## S4 — map units

- RE: 1 of 1 parts matched in the parent
- GF: 1 of 1 parts matched in the parent
- MQ: 1 of 1 parts matched in the parent
- YT: 2 of 2 parts matched in the parent
- GP: 6 of 6 parts matched in the parent
- BQ: 3 of 3 parts matched in the parent
- SJ: 22 of 22 parts matched in the parent
- CC: 2 of 2 parts matched in the parent
- CX: 1 of 1 parts matched in the parent

## Inputs (pinned)

| file | bytes | SHA-256 |
|---|---|---|
| ne_10m_admin_0_countries.shp | 8806224 | `7ce119ef6342e43cff7c0c3004e0911ab7ec1988a14734372031d2012180e7bc` |
| ne_10m_admin_0_countries.shx | 2164 | `ca19ec112d054c77bc8f7ac00e3b110d5dff32cc9bcf4cd1b8b66bdd0f611d32` |
| ne_10m_admin_0_countries.dbf | 878482 | `c5dbd3dd5fd7e2ef49051fc88562c03819e8ea63a382642df6eadd1243bf4b49` |
| ne_10m_admin_0_countries.prj | 145 | `a02a27b1d1982c8516d83398e85a3c8b1aef1713c13ef4d84d7bde17430c07c4` |
| ne_10m_admin_0_map_units.shp | 8878092 | `e62f2508578b812aeca46cd3d2f208e0a6ff141fb0d4048c0d984839f7917bcc` |
| ne_10m_admin_0_map_units.shx | 2484 | `5d5081948127a1b3153866ba6d6671df47ba26522175482ada82f1d413328c39` |
| ne_10m_admin_0_map_units.dbf | 1124400 | `0e0c5817fbf299fe9f65ba5f1e5abe66b5a84d6d16bda5b7fafba3861f2e5b20` |
| ne_10m_admin_0_map_units.prj | 145 | `a02a27b1d1982c8516d83398e85a3c8b1aef1713c13ef4d84d7bde17430c07c4` |
| ne_10m_admin_1_states_provinces.shp | 20998780 | `c6f5c8b4b1320d9417033762419c6df1eb423989cd880fba78ea0b1e3522cbe4` |
| ne_10m_admin_1_states_provinces.shx | 36868 | `37a9e2bc79ed31d3bdea3cb62d928f77281a1c88d645cd33430231c75dbcf350` |
| ne_10m_admin_1_states_provinces.dbf | 15161514 | `445a8a9bea889634faf0af18081830df0b05b8471fc6af8dc42aecdd7a71bba1` |
| ne_10m_admin_1_states_provinces.prj | 145 | `a02a27b1d1982c8516d83398e85a3c8b1aef1713c13ef4d84d7bde17430c07c4` |
