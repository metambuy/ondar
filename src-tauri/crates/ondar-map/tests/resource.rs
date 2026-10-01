//! Tests on the shipped resource, `src-tauri/resources/map/world.ondarmap` (commit 5): it loads,
//! it carries the pinned inputs, every blob meets its bound, the loader survives its bytes
//! mangled, and its coverage matches the clamp (D6).

use ondar_map::format::{Layer, Role, Store};
use ondar_map::index::{self, CLIP_MARGIN_PT, LAND_TOL_PT, SUB_TOL_PT};
use ondar_map::laea::Laea;
use ondar_map::rules::{self, LADDER, Pane};
use std::sync::OnceLock;

fn bytes() -> &'static [u8] {
    static B: OnceLock<Vec<u8>> = OnceLock::new();
    B.get_or_init(|| {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/map/world.ondarmap");
        std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    })
}

fn store() -> &'static Store {
    static S: OnceLock<Store> = OnceLock::new();
    S.get_or_init(|| Store::load(bytes()).unwrap())
}

/// The header names the inputs `ondar-map-build/pins.tsv` pins, the tag, the authalic radius and
/// the ladder. Fails on a resource built from other inputs.
#[test]
fn pins_match() {
    let s = store();
    let pins = include_str!("../../ondar-map-build/pins.tsv");
    let rows: Vec<(&str, &str)> = pins
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[0], f[2])
        })
        .collect();
    let p = s.pins();
    assert_eq!(p.ne_tag, "v5.1.2");
    assert_eq!(p.inputs.len(), rows.len());
    for ((name, sha), (rn, rs)) in p.inputs.iter().zip(&rows) {
        let hex: String = sha.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!((name.as_str(), hex.as_str()), (*rn, *rs));
    }
    assert_eq!(p.tool_git.len(), 40);
    assert_eq!(s.header.radius_km, ondar_map::laea::R_AUTHALIC_KM);
    assert_eq!(s.header.ladder, LADDER.to_vec());
    assert_eq!(s.header.golden_pane, Pane::GOLDEN);
    assert_eq!((s.units.len(), s.countries.len()), (267, 248));
}

/// Every blob's measured bound is within its layer's: 0.25 pt land, 0.5 pt subdivisions.
#[test]
fn every_blob_meets_the_spec() {
    for b in &store().blobs {
        let lim = match b.layer {
            Layer::Land => LAND_TOL_PT,
            Layer::Subdivisions => SUB_TOL_PT,
        };
        assert!(f64::from(b.bound_pt) <= lim + 1e-6, "{b:?}");
    }
}

/// The loader on the real file: every truncation of its first 4 KiB and 200 seeded lengths are
/// errors; 400 seeded mutations of 1–8 bytes load or fail, never panic, and a store that loads
/// decodes every ring. (The synthetic resource takes 10 000 mutations in `format::tests`; here
/// each full load inflates 2.7 MB, about 70 ms in a debug build.)
#[test]
fn loader_never_panics_on_the_resource() {
    let b = bytes();
    let mut x = 0x5EED_2026u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for cut in 0..4096.min(b.len()) {
        assert!(Store::load(&b[..cut]).is_err(), "{cut}");
    }
    for _ in 0..200 {
        let cut = (next() as usize) % b.len();
        assert!(Store::load(&b[..cut]).is_err(), "{cut}");
    }
    let mut out = Vec::new();
    for _ in 0..400 {
        let mut m = b.to_vec();
        for _ in 0..1 + next() % 8 {
            // most of the file is blob bytes behind a CRC; aim half the mutations at the tables
            let i = if next() % 2 == 0 {
                (next() as usize) % 200_000.min(m.len())
            } else {
                (next() as usize) % m.len()
            };
            m[i] ^= 1 << (next() % 8);
        }
        if let Ok(s) = Store::load(&m) {
            for blob in 0..s.blobs.len() {
                for r in 0..s.rings(blob).len() {
                    let _ = s.decode(blob, r, &mut out);
                }
            }
        }
    }
}

/// D6's views at their extremes: for every country and every level it can show, the views at the
/// level's finest and coarsest scale with the view in each corner of the fit rectangle and at its
/// centre, each clipped with the 2 pt margin. Every ring whose cap (grown by the level's
/// tolerance) meets such a clip rectangle must have its blob stored — the land at that level,
/// the subdivisions above 8 km/pt for a flagged country — and every inset's units at the inset's
/// level. The cap test is the frame index's own (`index::ground_cap`, `index::cap_meets`).
///
/// Fails if the tool's coverage is smaller than the clamp's reach: without the clip margin,
/// without the cap tolerance, with the fit rectangle at the fit for a country finer than the
/// floor, or with a frame index that admits more than the tool stored.
#[test]
fn coverage_matches_clamp() {
    let s = store();
    let pane = Pane::GOLDEN;
    let (uw, uh) = pane.usable();
    let mut views = 0;
    for (ci, c) in s.countries.iter().enumerate() {
        let [x0, y0, x1, y1] = c.bbox_km;
        let fit = ((x1 - x0) / uw).max((y1 - y0) / uh);
        let top = rules::initial_scale(fit);
        // D6's fit rectangle, from its wording rather than `index::fit_rect`: the pane at the
        // widest scale, centred on the frame bbox
        let (bcx, bcy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (fw, fh) = (pane.width / 2.0 * top, pane.height / 2.0 * top);
        let [fx0, fy0, fx1, fy1] = [bcx - fw, bcy - fh, bcx + fw, bcy + fh];
        let l = Laea::new(c.lat0, c.lon0);
        for (k, &level) in LADDER.iter().enumerate().take(rules::level_for(top) + 1) {
            let finest = level.max(rules::FLOOR_KM_PER_PT).min(top);
            let coarsest = LADDER
                .get(k + 1)
                .map_or(top, |n| (n * (1.0 - 1e-9)).min(top));
            for scale in [finest, coarsest] {
                assert_eq!(rules::level_for(scale), k, "{} {scale}", c.name);
                let (hw, hh) = (pane.width / 2.0 * scale, pane.height / 2.0 * scale);
                // the view's centre range: the fit rectangle less half a view
                let (cx0, cx1) = (fx0 + hw, fx1 - hw);
                let (cy0, cy1) = (fy0 + hh, fy1 - hh);
                assert!(cx0 <= cx1 + 1e-6 && cy0 <= cy1 + 1e-6, "{}", c.name);
                let m = CLIP_MARGIN_PT * scale;
                for (cx, cy) in [
                    (cx0, cy0),
                    (cx1, cy0),
                    (cx0, cy1),
                    (cx1, cy1),
                    ((cx0 + cx1) / 2.0, (cy0 + cy1) / 2.0),
                ] {
                    views += 1;
                    let clip = [cx - hw - m, cy - hh - m, cx + hw + m, cy + hh + m];
                    let g = index::ground_cap(&l, clip);
                    let tol = index::tolerance_km(level, LAND_TOL_PT);
                    for (u, unit) in s.units.iter().enumerate() {
                        if !index::cap_meets(&unit.cap, g, tol) {
                            continue;
                        }
                        let hit = unit
                            .parts
                            .iter()
                            .flat_map(|p| p.rings.iter())
                            .any(|r| index::cap_meets(r, g, tol));
                        if hit {
                            assert!(
                                s.blob(u as u16, k as u8, Layer::Land).is_some(),
                                "{}: unit {} at level {k} (scale {scale:.3}) is not stored",
                                c.name,
                                String::from_utf8_lossy(&unit.a3)
                            );
                        }
                    }
                    if c.subdivisions && scale > rules::SUBDIVISIONS_ABOVE_KM_PER_PT {
                        let tol = index::tolerance_km(level, SUB_TOL_PT);
                        if c.sub_lines.iter().any(|r| index::cap_meets(r, g, tol)) {
                            assert!(
                                s.blob(ci as u16, k as u8, Layer::Subdivisions).is_some(),
                                "{}: subdivisions at level {k}",
                                c.name
                            );
                        }
                    }
                }
            }
        }
        for (i, inset) in c.insets.iter().enumerate() {
            let k = rules::level_for(inset.scale) as u8;
            for &u in &c.units {
                let unit = &s.units[usize::from(u)];
                if unit.parts.iter().any(|p| p.role == Role::Inset(i as u8)) {
                    assert!(
                        s.blob(u, k, Layer::Land).is_some(),
                        "{} inset {}",
                        c.name,
                        inset.label
                    );
                }
            }
        }
    }
    assert!(views > 248 * 10, "{views}");
}
