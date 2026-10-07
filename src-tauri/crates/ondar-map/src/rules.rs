//! The frame rules shared by the build tool and the runtime (one code path): the pane, the fit,
//! the S1 floor, the global ladder and the level a view uses; the one box rule, the controls'
//! rect and the inset box at a band and a scale (I1, C1).

use crate::format::Corner;

/// The zoom-in limit and the initial scale's floor (S1), km/pt.
pub const FLOOR_KM_PER_PT: f64 = 1.5;

/// The global ladder (R4): 1.5 · 2^k km/pt up to the coarsest fit (RU, 28.01 km/pt).
pub const LADDER: [f64; 5] = [1.5, 3.0, 6.0, 12.0, 24.0];

/// Subdivisions are drawn for a flagged country when the view is coarser than this (S7), km/pt.
pub const SUBDIVISIONS_ABOVE_KM_PER_PT: f64 = 8.0;

/// The map band's width, points: the panel's 360 less two 16 pt margins (B1).
pub const BAND_WIDTH_PT: f64 = 328.0;
/// The band's padding, kept clear at the fit on every side, points.
pub const BAND_PADDING_PT: f64 = 20.0;
/// The shortest band the app shows (D1's floor, M4b) and the tallest (the uncapped layout),
/// points: coverage is built for every integer height between them.
pub const BAND_FLOOR: u32 = 140;
pub const BAND_MAX: u32 = 300;

/// Band height `h`'s index in a per-band table (`h − BAND_FLOOR`), or `None` outside
/// `BAND_FLOOR..=BAND_MAX` (M4b's review, latent 6: the tool's six sites subtracted unchecked).
pub fn band_index(h: u32) -> Option<usize> {
    if h > BAND_MAX {
        return None;
    }
    usize::try_from(h.checked_sub(BAND_FLOOR)?).ok()
}

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

    /// The map band at a layout height (M4b, B1): `BAND_WIDTH_PT` wide, `h` points tall, the
    /// golden padding. `band(BAND_MAX)` is `GOLDEN`. The resource's coverage is built for every
    /// integer `h` in `BAND_FLOOR..=BAND_MAX` (M4b commit 3); the shell shows a band only in that
    /// range (D1's floor is `BAND_FLOOR`).
    pub fn band(h: u32) -> Pane {
        Pane {
            width: BAND_WIDTH_PT,
            height: f64::from(h),
            padding: BAND_PADDING_PT,
        }
    }

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
/// through here, so none of them sees such a pane; `project` and `unproject`, which need no fit,
/// check `Pane::is_valid` themselves (review 2, finding 6).
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

/// The zoom controls' row, `− fit +` (Z1, C1; review P5): its size, points…
pub const CONTROLS_SIZE_PT: [f64; 2] = [74.0, 24.0];
/// …and its distance from the pane's bottom and right edges, points.
pub const CONTROLS_MARGIN_PT: f64 = 8.0;

/// The controls' rect `[x, y, w, h]` at a pane: the bottom-right corner, `CONTROLS_MARGIN_PT`
/// in from the pane's bottom and right edges. The tool, the frame and the page read this one
/// function — the corner is reserved (C1): the controls' rect is the first box placed at every
/// band, so no inset box overlaps it. The controls may sit over land; they are not an inset.
pub fn controls_rect(pane: &Pane) -> [f64; 4] {
    let [w, h] = CONTROLS_SIZE_PT;
    [
        pane.width - CONTROLS_MARGIN_PT - w,
        pane.height - CONTROLS_MARGIN_PT - h,
        w,
        h,
    ]
}

/// Whether a box `[x, y, w, h]` (points, y down) lies inside the pane; a box whose edge lies on
/// the pane's edge fits. The one box rule for the tool and the frame (review 3, finding 4): the
/// tool refuses what this refuses, the frame drops what this refuses, and nothing else.
pub fn box_fits([x, y, w, h]: [f64; 4], pane: &Pane) -> bool {
    x >= 0.0 && y >= 0.0 && x + w <= pane.width && y + h <= pane.height
}

/// Whether two boxes `[x, y, w, h]` are apart: they share no area (boxes that touch along an
/// edge or at a corner are apart). The other half of the one box rule.
pub fn boxes_apart([ax, ay, aw, ah]: [f64; 4], [bx, by, bw, bh]: [f64; 4]) -> bool {
    ax + aw <= bx || bx + bw <= ax || ay + ah <= by || by + bh <= ay
}

/// An inset's land is fitted inside its box less this padding on every side, points.
pub const INSET_PAD_PT: f64 = 4.0;
/// …and above a label strip this tall along the box's bottom edge, points.
pub const INSET_LABEL_PT: f64 = 8.0;
/// No inset box comes closer than this to its country's land at the initial view (S6), points.
pub const INSET_CLEARANCE_PT: f64 = 12.0;
/// The label's type size, points: the "Ondar map style" artifact (v3, 2026-09-24), specification
/// row "Inset labels: 8 pt name, bottom-right of the inset; names only". Read at M4b commit 4
/// (review P3), not assumed; under 9 pt, which the STOP says.
pub const INSET_LABEL_FONT_PT: f64 = 8.0;
/// The smallest land area an inset box may fit its land into at any band (I1, decision A): 28 pt
/// across and 12 pt tall, so with the 8 pt strip and the 4 pt pads unscaled no box is under
/// 36 × 28 pt. The proposal Martín judges with commit 4's table (review P2).
pub const INSET_MIN_LAND_PT: [f64; 2] = [28.0, 12.0];

/// The inset box at a band and a scale (I1; M4b commit 4): the golden row's `[x, y, w, h]`
/// scaled to `w·s × h·s` and anchored at `corner` with the golden row's gaps to its two edges —
/// a right corner moves with the pane's width, a bottom corner with its height. At the golden
/// pane and `s = 1` it is the row's rect. The label strip and the pads are inside the box and do
/// not scale, so the land area is `(w·s − 8) × (h·s − 16)` (`inset_area`).
pub fn inset_box_at([x, y, w, h]: [f64; 4], corner: Corner, pane: &Pane, s: f64) -> [f64; 4] {
    let (gap_r, gap_b) = (Pane::GOLDEN.width - x - w, Pane::GOLDEN.height - y - h);
    let (bw, bh) = (w * s, h * s);
    let bx = match corner {
        Corner::TopLeft | Corner::BottomLeft => x,
        Corner::TopRight | Corner::BottomRight => pane.width - gap_r - bw,
    };
    let by = match corner {
        Corner::TopLeft | Corner::TopRight => y,
        Corner::BottomLeft | Corner::BottomRight => pane.height - gap_b - bh,
    };
    [bx, by, bw, bh]
}

/// How one inset box abuts another in the golden table (the stacking rule, M4b commit 4b — the
/// commit 4 STOP's decision 1): `Beside`, along the row (B to the right of A for a left corner, to
/// its left for a right corner, their y ranges overlapping), or `Stacked`, along the column (B above
/// A for a bottom corner, below it for a top corner, their x ranges overlapping); `gap` is the
/// golden distance between them, points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Abut {
    Beside { gap: f64 },
    Stacked { gap: f64 },
}

/// Whether box `b` abuts box `a`, both anchored at `corner` on the golden pane (`None` if not).
/// Hawaii `[98, 258, 60, 32]` is `Beside { gap: 6 }` Alaska `[8, 236, 84, 56]` at the bottom-left;
/// Madeira `[10, 84, 52, 40]` is `Stacked { gap: 8 }` under the Azores `[10, 24, 92, 52]` at the
/// top-left.
pub fn abuts(
    [bx, by, bw, bh]: [f64; 4],
    [ax, ay, aw, ah]: [f64; 4],
    corner: Corner,
) -> Option<Abut> {
    let y_overlap = by < ay + ah && ay < by + bh;
    let x_overlap = bx < ax + aw && ax < bx + bw;
    let beside = match corner {
        Corner::TopLeft | Corner::BottomLeft => (bx >= ax + aw).then_some(bx - (ax + aw)),
        Corner::TopRight | Corner::BottomRight => (bx + bw <= ax).then_some(ax - (bx + bw)),
    };
    if let Some(gap) = beside
        && y_overlap
    {
        return Some(Abut::Beside { gap });
    }
    let stacked = match corner {
        Corner::BottomLeft | Corner::BottomRight => (by + bh <= ay).then_some(ay - (by + bh)),
        Corner::TopLeft | Corner::TopRight => (by >= ay + ah).then_some(by - (ay + ah)),
    };
    match stacked {
        Some(gap) if x_overlap => Some(Abut::Stacked { gap }),
        _ => None,
    }
}

/// The inset box at a band and a scale when it abuts another box drawn at `a_rect` (the stacking
/// rule): along the abutting axis the box keeps the golden gap to A's near edge — Hawaii's left edge
/// follows Alaska's right edge as Alaska narrows — and along the other axis it anchors at the pane's
/// edge as `inset_box_at` does. The box stays anchored at the same side, so a smaller scale is a
/// subset of a larger one (the tool's bisection relies on it).
pub fn inset_box_beside(
    golden: [f64; 4],
    corner: Corner,
    pane: &Pane,
    s: f64,
    a_rect: [f64; 4],
    abut: Abut,
) -> [f64; 4] {
    let [x, y, w, h] = inset_box_at(golden, corner, pane, s);
    let [ax, ay, aw, ah] = a_rect;
    match abut {
        Abut::Beside { gap } => match corner {
            Corner::TopLeft | Corner::BottomLeft => [ax + aw + gap, y, w, h],
            Corner::TopRight | Corner::BottomRight => [ax - gap - w, y, w, h],
        },
        Abut::Stacked { gap } => match corner {
            Corner::BottomLeft | Corner::BottomRight => [x, ay - gap - h, w, h],
            Corner::TopLeft | Corner::TopRight => [x, ay + ah + gap, w, h],
        },
    }
}

/// A golden row's box moved to another corner, keeping the gaps it has to its own corner's two
/// edges (Step 0's corner table did the same): the tool's corner table tries each of TL, TR and
/// BL this way. `to == from` is the rect itself.
pub fn inset_rect_at_corner([x, y, w, h]: [f64; 4], from: Corner, to: Corner) -> [f64; 4] {
    let g = Pane::GOLDEN;
    let gap_x = match from {
        Corner::TopLeft | Corner::BottomLeft => x,
        Corner::TopRight | Corner::BottomRight => g.width - x - w,
    };
    let gap_y = match from {
        Corner::TopLeft | Corner::TopRight => y,
        Corner::BottomLeft | Corner::BottomRight => g.height - y - h,
    };
    let nx = match to {
        Corner::TopLeft | Corner::BottomLeft => gap_x,
        Corner::TopRight | Corner::BottomRight => g.width - gap_x - w,
    };
    let ny = match to {
        Corner::TopLeft | Corner::TopRight => gap_y,
        Corner::BottomLeft | Corner::BottomRight => g.height - gap_y - h,
    };
    [nx, ny, w, h]
}

/// The smallest scale at which a `w × h` golden box still holds the minimum land area: the
/// larger of `36 / w` and `28 / h` (the strip and the pads do not scale). Hawaii's 60 × 32 needs
/// 0.875; the Azores' 92 × 52 needs 0.538.
pub fn inset_min_scale([_, _, w, h]: [f64; 4]) -> f64 {
    let [lw, lh] = INSET_MIN_LAND_PT;
    ((lw + 2.0 * INSET_PAD_PT) / w).max((lh + INSET_LABEL_PT + 2.0 * INSET_PAD_PT) / h)
}

/// A conservative width for a label at `INSET_LABEL_FONT_PT` (review P3): 0.6 em per character
/// and 0.3 em per space — upper bounds for SF Pro at text sizes — so a label this rule passes is
/// never clipped on screen. "Azores" is 28.8 pt; "Guadeloupe & Martinique" 105.6.
pub fn label_width_pt(label: &str) -> f64 {
    label
        .chars()
        .map(|c| if c == ' ' { 0.3 } else { 0.6 })
        .sum::<f64>()
        * INSET_LABEL_FONT_PT
}

/// The width a label may take inside a box `[x, y, w, h]`: the box less the two pads.
pub fn label_inner_width([_, _, w, _]: [f64; 4]) -> f64 {
    w - 2.0 * INSET_PAD_PT
}

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
    /// 1 km/pt; on the ANMITE's 328 × 178 pane the height binds: h / 138. Fails on padding
    /// once, or `min` for `max`.
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

    /// A per-band table's index: the floor is 0, the top 160, and a height outside the range has
    /// none. Fails if the subtraction goes unchecked (139 underflows: a panic in a debug build,
    /// a wrapped index in release) or the top is not bounded (301 → 161, past the table).
    #[test]
    fn band_index_is_bounded_at_both_ends() {
        assert_eq!(band_index(BAND_FLOOR - 1), None);
        assert_eq!(band_index(BAND_FLOOR), Some(0));
        assert_eq!(band_index(BAND_MAX), Some(160));
        assert_eq!(band_index(BAND_MAX + 1), None);
    }

    /// The band at 300 is the golden pane; at the floor its usable area is 288 × 100, so a
    /// 288 × 260 km country fits at 2.6 km/pt there against 1.0 at 300 (fails with the padding
    /// or the width hand-typed differently from `GOLDEN`'s).
    #[test]
    fn the_band_at_300_is_the_golden_pane() {
        assert_eq!(Pane::band(BAND_MAX), Pane::GOLDEN);
        assert_eq!(Pane::band(BAND_FLOOR).usable(), (288.0, 100.0));
        assert_eq!(fit_scale(288.0, 260.0, &Pane::band(BAND_FLOOR)), Some(2.6));
        assert!((BAND_FLOOR..=BAND_MAX).all(|h| Pane::band(h).is_valid()));
        assert_eq!(Pane::band(178).height, 178.0);
    }

    /// The S1 floor: a fit finer than 1.5 km/pt starts at 1.5, a coarser one at itself.
    #[test]
    fn initial_scale_floor() {
        assert_eq!(initial_scale(0.102), 1.5);
        assert_eq!(initial_scale(2.2187), 2.2187);
    }

    /// The Azores' box on the style page, 92 × 52 at (10, 24): the land area is 84 × 36 pt,
    /// centred 4 pt below the top and above the 8 pt label strip. Fails with the strip dropped.
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

    /// The one box rule (review 3, finding 4). `box_fits`: a box on the pane's edge fits, 0.01 pt
    /// past any edge does not (fails on a strict compare, or with a margin — the clip margin,
    /// say — allowed past the edge). `boxes_apart`: boxes touching along an edge or at a corner
    /// are apart, boxes sharing 0.01 pt are not (fails on `<` for `<=`). Both read like the tool's
    /// and the frame's former inline copies, which is the point.
    #[test]
    fn the_one_box_rule() {
        let pane = Pane {
            width: 328.0,
            height: 178.0,
            padding: 20.0,
        };
        assert!(box_fits([0.0, 0.0, 328.0, 178.0], &pane));
        assert!(box_fits([248.0, 118.0, 80.0, 60.0], &pane));
        assert!(!box_fits([-0.01, 0.0, 80.0, 60.0], &pane));
        assert!(!box_fits([0.0, -0.01, 80.0, 60.0], &pane));
        assert!(!box_fits([248.01, 0.0, 80.0, 60.0], &pane));
        assert!(!box_fits([0.0, 118.01, 80.0, 60.0], &pane));
        assert!(
            !box_fits([0.0, 0.0, 330.0, 10.0], &pane),
            "the clip margin is not the pane"
        );
        assert!(!box_fits([f64::NAN, 0.0, 1.0, 1.0], &pane));

        let a = [8.0, 8.0, 80.0, 60.0];
        assert!(
            boxes_apart(a, [88.0, 8.0, 80.0, 60.0]),
            "touching along an edge"
        );
        assert!(boxes_apart(a, [8.0, 68.0, 80.0, 60.0]));
        assert!(
            boxes_apart(a, [88.0, 68.0, 80.0, 60.0]),
            "touching at a corner"
        );
        assert!(boxes_apart([88.0, 8.0, 80.0, 60.0], a), "symmetric");
        assert!(!boxes_apart(a, [87.99, 8.0, 80.0, 60.0]));
        assert!(!boxes_apart(a, [8.0, 67.99, 80.0, 60.0]));
        assert!(
            !boxes_apart(a, [20.0, 20.0, 10.0, 10.0]),
            "one inside the other"
        );
        assert!(!boxes_apart(a, a));
    }

    /// The controls' row (C1, review P5): 74 × 24 pt, 8 pt from the pane's bottom and right
    /// edges, inside the pane at 178 and 300 — `[246, 146, 74, 24]` and `[246, 268, 74, 24]`
    /// (fails with either margin dropped or the size transposed).
    #[test]
    fn the_controls_rect_is_bottom_right() {
        for (h, want) in [
            (178.0, [246.0, 146.0, 74.0, 24.0]),
            (300.0, [246.0, 268.0, 74.0, 24.0]),
        ] {
            let pane = Pane {
                width: 328.0,
                height: h,
                padding: 20.0,
            };
            let r = controls_rect(&pane);
            assert_eq!(r, want, "at {h}");
            assert!(box_fits(r, &pane));
            assert_eq!(pane.width - (r[0] + r[2]), CONTROLS_MARGIN_PT);
            assert_eq!(pane.height - (r[1] + r[3]), CONTROLS_MARGIN_PT);
        }
    }

    /// I1's box (M4b commit 4): at the golden pane and `s = 1` the row's rect; at 328 × 178 a
    /// bottom-left box keeps its 8 pt bottom gap (Alaska's `[8, 236, 84, 56]` → y 114) and a
    /// top-right one its right gap; at `s = 0.5` the size halves and the gaps stay (fails with the
    /// gaps scaled, the size unscaled, or a corner's axis mixed up). The minimum scale: Hawaii's
    /// 60 × 32 needs 0.875 (the height binds), the Azores' 92 × 52 0.538 (the height again:
    /// 28 / 52), an 80 × 60 box 0.467 (fails with the strip or a pad scaled, or `min` for `max`;
    /// the moved row fails with the rect not moved).
    #[test]
    fn the_inset_box_at_a_band_and_a_scale() {
        let anmite = Pane::band(178);
        let alaska = [8.0, 236.0, 84.0, 56.0];
        assert_eq!(
            inset_box_at(alaska, Corner::BottomLeft, &Pane::GOLDEN, 1.0),
            alaska
        );
        assert_eq!(
            inset_box_at(alaska, Corner::BottomLeft, &anmite, 1.0),
            [8.0, 114.0, 84.0, 56.0]
        );
        assert_eq!(
            inset_box_at(alaska, Corner::BottomLeft, &anmite, 0.5),
            [8.0, 142.0, 42.0, 28.0]
        );
        let gm = [260.0, 8.0, 60.0, 44.0];
        assert_eq!(
            inset_box_at(gm, Corner::TopRight, &anmite, 0.5),
            [290.0, 8.0, 30.0, 22.0]
        );
        // a row moved to another corner keeps its own gaps (8 right, 8 top): Guadeloupe's box at
        // BL is [8, 248, 60, 44] on the golden pane and, anchored there at 178, [8, 126, 60, 44]
        let gm_bl = inset_rect_at_corner(gm, Corner::TopRight, Corner::BottomLeft);
        assert_eq!(gm_bl, [8.0, 248.0, 60.0, 44.0]);
        assert_eq!(
            inset_box_at(gm_bl, Corner::BottomLeft, &anmite, 1.0),
            [8.0, 126.0, 60.0, 44.0]
        );
        assert_eq!(
            inset_rect_at_corner(alaska, Corner::BottomLeft, Corner::TopRight),
            [236.0, 8.0, 84.0, 56.0]
        );
        assert_eq!(
            inset_rect_at_corner(gm, Corner::TopRight, Corner::TopRight),
            gm
        );
        assert_eq!(
            inset_box_at([8.0, 8.0, 80.0, 60.0], Corner::TopLeft, &anmite, 0.75),
            [8.0, 8.0, 60.0, 45.0]
        );
        assert!((inset_min_scale([98.0, 258.0, 60.0, 32.0]) - 0.875).abs() < 1e-12);
        assert!((inset_min_scale([10.0, 24.0, 92.0, 52.0]) - 28.0 / 52.0).abs() < 1e-12);
        assert!((inset_min_scale([8.0, 8.0, 80.0, 60.0]) - 28.0 / 60.0).abs() < 1e-12);
        // a box at its minimum scale holds exactly the minimum land area
        let r = inset_box_at(
            [98.0, 258.0, 60.0, 32.0],
            Corner::BottomLeft,
            &anmite,
            0.875,
        );
        let (_, _, aw, ah) = inset_area(r);
        assert!(aw >= INSET_MIN_LAND_PT[0] - 1e-9 && (ah - INSET_MIN_LAND_PT[1]).abs() < 1e-9);
    }

    /// The stacking rule (M4b commit 4b): Hawaii is beside Alaska at the bottom-left with a 6 pt
    /// gap and Madeira under the Azores at the top-left with 8; boxes at different corners, or
    /// diagonal, do not abut. With Alaska at 83 % at 328 × 178 (`[8, 117.52, 69.72, 46.48]`)
    /// Hawaii at 100 % sits at x 83.72 (Alaska's right edge + 6), its bottom 10 pt up as before;
    /// Madeira under a shrunken Azores box follows its bottom + 8. At 100 % and the golden pane the
    /// stacked box is the row's own rect (fails with the gap dropped, A's far edge taken for its
    /// near one, the y-overlap check dropped, or the other axis re-anchored).
    #[test]
    fn the_stacking_rule() {
        let alaska = [8.0, 236.0, 84.0, 56.0];
        let hawaii = [98.0, 258.0, 60.0, 32.0];
        let azores = [10.0, 24.0, 92.0, 52.0];
        let madeira = [10.0, 84.0, 52.0, 40.0];
        assert_eq!(
            abuts(hawaii, alaska, Corner::BottomLeft),
            Some(Abut::Beside { gap: 6.0 })
        );
        assert_eq!(
            abuts(madeira, azores, Corner::TopLeft),
            Some(Abut::Stacked { gap: 8.0 })
        );
        assert_eq!(
            abuts(alaska, hawaii, Corner::BottomLeft),
            None,
            "A does not abut B"
        );
        assert_eq!(abuts(azores, madeira, Corner::TopLeft), None);
        assert_eq!(
            abuts(hawaii, alaska, Corner::TopRight),
            None,
            "at a right corner B would have to lie left of A"
        );
        assert_eq!(
            abuts(
                [8.0, 8.0, 60.0, 44.0],
                [260.0, 8.0, 60.0, 44.0],
                Corner::TopRight
            ),
            Some(Abut::Beside { gap: 192.0 })
        );
        assert_eq!(
            abuts(
                [100.0, 100.0, 20.0, 20.0],
                [8.0, 236.0, 84.0, 56.0],
                Corner::BottomLeft
            ),
            None,
            "diagonal"
        );
        let anmite = Pane::band(178);
        let a178 = inset_box_at(alaska, Corner::BottomLeft, &anmite, 0.83);
        assert!((a178[0] - 8.0).abs() < 1e-9 && (a178[2] - 69.72).abs() < 1e-9);
        let h = inset_box_beside(
            hawaii,
            Corner::BottomLeft,
            &anmite,
            1.0,
            a178,
            Abut::Beside { gap: 6.0 },
        );
        assert!(
            (h[0] - 83.72).abs() < 1e-9 && (h[1] - 136.0).abs() < 1e-9,
            "{h:?}"
        );
        assert_eq!((h[2], h[3]), (60.0, 32.0));
        let az = inset_box_at(azores, Corner::TopLeft, &Pane::GOLDEN, 0.5);
        let m = inset_box_beside(
            madeira,
            Corner::TopLeft,
            &Pane::GOLDEN,
            1.0,
            az,
            Abut::Stacked { gap: 8.0 },
        );
        assert_eq!(m, [10.0, 24.0 + 26.0 + 8.0, 52.0, 40.0]);
        // at 100 % and the golden pane the rule gives the row's own rect
        let a300 = inset_box_at(alaska, Corner::BottomLeft, &Pane::GOLDEN, 1.0);
        assert_eq!(
            inset_box_beside(
                hawaii,
                Corner::BottomLeft,
                &Pane::GOLDEN,
                1.0,
                a300,
                Abut::Beside { gap: 6.0 }
            ),
            hawaii
        );
        assert_eq!(
            inset_box_beside(
                madeira,
                Corner::TopLeft,
                &Pane::GOLDEN,
                1.0,
                inset_box_at(azores, Corner::TopLeft, &Pane::GOLDEN, 1.0),
                Abut::Stacked { gap: 8.0 }
            ),
            madeira
        );
    }

    /// P3's label metric at the artifact's 8 pt: 0.6 em a character, 0.3 em a space (fails with
    /// a space counted as a character, or the size assumed at 9 or 10 pt), and the inner width is
    /// the box less two pads.
    #[test]
    fn label_widths_at_the_artifacts_size() {
        assert_eq!(INSET_LABEL_FONT_PT, 8.0);
        assert!((label_width_pt("Azores") - 28.8).abs() < 1e-9);
        assert!((label_width_pt("Guadeloupe & Martinique") - 105.6).abs() < 1e-9);
        assert!((label_width_pt("Sabah & Sarawak") - (13.0 * 4.8 + 2.0 * 2.4)).abs() < 1e-9);
        assert_eq!(label_width_pt(""), 0.0);
        assert_eq!(label_inner_width([0.0, 0.0, 60.0, 44.0]), 52.0);
    }

    /// The coarsest level at or below the scale: RU's 28.01 → 24, 12.0 → 12, 11.99 → 6. Fails on
    /// `<` for `≤`.
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
