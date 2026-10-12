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
//!
//! **The stations (M4c, S3).** `select` frames at once with no dots: the stored list is read,
//! gathered and located in a task spawned after it (`spawn_gather`), never in front of the first
//! pull. The task holds a `Ticket` (the country, `select_gen`, an issue number); `install` takes
//! its dots only if the selection and its generation are still the ticket's and no later ticket
//! installed first, then bumps `dots_gen`, which the next pull treats as a change, at the view it
//! has; the task then emits `map:changed`. A refresh that lands for the selected country takes the
//! same path (`on_landed`). `hit` tests the dots of the newest frame produced.

use ondar_map::format::Store;
use ondar_map::frame::{Dot, Frame, Lookup, View};
use ondar_map::gather::{Gathered, Point};
use ondar_map::rules::{self, Pane};
use ondar_stations::{ServiceError, Station};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tauri::{Emitter, Manager};
use ts_rs::TS;

use crate::panel::MapBand;

/// The loaded store, set once by the loader thread; `None` inside if the load failed.
#[derive(Clone, Default)]
pub struct MapState(pub Arc<OnceLock<Option<Store>>>);

/// The view session, in Tauri state beside the store. `Mutex::lock().unwrap()`: poison
/// propagation only (`.claude/rules/rust.md`'s exemption).
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

/// What `map_hit` answers: the dot's stations (uuids, list order), their count and the dot's
/// place (empty when none of its stations has one).
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct MapHit {
    pub uuids: Vec<String>,
    pub n: u32,
    pub place: String,
}

/// How far outside a dot's radius a click still hits it, points.
pub const HIT_SLOP_PT: f32 = 2.0;

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
    /// The (country, band, view, `dots_gen`) last framed, so an unchanged view frames nothing.
    framed: Option<(usize, MapBand, View, u64)>,
    /// The selected code as given, trimmed and upper-cased (`""` before any select).
    code: String,
    /// Bumped by every `select`: a ticket from before it installs nothing.
    select_gen: u64,
    /// The last ticket issued and the newest one installed, so a slower read of an older list
    /// never replaces a newer one.
    issued: u64,
    installed: u64,
    /// The selected country's dots and counts (empty after `select` until a task installs them).
    dots: Arc<Gathered>,
    /// Bumped by every install; part of `framed`, so the next pull frames them.
    dots_gen: u64,
    /// The newest frame's seq and its dots, for `hit`.
    framed_dots: (u32, Vec<Dot>),
}

/// A gather in flight for the selected country, as `ticket_for` issued it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub code: String,
    country: usize,
    select_gen: u64,
    issue: u64,
}

/// What a pull decided: a frame to compute (off the lock), or a reply as it stands.
// `Reply` holds a whole `Frame` inline (336 B since M4c's dots, the `Frame` variant 124): a `Step`
// is one pull's return value, moved once and never stored in a collection, so boxing it would
// add an allocation per pull for nothing the lint guards against.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq)]
pub enum Step {
    Frame {
        seq: u32,
        country: usize,
        pane: Pane,
        view: View,
        band: MapBand,
        dots: Arc<Gathered>,
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
        self.code = code.trim().to_ascii_uppercase();
        self.select_gen += 1;
        self.dots = Arc::default();
        self.framed_dots = (self.seq, Vec::new());
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
        if self.framed == Some((country, band, view, self.dots_gen)) {
            return Step::Reply(None);
        }
        self.framed = Some((country, band, view, self.dots_gen));
        self.seq = self.seq.wrapping_add(1);
        Step::Frame {
            seq: self.seq,
            country,
            pane,
            view,
            band,
            dots: self.dots.clone(),
        }
    }

    /// A ticket for a gather of `code`'s stations, iff `code` is the selected country's (a
    /// landed refresh for another country, or no country selected, gets none).
    pub fn ticket_for(&mut self, code: &str) -> Option<Ticket> {
        let Selected::Country(country) = self.selected else {
            return None;
        };
        if !code.trim().eq_ignore_ascii_case(&self.code) {
            return None;
        }
        self.issued += 1;
        Some(Ticket {
            code: self.code.clone(),
            country,
            select_gen: self.select_gen,
            issue: self.issued,
        })
    }

    /// Install `g` as the selected country's dots iff the ticket's selection is still the
    /// session's (same country, same `select_gen`) and no later ticket has installed; bumps
    /// `dots_gen` so the next pull frames them. Returns whether it installed.
    pub fn install(&mut self, t: &Ticket, g: Gathered) -> bool {
        let current =
            self.selected == Selected::Country(t.country) && self.select_gen == t.select_gen;
        if !current || t.issue <= self.installed {
            return false;
        }
        self.installed = t.issue;
        self.dots = Arc::new(g);
        self.dots_gen += 1;
        true
    }

    /// A frame was produced for `seq`: its dots become the ones `hit` tests, unless a newer
    /// frame (or a `select`) has already replaced them.
    pub fn framed(&mut self, seq: u32, dots: &[Dot]) {
        // wrapping order: `seq` is newer iff it is ahead by less than half the range
        if (seq.wrapping_sub(self.framed_dots.0) as i32) > 0 {
            self.framed_dots = (seq, dots.to_vec());
        }
    }

    /// The dot under `pt` (pane points) in the newest frame: the nearest whose centre is within
    /// its radius + `HIT_SLOP_PT`, with its index; `None` on a miss or before any frame.
    pub fn hit(&self, pt: [f32; 2]) -> Option<(usize, MapHit)> {
        let d2 = |d: &Dot| (d.x - pt[0]).powi(2) + (d.y - pt[1]).powi(2);
        let (i, d) = self
            .framed_dots
            .1
            .iter()
            .enumerate()
            .filter(|(_, d)| d2(d) <= (d.r + HIT_SLOP_PT).powi(2))
            .min_by(|a, b| d2(a.1).total_cmp(&d2(b.1)))?;
        Some((
            i,
            MapHit {
                uuids: d.uuids.clone(),
                n: d.n,
                place: d.place.clone(),
            },
        ))
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
                dots,
            } => {
                let r = frame_reply(store?, seq, country, &pane, view, band, &dots);
                self.framed(seq, r.frame.as_ref().map_or(&[], |f| &f.dots));
                Some(r)
            }
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
    dots: &Gathered,
) -> MapReply {
    let frame = store.frame_dots(country, pane, view, dots);
    MapReply {
        seq,
        status: MapStatus::Frame,
        band: Some(band),
        view: Some(view),
        frame,
    }
}

/// The shell's one conversion from the directory to the map: a station's uuid, its `geo`
/// (lat, lon) and its `state`, in list order.
pub fn points_of(list: &[Station]) -> Vec<Point> {
    list.iter()
        .map(|s| Point {
            id: s.uuid.clone(),
            geo: s.geo,
            place: s.state.clone(),
        })
        .collect()
}

/// Why a gather ran, for its log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    Select,
    Landed,
}

impl Cause {
    fn as_str(self) -> &'static str {
        match self {
            Cause::Select => "select",
            Cause::Landed => "landed",
        }
    }
}

/// The gather for a ticket: `read` the stored list, gather and locate it on a blocking thread
/// (the store is read-only), then `install` under the session lock and, iff it installed, `emit`.
/// One `map dots …` line: the counts and `ms` from `t0` (the select or the landing) to the
/// install, or why nothing was installed. Returns whether it installed.
pub async fn gather_and_install<R>(
    map: MapState,
    session: &MapSessionState,
    ticket: Ticket,
    cause: Cause,
    t0: Instant,
    read: R,
    emit: impl FnOnce(),
) -> bool
where
    R: Future<Output = Result<Option<Vec<Station>>, ServiceError>>,
{
    let (code, why) = (ticket.code.clone(), cause.as_str());
    let list = match read.await {
        Ok(Some(list)) => list,
        Ok(None) => {
            log::info!("map dots code={code} cause={why} none=no_stored_list");
            return false;
        }
        Err(e) => {
            log::warn!("map dots code={code} cause={why} none=read_failed: {e}");
            return false;
        }
    };
    let t_read = Instant::now();
    let country = ticket.country;
    let gathered = tauri::async_runtime::spawn_blocking(move || {
        let points = points_of(&list);
        map.0
            .get()
            .and_then(|s| s.as_ref())
            .map(|s| s.gather_dots(country, &points))
    })
    .await
    .ok()
    .flatten();
    let Some(g) = gathered else {
        log::warn!("map dots code={code} cause={why} none=no_store");
        return false;
    };
    let t_gather = Instant::now();
    let (total, located, dots, outside) = (g.total, g.located, g.dots.len(), g.outside);
    if !session.0.lock().unwrap().install(&ticket, g) {
        log::info!("map dots code={code} cause={why} dropped=superseded");
        return false;
    }
    emit();
    let ms = |a: Instant, b: Instant| b.duration_since(a).as_secs_f64() * 1e3;
    log::info!(
        "map dots code={code} cause={why} stations={total} located={located} dots={dots} \
         outside={outside} read_ms={:.2} gather_ms={:.2} ms={:.2}",
        ms(t0, t_read),
        ms(t_read, t_gather),
        ms(t0, Instant::now()),
    );
    true
}

/// Spawns `gather_and_install` for a ticket on Tauri's runtime, reading the stored list through
/// the stations handle and emitting `map:changed` on install. Never awaited by its caller.
pub fn spawn_gather(app: tauri::AppHandle, ticket: Ticket, cause: Cause, t0: Instant) {
    tauri::async_runtime::spawn(async move {
        let (Some(map), Some(session), Some(app_state)) = (
            app.try_state::<MapState>(),
            app.try_state::<MapSessionState>(),
            app.try_state::<crate::AppState>(),
        ) else {
            return;
        };
        let code = ticket.code.clone();
        let read = app_state.stations.cached_stations(&code);
        let emit = || {
            if let Err(e) = app.emit(crate::events::MAP_CHANGED, ()) {
                log::warn!("failed to emit map:changed: {e}");
            }
        };
        gather_and_install(map.inner().clone(), &session, ticket, cause, t0, read, emit).await;
    });
}

/// A refresh landed for `cc` (the stations sink, on the service's DB thread): if `cc` is the
/// selected country, regather in a spawned task. Never waits here: the DB thread is the one that
/// answers the task's read, so awaiting it inline would deadlock.
pub fn on_landed(app: &tauri::AppHandle, cc: String) {
    let t0 = Instant::now();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(session) = app.try_state::<MapSessionState>() else {
            return;
        };
        let ticket = session.0.lock().unwrap().ticket_for(&cc);
        if let Some(t) = ticket {
            spawn_gather(app.clone(), t, Cause::Landed, t0);
        }
    });
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
mod dots_tests {
    //! M4c k+3: the dots reach the session off the select path, through a ticket, and the
    //! newest frame's dots answer a click.
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tauri::async_runtime::block_on;

    fn map_state() -> MapState {
        static S: OnceLock<MapState> = OnceLock::new();
        S.get_or_init(|| {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
            let store = load(&resource_path(&dir)).unwrap().0;
            MapState(Arc::new(OnceLock::from(Some(store))))
        })
        .clone()
    }

    fn store() -> &'static Store {
        static S: OnceLock<MapState> = OnceLock::new();
        S.get_or_init(map_state).0.get().unwrap().as_ref().unwrap()
    }

    /// A committed geo slice as the service serves it (normalised; every row survived the rank).
    fn slice(cc: &str) -> Vec<Station> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "crates/ondar-stations/fixtures/stations-{cc}-geo.json"
        ));
        ondar_stations::normalise::stations(&std::fs::read(p).unwrap()).unwrap()
    }

    fn band300() -> Option<MapBand> {
        crate::panel::band_rect(420.0 + 300.0)
    }

    fn zoom(steps: i32) -> MapInputs {
        MapInputs {
            pan_pt: [0.0, 0.0],
            zoom_steps: steps,
            fit: false,
        }
    }

    /// Runs `gather_and_install` to its end with `list` as the read, counting emits.
    fn gather(
        ses: &MapSessionState,
        t: Ticket,
        list: Option<Vec<Station>>,
        emits: &AtomicUsize,
    ) -> bool {
        block_on(gather_and_install(
            map_state(),
            ses,
            t,
            Cause::Select,
            Instant::now(),
            async move { Ok(list) },
            || {
                emits.fetch_add(1, Ordering::SeqCst);
            },
        ))
    }

    fn pull(ses: &MapSessionState, inputs: MapInputs) -> Option<MapReply> {
        ses.0
            .lock()
            .unwrap()
            .pull_sync(Some(store()), band300(), inputs)
    }

    /// The select frames at once with no dots (the stations are not read on that path); a
    /// refresh landing for the selected country, through `ticket_for` and the gather, installs
    /// PT's slice (68 stations: 65 located, 3 outside), emits once, and the next pull frames them
    /// at the zoomed view it had, the one after that frames nothing; a re-select drops them.
    /// Fails if `dots_gen` is left out of `framed` (the pull after the install frames nothing),
    /// if `points_of` swaps lat and lon (0 located), if the install does not emit, or if `select`
    /// keeps the previous country's dots.
    #[test]
    fn a_landed_list_frames_its_dots_at_the_view_kept() {
        let ses = MapSessionState::default();
        ses.0.lock().unwrap().select(Some(store()), "PT");
        let first = pull(&ses, MapInputs::default()).unwrap().frame.unwrap();
        assert_eq!((first.dots.len(), first.stats.stations_total), (0, 0));
        let zoomed = pull(&ses, zoom(1)).unwrap();
        let t = ses.0.lock().unwrap().ticket_for("PT").unwrap();
        let emits = AtomicUsize::new(0);
        assert!(gather(&ses, t, Some(slice("PT")), &emits));
        assert_eq!(emits.load(Ordering::SeqCst), 1);
        let r = pull(&ses, MapInputs::default()).expect("the install is a change");
        assert_eq!(r.view, zoomed.view, "the view is kept");
        assert!(r.seq > zoomed.seq);
        let f = r.frame.unwrap();
        let st = f.stats;
        assert_eq!(
            (st.stations_total, st.stations_located, st.dots_outside),
            (68, 65, 3)
        );
        assert!(!f.dots.is_empty());
        assert_eq!(pull(&ses, MapInputs::default()), None);
        ses.0.lock().unwrap().select(Some(store()), "PT");
        let again = pull(&ses, MapInputs::default()).unwrap().frame.unwrap();
        assert_eq!((again.dots.len(), again.stats.stations_total), (0, 0));
    }

    /// A refresh landing for a country that is not the selected one gets no ticket (no gather, no
    /// `map:changed`), and nothing is framed; before any select nothing gets one. The code is
    /// matched trimmed and case-blind, as `select` takes it. Fails if `ticket_for` ignores the
    /// code (ES would regather PT).
    #[test]
    fn a_landed_list_for_another_country_gets_no_ticket() {
        let mut ses = Session::default();
        assert_eq!(ses.ticket_for("PT"), None, "nothing selected");
        ses.select(Some(store()), "pt");
        ses.pull_sync(Some(store()), band300(), MapInputs::default())
            .unwrap();
        assert_eq!(ses.ticket_for("ES"), None);
        assert_eq!(
            ses.pull_sync(Some(store()), band300(), MapInputs::default()),
            None
        );
        assert!(ses.ticket_for(" PT ").is_some());
        ses.select(Some(store()), "XX");
        assert_eq!(ses.ticket_for("XX"), None, "no map, no dots");
    }

    /// Amendment 5: PT's gather is held on a gate after its ticket was issued; US is selected;
    /// the gate opens and PT's result reaches `install`: nothing is installed and nothing emitted
    /// (the pull after it frames nothing), then US's own gather installs US's slice (169
    /// stations). Fails if `install` drops the re-check of the selection (PT's 68 land on US,
    /// a second emit).
    #[test]
    fn a_select_between_the_landed_and_the_apply_installs_nothing() {
        let ses: &'static MapSessionState = Box::leak(Box::default());
        ses.0.lock().unwrap().select(Some(store()), "PT");
        pull(ses, MapInputs::default()).unwrap();
        let t_pt = ses.0.lock().unwrap().ticket_for("PT").unwrap();
        let emits: &'static AtomicUsize = Box::leak(Box::default());
        let (gate, mut opened) = tauri::async_runtime::channel::<()>(1);
        let pt = slice("PT");
        let held = tauri::async_runtime::spawn(gather_and_install(
            map_state(),
            ses,
            t_pt,
            Cause::Landed,
            Instant::now(),
            async move {
                opened.recv().await;
                Ok(Some(pt))
            },
            || {
                emits.fetch_add(1, Ordering::SeqCst);
            },
        ));
        ses.0.lock().unwrap().select(Some(store()), "US");
        let us = pull(ses, MapInputs::default()).unwrap();
        block_on(gate.send(())).unwrap();
        assert!(!block_on(held).unwrap(), "PT's result is dropped");
        assert_eq!(emits.load(Ordering::SeqCst), 0);
        assert_eq!(pull(ses, MapInputs::default()), None, "nothing installed");
        let t_us = ses.0.lock().unwrap().ticket_for("US").unwrap();
        assert!(gather(ses, t_us, Some(slice("US")), emits));
        let f = pull(ses, MapInputs::default()).unwrap();
        assert_eq!(f.view, us.view);
        assert_eq!(f.frame.unwrap().stats.stations_total, 169);
        assert_eq!(emits.load(Ordering::SeqCst), 1);
    }

    /// `install` takes a ticket only for the current selection's generation (a re-select of the
    /// same country makes an older ticket stale) and only if no later ticket installed first (a
    /// slow read of an older list never replaces a newer one); no stored list installs nothing.
    /// Fails without the generation check or without the issue order.
    #[test]
    fn install_takes_the_current_selection_and_the_newest_ticket() {
        let g = |n: usize| Gathered {
            total: n,
            ..Gathered::default()
        };
        let mut ses = Session::default();
        ses.select(Some(store()), "PT");
        let (t1, t2) = (ses.ticket_for("PT").unwrap(), ses.ticket_for("PT").unwrap());
        assert!(ses.install(&t2, g(2)));
        assert!(!ses.install(&t1, g(1)), "older than the one installed");
        assert_eq!(ses.dots.total, 2);
        let t3 = ses.ticket_for("PT").unwrap();
        ses.select(Some(store()), "PT");
        assert!(!ses.install(&t3, g(3)), "from before the re-select");
        let shared = MapSessionState::default();
        shared.0.lock().unwrap().select(Some(store()), "PT");
        let t = shared.0.lock().unwrap().ticket_for("PT").unwrap();
        let emits = AtomicUsize::new(0);
        assert!(!gather(&shared, t, None, &emits));
        assert_eq!(emits.load(Ordering::SeqCst), 0);
    }

    fn dot(x: f32, r: f32, uuids: &[&str]) -> Dot {
        Dot {
            x,
            y: 50.0,
            r,
            n: u32::try_from(uuids.len()).unwrap(),
            uuids: uuids.iter().map(|u| u.to_string()).collect(),
            place: "Lisboa".into(),
        }
    }

    /// `hit` reads the newest frame's dots: `None` before any frame; within `r + 2` pt of a
    /// centre hits, at `r + 2.1` misses; of two in reach the nearest wins; the dot's uuids come
    /// back in its order; a frame with an older seq does not replace the dots; a `select` clears
    /// them. Fails with the slop at 2.2, the first in reach taken, the seq order ignored, or the
    /// select not clearing.
    #[test]
    fn a_click_hits_the_nearest_dot_within_r_plus_2() {
        let mut ses = Session::default();
        assert_eq!(ses.hit([100.0, 50.0]), None);
        ses.select(Some(store()), "PT");
        let seq = ses.seq().wrapping_add(1);
        // dot 0 reaches 8 pt from x 112, dot 1 5 pt from x 100: at x 104.9 both are in reach
        ses.framed(
            seq,
            &[dot(112.0, 6.0, &["c"]), dot(100.0, 3.0, &["b", "a"])],
        );
        let (i, h) = ses.hit([104.9, 50.0]).unwrap();
        assert_eq!(
            (i, h.uuids, h.n, h.place.as_str()),
            (1, vec!["b".to_string(), "a".to_string()], 2, "Lisboa"),
            "the nearest of two in reach, at r + 1.9 from it"
        );
        assert_eq!(
            ses.hit([94.9, 50.0]),
            None,
            "r + 2.1 from dot 1, far from dot 0"
        );
        ses.framed(seq.wrapping_sub(1), &[dot(94.9, 1.0, &["old"])]);
        assert_eq!(
            ses.hit([94.9, 50.0]),
            None,
            "an older frame does not replace them"
        );
        ses.select(Some(store()), "ES");
        assert_eq!(ses.hit([100.0, 50.0]), None, "a select clears them");
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
