//! The ring index (R10) and the clamp's reach (D6), shared by the build tool (coverage: which
//! blobs to store) and the frame (which rings to read) — one code path, so the index never asks
//! for a blob the tool did not store.
//!
//! D6 (decided 2026-10-01): **the view stays inside the fit rectangle** — the pane at the
//! country's widest scale (the fit, or the 1.5 floor for a country finer than it), centred on
//! the frame bbox. At that scale the view is the fit rectangle and cannot pan; zoomed in, its
//! centre moves only so far that the view's edge reaches the rectangle's. Every frame clips to
//! the view grown by `CLIP_MARGIN_PT` (so a hairline at the pane's edge stays outside).

use crate::clip::Rect;
use crate::format::Cap;
use crate::laea::{Laea, R_AUTHALIC_KM, haversine_km};
use crate::rules::{LADDER, Pane, initial_scale, level_for};

/// The clip rectangle is the view grown by this on every side, points.
pub const CLIP_MARGIN_PT: f64 = 2.0;

/// The clip rectangle in pane points, `[x0, y0, x1, y1]`: the pane grown by `CLIP_MARGIN_PT` on
/// every side. The frame clips every ring to it and the tool clips the land its clearance check
/// measures to it — one function, so the two cannot drift (review 3, finding 5: the Jan Mayen
/// defect was the tool measuring land the frame never draws).
pub fn clip_rect(pane: &Pane) -> Rect {
    [
        -CLIP_MARGIN_PT,
        -CLIP_MARGIN_PT,
        pane.width + CLIP_MARGIN_PT,
        pane.height + CLIP_MARGIN_PT,
    ]
}
/// Land's simplification bound, points at its level (R3).
pub const LAND_TOL_PT: f64 = 0.25;
/// Subdivisions' (S7).
pub const SUB_TOL_PT: f64 = 0.5;
/// What the codec adds: half a quantum's diagonal, points at the level.
pub const QUANT_PT: f64 = crate::codec::QUANTUM_PT * std::f64::consts::FRAC_1_SQRT_2;

/// How far a stored ring may lie from its cap (computed on the unsimplified ring) at a level,
/// km: the simplification bound plus the codec's.
pub fn tolerance_km(level_km_per_pt: f64, tol_pt: f64) -> f64 {
    (tol_pt + QUANT_PT) * level_km_per_pt
}

/// The fit rectangle, km in the frame's LAEA: the pane at the widest scale, centred on the frame
/// bbox `[min_x, min_y, max_x, max_y]`.
pub fn fit_rect(bbox: [f64; 4], fit: f64, pane: &Pane) -> [f64; 4] {
    let [x0, y0, x1, y1] = bbox;
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let s = initial_scale(fit);
    let (hw, hh) = (pane.width / 2.0 * s, pane.height / 2.0 * s);
    [cx - hw, cy - hh, cx + hw, cy + hh]
}

/// The coarsest scale a view at level `k` can have: below the next level, and never wider than
/// the widest view.
pub fn max_scale_at(k: usize, fit: f64) -> f64 {
    let top = initial_scale(fit);
    LADDER.get(k + 1).copied().map_or(top, |next| next.min(top))
}

/// The country's top level: the one its widest view uses.
pub fn top_level(fit: f64) -> usize {
    level_for(initial_scale(fit))
}

/// Every clip rectangle a view at level `k` can have lies inside this: the fit rectangle grown by
/// the clip margin at the level's coarsest scale (D6: the view never leaves the fit rectangle).
pub fn reach(bbox: [f64; 4], fit: f64, pane: &Pane, k: usize) -> [f64; 4] {
    let [x0, y0, x1, y1] = fit_rect(bbox, fit, pane);
    let m = CLIP_MARGIN_PT * max_scale_at(k, fit);
    [x0 - m, y0 - m, x1 + m, y1 + m]
}

/// A rectangle of a projection as a ground cap `(lon, lat, radius km)` holding it: its centre's
/// inverse and the farthest of 64 samples per edge (the distance from a point has no maximum
/// inside a region short of the antipode, so the boundary bounds it), + 1 % + 1 km. A rectangle
/// leaving the projection's disc is the whole sphere.
pub fn ground_cap(l: &Laea, [x0, y0, x1, y1]: [f64; 4]) -> (f64, f64, f64) {
    let whole = std::f64::consts::PI * R_AUTHALIC_KM;
    let Some((clon, clat)) = l.inv((x0 + x1) / 2.0, (y0 + y1) / 2.0) else {
        return (0.0, 0.0, whole);
    };
    let mut r = 0f64;
    for i in 0..=64 {
        let t = f64::from(i) / 64.0;
        for (x, y) in [
            (x0 + t * (x1 - x0), y0),
            (x0 + t * (x1 - x0), y1),
            (x0, y0 + t * (y1 - y0)),
            (x1, y0 + t * (y1 - y0)),
        ] {
            match l.inv(x, y) {
                Some((lon, lat)) => r = r.max(haversine_km(clon, clat, lon, lat)),
                None => return (clon, clat, whole),
            }
        }
    }
    (clon, clat, r * 1.01 + 1.0)
}

/// Whether a ring's cap, grown by `tol_km`, meets a ground cap.
pub fn cap_meets(c: &Cap, (lon, lat, r): (f64, f64, f64), tol_km: f64) -> bool {
    haversine_km(f64::from(c.lon), f64::from(c.lat), lon, lat)
        <= f64::from(c.radius_km) + r + tol_km
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 288 × 260 km country fits at 1 km/pt, so its widest view is the floor's, 1.5: the fit
    /// rectangle is the pane at 1.5, and the reach adds 2 pt at 1.5. At fit 20 (top level 3, 12
    /// km/pt) level 0's coarsest view is just under 3 km/pt and level 3's is the fit.
    #[test]
    fn the_reach_is_the_fit_rectangle_and_the_clip_margin() {
        let b = [-144.0, -130.0, 144.0, 130.0];
        assert_eq!(
            fit_rect(b, 1.0, &Pane::GOLDEN),
            [-246.0, -225.0, 246.0, 225.0]
        );
        assert_eq!(
            reach(b, 1.0, &Pane::GOLDEN, 0),
            [-249.0, -228.0, 249.0, 228.0]
        );
        let b = [-2880.0, -2600.0, 2880.0, 2600.0];
        assert_eq!(top_level(20.0), 3);
        assert_eq!(max_scale_at(0, 20.0), 3.0);
        assert_eq!(max_scale_at(3, 20.0), 20.0);
        assert_eq!(
            reach(b, 20.0, &Pane::GOLDEN, 3)[2],
            164.0 * 20.0 + 2.0 * 20.0
        );
        assert_eq!(
            reach(b, 20.0, &Pane::GOLDEN, 0)[2],
            164.0 * 20.0 + 2.0 * 3.0
        );
    }

    /// The clip rectangle is the pane plus 2 pt per side: at the golden pane `[-2, -2, 330, 302]`,
    /// the literal the tool and the frame each built by hand before (fails with the margin
    /// applied on one side only, or in km).
    #[test]
    fn the_clip_rect_is_the_pane_plus_the_margin() {
        assert_eq!(clip_rect(&Pane::GOLDEN), [-2.0, -2.0, 330.0, 302.0]);
        let anmite = Pane {
            width: 328.0,
            height: 178.0,
            padding: 20.0,
        };
        assert_eq!(clip_rect(&anmite), [-2.0, -2.0, 330.0, 180.0]);
    }

    #[test]
    fn caps_meet_with_the_tolerance() {
        let c = Cap {
            lon: 0.0,
            lat: 0.0,
            radius_km: 10.0,
        };
        let deg = R_AUTHALIC_KM * std::f64::consts::PI / 180.0;
        let g = (1.0, 0.0, 100.0); // 1° east: ~111.2 km apart
        assert!(!cap_meets(&c, g, 0.0));
        assert!(cap_meets(&c, g, deg - 110.0 + 1e-6));
        assert!(!cap_meets(&c, g, deg - 110.0 - 1e-6));
    }

    #[test]
    fn a_rectangle_off_the_disc_is_the_whole_sphere() {
        let l = Laea::new(0.0, 0.0);
        let r = 2.0 * R_AUTHALIC_KM;
        assert_eq!(
            ground_cap(&l, [-r, -r, r, r]).2,
            std::f64::consts::PI * R_AUTHALIC_KM
        );
        let (_, _, small) = ground_cap(&l, [-100.0, -100.0, 100.0, 100.0]);
        assert!(
            (small - (100.0 * 2f64.sqrt() * 1.01 + 1.0)).abs() < 0.5,
            "{small}"
        );
    }
}
