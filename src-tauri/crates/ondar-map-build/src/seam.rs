//! R9 (extended): the antimeridian seam, removed before projection.
//!
//! Natural Earth cuts every unit at ±180°. Antarctica's ring also runs down the seam to the pole,
//! along it at −90° (724 vertices) and back up: in a pole-centred LAEA that is a zero-width slit,
//! and VW's removal there moves a vertex 628 km (Step 0, Q2b). So: Antarctica's polar run is
//! dropped, leaving the coast ring; every other unit with parts on the seam has those parts'
//! western halves shifted by +360° and all of them unioned, so no ring keeps an edge along it.

use geo::{BooleanOps, Coord, LineString, MultiPolygon, Polygon};

/// A longitude on the seam, either side.
pub fn on_seam(lon: f64) -> bool {
    (ondar_map::laea::wrap_lon(lon).abs() - 180.0).abs() <= 1e-6
}

/// A vertex of Antarctica's polar run: at the pole, or on the seam.
fn polar(c: &Coord<f64>) -> bool {
    c.y <= -89.99 || on_seam(c.x)
}

fn touches(p: &Polygon<f64>) -> bool {
    crate::geom::rings(p).any(|r| r.0.iter().any(|c| on_seam(c.x)))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stitch {
    pub parts_in: usize,
    pub parts_out: usize,
    pub seam_parts: usize,
    pub removed_vertices: usize,
    /// Edges along the seam left by the union where the two halves' coasts meet 180° at
    /// slightly different latitudes, closed by dropping one of their two vertices.
    pub notches: usize,
    /// The longest such edge, km.
    pub notch_max_km: f64,
}

/// Antarctica: drop every vertex of the polar run from each ring that reaches the pole.
pub fn strip_polar(parts: Vec<Polygon<f64>>) -> (Vec<Polygon<f64>>, Stitch) {
    let mut st = Stitch {
        parts_in: parts.len(),
        ..Stitch::default()
    };
    let strip = |r: &LineString<f64>, removed: &mut usize| -> LineString<f64> {
        if !r.0.iter().any(|c| c.y <= -89.99) {
            return r.clone();
        }
        let mut kept: Vec<Coord<f64>> = r.0.iter().filter(|c| !polar(c)).copied().collect();
        *removed += r.0.len() - kept.len();
        // the ring was closed through the run; close it again
        if let (Some(&f), Some(&l)) = (kept.first(), kept.last())
            && f != l
        {
            kept.push(f);
        }
        LineString::from(kept)
    };
    let out: Vec<Polygon<f64>> = parts
        .iter()
        .map(|p| {
            if touches(p) || p.exterior().0.iter().any(|c| c.y <= -89.99) {
                st.seam_parts += 1;
            }
            Polygon::new(
                strip(p.exterior(), &mut st.removed_vertices),
                p.interiors()
                    .iter()
                    .map(|r| strip(r, &mut st.removed_vertices))
                    .collect(),
            )
        })
        .collect();
    st.parts_out = out.len();
    (out, st)
}

/// Shifts a part lying west of the seam (its mean longitude negative) by +360°.
pub fn shift_east(p: &Polygon<f64>) -> Polygon<f64> {
    let n = p.exterior().0.len().max(1) as f64;
    if p.exterior().0.iter().map(|c| c.x).sum::<f64>() / n >= 0.0 {
        return p.clone();
    }
    let sh = |r: &LineString<f64>| {
        LineString::from(
            r.0.iter()
                .map(|c| Coord {
                    x: c.x + 360.0,
                    y: c.y,
                })
                .collect::<Vec<_>>(),
        )
    };
    Polygon::new(sh(p.exterior()), p.interiors().iter().map(sh).collect())
}

/// Every other unit: the parts on the seam, shifted east and unioned. Parts not on the seam are
/// returned unchanged and in their order; the union's parts follow them.
pub fn union_seam(parts: Vec<Polygon<f64>>) -> (Vec<Polygon<f64>>, Stitch) {
    let mut st = Stitch {
        parts_in: parts.len(),
        ..Stitch::default()
    };
    let (seam, mut out): (Vec<Polygon<f64>>, Vec<Polygon<f64>>) =
        parts.into_iter().partition(touches);
    st.seam_parts = seam.len();
    if !seam.is_empty() {
        let shifted = MultiPolygon::new(seam.iter().map(shift_east).collect());
        let empty = MultiPolygon::<f64>::new(vec![]);
        let u = shifted.union(&empty);
        for p in u.0 {
            let mut fix = |r: &LineString<f64>| close_notches(r, &mut st);
            let ext = fix(p.exterior());
            let ints = p.interiors().iter().map(&mut fix).collect();
            out.push(Polygon::new(ext, ints));
        }
    }
    st.parts_out = out.len();
    (out, st)
}

/// Drops the first vertex of every edge along the seam, so the coast closes across 180° instead
/// of running along it for the few metres between the halves' endpoints.
fn close_notches(r: &LineString<f64>, st: &mut Stitch) -> LineString<f64> {
    let mut v: Vec<Coord<f64>> = r.0.clone();
    if v.first() == v.last() {
        v.pop();
    }
    loop {
        let n = v.len();
        let Some(i) = (0..n).find(|&i| on_seam(v[i].x) && on_seam(v[(i + 1) % n].x)) else {
            break;
        };
        if n <= 3 {
            break;
        }
        let (a, b) = (v[i], v[(i + 1) % n]);
        st.notches += 1;
        st.notch_max_km = st
            .notch_max_km
            .max(ondar_map::laea::haversine_km(a.x, a.y, b.x, b.y));
        v.remove(i);
        st.removed_vertices += 1;
    }
    if let Some(&f) = v.first() {
        v.push(f);
    }
    LineString::from(v)
}

/// The R9 check: no ring has an edge with both ends on the seam.
pub fn seam_edges(parts: &[Polygon<f64>]) -> usize {
    parts
        .iter()
        .flat_map(crate::geom::rings)
        .map(|r| {
            r.0.windows(2)
                .filter(|w| matches!(w, [a, b] if on_seam(a.x) && on_seam(b.x)))
                .count()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(v: &[(f64, f64)]) -> LineString<f64> {
        LineString::from(v.to_vec())
    }

    /// An Antarctica-shaped ring: a coast from 180° W to 180° E along −70°, then down the seam
    /// to the pole, along the pole, and back up. The run goes; the coast stays, closed, with no
    /// seam edge. Fails with the strip off (three seam edges and the pole remain).
    #[test]
    fn the_polar_run_is_stripped() {
        let mut v = vec![(-180.0, -70.0)];
        for lon in (-170..=170).step_by(10) {
            v.push((f64::from(lon), -70.0));
        }
        v.extend([
            (180.0, -70.0),
            (180.0, -80.0),
            (180.0, -90.0),
            (90.0, -90.0),
            (0.0, -90.0),
            (-90.0, -90.0),
            (-180.0, -90.0),
            (-180.0, -80.0),
            (-180.0, -70.0),
        ]);
        let aq = vec![Polygon::new(ring(&v), vec![])];
        assert!(seam_edges(&aq) > 0);
        let (out, st) = strip_polar(aq);
        let r = &out[0].exterior().0;
        assert_eq!(seam_edges(&out), 0);
        assert!(r.iter().all(|c| c.y > -89.99 && !on_seam(c.x)));
        assert_eq!(r.first(), r.last());
        assert_eq!(r.len(), 35 + 1);
        assert_eq!(st.removed_vertices, 10);
    }

    /// The halves' coasts meet 180° 0.001° apart (as Fiji's do, by up to 0.0014°): the union
    /// leaves a notch along the seam, which is closed. Fails with the notch kept.
    #[test]
    fn a_notch_along_the_seam_is_closed() {
        let east = Polygon::new(
            ring(&[
                (178.0, 65.0),
                (180.0, 65.0),
                (180.0, 67.0),
                (178.0, 67.0),
                (178.0, 65.0),
            ]),
            vec![],
        );
        let west = Polygon::new(
            ring(&[
                (-180.0, 65.0),
                (-178.0, 65.0),
                (-178.0, 67.001),
                (-180.0, 67.001),
                (-180.0, 65.0),
            ]),
            vec![],
        );
        let (out, st) = union_seam(vec![east, west]);
        assert_eq!(out.len(), 1);
        assert_eq!(seam_edges(&out), 0);
        assert_eq!(st.notches, 1);
        assert!(
            (st.notch_max_km - 0.111).abs() < 0.01,
            "{}",
            st.notch_max_km
        );
    }

    /// Two halves of an island cut at 180°: the union is one part with no seam edge, its
    /// western half moved to 180°–190°. Fails with the shift off (no shared edge to union).
    #[test]
    fn the_halves_are_unioned() {
        let east = Polygon::new(
            ring(&[
                (178.0, 65.0),
                (180.0, 65.0),
                (180.0, 67.0),
                (178.0, 67.0),
                (178.0, 65.0),
            ]),
            vec![],
        );
        let west = Polygon::new(
            ring(&[
                (-180.0, 65.0),
                (-178.0, 65.0),
                (-178.0, 67.0),
                (-180.0, 67.0),
                (-180.0, 65.0),
            ]),
            vec![],
        );
        let away = Polygon::new(
            ring(&[(100.0, 60.0), (101.0, 60.0), (101.0, 61.0), (100.0, 60.0)]),
            vec![],
        );
        let (out, st) = union_seam(vec![east, away.clone(), west]);
        assert_eq!(st.seam_parts, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], away);
        assert_eq!(seam_edges(&out), 0);
        let xs: Vec<f64> = out[1].exterior().0.iter().map(|c| c.x).collect();
        assert!(xs.iter().all(|&x| (178.0..=182.0).contains(&x)), "{xs:?}");
    }
}
