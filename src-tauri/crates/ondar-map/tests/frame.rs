//! M4a § 5's frame tests, on the shipped resource: the golden table, Step 0 reproduced, the
//! antimeridian, Antarctica, the insets' clearance, R7's lookup, subdivisions by scale, the clamp
//! (D6) and the index's exactness.

use ondar_map::format::{Layer, Role, Store};
use ondar_map::frame::{Frame, Lookup, View};
use ondar_map::laea::Laea;
use ondar_map::rules::{self, FLOOR_KM_PER_PT, LADDER, Pane};
use std::sync::OnceLock;

fn store() -> &'static Store {
    static S: OnceLock<Store> = OnceLock::new();
    S.get_or_init(|| {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/map/world.ondarmap");
        Store::load(&std::fs::read(p).unwrap()).unwrap()
    })
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
    // AQ's top level is 3 (12 km/pt); with D6 no frame reaches it at 4
    let b3 = s.blob(u, 3, Layer::Land).unwrap();
    assert!(s.blobs[b3].vertices * 2 < s.blobs[b0].vertices);
    assert!(s.blob(u, 4, Layer::Land).is_none());
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
        s.clamp(
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
        s.clamp(
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
        .clamp(
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
        .clamp(
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
        s.clamp(
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
        s.clamp(
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
