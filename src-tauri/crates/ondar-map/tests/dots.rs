//! M4c § 6 on the shipped resource: the four geo slices gathered and located (S3, per dot), the
//! dots in the frame, a dot hidden under a drawn box, and an inset's dots at 178 and 300.
//!
//! The slices are `ondar-stations`' fixtures, read here as its normaliser reads them: the uuid,
//! `geo_lat` / `geo_long` with (0, 0) as none and the values as given, `state` trimmed.
//! `the_geo_slices_normalise_to_their_row_counts` (ondar-stations) pins that every row of each
//! slice normalises with its `geo` and survives the rank, in this order, so these `Point`s are
//! the ones the shell builds from the cached list.

use ondar_map::format::{Role, Store};
use ondar_map::frame::{Frame, Lookup, View, dot_radius};
use ondar_map::gather::{GATHER_KM, Gathered, GroundDot, OUTSIDE_KM, PartRef, Point, gather};
use ondar_map::laea::{Laea, haversine_km};
use ondar_map::rules::Pane;
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

fn points(cc: &str) -> Vec<Point> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../ondar-stations/fixtures/stations-{cc}-geo.json"));
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    rows.iter()
        .map(|r| {
            let (lat, lon) = (r["geo_lat"].as_f64(), r["geo_long"].as_f64());
            Point {
                id: r["stationuuid"].as_str().unwrap().to_string(),
                geo: match (lat, lon) {
                    (Some(la), Some(lo)) if !(la == 0.0 && lo == 0.0) => Some((la, lo)),
                    _ => None,
                },
                place: r["state"].as_str().unwrap_or("").trim().to_string(),
            }
        })
        .collect()
}

fn fit_frame(code: &str, h: u32, g: &Gathered) -> Frame {
    let s = store();
    let pane = Pane::band(h);
    s.frame_dots(c(code), &pane, s.fit(c(code), &pane).unwrap(), g)
        .unwrap()
}

fn inset_index(code: &str, label: &str) -> u8 {
    let i = store().countries[c(code)]
        .insets
        .iter()
        .position(|i| i.label == label)
        .unwrap();
    u8::try_from(i).unwrap()
}

/// Pins the slices' dots as Step 0 (g) recorded them on 2026-10-07 (`q4dots`, 10 km), and R6
/// per dot: groups PT 31 / US 48 / BR 124 / RU 10; located dots 28 / 46 / 122 / 10; stations
/// outside 3 / 2 / 2 / 0, each outside group a single station. PT's three are its known
/// outliers, each its own dot: two in Brazil and one at Zürich, each over 25 km from every part
/// of PT. Every slice row has `geo`, so `total` is the row count and located + outside = total.
/// Fails with the merge radius at 9 or 11 km (US 49 groups; PT 30) or with R6 dropped (PT 0
/// outside). It does not see the tie rule, a centroid at the densest member, R6 moved by
/// 0.2 km, R6 per station, or the locate at level 4: the slices' figures are the same under
/// each (the synthetic tests in `gather.rs` pin those; mutations in the commit message).
#[test]
fn the_geo_slices_gather_to_the_recorded_dots() {
    let s = store();
    for (cc, rows, groups, dots, outside) in [
        ("PT", 68, 31, 28, 3),
        ("US", 169, 48, 46, 2),
        ("BR", 220, 124, 122, 2),
        ("RU", 50, 10, 10, 0),
    ] {
        let pts = points(cc);
        let gs = gather(&pts, GATHER_KM);
        let g = s.gather_dots(c(cc), &pts);
        assert_eq!(
            (pts.len(), gs.len(), g.dots.len(), g.outside),
            (rows, groups, dots, outside),
            "{cc}"
        );
        assert_eq!((g.total, g.located + g.outside), (rows, rows), "{cc}");
        let out: Vec<_> = gs
            .iter()
            .filter(|gr| !g.dots.iter().any(|d| d.ids == gr.ids))
            .collect();
        assert_eq!(out.len(), outside, "{cc}: one station per outside group");
        assert!(out.iter().all(|gr| gr.ids.len() == 1), "{cc}");
        let loc = s.locator(c(cc));
        for gr in &out {
            assert!(loc.locate(gr.lat, gr.lon).km > OUTSIDE_KM, "{cc}");
        }
        if cc == "PT" {
            let brazil = out
                .iter()
                .filter(|gr| gr.lat < 0.0 && gr.lon < -30.0)
                .count();
            let zurich = out
                .iter()
                .filter(|gr| haversine_km(gr.lon, gr.lat, 8.54, 47.37) < 20.0)
                .count();
            assert_eq!((brazil, zurich), (2, 1), "PT's outliers");
        }
    }
}

/// Pins decision 4 for an inset's dots: US at the fit at 178, where Hawaii's box is not drawn
/// (its stored scale is 0 there), hides Honolulu's dot — the only US dot in Hawaii's part —
/// and counts it: `dots_hidden` 1, its uuids not on the pane. At 300 the box is drawn and the
/// dot is drawn inside it, `dots_hidden` 0. Fails with the inset's dots projected with the view
/// at the fit (drawn off the pane or dropped uncounted: hidden 0), with the hidden count
/// dropped, or with the box's projection replaced by the main one (the dot outside the box).
#[test]
fn an_inset_s_dots_follow_its_box_honolulu() {
    let pts = points("US");
    let g = store().gather_dots(c("US"), &pts);
    let hawaii = inset_index("US", "Hawaii");
    let hi: Vec<&GroundDot> = g
        .dots
        .iter()
        .filter(|d| d.part.role == Role::Inset(hawaii))
        .collect();
    assert_eq!(hi.len(), 1, "one dot in Hawaii");
    assert!(
        haversine_km(hi[0].lon, hi[0].lat, -157.86, 21.31) < 20.0,
        "Honolulu"
    );
    let drawn = |f: &Frame| f.dots.iter().find(|d| d.uuids == hi[0].ids).cloned();
    let f = fit_frame("US", 178, &g);
    assert!(f.insets.iter().all(|i| i.label != "Hawaii"));
    assert_eq!(f.stats.dots_hidden, 1);
    assert_eq!(drawn(&f), None);
    assert_eq!(f.dots.len(), g.dots.len() - 1);
    let f = fit_frame("US", 300, &g);
    assert_eq!(f.stats.dots_hidden, 0);
    let [x, y, w, h] = f.insets.iter().find(|i| i.label == "Hawaii").unwrap().rect;
    let d = drawn(&f).expect("Honolulu drawn at 300");
    assert!(
        d.x >= x && d.x <= x + w && d.y >= y && d.y <= y + h,
        "{d:?} in {x} {y} {w} {h}"
    );
}

/// Pins decision 4 for a dot in the main projection: a synthetic one-station dot on PT's
/// mainland part, placed at the Azores box's centre at the fit (300), is under the drawn box:
/// not drawn, `dots_hidden` 1. Zoomed one step (half the fit's scale) onto it, no box is drawn
/// (D7) and the dot is, at the pane's centre ± the clamp, `dots_hidden` 0. Fails with the box
/// test dropped (the dot drawn over the box at the fit) or with the hidden count not made.
#[test]
fn a_dot_under_a_drawn_box_at_the_fit_is_hidden_and_counted() {
    let s = store();
    let pt = c("PT");
    let pane = Pane::GOLDEN;
    let fit = s.fit(pt, &pane).unwrap();
    let f0 = s.frame(pt, &pane, fit).unwrap();
    let [x, y, w, h] = f0.insets.iter().find(|i| i.label == "Azores").unwrap().rect;
    let (bx, by) = (f64::from(x + w / 2.0), f64::from(y + h / 2.0));
    let (lon, lat) = s.unproject(pt, &pane, &fit, bx, by).unwrap();
    let main = s.countries[pt].units[0];
    let g = Gathered {
        dots: vec![GroundDot {
            lat,
            lon,
            ids: vec!["synthetic".into()],
            place: String::new(),
            part: PartRef {
                unit: main,
                part: 0,
                role: Role::Frame,
            },
        }],
        outside: 0,
        located: 1,
        total: 1,
    };
    let f = s.frame_dots(pt, &pane, fit, &g).unwrap();
    assert_eq!((f.dots.len(), f.stats.dots_hidden), (0, 1));
    let ct = &s.countries[pt];
    let centre = Laea::new(ct.lat0, ct.lon0).fwd(lon, lat).unwrap();
    let zoomed = View {
        centre,
        scale: fit.scale / 2.0,
    };
    let f = s.frame_dots(pt, &pane, zoomed, &g).unwrap();
    assert!(f.insets.is_empty());
    assert_eq!((f.dots.len(), f.stats.dots_hidden), (1, 0));
    let d = &f.dots[0];
    assert!(
        d.x >= 0.0 && d.x <= 328.0 && d.y >= 0.0 && d.y <= 300.0,
        "{d:?}"
    );
}

/// PT at the fit (300): its 28 located dots are all drawn, none hidden, the Azores' and
/// Madeira's inside their boxes, the frame's stats carry the gathering's counts, and the dots
/// keep the gathering's order (larger first). Fails if an inset's dot is projected with the
/// view, if the counts are not carried, or if the frame reorders the dots.
#[test]
fn pt_s_dots_at_the_fit() {
    let pts = points("PT");
    let g = store().gather_dots(c("PT"), &pts);
    let f = fit_frame("PT", 300, &g);
    assert_eq!((f.dots.len(), f.stats.dots_hidden), (28, 0));
    assert_eq!(
        (
            f.stats.dots_outside,
            f.stats.stations_located,
            f.stats.stations_total
        ),
        (3, 65, 68)
    );
    let order: Vec<&Vec<String>> = g.dots.iter().map(|d| &d.ids).collect();
    let drawn: Vec<&Vec<String>> = f.dots.iter().map(|d| &d.uuids).collect();
    assert_eq!(drawn, order);
    assert!(f.dots.windows(2).all(|w| w[0].n >= w[1].n));
    for label in ["Azores", "Madeira"] {
        let i = inset_index("PT", label);
        let [x, y, w, h] = f.insets.iter().find(|b| b.label == label).unwrap().rect;
        let mine: Vec<_> = g
            .dots
            .iter()
            .filter(|d| d.part.role == Role::Inset(i))
            .collect();
        assert!(!mine.is_empty(), "{label} has dots");
        for gd in mine {
            let d = f.dots.iter().find(|d| d.uuids == gd.ids).unwrap();
            assert!(
                d.x >= x && d.x <= x + w && d.y >= y && d.y <= y + h,
                "{label} {d:?}"
            );
        }
    }
}

/// Pins the radius: `min(6, 2.5 + 0.6 ln n)` — n = 1 → 2.5, 64 → 5.0 (to 0.01), 1 000 → 6.0
/// (capped); a frame's `r` is the formula's, rounded to 0.01 pt. Fails with the slope at 0.5 or
/// the cap at 7.
#[test]
fn radius_formula() {
    assert_eq!(dot_radius(1), 2.5);
    assert!((dot_radius(64) - 5.0).abs() < 0.005, "{}", dot_radius(64));
    assert_eq!(dot_radius(1000), 6.0);
    // the cap binds from e^(35/6) ≈ 341.6
    assert!(dot_radius(341) < 6.0 && dot_radius(342) == 6.0);
    let g = store().gather_dots(c("BR"), &points("BR"));
    let f = fit_frame("BR", 300, &g);
    for d in &f.dots {
        let want = (dot_radius(d.uuids.len()) * 100.0).round() / 100.0;
        assert_eq!(f64::from(d.r), want as f32 as f64);
        assert_eq!(d.n as usize, d.uuids.len());
    }
}
