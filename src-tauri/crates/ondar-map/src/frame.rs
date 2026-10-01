//! Framing (M4a): a country's view — the fit, the clamp (D6), and the frame at any clamped view
//! as layers of paths in pane points.
//!
//! A view is a centre in the country's frame LAEA (km) and a scale (km/pt). The pane's origin is
//! its top-left corner, y down. The level is the coarsest ladder level at or below the scale;
//! the clip rectangle is the view grown by `index::CLIP_MARGIN_PT`; a ring is read only if its
//! cap meets the clip rectangle (the index), then decoded, reprojected (the country's main unit
//! by a translation and a scale, every other unit inverse-then-forward), clipped and rounded to
//! 0.01 pt.

use crate::clip::{self, Rect};
use crate::format::{self, Corner, Layer, Role, Store};
use crate::index::{self, CLIP_MARGIN_PT, LAND_TOL_PT, SUB_TOL_PT};
use crate::laea::Laea;
use crate::rules::{self, LADDER, Pane};

/// A view: its centre in the country's frame LAEA, km, and its scale, km/pt.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct View {
    pub centre: [f64; 2],
    pub scale: f64,
}

/// What a radio-browser code frames (R7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// The country's index in `Store::countries`.
    Country(usize),
    /// No shape for this code: `XX`, an unknown code.
    NoMap,
}

/// Rings in pane points (exterior first, then holes; fill rule even-odd).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Shape {
    pub rings: Vec<Vec<[f32; 2]>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Inset {
    pub label: String,
    /// x, y, w, h, points.
    pub rect: [f32; 4],
    pub land: Vec<Shape>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct FrameStats {
    pub vertices: usize,
    pub rings_considered: usize,
    pub rings_skipped: usize,
    /// Blobs the frame needed and the resource does not store, counted per unit (review finding
    /// 9): a unit the index admitted at the view's level, once per inset for a unit holding its
    /// parts, and the country's subdivisions. 0 at the golden pane (the coverage test).
    pub missing_blobs: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Frame {
    pub view: View,
    pub level: f64,
    pub neighbours: Vec<Shape>,
    pub land: Vec<Shape>,
    pub subdivisions: Vec<Vec<[f32; 2]>>,
    /// At the fit view only (D7).
    pub insets: Vec<Inset>,
    pub stats: FrameStats,
}

impl format::Inset {
    /// The box at `pane`, x, y, w, h points: the golden pane's box kept at its distance from the
    /// corner it is anchored to (S6; review finding 2) — a right corner moves with the pane's
    /// width, a bottom corner with its height, the size never changes. At the golden pane it is
    /// `rect`. Whether it clears the land at another pane is M4b's to judge.
    pub fn rect_at(&self, pane: &Pane) -> [f64; 4] {
        let [x, y, w, h] = self.rect.map(f64::from);
        let (dx, dy) = (
            pane.width - Pane::GOLDEN.width,
            pane.height - Pane::GOLDEN.height,
        );
        match self.corner {
            Corner::TopLeft => [x, y, w, h],
            Corner::TopRight => [x + dx, y, w, h],
            Corner::BottomLeft => [x, y + dy, w, h],
            Corner::BottomRight => [x + dx, y + dy, w, h],
        }
    }
}

fn round(v: f64) -> f32 {
    ((v * 100.0).round() / 100.0) as f32
}

impl Store {
    /// R7: the code ASCII-uppercased; `XX` and a code with no country are `NoMap`.
    pub fn lookup(&self, code: &str) -> Lookup {
        let up = code.to_ascii_uppercase();
        if up == "XX" || up.len() != 2 {
            return Lookup::NoMap;
        }
        self.countries
            .iter()
            .position(|c| c.code.as_slice() == up.as_bytes())
            .map_or(Lookup::NoMap, Lookup::Country)
    }

    /// The country's fit at `pane`: the frame bbox fills the usable area, never finer than the
    /// floor (S1). `None` for an unknown index or a pane with no usable area.
    pub fn fit_scale(&self, c: usize, pane: &Pane) -> Option<f64> {
        let [x0, y0, x1, y1] = self.countries.get(c)?.bbox_km;
        rules::fit_scale(x1 - x0, y1 - y0, pane)
    }

    /// The initial view: the bbox centre at the fit (S1 floor).
    pub fn fit(&self, c: usize, pane: &Pane) -> Option<View> {
        let [x0, y0, x1, y1] = self.countries.get(c)?.bbox_km;
        Some(View {
            centre: [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
            scale: rules::initial_scale(self.fit_scale(c, pane)?),
        })
    }

    /// D6: the scale into [1.5, widest] and the view inside the fit rectangle (the pane at the
    /// widest scale, centred on the frame bbox) — at the widest scale the view is the fit and
    /// cannot pan. A non-finite view is the fit.
    pub fn clamp(&self, c: usize, pane: &Pane, view: View) -> Option<View> {
        let fit = self.fit(c, pane)?;
        let [vx, vy] = view.centre;
        if !(vx.is_finite() && vy.is_finite() && view.scale.is_finite()) {
            return Some(fit);
        }
        let top = fit.scale;
        // max then min, never `f64::clamp`, which panics when min > max (finding 4)
        let scale = view.scale.max(rules::FLOOR_KM_PER_PT.min(top)).min(top);
        let [fx, fy] = fit.centre;
        let (rx, ry) = (
            pane.width / 2.0 * (top - scale),
            pane.height / 2.0 * (top - scale),
        );
        Some(View {
            centre: [vx.max(fx - rx).min(fx + rx), vy.max(fy - ry).min(fy + ry)],
            scale,
        })
    }

    /// A lon/lat point in pane points at a view.
    pub fn project(
        &self,
        c: usize,
        pane: &Pane,
        view: &View,
        lon: f64,
        lat: f64,
    ) -> Option<[f64; 2]> {
        let ct = self.countries.get(c)?;
        let [x, y] = Laea::new(ct.lat0, ct.lon0).fwd(lon, lat)?;
        let [cx, cy] = view.centre;
        Some([
            (x - cx) / view.scale + pane.width / 2.0,
            (cy - y) / view.scale + pane.height / 2.0,
        ])
    }

    /// A pane point back to (lon, lat) at a view.
    pub fn unproject(
        &self,
        c: usize,
        pane: &Pane,
        view: &View,
        x: f64,
        y: f64,
    ) -> Option<(f64, f64)> {
        let ct = self.countries.get(c)?;
        let [cx, cy] = view.centre;
        Laea::new(ct.lat0, ct.lon0).inv(
            (x - pane.width / 2.0) * view.scale + cx,
            cy - (y - pane.height / 2.0) * view.scale,
        )
    }

    /// The country's frame at a view (clamped first).
    pub fn frame(&self, c: usize, pane: &Pane, view: View) -> Option<Frame> {
        self.frame_with(c, pane, view, true)
    }

    /// The frame with the ring index disabled: every unit and ring is decoded and clipped. For
    /// the test that the index skips nothing visible (`index_is_exact`); never used by the app.
    #[doc(hidden)]
    pub fn frame_unindexed(&self, c: usize, pane: &Pane, view: View) -> Option<Frame> {
        self.frame_with(c, pane, view, false)
    }

    fn frame_with(&self, c: usize, pane: &Pane, view: View, use_index: bool) -> Option<Frame> {
        let view = Store::clamp(self, c, pane, view)?;
        let fit_view = self.fit(c, pane)?;
        let ct = self.countries.get(c)?;
        let k = rules::level_for(view.scale);
        let level = *LADDER.get(k)?;
        let k8 = u8::try_from(k).ok()?;
        let c16 = u16::try_from(c).ok()?;
        let frame_l = Laea::new(ct.lat0, ct.lon0);
        let [cx, cy] = view.centre;
        let s = view.scale;
        let (hw, hh) = (pane.width / 2.0 * s, pane.height / 2.0 * s);
        let m = CLIP_MARGIN_PT * s;
        let ground = index::ground_cap(
            &frame_l,
            [cx - hw - m, cy - hh - m, cx + hw + m, cy + hh + m],
        );
        let clip_pt: Rect = [
            -CLIP_MARGIN_PT,
            -CLIP_MARGIN_PT,
            pane.width + CLIP_MARGIN_PT,
            pane.height + CLIP_MARGIN_PT,
        ];
        let to_pt = |x: f64, y: f64| {
            [
                (x - cx) / s + pane.width / 2.0,
                (cy - y) / s + pane.height / 2.0,
            ]
        };
        let tol = index::tolerance_km(level, LAND_TOL_PT);
        let main = ct.units.first().copied();
        // D7: the insets are on screen at the fit view only
        let at_fit = view == fit_view;

        let mut out = Frame {
            view,
            level,
            neighbours: Vec::new(),
            land: Vec::new(),
            subdivisions: Vec::new(),
            insets: Vec::new(),
            stats: FrameStats::default(),
        };
        let mut buf = Vec::new();
        let mut pts = Vec::new();
        for (u, unit) in self.units.iter().enumerate() {
            if use_index && !index::cap_meets(&unit.cap, ground, tol) {
                continue;
            }
            let Ok(u16_) = u16::try_from(u) else { continue };
            let own = ct.units.contains(&u16_);
            let same = main == Some(u16_);
            let unit_l = Laea::new(unit.lat0, unit.lon0);
            // own land: the frame's parts and the small groups outside the usable area (S6's
            // `Dropped`), drawn where the view meets them as a neighbour's would be (review
            // finding 5); an inset's part is drawn in its box at the fit, and as land at every
            // other view, where its box is not on screen (review 2, finding 1). `Some(neighbour)`
            // if drawn.
            let drawn = |part: &crate::format::Part| match part.role {
                _ if !own => (part.omit_in != Some(c16)).then_some(true),
                Role::Frame | Role::Dropped => Some(false),
                Role::Inset(_) => (!at_fit).then_some(false),
                Role::NeighbourOnly => None,
            };
            let Some(b) = self.blob(u16_, k8, Layer::Land) else {
                // no blob at this level: one missing unit if the index admits a ring this frame
                // would draw — counted per unit, no ring considered (review finding 9)
                let admitted = unit.parts.iter().any(|p| {
                    drawn(p).is_some()
                        && p.rings
                            .iter()
                            .any(|cap| !use_index || index::cap_meets(cap, ground, tol))
                });
                out.stats.missing_blobs += usize::from(admitted);
                continue;
            };
            let mut ri = 0usize;
            for part in &unit.parts {
                let first = ri;
                ri += part.rings.len();
                let Some(neighbour) = drawn(part) else {
                    continue;
                };
                let mut shape = Shape::default();
                for (j, cap) in part.rings.iter().enumerate() {
                    if use_index && !index::cap_meets(cap, ground, tol) {
                        out.stats.rings_skipped += 1;
                        continue;
                    }
                    out.stats.rings_considered += 1;
                    if self.decode(b, first + j, &mut buf).is_none() {
                        continue;
                    }
                    pts.clear();
                    let mut ok = true;
                    for &[x, y] in &buf {
                        let p = if same {
                            Some([x, y])
                        } else {
                            unit_l
                                .inv(x, y)
                                .and_then(|(lon, lat)| frame_l.fwd(lon, lat))
                        };
                        match p {
                            Some([x, y]) => pts.push(to_pt(x, y)),
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        continue;
                    }
                    let clipped = clip::clip_ring(&pts, &clip_pt);
                    if clipped.len() >= 3 {
                        out.stats.vertices += clipped.len();
                        shape
                            .rings
                            .push(clipped.iter().map(|&[x, y]| [round(x), round(y)]).collect());
                    }
                }
                if !shape.rings.is_empty() {
                    if neighbour {
                        out.neighbours.push(shape);
                    } else {
                        out.land.push(shape);
                    }
                }
            }
        }

        // subdivisions, above 8 km/pt for a flagged country (S7), in the frame's LAEA
        if ct.subdivisions && s > rules::SUBDIVISIONS_ABOVE_KM_PER_PT {
            let stol = index::tolerance_km(level, SUB_TOL_PT);
            match self.blob(c16, k8, Layer::Subdivisions) {
                Some(b) => {
                    for (j, cap) in ct.sub_lines.iter().enumerate() {
                        if use_index && !index::cap_meets(cap, ground, stol) {
                            out.stats.rings_skipped += 1;
                            continue;
                        }
                        out.stats.rings_considered += 1;
                        if self.decode(b, j, &mut buf).is_none() {
                            continue;
                        }
                        pts.clear();
                        pts.extend(buf.iter().map(|&[x, y]| to_pt(x, y)));
                        for piece in clip::clip_polyline(&pts, &clip_pt) {
                            out.stats.vertices += piece.len();
                            out.subdivisions
                                .push(piece.iter().map(|&[x, y]| [round(x), round(y)]).collect());
                        }
                    }
                }
                None => out.stats.missing_blobs += 1,
            }
        }

        // insets, at the fit view only (D7)
        if at_fit {
            out.insets = self.insets(c, pane, &mut out.stats);
        }
        Some(out)
    }

    fn insets(&self, c: usize, pane: &Pane, stats: &mut FrameStats) -> Vec<Inset> {
        let Some(ct) = self.countries.get(c) else {
            return Vec::new();
        };
        let mut buf = Vec::new();
        let mut out = Vec::new();
        for (i, ins) in ct.insets.iter().enumerate() {
            let k = rules::level_for(ins.scale);
            let (Ok(k8), Ok(i8_)) = (u8::try_from(k), u8::try_from(i)) else {
                continue;
            };
            let [rx, ry, rw, rh] = ins.rect_at(pane);
            let (acx, acy, _, _) = rules::inset_area([rx, ry, rw, rh]);
            let il = Laea::new(ins.lat0, ins.lon0);
            let [icx, icy] = ins.centre_km;
            let rect: Rect = [rx, ry, rx + rw, ry + rh];
            let mut land = Vec::new();
            for &u in &ct.units {
                let Some(unit) = self.units.get(usize::from(u)) else {
                    continue;
                };
                let unit_l = Laea::new(unit.lat0, unit.lon0);
                let Some(b) = self.blob(u, k8, Layer::Land) else {
                    // one missing unit, however many of its parts the inset holds (finding 9)
                    let holds = unit.parts.iter().any(|p| p.role == Role::Inset(i8_));
                    stats.missing_blobs += usize::from(holds);
                    continue;
                };
                let mut ri = 0usize;
                for part in &unit.parts {
                    let first = ri;
                    ri += part.rings.len();
                    if part.role != Role::Inset(i8_) {
                        continue;
                    }
                    let mut shape = Shape::default();
                    for j in 0..part.rings.len() {
                        if self.decode(b, first + j, &mut buf).is_none() {
                            continue;
                        }
                        let pts: Option<Vec<[f64; 2]>> = buf
                            .iter()
                            .map(|&[x, y]| {
                                let (lon, lat) = unit_l.inv(x, y)?;
                                let [x, y] = il.fwd(lon, lat)?;
                                Some([(x - icx) / ins.scale + acx, (icy - y) / ins.scale + acy])
                            })
                            .collect();
                        let Some(pts) = pts else { continue };
                        let clipped = clip::clip_ring(&pts, &rect);
                        if clipped.len() >= 3 {
                            stats.vertices += clipped.len();
                            shape
                                .rings
                                .push(clipped.iter().map(|&[x, y]| [round(x), round(y)]).collect());
                        }
                    }
                    if !shape.rings.is_empty() {
                        land.push(shape);
                    }
                }
            }
            out.push(Inset {
                label: ins.label.clone(),
                rect: [rx, ry, rw, rh].map(|v| v as f32),
                land,
            });
        }
        out
    }

    /// Per inset, its box's distance to the country's land at the fit view, points (S6: ≥ 12).
    pub fn inset_clearance(&self, c: usize, pane: &Pane) -> Vec<(String, f64)> {
        let (Some(ct), Some(fit)) = (self.countries.get(c), self.fit(c, pane)) else {
            return Vec::new();
        };
        let Some(frame) = self.frame(c, pane, fit) else {
            return Vec::new();
        };
        ct.insets
            .iter()
            .map(|ins| {
                let [x, y, w, h] = ins.rect_at(pane);
                let d = frame
                    .land
                    .iter()
                    .flat_map(|s| s.rings.iter())
                    .map(|r| {
                        rules::rect_ring_distance(
                            [x, y, x + w, y + h],
                            r.iter().map(|&[x, y]| [f64::from(x), f64::from(y)]),
                        )
                    })
                    .fold(f64::INFINITY, f64::min);
                (ins.label.clone(), d)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::tests::{
        cap, header, synthetic_blobs, synthetic_countries, synthetic_units,
    };
    use crate::format::{Encoding, Part, Unit, write};

    /// `missing_blobs` counts units, as documented (review finding 9): a neighbour unit with three
    /// parts in view and no blob at the frame's level adds one missing blob, and its rings are not
    /// counted as considered — none was decoded; an inset of two parts whose level has no blob is
    /// one missing blob, as an inset of one part was. On `dddb4da` the two added 4 (one per part),
    /// and `rings_considered` counted the three rings never read. The unindexed frame agrees.
    #[test]
    fn missing_blobs_counts_units_not_parts() {
        let mut units = synthetic_units();
        let part = |lon: f32| Part {
            role: Role::NeighbourOnly,
            omit_in: None,
            rings: vec![cap(lon, 40.0, 60.0)],
        };
        units.push(Unit {
            a3: *b"ESP",
            code: None,
            name: "three parts, no blob".into(),
            lat0: 40.0,
            lon0: -7.0,
            cap: cap(-7.0, 40.0, 200.0),
            parts: vec![part(-7.2), part(-7.0), part(-6.8)],
        });
        // a second part for the Azores inset (unit 0), its ring in unit 0's two blobs
        units[0].parts.push(Part {
            role: Role::Inset(0),
            omit_in: None,
            rings: vec![cap(-25.0, 37.8, 50.0)],
        });
        let mut blobs = synthetic_blobs();
        for b in blobs
            .iter_mut()
            .filter(|b| b.owner == 0 && b.layer == Layer::Land)
        {
            b.rings.push(vec![[0, 0], [100, 0], [100, 100]]);
        }
        let b = write(
            &header(),
            &units,
            &synthetic_countries(),
            &blobs,
            Encoding::Raw,
        )
        .unwrap();
        let s = Store::load(&b).unwrap();
        let pane = Pane::GOLDEN;
        let fit = s.fit(0, &pane).unwrap();
        let with = |f: Frame| (f.stats.missing_blobs, f.stats.rings_considered);
        let (base, base_unindexed) = {
            // the same resource without the extra unit: what the rest of the frame considers
            let b = write(
                &header(),
                &synthetic_units(),
                &synthetic_countries(),
                &synthetic_blobs(),
                Encoding::Raw,
            )
            .unwrap();
            let s0 = Store::load(&b).unwrap();
            (
                with(s0.frame(0, &pane, fit).unwrap()),
                with(s0.frame_unindexed(0, &pane, fit).unwrap()),
            )
        };
        // the base frame's own count: its inset's level has no blob in the synthetic resource
        assert_eq!(with(s.frame(0, &pane, fit).unwrap()), (base.0 + 1, base.1));
        assert_eq!(
            with(s.frame_unindexed(0, &pane, fit).unwrap()),
            (base_unindexed.0 + 1, base_unindexed.1)
        );
    }
}
