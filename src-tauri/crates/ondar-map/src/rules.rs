//! The frame rules shared by the build tool and the runtime (one code path): the pane, the fit,
//! the S1 floor, the global ladder and the level a view uses.

/// The zoom-in limit and the initial scale's floor (S1), km/pt.
pub const FLOOR_KM_PER_PT: f64 = 1.5;

/// The global ladder (R4): 1.5 · 2^k km/pt up to the coarsest fit (RU, 28.01 km/pt).
pub const LADDER: [f64; 5] = [1.5, 3.0, 6.0, 12.0, 24.0];

/// Subdivisions are drawn for a flagged country when the view is coarser than this (S7), km/pt.
pub const SUBDIVISIONS_ABOVE_KM_PER_PT: f64 = 8.0;

/// The map pane, points; `padding` on every side is kept clear at the fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pane {
    pub width: f64,
    pub height: f64,
    pub padding: f64,
}

impl Pane {
    /// The pane the golden tables and the inset boxes are laid out for (DIRECTION, 328 × 300,
    /// 20 pt padding). The runtime takes the pane Rust reports; this one is for the tables.
    pub const GOLDEN: Pane = Pane {
        width: 328.0,
        height: 300.0,
        padding: 20.0,
    };

    /// A pane is finite, its sides positive and its padding not negative (review finding 4: a
    /// negative side with a negative padding has a positive usable area).
    pub fn is_valid(&self) -> bool {
        [self.width, self.height, self.padding]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.0
            && self.height > 0.0
            && self.padding >= 0.0
    }

    /// The usable area inside the padding, points.
    pub fn usable(&self) -> (f64, f64) {
        (
            self.width - 2.0 * self.padding,
            self.height - 2.0 * self.padding,
        )
    }
}

/// The fit: the scale (km/pt) at which a projected bbox of `w` × `h` km just fills the usable
/// area. `None` for a pane that is not valid or has no usable area — every frame function goes
/// through here, so none of them sees such a pane.
pub fn fit_scale(w_km: f64, h_km: f64, pane: &Pane) -> Option<f64> {
    let (uw, uh) = pane.usable();
    if !pane.is_valid() || uw <= 0.0 || uh <= 0.0 {
        return None;
    }
    Some((w_km / uw).max(h_km / uh))
}

/// The initial scale (S1): the fit, but never finer than the floor.
pub fn initial_scale(fit: f64) -> f64 {
    fit.max(FLOOR_KM_PER_PT)
}

/// The level a view at `scale` km/pt uses: the coarsest ladder level at or below it, so a level's
/// tolerance in km is never more than its tolerance in points at the view. A scale below the
/// floor uses level 0.
pub fn level_for(scale: f64) -> usize {
    LADDER
        .iter()
        .rposition(|&l| l <= scale * (1.0 + 1e-12))
        .unwrap_or(0)
}

/// An inset's land is fitted inside its box less this padding on every side, points.
pub const INSET_PAD_PT: f64 = 4.0;
/// …and above a label strip this tall along the box's bottom edge, points.
pub const INSET_LABEL_PT: f64 = 8.0;
/// No inset box comes closer than this to its country's land at the initial view (S6), points.
pub const INSET_CLEARANCE_PT: f64 = 12.0;

/// The area of an inset box `[x, y, w, h]` (points, y down) its land is fitted into:
/// `(centre_x, centre_y, width, height)`.
pub fn inset_area([x, y, w, h]: [f64; 4]) -> (f64, f64, f64, f64) {
    let (aw, ah) = (
        w - 2.0 * INSET_PAD_PT,
        h - INSET_LABEL_PT - 2.0 * INSET_PAD_PT,
    );
    (x + w / 2.0, y + INSET_PAD_PT + ah / 2.0, aw, ah)
}

/// The scale (km/pt) at which a `w` × `h` km group fills its inset box's area.
pub fn inset_scale(w_km: f64, h_km: f64, rect: [f64; 4]) -> Option<f64> {
    let (_, _, aw, ah) = inset_area(rect);
    (aw > 0.0 && ah > 0.0).then(|| (w_km / aw).max(h_km / ah))
}

/// The distance from `p` to the segment `a`–`b` (same units).
// max then min: the crate's no-panic scan refuses `.clamp(` (review finding 4); the bounds here
// are constants, but one rule for the whole crate is the one the scan can check
#[allow(clippy::manual_clamp)]
pub fn seg_dist([px, py]: [f64; 2], [ax, ay]: [f64; 2], [bx, by]: [f64; 2]) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((px - ax) * dx + (py - ay) * dy) / l2).max(0.0).min(1.0)
    } else {
        0.0
    };
    (px - ax - t * dx).hypot(py - ay - t * dy)
}

/// Whether the segments `a`–`b` and `c`–`d` intersect; touching (an end on the other segment,
/// collinear overlap) counts.
pub fn segments_cross(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let o = |[px, py]: [f64; 2], [qx, qy]: [f64; 2], [rx, ry]: [f64; 2]| {
        ((qx - px) * (ry - py) - (qy - py) * (rx - px)).signum()
    };
    if o(a, b, c) != o(a, b, d) && o(c, d, a) != o(c, d, b) {
        return true;
    }
    let on = |p, q, r| seg_dist(r, p, q) == 0.0;
    on(a, b, c) || on(a, b, d) || on(c, d, a) || on(c, d, b)
}

/// The distance (same units) from a rectangle `[x0, y0, x1, y1]` to a ring — open or closed
/// (first == last); 0 if they meet or one holds the other (even-odd). S6's clearance: the build
/// tool's check at the golden pane and the frame's `inset_clearance` share this one function.
pub fn rect_ring_distance(
    [x0, y0, x1, y1]: [f64; 4],
    ring: impl IntoIterator<Item = [f64; 2]>,
) -> f64 {
    let r: Vec<[f64; 2]> = ring.into_iter().collect();
    let inside = |[x, y]: [f64; 2]| x >= x0 && x <= x1 && y >= y0 && y <= y1;
    if r.iter().any(|&p| inside(p)) {
        return 0.0;
    }
    // every edge, the closing one included (a closed ring adds a zero-length edge: no effect)
    let edges = || {
        r.iter()
            .copied()
            .zip(r.iter().copied().cycle().skip(1))
            .take(r.len())
    };
    let in_ring = |[px, py]: [f64; 2]| {
        edges().fold(false, |odd, ([ax, ay], [bx, by])| {
            let crosses = (ay > py) != (by > py) && px < ax + (py - ay) / (by - ay) * (bx - ax);
            odd != crosses
        })
    };
    let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    if corners.iter().any(|&c| in_ring(c)) {
        return 0.0;
    }
    let sides: Vec<([f64; 2], [f64; 2])> = corners
        .iter()
        .copied()
        .zip(corners.iter().copied().cycle().skip(1))
        .take(4)
        .collect();
    let mut best = f64::INFINITY;
    for (a, b) in edges() {
        for &(c, d) in &sides {
            if segments_cross(a, b, c, d) {
                return 0.0;
            }
            best = best.min(seg_dist(a, c, d)).min(seg_dist(c, a, b));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic 288 × 260 km rectangle fills the golden pane's usable area at exactly
    /// 1 km/pt; on the ANMITE's 328 × 178 pane the height binds: h / 138.
    #[test]
    fn pane_arithmetic() {
        assert_eq!(fit_scale(288.0, 260.0, &Pane::GOLDEN), Some(1.0));
        assert_eq!(fit_scale(576.0, 260.0, &Pane::GOLDEN), Some(2.0));
        assert_eq!(fit_scale(288.0, 520.0, &Pane::GOLDEN), Some(2.0));
        let anmite = Pane {
            width: 328.0,
            height: 178.0,
            padding: 20.0,
        };
        assert_eq!(fit_scale(288.0, 260.0, &anmite), Some(260.0 / 138.0));
        let none = Pane {
            width: 40.0,
            height: 300.0,
            padding: 20.0,
        };
        assert_eq!(fit_scale(1.0, 1.0, &none), None);
    }

    #[test]
    fn initial_scale_floor() {
        assert_eq!(initial_scale(0.102), 1.5);
        assert_eq!(initial_scale(2.2187), 2.2187);
    }

    /// The Azores' box on the style page, 92 × 52 at (10, 24): the land area is 84 × 36 pt,
    /// centred 4 pt below the top and above the 8 pt label strip.
    #[test]
    fn inset_box_arithmetic() {
        assert_eq!(
            inset_area([10.0, 24.0, 92.0, 52.0]),
            (56.0, 46.0, 84.0, 36.0)
        );
        assert_eq!(
            inset_scale(840.0, 36.0, [10.0, 24.0, 92.0, 52.0]),
            Some(10.0)
        );
        assert_eq!(
            inset_scale(84.0, 360.0, [10.0, 24.0, 92.0, 52.0]),
            Some(10.0)
        );
        assert_eq!(inset_scale(1.0, 1.0, [0.0, 0.0, 8.0, 16.0]), None);
    }

    /// S6's distance, open and closed rings alike: apart, diagonal, overlapping, a ring around
    /// the rectangle, and a ring whose edge only touches a side (collinear) — 0, as the tool
    /// counted it before the two copies were one (review finding 7).
    #[test]
    fn rect_ring_distances() {
        let sq = |x: f64, y: f64| vec![[x, y], [x + 1.0, y], [x + 1.0, y + 1.0], [x, y + 1.0]];
        let closed = |x: f64, y: f64| {
            let mut r = sq(x, y);
            r.push([x, y]);
            r
        };
        let rect = [0.0, 0.0, 2.0, 2.0];
        for ring in [sq(5.0, 0.5), closed(5.0, 0.5)] {
            assert_eq!(rect_ring_distance(rect, ring), 3.0);
        }
        assert_eq!(rect_ring_distance(rect, sq(1.5, 1.5)), 0.0);
        assert_eq!(rect_ring_distance(rect, sq(5.0, 6.0)), 5.0);
        let big = vec![[-10.0, -10.0], [10.0, -10.0], [10.0, 10.0], [-10.0, 10.0]];
        assert_eq!(rect_ring_distance(rect, big), 0.0);
        // an edge lying along the rectangle's right side, no vertex inside it
        let touching = vec![[2.0, -1.0], [2.0, 3.0], [4.0, 3.0], [4.0, -1.0]];
        assert_eq!(rect_ring_distance(rect, touching), 0.0);
        assert!(segments_cross(
            [2.0, -1.0],
            [2.0, 3.0],
            [2.0, 0.0],
            [2.0, 2.0]
        ));
        assert!(!segments_cross(
            [0.0, 0.0],
            [1.0, 0.0],
            [2.0, 0.0],
            [3.0, 0.0]
        ));
    }

    /// The coarsest level at or below the scale: RU's 28.01 → 24, 12.0 → 12, 11.99 → 6.
    #[test]
    fn level_is_the_coarsest_at_or_below() {
        let at = |s: f64| LADDER[level_for(s)];
        assert_eq!(at(28.0107), 24.0);
        assert_eq!(at(12.0), 12.0);
        assert_eq!(at(11.99), 6.0);
        assert_eq!(at(15.9485), 12.0);
        assert_eq!(at(1.5), 1.5);
        assert_eq!(at(2.9999), 1.5);
        assert_eq!(at(1.0), 1.5);
    }
}
