//! Stations on the ground (M4c): Q4's gathering (`m4-step0/proto/src/q4.rs`) and R6's locate.
//!
//! A country's stations with coordinates are gathered into dots on the ground, 10 km apart at
//! the least, densest first; each dot is then located, once, against its country's land (S3:
//! per dot, not per station). A dot whose centroid is more than 25 km outside every part of its
//! country is not drawn and its stations are counted outside (R6). The frame places the located
//! dots (`Store::frame_dots`). No Tauri and no stations dependency: the shell turns a station
//! list into `Point`s.

use crate::format::{Layer, Role, Store};
use crate::laea::{Laea, haversine_km};
use crate::rules::{in_ring, ring_edges, seg_dist};

/// Q4's merge radius: a point gathers every remaining point within this distance on the ground.
pub const GATHER_KM: f64 = 10.0;
/// R6: a dot farther than this from every part of its country is outside, not drawn.
pub const OUTSIDE_KM: f64 = 25.0;
/// The level `locate` reads for the country's own units: 6 km/pt, bound 1.5 km, inside R6's
/// 25 km by far. A unit whose top level is finer (PT's is 0) stores no level 2; it reads the
/// highest stored level below it (S3).
pub const LOCATE_LEVEL: u8 = 2;

/// One station, as the shell hands it over: its uuid, radio-browser's `geo` (lat, lon) if any,
/// and its `state` (the dot's place).
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub id: String,
    pub geo: Option<(f64, f64)>,
    pub place: String,
}

/// A gathered group, before it is located.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub lat: f64,
    pub lon: f64,
    /// The members' ids, in list order.
    pub ids: Vec<String>,
    /// The members' majority non-empty place; ties to the first in list order; empty when every
    /// member's is.
    pub place: String,
}

/// The part of the country a dot is located in, or nearest to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartRef {
    pub unit: u16,
    /// The part's index in the unit's `parts`.
    pub part: u16,
    pub role: Role,
}

/// A located dot: a group within `OUTSIDE_KM` of its country, and the part that holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct GroundDot {
    pub lat: f64,
    pub lon: f64,
    pub ids: Vec<String>,
    pub place: String,
    pub part: PartRef,
}

/// A country's stations on the ground: the located dots, larger first, and the counts the frame
/// reports (`FrameStats`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gathered {
    pub dots: Vec<GroundDot>,
    /// Stations in dots more than `OUTSIDE_KM` outside every part (R6).
    pub outside: usize,
    /// Stations in the located dots.
    pub located: usize,
    /// Every station given, with coordinates or not.
    pub total: usize,
}

/// Q4's rule: among the remaining points with coordinates, the one with the most remaining
/// neighbours within `km` (ties: the lowest id) takes them all as one group, at their
/// unit-vector centroid; repeat until none remains. Members in input (list) order; groups by
/// size, larger first, then by their first member's id. Points without `geo` are skipped.
pub fn gather(points: &[Point], km: f64) -> Vec<Group> {
    let pts: Vec<(&Point, f64, f64)> = points
        .iter()
        .filter_map(|p| p.geo.map(|(lat, lon)| (p, lat, lon)))
        .collect();
    let n = pts.len();
    let mut nb: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, &(_, la, lo)) in pts.iter().enumerate() {
        for (j, &(_, lb, lob)) in pts.iter().enumerate().skip(i + 1) {
            if haversine_km(lo, la, lob, lb) <= km {
                if let Some(v) = nb.get_mut(i) {
                    v.push(j);
                }
                if let Some(v) = nb.get_mut(j) {
                    v.push(i);
                }
            }
        }
    }
    // the tie rule: candidates in id order, the first with the most neighbours wins
    let mut by_id: Vec<usize> = (0..n).collect();
    by_id.sort_by(|&a, &b| id_of(&pts, a).cmp(id_of(&pts, b)).then(a.cmp(&b)));
    let mut alive = vec![true; n];
    let is_alive = |alive: &[bool], i: usize| alive.get(i).copied().unwrap_or(false);
    let mut groups = Vec::new();
    loop {
        let mut best: Option<(usize, usize)> = None;
        for &i in &by_id {
            if !is_alive(&alive, i) {
                continue;
            }
            let c = nb
                .get(i)
                .map_or(0, |v| v.iter().filter(|&&j| is_alive(&alive, j)).count());
            if best.is_none_or(|(bc, _)| c > bc) {
                best = Some((c, i));
            }
        }
        let Some((_, i)) = best else { break };
        let mut mem: Vec<usize> = std::iter::once(i)
            .chain(
                nb.get(i)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&j| is_alive(&alive, j)),
            )
            .collect();
        mem.sort_unstable();
        let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
        let mut ids = Vec::with_capacity(mem.len());
        let mut places: Vec<&str> = Vec::with_capacity(mem.len());
        for &m in &mem {
            if let Some(a) = alive.get_mut(m) {
                *a = false;
            }
            let Some(&(p, lat, lon)) = pts.get(m) else {
                continue;
            };
            let (la, lo) = (lat.to_radians(), lon.to_radians());
            x += la.cos() * lo.cos();
            y += la.cos() * lo.sin();
            z += la.sin();
            ids.push(p.id.clone());
            places.push(p.place.as_str());
        }
        groups.push(Group {
            lat: z.atan2(x.hypot(y)).to_degrees(),
            lon: y.atan2(x).to_degrees(),
            ids,
            place: majority(&places).to_string(),
        });
    }
    groups.sort_by(|a, b| {
        b.ids
            .len()
            .cmp(&a.ids.len())
            .then_with(|| a.ids.first().cmp(&b.ids.first()))
    });
    groups
}

fn id_of<'a>(pts: &[(&'a Point, f64, f64)], i: usize) -> &'a str {
    pts.get(i).map_or("", |&(p, _, _)| p.id.as_str())
}

/// The most frequent non-empty place; ties to the one met first; "" when none is non-empty.
fn majority<'a>(places: &[&'a str]) -> &'a str {
    let mut best = ("", 0usize);
    for (i, &p) in places.iter().enumerate() {
        if p.is_empty() || places.iter().take(i).any(|&q| q == p) {
            continue;
        }
        let c = places.iter().filter(|&&q| q == p).count();
        if c > best.1 {
            best = (p, c);
        }
    }
    best.0
}

/// Where a point lies against its country: the part holding it (`km` 0) or the nearest part and
/// its distance, km on the ground in that part's unit LAEA. `part` is `None` only for a country
/// with no own land at the locate level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Located {
    pub part: Option<PartRef>,
    pub km: f64,
}

/// A country's own land at the locate level, decoded once (S3), for any number of `locate`s.
pub struct Locator {
    units: Vec<LocUnit>,
}

struct LocUnit {
    laea: Laea,
    parts: Vec<LocPart>,
}

struct LocPart {
    part: PartRef,
    /// The exterior first, then the holes, in the unit's LAEA, km.
    rings: Vec<Vec<[f64; 2]>>,
}

impl Locator {
    /// The point's part, or the nearest part and its distance (`f64::INFINITY` with no land).
    /// Inside a part is the even-odd rule over all its rings, so a point in a hole is outside it
    /// and its distance is the hole's edge.
    pub fn locate(&self, lat: f64, lon: f64) -> Located {
        let mut best = Located {
            part: None,
            km: f64::INFINITY,
        };
        for u in &self.units {
            let Some(p) = u.laea.fwd(lon, lat) else {
                continue;
            };
            for lp in &u.parts {
                let mut inside = false;
                let mut d = f64::INFINITY;
                for r in &lp.rings {
                    inside ^= in_ring(p, r);
                    for (a, b) in ring_edges(r) {
                        d = d.min(seg_dist(p, a, b));
                    }
                }
                let km = if inside { 0.0 } else { d };
                if km < best.km {
                    best = Located {
                        part: Some(lp.part),
                        km,
                    };
                }
            }
        }
        best
    }
}

impl Store {
    /// The locator for country `c`: every part of its own units but a neighbour-only one, at
    /// `LOCATE_LEVEL` or the highest stored level below it, decoded once. An unknown country,
    /// a unit with no land blob at those levels, or a ring that does not decode adds nothing.
    pub fn locator(&self, c: usize) -> Locator {
        let mut units = Vec::new();
        let Some(ct) = self.countries.get(c) else {
            return Locator { units };
        };
        let mut buf = Vec::new();
        for &u in &ct.units {
            let Some(unit) = self.units.get(usize::from(u)) else {
                continue;
            };
            let Some(b) = (0..=LOCATE_LEVEL)
                .rev()
                .find_map(|k| self.blob(u, k, Layer::Land))
            else {
                continue;
            };
            let mut parts = Vec::new();
            let mut ri = 0usize;
            for (pi, part) in unit.parts.iter().enumerate() {
                let first = ri;
                ri += part.rings.len();
                let Ok(pi) = u16::try_from(pi) else { break };
                if part.role == Role::NeighbourOnly {
                    continue;
                }
                let mut rings = Vec::with_capacity(part.rings.len());
                for j in first..ri {
                    if self.decode(b, j, &mut buf).is_some() && !buf.is_empty() {
                        rings.push(buf.clone());
                    }
                }
                parts.push(LocPart {
                    part: PartRef {
                        unit: u,
                        part: pi,
                        role: part.role,
                    },
                    rings,
                });
            }
            units.push(LocUnit {
                laea: Laea::new(unit.lat0, unit.lon0),
                parts,
            });
        }
        Locator { units }
    }

    /// Country `c`'s stations on the ground: `Locator::gather` on its locator.
    pub fn gather_dots(&self, c: usize, points: &[Point]) -> Gathered {
        self.locator(c).gather(points)
    }
}

impl Locator {
    /// The stations on the ground: `gather` at `GATHER_KM`, then each group located once at its
    /// centroid (S3, per dot); a group more than `OUTSIDE_KM` from every part is outside (R6),
    /// its stations counted, never drawn.
    pub fn gather(&self, points: &[Point]) -> Gathered {
        let mut out = Gathered {
            total: points.len(),
            ..Gathered::default()
        };
        for g in gather(points, GATHER_KM) {
            let at = self.locate(g.lat, g.lon);
            match at.part {
                Some(part) if at.km <= OUTSIDE_KM => {
                    out.located += g.ids.len();
                    out.dots.push(GroundDot {
                        lat: g.lat,
                        lon: g.lon,
                        ids: g.ids,
                        place: g.place,
                        part,
                    });
                }
                _ => out.outside += g.ids.len(),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(id: &str, lat: f64, lon: f64, place: &str) -> Point {
        Point {
            id: id.into(),
            geo: Some((lat, lon)),
            place: place.into(),
        }
    }

    /// Degrees of latitude for `km` on the ground (the authalic sphere's meridian).
    fn dlat(km: f64) -> f64 {
        (km / crate::laea::R_AUTHALIC_KM).to_degrees()
    }

    /// The canonical form of a gathering: each group's ids sorted, the groups sorted, and the
    /// centroids to 1e-9°; equal for two inputs iff they gather into the same dots.
    fn canon(gs: &[Group]) -> Vec<(Vec<String>, i64, i64, String)> {
        let mut v: Vec<_> = gs
            .iter()
            .map(|g| {
                let mut ids = g.ids.clone();
                ids.sort();
                (
                    ids,
                    (g.lat * 1e9).round() as i64,
                    (g.lon * 1e9).round() as i64,
                    g.place.clone(),
                )
            })
            .collect();
        v.sort();
        v
    }

    /// Pins the tie rule (Q4: ties to the lowest id): a chain a–b–c–d, 8 km apart on a meridian
    /// (a–c 16 km), where b and c each have two neighbours. b, the lower id, takes a and c; d is
    /// alone. Three input orders gather into those same two dots; the members come in list
    /// order. Fails with ties broken by list position (the reversed input gathers {b, c, d} and
    /// {a}), or by the highest id.
    #[test]
    fn gather_is_deterministic_under_shuffles() {
        let chain: Vec<Point> = ["a", "b", "c", "d"]
            .iter()
            .enumerate()
            .map(|(i, id)| pt(id, 40.0 + dlat(8.0 * i as f64), -8.0, ""))
            .collect();
        let orders: [[usize; 4]; 3] = [[0, 1, 2, 3], [3, 2, 1, 0], [2, 0, 3, 1]];
        let want = {
            let g = gather(&chain, GATHER_KM);
            assert_eq!(g.len(), 2);
            assert_eq!(g[0].ids, ["a", "b", "c"]);
            assert_eq!(g[1].ids, ["d"]);
            canon(&g)
        };
        for o in orders {
            let input: Vec<Point> = o.iter().map(|&i| chain[i].clone()).collect();
            let g = gather(&input, GATHER_KM);
            assert_eq!(canon(&g), want, "order {o:?}");
            // members in the input's order
            let pos = |id: &str| input.iter().position(|p| p.id == id).unwrap();
            for grp in &g {
                assert!(grp.ids.windows(2).all(|w| pos(&w[0]) < pos(&w[1])), "{o:?}");
            }
        }
    }

    /// Pins the place rule: the members' majority non-empty place, ties to the first in list
    /// order, empty only when every member's is. Five stations within 3 km: places "", "Porto",
    /// "Braga", "Braga", "" → "Braga" (fails if the first member's place is taken: ""); a 1–1
    /// tie "Porto", "Braga" → "Porto" (fails with ties to the
    /// last); all empty → "". Stations without `geo` are not gathered.
    #[test]
    fn a_dot_s_place_is_its_members_majority() {
        let near = |id: &str, k: f64, place: &str| pt(id, 41.0 + dlat(k), -8.5, place);
        let g = gather(
            &[
                near("1", 0.0, ""),
                near("2", 0.5, "Porto"),
                near("3", 1.0, "Braga"),
                near("4", 1.5, "Braga"),
                near("5", 2.0, ""),
            ],
            GATHER_KM,
        );
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].place, "Braga");
        let g = gather(
            &[near("1", 0.0, "Porto"), near("2", 1.0, "Braga")],
            GATHER_KM,
        );
        assert_eq!(g[0].place, "Porto");
        let g = gather(&[near("1", 0.0, ""), near("2", 1.0, "")], GATHER_KM);
        assert_eq!(g[0].place, "");
        let none = Point {
            id: "0".into(),
            geo: None,
            place: "Lisboa".into(),
        };
        let g = gather(&[none, near("1", 0.0, "")], GATHER_KM);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ids, ["1"]);
        assert_eq!(g[0].place, "");
    }

    /// Pins the unit-vector centroid: five stations 0.5 km apart on a meridian gather into one
    /// dot at the middle one's latitude (to 1e-9°), not at the densest-by-id member's (the
    /// first, at 0 km: all five have four neighbours). Fails with the dot placed at its densest
    /// member, or with a centroid weighted towards either end.
    #[test]
    fn a_dot_is_at_its_members_centroid() {
        let pts: Vec<Point> = (0..5)
            .map(|i| pt(&i.to_string(), 41.0 + dlat(0.5 * f64::from(i)), -8.5, ""))
            .collect();
        let g = gather(&pts, GATHER_KM);
        assert_eq!(g.len(), 1);
        assert!((g[0].lat - (41.0 + dlat(1.0))).abs() < 1e-9, "{}", g[0].lat);
        assert!((g[0].lon + 8.5).abs() < 1e-9, "{}", g[0].lon);
    }

    /// A locator over one unit at (0°, 0°): a 100 km square around the origin with a 20 km
    /// square hole at its centre, as `Store::locator` builds it.
    fn square_locator() -> Locator {
        let sq = |h: f64| vec![[-h, -h], [h, -h], [h, h], [-h, h]];
        let part = PartRef {
            unit: 0,
            part: 0,
            role: Role::Frame,
        };
        Locator {
            units: vec![LocUnit {
                laea: Laea::new(0.0, 0.0),
                parts: vec![LocPart {
                    part,
                    rings: vec![sq(50.0), sq(10.0)],
                }],
            }],
        }
    }

    /// (lat, lon) of the unit LAEA's point (x, 0), km.
    fn at_x(x: f64) -> (f64, f64) {
        let (lon, lat) = Laea::new(0.0, 0.0).inv(x, 0.0).unwrap();
        (lat, lon)
    }

    /// Pins R6's threshold and the locate's geometry. A point 24.9 km east of the square is
    /// located (km 24.9), at 25.1 km it is outside (fails with `OUTSIDE_KM` at 24.8 or 25.2); a
    /// point in the hole is outside the part at the
    /// hole's edge, 5 km (fails with the holes ignored: km 0), and a point in the land is 0.
    #[test]
    fn a_point_25_km_outside_every_part_is_outside() {
        let loc = square_locator();
        let km = |x: f64| {
            let (lat, lon) = at_x(x);
            loc.locate(lat, lon).km
        };
        assert!((km(74.9) - 24.9).abs() < 1e-6, "{}", km(74.9));
        assert!((km(75.1) - 25.1).abs() < 1e-6, "{}", km(75.1));
        assert!(km(74.9) <= OUTSIDE_KM && km(75.1) > OUTSIDE_KM);
        assert!((km(5.0) - 5.0).abs() < 1e-6, "in the hole: {}", km(5.0));
        assert_eq!(km(30.0), 0.0);
        assert_eq!(loc.locate(at_x(30.0).0, at_x(30.0).1).part.unwrap().part, 0);
        let empty = Locator { units: Vec::new() };
        assert_eq!(empty.locate(0.0, 0.0).part, None);
        assert_eq!(empty.locate(0.0, 0.0).km, f64::INFINITY);
    }

    /// Pins S3's per-dot R6: a dot is judged at its centroid and its stations go with it. Two
    /// stations 9 km apart east of the square, at 70 and 79 km from its centre (20 and 29 km
    /// out), gather into one dot at ~24.5 km: located, both stations counted located, none
    /// outside. At 71 and 80 (centroid ~25.5 km) both are outside, though the first alone is
    /// 21 km out. Fails with R6 judged per station (1 outside in each case), or with a dot's
    /// count taken as 1 station. A station without `geo` counts in `total` only.
    #[test]
    fn r6_is_judged_per_dot() {
        let loc = square_locator();
        let east = |id: &str, x: f64| {
            let (lat, lon) = at_x(x);
            pt(id, lat, lon, "")
        };
        let none = Point {
            id: "n".into(),
            geo: None,
            place: String::new(),
        };
        let g = loc.gather(&[east("a", 70.0), east("b", 79.0), none.clone()]);
        assert_eq!((g.dots.len(), g.located, g.outside, g.total), (1, 2, 0, 3));
        assert_eq!(g.dots[0].ids, ["a", "b"]);
        let g = loc.gather(&[east("a", 71.0), east("b", 80.0), none]);
        assert_eq!((g.dots.len(), g.located, g.outside, g.total), (0, 0, 2, 3));
    }

    /// `rules::seg_dist`, which `locate` reads per edge: a degenerate segment (a ring's repeated
    /// vertex) is its point, a point beside the segment its perpendicular distance, a point past
    /// an end the distance to that end. Fails with the projection not held to the segment (the
    /// third case reads 0). A zero-length segment without the guard still reads 5 (`f64::max`
    /// drops the NaN), so the guard is not what this pins.
    #[test]
    fn the_distance_to_a_degenerate_segment_is_to_its_point() {
        assert_eq!(seg_dist([3.0, 4.0], [0.0, 0.0], [0.0, 0.0]), 5.0);
        assert_eq!(seg_dist([0.0, 2.0], [-1.0, 0.0], [1.0, 0.0]), 2.0);
        assert_eq!(seg_dist([5.0, 0.0], [-1.0, 0.0], [1.0, 0.0]), 4.0);
    }
}
