//! M4a § 5's frame tests, on the shipped resource: the golden table, Step 0 reproduced, the
//! antimeridian, Antarctica, the insets' clearance, R7's lookup, subdivisions by scale, the clamp
//! (D6) and the index's exactness.

use ondar_map::format::{Layer, Role, Store};
use ondar_map::frame::{Frame, Lookup, View};
use ondar_map::laea::Laea;
use ondar_map::rules::{self, FLOOR_KM_PER_PT, LADDER, Pane};
use std::sync::OnceLock;

fn resource_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/map/world.ondarmap")
}

fn store() -> &'static Store {
    static S: OnceLock<Store> = OnceLock::new();
    S.get_or_init(|| Store::load(&std::fs::read(resource_path()).unwrap()).unwrap())
}

fn c(code: &str) -> usize {
    match store().lookup(code) {
        Lookup::Country(i) => i,
        Lookup::NoMap => panic!("{code}"),
    }
}

const P: Pane = Pane::GOLDEN;

fn at(code: &str, lon: f64, lat: f64, scale: f64) -> View {
    let ct = &store().countries[c(code)];
    let [x, y] = Laea::new(ct.lat0, ct.lon0).fwd(lon, lat).unwrap();
    View {
        centre: [x, y],
        scale,
    }
}

/// All 248 countries against `golden-fit.tsv`: centre, bbox, fit, initial scale, level and
/// flags, each to the table's printed precision (half a unit in its last decimal). Fails on any
/// rule change, a wrongly rebuilt resource, or a padding slip.
#[test]
fn golden_fit_table() {
    let s = store();
    let t = include_str!("../fixtures/golden-fit.tsv");
    let mut n = 0;
    for line in t.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        let num = |i: usize| f[i].parse::<f64>().unwrap();
        let i = c(f[0]);
        let ct = &s.countries[i];
        assert_eq!(
            String::from_utf8_lossy(&s.units[usize::from(ct.units[0])].a3),
            f[1],
            "{}",
            f[0]
        );
        assert!(
            (ct.lat0 - num(3)).abs() <= 5e-11 && (ct.lon0 - num(4)).abs() <= 5e-11,
            "{} centre",
            f[0]
        );
        for (k, v) in ct.bbox_km.iter().enumerate() {
            assert!((v - num(5 + k)).abs() <= 5e-7, "{} bbox", f[0]);
        }
        let fit = s.fit_scale(i, &P).unwrap();
        assert!(
            (fit - num(9)).abs() <= 5e-7,
            "{} fit {fit} vs {}",
            f[0],
            f[9]
        );
        assert!(
            (rules::initial_scale(fit) - num(10)).abs() <= 5e-7,
            "{}",
            f[0]
        );
        assert_eq!(rules::level_for(fit).to_string(), f[11], "{} level", f[0]);
        let flag = |b: bool| if b { "1" } else { "0" };
        assert_eq!(
            (flag(ct.subdivisions), flag(ct.overridden), flag(ct.alias)),
            (f[12], f[13], f[14]),
            "{} flags",
            f[0]
        );
        assert_eq!(ct.insets.len().to_string(), f[15], "{} insets", f[0]);
        n += 1;
    }
    assert_eq!(n, 248);
    assert_eq!(s.countries.len(), 248);
}

/// Step 0's fits on the resource: 237 codes within 0.05 % or the fixture's rounding, and the
/// three listed differences — MY's override (2.326), AQ pole-centred, MM's subdivisions at 8.008.
/// Fails if the centre or the grouping diverges from the verified prototype.
#[test]
fn step0_reproduced() {
    let s = store();
    let mut n = 0;
    for line in include_str!("../fixtures/step0-fit.tsv").lines().skip(1) {
        let (code, fit) = line.split_once('\t').unwrap();
        let step0: f64 = fit.parse().unwrap();
        let i = c(code);
        let now = s.fit_scale(i, &P).unwrap();
        match code {
            "MY" => assert!((now - 2.3257).abs() < 1e-3 && s.countries[i].overridden),
            "AQ" => assert_eq!((s.countries[i].lat0, s.countries[i].lon0), (-90.0, 0.0)),
            _ => {
                assert!(
                    (now - step0).abs() <= (step0 * 5e-4).max(5e-5),
                    "{code}: {now} vs {step0}"
                );
                n += 1;
            }
        }
    }
    assert_eq!(n, 237);
    let mm = c("MM");
    let f = s.fit_scale(mm, &P).unwrap();
    assert!(s.countries[mm].subdivisions && f > 8.0 && f < 8.01, "{f}");
}

fn spans_the_seam(s: &Store, code: &str, v: &View, ring: &[[f32; 2]]) -> (bool, bool, bool) {
    let i = c(code);
    let lon_of = |p: &[f32; 2]| {
        s.unproject(i, &P, v, f64::from(p[0]), f64::from(p[1]))
            .map(|(lon, _)| lon)
    };
    let lons: Vec<f64> = ring.iter().filter_map(lon_of).collect();
    // a vertex on 180° comes back within a quantum of it (0.05 pt at 1.5 km/pt is 75 m, ~0.002°
    // at 67° N), so "on the seam" is within 0.01°, and a side is strictly off it
    let on = |p: &[f32; 2]| lon_of(p).is_some_and(|lon| (lon.abs() - 180.0).abs() < 0.01);
    let seam_edge = ring
        .iter()
        .zip(ring.iter().cycle().skip(1))
        .take(ring.len())
        .any(|(a, b)| on(a) && on(b));
    (
        lons.iter().any(|&l| l > 179.5 && l < 179.99),
        lons.iter().any(|&l| l < -179.5 && l > -179.99),
        seam_edge,
    )
}

fn ring_holding(f: &Frame, p: [f64; 2]) -> Vec<&Vec<[f32; 2]>> {
    f.land
        .iter()
        .flat_map(|s| s.rings.iter())
        .filter(|r| {
            let n = r.len();
            let mut odd = false;
            for k in 0..n {
                let (a, b) = (r[k], r[(k + 1) % n]);
                let (ax, ay, bx, by) = (
                    f64::from(a[0]),
                    f64::from(a[1]),
                    f64::from(b[0]),
                    f64::from(b[1]),
                );
                if (ay > p[1]) != (by > p[1]) && p[0] < ax + (p[1] - ay) / (by - ay) * (bx - ax) {
                    odd = !odd;
                }
            }
            odd
        })
        .collect()
}

/// FJ and RU fit as Step 0 measured them (a lon/lat bbox would give RU ~100 km/pt); at the floor
/// over Chukotka (67° N, 180°) and Taveuni (16.8° S, 180°) the point lies in exactly one land
/// ring, that ring has land strictly on both sides of 180°, and no land ring has an edge whose
/// ends both unproject to within 0.01° of 180° (the plan's 179.9999° is below the quantum: a
/// vertex on the seam comes back ~0.002° off). Fails with the seam unstitched.
#[test]
fn antimeridian_ru_fj() {
    let s = store();
    assert!((s.fit_scale(c("FJ"), &P).unwrap() - 1.9495).abs() < 1e-3);
    assert!((s.fit_scale(c("RU"), &P).unwrap() - 28.0107).abs() < 1e-3);
    for (code, lon, lat) in [("RU", 180.0, 67.0), ("FJ", 180.0, -16.8)] {
        let v = at(code, lon, lat, FLOOR_KM_PER_PT);
        let f = s.frame(c(code), &P, v).unwrap();
        let centre = s.project(c(code), &P, &f.view, lon, lat).unwrap();
        let hold = ring_holding(&f, centre);
        assert_eq!(hold.len(), 1, "{code}: {} rings hold the point", hold.len());
        let (east, west, edge) = spans_the_seam(s, code, &f.view, hold[0]);
        assert!(east && west, "{code}: the ring does not cross 180°");
        for r in f.land.iter().flat_map(|sh| sh.rings.iter()) {
            assert!(
                !spans_the_seam(s, code, &f.view, r).2,
                "{code}: a seam edge"
            );
        }
        let _ = edge;
    }
}

/// AQ is pole-centred; no stored vertex of its unit reaches −89.99°; every blob is within
/// 0.25 pt and its level-0 blob was simplified (bound > 0, and its top level, 3, holds under
/// half of level 0's vertices); Peter I Island's part is in the frame. Fails with R9 skipped (the run
/// cannot be simplified: bound 0 and the pole present) or the centre rule applied to AQ.
#[test]
fn antarctica() {
    let s = store();
    let i = c("AQ");
    let ct = &s.countries[i];
    assert_eq!((ct.lat0, ct.lon0), (-90.0, 0.0));
    let u = ct.units[0];
    let unit = &s.units[usize::from(u)];
    let l = Laea::new(unit.lat0, unit.lon0);
    let mut buf = Vec::new();
    let b0 = s.blob(u, 0, Layer::Land).unwrap();
    for r in 0..s.rings(b0).len() {
        s.decode(b0, r, &mut buf).unwrap();
        for &[x, y] in &buf {
            assert!(l.inv(x, y).unwrap().1 > -89.99);
        }
    }
    for k in 0..LADDER.len() {
        if let Some(b) = s.blob(u, k as u8, Layer::Land) {
            assert!(s.blobs[b].bound_pt <= 0.25);
        }
    }
    assert!(s.blobs[b0].bound_pt > 0.0);
    // AQ's top level at the golden pane is 3 (12 km/pt); since the band coverage (M4b commit 3)
    // the 140 pt floor's coarser fit reaches 24 km/pt, so a blob at 4 exists and is simplified
    // too — though not below level 3's count: at 24 km/pt more of AQ's rings fall back from RDP
    // to VW (7 789 vertices against 4 127 at 12; the hybrid's rule), as RU's mainland ring does
    let b3 = s.blob(u, 3, Layer::Land).unwrap();
    assert!(s.blobs[b3].vertices * 2 < s.blobs[b0].vertices);
    let b4 = s.blob(u, 4, Layer::Land).unwrap();
    assert!(s.blobs[b4].vertices < s.blobs[b0].vertices && s.blobs[b4].bound_pt > 0.0);
    // Peter I Island, 68.8° S 90.6° W: the part whose ring cap holds it is in the frame
    let peter = unit
        .parts
        .iter()
        .find(|p| {
            p.rings.first().is_some_and(|cap| {
                ondar_map::laea::haversine_km(f64::from(cap.lon), f64::from(cap.lat), -90.6, -68.8)
                    < 30.0
            })
        })
        .unwrap();
    assert_eq!(peter.role, Role::Frame);
}

/// Each of the 14 insets clears its country's land by ≥ 12 pt at the golden pane, its land fits
/// its box, and no two boxes overlap. Fails with a box moved onto the land.
#[test]
fn insets_clear_12pt() {
    let s = store();
    let mut n = 0;
    for (i, ct) in s.countries.iter().enumerate() {
        for (label, d) in s.inset_clearance(i, &P) {
            assert!(d >= 12.0, "{} {label}: {d:.1} pt", ct.name);
            n += 1;
        }
        if ct.insets.is_empty() {
            continue;
        }
        let f = s.frame(i, &P, s.fit(i, &P).unwrap()).unwrap();
        assert_eq!(f.insets.len(), ct.insets.len());
        for ins in &f.insets {
            let [x, y, w, h] = ins.rect;
            assert!(!ins.land.is_empty(), "{} {}: no land", ct.name, ins.label);
            for p in ins.land.iter().flat_map(|sh| sh.rings.iter()).flatten() {
                assert!(
                    p[0] >= x && p[0] <= x + w && p[1] >= y && p[1] <= y + h - 8.0 + 0.01,
                    "{}",
                    ins.label
                );
            }
        }
        for (a, ia) in ct.insets.iter().enumerate() {
            for ib in &ct.insets[a + 1..] {
                let ([ax, ay, aw, ah], [bx, by, bw, bh]) = (ia.rect, ib.rect);
                assert!(
                    ax + aw <= bx || bx + bw <= ax || ay + ah <= by || by + bh <= ay,
                    "{} {} / {}",
                    ct.name,
                    ia.label,
                    ib.label
                );
            }
        }
    }
    assert_eq!(n, 14);
    // insets at the fit view only (D7): zoomed in, none
    let pt = c("PT");
    assert!(
        s.frame(pt, &P, at("PT", -9.1393, 38.7223, FLOOR_KM_PER_PT))
            .unwrap()
            .insets
            .is_empty()
    );
    let fit = s.fit(pt, &P).unwrap();
    let nudged = View {
        centre: fit.centre,
        scale: fit.scale * 0.999,
    };
    assert!(s.frame(pt, &P, nudged).unwrap().insets.is_empty());
}

/// An inset box at a pane is the golden box at the band's stored scale, anchored at its corner
/// with the golden gaps (I1, M4b commit 5; the size no longer fixed, as review finding 2's form had
/// it): at the ANMITE's 328 × 178, at 400 × 360 (whose height reads the 300 entry) and at the
/// golden pane, every drawn box has the golden size times its stored scale, the gaps to its
/// corner's edges the golden pane's — except a box that abuts another (Hawaii beside Alaska): its
/// left edge is that box's right edge plus the golden 6 pt — its land inside it, and `drawn +
/// insets_dropped` is 14. The clearance is measured from the box where the frame draws it.
#[test]
fn insets_anchor_by_corner() {
    use ondar_map::format::Corner;
    let s = store();
    let g = P;
    for pane in [
        Pane::band(178),
        Pane {
            width: 400.0,
            height: 360.0,
            padding: 20.0,
        },
        P,
    ] {
        let mut n = 0;
        let mut dropped = 0;
        for (i, ct) in s.countries.iter().enumerate() {
            if ct.insets.is_empty() {
                continue;
            }
            let f = s.frame(i, &pane, s.fit(i, &pane).unwrap()).unwrap();
            dropped += f.stats.insets_dropped;
            // the clearance is measured from the box where the frame draws it
            let clearance = s.inset_clearance(i, &pane);
            assert_eq!(clearance.len(), f.insets.len(), "{}", ct.name);
            for (got, (label, d)) in f.insets.iter().zip(&clearance) {
                assert_eq!(&got.label, label);
                let [x, y, w, h] = got.rect.map(f64::from);
                let want = f
                    .land
                    .iter()
                    .flat_map(|sh| sh.rings.iter())
                    .map(|r| {
                        rules::rect_ring_distance(
                            [x, y, x + w, y + h],
                            r.iter().map(|p| p.map(f64::from)),
                        )
                    })
                    .fold(f64::INFINITY, f64::min);
                assert!((d - want).abs() < 1e-3, "{label}: clearance {d} vs {want}");
            }
            for got in &f.insets {
                let stored = ct.insets.iter().find(|i| i.label == got.label).unwrap();
                let [x, y, w, h] = got.rect.map(f64::from);
                let [gx, gy, gw, gh] = stored.golden();
                let sc = stored.scale_at(&s.header.bands, &pane);
                let what = format!(
                    "{} {} at {}×{}",
                    ct.name, got.label, pane.width, pane.height
                );
                assert!(
                    rules::box_fits([x, y, w, h], &pane),
                    "{what}: leaves the pane"
                );
                assert!(
                    (w - gw * sc).abs() < 1e-3 && (h - gh * sc).abs() < 1e-3,
                    "{what}: size {w} × {h}, golden {gw} × {gh} at {sc}"
                );
                let (left, top) = match stored.corner {
                    Corner::TopLeft => (true, true),
                    Corner::TopRight => (false, true),
                    Corner::BottomLeft => (true, false),
                    Corner::BottomRight => (false, false),
                };
                let gap_x = |x: f64, w: f64, pw: f64| if left { x } else { pw - (x + w) };
                let gap_y = |y: f64, h: f64, ph: f64| if top { y } else { ph - (y + h) };
                let beside = ct
                    .insets
                    .iter()
                    .take_while(|a| a.label != got.label)
                    .find(|a| {
                        a.corner == stored.corner
                            && rules::abuts(stored.golden(), a.golden(), stored.corner).is_some()
                    });
                let anchored_x = (gap_x(x, w, pane.width) - gap_x(gx, gw, g.width)).abs() < 1e-3;
                let anchored_y = (gap_y(y, h, pane.height) - gap_y(gy, gh, g.height)).abs() < 1e-3;
                match beside.and_then(|a| {
                    f.insets
                        .iter()
                        .find(|d| d.label == a.label)
                        .map(|d| (d, rules::abuts(stored.golden(), a.golden(), stored.corner)))
                }) {
                    // the stacking rule: Hawaii's left edge is Alaska's right edge + 6 (beside),
                    // Madeira's top the Azores' bottom + 8 (stacked, top-left)
                    Some((a, Some(rules::Abut::Beside { gap }))) => {
                        let [ax, _, aw, _] = a.rect.map(f64::from);
                        assert!(
                            (x - (ax + aw + gap)).abs() < 1e-3 && anchored_y,
                            "{what}: beside {}",
                            a.label
                        );
                    }
                    Some((a, Some(rules::Abut::Stacked { gap }))) => {
                        let [_, ay, _, ah] = a.rect.map(f64::from);
                        assert!(
                            (y - (ay + ah + gap)).abs() < 1e-3 && anchored_x,
                            "{what}: under {}",
                            a.label
                        );
                    }
                    _ => assert!(
                        anchored_x && anchored_y,
                        "{what}: [{x}, {y}] is not anchored to {:?}",
                        stored.corner
                    ),
                }
                assert!(!got.land.is_empty(), "{what}: no land");
                for p in got.land.iter().flat_map(|sh| sh.rings.iter()).flatten() {
                    let [px, py] = p.map(f64::from);
                    assert!(
                        px >= x - 0.01
                            && px <= x + w + 0.01
                            && py >= y - 0.01
                            && py <= y + h + 0.01,
                        "{what}: land outside the box"
                    );
                }
                n += 1;
            }
        }
        assert_eq!(n + dropped, 14, "{}×{}", pane.width, pane.height);
        if pane.height >= 300.0 {
            assert_eq!(n, 14, "{}×{}: every box whole", pane.width, pane.height);
        } else {
            assert_eq!(dropped, 1, "Hawaii at 178");
        }
    }
}

/// S4: in Réunion's own frame, France's copy of the island (the parent's identical part) is not
/// drawn as a neighbour: no neighbour ring's vertex mean lies inside Réunion's land (a first
/// vertex would sit on the shared coast). Fails with the omit-in ignored.
#[test]
fn an_alias_frame_omits_the_parents_copy() {
    let s = store();
    let re = c("RE");
    let f = s.frame(re, &P, s.fit(re, &P).unwrap()).unwrap();
    assert!(!f.land.is_empty());
    for r in f.neighbours.iter().flat_map(|sh| sh.rings.iter()) {
        let n = r.len() as f64;
        let mx = r.iter().map(|p| f64::from(p[0])).sum::<f64>() / n;
        let my = r.iter().map(|p| f64::from(p[1])).sum::<f64>() / n;
        assert!(
            ring_holding(&f, [mx, my]).is_empty(),
            "a neighbour ring inside Réunion's land"
        );
    }
}

/// R7: uppercase before lookup; `XX`, unknown and empty codes are no map; the S4 aliases and
/// Taiwan (`ISO_A2_EH`) resolve.
#[test]
fn no_map() {
    let s = store();
    for code in ["XX", "xx", "zz", "", "P", "PRT"] {
        assert_eq!(s.lookup(code), Lookup::NoMap, "{code}");
    }
    assert_eq!(s.lookup("pt"), s.lookup("PT"));
    assert_eq!(s.countries[c("PT")].name, "Portugal");
    let re = &s.countries[c("RE")];
    assert!(re.alias);
    assert_eq!(&s.units[usize::from(re.units[0])].a3, b"REU");
    assert_eq!(
        &s.units[usize::from(s.countries[c("TW")].units[0])].a3,
        b"TWN"
    );
}

/// Subdivisions above 8 km/pt only, for flagged countries: the US at 8.001 has them and at 7.999
/// none; MM at its fit has them; DE never. Fails on `<` for `≤`, a flag computed at runtime, or
/// the rule inverted.
#[test]
fn subdivisions_by_scale() {
    let s = store();
    let us_fit = s.fit(c("US"), &P).unwrap();
    let us = |scale: f64| {
        s.frame(
            c("US"),
            &P,
            View {
                centre: us_fit.centre,
                scale,
            },
        )
        .unwrap()
    };
    assert!(!us(8.001).subdivisions.is_empty());
    assert!(us(7.999).subdivisions.is_empty());
    assert!(us(8.0).subdivisions.is_empty());
    let mm = c("MM");
    assert!(
        !s.frame(mm, &P, s.fit(mm, &P).unwrap())
            .unwrap()
            .subdivisions
            .is_empty()
    );
    let de = c("DE");
    assert!(!s.countries[de].subdivisions);
    assert!(
        s.frame(de, &P, s.fit(de, &P).unwrap())
            .unwrap()
            .subdivisions
            .is_empty()
    );
    assert_eq!(us(12.0).level, 12.0);
    assert_eq!(us(11.99).level, 6.0);
}

/// An enclave's border is a closed loop on screen (review finding 1): the ACT inside NSW and
/// Brazil's Distrito Federal inside Goiás at every subdivision level the country's zoom reaches,
/// and Moscow's exclave Zelenograd at 8.5 km/pt (level 6; from level 12 it is under a point
/// across and simplifies away) — each one polyline whose last point is its first and which holds
/// the enclave. Moscow itself is not a case: in NE v5.1.2 it borders Kaluga Oblast, so its border
/// is open lines between junctions. Fails if the tool stores a closed line as a ring (the closing
/// vertex dropped), which leaves the loop open by one edge.
#[test]
fn enclave_borders_are_closed() {
    let s = store();
    let every = [8.5, 12.5, 24.5, f64::INFINITY];
    for (code, name, lon, lat, scales) in [
        ("AU", "ACT", 149.0, -35.5, &every[..]),
        ("BR", "Distrito Federal", -47.8, -15.8, &every[..]),
        ("RU", "Zelenograd", 37.19, 55.99, &[8.5][..]),
    ] {
        let i = c(code);
        let fit = s.fit(i, &P).unwrap().scale;
        for &scale in scales {
            let scale = scale.min(fit);
            let f = s.frame(i, &P, at(code, lon, lat, scale)).unwrap();
            assert!(!f.subdivisions.is_empty(), "{code} at {scale}");
            let [px, py] = s.project(i, &P, &f.view, lon, lat).unwrap();
            let holds = |l: &Vec<[f32; 2]>| {
                l.len() >= 4
                    && l.first() == l.last()
                    && l.windows(2).fold(false, |odd, w| {
                        let ([ax, ay], [bx, by]) = (w[0].map(f64::from), w[1].map(f64::from));
                        odd != ((ay > py) != (by > py)
                            && px < ax + (py - ay) / (by - ay) * (bx - ax))
                    })
            };
            assert!(
                f.subdivisions.iter().any(holds),
                "{code} {name} at {scale} km/pt (level {}): no closed loop holds it",
                f.level
            );
        }
    }
}

/// D6: past the fit → the fit; below 1.5 → 1.5; at the widest scale the view is the fit wherever
/// its centre was asked to be; zoomed in, the centre stays where the view's edge meets the fit
/// rectangle's; a non-finite view is the fit. Fails if any limit is dropped or the plan's reading
/// (the centre anywhere in the fit rectangle) returns.
#[test]
fn clamp_limits() {
    let s = store();
    let i = c("PT");
    let fit = s.fit(i, &P).unwrap();
    let far = [fit.centre[0] + 5000.0, fit.centre[1] - 5000.0];
    assert_eq!(
        s.clamp_view(
            i,
            &P,
            View {
                centre: fit.centre,
                scale: fit.scale * 3.0
            }
        )
        .unwrap(),
        fit
    );
    assert_eq!(
        s.clamp_view(
            i,
            &P,
            View {
                centre: far,
                scale: fit.scale
            }
        )
        .unwrap(),
        fit
    );
    let floor = s
        .clamp_view(
            i,
            &P,
            View {
                centre: fit.centre,
                scale: 0.5,
            },
        )
        .unwrap();
    assert_eq!(floor.scale, FLOOR_KM_PER_PT);
    let z = s
        .clamp_view(
            i,
            &P,
            View {
                centre: far,
                scale: 1.5,
            },
        )
        .unwrap();
    let (rx, ry) = (
        P.width / 2.0 * (fit.scale - 1.5),
        P.height / 2.0 * (fit.scale - 1.5),
    );
    assert!((z.centre[0] - (fit.centre[0] + rx)).abs() < 1e-9);
    assert!((z.centre[1] - (fit.centre[1] - ry)).abs() < 1e-9);
    // the view's right edge is the fit rectangle's
    assert!(
        ((z.centre[0] + P.width / 2.0 * 1.5) - (fit.centre[0] + P.width / 2.0 * fit.scale)).abs()
            < 1e-9
    );
    assert_eq!(
        s.clamp_view(
            i,
            &P,
            View {
                centre: [f64::NAN, 0.0],
                scale: 2.0
            }
        )
        .unwrap(),
        fit
    );
    // a country finer than the floor: only the fit, at 1.5
    let va = c("VA");
    let vf = s.fit(va, &P).unwrap();
    assert_eq!(vf.scale, 1.5);
    assert_eq!(
        s.clamp_view(
            va,
            &P,
            View {
                centre: far,
                scale: 1.0
            }
        )
        .unwrap(),
        vf
    );
}

/// The index is exact: with it disabled, PT, US and RU at the fit, the floor and mid zoom give
/// the same frame. Fails on a cap computed without the tolerance (a ring wrongly skipped).
#[test]
fn index_is_exact() {
    let s = store();
    for (code, lon, lat) in [
        ("PT", -9.1393, 38.7223),
        ("US", -74.006, 40.7128),
        ("RU", 131.8855, 43.1155),
    ] {
        let i = c(code);
        let fit = s.fit(i, &P).unwrap();
        for v in [
            fit,
            at(code, lon, lat, FLOOR_KM_PER_PT),
            at(code, lon, lat, (fit.scale * 1.5).sqrt()),
        ] {
            let a = s.frame(i, &P, v).unwrap();
            let b = s.frame_unindexed(i, &P, v).unwrap();
            assert_eq!(
                (&a.land, &a.neighbours, &a.subdivisions),
                (&b.land, &b.neighbours, &b.subdivisions),
                "{code} {v:?}"
            );
            assert!(a.stats.rings_considered < b.stats.rings_considered);
            assert_eq!(a.stats.missing_blobs, 0);
        }
    }
}

/// A pane that is not a pane — a negative or zero side, a negative padding, a non-finite field —
/// frames nothing and never panics (review finding 4): `fit_scale`, `fit`, `clamp_view`, `frame`,
/// `inset_clearance`, `project` and `unproject` (review 2, finding 6) answer `None` / empty. The finding's pane, −10 × 300 with −20 padding, has a
/// positive usable area (30 × 340), so on `dddb4da` `fit_scale` accepted it and `clamp` panicked
/// in `f64::clamp` (min > max); the release profile aborts on a panic.
#[test]
fn a_bad_pane_is_none_not_a_panic() {
    let s = store();
    let pt = c("PT");
    let view = s.fit(pt, &P).unwrap();
    let pane = |width: f64, height: f64, padding: f64| Pane {
        width,
        height,
        padding,
    };
    for bad in [
        pane(-10.0, 300.0, -20.0),
        pane(328.0, -10.0, -200.0),
        pane(0.0, 300.0, -20.0),
        pane(328.0, 300.0, -20.0),
        pane(328.0, 300.0, f64::NAN),
        pane(f64::INFINITY, 300.0, 20.0),
        pane(328.0, f64::NAN, 20.0),
    ] {
        assert_eq!(s.clamp_view(pt, &bad, view), None, "{bad:?}");
        assert_eq!(s.fit_scale(pt, &bad), None, "{bad:?}");
        assert_eq!(s.fit(pt, &bad), None, "{bad:?}");
        assert!(s.frame(pt, &bad, view).is_none(), "{bad:?}");
        assert!(s.inset_clearance(pt, &bad).is_empty(), "{bad:?}");
        // the two M4b reads with: a pane that is not a pane projects nothing (review 2, finding 6;
        // on `8324e68` `project` gave `Some([-54.09, 191.74])` at −10 × 300)
        assert_eq!(s.project(pt, &bad, &view, -9.14, 38.72), None, "{bad:?}");
        assert_eq!(s.unproject(pt, &bad, &view, 164.0, 150.0), None, "{bad:?}");
    }
    // a pane with no padding is a pane
    assert!(s.frame(pt, &pane(328.0, 178.0, 0.0), view).is_some());
}

/// A view that is not a view projects nothing (review 3, finding 1): a scale of 0, negative, NaN
/// or ∞, or a centre not finite, gives `None` from `project` and `unproject`, as a bad pane does.
/// On `b3cf735` a scale of 0 gave `Some([inf, -inf])`.
#[test]
fn a_bad_view_is_none() {
    let s = store();
    let pt = c("PT");
    let fit = s.fit(pt, &P).unwrap();
    let with = |centre: [f64; 2], scale: f64| View { centre, scale };
    for bad in [
        with(fit.centre, 0.0),
        with(fit.centre, -fit.scale),
        with(fit.centre, f64::NAN),
        with(fit.centre, f64::INFINITY),
        with([f64::NAN, fit.centre[1]], fit.scale),
        with([fit.centre[0], f64::INFINITY], fit.scale),
    ] {
        assert_eq!(s.project(pt, &P, &bad, -9.14, 38.72), None, "{bad:?}");
        assert_eq!(s.unproject(pt, &P, &bad, 164.0, 150.0), None, "{bad:?}");
    }
    // the fit itself still projects
    assert!(s.project(pt, &P, &fit, -9.14, 38.72).is_some());
    assert!(s.unproject(pt, &P, &fit, 164.0, 150.0).is_some());
}

/// A view a hair finer than the fit is the fit (review 3, finding 3): `clamp_view` snaps a scale
/// at or above `top × (1 − 1e-6)` to the fit, so the frame there is the fit's — insets drawn,
/// the remote groups not drawn as land. On `b3cf735` a view at `top × (1 − 1e-12)` lost the US's
/// two insets. At `top × 0.99` the view is not the fit and draws no inset.
#[test]
fn a_view_a_hair_below_the_fit_is_the_fit() {
    let s = store();
    for code in ["US", "PT", "FR"] {
        let i = c(code);
        let fit = s.fit(i, &P).unwrap();
        let at_fit = s.frame(i, &P, fit).unwrap();
        assert!(!at_fit.insets.is_empty(), "{code}");
        // panned 1 km too: the snap is to the fit, centre and all
        let near = View {
            centre: [fit.centre[0] + 1.0, fit.centre[1] - 1.0],
            scale: fit.scale * (1.0 - 1e-12),
        };
        assert_eq!(s.clamp_view(i, &P, near), Some(fit), "{code}");
        assert_eq!(s.frame(i, &P, near).unwrap(), at_fit, "{code}");
        let zoomed = View {
            scale: fit.scale * 0.99,
            ..fit
        };
        assert_ne!(s.clamp_view(i, &P, zoomed), Some(fit), "{code}");
        assert!(s.frame(i, &P, zoomed).unwrap().insets.is_empty(), "{code}");
    }
}

/// A box that leaves the pane blocks nothing (review 3, finding 2), on the rebuilt resource:
/// France's Fr. Guiana box moved to y −10 (off the pane at every band) and Réunion's moved to the
/// top-left on top of it, inside the pane — at the golden pane Réunion and the Antilles are drawn,
/// Fr. Guiana is the one dropped. On `b3cf735` a box not drawn still took part in the overlap
/// test and dropped the box over it.
#[test]
fn an_off_pane_box_does_not_drop_an_inset() {
    use ondar_map::format::Corner;
    let mut s = Store::load(&std::fs::read(resource_path()).unwrap()).unwrap();
    let fr = c("FR");
    let at = |s: &Store, label: &str| {
        s.countries[fr]
            .insets
            .iter()
            .position(|i| i.label == label)
            .unwrap()
    };
    let (fg, re) = (at(&s, "Fr. Guiana"), at(&s, "Réunion"));
    assert!(fg < re, "table order: the off-pane box first");
    s.countries[fr].insets[fg].rect = [8.0, -10.0, 60.0, 44.0];
    s.countries[fr].insets[re].corner = Corner::TopLeft;
    s.countries[fr].insets[re].rect = [8.0, 20.0, 60.0, 44.0];
    let boxes = s.inset_boxes(fr, &P);
    // the premise: Fr. Guiana's box would overlap Réunion's, and leaves the pane
    assert!(!rules::boxes_apart(
        [8.0, -10.0, 60.0, 44.0],
        [8.0, 20.0, 60.0, 44.0]
    ));
    assert_eq!(boxes[fg], None);
    assert_eq!(boxes[re], Some([8.0, 20.0, 60.0, 44.0]));
    let f = s.frame(fr, &P, s.fit(fr, &P).unwrap()).unwrap();
    let labels: Vec<&str> = f.insets.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["Antilles", "Réunion"]);
    assert_eq!(f.stats.insets_dropped, 1);
}

/// A country's own small island groups in the padding band are drawn (review finding 5). At the
/// golden pane's fit, every vertex of an own `Dropped` part (< 1 000 km², outside the usable
/// area, not an inset) that falls inside the pane is a vertex of a land ring — as it already was
/// when another country's frame drew the same part as a neighbour. 20 parts in 6 countries: the
/// South Orkneys (AQ), Lord Howe (AU), Trindade and Fernando de Noronha (BR), San Andrés (CO),
/// the Bonin Islands (JP), PF. On `dddb4da` the frame skipped an own
/// unit's `Dropped` parts, so zoomed and panned onto one the pane showed empty sea.
#[test]
fn own_islets_in_the_padding_are_drawn() {
    let s = store();
    let mut buf = Vec::new();
    let (mut parts, mut countries) = (0, std::collections::BTreeSet::new());
    for (i, ct) in s.countries.iter().enumerate() {
        let v = s.fit(i, &P).unwrap();
        let f = s.frame(i, &P, v).unwrap();
        let k = u8::try_from(rules::level_for(v.scale)).unwrap();
        let land: Vec<[f32; 2]> = f
            .land
            .iter()
            .flat_map(|sh| sh.rings.iter())
            .flatten()
            .copied()
            .collect();
        for &u in &ct.units {
            let unit = &s.units[usize::from(u)];
            let ul = Laea::new(unit.lat0, unit.lon0);
            let Some(b) = s.blob(u, k, Layer::Land) else {
                continue;
            };
            let mut ri = 0;
            for part in &unit.parts {
                let first = ri;
                ri += part.rings.len();
                if part.role != Role::Dropped {
                    continue;
                }
                let mut seen = false;
                for j in 0..part.rings.len() {
                    s.decode(b, first + j, &mut buf).unwrap();
                    // a ring the quanta collapsed below three vertices is no shape to draw, own
                    // or neighbour (the Coral Sea Islands' at AU's fit level: one vertex)
                    if buf.len() < 3 {
                        continue;
                    }
                    for &[x, y] in &buf {
                        let (lon, lat) = ul.inv(x, y).unwrap();
                        let [px, py] = s.project(i, &P, &f.view, lon, lat).unwrap();
                        if !(0.0..=P.width).contains(&px) || !(0.0..=P.height).contains(&py) {
                            continue;
                        }
                        seen = true;
                        assert!(
                            land.iter()
                                .any(|&[lx, ly]| (f64::from(lx) - px).abs() < 0.02
                                    && (f64::from(ly) - py).abs() < 0.02),
                            "{} {}: an own islet's vertex at ({px:.2}, {py:.2}) is not drawn",
                            ct.name,
                            String::from_utf8_lossy(&unit.a3)
                        );
                    }
                }
                if seen {
                    parts += 1;
                    countries.insert(ct.name.clone());
                }
            }
        }
    }
    assert_eq!((parts, countries.len()), (20, 6), "{countries:?}");
}

/// The in-pane vertices of a country's own parts of `role` at a view, from the frame's level
/// (rings the quanta collapse below three vertices are no shape and are skipped), each with the
/// unit's code.
fn own_vertices_in_pane(
    s: &Store,
    i: usize,
    pane: &Pane,
    view: &View,
    level: f64,
    role: impl Fn(Role) -> bool,
) -> Vec<(String, [f64; 2])> {
    let ct = &s.countries[i];
    let k = u8::try_from(LADDER.iter().position(|&l| l == level).unwrap()).unwrap();
    let mut buf = Vec::new();
    let mut out = Vec::new();
    for &u in &ct.units {
        let unit = &s.units[usize::from(u)];
        let ul = Laea::new(unit.lat0, unit.lon0);
        let Some(b) = s.blob(u, k, Layer::Land) else {
            continue;
        };
        let mut ri = 0;
        for part in &unit.parts {
            let first = ri;
            ri += part.rings.len();
            if !role(part.role) {
                continue;
            }
            for j in 0..part.rings.len() {
                s.decode(b, first + j, &mut buf).unwrap();
                if buf.len() < 3 {
                    continue;
                }
                for &[x, y] in &buf {
                    let (lon, lat) = ul.inv(x, y).unwrap();
                    let [px, py] = s.project(i, pane, view, lon, lat).unwrap();
                    if (0.0..=pane.width).contains(&px) && (0.0..=pane.height).contains(&py) {
                        out.push((String::from_utf8_lossy(&unit.a3).into_owned(), [px, py]));
                    }
                }
            }
        }
    }
    out
}

fn is_land_vertex(f: &Frame, [px, py]: [f64; 2]) -> bool {
    f.land
        .iter()
        .flat_map(|sh| sh.rings.iter())
        .flatten()
        .any(|&[lx, ly]| (f64::from(lx) - px).abs() < 0.02 && (f64::from(ly) - py).abs() < 0.02)
}

/// A country's own inset groups are land in its main frame whenever the view is not the fit —
/// their box is not on screen then (D7) — and in their box only at the fit (review 2, finding 1).
/// For every country with insets, at the fit nudged in by 0.1 % and at the floor centred on each
/// inset's centre (clamped into the fit rectangle), every in-pane vertex of an own `Inset` part
/// is a vertex of a land ring; at the fit, none is. The finding's four must be among those seen:
/// India's Andaman & Nicobar, Yemen's Socotra, the Aleutians (Alaska's group), the Marquesas.
/// On `8324e68` the frame skipped an own unit's `Inset` parts at every view, so zoomed onto the
/// Andamans the pane showed empty sea while Myanmar's frame drew them as a neighbour.
#[test]
fn own_insets_are_land_when_the_view_is_not_the_fit() {
    let s = store();
    let mut seen = std::collections::BTreeSet::new();
    for (i, ct) in s.countries.iter().enumerate() {
        if ct.insets.is_empty() {
            continue;
        }
        let code = String::from_utf8_lossy(&ct.code).into_owned();
        let fit = s.fit(i, &P).unwrap();
        let mut views = vec![View {
            centre: fit.centre,
            scale: fit.scale * 0.999,
        }];
        for ins in &ct.insets {
            views.push(at(&code, ins.lon0, ins.lat0, FLOOR_KM_PER_PT));
        }
        let is_inset = |r: Role| matches!(r, Role::Inset(_));
        for v in views {
            let f = s.frame(i, &P, v).unwrap();
            assert_ne!(f.view, fit, "{code}");
            for (a3, p) in own_vertices_in_pane(s, i, &P, &f.view, f.level, is_inset) {
                assert!(
                    is_land_vertex(&f, p),
                    "{code} {a3}: an own inset vertex at ({:.2}, {:.2}) is not land at {:?}",
                    p[0],
                    p[1],
                    f.view
                );
                seen.insert(code.clone());
            }
        }
        let f = s.frame(i, &P, fit).unwrap();
        for (a3, p) in own_vertices_in_pane(s, i, &P, &fit, f.level, is_inset) {
            assert!(
                !is_land_vertex(&f, p),
                "{code} {a3}: an own inset vertex at ({:.2}, {:.2}) is land at the fit",
                p[0],
                p[1]
            );
        }
    }
    for code in ["IN", "YE", "US", "PF"] {
        assert!(seen.contains(code), "{code} not seen: {seen:?}");
    }
}

/// An inset is drawn iff its band's stored scale is above 0 and its box — at that scale, anchored
/// by its corner or beside the box it abuts — is inside the pane, apart from the controls' rect
/// and from every box drawn before it in table order (I1 + C1, M4b commit 5; review 2 finding 4
/// and review 3 finding 2's rule kept): `Store::inset_boxes` is that rule, and the frame draws
/// exactly its `Some`s. At 328 × 60 the scales are the 140 pt floor's (the height clamps) and most
/// boxes leave the pane; at 178 all but Hawaii are drawn. Every inset not drawn is counted.
#[test]
fn an_inset_that_does_not_fit_the_pane_is_not_drawn() {
    let s = store();
    for height in [60.0, 178.0] {
        let pane = Pane {
            width: 328.0,
            height,
            padding: 20.0,
        };
        let controls = rules::controls_rect(&pane);
        let mut drawn = Vec::new();
        let mut dropped = 0;
        for (i, ct) in s.countries.iter().enumerate() {
            if ct.insets.is_empty() {
                continue;
            }
            let boxes = s.inset_boxes(i, &pane);
            let f = s.frame(i, &pane, s.fit(i, &pane).unwrap()).unwrap();
            let mut before: Vec<[f64; 4]> = Vec::new();
            for (k, ins) in ct.insets.iter().enumerate() {
                let is_drawn = f.insets.iter().any(|d| d.label == ins.label);
                assert_eq!(
                    is_drawn,
                    boxes[k].is_some(),
                    "{} {} at 328×{height}",
                    ct.name,
                    ins.label
                );
                if let Some(rect) = boxes[k] {
                    let what = format!("{} {} at 328×{height}", ct.name, ins.label);
                    assert!(rules::box_fits(rect, &pane), "{what}: leaves the pane");
                    assert!(
                        rules::boxes_apart(rect, controls),
                        "{what}: on the controls"
                    );
                    assert!(
                        before.iter().all(|&o| rules::boxes_apart(rect, o)),
                        "{what}: overlaps"
                    );
                    assert!(
                        ins.scale_at(&s.header.bands, &pane) > 0.0,
                        "{what}: scale 0"
                    );
                    let got = f.insets.iter().find(|d| d.label == ins.label).unwrap();
                    let r = got.rect.map(f64::from);
                    assert!(
                        (0..4).all(|k| (r[k] - rect[k]).abs() < 1e-3),
                        "{what}: {r:?} vs {rect:?}"
                    );
                    before.push(rect);
                    drawn.push(ins.label.clone());
                }
            }
            assert_eq!(
                f.insets.len() + f.stats.insets_dropped,
                ct.insets.len(),
                "{} at 328×{height}",
                ct.name
            );
            dropped += f.stats.insets_dropped;
            let labels: Vec<String> = s
                .inset_clearance(i, &pane)
                .into_iter()
                .map(|(l, _)| l)
                .collect();
            let want: Vec<String> = f.insets.iter().map(|d| d.label.clone()).collect();
            assert_eq!(labels, want, "{} at 328×{height}", ct.name);
        }
        assert_eq!(drawn.len() + dropped, 14, "328×{height}");
        if height == 178.0 {
            assert_eq!(dropped, 1, "328×178: Hawaii alone, {drawn:?}");
        } else {
            eprintln!("drawn at 328×{height}: {drawn:?}");
            assert!(dropped >= 10, "328×60: {drawn:?}");
        }
    }
}

/// Svalbard's clearance read 61.1 pt at the golden pane and 1.97 at 328 × 178 (Step 0, M4b). The
/// cause, measured on the resource rather than inferred: at NO's fit at 178 the land ring nearest
/// Svalbard's full-size golden box at the top-left is Jan Mayen (71.0° N, 8.5° W) — an own
/// `Dropped` group, drawn as land since `ff9a75a`, which the shorter pane's coarser fit brings on
/// screen — and with that one ring excluded the box clears the rest of the land by ≥ 40 pt. Since
/// I1 (commit 5) the drawn box is the 78 % one, which clears Jan Mayen by ≥ 12. Fails if the
/// nearest ring is the mainland or Bear Island.
#[test]
fn svalbard_clearance_at_178_is_jan_mayen() {
    use ondar_map::laea::haversine_km;
    let s = store();
    let no = c("NO");
    let pane = Pane::band(178);
    let fit = s.fit(no, &pane).unwrap();
    let f = s.frame(no, &pane, fit).unwrap();
    let stored = s.countries[no]
        .insets
        .iter()
        .find(|i| i.label == "Svalbard")
        .unwrap();
    let [x, y, w, h] = rules::inset_box_at(stored.golden(), stored.corner, &pane, 1.0);
    let rect = [x, y, x + w, y + h];
    let rings: Vec<&Vec<[f32; 2]>> = f.land.iter().flat_map(|sh| sh.rings.iter()).collect();
    let dist = |r: &[[f32; 2]]| rules::rect_ring_distance(rect, r.iter().map(|p| p.map(f64::from)));
    let (nearest, d_near) = rings
        .iter()
        .enumerate()
        .map(|(i, r)| (i, dist(r)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert!(d_near < 12.0, "the full box clears at 178: {d_near:.2} pt");
    let r = rings[nearest];
    let n = r.len() as f64;
    let (mx, my) = r.iter().fold((0.0, 0.0), |(sx, sy), p| {
        (sx + f64::from(p[0]), sy + f64::from(p[1]))
    });
    let (lon, lat) = s.unproject(no, &pane, &f.view, mx / n, my / n).unwrap();
    let km = haversine_km(lon, lat, -8.5, 71.0);
    assert!(
        km <= 100.0,
        "the nearest ring ({} vertices, {d_near:.2} pt) is at {lat:.2}° N {lon:.2}° E, {km:.0} km from Jan Mayen",
        r.len()
    );
    let rest = rings
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != nearest)
        .map(|(_, r)| dist(r))
        .fold(f64::INFINITY, f64::min);
    assert!(
        rest >= 40.0,
        "with Jan Mayen excluded the full box clears {rest:.2} pt"
    );
    // the drawn box: 78 % at 178, ≥ 12 pt from everything
    let drawn = f.insets.iter().find(|i| i.label == "Svalbard").unwrap();
    assert!(
        (f64::from(drawn.rect[2]) - 80.0 * 0.78).abs() < 1e-3,
        "{:?}",
        drawn.rect
    );
    let (_, reported) = s
        .inset_clearance(no, &pane)
        .into_iter()
        .find(|(l, _)| l == "Svalbard")
        .unwrap();
    assert!(reported >= 12.0, "{reported}");
    eprintln!(
        "svalbard at 328×178: the full box's nearest ring {} vertices at {d_near:.2} pt, ({lat:.3}, {lon:.3}), {km:.1} km from Jan Mayen; the rest ≥ {rest:.2} pt; drawn at 78 %, {reported:.2} pt",
        r.len()
    );
}

/// The frame clips to `index::clip_rect`, the pane grown by 2 pt on every side (review 3,
/// finding 5): at the golden fit the neighbours of RU, FR, DE and NO cross all four edges, so
/// the clipped vertices' extremes are exactly −2 and 330 in x and −2 and 302 in y (Sutherland–
/// Hodgman puts a vertex on the clip edge), and no vertex lies outside; at 328 × 178 DE's and
/// NO's reach −2, 330, −2 and 180. Fails with the margin dropped from the frame (0 and 328) or
/// applied in km at the view's scale.
#[test]
fn the_frame_clips_to_the_margin() {
    use ondar_map::index::{CLIP_MARGIN_PT, clip_rect};
    let s = store();
    let anmite = Pane {
        width: 328.0,
        height: 178.0,
        padding: 20.0,
    };
    for (pane, codes) in [
        (P, vec!["RU", "FR", "DE", "NO"]),
        (anmite, vec!["DE", "NO"]),
    ] {
        let [x0, y0, x1, y1] = clip_rect(&pane);
        assert_eq!(x0, -CLIP_MARGIN_PT);
        assert_eq!((x1, y1), (pane.width + 2.0, pane.height + 2.0));
        for code in codes {
            let i = c(code);
            let f = s.frame(i, &pane, s.fit(i, &pane).unwrap()).unwrap();
            let mut lo = [f64::INFINITY; 2];
            let mut hi = [f64::NEG_INFINITY; 2];
            for p in f.neighbours.iter().flat_map(|sh| sh.rings.iter()).flatten() {
                for k in 0..2 {
                    lo[k] = lo[k].min(f64::from(p[k]));
                    hi[k] = hi[k].max(f64::from(p[k]));
                }
            }
            assert_eq!(
                (lo, hi),
                ([x0, y0], [x1, y1]),
                "{code} at {}×{}: the neighbours' extent",
                pane.width,
                pane.height
            );
            for p in f
                .land
                .iter()
                .chain(&f.neighbours)
                .flat_map(|sh| sh.rings.iter())
                .chain(&f.subdivisions)
                .flatten()
            {
                let [x, y] = p.map(f64::from);
                assert!(
                    x >= x0 && x <= x1 && y >= y0 && y <= y1,
                    "{code}: ({x}, {y}) outside the clip rect"
                );
            }
        }
    }
}

/// The insets' per-band scales as the frame reads them (I1, M4b commit 5; what commit 4's tables
/// decided and the STOP accepted): the US at 328 × 178 draws Alaska at 83 % (69.72 × 46.48) and
/// not Hawaii (`insets_dropped` 1, decision 1 case (c)); at 274 Hawaii appears at 88 %; at 300 both
/// are whole. No drawn box of any country meets the controls' rect at any band from 140 to 300; a
/// pane of 139 reads the 140 entry and one of 360 the 300 entry. Fails with the table read at the
/// wrong height, the controls' rect left out of the placement, or the scale applied to the pads.
#[test]
fn insets_scale_per_band() {
    let s = store();
    let us = c("US");
    let box_of = |pane: &Pane, label: &str| {
        let f = s.frame(us, pane, s.fit(us, pane).unwrap()).unwrap();
        (
            f.insets
                .iter()
                .find(|i| i.label == label)
                .map(|i| i.rect.map(f64::from)),
            f.stats.insets_dropped,
        )
    };
    let (alaska, dropped) = box_of(&Pane::band(178), "Alaska");
    let [_, _, w, h] = alaska.unwrap();
    assert!(
        (w - 84.0 * 0.83).abs() < 1e-3 && (h - 56.0 * 0.83).abs() < 1e-3,
        "{alaska:?}"
    );
    assert_eq!((box_of(&Pane::band(178), "Hawaii").0, dropped), (None, 1));
    let (hawaii, _) = box_of(&Pane::band(274), "Hawaii");
    assert!(
        (hawaii.unwrap()[2] - 60.0 * 0.88).abs() < 1e-3,
        "{hawaii:?}"
    );
    let (hawaii, dropped) = box_of(&Pane::band(300), "Hawaii");
    assert_eq!((hawaii, dropped), (Some([98.0, 258.0, 60.0, 32.0]), 0));
    for (i, ct) in s.countries.iter().enumerate() {
        if ct.insets.is_empty() {
            continue;
        }
        for h in rules::BAND_FLOOR..=rules::BAND_MAX {
            let pane = Pane::band(h);
            let controls = rules::controls_rect(&pane);
            for (k, b) in s.inset_boxes(i, &pane).into_iter().enumerate() {
                if let Some(rect) = b {
                    assert!(
                        rules::boxes_apart(rect, controls),
                        "{} {} at 328 × {h}: {rect:?} on the controls",
                        ct.name,
                        ct.insets[k].label
                    );
                }
            }
        }
    }
    let at = |h: f64| {
        s.inset_boxes(
            us,
            &Pane {
                width: 328.0,
                height: h,
                padding: 20.0,
            },
        )
    };
    let sizes = |b: Vec<Option<[f64; 4]>>| -> Vec<Option<[f64; 2]>> {
        b.into_iter()
            .map(|r| r.map(|[_, _, w, h]| [w, h]))
            .collect()
    };
    // a pane of 139 reads the 140 entry (the box's size; its position follows the pane's height)
    assert_eq!(sizes(at(139.0)), sizes(at(140.0)));
    assert_eq!(
        sizes(s.inset_boxes(
            us,
            &Pane {
                width: 400.0,
                height: 360.0,
                padding: 20.0
            }
        )),
        sizes(at(300.0))
    );
}
