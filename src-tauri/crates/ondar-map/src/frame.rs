//! Framing (M4a): a country's view — the fit, the clamp (D6), and the frame at any clamped view
//! as layers of paths in pane points.
//!
//! A view is a centre in the country's frame LAEA (km) and a scale (km/pt). The pane's origin is
//! its top-left corner, y down. The level is the coarsest ladder level at or below the scale;
//! the clip rectangle is the view grown by `index::CLIP_MARGIN_PT` (`index::clip_rect`, the
//! tool's too); a ring is read only if its
//! cap meets the clip rectangle (the index), then decoded, reprojected (the country's main unit
//! by a translation and a scale, every other unit inverse-then-forward), clipped and rounded to
//! 0.01 pt.

use crate::clip::{self, Rect};
use crate::format::{self, Layer, Role, Store};
use crate::gather::Gathered;
use crate::index::{self, CLIP_MARGIN_PT, LAND_TOL_PT, SUB_TOL_PT};
use crate::laea::Laea;
use crate::rules::{self, LADDER, Pane};

/// A view: its centre in the country's frame LAEA, km, and its scale, km/pt.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct View {
    pub centre: [f64; 2],
    pub scale: f64,
}

impl View {
    /// A finite centre and a finite scale above 0 (review 3, finding 1).
    pub fn is_valid(&self) -> bool {
        self.centre.iter().all(|v| v.is_finite()) && self.scale.is_finite() && self.scale > 0.0
    }
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
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Shape {
    pub rings: Vec<Vec<[f32; 2]>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Inset {
    pub label: String,
    /// x, y, w, h, points.
    pub rect: [f32; 4],
    pub land: Vec<Shape>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct FrameStats {
    pub vertices: usize,
    pub rings_considered: usize,
    pub rings_skipped: usize,
    /// Blobs the frame needed and the resource does not store, counted per unit (review finding
    /// 9): a unit the index admitted at the view's level, once per inset for a unit holding its
    /// parts, and the country's subdivisions when one of its lines meets the view. 0 at the
    /// golden pane (the coverage test).
    pub missing_blobs: usize,
    /// Insets not drawn at the fit view: the band's stored scale is 0 (I1: the box cannot meet
    /// the minimum there — Hawaii below 274, Svalbard at 225–257), or the box, anchored at this
    /// pane, leaves the pane, meets the controls' rect or overlaps a box drawn before it in table
    /// order (review 2, finding 4; review 3, finding 2). 0 at every band but those the tool's
    /// tables name.
    pub insets_dropped: usize,
    /// Located dots not drawn at this view (decision 4): at the fit, a dot in an inset whose box
    /// is not drawn (US at 178: Honolulu), or a dot in the main projection under a drawn box.
    pub dots_hidden: usize,
    /// Stations more than 25 km outside every part of the country (R6), never drawn.
    pub dots_outside: usize,
    /// Stations in the located dots; with `stations_total > 0` and this 0 the page says the
    /// country has no station locations (MT).
    pub stations_located: usize,
    /// The country's stations, with coordinates or not.
    pub stations_total: usize,
}

/// A dot on the pane (M4c): its centre and radius, points; its stations (uuids, list order) and
/// their majority place.
#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Dot {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub n: u32,
    pub uuids: Vec<String>,
    pub place: String,
}

/// A dot's radius for `n` stations, points: `min(6, 2.5 + 0.6 ln n)` (n = 1 → 2.5, 64 → 5.0,
/// 1 000 → 6).
pub fn dot_radius(n: usize) -> f64 {
    (2.5 + 0.6 * (n.max(1) as f64).ln()).min(6.0)
}

/// An inset box's projection at a pane (`Store::inset_projection`): the group's LAEA, its centre,
/// km, the box's scale, km/pt, and the centre of the box's land area, points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InsetProjection {
    pub laea: Laea,
    pub centre_km: [f64; 2],
    pub scale: f64,
    pub origin: [f64; 2],
}

impl InsetProjection {
    /// A lon/lat point in the box, points; `None` where the projection cannot reach it.
    pub fn to_pt(&self, lon: f64, lat: f64) -> Option<[f64; 2]> {
        let [x, y] = self.laea.fwd(lon, lat)?;
        let ([icx, icy], [ox, oy]) = (self.centre_km, self.origin);
        Some([(x - icx) / self.scale + ox, (icy - y) / self.scale + oy])
    }
}

/// The projection of `ins`'s group into its box `rect` (`[x, y, w, h]`, points): the group fitted
/// to the box's land area (`rules::inset_scale`). `None` for a box with no land area.
pub fn inset_projection(ins: &format::Inset, rect: [f64; 4]) -> Option<InsetProjection> {
    let [size_w, size_h] = ins.size_km;
    let scale = rules::inset_scale(size_w, size_h, rect)?;
    let (acx, acy, _, _) = rules::inset_area(rect);
    Some(InsetProjection {
        laea: Laea::new(ins.lat0, ins.lon0),
        centre_km: ins.centre_km,
        scale,
        origin: [acx, acy],
    })
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Frame {
    pub view: View,
    pub level: f64,
    pub neighbours: Vec<Shape>,
    pub land: Vec<Shape>,
    pub subdivisions: Vec<Vec<[f32; 2]>>,
    /// At the fit view only (D7).
    pub insets: Vec<Inset>,
    /// The located dots on the pane, larger first (`Store::frame_dots`); empty from `frame`.
    pub dots: Vec<Dot>,
    pub stats: FrameStats,
}

impl format::Inset {
    /// The box's scale at a pane (I1): the stored table's entry for the band, the pane's height
    /// floored and clamped into the resource's range (`Bands::index`), as a fraction; 0 where the
    /// tool found no placeable box.
    pub fn scale_at(&self, bands: &format::Bands, pane: &Pane) -> f64 {
        self.scale_pct
            .get(bands.index(pane))
            .map_or(0.0, |&p| f64::from(p) / 100.0)
    }

    /// The golden rect as `f64`.
    pub fn golden(&self) -> [f64; 4] {
        self.rect.map(f64::from)
    }
}

/// A scale at or above `fit × (1 − FIT_SNAP)` is the fit (review 3, finding 3). Snapping 1e-6 of
/// the scale moves a point at the golden pane's corner, 222 pt from its centre, by 0.0002 pt;
/// any zoom step is far larger.
pub const FIT_SNAP: f64 = 1e-6;

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
    /// cannot pan. A non-finite view is the fit, and so is a scale within `FIT_SNAP` of the
    /// widest (review 3, finding 3): the frame decides the insets and the remote groups' land
    /// on `view == fit`, so a view a hair finer than the fit must not flip them.
    pub fn clamp_view(&self, c: usize, pane: &Pane, view: View) -> Option<View> {
        let fit = self.fit(c, pane)?;
        let [vx, vy] = view.centre;
        if !(vx.is_finite() && vy.is_finite() && view.scale.is_finite()) {
            return Some(fit);
        }
        let top = fit.scale;
        if view.scale >= top * (1.0 - FIT_SNAP) {
            return Some(fit);
        }
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

    /// A lon/lat point in pane points at a view, as given (not clamped); `None` for a pane that is
    /// not valid (review 2, finding 6), a view that is not valid — a scale not finite or ≤ 0, a
    /// centre not finite (review 3, finding 1) — an unknown country or a point the projection
    /// cannot reach.
    pub fn project(
        &self,
        c: usize,
        pane: &Pane,
        view: &View,
        lon: f64,
        lat: f64,
    ) -> Option<[f64; 2]> {
        if !pane.is_valid() || !view.is_valid() {
            return None;
        }
        let ct = self.countries.get(c)?;
        let [x, y] = Laea::new(ct.lat0, ct.lon0).fwd(lon, lat)?;
        let [cx, cy] = view.centre;
        Some([
            (x - cx) / view.scale + pane.width / 2.0,
            (cy - y) / view.scale + pane.height / 2.0,
        ])
    }

    /// A pane point back to (lon, lat) at a view; `None` as for `project`.
    pub fn unproject(
        &self,
        c: usize,
        pane: &Pane,
        view: &View,
        x: f64,
        y: f64,
    ) -> Option<(f64, f64)> {
        if !pane.is_valid() || !view.is_valid() {
            return None;
        }
        let ct = self.countries.get(c)?;
        let [cx, cy] = view.centre;
        Laea::new(ct.lat0, ct.lon0).inv(
            (x - pane.width / 2.0) * view.scale + cx,
            cy - (y - pane.height / 2.0) * view.scale,
        )
    }

    /// The country's frame at a view (clamped first), with no dots.
    pub fn frame(&self, c: usize, pane: &Pane, view: View) -> Option<Frame> {
        self.frame_dots(c, pane, view, &Gathered::default())
    }

    /// The frame with the country's located dots (M4c): a dot in a `Frame` or `Dropped` part is
    /// projected with the view and kept if its centre is within its radius of the pane. At the
    /// fit, a dot in an `Inset` part is projected with its box's projection (`inset_projection`)
    /// and drawn iff the box is drawn and holds its centre, else counted in `dots_hidden`; a
    /// main-projection dot whose centre is under a drawn box is counted there too (decision 4).
    /// Away from the fit every dot is in the main projection (D7). The counts of `g` go to the
    /// stats as they are.
    pub fn frame_dots(&self, c: usize, pane: &Pane, view: View, g: &Gathered) -> Option<Frame> {
        self.frame_with(c, pane, view, true, g)
    }

    /// The frame with the ring index disabled: every unit and ring is decoded and clipped. For
    /// the test that the index skips nothing visible (`index_is_exact`); never used by the app.
    #[doc(hidden)]
    pub fn frame_unindexed(&self, c: usize, pane: &Pane, view: View) -> Option<Frame> {
        self.frame_with(c, pane, view, false, &Gathered::default())
    }

    fn frame_with(
        &self,
        c: usize,
        pane: &Pane,
        view: View,
        use_index: bool,
        g: &Gathered,
    ) -> Option<Frame> {
        let view = self.clamp_view(c, pane, view)?;
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
        let clip_pt = index::clip_rect(pane);
        let to_pt = |x: f64, y: f64| {
            [
                (x - cx) / s + pane.width / 2.0,
                (cy - y) / s + pane.height / 2.0,
            ]
        };
        // own land at the view's level, neighbours one rung coarser (M4c, lever (c)); the index
        // tests each unit at the tolerance of the level it is drawn at, as the tool's coverage does
        let kn = rules::neighbour_level(k);
        let (tol, tol_n) = (
            index::tolerance_km(level, LAND_TOL_PT),
            index::tolerance_km(*LADDER.get(kn)?, LAND_TOL_PT),
        );
        let kn8 = u8::try_from(kn).ok()?;
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
            dots: Vec::new(),
            stats: FrameStats {
                dots_outside: g.outside,
                stations_located: g.located,
                stations_total: g.total,
                ..FrameStats::default()
            },
        };
        let mut buf = Vec::new();
        let mut pts = Vec::new();
        for (u, unit) in self.units.iter().enumerate() {
            let Ok(u16_) = u16::try_from(u) else { continue };
            let own = ct.units.contains(&u16_);
            let (ku8, tol) = if own { (k8, tol) } else { (kn8, tol_n) };
            if use_index && !index::cap_meets(&unit.cap, ground, tol) {
                continue;
            }
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
            let Some(b) = self.blob(u16_, ku8, Layer::Land) else {
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
                // one missing blob if a line the index admits would be drawn, as for the land
                // (review 2, finding 5)
                None => {
                    let admitted = ct
                        .sub_lines
                        .iter()
                        .any(|cap| !use_index || index::cap_meets(cap, ground, stol));
                    out.stats.missing_blobs += usize::from(admitted);
                }
            }
        }

        // insets, at the fit view only (D7)
        let boxes = if at_fit {
            self.inset_boxes(c, pane)
        } else {
            Vec::new()
        };
        if at_fit {
            out.insets = self.insets(c, &boxes, &mut out.stats);
        }

        // the dots, larger first as gathered; an inset's in its box at the fit (decision 4)
        let drawn_boxes: Vec<[f64; 4]> = boxes.iter().flatten().copied().collect();
        let under = |[px, py]: [f64; 2], [x, y, w, h]: [f64; 4]| {
            px >= x && px <= x + w && py >= y && py <= y + h
        };
        for d in &g.dots {
            let r = dot_radius(d.ids.len());
            let at = match d.part.role {
                Role::Inset(i) if at_fit => {
                    let placed = boxes.get(usize::from(i)).copied().flatten().and_then(|b| {
                        let ins = ct.insets.get(usize::from(i))?;
                        let p = inset_projection(ins, b)?.to_pt(d.lon, d.lat)?;
                        under(p, b).then_some(p)
                    });
                    if placed.is_none() {
                        out.stats.dots_hidden += 1;
                    }
                    placed
                }
                _ => {
                    let Some([x, y]) = frame_l.fwd(d.lon, d.lat) else {
                        continue;
                    };
                    let p = to_pt(x, y);
                    if drawn_boxes.iter().any(|&b| under(p, b)) {
                        out.stats.dots_hidden += 1;
                        None
                    } else {
                        let [px, py] = p;
                        let on =
                            px >= -r && px <= pane.width + r && py >= -r && py <= pane.height + r;
                        on.then_some(p)
                    }
                }
            };
            if let Some([x, y]) = at {
                out.dots.push(Dot {
                    x: round(x),
                    y: round(y),
                    r: round(r),
                    n: u32::try_from(d.ids.len()).unwrap_or(u32::MAX),
                    uuids: d.ids.clone(),
                    place: d.place.clone(),
                });
            }
        }
        Some(out)
    }

    /// The inset boxes at a pane, in table order (I1 + C1, M4b commit 5 — one rule with the
    /// tool's `inset_tables`): each box is the golden rect at the band's stored scale, anchored at
    /// its corner with the row's gaps (`rules::inset_box_at`) or, when its golden rect abuts an
    /// earlier drawn box at the same corner, beside that box with the golden gap (the stacking
    /// rule, `rules::inset_box_beside`); `None` where the scale is 0, or the box leaves the pane,
    /// meets the controls' rect or overlaps a box drawn before it (a box not drawn blocks
    /// nothing — review 3, finding 2). Never moved onto the land, never clamped.
    pub fn inset_boxes(&self, c: usize, pane: &Pane) -> Vec<Option<[f64; 4]>> {
        let Some(ct) = self.countries.get(c) else {
            return Vec::new();
        };
        let bands = &self.header.bands;
        let mut placed: Vec<[f64; 4]> = vec![rules::controls_rect(pane)];
        let mut out: Vec<Option<[f64; 4]>> = Vec::with_capacity(ct.insets.len());
        for (i, ins) in ct.insets.iter().enumerate() {
            let s = ins.scale_at(bands, pane);
            if s <= 0.0 {
                out.push(None);
                continue;
            }
            let golden = ins.golden();
            // the first earlier inset at the same corner whose box this one abuts, if drawn
            let abut = ct.insets.iter().take(i).enumerate().find_map(|(j, a)| {
                (a.corner == ins.corner)
                    .then(|| rules::abuts(golden, a.golden(), ins.corner).map(|ab| (j, ab)))
                    .flatten()
            });
            let rect = match abut.and_then(|(j, ab)| out.get(j).copied().flatten().map(|r| (r, ab)))
            {
                Some((a_rect, ab)) => {
                    rules::inset_box_beside(golden, ins.corner, pane, s, a_rect, ab)
                }
                None => rules::inset_box_at(golden, ins.corner, pane, s),
            };
            if rules::box_fits(rect, pane) && placed.iter().all(|&o| rules::boxes_apart(rect, o)) {
                placed.push(rect);
                out.push(Some(rect));
            } else {
                out.push(None);
            }
        }
        out
    }

    /// The insets drawn at the fit, in the boxes `inset_boxes` placed.
    fn insets(&self, c: usize, boxes: &[Option<[f64; 4]>], stats: &mut FrameStats) -> Vec<Inset> {
        let Some(ct) = self.countries.get(c) else {
            return Vec::new();
        };
        let mut buf = Vec::new();
        let mut out = Vec::new();
        for (i, ins) in ct.insets.iter().enumerate() {
            let Some(rect) = boxes.get(i).copied().flatten() else {
                stats.insets_dropped += 1;
                continue;
            };
            let [rx, ry, rw, rh] = rect;
            // the group fitted to this band's box: a smaller box, a coarser scale, maybe a
            // coarser level — the tool stored the blob for every band's level (commit 4)
            let Some(proj) = inset_projection(ins, rect) else {
                stats.insets_dropped += 1;
                continue;
            };
            let k = rules::level_for(proj.scale);
            let (Ok(k8), Ok(i8_)) = (u8::try_from(k), u8::try_from(i)) else {
                continue;
            };
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
                                proj.to_pt(lon, lat)
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

    /// Per inset drawn at the fit view, its box's distance to the country's land there, points
    /// (S6: ≥ 12). An inset not drawn at this pane (`insets_dropped`) has no entry.
    pub fn inset_clearance(&self, c: usize, pane: &Pane) -> Vec<(String, f64)> {
        let Some(fit) = self.fit(c, pane) else {
            return Vec::new();
        };
        let Some(frame) = self.frame(c, pane, fit) else {
            return Vec::new();
        };
        frame
            .insets
            .iter()
            .map(|ins| {
                let [x, y, w, h] = ins.rect.map(f64::from);
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
    use crate::format::{Corner, Encoding, Part, Unit, write};

    /// `missing_blobs` counts units, as documented (review finding 9): a neighbour unit with three
    /// parts in view and no blob at the frame's level adds one missing blob, and its rings are not
    /// counted as considered — none was decoded; an inset of two parts whose level has no blob is
    /// one missing blob, as an inset of one part was. The unindexed frame agrees. Fails with the
    /// count per part for the land or for the inset (the two add 4, and `rings_considered` counts
    /// the three rings never read), with it dropped, or with the ring caps ignored.
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

    /// The frame's placement reads the stored scales with the stacking rule and the controls' rect
    /// (M4b commit 5), on a synthetic resource where the real one cannot show it (Hawaii is never
    /// drawn while Alaska is under 100 %): inset A, top-left `[8, 8, 80, 60]` at 50 % everywhere, is
    /// `[8, 8, 40, 30]`; inset B, `[96, 8, 60, 44]` beside it in the golden table (gap 8), follows
    /// A's right edge to x 56; with A's table all 0, B is at its own 96 and A is the one dropped.
    /// A bottom-left box 250 pt wide at 100 % crosses the controls' rect and is `None`. Fails with
    /// the stacking rule left out of the frame (B at 96), with the controls not placed (the wide
    /// box drawn), or with a dropped A still followed.
    #[test]
    fn the_frame_places_boxes_by_the_stored_scales() {
        use crate::format::Inset;
        let inset = |label: &str, corner: Corner, rect: [f32; 4], pct: u8| Inset {
            label: label.into(),
            corner,
            rect,
            lat0: 38.4,
            lon0: -27.3,
            centre_km: [0.0, 0.0],
            size_km: [600.0, 300.0],
            scale: 7.37,
            scale_pct: vec![pct; crate::format::Bands::BUILT.span()],
        };
        let resource = |a_pct: u8| {
            let mut countries = synthetic_countries();
            countries[0].insets = vec![
                inset("A", Corner::TopLeft, [8.0, 8.0, 80.0, 60.0], a_pct),
                inset("B", Corner::TopLeft, [96.0, 8.0, 60.0, 44.0], 100),
                inset("Wide", Corner::BottomLeft, [8.0, 252.0, 250.0, 40.0], 100),
            ];
            let b = write(
                &header(),
                &synthetic_units(),
                &countries,
                &synthetic_blobs(),
                Encoding::Raw,
            )
            .unwrap();
            Store::load(&b).unwrap()
        };
        let s = resource(50);
        let boxes = s.inset_boxes(0, &Pane::GOLDEN);
        assert_eq!(boxes[0], Some([8.0, 8.0, 40.0, 30.0]));
        assert_eq!(
            boxes[1],
            Some([56.0, 8.0, 60.0, 44.0]),
            "B beside A's right edge + 8"
        );
        assert_eq!(boxes[2], None, "on the controls' rect");
        let f = s
            .frame(0, &Pane::GOLDEN, s.fit(0, &Pane::GOLDEN).unwrap())
            .unwrap();
        assert_eq!(f.stats.insets_dropped, 1);
        assert_eq!(f.insets.len(), 2);
        assert_eq!(f.insets[1].rect, [56.0, 8.0, 60.0, 44.0]);
        let s = resource(0);
        let boxes = s.inset_boxes(0, &Pane::GOLDEN);
        assert_eq!(boxes[0], None);
        assert_eq!(
            boxes[1],
            Some([96.0, 8.0, 60.0, 44.0]),
            "A dropped: B by itself"
        );
        // at 328 × 178 the bottom-left box anchors 8 pt up from the shorter pane and still meets
        // the controls; A and B keep their top anchors
        let s = resource(50);
        let boxes = s.inset_boxes(0, &Pane::band(178));
        assert_eq!(boxes[1], Some([56.0, 8.0, 60.0, 44.0]));
        assert_eq!(boxes[2], None);
    }

    /// The subdivisions count a missing blob only when one of the country's lines meets the view,
    /// as the land does (review 2, finding 5). A synthetic country 4 000 km across (fit 15.4
    /// km/pt, level 3) whose subdivisions are stored at level 2 only: a line far from the view
    /// adds nothing to `missing_blobs` (fails with the count unconditional: it adds 1, 3 for 2)
    /// and a line in the view, or just past it within the level's tolerance, adds one (fails
    /// with the count never made, by the index in the unindexed frame, or without the
    /// tolerance). The unindexed frame admits every line, as it admits every land ring.
    #[test]
    fn subdivisions_count_a_missing_blob_only_in_view() {
        let resource = |subdivisions: bool, line: crate::format::Cap| {
            let mut countries = synthetic_countries();
            countries[0].bbox_km = [-2000.0, -2000.0, 2000.0, 2000.0];
            countries[0].subdivisions = subdivisions;
            countries[0].sub_lines = vec![line];
            let b = write(
                &header(),
                &synthetic_units(),
                &countries,
                &synthetic_blobs(),
                Encoding::Raw,
            )
            .unwrap();
            Store::load(&b).unwrap()
        };
        let pane = Pane::GOLDEN;
        let near = cap(-8.0, 40.0, 100.0);
        let far = cap(100.0, -40.0, 50.0);
        let missing = |s: &Store, indexed: bool| {
            let fit = s.fit(0, &pane).unwrap();
            assert!(fit.scale > rules::SUBDIVISIONS_ABOVE_KM_PER_PT, "{fit:?}");
            assert_eq!(rules::level_for(fit.scale), 3);
            let f = if indexed {
                s.frame(0, &pane, fit)
            } else {
                s.frame_unindexed(0, &pane, fit)
            };
            f.unwrap().stats.missing_blobs
        };
        let base = missing(&resource(false, near), true);
        assert_eq!(
            missing(&resource(true, far), true),
            base,
            "a line out of view"
        );
        assert_eq!(
            missing(&resource(true, near), true),
            base + 1,
            "a line in view"
        );
        let base_u = missing(&resource(false, near), false);
        assert_eq!(missing(&resource(true, far), false), base_u + 1);
        // a 1 km line just past the view's ground cap, north by half the level's tolerance: in
        // view by the index's test, as the coverage test reads it (fails with the tolerance off)
        let s0 = resource(false, near);
        let fit = s0.fit(0, &pane).unwrap();
        let ct = &s0.countries[0];
        let (hw, hh) = (
            (pane.width / 2.0 + CLIP_MARGIN_PT) * fit.scale,
            (pane.height / 2.0 + CLIP_MARGIN_PT) * fit.scale,
        );
        let [cx, cy] = fit.centre;
        let g = index::ground_cap(
            &Laea::new(ct.lat0, ct.lon0),
            [cx - hw, cy - hh, cx + hw, cy + hh],
        );
        let stol = index::tolerance_km(LADDER[3], SUB_TOL_PT);
        let d = g.2 + 1.0 + stol / 2.0;
        let edge = cap(
            g.0 as f32,
            (g.1 + (d / crate::laea::R_AUTHALIC_KM).to_degrees()) as f32,
            1.0,
        );
        assert!(!index::cap_meets(&edge, g, 0.0) && index::cap_meets(&edge, g, stol));
        assert_eq!(
            missing(&resource(true, edge), true),
            base + 1,
            "a line in the band"
        );
    }
}
