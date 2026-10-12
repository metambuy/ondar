# world.ondarmap — build report

Built by `ondar-map-build` at `18028fcd0ff882db5cf8480daedade8e10bc527f` (working tree with changes) from Natural Earth 10m v5.1.2 (the 12 pinned inputs below). Format v3, Deflate per blob.

| | |
|---|---|
| resource | 2865839 B (2.866 MB) |
| SHA-256 | `acab65175723170273544b925be0262bccc8dc3ab3bbff4936747d94e0a70373` |
| units / countries / blobs | 267 / 248 / 1634 |
| build time | 20.6 s total (store 6.2 s, 14 threads) — the ~10 min budget |

## Bytes, vertices and bounds per layer and level

Bounds are the exact measure (every original vertex to the simplified line), points at the level; the codec adds at most 0.0354 pt (half a quantum's diagonal). Land's limit is 0.25 pt, subdivisions' 0.5 pt.

| layer | level km/pt | blobs | vertices in | vertices out | raw B | deflated B | max bound pt | + codec |
|---|---|---|---|---|---|---|---|---|
| Land | 1.5 | 253 | 546269 | 317022 | 1320656 | 855983 | 0.2500 | 0.2854 |
| Land | 3 | 267 | 547394 | 235495 | 994952 | 609238 | 0.2500 | 0.2853 |
| Land | 6 | 267 | 547394 | 181506 | 778948 | 437570 | 0.2500 | 0.2854 |
| Land | 12 | 267 | 547394 | 144000 | 628704 | 316696 | 0.2500 | 0.2854 |
| Land | 24 | 267 | 547394 | 116158 | 516864 | 225791 | 0.2500 | 0.2854 |
| Land | 48 | 267 | 547394 | 86020 | 395816 | 149868 | 0.2500 | 0.2854 |
| Subdivisions | 6 | 18 | 133691 | 15021 | 71352 | 45638 | 0.5000 | 0.5353 |
| Subdivisions | 12 | 17 | 131536 | 8863 | 46176 | 27940 | 0.4998 | 0.5352 |
| Subdivisions | 24 | 11 | 112444 | 4141 | 24084 | 13543 | 0.4998 | 0.5352 |

Total blob bytes: 4777552 B raw (4.778 MB), 2682267 B deflated per blob (2.682 MB). Neighbour-only blobs (a unit at a level only other countries' frames need): 887584 B raw, 429734 B deflated = 16.0 % of the deflated total. Max bound: land 0.2500 pt, subdivisions 0.5000 pt (spec 0.25 / 0.5).

## Coverage

D6 (decided 2026-10-01): the view stays inside the fit rectangle (the pane at the widest scale, centred on the frame bbox). **Per band (M4b commit 3):** the pane is 328 × h for every integer h in 140..=300, each with its own fit and fit rectangle. A unit is stored at level k when a ring's cap, grown by the level's tolerance (bound + codec), meets some country's reach at k — the bounding rectangle, over every band whose views can use k, of that band's fit rectangle grown by the 2 pt clip margin at the coarsest scale that uses k — or it is in an inset at the inset's level.

| level km/pt | units stored (of 267) |
|---|---|
| 1.5 | 253 |
| 3 | 267 |
| 6 | 267 |
| 12 | 267 |
| 24 | 267 |
| 48 | 267 |

**The bound against the exact union** (the union of the bands' reaches is not a rectangle): the bounding rectangle asks for 0 land blob(s) no single band's reach asks for, 0 B deflated of 2595146 B (0.00 %). The rule: over 5 % and the exact union is stored instead — not applied, the bound's blobs are stored.

**Collapsed rings** (fewer than three distinct quanta at the level; stored empty, no frame can draw them): 600. Kept as empty rings rather than dropped (the commit 4 STOP's decision 7): the loader requires a land blob's ring count to equal its unit's, part by part (`Corrupt { owner }`), because the ring index — the caps in the units table — addresses a blob's rings by position without decoding them; dropping a ring from one level's blob would need a per-blob ring map, and the slot costs 8 B in the ring table (4800 B raw here, before deflate). By level 1.5 / 3 / 6 / 12 / 24 / 48: 2 / 5 / 17 / 72 / 190 / 314. By unit (counts at each level): AIA 0/0/0/0/3/3, ASM 0/0/0/0/0/1, ATF 0/0/0/1/2/2, ATG 0/0/0/0/1/1, AUS 0/0/0/0/0/1, BHR 0/0/0/0/0/1, BHS 0/0/0/1/3/5, BJN 0/0/0/1/1/1, BRA 0/0/0/0/1/0, CAN 0/0/0/0/0/1, CHL 0/0/0/0/1/1, CHN 0/0/0/1/1/7, COK 0/0/0/0/1/4, COL 0/0/1/1/1/1, CSI 0/1/1/1/1/1, DEU 0/0/0/0/0/2, ECU 0/0/0/1/1/2, EGY 0/0/0/0/0/1, ESB 0/0/0/0/0/1, ESP 0/1/3/2/4/5, FRA 0/0/0/0/1/0, FSM 0/0/0/0/1/7, GAB 0/0/0/0/1/1, GBR 0/0/0/0/1/2, GGY 0/0/0/0/0/1, GRC 0/0/0/0/0/1, GRL 0/0/0/0/0/1, HND 0/0/0/0/0/1, IDN 0/0/0/0/2/2, IND 0/0/0/0/1/5, IOT 0/0/0/0/2/7, ITA 0/1/0/1/1/2, JPN 1/1/3/5/9/12, KIR 0/0/0/0/1/2, KOR 0/0/0/1/1/1, MDV 1/0/4/40/109/145, MEX 0/0/0/1/1/2, MHL 0/0/0/1/2/7, NCL 0/0/0/0/1/1, NZL 0/0/0/0/0/1, PCN 0/0/0/0/0/2, PGA 0/0/0/2/3/10, PHL 0/0/0/1/4/5, PNG 0/0/0/0/1/2, PRT 0/0/0/1/1/1, PYF 0/0/0/1/2/8, SCR 0/0/0/1/1/1, SER 0/0/0/1/1/1, SLB 0/0/0/0/0/1, SWE 0/0/0/0/0/1, SYC 0/0/0/0/4/13, TCA 0/0/0/0/0/1, THA 0/0/0/0/0/1, TUV 0/0/0/0/1/3, UMI 0/0/1/1/3/5, USA 0/0/1/1/7/9, VAT 0/1/1/1/1/1, VCT 0/0/0/0/0/1, VEN 0/0/2/4/6/6

**Subdivision candidates at the shortest band** (flagged off at the golden fit, fit at 140 above 8 km/pt; the flag is decided at the golden fit — an observation, not a rule the build applies): AF (10.09 at 140), AO (15.13 at 140), BO (14.73 at 140), BW (10.18 at 140), CF (9.73 at 140), CG (9.71 at 140), CM (12.69 at 140), CO (18.56 at 140), DE (8.68 at 140), DZ (20.23 at 140), EG (10.86 at 140), ES (9.70 at 140), ET (12.77 at 140), FI (11.37 at 140), FR (10.56 at 140), GB (12.14 at 140), GY (8.20 at 140), IQ (9.21 at 140), IR (16.43 at 140), IT (12.89 at 140), KE (10.79 at 140), LA (9.54 at 140), LY (15.16 at 140), MA (16.00 at 140), MG (15.17 at 140), ML (16.50 at 140), MR (13.96 at 140), MV (8.67 at 140), MW (8.62 at 140), MZ (18.18 at 140), NA (13.25 at 140), NE (13.13 at 140), NG (10.69 at 140), NO (14.36 at 140), NZ (20.54 at 140), OM (10.81 at 140), PE (20.38 at 140), PF (10.23 at 140), PG (11.47 at 140), PH (18.28 at 140), PK (14.98 at 140), PY (9.24 at 140), SA (17.61 at 140), SD (14.97 at 140), SE (15.16 at 140), SO (15.22 at 140), SS (9.71 at 140), TD (17.77 at 140), TH (16.47 at 140), TM (8.47 at 140), TZ (11.93 at 140), UZ (9.44 at 140), VE (12.85 at 140), VN (16.44 at 140), ZA (14.10 at 140), ZM (10.96 at 140).

## Insets per band (I1, C1)

At every band height the controls' rect (`rules::controls_rect`, 74 × 24 pt, 8 pt from the bottom and right) is placed first; each inset row, in table order, takes the largest scale in whole percent at which its box — the golden size scaled, the label strip and the pads not, anchored at its corner with the row's gaps — is inside the pane, apart from every box placed before it and ≥ 12 pt from the land the frame draws there. The minimum is a land area of 28 × 12 pt (box ≥ 36 × 28). Labels at the artifact's 8 pt, 0.6 em a character and 0.3 em a space.

| inset | corner | min % (at) | 140 | 161 | 178 | 200 | 250 | 300 | label pt | inner 178 / 300 |
|---|---|---|---|---|---|---|---|---|---|---|
| EC Galápagos | TopLeft | 79 (300) | 100 | 100 | 100 | 100 | 94 | 79 | 43.2 | 72 / 55 |
| ES Canaries | BottomLeft | 71 (284) | 100 | 100 | 98 | 86 | 81 | 74 | 38.4 | 70 / 51 |
| FR Fr. Guiana | TopLeft | 99 (202) | 100 | 100 | 100 | 100 | 100 | 100 | 45.6 | 52 / 52 |
| FR Antilles | TopRight | 100 (140) | 100 | 100 | 100 | 100 | 100 | 100 | 38.4 | 52 / 52 |
| FR Réunion | BottomLeft | 100 (140) | 100 | 100 | 100 | 100 | 100 | 100 | 33.6 | 52 / 52 |
| IN Andamans | TopLeft | 89 (227) | 100 | 100 | 100 | 95 | 90 | 90 | 38.4 | 72 / 64 |
| MY Sabah & Sarawak | TopRight | 100 (140) | 100 | 100 | 100 | 100 | 100 | 100 | 67.2 | 72 / 72 |
| NO Svalbard | TopLeft | 0 (225) | 100 | 90 | 78 | 63 | 0 | 100 | 38.4 | 54 / 72 |
| PF Marquesas | TopRight | 78 (165) | 81 | 79 | 80 | 88 | 100 | 100 | 43.2 | 56 / 72 |
| PT Azores | TopLeft | 100 (140) | 100 | 100 | 100 | 100 | 100 | 100 | 28.8 | 84 / 84 |
| PT Madeira | TopLeft | 100 (140) | 100 | 100 | 100 | 100 | 100 | 100 | 33.6 | 44 / 44 |
| US Alaska | BottomLeft | 80 (156) | 85 | 80 | 83 | 83 | 100 | 100 | 28.8 | 62 / 76 |
| US Hawaii | BottomLeft | 0 (140) | 0 | 0 | 0 | 0 | 0 | 100 | 28.8 | — / 52 |
| YE Socotra | TopLeft | 61 (192) | 86 | 74 | 65 | 61 | 92 | 100 | 33.6 | 44 / 72 |

The corner table — each box alone after the controls, with its own gaps, at TL / TR / BL: min % over the bands (at), % at 178, % at 300, the full box's clearance at 161 in pt.

| inset | current | TL | TR | BL |
|---|---|---|---|---|
| EC Galápagos | TopLeft | 79 (300) · 100 · 79 · 23.5 | 79 (296) · 100 · 79 · 21.7 | 60 (300) · 100 · 60 · 22.4 |
| ES Canaries | BottomLeft | 0 (222) · 79 · 0 · 5.2 | 58 (263) · 96 · 73 · 7.1 | 71 (284) · 98 · 74 · 18.8 |
| FR Fr. Guiana | TopLeft | 99 (202) · 100 · 100 · 30.3 | 100 (140) · 100 · 100 · 49.6 | 100 (140) · 100 · 100 · 55.2 |
| FR Antilles | TopRight | 99 (202) · 100 · 100 · 30.3 | 100 (140) · 100 · 100 · 49.6 | 100 (140) · 100 · 100 · 55.2 |
| FR Réunion | BottomLeft | 99 (202) · 100 · 100 · 30.3 | 100 (140) · 100 · 100 · 49.6 | 100 (140) · 100 · 100 · 55.2 |
| IN Andamans | TopLeft | 89 (227) · 100 · 90 · 18.1 | 74 (238) · 97 · 89 · 18.1 | 59 (300) · 100 · 59 · 26.6 |
| MY Sabah & Sarawak | TopRight | 0 (290) · 100 · 0 · 24.4 | 100 (140) · 100 · 100 · 42.6 | 100 (140) · 100 · 100 · 48.9 |
| NO Svalbard | TopLeft | 0 (225) · 78 · 100 · 4.7 | 0 (295) · 100 · 0 · 24.5 | 0 (294) · 100 · 0 · 24.5 |
| PF Marquesas | TopRight | 0 (148) · 0 · 100 · 0.0 | 78 (165) · 80 · 100 · 0.0 | 63 (184) · 68 · 100 · 0.0 |
| PT Azores | TopLeft | 100 (140) · 100 · 100 · 35.6 | 85 (300) · 100 · 85 · 32.7 | 100 (140) · 100 · 100 · 32.7 |
| PT Madeira | TopLeft | 100 (140) · 100 · 100 · 72.7 | 0 (140) · 100 · 100 · 84.8 | 100 (140) · 100 · 100 · 74.9 |
| US Alaska | BottomLeft | 0 (183) · 52 · 68 · 0.0 | 0 (171) · 0 · 86 · 0.0 | 80 (156) · 83 · 100 · 0.0 |
| US Hawaii | BottomLeft | 0 (140) · 0 · 100 · 0.0 | 0 (140) · 0 · 100 · 0.0 | 0 (140) · 0 · 100 · 0.0 |
| YE Socotra | TopLeft | 61 (192) · 65 · 100 · 0.0 | 0 (199) · 59 · 64 · 0.0 | 0 (181) · 48 · 64 · 0.0 |

**The stacking rule** (commit 4b, decision 1): a box whose golden rect abuts another's row or column at the same corner keeps the golden gap to that box's near edge as it shrinks (Hawaii beside Alaska, Madeira under the Azores). First band each inset is drawn at: EC Galápagos 140, ES Canaries 140, FR Fr. Guiana 140, FR Antilles 140, FR Réunion 140, IN Andamans 140, MY Sabah & Sarawak 140, NO Svalbard 140, PF Marquesas 140, PT Azores 140, PT Madeira 140, US Alaska 140, US Hawaii 274, YE Socotra 140.


The ship gate (an inset in `MAY_DROP_AT_178`, [("US", "Hawaii")], may be dropped at 178 — decision 1, case (c)): 0 inset(s) dropped at 178 or 300; 0 label(s) wider than their box.

## P4 — the simplifier

Stored: **Hybrid**. The hybrid (decided 2026-10-01): per ring and level, RDP at the level's tolerance, kept if simple (no self-intersection, ≥ 3 distinct vertices, non-zero area) and its exact measure is within the bound; else RDP again at t/2, t/4, t/8, the first simple result within the bound kept iff it has fewer vertices than the ring's per-ring VW (the retry, M4c); else that VW. No repair. The rule: ship the hybrid if every bound holds and the build stays under ~10 min, else VW.

RU land at level 4, 24 km/pt, the level its fit draws it at (open rings, RU's frame LAEA, after the seam stitch): **36503 in; per-ring VW 13664, RDP 3904, stored 12882** (M4a commit 4 measured VW 13 664, RDP 3 904; Q2b one ε per country 16 667 against RDP 4 128). In that blob the retry landed 1 / 0 / 0 ring(s) at t/2, t/4, t/8 and 14 ring(s) fell back to VW; the largest has 24183 vertices in, RDP 2023 (not simple, or over the bound), VW 11114.

Open vertices before quantisation, summed over the stored blobs:

| layer | level km/pt | rings | per-ring VW | RDP | stored | stored / RDP | rings the retry landed, t/2 / t/4 / t/8 | rings that fell back to VW |
|---|---|---|---|---|---|---|---|---|
| Land | 1.5 | 4297 | 449902 | 310144 | 317061 | 1.022 | 6 / 2 / 0 | 8 |
| Land | 3 | 4327 | 380946 | 214671 | 235619 | 1.098 | 22 / 3 / 0 | 18 |
| Land | 6 | 4327 | 304144 | 136502 | 181931 | 1.333 | 42 / 12 / 0 | 43 |
| Land | 12 | 4327 | 218342 | 85542 | 145121 | 1.696 | 56 / 20 / 1 | 104 |
| Land | 24 | 4327 | 156441 | 62425 | 120262 | 1.927 | 45 / 19 / 3 | 277 |
| Land | 48 | 4327 | 99509 | 67900 | 102120 | 1.504 | 34 / 10 / 4 | 396 |
| Subdivisions | 6 | 933 | 20795 | 15003 | 15003 | 1.000 | 0 / 0 / 0 | 0 |
| Subdivisions | 12 | 888 | 12264 | 8853 | 8853 | 1.000 | 0 / 0 / 0 | 0 |
| Subdivisions | 24 | 623 | 5411 | 4158 | 4158 | 1.000 | 0 / 0 / 0 | 0 |

## P2 — neighbours drawn in another projection

Per level, the largest stretch of a displacement between a ring's storage LAEA and a projection that draws it — k'(c_storage) × k'(c_drawing) over the ring's vertices inside that frame's reach (LAEA's scale factors are k' and 1/k'), × level / scale for an inset — and the worst displacement that makes: (ring bound + codec) × stretch, points. A report, not a gate; above ~0.5 pt the remedy (neighbours simplified at the stricter scale) is taken before commit 5.

| level km/pt | max ratio | worst displacement pt | where |
|---|---|---|---|
| 1.5 | 1.0556 | 0.2937 | IOA ring 1 in AU |
| 3 | 1.4982 | 0.4257 | GIN ring 0 in RU |
| 6 | 1.4994 | 0.4267 | LBR ring 0 in RU |
| 12 | 1.5018 | 0.4280 | LBR ring 0 in RU |
| 24 | 1.5018 | 0.4231 | LBR ring 0 in RU |
| 48 | 1.5242 | 0.4322 | LBR ring 0 in RU |

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

Subdivision bytes: 87121 B deflated (141612 B raw), against 527 856 B deflated when 13 of the 18 were stored as polygons (commit 4, VW).

## D1 — raw or deflated

Not run (`--bench`).

## Insets (S6)

| code | label | box (x, y, w, h) | km/pt | level | clearance pt |
|---|---|---|---|---|---|
| EC | Galápagos | 8, 8, 80, 60 | 7.749 | 6 | 0.0 |
| ES | Canaries | 8, 232, 80, 60 | 6.446 | 6 | 0.0 |
| FR | Fr. Guiana | 8, 8, 60, 44 | 14.431 | 12 | 15.0 |
| FR | Antilles | 260, 8, 60, 44 | 8.358 | 6 | 20.3 |
| FR | Réunion | 8, 248, 60, 44 | 2.023 | 1.5 | 17.4 |
| IN | Andamans | 8, 8, 80, 60 | 17.521 | 12 | 2.9 |
| MY | Sabah & Sarawak | 240, 8, 80, 60 | 16.432 | 12 | 26.6 |
| NO | Svalbard | 8, 8, 80, 60 | 16.185 | 12 | 61.1 |
| PF | Marquesas | 240, 8, 80, 60 | 6.572 | 6 | 21.4 |
| PT | Azores | 10, 24, 92, 52 | 8.615 | 6 | 24.8 |
| PT | Madeira | 10, 84, 52, 40 | 14.273 | 12 | 55.2 |
| US | Alaska | 8, 236, 84, 56 | 51.131 | 24 | 35.3 |
| US | Hawaii | 98, 258, 60, 32 | 32.642 | 24 | 19.5 |
| YE | Socotra | 8, 8, 80, 60 | 3.735 | 3 | 32.7 |

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
