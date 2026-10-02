//! The frame rules over the whole of Natural Earth: the 267 stored units (258 admin 0 + the nine
//! S4 map units), the seam stitched (R9), and per country (248 codes) its groups, frame group,
//! centre (R1), fit, roles, insets (S6) and flags. Everything here is deterministic: the units
//! in file order, the countries by code.

use crate::geom;
use crate::ne::{Admin1, Unit};
use crate::seam::{self, Stitch};
use crate::tables::{Alias, InsetRow, Override};
use geo::{Area, ChamberlainDuquetteArea, Contains, Coord, Distance, Euclidean, Point, Polygon};
use ondar_map::laea::Laea;
use ondar_map::rules::{self, Pane};
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
    pub scale: f64,
    pub clearance_pt: f64,
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
            scale,
            clearance_pt: f64::NAN,
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
    let to_pt = |c: &Coord<f64>| Coord {
        x: (c.x - cx) / s0 + pane.width / 2.0,
        y: (cy - c.y) / s0 + pane.height / 2.0,
    };
    // the land the frame draws at the initial view, which `Store::inset_clearance` measures
    // (review 2, finding 2): the frame's groups and the small ones it drops, clipped as the
    // frame clips them — to the pane grown by the clip margin — so a dropped group off the pane
    // (Jan Mayen, 6 pt above Norway's) counts no more here than on screen
    let clip_pt = [
        -index::CLIP_MARGIN_PT,
        -index::CLIP_MARGIN_PT,
        pane.width + index::CLIP_MARGIN_PT,
        pane.height + index::CLIP_MARGIN_PT,
    ];
    let land_rings: Vec<Vec<Coord<f64>>> = groups
        .iter()
        .filter(|g| matches!(g.role, GroupRole::Frame | GroupRole::Dropped))
        .flat_map(|g| g.parts.iter())
        .flat_map(|&i| {
            geom::rings(&proj[i])
                .map(|r| {
                    let pts: Vec<[f64; 2]> = r.0.iter().map(to_pt).map(|c| [c.x, c.y]).collect();
                    clip::clip_ring(&pts, &clip_pt)
                        .into_iter()
                        .map(|[x, y]| Coord { x, y })
                        .collect::<Vec<_>>()
                })
                .filter(|r| r.len() >= 3)
                .collect::<Vec<_>>()
        })
        .collect();
    for k in 0..insets.len() {
        let [x, y, w, h] = insets[k].row.rect;
        let r = [x, y, x + w, y + h];
        if x < 0.0 || y < 0.0 || r[2] > pane.width || r[3] > pane.height {
            return Err(format!(
                "{code} {}: the box leaves the pane",
                insets[k].row.label
            ));
        }
        for other in &insets[..k] {
            let [ox, oy, ow, oh] = other.row.rect;
            if x < ox + ow && ox < r[2] && y < oy + oh && oy < r[3] {
                return Err(format!(
                    "{code}: boxes {} and {} overlap",
                    other.row.label, insets[k].row.label
                ));
            }
        }
        let clearance = land_rings
            .iter()
            .map(|ring| geom::rect_ring_distance(r, ring))
            .fold(f64::INFINITY, f64::min);
        insets[k].clearance_pt = clearance;
        if clearance < rules::INSET_CLEARANCE_PT {
            // per corner, 8 pt in, the largest box of this aspect that clears: the refusal says
            // where a box would fit
            let clear = |r: [f64; 4]| {
                land_rings
                    .iter()
                    .map(|ring| geom::rect_ring_distance(r, ring))
                    .fold(f64::INFINITY, f64::min)
            };
            let largest = |corner: usize| {
                let box_at = |f: f64| {
                    let (bw, bh) = (w * f, h * f);
                    let bx = if corner.is_multiple_of(2) {
                        8.0
                    } else {
                        pane.width - 8.0 - bw
                    };
                    let by = if corner < 2 {
                        8.0
                    } else {
                        pane.height - 8.0 - bh
                    };
                    [bx, by, bx + bw, by + bh]
                };
                let (mut lo, mut hi) = (0.0f64, 1.0f64);
                if clear(box_at(1.0)) >= rules::INSET_CLEARANCE_PT {
                    return format!("{w:.0} × {h:.0}");
                }
                for _ in 0..30 {
                    let mid = (lo + hi) / 2.0;
                    if clear(box_at(mid)) >= rules::INSET_CLEARANCE_PT {
                        lo = mid
                    } else {
                        hi = mid
                    }
                }
                format!("{:.0} × {:.0}", w * lo, h * lo)
            };
            return Err(format!(
                "{code} {}: the box is {clearance:.1} pt from the land (< {}); the largest \
                 {w:.0} × {h:.0}-shaped box 8 pt in from each corner that clears: TL {} TR {} \
                 BL {} BR {}",
                insets[k].row.label,
                rules::INSET_CLEARANCE_PT,
                largest(0),
                largest(1),
                largest(2),
                largest(3)
            ));
        }
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

    Ok(CountryPlan {
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
    })
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
        // a box over the land is refused
        let over = InsetRow {
            rect: [150.0, 140.0, 20.0, 20.0],
            ..row.clone()
        };
        assert!(
            plan_country("AA", &[0], &w, None, &[&over], false)
                .unwrap_err()
                .contains("pt from the land")
        );
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
    /// on `8324e68` it built, its clearance read from the square alone (14 pt). The same box
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
        let e = plan_country("AA", &[0], &with, None, &[&row], false).unwrap_err();
        assert!(e.contains("pt from the land"), "{e}");
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
