# ondar-map fixtures

| file | from | what |
|---|---|---|
| `laea-reference.tsv` | M4a Step 0, Q5 (2026-09-30): `_handover/m4-step0/q5/points-rust.tsv` (case, centre, radius, point), `points-pyproj.tsv` (`py_sph_*`) and `points-d3.tsv`, joined by case, values copied verbatim | 37 points: Snyder's worked example (USGS PP 1395, pp. 332–333: R = 3, φ1 = 40° N, λ0 = 100° W; 20° S, 100° E) and, for PRT, USA, RUS and FJI, the centre, the four bbox corners, a point 60° north, 90° east, the North Pole and (RUS, FJI) both sides of the antimeridian. Projected by pyproj 3.6.1 (PROJ 9.3.0, `+proj=laea +R=…`) and d3-geo 3.1.1 (`geoAzimuthalEqualArea`, scale R), in metres, y up. The three implementations (with the Step 0 prototype) agree to 1.214e-8 m (`q5-laea-verify.log`). |

The R column is the WGS84 authalic radius, 6 371 007.2 m, except Snyder's R = 3.
