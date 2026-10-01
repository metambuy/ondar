//! Geometry helpers on `geo` types: projection, bounds, caps, point–segment distances.

use geo::{Coord, LineString, Polygon};
use ondar_map::laea::{Laea, lon_midpoint, wrap_lon};

/// Every ring of a polygon, exterior first.
pub fn rings(p: &Polygon<f64>) -> impl Iterator<Item = &LineString<f64>> {
    std::iter::once(p.exterior()).chain(p.interiors().iter())
}

pub fn vertices(ps: &[Polygon<f64>]) -> usize {
    ps.iter().flat_map(rings).map(|r| r.0.len()).sum()
}

/// R1's centre for a set of lon/lat polygons: the midpoint of their antimeridian-aware lon/lat
/// bbox, from the exterior rings' vertices. `(lat, lon)`.
pub fn lonlat_centre<'a>(ps: impl IntoIterator<Item = &'a Polygon<f64>>) -> Option<(f64, f64)> {
    let mut lons = Vec::new();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in ps {
        for c in &p.exterior().0 {
            lons.push(wrap_lon(c.x));
            lo = lo.min(c.y);
            hi = hi.max(c.y);
        }
    }
    let lon = lon_midpoint(&mut lons)?;
    Some(((lo + hi) / 2.0, lon))
}

pub fn project_ring(r: &LineString<f64>, l: &Laea) -> LineString<f64> {
    LineString::from(
        r.0.iter()
            .filter_map(|c| l.fwd(c.x, c.y).map(|[x, y]| Coord { x, y }))
            .collect::<Vec<_>>(),
    )
}

pub fn project(p: &Polygon<f64>, l: &Laea) -> Polygon<f64> {
    Polygon::new(
        project_ring(p.exterior(), l),
        p.interiors().iter().map(|r| project_ring(r, l)).collect(),
    )
}

/// `[min_x, min_y, max_x, max_y]` of the exterior rings.
pub fn bbox<'a>(ps: impl IntoIterator<Item = &'a Polygon<f64>>) -> Option<[f64; 4]> {
    let mut b: Option<[f64; 4]> = None;
    for p in ps {
        for c in &p.exterior().0 {
            b = Some(match b {
                None => [c.x, c.y, c.x, c.y],
                Some([a, bb, cc, d]) => [a.min(c.x), bb.min(c.y), cc.max(c.x), d.max(c.y)],
            });
        }
    }
    b
}

pub fn seg_dist(p: Coord<f64>, a: Coord<f64>, b: Coord<f64>) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.x - a.x - t * dx).hypot(p.y - a.y - t * dy)
}

/// Whether segments `a`–`b` and `c`–`d` intersect (touching counts).
pub fn segments_cross(a: Coord<f64>, b: Coord<f64>, c: Coord<f64>, d: Coord<f64>) -> bool {
    let o = |p: Coord<f64>, q: Coord<f64>, r: Coord<f64>| {
        ((q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)).signum()
    };
    let (o1, o2, o3, o4) = (o(a, b, c), o(a, b, d), o(c, d, a), o(c, d, b));
    if o1 != o2 && o3 != o4 {
        return true;
    }
    let on = |p: Coord<f64>, q: Coord<f64>, r: Coord<f64>| seg_dist(r, p, q) == 0.0;
    on(a, b, c) || on(a, b, d) || on(c, d, a) || on(c, d, b)
}

/// The distance (same units) from a rectangle `[x0, y0, x1, y1]` to a ring; 0 if they meet or
/// one holds the other.
pub fn rect_ring_distance(rect: [f64; 4], ring: &[Coord<f64>]) -> f64 {
    let [x0, y0, x1, y1] = rect;
    let corners = [
        Coord { x: x0, y: y0 },
        Coord { x: x1, y: y0 },
        Coord { x: x1, y: y1 },
        Coord { x: x0, y: y1 },
    ];
    let inside_rect = |p: &Coord<f64>| p.x >= x0 && p.x <= x1 && p.y >= y0 && p.y <= y1;
    if ring.iter().any(inside_rect) {
        return 0.0;
    }
    // a corner inside the ring (even-odd)
    let in_ring = |p: Coord<f64>| {
        let mut inside = false;
        for w in ring.windows(2) {
            if let [a, b] = w
                && (a.y > p.y) != (b.y > p.y)
                && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x)
            {
                inside = !inside;
            }
        }
        inside
    };
    if corners.iter().any(|&c| in_ring(c)) {
        return 0.0;
    }
    let mut best = f64::INFINITY;
    for w in ring.windows(2) {
        let [a, b] = [w[0], w[1]];
        for i in 0..4 {
            let (c, d) = (corners[i], corners[(i + 1) % 4]);
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

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    #[test]
    fn rect_ring_distances() {
        let sq = |x: f64, y: f64| {
            vec![
                c(x, y),
                c(x + 1.0, y),
                c(x + 1.0, y + 1.0),
                c(x, y + 1.0),
                c(x, y),
            ]
        };
        assert_eq!(rect_ring_distance([0.0, 0.0, 2.0, 2.0], &sq(5.0, 0.5)), 3.0);
        assert_eq!(rect_ring_distance([0.0, 0.0, 2.0, 2.0], &sq(1.5, 1.5)), 0.0);
        // a ring around the rectangle
        let big = vec![
            c(-10.0, -10.0),
            c(10.0, -10.0),
            c(10.0, 10.0),
            c(-10.0, 10.0),
            c(-10.0, -10.0),
        ];
        assert_eq!(rect_ring_distance([0.0, 0.0, 2.0, 2.0], &big), 0.0);
        // diagonal: corner (2,2) to (5,6) = 5
        assert_eq!(rect_ring_distance([0.0, 0.0, 2.0, 2.0], &sq(5.0, 6.0)), 5.0);
    }

    #[test]
    fn lonlat_centre_across_the_antimeridian() {
        let p = Polygon::new(
            LineString::from(vec![
                (170.0, -20.0),
                (-170.0, -20.0),
                (-170.0, -10.0),
                (170.0, -10.0),
            ]),
            vec![],
        );
        let (lat, lon) = lonlat_centre([&p]).unwrap();
        assert_eq!((lat, lon), (-15.0, 180.0));
    }
}
