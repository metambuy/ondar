//! D2: subdivisions as interior borders. Admin-1 polygons' edges are matched as unordered vertex
//! pairs (exact coordinates): an edge found twice is a border between two subdivisions (kept
//! once); an edge found once is the coast or the country's outer border (dropped — the land layer
//! draws it). Interior edges are chained into polylines between junctions (vertices of degree
//! ≠ 2); a closed loop of degree-2 vertices (an enclave) is one closed line.
//!
//! The gate, executed by the tool (decided 2026-10-01, replacing the plan's 1 m test, which
//! measured whether NE's admin-0 and admin-1 coasts coincide — NE does not promise that): every
//! once-found edge whose midpoint lies inside the country's admin-0 land — an interior border
//! found on one side only — is within `GATE_INSIDE_KM` of the admin-0 rings, and no edge is
//! found three times or more. If it fails, the subdivisions fall back to the prototype's polygons
//! and the report says so.

use crate::seam::on_seam;
use geo::{Coord, Polygon};
use std::collections::{BTreeMap, BTreeSet};

type Key = (u64, u64);

/// The gate's bound: 0.25 pt at 1.5 km/pt, km.
pub const GATE_INSIDE_KM: f64 = 0.375;
/// A once-edge within this of the admin-0 rings lies on them; only farther ones are tested for
/// being inside the land (the containment test is the expensive part), km.
pub const ON_RING_KM: f64 = 0.001;

/// A once-found edge's two ends and its distance to admin 0, km.
pub type FarEdge = (Coord<f64>, Coord<f64>, f64);

fn key(c: &Coord<f64>) -> Key {
    // +0.0 for −0.0, so the two zeros are one vertex
    ((c.x + 0.0).to_bits(), (c.y + 0.0).to_bits())
}

fn edge(a: Key, b: Key) -> (Key, Key) {
    if a <= b { (a, b) } else { (b, a) }
}

#[derive(Debug, Default)]
pub struct Census {
    /// Interior borders as lon/lat polylines, deterministic order.
    pub lines: Vec<Vec<Coord<f64>>>,
    pub edges_total: usize,
    pub once: Vec<(Coord<f64>, Coord<f64>)>,
    pub twice: usize,
    pub thrice_or_more: usize,
    /// Edges found twice along the seam (the halves of a subdivision cut at 180°): dropped.
    pub seam: usize,
}

/// The edge census and the interior lines of a set of lon/lat polygons (the seam shift applied:
/// see `seam::shift_east`).
pub fn census(polys: &[Polygon<f64>]) -> Census {
    let mut count: BTreeMap<(Key, Key), usize> = BTreeMap::new();
    let mut coord: BTreeMap<Key, Coord<f64>> = BTreeMap::new();
    for p in polys {
        for r in crate::geom::rings(p) {
            for w in r.0.windows(2) {
                if let [a, b] = w
                    && key(a) != key(b)
                {
                    coord.insert(key(a), *a);
                    coord.insert(key(b), *b);
                    *count.entry(edge(key(a), key(b))).or_default() += 1;
                }
            }
        }
    }
    let mut c = Census {
        edges_total: count.len(),
        ..Census::default()
    };
    let mut adj: BTreeMap<Key, BTreeSet<Key>> = BTreeMap::new();
    for (&(a, b), &n) in &count {
        let (ca, cb) = (coord[&a], coord[&b]);
        match n {
            // along the seam, found once or twice: the halves of a subdivision cut at 180°
            // (their seam vertices need not coincide), never a border or a coast (R9)
            1 | 2 if on_seam(ca.x) && on_seam(cb.x) => c.seam += 1,
            1 => c.once.push((ca, cb)),
            2 => {
                c.twice += 1;
                adj.entry(a).or_default().insert(b);
                adj.entry(b).or_default().insert(a);
            }
            _ => c.thrice_or_more += 1,
        }
    }
    let mut used: BTreeSet<(Key, Key)> = BTreeSet::new();
    let walk = |start: Key, next: Key, used: &mut BTreeSet<(Key, Key)>| -> Vec<Key> {
        let mut line = vec![start];
        let (mut prev, mut cur) = (start, next);
        used.insert(edge(prev, cur));
        loop {
            line.push(cur);
            let nb = &adj[&cur];
            if nb.len() != 2 || cur == start {
                break;
            }
            let Some(&n) = nb.iter().find(|&&n| n != prev) else {
                break;
            };
            if !used.insert(edge(cur, n)) {
                break;
            }
            (prev, cur) = (cur, n);
        }
        line
    };
    // lines between junctions first, then the closed loops
    let mut lines: Vec<Vec<Key>> = Vec::new();
    for (&v, nb) in &adj {
        if nb.len() != 2 {
            for &n in nb {
                if !used.contains(&edge(v, n)) {
                    lines.push(walk(v, n, &mut used));
                }
            }
        }
    }
    for (&v, nb) in &adj {
        for &n in nb {
            if !used.contains(&edge(v, n)) {
                lines.push(walk(v, n, &mut used));
            }
        }
    }
    c.lines = lines
        .into_iter()
        .map(|l| l.iter().map(|k| coord[k]).collect())
        .collect();
    c
}

/// The gate's measure: for every once-found edge (projected by `fwd`), the larger of its two
/// ends' distances to the nearest admin-0 segment (`segments`, the same projection), km. Returns
/// the edges farther than `tol_km`, and the largest distance seen.
pub fn gate(
    once: &[(Coord<f64>, Coord<f64>)],
    segments: &[(Coord<f64>, Coord<f64>)],
    fwd: impl Fn(Coord<f64>) -> Option<Coord<f64>>,
    tol_km: f64,
) -> (Vec<FarEdge>, f64) {
    // a uniform grid of the admin-0 segments, cells of 10 km
    let cell = 10.0;
    let k = |c: Coord<f64>| ((c.x / cell).floor() as i64, (c.y / cell).floor() as i64);
    let mut grid: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    for (i, (a, b)) in segments.iter().enumerate() {
        let (ka, kb) = (k(*a), k(*b));
        for x in ka.0.min(kb.0)..=ka.0.max(kb.0) {
            for y in ka.1.min(kb.1)..=ka.1.max(kb.1) {
                grid.entry((x, y)).or_default().push(i);
            }
        }
    }
    let near = |p: Coord<f64>| -> f64 {
        let (cx, cy) = k(p);
        let mut best = f64::INFINITY;
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &i in grid.get(&(x, y)).map(Vec::as_slice).unwrap_or_default() {
                    let (a, b) = segments[i];
                    best = best.min(crate::geom::seg_dist(p, a, b));
                }
            }
        }
        best
    };
    let mut far = Vec::new();
    let mut worst = 0f64;
    for &(a, b) in once {
        let d = match (fwd(a), fwd(b)) {
            (Some(pa), Some(pb)) => near(pa).max(near(pb)),
            _ => f64::INFINITY,
        };
        worst = worst.max(d);
        if d > tol_km {
            far.push((a, b, d));
        }
    }
    (far, worst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::LineString;

    fn sq(x: f64, y: f64) -> Polygon<f64> {
        Polygon::new(
            LineString::from(vec![
                (x, y),
                (x + 1.0, y),
                (x + 1.0, y + 1.0),
                (x, y + 1.0),
                (x, y),
            ]),
            vec![],
        )
    }

    /// Three unit squares in a row: the two shared edges are found twice, the outline's eight
    /// once; the interior edges do not meet, so two lines. Fails if once-found edges are kept or
    /// twice-found ones dropped.
    #[test]
    fn three_squares() {
        let c = census(&[sq(0.0, 0.0), sq(1.0, 0.0), sq(2.0, 0.0)]);
        assert_eq!(c.twice, 2);
        assert_eq!(c.once.len(), 8);
        assert_eq!(c.thrice_or_more, 0);
        assert_eq!(c.lines.len(), 2);
        let mut lines: Vec<Vec<(f64, f64)>> = c
            .lines
            .iter()
            .map(|l| l.iter().map(|p| (p.x, p.y)).collect())
            .collect();
        for l in &mut lines {
            l.sort_by(|a, b| a.partial_cmp(b).unwrap());
        }
        lines.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            lines,
            vec![vec![(1.0, 0.0), (1.0, 1.0)], vec![(2.0, 0.0), (2.0, 1.0)]]
        );
    }

    /// A fourth square above the middle one: the interior edges x = 1, y = 1 and x = 2 chain
    /// through two vertices of degree 2 into one line, not three. Two rows of two: the four
    /// interior edges meet at the centre (degree 4), a junction, so four lines. Fails if chains
    /// are not cut.
    #[test]
    fn junctions_and_chains() {
        let c = census(&[sq(0.0, 0.0), sq(1.0, 0.0), sq(2.0, 0.0), sq(1.0, 1.0)]);
        assert_eq!(c.twice, 3);
        assert_eq!(c.lines.len(), 1, "{:?}", c.lines);
        // the three interior edges chain through (1,1) and (2,1): x=1 up, y=1 across, x=2 down
        assert_eq!(c.lines[0].len(), 4);
        // two rows of two: a plus-shaped border, junction at the centre (degree 4)
        let c = census(&[sq(0.0, 0.0), sq(1.0, 0.0), sq(0.0, 1.0), sq(1.0, 1.0)]);
        assert_eq!(c.twice, 4);
        assert_eq!(c.lines.len(), 4);
    }

    /// An enclave: a square inside a ring with a hole of the same shape. Its four edges are
    /// found twice and form one closed line.
    #[test]
    fn an_enclave_is_a_closed_line() {
        let outer = Polygon::new(
            LineString::from(vec![
                (0.0, 0.0),
                (3.0, 0.0),
                (3.0, 3.0),
                (0.0, 3.0),
                (0.0, 0.0),
            ]),
            vec![LineString::from(vec![
                (1.0, 1.0),
                (1.0, 2.0),
                (2.0, 2.0),
                (2.0, 1.0),
                (1.0, 1.0),
            ])],
        );
        let c = census(&[outer, sq(1.0, 1.0)]);
        assert_eq!(c.twice, 4);
        assert_eq!(c.lines.len(), 1);
        assert_eq!(c.lines[0].first(), c.lines[0].last());
        assert_eq!(c.lines[0].len(), 5);
    }

    /// An edge found three times is counted.
    #[test]
    fn an_edge_three_times_is_counted() {
        let c = census(&[sq(0.0, 0.0), sq(0.0, 0.0), sq(0.0, 0.0)]);
        assert_eq!(c.thrice_or_more, 4);
    }

    /// Two halves of one subdivision cut at 180°, shifted: their shared seam edge is found twice
    /// and dropped, not drawn as a border. Fails if only once-found seam edges are dropped.
    #[test]
    fn a_seam_edge_is_not_a_border() {
        let a = Polygon::new(
            LineString::from(vec![
                (179.0, 0.0),
                (180.0, 0.0),
                (180.0, 1.0),
                (179.0, 1.0),
                (179.0, 0.0),
            ]),
            vec![],
        );
        let b = Polygon::new(
            LineString::from(vec![
                (180.0, 0.0),
                (181.0, 0.0),
                (181.0, 1.0),
                (180.0, 1.0),
                (180.0, 0.0),
            ]),
            vec![],
        );
        let c = census(&[a, b]);
        assert_eq!((c.twice, c.seam), (0, 1));
        assert!(c.lines.is_empty());
    }

    /// The rule: 375 m inside the land passes, 375.001 m fails, an edge found three times fails
    /// whatever the distances. Fails at 1 m (the plan's test), on `<` for `≤`, or with the 3+
    /// check dropped.
    #[test]
    fn the_gate_rule() {
        assert!(judge(0.0, 0));
        assert!(judge(0.079, 0));
        assert!(judge(0.375, 0));
        assert!(!judge(0.375_001, 0));
        assert!(!judge(0.0, 1));
        assert!(!judge(f64::INFINITY, 0));
    }

    /// The 1 m on-ring prefilter. Fails with the tolerance × 10.
    #[test]
    fn the_gate_measures_once_edges_against_admin0() {
        let id = |c: Coord<f64>| Some(c);
        let c0 = |x: f64, y: f64| Coord { x, y };
        let admin0 = [(c0(0.0, 0.0), c0(100.0, 0.0))];
        let on = [(c0(10.0, 0.0), c0(20.0, 0.0005))];
        let off = [(c0(10.0, 0.0), c0(20.0, 0.002))];
        assert!(gate(&on, &admin0, id, 0.001).0.is_empty());
        let (far, worst) = gate(&off, &admin0, id, 0.001);
        assert_eq!(far.len(), 1);
        assert!((worst - 0.002).abs() < 1e-12);
    }
}

/// One subdivision country's borders: the census of its admin-1 polygons (Antarctica's polar
/// run stripped and every part on the seam shifted east, as for admin 0), the gate against its
/// stitched admin-0 rings in its frame LAEA, and the lines.
#[derive(Debug)]
pub struct CountryBorders {
    pub code: String,
    pub admin1: usize,
    pub census: Census,
    /// Once-found edges farther than 1 m from admin 0, and the farthest, km.
    pub gate_far: usize,
    pub gate_worst_km: f64,
    /// Of those, the ones whose midpoint lies inside the country's admin-0 land — an interior
    /// border found on one side only, the failure the gate is there to catch — and the farthest.
    pub far_inside: usize,
    pub far_inside_worst_km: f64,
}

impl CountryBorders {
    pub fn passes(&self) -> bool {
        judge(self.far_inside_worst_km, self.census.thrice_or_more)
    }
}

/// The gate's rule on its two measures: the farthest once-edge inside the land (km), and the
/// edges found three times or more.
pub fn judge(far_inside_worst_km: f64, thrice_or_more: usize) -> bool {
    far_inside_worst_km <= GATE_INSIDE_KM && thrice_or_more == 0
}

pub fn for_country(
    plan: &crate::world::CountryPlan,
    world: &crate::world::World,
    admin1: &[crate::ne::Admin1],
) -> CountryBorders {
    let a3s: Vec<&str> = plan
        .units
        .iter()
        .map(|&u| world.units[u].a3.as_str())
        .collect();
    let recs: Vec<&crate::ne::Admin1> = admin1
        .iter()
        .filter(|a| a3s.contains(&a.adm0_a3.as_str()))
        .collect();
    let mut polys: Vec<Polygon<f64>> = recs.iter().flat_map(|a| a.parts.iter().cloned()).collect();
    if a3s.contains(&"ATA") {
        polys = crate::seam::strip_polar(polys).0;
    } else {
        polys = polys
            .iter()
            .map(|p| {
                if crate::geom::rings(p).any(|r| r.0.iter().any(|c| on_seam(c.x))) {
                    crate::seam::shift_east(p)
                } else {
                    p.clone()
                }
            })
            .collect();
    }
    let census = census(&polys);
    let l = plan.laea();
    let fwd = |c: Coord<f64>| l.fwd(c.x, c.y).map(|[x, y]| Coord { x, y });
    let mut segments = Vec::new();
    for &u in &plan.units {
        for p in &world.units[u].parts {
            for r in crate::geom::rings(p) {
                for w in r.0.windows(2) {
                    if let [a, b] = w
                        && let (Some(pa), Some(pb)) = (fwd(*a), fwd(*b))
                    {
                        segments.push((pa, pb));
                    }
                }
            }
        }
    }
    let (far, gate_worst_km) = gate(&census.once, &segments, fwd, ON_RING_KM);
    let land: Vec<&Polygon<f64>> = plan
        .units
        .iter()
        .flat_map(|&u| world.units[u].parts.iter())
        .collect();
    let inside: Vec<f64> = far
        .iter()
        .filter(|(a, b, _)| {
            let mid = geo::Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
            land.iter().any(|p| geo::Contains::contains(*p, &mid))
        })
        .map(|&(_, _, d)| d)
        .collect();
    CountryBorders {
        code: plan.code.clone(),
        admin1: recs.len(),
        census,
        gate_far: far.len(),
        gate_worst_km,
        far_inside: inside.len(),
        far_inside_worst_km: inside.iter().copied().fold(0.0, f64::max),
    }
}
