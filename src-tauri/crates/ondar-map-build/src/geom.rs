//! Geometry helpers on `geo` types: projection, bounds, caps, point–segment distances.

use geo::{Coord, LineString, Polygon};
use ondar_map::format::Cap;
use ondar_map::laea::{Laea, haversine_km, lon_midpoint, wrap_lon};
use ondar_map::rules;

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

// S6's distance and its two helpers live in `ondar_map::rules` (one copy for the tool and the
// frame, review finding 7); these adapt `geo::Coord` for the simplifier and the border census.
// `world.rs` measures its clearance on `[f64; 2]` rings and calls `rules::rect_ring_distance`
// itself (review 3, finding 5).
fn xy(c: Coord<f64>) -> [f64; 2] {
    [c.x, c.y]
}

pub fn seg_dist(p: Coord<f64>, a: Coord<f64>, b: Coord<f64>) -> f64 {
    rules::seg_dist(xy(p), xy(a), xy(b))
}

/// Whether segments `a`–`b` and `c`–`d` intersect (touching counts).
pub fn segments_cross(a: Coord<f64>, b: Coord<f64>, c: Coord<f64>, d: Coord<f64>) -> bool {
    rules::segments_cross(xy(a), xy(b), xy(c), xy(d))
}

/// A cap around lon/lat points: centred on their R1 centre, radius the farthest point (km).
pub fn cap<'a>(points: impl IntoIterator<Item = &'a Coord<f64>> + Clone) -> Option<Cap> {
    let mut lons: Vec<f64> = points.clone().into_iter().map(|c| wrap_lon(c.x)).collect();
    let (lo, hi) = points
        .clone()
        .into_iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| {
            (lo.min(c.y), hi.max(c.y))
        });
    let lon = lon_midpoint(&mut lons)?;
    let lat = (lo + hi) / 2.0;
    let r = points
        .into_iter()
        .map(|c| haversine_km(lon, lat, c.x, c.y))
        .fold(0.0, f64::max);
    Some(Cap {
        lon: lon as f32,
        lat: lat as f32,
        // f32 rounding of the centre moves it by < 1 m; the margin covers it and the straight
        // projected edges between vertices
        radius_km: (r * 1.001 + 0.01) as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
