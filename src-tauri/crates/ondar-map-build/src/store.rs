//! The resource's contents: every unit's rings at each ladder level in its storage LAEA, the
//! coverage the clamp needs, the ring index (caps), the subdivisions, and the figures the report
//! carries (bytes, vertices, bounds, P2, P4).
//!
//! Storage LAEA: a country's main unit is stored in its frame's LAEA (the frame reads it by a
//! translation and a scale); every other unit in its own R1 centre.
//!
//! Coverage (the clamp turned into blobs): a view of country C at scale s uses the coarsest
//! level ≤ s; its centre is clamped into C's fit rectangle and s into [1.5, fit], so a view at
//! level k reaches at most (W/2 + 2) × s_max(k) beyond the fit rectangle (+2 pt: the clip
//! margin), s_max(k) = min(L_{k+1}, max(fit, 1.5)). **The fit, the fit rectangle and so the
//! reach are the band's** (M4b commit 3): the app shows the map at 328 × h for every integer h
//! in `BAND_FLOOR..=BAND_MAX`, and a shorter band has a coarser fit and a wider reach in km. A
//! unit is stored at k when a ring's cap meets the bounding rectangle of the reaches over every
//! band whose views can use k (one cap test; conservative — the exact union is not a rectangle);
//! the blobs the bound adds over the exact union are reported, and if they exceed 5 % of the
//! file the exact union is stored instead. Every unit in an inset is stored at the inset's level.
//! A ring whose quanta leave fewer than three distinct vertices is stored empty and counted
//! (`collapsed`): no frame can draw it.

use crate::borders;
use crate::geom;
use crate::simplify;
use crate::world::{CountryPlan, GroupRole, World};
use geo::{Coord, Polygon};
use ondar_map::format::{self, BlobIn, Cap, Layer};
use ondar_map::index;
use ondar_map::laea::{Laea, R_AUTHALIC_KM, haversine_km};
use ondar_map::rules::{self, BAND_FLOOR, BAND_MAX, LADDER, Pane};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub use ondar_map::index::{LAND_TOL_PT, QUANT_PT, SUB_TOL_PT};
/// Subdivisions are drawn above 8 km/pt, so stored from the level 6 views use (index 2).
pub const SUB_FIRST_LEVEL: usize = 2;

pub struct Ring {
    /// Projected, km, closed (first = last).
    pub km: Vec<Coord<f64>>,
    pub cap: Cap,
}

pub struct UnitGeom {
    pub lat0: f64,
    pub lon0: f64,
    /// Per part, its rings (exterior first).
    pub parts: Vec<Vec<Ring>>,
}

impl UnitGeom {
    pub fn rings(&self) -> impl Iterator<Item = &Ring> {
        self.parts.iter().flatten()
    }
}

/// Simplified rings of one blob, and its figures.
pub struct BlobOut {
    pub owner: usize,
    pub level: usize,
    pub layer: Layer,
    pub rings: Vec<Vec<[i32; 2]>>,
    pub vertices_in: usize,
    pub vertices_out: usize,
    /// The largest exact displacement of a ring, points at the level.
    pub bound_pt: f64,
    /// Per ring, its exact displacement in points (for P2).
    pub ring_bounds_pt: Vec<f64>,
    pub evaluations: usize,
    pub neighbour_only: bool,
    /// P4: open vertex counts of the two candidates per ring, summed — per-ring VW and RDP at
    /// the same tolerance — and the rings that took VW's (RDP not simple or over the bound).
    pub vertices_vw: usize,
    pub vertices_rdp: usize,
    /// The chosen rings' open vertex count before quantisation (VW's, RDP's or the hybrid's).
    pub vertices_chosen: usize,
    pub fallbacks: usize,
    /// The fallen-back ring with the most VW vertices: (its RDP count, VW count, input count).
    pub largest_fallback: (usize, usize, usize),
    /// Rings whose quanta left fewer than three distinct vertices, stored empty (M4b commit 3).
    pub collapsed: usize,
}

/// What the bounding reach stores over the exact union of the bands' reaches (M4b commit 3):
/// the land blobs only the bound asked for, their deflated bytes against the file's, and whether
/// the 5 % rule made the build store the exact union instead (those blobs dropped).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoundAdded {
    pub blobs: usize,
    pub bytes: usize,
    pub total_bytes: usize,
    pub exact_stored: bool,
}

/// Which simplification is stored (P4's rule: the hybrid if every bound holds and the build
/// stays under ~10 min; else VW).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Simplifier {
    Hybrid,
    Vw,
}

fn simplify_one(
    line: &[Coord<f64>],
    closed: bool,
    t: f64,
    how: Simplifier,
    b: &mut BlobOut,
) -> simplify::Tuned {
    let h = simplify::hybrid(line, closed, t);
    b.vertices_vw += h.vw;
    b.vertices_rdp += h.rdp;
    let chosen = match how {
        Simplifier::Hybrid => {
            b.fallbacks += usize::from(h.pick == simplify::Pick::Vw);
            h.chosen
        }
        Simplifier::Vw => simplify::tune(line, closed, t),
    };
    b.vertices_chosen += chosen.line.len().saturating_sub(usize::from(closed));
    if how == Simplifier::Hybrid && h.pick == simplify::Pick::Vw && h.vw > b.largest_fallback.1 {
        b.largest_fallback = (h.rdp, h.vw, line.len().saturating_sub(usize::from(closed)));
    }
    chosen
}

/// Per country: how its subdivisions are stored.
#[derive(Clone, Debug, PartialEq)]
pub enum SubStorage {
    /// D2: interior borders (the gate passed).
    Borders,
    /// The prototype's polygon rings (the gate failed: the plan's fallback).
    Polygons,
}

pub struct SubPlan {
    pub country: usize,
    pub storage: SubStorage,
    /// Lines in the frame's LAEA, km, and their caps.
    pub lines: Vec<(Vec<Coord<f64>>, Cap)>,
}

/// P2: per level, the largest scale ratio between a ring's storage LAEA and a projection that
/// draws it, and the worst displacement that makes in points (ring bound × ratio).
#[derive(Clone, Debug, Default)]
pub struct P2 {
    pub max_ratio: f64,
    pub worst_pt: f64,
    pub worst_at: String,
}

pub struct Built {
    pub header: format::Header,
    pub units: Vec<format::Unit>,
    pub countries: Vec<format::Country>,
    pub blobs: Vec<BlobOut>,
    pub subs: Vec<SubPlan>,
    pub p2: Vec<P2>,
    /// P4: RU's land at 24 km/pt: (input vertices, per-ring VW, RDP at the same tolerance).
    /// P4: RU's land at 24 km/pt — (input, per-ring VW, RDP, stored) open vertices.
    pub p4: (usize, usize, usize, usize),
    /// RU's blob at 24 km/pt: rings that fell back, and the largest of them (RDP, VW, input).
    pub p4_fallback: (usize, (usize, usize, usize)),
    pub simplifier: Simplifier,
    pub seconds: f64,
    /// The band heights the coverage is built for (M4b commit 3).
    pub bands: (u32, u32),
    pub bound_added: BoundAdded,
    /// Collapsed rings: (unit, level, ring index within the unit).
    pub collapsed: Vec<(usize, usize, usize)>,
    /// Countries flagged for no subdivisions at the golden fit whose fit at the shortest band is
    /// above the 8 km/pt rule — an observation for the chat, not a rule the build applies.
    pub subdivision_candidates: Vec<String>,
}

/// LAEA's tangential scale factor at angular distance `c` (radians) from the centre; the radial
/// one is its inverse, so a small displacement is stretched by at most this.
fn k_prime(c: f64) -> f64 {
    (2.0 / (1.0 + c.cos())).sqrt()
}

/// The country's fit at the band of height `h` (D6 per band: the fit rectangle and the clamp are
/// the band's). `fit_at(p, BAND_MAX)` is the golden fit, `p.fit`.
pub fn fit_at(p: &CountryPlan, h: u32) -> f64 {
    let [x0, y0, x1, y1] = p.bbox;
    rules::fit_scale(x1 - x0, y1 - y0, &Pane::band(h)).unwrap_or(p.fit)
}

/// The bands — every integer height in `BAND_FLOOR..=BAND_MAX` — at which a view of country `p`
/// can use level `k`: those whose widest view is at `k` or coarser.
pub fn bands_using(p: &CountryPlan, k: usize) -> impl Iterator<Item = u32> + '_ {
    (BAND_FLOOR..=BAND_MAX).filter(move |&h| index::top_level(fit_at(p, h)) >= k)
}

/// The reach of country `p` at level `k` at one band (D6: the view inside that band's fit
/// rectangle).
pub fn reach_at(p: &CountryPlan, k: usize, h: u32) -> [f64; 4] {
    index::reach(p.bbox, fit_at(p, h), &Pane::band(h), k)
}

/// The reach of country `p` at level `k` over every band (M4b commit 3): the bounding rectangle
/// of `reach_at` over the bands whose views can use `k`. `None` if no band's can (k above the
/// country's top level).
pub fn reach(p: &CountryPlan, k: usize) -> Option<[f64; 4]> {
    bands_using(p, k).map(|h| reach_at(p, k, h)).reduce(|a, b| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    })
}

/// The country's top level over the bands: the level its widest view uses at the shortest band
/// (the fit is non-increasing in the band's height, so the shortest band has the coarsest).
pub fn top_level(p: &CountryPlan) -> usize {
    index::top_level(fit_at(p, BAND_FLOOR))
}

/// The 5 % rule (M4b plan § 4): the blobs the bounding reach adds over the exact union are
/// dropped when their deflated bytes exceed 5 % of the file's.
pub fn store_exact_only(added_bytes: usize, total_bytes: usize) -> bool {
    added_bytes * 20 > total_bytes
}

/// How many distinct vertices a quantised ring has.
fn distinct(q: &[[i32; 2]]) -> usize {
    q.iter().collect::<BTreeSet<_>>().len()
}

fn quantise_ring(ring: &[Coord<f64>], level: f64) -> Vec<[i32; 2]> {
    let open = match ring.split_last() {
        Some((last, rest)) if Some(last) == ring.first() && rest.len() >= 3 => rest,
        _ => ring,
    };
    let mut q: Vec<[i32; 2]> = Vec::with_capacity(open.len());
    for c in open {
        if let Some(v) = ondar_map::codec::quantise([c.x, c.y], level)
            && q.last() != Some(&v)
        {
            q.push(v);
        }
    }
    q
}

pub(crate) fn job<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, T)>> = Mutex::new(Vec::with_capacity(n));
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let r = f(i);
                    out.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, r)| r).collect()
}

/// The subdivision lines of a country, by the gate: borders, or the polygons' rings.
fn sub_plan(ci: usize, p: &CountryPlan, world: &World, admin1: &[crate::ne::Admin1]) -> SubPlan {
    let census = borders::for_country(p, world, admin1);
    let l = p.laea();
    let (storage, lines_ll): (SubStorage, Vec<Vec<Coord<f64>>>) = if census.passes() {
        (SubStorage::Borders, census.census.lines.clone())
    } else {
        let a3s: Vec<&str> = p
            .units
            .iter()
            .map(|&u| world.units[u].a3.as_str())
            .collect();
        let polys: Vec<Polygon<f64>> = admin1
            .iter()
            .filter(|a| a3s.contains(&a.adm0_a3.as_str()))
            .flat_map(|a| a.parts.iter().cloned())
            .collect();
        let polys = if a3s.contains(&"ATA") {
            crate::seam::strip_polar(polys).0
        } else {
            polys
        };
        (
            SubStorage::Polygons,
            polys
                .iter()
                .flat_map(|p| geom::rings(p).map(|r| r.0.clone()).collect::<Vec<_>>())
                .collect(),
        )
    };
    let lines = lines_ll
        .iter()
        .filter_map(|line| {
            let km: Vec<Coord<f64>> = line
                .iter()
                .filter_map(|c| l.fwd(c.x, c.y).map(|[x, y]| Coord { x, y }))
                .collect();
            let cap = geom::cap(line.iter())?;
            (km.len() >= 2).then_some((km, cap))
        })
        .collect();
    SubPlan {
        country: ci,
        storage,
        lines,
    }
}

pub fn build(
    world: &World,
    admin1: &[crate::ne::Admin1],
    plans: &[CountryPlan],
    aliases: &[crate::tables::Alias],
    pins: format::Pins,
    how: Simplifier,
) -> Result<Built, String> {
    let t0 = std::time::Instant::now();
    let nu = world.units.len();

    // storage LAEA per unit: a country's main unit in its frame's. The frame reprojects its main
    // unit by translation alone, so a unit may be the main unit of one country only (review 2,
    // second pass); the build is refused otherwise
    let mut centre: Vec<Option<(f64, f64)>> = vec![None; nu];
    let mut main_of: Vec<Option<&str>> = vec![None; nu];
    for p in plans {
        if let Some(&main) = p.units.first() {
            let (Some(c), Some(owner)) = (centre.get_mut(main), main_of.get_mut(main)) else {
                return Err(format!("{}: main unit {main} is not in the world", p.code));
            };
            if let Some(first) = owner {
                return Err(format!(
                    "unit {} is the main unit of {first} and {}",
                    world.units[main].a3, p.code
                ));
            }
            *c = Some((p.lat0, p.lon0));
            *owner = Some(p.code.as_str());
        }
    }
    let geoms: Vec<UnitGeom> = job(nu, |u| {
        let unit = &world.units[u];
        let (lat0, lon0) = centre[u]
            .or_else(|| geom::lonlat_centre(unit.parts.iter()))
            .unwrap_or((0.0, 0.0));
        let l = Laea::new(lat0, lon0);
        UnitGeom {
            lat0,
            lon0,
            parts: unit
                .parts
                .iter()
                .map(|part| {
                    geom::rings(part)
                        .filter_map(|r| {
                            Some(Ring {
                                km: geom::project_ring(r, &l).0,
                                cap: geom::cap(r.0.iter())?,
                            })
                        })
                        .collect()
                })
                .collect(),
        }
    });

    // roles and the S4 omit-in
    let country_of = |code: &str| plans.iter().position(|p| p.code == code);
    let mut roles: Vec<Vec<format::Role>> = world
        .units
        .iter()
        .map(|u| vec![format::Role::NeighbourOnly; u.parts.len()])
        .collect();
    for p in plans {
        for (pi, &(u, part)) in p.parts.iter().enumerate() {
            roles[u][part] = match p.role_of_part(pi) {
                GroupRole::Frame => format::Role::Frame,
                GroupRole::Inset(k) => format::Role::Inset(k as u8),
                GroupRole::Dropped => format::Role::Dropped,
            };
        }
    }
    let mut omit: Vec<Vec<Option<u16>>> = world
        .units
        .iter()
        .map(|u| vec![None; u.parts.len()])
        .collect();
    for (code, hits, _) in crate::world::s4_matches(world, aliases) {
        let ci = country_of(&code).ok_or(format!("alias {code} has no country"))?;
        for (u, part) in hits {
            omit[u][part] = Some(ci as u16);
        }
    }

    // coverage over the bands (M4b commit 3): (unit, level) needed by the bounding reach, by the
    // exact union of the bands' reaches (`exact`, for the report and the 5 % rule), and whether
    // by its own country
    let mut need = vec![[false; LADDER.len()]; nu];
    let mut exact = vec![[false; LADDER.len()]; nu];
    let mut own_need = vec![[false; LADDER.len()]; nu];
    // which (plan, level) each unit's rings were found in, for P2
    let mut drawn_by: Vec<Vec<(usize, usize)>> = vec![Vec::new(); nu];
    for (pi, p) in plans.iter().enumerate() {
        let l = p.laea();
        for k in 0..=top_level(p) {
            let Some(bound) = reach(p, k) else {
                continue;
            };
            let rc = index::ground_cap(&l, bound);
            let per_band: Vec<(f64, f64, f64)> = bands_using(p, k)
                .map(|h| index::ground_cap(&l, reach_at(p, k, h)))
                .collect();
            let tol = index::tolerance_km(LADDER[k], LAND_TOL_PT);
            for (u, g) in geoms.iter().enumerate() {
                let meets = |cap: (f64, f64, f64)| {
                    index::cap_meets(&world_cap(g), cap, tol)
                        && g.rings().any(|r| index::cap_meets(&r.cap, cap, tol))
                };
                if !meets(rc) {
                    continue;
                }
                need[u][k] = true;
                if per_band.iter().any(|&cap| meets(cap)) {
                    exact[u][k] = true;
                }
                if p.units.contains(&u) {
                    own_need[u][k] = true;
                }
                if p.units.first() != Some(&u) {
                    drawn_by[u].push((pi, k));
                }
            }
        }
        for ins in &p.insets {
            // the inset's level at the golden box and at every band's scaled box (I1, M4b
            // commit 4): a smaller box fits the same group at a coarser scale
            let mut ks = BTreeSet::from([rules::level_for(ins.scale)]);
            for (i, &pct) in ins.scale_pct.iter().enumerate() {
                if pct == 0 {
                    continue;
                }
                let h = BAND_FLOOR + i as u32;
                let rect = rules::inset_box_at(
                    ins.row.rect,
                    ins.row.corner,
                    &Pane::band(h),
                    f64::from(pct) / 100.0,
                );
                if let Some(sc) = rules::inset_scale(ins.size_km[0], ins.size_km[1], rect) {
                    ks.insert(rules::level_for(sc));
                }
            }
            for k in ks {
                for &gp in &p.groups[ins.group].parts {
                    let (u, _) = p.parts[gp];
                    need[u][k] = true;
                    exact[u][k] = true;
                    own_need[u][k] = true;
                }
            }
        }
    }

    // P2: per (unit, ring, level), the largest stretch of a displacement between the storage
    // LAEA and a projection that draws the ring, over its vertices inside that projection's
    // reach: k'(c_storage) × k'(c_drawing) (LAEA's scale factors are k' and 1/k'). An inset
    // draws at its own scale, so its stretch is also × level / inset scale.
    type Stretch = Vec<Vec<[(f64, String); LADDER.len()]>>;
    let stretch: Stretch = job(nu, |u| {
        let g = &geoms[u];
        let gl = Laea::new(g.lat0, g.lon0);
        let c_of = |rho: f64| 2.0 * (rho / (2.0 * R_AUTHALIC_KM)).min(1.0).asin();
        let mut out: Vec<[(f64, String); LADDER.len()]> =
            (0..g.rings().count()).map(|_| Default::default()).collect();
        let mut consider = |ri: usize, k: usize, q: f64, at: &dyn Fn() -> String| {
            if let Some(slot) = out.get_mut(ri).and_then(|o| o.get_mut(k))
                && q > slot.0
            {
                *slot = (q, at());
            }
        };
        for &(pi, k) in &drawn_by[u] {
            let p = &plans[pi];
            let fl = p.laea();
            let Some(rect) = reach(p, k) else {
                continue;
            };
            for (ri, r) in g.rings().enumerate() {
                let mut best = 0f64;
                for c in &r.km {
                    let Some((lon, lat)) = gl.inv(c.x, c.y) else {
                        continue;
                    };
                    let Some([x, y]) = fl.fwd(lon, lat) else {
                        continue;
                    };
                    if x < rect[0] || x > rect[2] || y < rect[1] || y > rect[3] {
                        continue;
                    }
                    let q = k_prime(c_of(c.x.hypot(c.y))) * k_prime(c_of(x.hypot(y)));
                    best = best.max(q);
                }
                consider(ri, k, best, &|| p.code.clone());
            }
        }
        for p in plans {
            for ins in &p.insets {
                let k = rules::level_for(ins.scale);
                let il = Laea::new(ins.lat0, ins.lon0);
                let first_of = |part: usize| g.parts[..part].iter().map(Vec::len).sum::<usize>();
                for &gp in &p.groups[ins.group].parts {
                    let (pu, part) = p.parts[gp];
                    if pu != u {
                        continue;
                    }
                    for (j, r) in g.parts[part].iter().enumerate() {
                        let mut best = 0f64;
                        for c in &r.km {
                            let Some((lon, lat)) = gl.inv(c.x, c.y) else {
                                continue;
                            };
                            let Some([x, y]) = il.fwd(lon, lat) else {
                                continue;
                            };
                            best =
                                best.max(k_prime(c_of(c.x.hypot(c.y))) * k_prime(c_of(x.hypot(y))));
                        }
                        let q = best * LADDER[k] / ins.scale;
                        consider(first_of(part) + j, k, q, &|| {
                            format!("{} inset {}", p.code, ins.row.label)
                        });
                    }
                }
            }
        }
        out
    });

    // simplify every needed (unit, level), every ring alone
    let jobs: Vec<(usize, usize)> = (0..nu)
        .flat_map(|u| (0..LADDER.len()).map(move |k| (u, k)))
        .filter(|&(u, k)| need[u][k])
        .collect();
    let mut blobs: Vec<BlobOut> = job(jobs.len(), |j| {
        let (u, k) = jobs[j];
        let t = LAND_TOL_PT * LADDER[k];
        let mut b = BlobOut {
            owner: u,
            level: k,
            layer: Layer::Land,
            rings: Vec::new(),
            vertices_in: 0,
            vertices_out: 0,
            bound_pt: 0.0,
            ring_bounds_pt: Vec::new(),
            evaluations: 0,
            neighbour_only: !own_need[u][k],
            vertices_vw: 0,
            vertices_rdp: 0,
            vertices_chosen: 0,
            fallbacks: 0,
            largest_fallback: (0, 0, 0),
            collapsed: 0,
        };
        for r in geoms[u].rings() {
            let tuned = simplify_one(&r.km, true, t, how, &mut b);
            b.vertices_in += r.km.len().saturating_sub(1);
            let mut q = quantise_ring(&tuned.line, LADDER[k]);
            // fewer than three distinct quanta: no frame can draw it — stored empty, counted
            if distinct(&q) < 3 {
                b.collapsed += 1;
                q.clear();
            }
            b.vertices_out += q.len();
            b.bound_pt = b.bound_pt.max(tuned.bound / LADDER[k]);
            b.ring_bounds_pt.push(tuned.bound / LADDER[k]);
            b.evaluations += tuned.evaluations;
            b.rings.push(q);
        }
        b
    });

    // the blobs the bounding reach adds over the exact union, and the 5 % rule (M4b plan § 4):
    // measured in deflated bytes, as the file stores them
    let deflated =
        |b: &BlobOut| format::deflate(&format::blob_raw(&b.rings)).map_or(0, |v| v.len());
    let sizes: Vec<usize> = job(blobs.len(), |i| deflated(&blobs[i]));
    let total_bytes: usize = sizes.iter().sum();
    let extra: Vec<bool> = blobs
        .iter()
        .map(|b| b.layer == Layer::Land && !exact[b.owner][b.level])
        .collect();
    let mut bound_added = BoundAdded {
        blobs: extra.iter().filter(|&&e| e).count(),
        bytes: sizes
            .iter()
            .zip(&extra)
            .filter(|&(_, &e)| e)
            .map(|(&s, _)| s)
            .sum(),
        total_bytes,
        exact_stored: false,
    };
    if store_exact_only(bound_added.bytes, total_bytes) {
        bound_added.exact_stored = true;
        let mut keep = extra.iter().map(|&e| !e);
        blobs.retain(|_| keep.next().unwrap_or(true));
    }
    let collapsed: Vec<(usize, usize, usize)> = blobs
        .iter()
        .flat_map(|b| {
            b.rings
                .iter()
                .enumerate()
                .filter(|(_, r)| r.is_empty())
                .map(move |(ri, _)| (b.owner, b.level, ri))
        })
        .collect();

    // P2 per level
    let mut p2 = vec![P2::default(); LADDER.len()];
    for b in blobs.iter().filter(|b| b.layer == Layer::Land) {
        for (ri, &bp) in b.ring_bounds_pt.iter().enumerate() {
            let Some((q, at)) = stretch
                .get(b.owner)
                .and_then(|s| s.get(ri))
                .and_then(|s| s.get(b.level))
            else {
                continue;
            };
            let e = &mut p2[b.level];
            e.max_ratio = e.max_ratio.max(*q);
            let d = (bp + QUANT_PT) * q;
            if d > e.worst_pt {
                e.worst_pt = d;
                e.worst_at = format!("{} ring {ri} in {at}", world.units[b.owner].a3);
            }
        }
    }

    // P4: RU's land at 24 km/pt
    let p4 = {
        let ru = plans
            .iter()
            .find(|p| p.code == "RU")
            .and_then(|p| p.units.first().copied());
        let k = LADDER.len() - 1;
        let blob = blobs.iter().find(|b| Some(b.owner) == ru && b.level == k);
        blob.map_or((0, 0, 0, 0), |b| {
            (
                b.vertices_in,
                b.vertices_vw,
                b.vertices_rdp,
                b.vertices_chosen,
            )
        })
    };

    let p4_fallback = {
        let ru = plans
            .iter()
            .find(|p| p.code == "RU")
            .and_then(|p| p.units.first().copied());
        blobs
            .iter()
            .find(|b| Some(b.owner) == ru && b.level == LADDER.len() - 1)
            .map_or((0, (0, 0, 0)), |b| (b.fallbacks, b.largest_fallback))
    };

    // subdivisions
    let subs: Vec<SubPlan> = plans
        .iter()
        .enumerate()
        .filter(|(_, p)| p.subdivisions)
        .map(|(ci, p)| sub_plan(ci, p, world, admin1))
        .collect();
    let sub_jobs: Vec<(usize, usize)> = subs
        .iter()
        .enumerate()
        .flat_map(|(si, s)| (SUB_FIRST_LEVEL..=top_level(&plans[s.country])).map(move |k| (si, k)))
        .collect();
    let sub_blobs: Vec<BlobOut> = job(sub_jobs.len(), |j| {
        let (si, k) = sub_jobs[j];
        let s = &subs[si];
        let t = SUB_TOL_PT * LADDER[k];
        let mut b = BlobOut {
            owner: s.country,
            level: k,
            layer: Layer::Subdivisions,
            rings: Vec::new(),
            vertices_in: 0,
            vertices_out: 0,
            bound_pt: 0.0,
            ring_bounds_pt: Vec::new(),
            evaluations: 0,
            neighbour_only: false,
            vertices_vw: 0,
            vertices_rdp: 0,
            vertices_chosen: 0,
            fallbacks: 0,
            largest_fallback: (0, 0, 0),
            collapsed: 0,
        };
        for (line, _) in &s.lines {
            let closed = line.len() > 3 && line.first() == line.last();
            let tuned = simplify_one(line, closed, t, how, &mut b);
            b.vertices_in += line.len();
            // a line is drawn as a polyline: an enclave's loop keeps its closing vertex, or the
            // edge back to its start is never drawn (review finding 1; `quantise_ring` drops it)
            let mut q: Vec<[i32; 2]> = Vec::new();
            for c in &tuned.line {
                if let Some(v) = ondar_map::codec::quantise([c.x, c.y], LADDER[k])
                    && q.last() != Some(&v)
                {
                    q.push(v);
                }
            }
            b.vertices_out += q.len();
            b.bound_pt = b.bound_pt.max(tuned.bound / LADDER[k]);
            b.evaluations += tuned.evaluations;
            b.rings.push(q);
        }
        b
    });
    blobs.extend(sub_blobs);

    // the tables
    let units: Vec<format::Unit> = world
        .units
        .iter()
        .enumerate()
        .map(|(u, unit)| {
            let g = &geoms[u];
            let code = unit
                .code
                .as_deref()
                .and_then(|c| <[u8; 2]>::try_from(c.as_bytes()).ok());
            format::Unit {
                a3: <[u8; 3]>::try_from(unit.a3.as_bytes()).unwrap_or(*b"???"),
                code,
                name: unit.name.clone(),
                lat0: g.lat0,
                lon0: g.lon0,
                cap: world_cap(g),
                parts: g
                    .parts
                    .iter()
                    .enumerate()
                    .map(|(pi, rings)| format::Part {
                        role: roles[u][pi],
                        omit_in: omit[u][pi],
                        rings: rings.iter().map(|r| r.cap).collect(),
                    })
                    .collect(),
            }
        })
        .collect();
    let countries: Vec<format::Country> = plans
        .iter()
        .enumerate()
        .map(|(ci, p)| format::Country {
            code: <[u8; 2]>::try_from(p.code.as_bytes()).unwrap_or(*b"??"),
            name: p.name.clone(),
            units: p.units.iter().map(|&u| u as u16).collect(),
            lat0: p.lat0,
            lon0: p.lon0,
            bbox_km: p.bbox,
            subdivisions: p.subdivisions,
            overridden: p.overridden,
            alias: p.alias,
            insets: p
                .insets
                .iter()
                .map(|i| format::Inset {
                    label: i.row.label.clone(),
                    corner: i.row.corner,
                    rect: i.row.rect.map(|v| v as f32),
                    lat0: i.lat0,
                    lon0: i.lon0,
                    centre_km: i.centre_km,
                    size_km: i.size_km,
                    scale: i.scale,
                    scale_pct: i.scale_pct.clone(),
                })
                .collect(),
            sub_lines: subs
                .iter()
                .find(|s| s.country == ci)
                .map(|s| s.lines.iter().map(|(_, c)| *c).collect())
                .unwrap_or_default(),
        })
        .collect();
    let header = format::Header {
        pins,
        bands: format::Bands::BUILT,
        radius_km: R_AUTHALIC_KM,
        ladder: LADDER.to_vec(),
    };
    let subdivision_candidates: Vec<String> = plans
        .iter()
        .filter(|p| !p.subdivisions && fit_at(p, BAND_FLOOR) > rules::SUBDIVISIONS_ABOVE_KM_PER_PT)
        .map(|p| format!("{} ({:.2} at {BAND_FLOOR})", p.code, fit_at(p, BAND_FLOOR)))
        .collect();
    Ok(Built {
        header,
        units,
        countries,
        blobs,
        subs,
        p2,
        p4,
        p4_fallback,
        simplifier: how,
        seconds: t0.elapsed().as_secs_f64(),
        bands: (BAND_FLOOR, BAND_MAX),
        bound_added,
        collapsed,
        subdivision_candidates,
    })
}

/// A unit's cap: the union of its rings' caps, around the first ring's centre (conservative).
fn world_cap(g: &UnitGeom) -> Cap {
    let Some(first) = g.rings().next() else {
        return Cap {
            lon: 0.0,
            lat: 0.0,
            radius_km: 0.0,
        };
    };
    let (lon, lat) = (f64::from(first.cap.lon), f64::from(first.cap.lat));
    let r = g
        .rings()
        .map(|r| {
            haversine_km(lon, lat, f64::from(r.cap.lon), f64::from(r.cap.lat))
                + f64::from(r.cap.radius_km)
        })
        .fold(0.0, f64::max);
    Cap {
        lon: first.cap.lon,
        lat: first.cap.lat,
        radius_km: (r * 1.001 + 0.01) as f32,
    }
}

impl Built {
    pub fn blobs_in(&self) -> Vec<BlobIn> {
        self.blobs
            .iter()
            .map(|b| BlobIn {
                owner: b.owner as u16,
                level: b.level as u8,
                layer: b.layer,
                bound_pt: b.bound_pt as f32,
                rings: b.rings.clone(),
            })
            .collect()
    }

    pub fn write(&self, encoding: format::Encoding) -> std::io::Result<Vec<u8>> {
        format::write(
            &self.header,
            &self.units,
            &self.countries,
            &self.blobs_in(),
            encoding,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage on a synthetic world, where only the rule under test can store a blob: country
    /// AA (a 150 km square: its fit is the 1.5 floor at every band from 140 to 300, so top level 0
    /// everywhere — at 400 km the floor band's fit would be 4.0 and level 1 needed anyway, M4b
    /// commit 3) lists an inset group whose scale needs level 1 — nothing else asks for AA's unit
    /// at level 1 — and a `-99` unit sits just past the disc around AA's level-0 reach, along its
    /// diagonal, by half the level's tolerance — nothing else asks for it. Fails with the inset rule dropped or the cap tolerance ignored (both are invisible on
    /// the real data: other countries' reaches store those blobs, and the disc around a reach is
    /// wider than the tolerance everywhere a ring happens to lie).
    #[test]
    fn coverage_stores_the_inset_level_and_the_tolerance_band() {
        use crate::world::tests::{square, unit};
        let inset_part = square(0.0, 30.0, 1001f64.sqrt());
        let row = crate::tables::InsetRow {
            code: "AA".into(),
            lat: 0.0,
            lon: 30.0,
            keep: String::new(),
            corner: format::Corner::TopLeft,
            rect: [2.0, 2.0, 18.0, 30.0],
            label: "Far".into(),
        };
        let probe = |world: &World| {
            crate::world::plan_country("AA", &[0], world, None, &[&row], false).unwrap()
        };
        // place the neighbour from AA's plan alone
        let w0 = World::new(
            vec![unit(
                "AAA",
                "AA",
                vec![square(0.0, 0.0, 150.0), inset_part.clone()],
            )],
            vec![],
            &[],
        )
        .unwrap();
        let p0 = probe(&w0);
        let l = p0.laea();
        let g = index::ground_cap(&l, reach(&p0, 0).unwrap());
        let tol = index::tolerance_km(LADDER[0], LAND_TOL_PT);
        let side = 1.0;
        let r_cap = side / 2.0 * 2f64.sqrt() * 1.001 + 0.01 + 0.01;
        let dist = g.2 + r_cap + tol / 2.0;
        // along the bearing 45° from the reach's centre (g is centred on the bbox's centre)
        let ang = dist / R_AUTHALIC_KM;
        let (lat, lon) = (
            ang.to_degrees() / 2f64.sqrt(),
            ang.to_degrees() / 2f64.sqrt(),
        );
        let mut nb = unit("BBB", "--", vec![square(g.1 + lat, g.0 + lon, side)]);
        nb.code = None;
        let w = World::new(
            vec![
                unit("AAA", "AA", vec![square(0.0, 0.0, 150.0), inset_part]),
                nb,
            ],
            vec![],
            &[],
        )
        .unwrap();
        let p = probe(&w);
        assert_eq!(top_level(&p), 0);
        assert_eq!(
            rules::level_for(p.insets[0].scale),
            1,
            "{}",
            p.insets[0].scale
        );
        let pins = format::Pins {
            tool_git: "0".repeat(40),
            ne_tag: String::new(),
            inputs: vec![],
        };
        let b = build(&w, &[], &[p], &[], pins, Simplifier::Vw).unwrap();
        let has = |u: usize, k: usize| {
            b.blobs
                .iter()
                .any(|x| x.owner == u && x.level == k && x.layer == Layer::Land)
        };
        assert!(has(0, 0) && has(0, 1), "the inset's level");
        assert!(has(1, 0), "the neighbour in the tolerance band");
        assert!(!has(1, 1) && !has(0, 2));
    }

    /// A unit is the main unit of one country at most (review 2, second pass): the frame
    /// reprojects a country's main unit by translation alone, so a unit stored in one country's
    /// LAEA and framed as another's main unit would be drawn in the wrong place. Two plans naming
    /// the same main unit are refused. Fails with the check off: it builds, and the last one's
    /// centre wins silently.
    #[test]
    fn a_main_unit_shared_by_two_countries_is_refused() {
        use crate::world::tests::{square, unit};
        let w = World::new(
            vec![unit("AAA", "AA", vec![square(0.0, 0.0, 400.0)])],
            vec![],
            &[],
        )
        .unwrap();
        let p = crate::world::plan_country("AA", &[0], &w, None, &[], false).unwrap();
        let mut q = crate::world::plan_country("AA", &[0], &w, None, &[], false).unwrap();
        q.code = "BB".into();
        q.lat0 += 1.0;
        let pins = || format::Pins {
            tool_git: "0".repeat(40),
            ne_tag: String::new(),
            inputs: vec![],
        };
        assert!(
            build(
                &w,
                &[],
                std::slice::from_ref(&p),
                &[],
                pins(),
                Simplifier::Vw
            )
            .is_ok()
        );
        let e = build(&w, &[], &[p, q], &[], pins(), Simplifier::Vw)
            .err()
            .expect("a shared main unit builds");
        assert!(e.contains("main unit"), "{e}");
    }

    /// Coverage is for every band (M4b commit 3). Country AA, a 400 km square: at the golden pane
    /// its usable height binds, 400 / 260 = 1.538 km/pt (top level 0), and its level-0 reach runs
    /// to 166 × 1.538 = 255.4 km; at the 140 pt floor the usable height is 100 pt, the fit 4.0
    /// km/pt (top level 1, the 3 km/pt level) and the level-0 reach 164 × 4 + 6 = 662 km. A 1 km
    /// `-99` neighbour 400 km east of AA's centre is inside the floor band's reach at levels 0
    /// and 1 and outside the golden pane's, so it is stored at both — and AA itself at level 1,
    /// which only the bands up to 173 pt use. Fails with the reach taken at the golden
    /// pane alone (the neighbour is not stored, nor AA above level 0) and with the top level from
    /// the golden fit. The bound adds nothing here (one neighbour on the x axis, inside the
    /// floor's own reach), so the file keeps every blob and `exact_stored` is false.
    #[test]
    fn coverage_covers_the_shortest_band() {
        use crate::world::tests::{square, unit};
        let aa = unit("AAA", "AA", vec![square(0.0, 0.0, 400.0)]);
        // 400 km east along the equator: 1° is 111.19 km on the authalic sphere
        let dlon = 400.0 / (R_AUTHALIC_KM * std::f64::consts::PI / 180.0);
        let mut nb = unit("BBB", "--", vec![square(0.0, dlon, 1.0)]);
        nb.code = None;
        let w = World::new(vec![aa, nb], vec![], &[]).unwrap();
        let p = crate::world::plan_country("AA", &[0], &w, None, &[], false).unwrap();
        assert_eq!(index::top_level(p.fit), 0, "golden: {}", p.fit);
        assert!(
            (fit_at(&p, BAND_FLOOR) - 4.0).abs() < 1e-9,
            "{}",
            fit_at(&p, 140)
        );
        assert_eq!(top_level(&p), 1);
        assert_eq!(bands_using(&p, 0).count(), 161);
        // fit(h) = 400 / (h − 40) ≥ 3 for h ≤ 173: 34 bands use level 1
        assert_eq!(bands_using(&p, 1).count(), 34);
        let golden = index::reach(p.bbox, p.fit, &Pane::GOLDEN, 0);
        let bound = reach(&p, 0).unwrap();
        assert!(
            (golden[2] - 166.0 * 400.0 / 260.0).abs() < 1e-9,
            "{golden:?}"
        );
        assert!((bound[2] - 662.0).abs() < 1e-9, "{bound:?}");
        assert_eq!(reach(&p, 2), None);
        let pins = format::Pins {
            tool_git: "0".repeat(40),
            ne_tag: String::new(),
            inputs: vec![],
        };
        let b = build(&w, &[], &[p], &[], pins, Simplifier::Vw).unwrap();
        let has = |u: usize, k: usize| {
            b.blobs
                .iter()
                .any(|x| x.owner == u && x.level == k && x.layer == Layer::Land)
        };
        assert!(
            has(1, 0) && has(1, 1),
            "the neighbour inside the floor band's reach"
        );
        assert!(has(0, 0) && has(0, 1), "AA at the shorter bands' level");
        assert!(!has(0, 2) && !has(1, 2));
        assert_eq!(b.bands, (BAND_FLOOR, BAND_MAX));
        assert!(!b.bound_added.exact_stored);
        assert_eq!(b.bound_added.blobs, 0, "{:?}", b.bound_added);
        assert!(b.collapsed.is_empty());
    }

    /// A ring whose quanta leave fewer than three distinct vertices is stored empty and counted
    /// (M4b commit 3): beside a 400 km square, a 0.02 km islet — at 1.5 km/pt a quantum is
    /// 0.075 km, so every vertex rounds to one — and a 0.16 × 0.01 km reef, whose ends round to
    /// two quanta apart and whose sides to the same ones, give empty second and third rings at
    /// every level, two `collapsed` per blob, and the frame draws nothing for them. Fails with
    /// the drop skipped (a one-vertex ring stored: `vertices_out` counts it) and with the
    /// threshold at two distinct vertices (the reef's two-point ring kept).
    #[test]
    fn a_collapsed_ring_is_stored_empty() {
        use crate::world::tests::{square, unit};
        let reef = {
            let l = Laea::new(0.0, 6.0);
            let c: Vec<(f64, f64)> = [
                (-0.08, -0.005),
                (0.08, -0.005),
                (0.08, 0.005),
                (-0.08, 0.005),
                (-0.08, -0.005),
            ]
            .iter()
            .map(|&(x, y)| l.inv(x, y).unwrap())
            .collect();
            Polygon::new(geo::LineString::from(c), vec![])
        };
        let aa = unit(
            "AAA",
            "AA",
            vec![square(0.0, 0.0, 400.0), square(0.0, 3.0, 0.02), reef],
        );
        let w = World::new(vec![aa], vec![], &[]).unwrap();
        let p = crate::world::plan_country("AA", &[0], &w, None, &[], false).unwrap();
        let pins = format::Pins {
            tool_git: "0".repeat(40),
            ne_tag: String::new(),
            inputs: vec![],
        };
        let b = build(&w, &[], &[p], &[], pins, Simplifier::Vw).unwrap();
        let land: Vec<&BlobOut> = b
            .blobs
            .iter()
            .filter(|x| x.owner == 0 && x.layer == Layer::Land)
            .collect();
        assert!(!land.is_empty());
        for x in &land {
            assert_eq!(x.rings.len(), 3, "every ring keeps its slot");
            assert!(x.rings[0].len() >= 4, "the square");
            assert!(x.rings[1].is_empty(), "the islet at {}", LADDER[x.level]);
            assert!(x.rings[2].is_empty(), "the reef at {}", LADDER[x.level]);
            assert_eq!(x.collapsed, 2);
            assert_eq!(x.vertices_out, x.rings[0].len());
        }
        assert_eq!(b.collapsed.len(), 2 * land.len());
        assert!(b.collapsed.iter().all(|&(u, _, ri)| u == 0 && ri >= 1));
        // the reef's quanta at 1.5 km/pt are two distinct points, the islet's one
        assert_eq!(distinct(&quantise_ring(&reef_km(), 1.5)), 2);
        // the file round-trips: an empty ring is a (0, 0) table entry, and the frame draws nothing
        let bytes = b.write(format::Encoding::Raw).unwrap();
        let s = format::Store::load(&bytes).unwrap();
        let blob = s.blob(0, 0, Layer::Land).unwrap();
        assert_eq!(s.rings(blob)[1].vertices, 0);
        let mut out = vec![[1.0, 1.0]];
        assert_eq!(s.decode(blob, 1, &mut out), Some(()));
        assert!(out.is_empty());
        let f = s
            .frame(0, &Pane::GOLDEN, s.fit(0, &Pane::GOLDEN).unwrap())
            .unwrap();
        assert_eq!(f.land.iter().map(|sh| sh.rings.len()).sum::<usize>(), 1);
        // three distinct quanta are kept: a 0.3 km triangle at 1.5 km/pt (quantum 0.075 km)
        assert_eq!(distinct(&[[0, 0], [4, 0], [0, 4]]), 3);
        assert_eq!(distinct(&[[0, 0], [4, 0], [0, 0]]), 2);
    }

    /// The reef of `a_collapsed_ring_is_stored_empty` in its own LAEA, km.
    fn reef_km() -> Vec<Coord<f64>> {
        [
            (-0.08, -0.005),
            (0.08, -0.005),
            (0.08, 0.005),
            (-0.08, 0.005),
            (-0.08, -0.005),
        ]
        .iter()
        .map(|&(x, y)| Coord { x, y })
        .collect()
    }

    /// The bound against the exact union (M4b commit 3). Country AA, 600 × 300 km: at the 140 pt
    /// floor its fit is 300 / 100 = 3.0 km/pt (level 1) and its level-0 reach 164 × 3 + 6 = 498 km
    /// wide, 70 × 3 + 6 = 216 tall; at 300 the width binds, 600 / 288 = 2.083, and the reach is
    /// 166 × 2.083 = 345.8 wide, 152 × 2.083 = 316.7 tall — the bounding rectangle is 498 × 316.7.
    /// The cap test works on ground discs around these rectangles: the bound's disc reaches ~597
    /// km from the centre, the widest band's (140's) ~549 km, and between them lies a 1 km `-99`
    /// neighbour no band's own disc holds. The bound adds that one blob — a third of this
    /// three-blob file, so the 5 % rule fires: `exact_stored`, and the neighbour has no blob. The
    /// rule itself, `store_exact_only`, flips at 5 % (on NE v5.1.2 the bound adds 0 blobs). Fails
    /// with the exact set computed from the bound (`blobs` 0, the neighbour stored), with the
    /// rule's factor off, and with the drop skipped.
    #[test]
    fn the_bound_adds_a_corner_neighbour_no_band_reaches() {
        use crate::world::tests::unit;
        let rect = |cx_km: f64, cy_km: f64, w: f64, h: f64, lon0: f64| {
            let l = Laea::new(0.0, lon0);
            let (hw, hh) = (w / 2.0, h / 2.0);
            let c: Vec<(f64, f64)> = [
                (cx_km - hw, cy_km - hh),
                (cx_km + hw, cy_km - hh),
                (cx_km + hw, cy_km + hh),
                (cx_km - hw, cy_km + hh),
                (cx_km - hw, cy_km - hh),
            ]
            .iter()
            .map(|&(x, y)| l.inv(x, y).unwrap())
            .collect();
            Polygon::new(geo::LineString::from(c), vec![])
        };
        let aa = || unit("AAA", "AA", vec![rect(0.0, 0.0, 600.0, 300.0, 0.0)]);
        // AA planned alone: the discs, then the neighbour between them
        let w0 = World::new(vec![aa()], vec![], &[]).unwrap();
        let p0 = crate::world::plan_country("AA", &[0], &w0, None, &[], false).unwrap();
        assert!((fit_at(&p0, BAND_FLOOR) - 3.0).abs() < 1e-9);
        assert!((p0.fit - 600.0 / 288.0).abs() < 1e-9);
        let l = p0.laea();
        let bound = reach(&p0, 0).unwrap();
        assert!(
            (bound[2] - 498.0).abs() < 1e-9 && (bound[3] - 152.0 * 600.0 / 288.0).abs() < 1e-9,
            "{bound:?}"
        );
        let r_bound = index::ground_cap(&l, bound).2;
        let r_band = bands_using(&p0, 0)
            .map(|h| index::ground_cap(&l, reach_at(&p0, 0, h)).2)
            .fold(0.0, f64::max);
        assert!(
            r_band > 540.0 && r_band < 560.0 && r_bound > 590.0,
            "{r_band} {r_bound}"
        );
        let d = (r_band + r_bound) / 2.0;
        let theta = bound[3].atan2(bound[2]);
        let (nx, ny) = (d * theta.cos(), d * theta.sin());
        let mut nb = unit("BBB", "--", vec![rect(nx, ny, 1.0, 1.0, 0.0)]);
        nb.code = None;
        let w = World::new(vec![aa(), nb], vec![], &[]).unwrap();
        let p = crate::world::plan_country("AA", &[0], &w, None, &[], false).unwrap();
        let pins = format::Pins {
            tool_git: "0".repeat(40),
            ne_tag: String::new(),
            inputs: vec![],
        };
        let b = build(&w, &[], &[p], &[], pins, Simplifier::Vw).unwrap();
        assert_eq!(b.bound_added.blobs, 1, "{:?}", b.bound_added);
        assert!(
            b.bound_added.bytes > 0
                && b.bound_added.bytes * 20 > b.bound_added.total_bytes
                && b.bound_added.exact_stored,
            "{:?}",
            b.bound_added
        );
        assert!(
            !b.blobs.iter().any(|x| x.owner == 1),
            "the bound's blob is dropped over 5 %"
        );
        assert!(b.blobs.iter().any(|x| x.owner == 0 && x.level == 0));
        assert!(!store_exact_only(5, 100) && store_exact_only(6, 100));
        assert!(!store_exact_only(0, 0));
    }

    /// k' is 1 at the centre and √2 at 90°. Fails inverted.
    #[test]
    fn k_prime_is_one_at_the_centre_and_root_two_at_90_degrees() {
        assert_eq!(k_prime(0.0), 1.0);
        assert!((k_prime(std::f64::consts::FRAC_PI_2) - 2f64.sqrt()).abs() < 1e-12);
    }

    /// A ring is stored open without repeated quanta. Fails if they are kept.
    #[test]
    fn a_ring_is_stored_open_without_repeated_quanta() {
        let c = |x: f64, y: f64| Coord { x, y };
        // level 1.5: a quantum is 0.075 km
        let ring = [
            c(0.0, 0.0),
            c(0.01, 0.0),
            c(1.5, 0.0),
            c(1.5, 1.5),
            c(0.0, 0.0),
        ];
        assert_eq!(quantise_ring(&ring, 1.5), vec![[0, 0], [20, 0], [20, 20]]);
    }
}
