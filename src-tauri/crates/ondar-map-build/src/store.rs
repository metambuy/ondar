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
//! margin), s_max(k) = min(L_{k+1}, max(fit, 1.5)). Every unit with a ring whose cap meets that
//! reach (with the level's tolerance) is stored at k; and every unit in an inset at the inset's
//! level.

use crate::borders;
use crate::geom;
use crate::simplify;
use crate::world::{CountryPlan, GroupRole, World};
use geo::{Coord, Polygon};
use ondar_map::codec::QUANTUM_PT;
use ondar_map::format::{self, BlobIn, Cap, Layer};
use ondar_map::laea::{Laea, R_AUTHALIC_KM, haversine_km};
use ondar_map::rules::{self, LADDER, Pane};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Land is simplified to within this of the original, points at its level (R3).
pub const LAND_TOL_PT: f64 = 0.25;
/// Subdivisions, the same (S7).
pub const SUB_TOL_PT: f64 = 0.5;
/// Subdivisions are drawn above 8 km/pt, so stored from the level 6 views use (index 2).
pub const SUB_FIRST_LEVEL: usize = 2;
/// Half a quantum's diagonal: what the codec adds to a stored bound, points.
pub const QUANT_PT: f64 = QUANTUM_PT * std::f64::consts::FRAC_1_SQRT_2;

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
    /// For P4's question: RDP at the same tolerance, vertices (open) and rings it leaves
    /// invalid (self-intersecting, or fewer than three vertices) — what a repair would face.
    pub rdp_vertices: usize,
    pub rdp_invalid: usize,
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
    pub p4: (usize, usize, usize),
    pub reach: Reach,
    pub seconds: f64,
}

/// LAEA's tangential scale factor at angular distance `c` (radians) from the centre; the radial
/// one is its inverse, so a small displacement is stretched by at most this.
fn k_prime(c: f64) -> f64 {
    (2.0 / (1.0 + c.cos())).sqrt()
}

/// The ground cap (lon, lat, radius km) around a rectangle of a projection: its centre's
/// inverse and the farthest of 64 samples per edge. `None` radius = the whole sphere (the
/// rectangle leaves the projection's disc).
fn rect_cap(l: &Laea, [x0, y0, x1, y1]: [f64; 4]) -> (f64, f64, f64) {
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

fn caps_meet(c: &Cap, (lon, lat, r): (f64, f64, f64), tol: f64) -> bool {
    haversine_km(f64::from(c.lon), f64::from(c.lat), lon, lat) <= f64::from(c.radius_km) + r + tol
}

/// How far a view can reach, by the clamp's two readings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The plan's (§ 2, D6 as written): the view's centre anywhere in the fit rectangle, so a
    /// view at scale s reaches half a view, (W/2 + 2) × s, beyond it.
    Plan,
    /// The view itself inside the fit rectangle ("at fit the view is the fit and cannot pan"):
    /// only the 2 pt clip margin beyond it.
    Fit,
}

/// The reach rectangle of country `p` at level `k`, in its frame LAEA, km.
pub fn reach(p: &CountryPlan, k: usize, mode: Reach) -> [f64; 4] {
    let pane = Pane::GOLDEN;
    let [bx0, by0, bx1, by1] = p.bbox;
    let (cx, cy) = ((bx0 + bx1) / 2.0, (by0 + by1) / 2.0);
    let top = rules::initial_scale(p.fit);
    let s_max = LADDER.get(k + 1).copied().unwrap_or(f64::INFINITY).min(top);
    // the fit rectangle: the pane at the fit, or at the floor for a country finer than it
    let (hw, hh) = (pane.width / 2.0 * top, pane.height / 2.0 * top);
    let m = match mode {
        Reach::Plan => (pane.width.max(pane.height) / 2.0 + 2.0) * s_max,
        Reach::Fit => 2.0 * s_max,
    };
    [cx - hw - m, cy - hh - m, cx + hw + m, cy + hh + m]
}

/// The country's top level: the one its widest view (the fit, or the floor) uses.
pub fn top_level(p: &CountryPlan) -> usize {
    rules::level_for(rules::initial_scale(p.fit))
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

fn job<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
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
    mode: Reach,
) -> Result<Built, String> {
    let t0 = std::time::Instant::now();
    let nu = world.units.len();

    // storage LAEA per unit
    let mut centre: Vec<Option<(f64, f64)>> = vec![None; nu];
    for p in plans {
        if let Some(&main) = p.units.first() {
            centre[main] = Some((p.lat0, p.lon0));
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

    // coverage: (unit, level) needed, and whether by its own country
    let mut need = vec![[false; LADDER.len()]; nu];
    let mut own_need = vec![[false; LADDER.len()]; nu];
    // which (plan, level) each unit's rings were found in, for P2
    let mut drawn_by: Vec<Vec<(usize, usize)>> = vec![Vec::new(); nu];
    for (pi, p) in plans.iter().enumerate() {
        let l = p.laea();
        for k in 0..=top_level(p) {
            let rc = rect_cap(&l, reach(p, k, mode));
            let tol = (LAND_TOL_PT + QUANT_PT) * LADDER[k];
            for (u, g) in geoms.iter().enumerate() {
                if !caps_meet(&world_cap(g), rc, tol)
                    || !g.rings().any(|r| caps_meet(&r.cap, rc, tol))
                {
                    continue;
                }
                need[u][k] = true;
                if p.units.contains(&u) {
                    own_need[u][k] = true;
                }
                if p.units.first() != Some(&u) {
                    drawn_by[u].push((pi, k));
                }
            }
        }
        for ins in &p.insets {
            let k = rules::level_for(ins.scale);
            for &gp in &p.groups[ins.group].parts {
                let (u, _) = p.parts[gp];
                need[u][k] = true;
                own_need[u][k] = true;
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
            let rect = reach(p, k, mode);
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
            rdp_vertices: 0,
            rdp_invalid: 0,
        };
        for r in geoms[u].rings() {
            let (rv, valid) = simplify::rdp_ring(&r.km, t);
            b.rdp_vertices += rv;
            b.rdp_invalid += usize::from(!valid);
            let tuned = simplify::tune(&r.km, true, t);
            b.vertices_in += r.km.len().saturating_sub(1);
            let q = quantise_ring(&tuned.line, LADDER[k]);
            b.vertices_out += q.len();
            b.bound_pt = b.bound_pt.max(tuned.bound / LADDER[k]);
            b.ring_bounds_pt.push(tuned.bound / LADDER[k]);
            b.evaluations += tuned.evaluations;
            b.rings.push(q);
        }
        b
    });

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
        blob.map_or((0, 0, 0), |b| {
            (b.vertices_in, b.vertices_out, b.rdp_vertices)
        })
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
            rdp_vertices: 0,
            rdp_invalid: 0,
        };
        for (line, _) in &s.lines {
            let closed = line.len() > 3 && line.first() == line.last();
            let tuned = simplify::tune(line, closed, t);
            b.vertices_in += line.len();
            let q: Vec<[i32; 2]> = if closed {
                quantise_ring(&tuned.line, LADDER[k])
            } else {
                let mut q: Vec<[i32; 2]> = Vec::new();
                for c in &tuned.line {
                    if let Some(v) = ondar_map::codec::quantise([c.x, c.y], LADDER[k])
                        && q.last() != Some(&v)
                    {
                        q.push(v);
                    }
                }
                q
            };
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
                    scale: i.scale,
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
        golden_pane: Pane::GOLDEN,
        radius_km: R_AUTHALIC_KM,
        ladder: LADDER.to_vec(),
    };
    Ok(Built {
        header,
        units,
        countries,
        blobs,
        subs,
        p2,
        p4,
        reach: mode,
        seconds: t0.elapsed().as_secs_f64(),
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

    /// The plan's reach grows the fit rectangle by (W/2 + 2) × the coarsest scale that uses the
    /// level, capped at the country's widest view; the fit reading by the 2 pt margin only. A
    /// 288 × 260 km country fits at 1 km/pt: its widest view is the floor's, 1.5, at level 0.
    #[test]
    fn the_reach_follows_the_clamp() {
        let p = CountryPlan {
            code: "AA".into(),
            name: String::new(),
            units: vec![],
            parts: vec![],
            groups: vec![],
            frame_group: 0,
            lat0: 0.0,
            lon0: 0.0,
            bbox: [-144.0, -130.0, 144.0, 130.0],
            fit: 1.0,
            overridden: false,
            alias: false,
            subdivisions: false,
            insets: vec![],
        };
        assert_eq!(top_level(&p), 0);
        // the fit rectangle is the pane at the floor (fit 1 < 1.5): 246 × 225 km half
        assert_eq!(
            reach(&p, 0, Reach::Plan),
            [-246.0 - 249.0, -225.0 - 249.0, 246.0 + 249.0, 225.0 + 249.0]
        );
        assert_eq!(reach(&p, 0, Reach::Fit), [-249.0, -228.0, 249.0, 228.0]);
        // fit 20: level 0's views are below 3 km/pt, level 3's below 24 but never past 20
        let q = CountryPlan {
            bbox: [-2880.0, -2600.0, 2880.0, 2600.0],
            fit: 20.0,
            ..p
        };
        assert_eq!(top_level(&q), 3);
        let r0 = reach(&q, 0, Reach::Plan);
        assert_eq!(r0[2], 164.0 * 20.0 + 166.0 * 3.0);
        let r3 = reach(&q, 3, Reach::Plan);
        assert_eq!(r3[2], 164.0 * 20.0 + 166.0 * 20.0);
        assert_eq!(reach(&q, 3, Reach::Fit)[2], 164.0 * 20.0 + 2.0 * 20.0);
    }

    #[test]
    fn k_prime_is_one_at_the_centre_and_root_two_at_90_degrees() {
        assert_eq!(k_prime(0.0), 1.0);
        assert!((k_prime(std::f64::consts::FRAC_PI_2) - 2f64.sqrt()).abs() < 1e-12);
    }

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
