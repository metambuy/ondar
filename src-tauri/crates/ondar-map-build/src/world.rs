//! The frame rules over the whole of Natural Earth: the 267 stored units (258 admin 0 + the nine
//! S4 map units), the seam stitched (R9), and per country (248 codes) its groups, frame group,
//! centre (R1), fit, roles, insets (S6) and flags. Everything here is deterministic: the units
//! in file order, the countries by code.

use crate::geom;
use crate::ne::{Admin1, Unit};
use crate::seam::{self, Stitch};
use crate::tables::{Alias, InsetRow, Override};
use geo::{Area, ChamberlainDuquetteArea, Contains, Coord, Distance, Euclidean, Point, Polygon};
use ondar_map::format::Corner;
use ondar_map::laea::Laea;
use ondar_map::rules::{self, BAND_FLOOR, BAND_MAX, Pane};
use ondar_map::{clip, index};
use std::collections::{BTreeMap, BTreeSet};

/// Parts closer than this on the ground chain into one group (the survey's rule).
pub const CHAIN_KM: f64 = 300.0;
/// S6: a group outside the frame this large must be an inset (listed in `insets.tsv`).
pub const INSET_MIN_KM2: f64 = 1000.0;
/// An `insets.tsv` anchor must be within this of its group's nearest part.
pub const INSET_ANCHOR_MAX_KM: f64 = 100.0;

pub struct World {
    pub units: Vec<Unit>,
    /// Per stitched unit: its a3 and what the stitch did.
    pub stitched: Vec<(String, Stitch)>,
    /// Seam edges left anywhere after stitching (the R9 check: must be 0).
    pub seam_edges_left: usize,
}

impl World {
    /// Admin 0 then the S4 map units (in `aliases.tsv` order), every unit on the seam stitched.
    pub fn new(
        admin0: Vec<Unit>,
        map_units: Vec<Unit>,
        aliases: &[Alias],
    ) -> Result<World, String> {
        let mut units = admin0;
        for a in aliases {
            let mu = map_units.iter().find(|u| u.a3 == a.gu_a3).ok_or(format!(
                "map unit {} ({}) not in the layer",
                a.gu_a3, a.code
            ))?;
            if mu.code.as_deref() != Some(a.code.as_str()) || mu.parent_a3 != a.parent_a3 {
                return Err(format!(
                    "map unit {}: ISO_A2_EH {:?} parent {} — aliases.tsv says {} {}",
                    a.gu_a3, mu.code, mu.parent_a3, a.code, a.parent_a3
                ));
            }
            units.push(mu.clone());
        }
        let mut stitched = Vec::new();
        for u in &mut units {
            if u.a3 == "ATA" {
                let (parts, st) = seam::strip_polar(std::mem::take(&mut u.parts));
                u.parts = parts;
                stitched.push((u.a3.clone(), st));
            } else if seam::seam_edges(&u.parts) > 0 {
                let (parts, st) = seam::union_seam(std::mem::take(&mut u.parts));
                u.parts = parts;
                stitched.push((u.a3.clone(), st));
            }
        }
        let seam_edges_left = units.iter().map(|u| seam::seam_edges(&u.parts)).sum();
        Ok(World {
            units,
            stitched,
            seam_edges_left,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GroupRole {
    Frame,
    Inset(usize),
    Dropped,
}

#[derive(Clone, Debug)]
pub struct Group {
    /// Indices into `CountryPlan::parts`.
    pub parts: Vec<usize>,
    /// Planar area in the frame's LAEA (equal-area), km².
    pub area_km2: f64,
    /// Projected bbox in the frame's LAEA, km.
    pub bbox: [f64; 4],
    pub role: GroupRole,
}

#[derive(Clone, Debug)]
pub struct InsetPlan {
    pub row: InsetRow,
    pub group: usize,
    pub anchor_km: f64,
    pub lat0: f64,
    pub lon0: f64,
    pub centre_km: [f64; 2],
    /// The group's projected bbox size in its own LAEA, km (what the box's area is fitted to).
    pub size_km: [f64; 2],
    /// The scale at the golden box, km/pt.
    pub scale: f64,
    pub clearance_pt: f64,
    /// I1 (M4b commit 4): the box's scale in whole percent at each band height
    /// `BAND_FLOOR..=BAND_MAX` (index `h − BAND_FLOOR`), placed in table order after the controls'
    /// rect; 0 = dropped at that band.
    pub scale_pct: Vec<u8>,
    /// The corner table: this box alone (the controls placed) at TL, TR and BL.
    pub corners: Vec<CornerChoice>,
}

/// One row of the corner table (M4b commit 4): the golden row's box moved to `corner` with its
/// own gaps, placed alone after the controls' rect at every band.
#[derive(Clone, Debug, PartialEq)]
pub struct CornerChoice {
    pub corner: Corner,
    /// The moved golden rect.
    pub rect: [f64; 4],
    /// The smallest scale over the bands and the first height it occurs at.
    pub min_pct: u8,
    pub min_at: u32,
    pub pct_161: u8,
    pub pct_178: u8,
    pub pct_300: u8,
    /// The full-size box's distance to the land at 328 × 161 (Step 0's figure, review P6).
    pub clearance_161: f64,
}

/// What `inset_tables` computes for a country: per inset, the per-band scales and the corner
/// table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InsetTables {
    pub scale_pct: Vec<Vec<u8>>,
    pub corners: Vec<Vec<CornerChoice>>,
}

/// The three corners an inset may use (C1: the bottom-right is the controls').
pub const INSET_CORNERS: [Corner; 3] = [Corner::TopLeft, Corner::TopRight, Corner::BottomLeft];

/// The land the frame draws at a band's fit view, in pane points, clipped to `index::clip_rect`
/// (review 2, finding 2; one rule with the frame): the frame's groups and the dropped ones.
fn land_at(bbox: [f64; 4], land_km: &[Vec<[f64; 2]>], h: u32) -> Vec<Vec<[f64; 2]>> {
    let pane = Pane::band(h);
    let fit = rules::fit_scale(bbox[2] - bbox[0], bbox[3] - bbox[1], &pane).unwrap_or(1.0);
    let s0 = rules::initial_scale(fit);
    let (cx, cy) = ((bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0);
    let clip_pt = index::clip_rect(&pane);
    land_km
        .iter()
        .map(|r| {
            let pts: Vec<[f64; 2]> = r
                .iter()
                .map(|&[x, y]| {
                    [
                        (x - cx) / s0 + pane.width / 2.0,
                        (cy - y) / s0 + pane.height / 2.0,
                    ]
                })
                .collect();
            clip::clip_ring(&pts, &clip_pt)
        })
        .filter(|r| r.len() >= 3)
        .collect()
}

/// A box's distance to the land, points (S6).
fn clearance_of(land: &[Vec<[f64; 2]>], [x, y, w, h]: [f64; 4]) -> f64 {
    land.iter()
        .map(|ring| rules::rect_ring_distance([x, y, x + w, y + h], ring.iter().copied()))
        .fold(f64::INFINITY, f64::min)
}

/// I1's placement test: inside the pane, apart from every box placed before it, ≥ 12 pt from the
/// land.
fn placeable(land: &[Vec<[f64; 2]>], pane: &Pane, placed: &[[f64; 4]], r: [f64; 4]) -> bool {
    rules::box_fits(r, pane)
        && placed.iter().all(|&o| rules::boxes_apart(r, o))
        && clearance_of(land, r) >= rules::INSET_CLEARANCE_PT
}

/// The largest scale in whole percent, from 100 down to the row's minimum (`inset_min_scale`), at
/// which the golden `rect` anchored at `corner` is placeable; 0 if none. A smaller box at the same
/// corner is a subset of a larger one, so placeability is monotone and a bisection finds it.
fn largest_pct(
    land: &[Vec<[f64; 2]>],
    pane: &Pane,
    placed: &[[f64; 4]],
    rect: [f64; 4],
    corner: Corner,
) -> u8 {
    let lo = (rules::inset_min_scale(rect) * 100.0).ceil();
    if !(1.0..=100.0).contains(&lo) {
        return 0;
    }
    let lo = lo as u8;
    let ok = |pct: u8| {
        placeable(
            land,
            pane,
            placed,
            rules::inset_box_at(rect, corner, pane, f64::from(pct) / 100.0),
        )
    };
    if !ok(lo) {
        return 0;
    }
    if ok(100) {
        return 100;
    }
    let (mut a, mut b) = (lo, 100u8);
    while b - a > 1 {
        let m = a + (b - a) / 2;
        if ok(m) {
            a = m;
        } else {
            b = m;
        }
    }
    a
}

/// I1 + C1 (M4b commit 4): for every band height, the land the frame draws there, the controls'
/// rect placed first (when `controls`; the review P6 comparison with Step 0 disables it), then
/// each inset row in table order at the largest placeable scale — and, per row, the corner table
/// (the row alone after the controls, at TL, TR and BL with its own gaps).
pub fn inset_tables(
    bbox: [f64; 4],
    land_km: &[Vec<[f64; 2]>],
    rows: &[&InsetRow],
    controls: bool,
) -> InsetTables {
    if rows.is_empty() {
        return InsetTables::default();
    }
    let n = usize::try_from(BAND_MAX - BAND_FLOOR + 1).unwrap_or(0);
    // per band: the table-order scales, and per row per corner (pct, full-box clearance)
    let per_band: Vec<(Vec<u8>, Vec<[(u8, f64); 3]>)> = crate::store::job(n, |i| {
        let h = BAND_FLOOR + i as u32;
        let pane = Pane::band(h);
        let land = land_at(bbox, land_km, h);
        let base: Vec<[f64; 4]> = if controls {
            vec![rules::controls_rect(&pane)]
        } else {
            Vec::new()
        };
        let mut placed = base.clone();
        let mut pcts = Vec::with_capacity(rows.len());
        for row in rows {
            let pct = largest_pct(&land, &pane, &placed, row.rect, row.corner);
            if pct > 0 {
                placed.push(rules::inset_box_at(
                    row.rect,
                    row.corner,
                    &pane,
                    f64::from(pct) / 100.0,
                ));
            }
            pcts.push(pct);
        }
        let corners = rows
            .iter()
            .map(|row| {
                INSET_CORNERS.map(|c| {
                    let rect = rules::inset_rect_at_corner(row.rect, row.corner, c);
                    (
                        largest_pct(&land, &pane, &base, rect, c),
                        clearance_of(&land, rules::inset_box_at(rect, c, &pane, 1.0)),
                    )
                })
            })
            .collect();
        (pcts, corners)
    });
    let at = |h: u32| usize::try_from(h - BAND_FLOOR).unwrap_or(0);
    let scale_pct = (0..rows.len())
        .map(|k| per_band.iter().map(|(p, _)| p[k]).collect())
        .collect();
    let corners = rows
        .iter()
        .enumerate()
        .map(|(k, row)| {
            INSET_CORNERS
                .iter()
                .enumerate()
                .map(|(ci, &corner)| {
                    let series: Vec<(u8, f64)> = per_band.iter().map(|(_, c)| c[k][ci]).collect();
                    let (min_i, &(min_pct, _)) = series
                        .iter()
                        .enumerate()
                        .min_by_key(|&(_, &(p, _))| p)
                        .unwrap_or((0, &(0, 0.0)));
                    CornerChoice {
                        corner,
                        rect: rules::inset_rect_at_corner(row.rect, row.corner, corner),
                        min_pct,
                        min_at: BAND_FLOOR + min_i as u32,
                        pct_161: series.get(at(161)).map_or(0, |x| x.0),
                        pct_178: series.get(at(178)).map_or(0, |x| x.0),
                        pct_300: series.get(at(300)).map_or(0, |x| x.0),
                        clearance_161: series.get(at(161)).map_or(f64::NAN, |x| x.1),
                    }
                })
                .collect()
        })
        .collect();
    InsetTables { scale_pct, corners }
}

/// `inset_tables` for a planned country, its land re-projected from the world (the frame's groups
/// and the dropped ones, in the frame's LAEA).
pub fn inset_tables_for(p: &CountryPlan, world: &World, controls: bool) -> InsetTables {
    let l = p.laea();
    let land_km: Vec<Vec<[f64; 2]>> = p
        .groups
        .iter()
        .filter(|g| matches!(g.role, GroupRole::Frame | GroupRole::Dropped))
        .flat_map(|g| g.parts.iter())
        .flat_map(|&i| {
            let (u, part) = p.parts[i];
            geom::rings(&world.units[u].parts[part])
                .map(|r| {
                    geom::project_ring(r, &l)
                        .0
                        .iter()
                        .map(|c| [c.x, c.y])
                        .collect()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let rows: Vec<&InsetRow> = p.insets.iter().map(|i| &i.row).collect();
    inset_tables(p.bbox, &land_km, &rows, controls)
}

/// What must hold before the resource ships (M4b commit 4; the CLI refuses the build on either
/// unless told otherwise): no inset dropped at 328 × 178 or × 300 (the brief's STOP), and no
/// label wider than its box's inner width there (review P3: labels are never clipped).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShipGate {
    pub dropped: Vec<String>,
    pub wide_labels: Vec<String>,
}

pub fn ship_gate(plans: &[CountryPlan]) -> ShipGate {
    let mut g = ShipGate::default();
    for p in plans {
        for ins in &p.insets {
            let width = rules::label_width_pt(&ins.row.label);
            for h in [178u32, 300] {
                let pct = ins
                    .scale_pct
                    .get(usize::try_from(h - BAND_FLOOR).unwrap_or(0))
                    .copied()
                    .unwrap_or(0);
                if pct == 0 {
                    g.dropped.push(format!(
                        "{} {}: dropped at 328 × {h}",
                        p.code, ins.row.label
                    ));
                    continue;
                }
                let rect = rules::inset_box_at(
                    ins.row.rect,
                    ins.row.corner,
                    &Pane::band(h),
                    f64::from(pct) / 100.0,
                );
                let inner = rules::label_inner_width(rect);
                if width > inner {
                    g.wide_labels.push(format!(
                        "{} {}: the label is {width:.1} pt, the box's inner width {inner:.1} at 328 × {h} ({pct} %)",
                        p.code, ins.row.label
                    ));
                }
            }
        }
    }
    g
}

#[derive(Clone, Debug)]
pub struct CountryPlan {
    pub code: String,
    pub name: String,
    /// Unit indices (into `World::units`), the main unit first.
    pub units: Vec<usize>,
    /// (unit, part) for every part of the country's units.
    pub parts: Vec<(usize, usize)>,
    pub groups: Vec<Group>,
    pub frame_group: usize,
    pub lat0: f64,
    pub lon0: f64,
    pub bbox: [f64; 4],
    pub fit: f64,
    pub overridden: bool,
    pub alias: bool,
    pub subdivisions: bool,
    pub insets: Vec<InsetPlan>,
}

impl CountryPlan {
    pub fn laea(&self) -> Laea {
        Laea::new(self.lat0, self.lon0)
    }

    pub fn initial(&self) -> f64 {
        rules::initial_scale(self.fit)
    }

    pub fn level(&self) -> usize {
        rules::level_for(self.fit)
    }

    pub fn role_of_part(&self, part: usize) -> GroupRole {
        self.groups
            .iter()
            .find(|g| g.parts.contains(&part))
            .map(|g| g.role)
            .unwrap_or(GroupRole::Dropped)
    }
}

struct Dsu(Vec<usize>);
impl Dsu {
    fn find(&mut self, i: usize) -> usize {
        let mut r = i;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut j = i;
        while self.0[j] != r {
            let n = self.0[j];
            self.0[j] = r;
            j = n;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

/// Single linkage at `chain_km` on the ground: each pair's polygon distance in an LAEA centred on
/// the smaller part (by vertices), after a bbox prefilter in the grouping projection `gproj` with
/// a factor-2 slack (the prototype's rule — the reference since Step 0, round 2).
pub fn group_parts(gproj: &[Polygon<f64>], ll: &[&Polygon<f64>], chain_km: f64) -> Vec<Vec<usize>> {
    let n = gproj.len();
    let sizes: Vec<usize> = ll.iter().map(|p| p.exterior().0.len()).collect();
    let bbs: Vec<Option<[f64; 4]>> = gproj.iter().map(|p| geom::bbox([p])).collect();
    let mut d = Dsu((0..n).collect());
    for i in 0..n {
        for j in i + 1..n {
            let (Some(a), Some(b)) = (bbs[i], bbs[j]) else {
                continue;
            };
            let gx = (b[0] - a[2]).max(a[0] - b[2]).max(0.0);
            let gy = (b[1] - a[3]).max(a[1] - b[3]).max(0.0);
            if gx.hypot(gy) >= chain_km * 2.0 || d.find(i) == d.find(j) {
                continue;
            }
            let (small, big) = if sizes[i] <= sizes[j] { (i, j) } else { (j, i) };
            let Some((la, lo)) = geom::lonlat_centre([ll[small]]) else {
                continue;
            };
            let l = Laea::new(la, lo);
            let dist =
                Euclidean.distance(&geom::project(ll[small], &l), &geom::project(ll[big], &l));
            if dist < chain_km {
                d.union(i, j);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = d.find(i);
        groups.entry(r).or_default().push(i);
    }
    groups.into_values().collect()
}

/// Distance on the ground (km) from a point to a lon/lat polygon: 0 inside.
pub fn point_part_km(lat: f64, lon: f64, p: &Polygon<f64>) -> f64 {
    let l = Laea::new(lat, lon);
    let q = geom::project(p, &l);
    let o = Point::new(0.0, 0.0);
    if q.contains(&o) {
        0.0
    } else {
        Euclidean.distance(&o, &q)
    }
}

/// Every country's plan: the 239 admin-0 codes and the nine aliases, by code.
pub fn plan_all(
    world: &World,
    admin1: &[Admin1],
    overrides: &[Override],
    inset_rows: &[InsetRow],
    aliases: &[Alias],
) -> Result<Vec<CountryPlan>, String> {
    let mut by_code: BTreeMap<String, (Vec<usize>, bool)> = BTreeMap::new();
    let n_admin0 = world.units.len() - aliases.len();
    for (i, u) in world.units.iter().enumerate().take(n_admin0) {
        if let Some(c) = &u.code {
            by_code.entry(c.clone()).or_default().0.push(i);
        }
    }
    for (k, a) in aliases.iter().enumerate() {
        if by_code
            .insert(a.code.clone(), (vec![n_admin0 + k], true))
            .is_some()
        {
            return Err(format!("alias {} is also an admin-0 code", a.code));
        }
    }
    let a1_units: BTreeSet<&str> = admin1.iter().map(|a| a.adm0_a3.as_str()).collect();
    let mut plans = Vec::with_capacity(by_code.len());
    for (code, (units, alias)) in &by_code {
        let ov = overrides.iter().find(|o| &o.code == code);
        let rows: Vec<&InsetRow> = inset_rows.iter().filter(|r| &r.code == code).collect();
        let mut p = plan_country(code, units, world, ov, &rows, *alias)?;
        p.subdivisions = p.fit > rules::SUBDIVISIONS_ABOVE_KM_PER_PT
            && p.units
                .iter()
                .any(|&u| a1_units.contains(world.units[u].a3.as_str()));
        plans.push(p);
    }
    for o in overrides {
        if !by_code.contains_key(&o.code) {
            return Err(format!("overrides.tsv: no country {}", o.code));
        }
    }
    for r in inset_rows {
        if !by_code.contains_key(&r.code) {
            return Err(format!("insets.tsv: no country {}", r.code));
        }
    }
    Ok(plans)
}

/// One country's plan. Refuses (an `Err` naming the rule) an override anchor in no group, an
/// inset row with no group within 100 km or sharing a group, an unlisted group ≥ 1 000 km², and
/// an inset box outside the pane, overlapping another, or within 12 pt of the land.
pub fn plan_country(
    code: &str,
    units: &[usize],
    world: &World,
    ov: Option<&Override>,
    rows: &[&InsetRow],
    alias: bool,
) -> Result<CountryPlan, String> {
    let mut parts: Vec<(usize, usize)> = Vec::new();
    for &u in units {
        for p in 0..world.units[u].parts.len() {
            parts.push((u, p));
        }
    }
    let ll: Vec<&Polygon<f64>> = parts
        .iter()
        .map(|&(u, p)| &world.units[u].parts[p])
        .collect();
    if ll.is_empty() {
        return Err(format!("{code}: no parts"));
    }
    let areas: Vec<f64> = ll
        .iter()
        .map(|p| p.chamberlain_duquette_unsigned_area() / 1e6)
        .collect();
    let largest = (0..ll.len())
        .max_by(|&a, &b| areas[a].total_cmp(&areas[b]).then(b.cmp(&a)))
        .unwrap_or(0);
    let (glat, glon) = geom::lonlat_centre([ll[largest]]).ok_or(format!("{code}: empty part"))?;
    let gl = Laea::new(glat, glon);
    let gproj: Vec<Polygon<f64>> = ll.iter().map(|p| geom::project(p, &gl)).collect();
    let groups_idx = group_parts(&gproj, &ll, CHAIN_KM);
    let group_of = |part: usize| {
        groups_idx
            .iter()
            .position(|g| g.contains(&part))
            .unwrap_or(0)
    };

    let frame_group = match ov {
        Some(o) => {
            let pt = Point::new(o.lon, o.lat);
            let hit = (0..ll.len()).find(|&i| ll[i].contains(&pt)).ok_or(format!(
                "{code}: override anchor ({}, {}) is in no part",
                o.lat, o.lon
            ))?;
            group_of(hit)
        }
        None => group_of(largest),
    };
    let frame_ll: Vec<&Polygon<f64>> = groups_idx[frame_group].iter().map(|&i| ll[i]).collect();
    let is_aq = units.iter().any(|&u| world.units[u].a3 == "ATA");
    let (lat0, lon0) = if is_aq {
        (-90.0, 0.0)
    } else {
        geom::lonlat_centre(frame_ll.iter().copied()).ok_or(format!("{code}: empty frame"))?
    };
    let l = Laea::new(lat0, lon0);
    let proj: Vec<Polygon<f64>> = ll.iter().map(|p| geom::project(p, &l)).collect();
    let bbox = geom::bbox(groups_idx[frame_group].iter().map(|&i| &proj[i]))
        .ok_or(format!("{code}: empty frame"))?;
    let pane = Pane::GOLDEN;
    let fit = rules::fit_scale(bbox[2] - bbox[0], bbox[3] - bbox[1], &pane)
        .ok_or(format!("{code}: no fit"))?;
    let (cx, cy) = ((bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0);
    let (uw, uh) = pane.usable();
    let usable = [
        cx - uw / 2.0 * fit,
        cy - uh / 2.0 * fit,
        cx + uw / 2.0 * fit,
        cy + uh / 2.0 * fit,
    ];

    let mut groups: Vec<Group> = groups_idx
        .iter()
        .enumerate()
        .map(|(g, ps)| {
            let gb = geom::bbox(ps.iter().map(|&i| &proj[i])).unwrap_or([0.0; 4]);
            let inside = gb[0] >= usable[0]
                && gb[1] >= usable[1]
                && gb[2] <= usable[2]
                && gb[3] <= usable[3];
            Group {
                parts: ps.clone(),
                area_km2: ps.iter().map(|&i| proj[i].unsigned_area()).sum(),
                bbox: gb,
                role: if g == frame_group || inside {
                    GroupRole::Frame
                } else {
                    GroupRole::Dropped
                },
            }
        })
        .collect();

    // S6: the rows matched to the nearest non-frame group
    let mut insets = Vec::new();
    for row in rows {
        // C1: the bottom-right corner is the controls' at every band
        if row.corner == Corner::BottomRight {
            return Err(format!(
                "{code} {}: the bottom-right corner is the controls' (C1); use top-left, top-right or bottom-left",
                row.label
            ));
        }
        let mut best: Option<(f64, usize)> = None;
        for (g, grp) in groups.iter().enumerate() {
            if grp.role == GroupRole::Frame {
                continue;
            }
            let d = grp
                .parts
                .iter()
                .map(|&i| point_part_km(row.lat, row.lon, ll[i]))
                .fold(f64::INFINITY, f64::min);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, g));
            }
        }
        let (d, g) = best.ok_or(format!("{code} {}: no group outside the frame", row.label))?;
        if d > INSET_ANCHOR_MAX_KM {
            return Err(format!(
                "{code} {}: nearest group is {d:.1} km from the anchor (> 100)",
                row.label
            ));
        }
        if let GroupRole::Inset(k) = groups[g].role {
            return Err(format!(
                "{code}: rows {} and {} match one group",
                insets_label(&insets, k),
                row.label
            ));
        }
        groups[g].role = GroupRole::Inset(insets.len());
        let gll: Vec<&Polygon<f64>> = groups[g].parts.iter().map(|&i| ll[i]).collect();
        let (ilat, ilon) = geom::lonlat_centre(gll.iter().copied()).ok_or("empty inset")?;
        let il = Laea::new(ilat, ilon);
        let ib = geom::bbox(
            gll.iter()
                .map(|p| geom::project(p, &il))
                .collect::<Vec<_>>()
                .iter(),
        )
        .ok_or("empty inset")?;
        let scale = rules::inset_scale(ib[2] - ib[0], ib[3] - ib[1], row.rect)
            .ok_or(format!("{code} {}: the box has no area", row.label))?;
        insets.push(InsetPlan {
            row: (*row).clone(),
            group: g,
            anchor_km: d,
            lat0: ilat,
            lon0: ilon,
            centre_km: [(ib[0] + ib[2]) / 2.0, (ib[1] + ib[3]) / 2.0],
            size_km: [ib[2] - ib[0], ib[3] - ib[1]],
            scale,
            clearance_pt: f64::NAN,
            scale_pct: Vec::new(),
            corners: Vec::new(),
        });
    }
    let unlisted: Vec<String> = groups
        .iter()
        .filter(|g| g.role == GroupRole::Dropped && g.area_km2 >= INSET_MIN_KM2)
        .map(|g| format!("{:.0} km²", g.area_km2))
        .collect();
    if !unlisted.is_empty() {
        return Err(format!(
            "{code}: groups ≥ 1 000 km² outside the frame and not in insets.tsv: {}",
            unlisted.join(", ")
        ));
    }

    // the boxes: inside the pane, apart, and ≥ 12 pt from the land at the initial view
    let s0 = rules::initial_scale(fit);
    let to_pt = |c: &Coord<f64>| -> [f64; 2] {
        [
            (c.x - cx) / s0 + pane.width / 2.0,
            (cy - c.y) / s0 + pane.height / 2.0,
        ]
    };
    // the land the frame draws at the initial view, which `Store::inset_clearance` measures
    // (review 2, finding 2): the frame's groups and the small ones it drops, clipped as the
    // frame clips them — to `index::clip_rect`, the frame's own (review 3, finding 5) — so a
    // dropped group off the pane (Jan Mayen, 6 pt above Norway's) counts no more here than on
    // screen
    let clip_pt = index::clip_rect(&pane);
    let land_rings: Vec<Vec<[f64; 2]>> = groups
        .iter()
        .filter(|g| matches!(g.role, GroupRole::Frame | GroupRole::Dropped))
        .flat_map(|g| g.parts.iter())
        .flat_map(|&i| {
            geom::rings(&proj[i])
                .map(|r| {
                    let pts: Vec<[f64; 2]> = r.0.iter().map(to_pt).collect();
                    clip::clip_ring(&pts, &clip_pt)
                })
                .filter(|r| r.len() >= 3)
                .collect::<Vec<_>>()
        })
        .collect();
    let clear = |r: [f64; 4]| {
        land_rings
            .iter()
            .map(|ring| rules::rect_ring_distance(r, ring.iter().copied()))
            .fold(f64::INFINITY, f64::min)
    };
    // the boxes, by the frame's rule (`rules::box_fits` / `boxes_apart`, review 3, finding 4):
    // what the tool refuses here the frame would drop, and nothing else
    for k in 0..insets.len() {
        let rect = insets[k].row.rect;
        let [x, y, w, h] = rect;
        let r = [x, y, x + w, y + h];
        if !rules::box_fits(rect, &pane) {
            return Err(format!(
                "{code} {}: the box leaves the pane",
                insets[k].row.label
            ));
        }
        if !rules::boxes_apart(rect, rules::controls_rect(&pane)) {
            return Err(format!(
                "{code} {}: the box overlaps the controls' rect (C1)",
                insets[k].row.label
            ));
        }
        for other in &insets[..k] {
            if !rules::boxes_apart(rect, other.row.rect) {
                return Err(format!(
                    "{code}: boxes {} and {} overlap",
                    other.row.label, insets[k].row.label
                ));
            }
        }
        // the clearance at the golden pane at full size, recorded; since I1 (M4b commit 4) a box
        // too close to the land shrinks there as at any band, and the ship gate refuses only a box
        // dropped at 178 or 300 — the corner table replaces the old refusal's search
        insets[k].clearance_pt = clear(r);
    }

    // the main unit: the one holding the frame group's largest part
    let main_part = groups_idx[frame_group]
        .iter()
        .copied()
        .max_by(|&a, &b| areas[a].total_cmp(&areas[b]).then(b.cmp(&a)))
        .unwrap_or(largest);
    let main = parts[main_part].0;
    let mut ordered = vec![main];
    ordered.extend(units.iter().copied().filter(|&u| u != main));

    let mut plan = CountryPlan {
        code: code.to_string(),
        name: world.units[main].name.clone(),
        units: ordered,
        parts,
        groups,
        frame_group,
        lat0,
        lon0,
        bbox,
        fit,
        overridden: ov.is_some(),
        alias,
        subdivisions: false,
        insets,
    };
    // I1 + C1: the per-band scales and the corner table (M4b commit 4)
    if !plan.insets.is_empty() {
        let t = inset_tables_for(&plan, world, true);
        for (ins, (pct, corners)) in plan
            .insets
            .iter_mut()
            .zip(t.scale_pct.into_iter().zip(t.corners))
        {
            ins.scale_pct = pct;
            ins.corners = corners;
        }
    }
    Ok(plan)
}

fn insets_label(insets: &[InsetPlan], k: usize) -> String {
    insets
        .get(k)
        .map(|i| i.row.label.clone())
        .unwrap_or_default()
}

/// S4: for each alias unit, the parent's parts identical to its parts (same vertex count and
/// first vertex). Returns, per alias, (parent unit, parent part) pairs and the alias's part count.
/// Per alias: its code, the matched (parent unit, parent part) pairs, its own part count.
pub type S4Match = (String, Vec<(usize, usize)>, usize);

pub fn s4_matches(world: &World, aliases: &[Alias]) -> Vec<S4Match> {
    let n_admin0 = world.units.len() - aliases.len();
    aliases
        .iter()
        .enumerate()
        .map(|(k, a)| {
            let mu = &world.units[n_admin0 + k];
            let mut hits = Vec::new();
            if let Some(pi) = world
                .units
                .iter()
                .take(n_admin0)
                .position(|u| u.a3 == a.parent_a3)
            {
                for mp in &mu.parts {
                    let key = (mp.exterior().0.len(), mp.exterior().0.first().copied());
                    if let Some(pp) = world.units[pi].parts.iter().position(|pp| {
                        (pp.exterior().0.len(), pp.exterior().0.first().copied()) == key
                    }) {
                        hits.push((pi, pp));
                    }
                }
            }
            (a.code.clone(), hits, mu.parts.len())
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use geo::LineString;

    /// A small square of side `km` centred at (lat, lon).
    pub fn square(lat: f64, lon: f64, km: f64) -> Polygon<f64> {
        let l = Laea::new(lat, lon);
        let h = km / 2.0;
        let c: Vec<(f64, f64)> = [(-h, -h), (h, -h), (h, h), (-h, h), (-h, -h)]
            .iter()
            .map(|&(x, y)| l.inv(x, y).unwrap())
            .collect();
        Polygon::new(LineString::from(c), vec![])
    }

    pub fn unit(a3: &str, code: &str, parts: Vec<Polygon<f64>>) -> Unit {
        Unit {
            a3: a3.into(),
            code: Some(code.into()),
            name: a3.into(),
            parent_a3: a3.into(),
            parts,
        }
    }

    /// A `w × h` km rectangle centred at (lat, lon), its edges straight in its own LAEA.
    pub fn rect_km(lat: f64, lon: f64, w: f64, h: f64) -> Polygon<f64> {
        let l = Laea::new(lat, lon);
        let (hw, hh) = (w / 2.0, h / 2.0);
        let c: Vec<(f64, f64)> = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh), (-hw, -hh)]
            .iter()
            .map(|&(x, y)| l.inv(x, y).unwrap())
            .collect();
        Polygon::new(LineString::from(c), vec![])
    }

    fn row(corner: Corner, rect: [f64; 4], label: &str) -> InsetRow {
        InsetRow {
            code: "AA".into(),
            lat: 0.0,
            lon: 30.0,
            keep: String::new(),
            corner,
            rect,
            label: label.into(),
        }
    }

    /// A bar country `w × h` km with a 30 km inset group 30° east, planned with one row.
    fn bar_with_inset(w: f64, h: f64, r: &InsetRow) -> (World, CountryPlan) {
        let world = World::new(
            vec![unit(
                "AAA",
                "AA",
                vec![rect_km(0.0, 0.0, w, h), square(0.0, 30.0, 30.0)],
            )],
            vec![],
            &[],
        )
        .unwrap();
        let p = plan_country("AA", &[0], &world, None, &[r], false).unwrap();
        (world, p)
    }

    /// C1: a bottom-right row is refused — the corner is the controls' at every band, even when
    /// its box stops above the controls' rect — and a box at another corner that reaches into the
    /// controls' rect at the golden pane is refused too (fails with either check dropped).
    #[test]
    fn the_controls_corner_is_refused() {
        let world = World::new(
            vec![unit(
                "AAA",
                "AA",
                vec![rect_km(0.0, 0.0, 600.0, 104.0), square(0.0, 30.0, 30.0)],
            )],
            vec![],
            &[],
        )
        .unwrap();
        // a bottom-right row whose box stops above the controls' rect (bottom at 260 < 268), so
        // only the corner rule refuses it
        let br = row(Corner::BottomRight, [240.0, 200.0, 80.0, 60.0], "Far");
        let e = plan_country("AA", &[0], &world, None, &[&br], false).unwrap_err();
        assert!(e.contains("bottom-right corner is the controls'"), "{e}");
        // bottom-left, 250 wide: x 8..258 crosses the controls' 246 at the bottom
        let wide = row(Corner::BottomLeft, [8.0, 252.0, 250.0, 40.0], "Far");
        let e = plan_country("AA", &[0], &world, None, &[&wide], false).unwrap_err();
        assert!(e.contains("controls"), "{e}");
        // 230 wide (x to 238) is apart from it
        let ok = row(Corner::BottomLeft, [8.0, 252.0, 230.0, 40.0], "Far");
        assert!(plan_country("AA", &[0], &world, None, &[&ok], false).is_ok());
    }

    /// I1 on a synthetic country (M4b commit 4): a 600 × 104 km bar whose fit is width-bound at
    /// every band (2.083 km/pt), so the bar is 288 pt wide and 49.9 pt tall, centred. A top-left
    /// 80 × 60 box (gaps 8, 8) clears it at 300 by 57 pt — 100 %; at 178 the bar's top is at
    /// 64.04 pt, so the box's bottom must stay at 52.04: 60 · s ≤ 44.04 → 73 % (74 reads 11.64
    /// pt); at 140 the top is at 45.04 and 60 · s ≤ 25.04 → 0.417, under the 0.467 minimum → 0,
    /// dropped. The corner table: TR and BL mirror TL (73 at 178 — the bar is centred, and a
    /// bottom box anchored 8 pt up shrinks upward: 170 − 60 · s ≥ 125.96), each rect moved with
    /// its own gaps. The ship gate passes this country (73 and
    /// 100 at 178 and 300) and refuses a 600 × 200 km bar, dropped at 178 (60 · s ≤ 21) and not
    /// at 300. Fails with the loop over h skipped, the strip or pad scaled, the minimum ignored,
    /// a corner's gaps not mirrored, or the gate reading another height.
    #[test]
    fn insets_shrink_per_band() {
        let r = row(Corner::TopLeft, [8.0, 8.0, 80.0, 60.0], "Far");
        let (_, p) = bar_with_inset(600.0, 104.0, &r);
        let ins = &p.insets[0];
        assert_eq!(ins.scale_pct.len(), 161);
        let at = |h: u32| ins.scale_pct[(h - BAND_FLOOR) as usize];
        assert_eq!((at(300), at(178), at(140)), (100, 73, 0));
        // at 161 the bar's top is at 80.5 − 24.96 = 55.54: 60 · s ≤ 55.54 − 12 − 8 → 59 %
        assert_eq!(at(161), 59);
        let corner = |c: Corner| ins.corners.iter().find(|x| x.corner == c).unwrap();
        assert_eq!(corner(Corner::TopLeft).pct_178, 73);
        assert_eq!(corner(Corner::TopRight).pct_178, 73);
        assert_eq!(corner(Corner::TopRight).rect, [240.0, 8.0, 80.0, 60.0]);
        assert_eq!(corner(Corner::BottomLeft).pct_178, 73);
        assert_eq!(corner(Corner::BottomLeft).pct_300, 100);
        assert_eq!(corner(Corner::BottomLeft).rect, [8.0, 232.0, 80.0, 60.0]);
        assert_eq!(corner(Corner::TopLeft).min_pct, 0);
        assert_eq!(corner(Corner::TopLeft).min_at, 140);
        assert!(corner(Corner::TopLeft).clearance_161 < 12.0);
        let gate = ship_gate(std::slice::from_ref(&p));
        assert!(
            gate.dropped.is_empty() && gate.wide_labels.is_empty(),
            "{gate:?}"
        );
        let (_, thick) = bar_with_inset(600.0, 200.0, &r);
        let t = &thick.insets[0];
        assert_eq!((t.scale_pct[160], t.scale_pct[38]), (100, 0));
        let gate = ship_gate(std::slice::from_ref(&thick));
        assert_eq!(
            gate.dropped,
            vec!["AA Far: dropped at 328 × 178".to_string()]
        );
    }

    /// C1 in the placement itself (M4b commit 4): with no land at all, a 250 pt wide bottom-left
    /// box reaches the controls' rect (x 246) at every band and shrinks to 95 % (250 · s ≤ 238);
    /// with the controls disabled it is 100 % (fails with the controls not placed first).
    #[test]
    fn the_controls_rect_is_placed_first() {
        let r = row(Corner::BottomLeft, [8.0, 252.0, 250.0, 40.0], "Far");
        let bbox = [-300.0, -52.0, 300.0, 52.0];
        let with = inset_tables(bbox, &[], &[&r], true);
        let without = inset_tables(bbox, &[], &[&r], false);
        assert!(
            with.scale_pct[0].iter().all(|&p| p == 95),
            "{:?}",
            with.scale_pct[0]
        );
        assert!(without.scale_pct[0].iter().all(|&p| p == 100));
        assert_eq!(with.corners[0][2].pct_300, 95);
        assert_eq!(without.corners[0][2].pct_300, 100);
    }

    /// Review P3 (M4b commit 4): the ship gate names a label wider than its box's inner width at
    /// 178 or 300 — "Guadeloupe & Martinique" is 105.6 pt and an 80 × 60 box offers 72 at 100 %,
    /// 50.4 at 73 % — and passes "Azores" (28.8 pt); the message carries the figures (fails with
    /// the width under-counted or the inner width taken from the golden box).
    #[test]
    fn a_wide_label_is_flagged() {
        let wide = row(
            Corner::TopLeft,
            [8.0, 8.0, 80.0, 60.0],
            "Guadeloupe & Martinique",
        );
        let (_, p) = bar_with_inset(600.0, 104.0, &wide);
        let g = ship_gate(std::slice::from_ref(&p));
        assert_eq!(g.wide_labels.len(), 2, "{g:?}");
        assert!(
            g.wide_labels[0].contains("105.6 pt") && g.wide_labels[0].contains("50.4"),
            "{}",
            g.wide_labels[0]
        );
        assert!(
            g.wide_labels[1].contains("72.0 at 328 × 300"),
            "{}",
            g.wide_labels[1]
        );
        let ok = row(Corner::TopLeft, [8.0, 8.0, 80.0, 60.0], "Azores");
        let (_, p) = bar_with_inset(600.0, 104.0, &ok);
        assert!(ship_gate(std::slice::from_ref(&p)).wide_labels.is_empty());
    }

    /// Two 10 km squares on the equator, their gap 299 km then 301 km: one group, then two.
    #[test]
    fn grouping_at_299_and_301_km() {
        for (gap, groups) in [(299.0, 1), (301.0, 2)] {
            let a = square(0.0, 0.0, 10.0);
            // centres 10 + gap apart on the ground, along the equator
            let dlon =
                (10.0 + gap) / (ondar_map::laea::R_AUTHALIC_KM * std::f64::consts::PI / 180.0);
            let b = square(0.0, dlon, 10.0);
            let ll = [&a, &b];
            let gl = Laea::new(0.0, 0.0);
            let gp: Vec<Polygon<f64>> = ll.iter().map(|p| geom::project(p, &gl)).collect();
            assert_eq!(group_parts(&gp, &ll, CHAIN_KM).len(), groups, "gap {gap}");
        }
    }

    fn world_of(units: Vec<Unit>) -> World {
        World::new(units, vec![], &[]).unwrap()
    }

    /// The override's anchor picks its group over the largest part's (whose group then has to be
    /// an inset); an anchor in no part is refused.
    #[test]
    fn the_override_anchor() {
        let w = world_of(vec![unit(
            "AAA",
            "AA",
            vec![square(0.0, 0.0, 400.0), square(0.0, 20.0, 30.0)],
        )]);
        let p = plan_country("AA", &[0], &w, None, &[], false).unwrap();
        assert_eq!(p.groups[p.frame_group].parts, vec![0]);
        assert!((p.fit - 400.0 / 260.0).abs() < 0.01, "{}", p.fit);
        let ov = Override {
            code: "AA".into(),
            lat: 0.01,
            lon: 20.0,
            reason: String::new(),
        };
        let e = plan_country("AA", &[0], &w, Some(&ov), &[], false).unwrap_err();
        assert!(e.contains("not in insets.tsv"), "{e}");
        let row = InsetRow {
            code: "AA".into(),
            lat: 0.0,
            lon: 0.0,
            keep: String::new(),
            corner: ondar_map::format::Corner::TopLeft,
            rect: [2.0, 2.0, 18.0, 30.0],
            label: "Big".into(),
        };
        let p = plan_country("AA", &[0], &w, Some(&ov), &[&row], false).unwrap();
        assert_eq!(p.groups[p.frame_group].parts, vec![1]);
        assert!(p.overridden);
        // the frame is now the small square: fit ≈ 30 km over 260 pt
        assert!((p.fit - 30.0 / 260.0).abs() < 0.001, "{}", p.fit);
        let off = Override {
            code: "AA".into(),
            lat: 10.0,
            lon: 10.0,
            reason: String::new(),
        };
        assert!(
            plan_country("AA", &[0], &w, Some(&off), &[], false)
                .unwrap_err()
                .contains("in no part")
        );
    }

    /// S6: a remote group of 999 km² is dropped; of 1 001 km² it must be listed, and is refused
    /// unlisted; listed, it is an inset whose box keeps its clearance.
    #[test]
    fn the_s6_threshold() {
        let small = square(0.0, 30.0, 999f64.sqrt());
        let w = world_of(vec![unit(
            "AAA",
            "AA",
            vec![square(0.0, 0.0, 400.0), small],
        )]);
        let p = plan_country("AA", &[0], &w, None, &[], false).unwrap();
        assert_eq!(
            p.groups
                .iter()
                .filter(|g| g.role == GroupRole::Dropped)
                .count(),
            1
        );
        let big = square(0.0, 30.0, 1001f64.sqrt());
        let w = world_of(vec![unit("AAA", "AA", vec![square(0.0, 0.0, 400.0), big])]);
        let e = plan_country("AA", &[0], &w, None, &[], false).unwrap_err();
        assert!(e.contains("not in insets.tsv"), "{e}");
        let row = InsetRow {
            code: "AA".into(),
            lat: 0.0,
            lon: 30.0,
            keep: String::new(),
            corner: ondar_map::format::Corner::TopLeft,
            rect: [2.0, 2.0, 18.0, 30.0],
            label: "Far".into(),
        };
        let p = plan_country("AA", &[0], &w, None, &[&row], false).unwrap();
        assert_eq!(p.insets.len(), 1);
        assert!(p.insets[0].clearance_pt >= 12.0);
        // a box over the land is no longer refused here (I1, M4b commit 4): it is dropped at
        // every band — 0 % at 300 with a clearance of 0 — and the ship gate names it
        let over = InsetRow {
            rect: [150.0, 140.0, 40.0, 40.0],
            ..row.clone()
        };
        let p = plan_country("AA", &[0], &w, None, &[&over], false).unwrap();
        assert_eq!(p.insets[0].clearance_pt, 0.0);
        assert!(p.insets[0].scale_pct.iter().all(|&s| s == 0));
        let g = ship_gate(std::slice::from_ref(&p));
        assert_eq!(g.dropped.len(), 2, "{g:?}");
        // an anchor 150 km from the group is refused
        let far = InsetRow { lon: 31.5, ..row };
        assert!(
            plan_country("AA", &[0], &w, None, &[&far], false)
                .unwrap_err()
                .contains("from the anchor")
        );
    }

    /// S6's clearance is measured against the land the frame draws: the frame's groups and the
    /// small ones it drops into the padding band (review 2, finding 2). A 4 000 km square (fit
    /// 15.4 km/pt, its sides at x 34 and 294 pt), a 20 km islet 400 km east of it — its own
    /// group, under 1 000 km², outside the usable area and inside the pane at (320, 150) — and a
    /// remote inset whose box sits 14 pt right of the square and ~4 pt above the islet. Refused;
    /// on `8324e68` it built, its clearance read from the square alone (14 pt); since I1 (M4b
    /// commit 4) the figure is recorded, under 12, rather than refused. The same box
    /// with no islet builds.
    #[test]
    fn s6_clearance_counts_the_dropped_groups() {
        let (lon, lat) = Laea::new(0.0, 0.0).inv(2400.0, 0.0).unwrap();
        let main = square(0.0, 0.0, 4000.0);
        let islet = square(lat, lon, 20.0);
        let remote = square(0.0, 90.0, 40.0);
        let row = InsetRow {
            code: "AA".into(),
            lat: 0.0,
            lon: 90.0,
            keep: String::new(),
            corner: ondar_map::format::Corner::TopRight,
            rect: [308.0, 120.0, 18.0, 25.0],
            label: "Remote".into(),
        };
        let without = world_of(vec![unit("AAA", "AA", vec![main.clone(), remote.clone()])]);
        let p = plan_country("AA", &[0], &without, None, &[&row], false).unwrap();
        assert!(
            (p.insets[0].clearance_pt - 14.0).abs() < 0.5,
            "{}",
            p.insets[0].clearance_pt
        );
        let with = world_of(vec![unit(
            "AAA",
            "AA",
            vec![main.clone(), remote.clone(), islet],
        )]);
        let p = plan_country("AA", &[0], &with, None, &[&row], false).unwrap();
        assert!(
            p.insets[0].clearance_pt > 0.0 && p.insets[0].clearance_pt < 12.0,
            "{}",
            p.insets[0].clearance_pt
        );
        // the same islet off the pane, past the frame's 2 pt clip margin (x 332.4–333.7 pt, the
        // clip ends at 330) and level with the box, 6.4 pt right of it: the frame does not draw
        // it, so the box stands, as it does on screen
        let (lon, lat) = Laea::new(0.0, 0.0).inv(2600.0, 277.0).unwrap();
        let off = world_of(vec![unit(
            "AAA",
            "AA",
            vec![main, remote, square(lat, lon, 20.0)],
        )]);
        let p = plan_country("AA", &[0], &off, None, &[&row], false).unwrap();
        assert!(p.groups.iter().any(|g| g.role == GroupRole::Dropped));
        assert!(
            (p.insets[0].clearance_pt - 14.0).abs() < 0.5,
            "{}",
            p.insets[0].clearance_pt
        );
    }
}
