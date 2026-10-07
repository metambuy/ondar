//! The map resource (M4a): loaded once at startup, on its own thread, from
//! `resource_dir()/map/world.ondarmap` — beside the binary in `pnpm tauri:dev` (tauri-build copies
//! `bundle.resources` there) and in `Contents/Resources` in the bundle. One log line either way:
//! `map resource loaded path=… bytes=… units=… countries=… ms=…`, or `map resource unavailable
//! reason=…` — the app runs without a map (`map_pull` answers `unavailable`).
//!
//! **The view session (M4b commit 6).** Rust owns the selected country, the band, the view and
//! the pending inputs; the page sends inputs and receives frames. `Session` is the pure part:
//! `select` picks the country, `pull` folds the page's inputs into the pending set, applies them —
//! fit → zoom steps about the pane's centre → pan — clamps (D6) and decides whether a frame is
//! due (the view, the country or the band changed since the last frame); the command frames off
//! the main thread with the lock released and replies. Every frame carries a sequence number,
//! bumped only when a frame is produced (and on `select`), so a reply that arrives behind a newer
//! one is never drawn over it. A pan at the fit changes nothing (D6) and replies nothing.

use ondar_map::format::Store;
use ondar_map::frame::{Frame, Lookup, View};
use ondar_map::rules::{self, Pane};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tauri::Manager;
use ts_rs::TS;

use crate::panel::MapBand;

/// The loaded store, set once by the loader thread; `None` inside if the load failed.
#[derive(Clone, Default)]
pub struct MapState(pub Arc<OnceLock<Option<Store>>>);

/// The view session, in Tauri state beside the store. `Mutex::lock().unwrap()`: poison
/// propagation only (CLAUDE.md's exemption).
#[derive(Default)]
pub struct MapSessionState(pub Mutex<Session>);

/// What the page accumulated since its last pull: wheel and drag deltas in points (a CSS px is a
/// point), zoom steps from the `−`/`+` controls (negative = out), and whether `fit` was pressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MapInputs {
    pub pan_pt: [f64; 2],
    pub zoom_steps: i32,
    pub fit: bool,
}

/// What a pull answers (the frame as JSON — Step 0's measured path; replies, not events).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MapStatus {
    /// A frame for the selected country at the band.
    Frame,
    /// R7: no shape for the selected code (`XX`, an unknown code).
    NoMap,
    /// The resource did not load (or has not loaded yet): the app runs without a map.
    Unavailable,
    /// The layout has no band (collapsed, or an expanded height under the floor).
    NoBand,
}

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct MapReply {
    /// Grows with every frame produced and every selection; the page draws a reply only if this
    /// is newer than the frame on screen.
    pub seq: u32,
    pub status: MapStatus,
    pub band: Option<MapBand>,
    /// With `Frame`: the clamped view the frame was made at, and the frame.
    pub view: Option<View>,
    pub frame: Option<Frame>,
}

/// What `select` landed on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Selected {
    #[default]
    None,
    NoMap,
    Country(usize),
}

/// The pure session (the command wraps it in `MapSessionState`).
#[derive(Debug, Default)]
pub struct Session {
    selected: Selected,
    band: Option<MapBand>,
    /// The view, or `None` for the fit.
    view: Option<View>,
    pending: MapInputs,
    seq: u32,
    /// The (country, band, view) last framed, so an unchanged view frames nothing.
    framed: Option<(usize, MapBand, View)>,
}

/// What a pull decided: a frame to compute (off the lock), or a reply as it stands.
#[derive(Debug, PartialEq)]
pub enum Step {
    Frame {
        seq: u32,
        country: usize,
        pane: Pane,
        view: View,
        band: MapBand,
    },
    Reply(Option<MapReply>),
}

/// The pane a band is framed in: the band's size with the crate's padding.
pub fn pane_of(band: MapBand) -> Pane {
    Pane {
        width: band.width,
        height: band.height,
        padding: rules::BAND_PADDING_PT,
    }
}

impl Session {
    #[cfg(test)]
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// A country was selected (or re-selected): the view returns to the fit, the pending inputs
    /// are dropped, and the next pull frames. `store` is `None` while the resource is unavailable.
    /// Returns what the lookup found, as the command's log line names it (`country`, `no_map`,
    /// `unavailable`) — the one line a normal run leaves per dropdown change (acceptance review A2).
    pub fn select(&mut self, store: Option<&Store>, code: &str) -> &'static str {
        self.selected = match store.map(|s| s.lookup(code)) {
            None => Selected::None,
            Some(Lookup::NoMap) => Selected::NoMap,
            Some(Lookup::Country(i)) => Selected::Country(i),
        };
        self.view = None;
        self.pending = MapInputs::default();
        self.framed = None;
        self.seq = self.seq.wrapping_add(1);
        match self.selected {
            Selected::None => "unavailable",
            Selected::NoMap => "no_map",
            Selected::Country(_) => "country",
        }
    }

    /// Fold `inputs` into the pending set (sums; `fit` sticks).
    fn fold(&mut self, inputs: MapInputs) {
        self.pending.pan_pt[0] += inputs.pan_pt[0];
        self.pending.pan_pt[1] += inputs.pan_pt[1];
        self.pending.zoom_steps += inputs.zoom_steps;
        self.pending.fit |= inputs.fit;
    }

    /// The page pulled: fold its inputs, apply the pending set to the view at `band`, clamp, and
    /// decide. A band change (the layout's band differs from the session's) returns the view to
    /// the fit — "per expanded session". `fit` wins over pending pan and zoom; zoom steps halve
    /// (`+`) or double (`−`) the scale about the pane's centre; a pan moves the centre by the
    /// deltas at the new scale (y up in km, down on the pane). The result is `clamp_view`'s.
    pub fn pull(
        &mut self,
        store: Option<&Store>,
        band: Option<MapBand>,
        inputs: MapInputs,
    ) -> Step {
        self.fold(inputs);
        if band != self.band {
            self.band = band;
            self.view = None;
            self.framed = None;
        }
        let reply = |status: MapStatus, seq: u32| {
            Step::Reply(Some(MapReply {
                seq,
                status,
                band,
                view: None,
                frame: None,
            }))
        };
        let Some(store) = store else {
            self.pending = MapInputs::default();
            return reply(MapStatus::Unavailable, self.seq);
        };
        let Some(band) = band else {
            self.pending = MapInputs::default();
            return reply(MapStatus::NoBand, self.seq);
        };
        let country = match self.selected {
            Selected::Country(c) => c,
            Selected::NoMap => {
                self.pending = MapInputs::default();
                return reply(MapStatus::NoMap, self.seq);
            }
            // nothing selected yet: nothing to frame, nothing to say
            Selected::None => {
                self.pending = MapInputs::default();
                return Step::Reply(None);
            }
        };
        let pane = pane_of(band);
        let Some(fit) = store.fit(country, &pane) else {
            self.pending = MapInputs::default();
            return Step::Reply(None);
        };
        let pending = std::mem::take(&mut self.pending);
        let base = if pending.fit {
            fit
        } else {
            self.view.unwrap_or(fit)
        };
        let (zoom, pan) = if pending.fit {
            (0, [0.0, 0.0])
        } else {
            (pending.zoom_steps, pending.pan_pt)
        };
        let scale = base.scale * 2f64.powi(-zoom);
        let wanted = View {
            centre: [
                base.centre[0] + pan[0] * scale,
                base.centre[1] - pan[1] * scale,
            ],
            scale,
        };
        let Some(view) = store.clamp_view(country, &pane, wanted) else {
            return Step::Reply(None);
        };
        self.view = Some(view);
        if self.framed == Some((country, band, view)) {
            return Step::Reply(None);
        }
        self.framed = Some((country, band, view));
        self.seq = self.seq.wrapping_add(1);
        Step::Frame {
            seq: self.seq,
            country,
            pane,
            view,
            band,
        }
    }

    /// `pull`, with the frame computed inline (the tests; the command frames off the lock
    /// instead).
    #[cfg(test)]
    pub fn pull_sync(
        &mut self,
        store: Option<&Store>,
        band: Option<MapBand>,
        inputs: MapInputs,
    ) -> Option<MapReply> {
        match self.pull(store, band, inputs) {
            Step::Reply(r) => r,
            Step::Frame {
                seq,
                country,
                pane,
                view,
                band,
            } => Some(frame_reply(store?, seq, country, &pane, view, band)),
        }
    }
}

/// The frame for a decided step, as the reply.
pub fn frame_reply(
    store: &Store,
    seq: u32,
    country: usize,
    pane: &Pane,
    view: View,
    band: MapBand,
) -> MapReply {
    let frame = store.frame(country, pane, view);
    MapReply {
        seq,
        status: MapStatus::Frame,
        band: Some(band),
        view: Some(view),
        frame,
    }
}

/// The resource's path under a resource directory.
pub fn resource_path(resource_dir: &Path) -> PathBuf {
    resource_dir.join("map").join("world.ondarmap")
}

/// Reads and loads the resource: the store and its byte count, or why not.
pub fn load(path: &Path) -> Result<(Store, usize), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let n = bytes.len();
    Store::load(&bytes)
        .map(|s| (s, n))
        .map_err(|e| e.to_string())
}

/// Manages the state and the session, and starts the loader thread.
pub fn setup(app: &tauri::App) {
    let state = MapState::default();
    app.manage(state.clone());
    app.manage(MapSessionState::default());
    let dir = app.path().resource_dir();
    let spawned = std::thread::Builder::new()
        .name("ondar-map-load".into())
        .spawn(move || {
            let t0 = Instant::now();
            let result = dir
                .map_err(|e| format!("no resource directory: {e}"))
                .map(|d| resource_path(&d))
                .and_then(|p| load(&p).map(|ok| (p, ok)));
            match result {
                Ok((path, (store, bytes))) => {
                    log::info!(
                        "map resource loaded path={} bytes={bytes} units={} countries={} ms={:.1}",
                        path.display(),
                        store.units.len(),
                        store.countries.len(),
                        t0.elapsed().as_secs_f64() * 1e3
                    );
                    let _ = state.0.set(Some(store));
                }
                Err(reason) => {
                    log::warn!("map resource unavailable reason={reason}");
                    let _ = state.0.set(None);
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("map resource unavailable reason=no loader thread: {e}");
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn store() -> &'static Store {
        static S: OnceLock<Store> = OnceLock::new();
        S.get_or_init(|| {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
            load(&resource_path(&dir)).unwrap().0
        })
    }

    fn band(h: f64) -> Option<MapBand> {
        crate::panel::band_rect(420.0 + h)
    }

    fn inputs(pan: [f64; 2], zoom: i32, fit: bool) -> MapInputs {
        MapInputs {
            pan_pt: pan,
            zoom_steps: zoom,
            fit,
        }
    }

    /// `select` resets to the fit and bumps `seq`; the first pull frames at the fit; a pull with
    /// nothing pending frames nothing; re-selecting the same country frames again (a new seq).
    /// Fails with every pull framing, with `seq` bumped on select alone, or with select not
    /// resetting.
    #[test]
    fn select_resets_to_the_fit_and_bumps_seq() {
        let s = store();
        let mut ses = Session::default();
        assert_eq!(
            ses.pull_sync(Some(s), band(178.0), MapInputs::default()),
            None,
            "nothing selected"
        );
        ses.select(Some(s), "PT");
        assert_eq!(ses.seq(), 1);
        let r = ses
            .pull_sync(Some(s), band(178.0), MapInputs::default())
            .unwrap();
        assert_eq!((r.seq, r.status), (2, MapStatus::Frame));
        let pt = match s.lookup("PT") {
            Lookup::Country(i) => i,
            Lookup::NoMap => unreachable!(),
        };
        let pane = pane_of(band(178.0).unwrap());
        assert_eq!(r.view, Some(s.fit(pt, &pane).unwrap()));
        assert!(r.frame.as_ref().is_some_and(|f| !f.land.is_empty()));
        assert_eq!(
            ses.pull_sync(Some(s), band(178.0), MapInputs::default()),
            None
        );
        // zoomed in, then re-selected: the view is the fit again (fails with select not resetting)
        ses.pull_sync(Some(s), band(178.0), inputs([0.0, 0.0], 2, false))
            .unwrap();
        ses.select(Some(s), "pt");
        let r2 = ses
            .pull_sync(Some(s), band(178.0), MapInputs::default())
            .unwrap();
        assert_eq!((r2.seq, r2.view), (5, r.view));
    }

    /// `+` halves the scale about the pane's centre, `−` doubles it back (clamped at the fit); a
    /// band change returns the view to the fit; `fit` wins over pending pan and zoom. Fails with
    /// the zoom sign inverted, the band change not resetting, or fit not winning.
    #[test]
    fn zoom_steps_band_change_and_fit() {
        let s = store();
        let mut ses = Session::default();
        ses.select(Some(s), "US");
        let fit = ses
            .pull_sync(Some(s), band(178.0), MapInputs::default())
            .unwrap()
            .view
            .unwrap();
        let r = ses
            .pull_sync(Some(s), band(178.0), inputs([0.0, 0.0], 1, false))
            .unwrap();
        let v = r.view.unwrap();
        assert!(
            (v.scale - fit.scale / 2.0).abs() < 1e-9 && v.centre == fit.centre,
            "{v:?}"
        );
        let back = ses
            .pull_sync(Some(s), band(178.0), inputs([0.0, 0.0], -3, false))
            .unwrap()
            .view
            .unwrap();
        assert_eq!(back, fit, "clamped at the fit");
        ses.pull_sync(Some(s), band(178.0), inputs([0.0, 0.0], 2, false))
            .unwrap();
        let r = ses
            .pull_sync(Some(s), band(300.0), MapInputs::default())
            .unwrap();
        let pane300 = pane_of(band(300.0).unwrap());
        let us = match s.lookup("US") {
            Lookup::Country(i) => i,
            Lookup::NoMap => unreachable!(),
        };
        assert_eq!(
            r.view,
            Some(s.fit(us, &pane300).unwrap()),
            "a band change returns to the fit"
        );
        ses.pull_sync(Some(s), band(300.0), inputs([40.0, -20.0], 2, false))
            .unwrap();
        let r = ses
            .pull_sync(Some(s), band(300.0), inputs([99.0, 99.0], 1, true))
            .unwrap();
        assert_eq!(r.view, Some(s.fit(us, &pane300).unwrap()), "fit wins");
    }

    /// Pan deltas sum across pulls and apply once at the new scale, y down on the pane; at the
    /// fit a pan changes nothing and replies `None` (D6); `seq` grows only with a frame; a huge
    /// pan is clamped to the fit rectangle and frames once. Fails with y not negated.
    #[test]
    fn pan_sums_and_the_fit_cannot_pan() {
        let s = store();
        let mut ses = Session::default();
        ses.select(Some(s), "RU");
        let fit = ses
            .pull_sync(Some(s), band(178.0), MapInputs::default())
            .unwrap();
        assert_eq!(
            ses.pull_sync(Some(s), band(178.0), inputs([50.0, 30.0], 0, false)),
            None
        );
        assert_eq!(ses.seq(), fit.seq);
        // zoomed in twice, then two pans that sum: the centre moves by (dx·s, −dy·s)
        let z = ses
            .pull_sync(Some(s), band(178.0), inputs([0.0, 0.0], 2, false))
            .unwrap();
        let zv = z.view.unwrap();
        // fold without a frame between: the first pull's step holds the frame decision, so the two
        // inputs are sent in one pull here
        let r = ses
            .pull_sync(Some(s), band(178.0), inputs([10.0, 5.0], 0, false))
            .unwrap();
        let v = r.view.unwrap();
        assert!((v.centre[0] - (zv.centre[0] + 10.0 * zv.scale)).abs() < 1e-6);
        assert!((v.centre[1] - (zv.centre[1] - 5.0 * zv.scale)).abs() < 1e-6);
        assert_eq!(r.seq, z.seq + 1);
        // a huge pan is clamped to the fit rectangle and still frames once
        let r = ses
            .pull_sync(Some(s), band(178.0), inputs([1e6, 1e6], 0, false))
            .unwrap();
        let ru = match s.lookup("RU") {
            Lookup::Country(i) => i,
            Lookup::NoMap => unreachable!(),
        };
        assert_eq!(
            r.view,
            s.clamp_view(
                ru,
                &pane_of(band(178.0).unwrap()),
                View {
                    centre: [1e9, -1e9],
                    scale: zv.scale
                }
            )
        );
        assert_eq!(
            ses.pull_sync(Some(s), band(178.0), inputs([1e6, 1e6], 0, false)),
            None,
            "at the edge already"
        );
    }

    /// Inputs folded while a frame is held are applied on the next pull, none lost: two `pull`s
    /// whose steps are not framed in between fold into one frame, with a higher `seq`. Fails
    /// with the pending set applied twice.
    #[test]
    fn inputs_fold_while_a_frame_is_held() {
        let s = store();
        let mut ses = Session::default();
        ses.select(Some(s), "FR");
        ses.pull_sync(Some(s), band(178.0), inputs([0.0, 0.0], 3, false))
            .unwrap();
        let a = ses.pull(Some(s), band(178.0), inputs([3.0, 0.0], 0, false));
        let Step::Frame {
            seq: sa, view: va, ..
        } = a
        else {
            panic!("{a:?}")
        };
        // the page has not drawn `a` yet; more input arrives and is pulled: it applies on top
        let b = ses.pull(Some(s), band(178.0), inputs([4.0, 0.0], 0, false));
        let Step::Frame {
            seq: sb, view: vb, ..
        } = b
        else {
            panic!("{b:?}")
        };
        assert_eq!(sb, sa + 1, "a stale seq is lower than a newer one");
        assert!((vb.centre[0] - (va.centre[0] + 4.0 * va.scale)).abs() < 1e-6);
    }

    /// No resource → `unavailable` at the current seq (not bumped: the page draws the state once);
    /// an unknown code → `no_map`; no band → `no_band`; each clears the pending inputs.
    #[test]
    fn the_three_states_without_a_frame() {
        let s = store();
        let mut ses = Session::default();
        assert_eq!(ses.select(None, "PT"), "unavailable");
        let r = ses
            .pull_sync(None, band(178.0), inputs([5.0, 5.0], 1, false))
            .unwrap();
        assert_eq!(
            (r.status, r.seq, r.frame.is_none()),
            (MapStatus::Unavailable, 1, true)
        );
        assert_eq!(
            ses.pull_sync(None, band(178.0), MapInputs::default())
                .unwrap()
                .seq,
            1
        );
        assert_eq!(ses.select(Some(s), "XX"), "no_map");
        let r = ses
            .pull_sync(Some(s), band(178.0), MapInputs::default())
            .unwrap();
        assert_eq!((r.status, r.seq), (MapStatus::NoMap, 2));
        assert_eq!(ses.select(Some(s), "PT"), "country");
        let r = ses
            .pull_sync(Some(s), None, inputs([5.0, 5.0], 1, false))
            .unwrap();
        assert_eq!((r.status, r.seq, r.band), (MapStatus::NoBand, 3, None));
        let r = ses
            .pull_sync(Some(s), band(178.0), MapInputs::default())
            .unwrap();
        assert_eq!((r.status, r.seq), (MapStatus::Frame, 4));
        assert_eq!(
            r.view.unwrap(),
            store()
                .fit(
                    match s.lookup("PT") {
                        Lookup::Country(i) => i,
                        Lookup::NoMap => unreachable!(),
                    },
                    &pane_of(band(178.0).unwrap())
                )
                .unwrap(),
            "the pending zoom was cleared by the no-band reply"
        );
    }

    /// Round 3, C1: RU, US and IN at 300 and RU, US at 178 are coarser than 8 km/pt and flagged,
    /// so the frame the session hands the page must carry their subdivision lines (RU 200 lines /
    /// 1 541 points at level 24, US 121, IN 73), with no blob missing — if this passes, the
    /// renderer is where they vanish (it was: the frame had them, the renderer hid them).
    #[test]
    fn c1_the_fit_frame_carries_subdivisions() {
        let s = store();
        for (code, h) in [
            ("RU", 300.0),
            ("US", 300.0),
            ("IN", 300.0),
            ("RU", 178.0),
            ("US", 178.0),
        ] {
            let mut ses = Session::default();
            ses.select(Some(s), code);
            let r = ses
                .pull_sync(Some(s), band(h), inputs([0.0, 0.0], 0, false))
                .unwrap();
            let f = r.frame.expect("a frame");
            assert_eq!(f.stats.missing_blobs, 0, "{code} at {h}: missing blobs");
            assert!(
                !f.subdivisions.is_empty(),
                "{code} at {h} (scale {:.2}): no subdivision lines",
                f.view.scale
            );
            let pts: usize = f.subdivisions.iter().map(|l| l.len()).sum();
            eprintln!(
                "{code} at {h}: level {} subdivisions {} lines, {pts} points; land {} shapes",
                f.level,
                f.subdivisions.len(),
                f.land.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped resource loads through the shell's path rule (`resources/` +
    /// `map/world.ondarmap`; fails with `map/` dropped); a missing file and a file that is not a
    /// resource are reasons, not panics.
    #[test]
    fn the_resource_loads_and_failures_are_reasons() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        let (store, bytes) = load(&resource_path(&dir)).unwrap();
        assert_eq!((store.units.len(), store.countries.len()), (267, 248));
        assert!(bytes > 1_000_000);
        let missing = load(Path::new("/nonexistent/map/world.ondarmap")).unwrap_err();
        assert!(missing.starts_with("read:"), "{missing}");
        let bad = load(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("Cargo.toml")
                .as_path(),
        )
        .unwrap_err();
        assert!(bad.contains("bad magic"), "{bad}");
    }
}
