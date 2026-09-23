//! The popover: a non-activating `NSPanel` anchored under the tray icon, dismissed when it
//! resigns key.
//!
//! Two traps in `tauri-nspanel` shape this file (verified against rev c9ec213, ONDAR.md):
//! - `Panel::to_window()` is a *conversion back*: it removes the panel from the plugin store,
//!   clears the delegate and resets the NSWindow's class to `TaoWindow`. It is never called
//!   here. The Tauri window is reached through `get_webview_window`, which touches neither.
//! - `PanelBuilder::no_activate(true)` does not make the panel non-activating; it only swaps
//!   the activation policy around window creation. That is the style mask's job.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{
    ActivationPolicy, App, AppHandle, Emitter, LogicalSize, Manager, Position, Rect, Runtime, Size,
    WebviewUrl, WebviewWindow, WindowEvent,
    window::{Effect, EffectState, EffectsBuilder},
};
use tauri_nspanel::{
    ManagerExt,
    PanelBuilder,
    PanelHandle,
    PanelLevel,
    // `doc(hidden)` re-exports; used instead of a direct `objc2-app-kit` dependency so the
    // version can never diverge from the one `tauri-nspanel` links.
    objc2_app_kit::{NSScreen, NSWindowOcclusionState, NSWindowStyleMask},
    // Qualified, not imported by name: `tauri_panel!` expands `use`s of these same names into
    // this module, and a second import is a hard error (E0252).
    objc2_foundation as foundation,
    tauri_panel,
};
use ts_rs::TS;

/// Label of the popover window — the only window, and the one `capabilities/default.json` is
/// scoped to since the M1 bench window retired (M2c). The page invokes, so the capability is a
/// real permission decision: `core:default` (events, app name/version) and nothing else.
/// App-defined commands need no ACL entry from a local page (tauri 2.11.5
/// `webview/mod.rs:1823`: the ACL gates plugin commands, a declared app manifest, and remote
/// origins).
const PANEL_LABEL: &str = "panel";

/// Width of the popover in points — one width for both height states (ONDAR.md product shape).
const PANEL_WIDTH: f64 = 360.0;

/// The collapsed height in points (ONDAR.md product shape, ~360×420). Also decision D1's
/// **provisional** floor: expansion is refused when the capped expanded height would not exceed
/// this. The real floor is M4's, derived from the map's minimum legible pane; M2d must not invent
/// one (ONDAR.md, "M2d: the expanded height is capped to the work area").
const COLLAPSED_HEIGHT: f64 = 420.0;

/// The expanded height in points **before** the D1 cap (ONDAR.md product shape, ~360×720). The
/// height the panel actually gets is `min(this, what fits under the icon)` — 598 pt measured on
/// the ANMITE while it hosted the menu bar (M2d Step 0 P4) — so "expanded" is a function of the
/// display, not a constant, and the page is told the height rather than computing it.
const EXPANDED_HEIGHT_NOMINAL: f64 = 720.0;

/// Corner radius of the popover, in **points**: Control Center's, measured on macOS 26.6.2
/// (25G83) on 2026-09-17 — 7.6 pt by a calibrated threshold fit and 8.3 pt by a differential
/// match, the spread being backdrop-contrast dependent (M2c Step 0, R9). Re-measure if the OS
/// major version changes; system radii move between releases.
///
/// The same number is `--radius-panel` in `src/styles/tokens.css`, and
/// `tokens_css_panel_radius_matches_the_effects_radius` fails if the two ever disagree.
///
/// Applied through `EffectsBuilder::radius`, which reaches window-vibrancy 0.6.0's
/// `setCornerRadius:` on the effect view — a selector that crate's own source calls possibly
/// private ("not listed in Apple documentation, might be private, but it works",
/// `ns_visual_effect_view_tagged.rs:92-99`). Ondar does not call it; Tauri does. Recorded in
/// ONDAR.md as a dependency risk: a Tauri or window-vibrancy bump could drop it. Measured
/// 2026-09-17: it rounds the material, and the window shadow follows the corners (R8).
pub const PANEL_CORNER_RADIUS: f64 = 8.0;

/// Logical **points** between the bottom edge of the tray icon and the top edge of the panel.
///
/// Points, not physical pixels. As pixels it was two different gaps: measured 2026-09-16, the
/// panel sat 3 pt below the menu bar on the 2x built-in and 6 pt below it on the 1x BenQ, from the
/// same constant. A visual spacing has to be in the unit the eye works in.
///
/// 6 pt is the gap the 1x display has been showing, and it is in the range macOS's own status item
/// menus leave; 3 pt reads as touching the menu bar. That part is a judgement, not a measurement.
const TRAY_GAP: f64 = 6.0;

/// Logical points kept between the panel and the left and right edges of a display's work area
/// when the panel has to be pushed back on screen. Equal to `TRAY_GAP` so the gaps read as one
/// spacing — an aesthetic choice, not a measured one.
const EDGE_MARGIN: f64 = TRAY_GAP;

/// How long after `show` the occlusion state is read.
///
/// A synchronous read is stale: straight after `orderFrontRegardless` + `makeKeyWindow` the
/// `Visible` bit was clear (raw 8192) in 14 of 14 probe runs, and in the 13 of them without
/// `hides_on_deactivate` it was set at the first later sample. Sampled every 10 ms across 6 of
/// those runs (2026-09-15), the bit went from clear to set between samples bracketing
/// (12, 24], (12, 27], (14, 30], (15, 23], (21, 34] and (23, 35] ms, so the latency never
/// exceeded 35 ms. 100 ms is ~2.9x that worst upper bound.
/// The log line prints the elapsed time actually observed, so a late read is visible as such.
const OCCLUSION_SETTLE: Duration = Duration::from_millis(100);

/// How long a layout waits for the page's commit before Rust completes it anyway (decision D3's
/// fallback timer). **Picked, provisional** — the reason, and what replaces it:
///
/// It must never fire on a healthy page (firing re-introduces the one-frame artefact the round
/// trip exists to remove, and moves a show), and it must be short enough that a dead or frozen page
/// still gets its popover before a click feels ignored. Nothing has yet measured the path it
/// bounds — Tauri event → React commit → `panel_layout_committed` — so the nearest measured
/// analogue is used: the page's first `resize` report **after a show**, worst case 111 ms (M2d
/// Step 0 P3, `run-10-p3.log:48→51`). 250 ms is 2.25× that, the same "two-to-three times the worst
/// observed" rule `OCCLUSION_SETTLE` uses. Every completion logs `after_ms`.
///
/// **Measured at acceptance, 2026-09-21** (`m2d-acc-02`, `m2d-acc-03`, bundled build `57bc490`):
/// visible resizes 1–4 ms, n = 32; hidden shows 2–13 ms and one 100 ms, n = 8. 250 ms is 2.5× that
/// maximum, so the value stands — **still provisional**, because n = 8 is short of the ≥ 20 hidden
/// shows the plan asked for; a later run with that n rewrites this comment, and the constant only
/// if the rule (≥ 2× the observed maximum, rounded to a frame) then says so. No fallback fired on a
/// healthy page in either run. **A `trigger=fallback` on a healthy page is a defect, not a tuning
/// knob.**
const LAYOUT_FALLBACK: Duration = Duration::from_millis(250);

tauri_panel! {
    panel!(OndarPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
}

/// The popover's page. A debug build may carry the measurement harness's query
/// (`measure.rs`, `ONDAR_MEASURE`); a release build has no other URL than the plain one.
fn panel_url() -> WebviewUrl {
    #[cfg(debug_assertions)]
    if let Some(url) = crate::measure::panel_url() {
        return url;
    }
    WebviewUrl::App("panel.html".into())
}

/// Build the popover, hidden. Call from `setup`, before `tray::setup`.
pub fn setup(app: &mut App) -> tauri::Result<()> {
    // Must precede `PanelBuilder::build()`. With `no_activate(true)` the builder forces
    // `Prohibited` around window creation and afterwards restores whatever the policy was
    // *before* — so setting Accessory later would be undone and the Dock icon would return.
    app.set_activation_policy(ActivationPolicy::Accessory);
    app.manage(PanelState::default());

    let panel = PanelBuilder::<_, OndarPanel<_>>::new(app.handle(), PANEL_LABEL)
        .url(panel_url())
        .size(Size::Logical(LogicalSize::new(
            PANEL_WIDTH,
            COLLAPSED_HEIGHT,
        )))
        .level(PanelLevel::PopUpMenu)
        .floating(true)
        // Keeps window *creation* from activating the app. Not what makes it non-activating.
        .no_activate(true)
        // No `hides_on_deactivate`: the app is never active, so it is permanently satisfied and
        // the panel never composites. Dismissal is explicit, on resign-key.
        .has_shadow(true)
        // Two transparencies, both needed. This one is the NSWindow (`clearColor`, not opaque)…
        .transparent(true)
        // …and this one the WKWebView, whose opacity wry decides at creation; the window-level
        // call cannot reach it retroactively.
        .with_window(|w| w.decorations(false).resizable(false).transparent(true))
        .build()?;

    // `build()` leaves the window ordered in.
    panel.hide();

    // Non-activating: clicks and `makeKeyWindow` make the panel key without activating the app
    // or taking focus from the frontmost one. ORed onto the mask tao chose rather than replacing
    // it (`PanelBuilder::style_mask` replaces), so tao's own mask recomputations in
    // `set_resizable`/`set_minimizable` start from what tao expects. Both values are logged.
    let ns = panel.as_panel();
    let before = ns.styleMask();
    panel.set_style_mask(before | NSWindowStyleMask::NonactivatingPanel);
    let after = ns.styleMask();
    log::info!(
        "panel style_mask before={:#x} after={:#x} nonactivating={}",
        before.0,
        after.0,
        after.contains(NSWindowStyleMask::NonactivatingPanel)
    );

    let window = app
        .get_webview_window(PANEL_LABEL)
        .ok_or(tauri::Error::WindowNotFound)?;

    // Applied after the style mask, so a frame view rebuilt by `setStyleMask:` cannot strand
    // the effect view. `apply_effects` returns silently if it finds no macOS effect, so this
    // `?` proves nothing; the view tree does (ONDAR.md, "The M2 spike").
    window.set_effects(
        EffectsBuilder::new()
            .effect(Effect::Popover)
            .radius(PANEL_CORNER_RADIUS)
            // `Active`, not `FollowsWindowActiveState`. Whether AppKit draws a key
            // non-activating panel in an inactive app as "active" was never measured, and the
            // popover should look active whenever it is on screen either way.
            .state(EffectState::Active)
            .build(),
    )?;

    // Dismissal. tao's own window delegate turns `windowDidResignKey:` into
    // `WindowEvent::Focused(false)`, so this is the resign-key hook without replacing that
    // delegate. `Panel::set_event_handler` would replace it — it keeps the original only to
    // restore it when the handler is set back to `None`, and forwards nothing while installed —
    // silencing tao's Resized/Moved/Focused/ScaleFactorChanged for this window. See ONDAR.md.
    let on_resign = app.handle().clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Focused(false) = event {
            // The measurement harness (debug builds only) keeps the popover up so a measurement
            // can be watched from Terminal or the Web Inspector; logged, never silent.
            #[cfg(debug_assertions)]
            if crate::measure::keep_open() {
                log::info!("panel hide reason=resign_key SUPPRESSED measure_keep_open=1");
                return;
            }
            hide(&on_resign, HideReason::ResignKey);
        }
    });

    Ok(())
}

/// Why the popover is being hidden. Logged on every hide, so the log can show that one close is
/// one *effective* hide whatever else fires: every close measured so far fires two hides — the
/// toggle's, then the resign-key one 2.2–3.7 ms later (2026-09-15, re-measured 2026-09-16) — and
/// that stays harmless only while the log can show it is a double.
///
/// Variants are added with their callers, because under `-D warnings` an unused variant is a
/// `dead_code` error rather than a placeholder.
#[derive(Clone, Copy, Debug)]
pub enum HideReason {
    /// The tray icon was clicked while the popover was showing.
    Toggle,
    /// The panel resigned key (`WindowEvent::Focused(false)`): a click elsewhere, or being
    /// ordered out by another hide.
    ResignKey,
    /// The page reported an Escape `keydown` (`commands::panel::panel_escape`). Escape reaches
    /// the webview's JS in both phases — before and after a click inside the panel — with the
    /// WKWebView first responder from the moment the panel is shown (M2c Step 0, item 1).
    Esc,
    /// The tray icon was right-clicked, so the menu is about to open. Decided 2026-09-16: a
    /// menu over a live popover is not wanted, and opening the menu does not resign the panel's
    /// key status (Step 0, item 2), so nothing else would hide it. Keyed off `Click{Right, Down}`
    /// — the only right-click event that arrives — and measured to land visually before the
    /// menu (R5: 5.6 ms on main).
    Menu,
}

impl HideReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::ResignKey => "resign_key",
            Self::Esc => "esc",
            Self::Menu => "menu",
        }
    }
}

/// Which pane the popover shows. Crosses the boundary as the `view` field of [`PanelLayout`] —
/// the `panel:layout` event and the `get_panel_layout` answer (M2c's `panel:view` and
/// `get_panel_view` until M2d) — so it is a generated type, not a string agreed on by hand on both
/// sides (`/code-review` finding 7, 2026-09-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum PanelView {
    /// The dev transport (M2c) — the M3 country/station UI later.
    Transport,
    /// The About pane, from the tray menu.
    About,
}

/// Which of the two heights the popover is at, or is asked for. Crosses the boundary inside
/// [`PanelLayout`], so it is a generated type, not a string agreed on by hand on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum PanelHeight {
    Collapsed,
    Expanded,
}

/// What the page renders: the pane, the height state, the size in points, and whether expansion
/// is available on the display the icon is on (decision D1's floor; decision D4 shows the control
/// disabled when it is not). Emitted as `panel:layout` on every effective show and every resize,
/// and answered by `get_panel_layout` on mount. **The page is told the height; it never computes
/// it** (D1's consequence; CLAUDE.md, "The one rule"). Supersedes M2c's `panel:view` event and
/// `get_panel_view`, whose value is the `view` field.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PanelLayout {
    /// Whether this layout arrives on a show — the hidden panel's frame is **already** at
    /// `width`×`height` when the page receives it — or on a resize of the visible panel, whose
    /// frame changes only after the page commits. The page needs the difference: a hidden
    /// WKWebView fires no `resize`, so on a show it takes `height` as the window's height rather
    /// than its last `resize` reading (`/code-review` C2, 2026-09-18: a show shorter than the
    /// last visible height committed with the expanded pane in a collapsed popover's first frame).
    pub transition: PanelTransition,
    /// Increments on every layout request. The page echoes it in `panel_layout_committed`, so a
    /// report for a layout that has since been superseded, completed or cancelled is a logged
    /// no-op rather than a second apply (decision D3). `u32`, not `u64`: ts-rs maps `u64` to
    /// `bigint`, and a click counter does not need it.
    pub generation: u32,
    pub view: PanelView,
    pub state: PanelHeight,
    pub width: f64,
    pub height: f64,
    pub expandable: bool,
}

/// How a [`PanelLayout`] reaches the page — see its `transition` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum PanelTransition {
    /// A show: the frame is already applied, the panel is about to be ordered in.
    Show,
    /// A resize of the visible panel: the frame changes on the page's commit.
    Resize,
}

/// The popover's layout state, held in Tauri state so the page can ask for it on mount: an emit
/// with no JS listener registered yet is dropped by Tauri with `Ok(())`, so without a getter a
/// show before `listen` resolved — or after a webview reload — rendered the wrong pane while the
/// log claimed the right one (`/code-review` finding 3, 2026-09-17). Mirrors `get_playback_state`
/// for the engine.
///
/// `expanded` is the height the user last chose and **persists across hides** within a run
/// (decided 2026-09-18): a hide has no side effect, and the next show lays the panel out for the
/// display the icon is then on — an expanded panel hidden on the ANMITE and reopened on the
/// built-in gets 720 pt, not 598. Where the chosen height no longer fits, the show collapses it
/// and records that.
pub struct PanelState {
    inner: Mutex<Inner>,
}

struct Inner {
    /// The layout last emitted.
    last: PanelLayout,
    /// The height the user last chose with the control — set by a successful resize, and by
    /// nothing else. Not always `last.state`: the About pane shows at the collapsed height
    /// whatever the choice, and the choice survives it so Back restores it (decided 2026-09-21,
    /// acceptance defect: the control showed on About; it does not, and About does not resize).
    chosen: PanelHeight,
    round_trip: RoundTrip,
}

/// The height a show lays out for: the About pane always at the collapsed height, the transport
/// at the user's choice. Pure, so the decision has a test.
fn show_height(view: PanelView, chosen: PanelHeight) -> PanelHeight {
    match view {
        PanelView::About => PanelHeight::Collapsed,
        PanelView::Transport => chosen,
    }
}

/// What a layout request is waiting to do once the page has committed (or the fallback fires).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutKind {
    /// Order the hidden panel in and make it key. The frame was set at request time.
    Show(ShowReason),
    /// Change the visible panel's frame.
    Resize,
}

/// A layout the page has been told about and Rust has not yet completed.
#[derive(Debug, Clone)]
struct Pending {
    generation: u32,
    kind: LayoutKind,
    /// Top-left, points.
    position: (f64, f64),
    /// Points.
    size: (f64, f64),
    /// The tray rect the layout was made against, for the placement log line.
    rect: Rect,
    requested_at: Instant,
}

/// Decision D3's bookkeeping, pure so the four transitions are unit-tested without AppKit:
/// a request supersedes any pending one; a completion applies only the generation that is
/// pending, once; a hide cancels. The caller does the AppKit work on what `complete` returns.
#[derive(Debug, Default)]
struct RoundTrip {
    generation: u32,
    pending: Option<Pending>,
}

impl RoundTrip {
    /// Register a layout the page is about to be told about. Returns its generation. Any pending
    /// layout is superseded: its later commit becomes a no-op.
    fn request(
        &mut self,
        kind: LayoutKind,
        position: (f64, f64),
        size: (f64, f64),
        rect: Rect,
    ) -> u32 {
        self.generation += 1;
        self.pending = Some(Pending {
            generation: self.generation,
            kind,
            position,
            size,
            rect,
            requested_at: Instant::now(),
        });
        self.generation
    }

    /// The page committed `generation`, or its fallback fired. `Some` exactly when that generation
    /// is the one pending — and then it no longer is, so the second of a commit and its fallback
    /// gets `None`.
    fn complete(&mut self, generation: u32) -> Option<Pending> {
        match &self.pending {
            Some(p) if p.generation == generation => self.pending.take(),
            _ => None,
        }
    }

    /// A hide: whatever was pending must not complete — a commit arriving after a hide would
    /// otherwise order the panel back in. Returns whether anything was cancelled.
    fn cancel(&mut self) -> bool {
        self.pending.take().is_some()
    }
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                // Before the first show nothing has been laid out; the first show replaces this.
                last: PanelLayout {
                    transition: PanelTransition::Show,
                    generation: 0,
                    view: PanelView::Transport,
                    state: PanelHeight::Collapsed,
                    width: PANEL_WIDTH,
                    height: COLLAPSED_HEIGHT,
                    expandable: true,
                },
                chosen: PanelHeight::Collapsed,
                round_trip: RoundTrip::default(),
            }),
        }
    }
}

impl PanelState {
    /// The layout last emitted. `Mutex::lock().unwrap()`: poison propagation only (CLAUDE.md's
    /// exemption), here and below.
    pub fn layout(&self) -> PanelLayout {
        self.inner.lock().unwrap().last
    }

    /// The height a show of `view` lays out for (`show_height`).
    fn wanted_height_for(&self, view: PanelView) -> PanelHeight {
        show_height(view, self.inner.lock().unwrap().chosen)
    }

    /// The height the user last chose with the control.
    fn chosen(&self) -> PanelHeight {
        self.inner.lock().unwrap().chosen
    }

    /// Why a resize request is refused before any layout is computed: `Some("view")` on the About
    /// pane, where there is no control and no resize (the page hides the button; this is the
    /// guard that does not depend on it).
    fn resize_refusal(&self) -> Option<&'static str> {
        match self.inner.lock().unwrap().last.view {
            PanelView::About => Some("view"),
            PanelView::Transport => None,
        }
    }

    /// Register a layout with the round trip and record it as the last one; returns what the page
    /// is told, generation included.
    fn request(
        &self,
        view: PanelView,
        layout: &Layout,
        kind: LayoutKind,
        rect: Rect,
    ) -> PanelLayout {
        let mut inner = self.inner.lock().unwrap();
        let generation =
            inner
                .round_trip
                .request(kind, layout.anchored.position, layout.size, rect);
        if kind == LayoutKind::Resize {
            inner.chosen = layout.state;
        }
        inner.last = PanelLayout {
            transition: match kind {
                LayoutKind::Show(_) => PanelTransition::Show,
                LayoutKind::Resize => PanelTransition::Resize,
            },
            generation,
            view,
            state: layout.state,
            width: layout.size.0,
            height: layout.size.1,
            expandable: layout.expandable,
        };
        inner.last
    }

    /// The page left the About pane through its Back button. Recorded so a later layout — a
    /// resize re-emits the pane along with the height — carries the pane the page is showing,
    /// not the one the last show landed on (`/code-review` C1, 2026-09-18: Expand after Back
    /// threw the user back to About).
    /// A refused expansion: the layout was computed against the display as it is now and says
    /// `expandable=false`, but the page's control is driven by the flag of the last *emitted*
    /// layout — enabled, if the work area shrank below decision D1's floor while the popover was
    /// up (Dock reveal, arrangement change). Refresh the flag on the last layout without a new
    /// generation — nothing is pending, no round trip — and return it for emitting, so the control
    /// disables (decision D4) instead of clicking into silent no-ops (`/code-review` C5).
    fn refresh_expandable(&self, expandable: bool) -> PanelLayout {
        let mut inner = self.inner.lock().unwrap();
        inner.last.expandable = expandable;
        inner.last
    }

    fn set_view(&self, view: PanelView) -> PanelView {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.last.view;
        inner.last.view = view;
        before
    }

    fn complete(&self, generation: u32) -> Option<Pending> {
        self.inner.lock().unwrap().round_trip.complete(generation)
    }

    /// Whether `generation` is the one still pending. Read off the main thread by the fallback,
    /// so a fallback with nothing to do never hops to main.
    fn is_pending(&self, generation: u32) -> bool {
        self.inner
            .lock()
            .unwrap()
            .round_trip
            .pending
            .as_ref()
            .is_some_and(|p| p.generation == generation)
    }

    fn cancel_pending(&self) -> bool {
        self.inner.lock().unwrap().round_trip.cancel()
    }

    /// A show is requested and not yet ordered in: the frame is set, the page has been told, the
    /// commit or the fallback has not arrived. `is_visible()` is false throughout, which is why
    /// `toggle` asks this too (`/code-review` C3).
    fn show_pending(&self) -> bool {
        matches!(
            self.inner.lock().unwrap().round_trip.pending,
            Some(Pending {
                kind: LayoutKind::Show(_),
                ..
            })
        )
    }
}

/// Why the popover is being shown. Logged like [`HideReason`], and **the quantity the page's
/// view is derived from**: every effective show stores and emits [`ShowReason::view`], so which
/// pane is showing is decided here and only mirrored by the webview (decided 2026-09-17; an
/// earlier draft let the About pane survive a hide, which put the next tray click on About).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowReason {
    /// The tray icon was clicked while the popover was hidden.
    Toggle,
    /// The tray menu's About item. Decided 2026-09-16: About lives inside the popover, not in
    /// the standard About panel, which an `Accessory` app opens at `NSNormalWindowLevel` behind
    /// the frontmost app (Step 0, item 3).
    About,
    /// `RunEvent::Reopen`: `open Ondar.app` or a Finder double-click against a running app.
    /// LaunchServices starts no second process for those, so the single-instance plugin cannot
    /// see them; they arrive as `applicationShouldHandleReopen:` (Step 0, item 5, cases (a)
    /// and (d) — the latter fires twice, the second show is the logged no-op).
    Reopen,
    /// The single-instance plugin's callback: a real second process started (`open -n`, the
    /// inner binary, a copy of the bundle at another path) and handed off to this one.
    SecondInstance,
}

impl ShowReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::About => "about",
            Self::Reopen => "reopen",
            Self::SecondInstance => "second_instance",
        }
    }

    /// The pane this show lands on: About for [`Self::About`], the transport for everything else.
    fn view(self) -> PanelView {
        match self {
            Self::About => PanelView::About,
            Self::Toggle | Self::Reopen | Self::SecondInstance => PanelView::Transport,
        }
    }
}

/// **The one hide path.** Every close goes through here — the tray toggle, resign-key, Esc, the
/// menu — so that a hide's side effect (today: cancelling a pending layout, below) runs once per
/// close, not once per caller. The height state is *not* a side effect of hiding: it persists
/// across hides (decided 2026-09-18; an older version of this comment predicted otherwise).
///
/// Always executed on the main thread. `PanelHandle` is `Send`, but its methods are bare
/// `msg_send!` with no dispatch of their own (tauri-nspanel c9ec213 `panel.rs:14-17`: "all actual
/// panel operations must be performed on the main thread"). `run_on_main_thread` runs the closure
/// inline when already on main (tauri-runtime-wry 2.11.4 `lib.rs:235-255`) and posts it otherwise,
/// so every caller gets the same path, and the log line records which thread it ran on.
///
/// Hiding an already-hidden panel is a logged no-op — `effective=false` — never silence. It still
/// **cancels a pending show** (D3): the panel is not visible during the round trip, so every hide
/// that arrives then is `effective=false`, and each has a reason to end the show anyway — a tray
/// click (`toggle`, C3), a right-click opening the menu, a resign-key that can only mean the panel
/// was ordered in and out again. A no-op hide has no other side effect (`/code-review` C6:
/// unverified, decided here — the cancel is the intended behaviour, not an accident of ordering).
pub fn hide<R: Runtime>(handle: &AppHandle<R>, reason: HideReason) {
    let on_main = handle.clone();
    let queued = handle.run_on_main_thread(move || {
        let Some(panel) = panel_handle(&on_main, "hide") else {
            return;
        };
        let effective = panel.is_visible();
        // A layout still waiting for the page's commit must not complete after this: for a
        // pending show it would order the panel back in (decision D3).
        let pending_cancelled = on_main.state::<PanelState>().cancel_pending();
        // `orderOut:` on a window that is already out is itself a no-op, so this is called
        // unconditionally: the log line is what distinguishes the two cases, not a branch.
        panel.hide();
        log::info!(
            "panel hide reason={} effective={effective} pending_cancelled={pending_cancelled} \
             thread={:?}",
            reason.as_str(),
            std::thread::current().name()
        );
    });
    if let Err(e) = queued {
        log::warn!("panel hide reason={} not queued: {e}", reason.as_str());
    }
}

/// **The one show path.** Anchors the panel under `rect` (a tray rect in the units
/// `TrayIconEvent::Click` uses), orders it in and makes it key. Showing an already-visible panel
/// is a logged no-op: a second request while the popover is up leaves it up (decided 2026-09-17)
/// rather than toggling it away.
///
/// Main-thread discipline and logging as in [`hide`]. Failures are logged here rather than
/// returned, so a caller on any thread can fire and forget.
pub fn show_at<R: Runtime>(handle: &AppHandle<R>, rect: Rect, reason: ShowReason) {
    let on_main = handle.clone();
    let queued = handle.run_on_main_thread(move || {
        if let Err(e) = show_on_main(&on_main, rect, reason) {
            log::warn!("panel show reason={} failed: {e}", reason.as_str());
        }
    });
    if let Err(e) = queued {
        log::warn!("panel show reason={} not queued: {e}", reason.as_str());
    }
}

fn show_on_main<R: Runtime>(
    handle: &AppHandle<R>,
    rect: Rect,
    reason: ShowReason,
) -> tauri::Result<()> {
    let Some((panel, window)) = panel_and_window(handle, "show") else {
        return Err(tauri::Error::WindowNotFound);
    };

    if panel.is_visible() {
        log::info!(
            "panel show reason={} effective=false thread={:?}",
            reason.as_str(),
            std::thread::current().name()
        );
        return Ok(());
    }

    // The view changes only on an effective show. Emitting on the no-op too read as "a second
    // request while the popover is up lands the pane it asked for", but no mouse route reaches
    // About with the popover visible (right-`Down` hides first), and the only live effect was
    // the reverse: a re-launch while About was up kicked the user back to the transport with no
    // gesture on the popover (`/code-review` finding 6, 2026-09-17).
    let view = reason.view();
    let state = handle.state::<PanelState>();
    let laid = layout_for(&window, rect, state.wanted_height_for(view))?;
    let emitted = state.request(view, &laid, LayoutKind::Show(reason), rect);
    // Origin and size in one call while the panel is still hidden, so the hidden-window
    // `setContentSize:` trap (bottom-left kept — M2d Step 0, measured, unexplained) has nothing to
    // act on: the frame is set whole, never a size against a remembered origin. Before the emit,
    // so `transition=show` is literally true when the page reads it (`/code-review` V2).
    apply_frame(&panel, laid.anchored.position, laid.size);
    emit_layout(handle, emitted, reason.as_str());
    // Ordering in waits for the page's commit (decision D3): with it before the commit, the
    // retained WKWebView layer was composited once with the previous pane (M2c review finding 8,
    // measured 2026-09-18). `complete_layout` orders in on the commit or on the fallback.
    arm_fallback(handle, emitted.generation);
    log::info!(
        "panel layout pending generation={} kind=show reason={}",
        emitted.generation,
        reason.as_str()
    );
    Ok(())
}

/// The page committed the DOM for `generation` (`commands::panel::panel_layout_committed`), or
/// its fallback fired. Completes the visible change for that layout if — and only if — it is the
/// one still pending; anything else is a logged no-op. Main thread, as everything here.
///
/// "Commit" is React's DOM commit, not a paint: a hidden WKWebView runs no rendering updates
/// (M2d Step 0 P3 — no `resize` report until shown, and `requestAnimationFrame` never fires), so
/// the page reports from an effect. For a show that is sufficient — the first composite after
/// `orderFrontRegardless` lays out and paints what is committed. For a visible resize it puts the
/// state-dependent content in place before the frame changes; whether the newly exposed band is
/// painted in the same frame depends on WebKit having rasterised the page's pre-laid-out overflow,
/// which is measured at acceptance, not assumed.
pub fn layout_committed<R: Runtime>(handle: &AppHandle<R>, generation: u32) {
    let on_main = handle.clone();
    let queued = handle.run_on_main_thread(move || complete_layout(&on_main, generation, "commit"));
    if let Err(e) = queued {
        log::warn!("panel layout commit generation={generation} not queued: {e}");
    }
}

fn complete_layout<R: Runtime>(handle: &AppHandle<R>, generation: u32, trigger: &str) {
    let Some(pending) = handle.state::<PanelState>().complete(generation) else {
        log::info!(
            "panel layout complete generation={generation} trigger={trigger} effective=false"
        );
        return;
    };
    let Some(panel) = panel_handle(handle, "layout complete") else {
        return;
    };
    let after_ms = pending.requested_at.elapsed().as_millis();
    match pending.kind {
        LayoutKind::Show(reason) => {
            panel.show();
            // `show()` is `orderFrontRegardless` alone, and a panel that is never key can never
            // resign key. Not `show_and_make_key()`: that also makes the content view
            // (`WryWebViewParent`) first responder, taking it from the WKWebView — measured
            // 2026-09-16: after plain `make_key_window()` the first responder is the `WryWebView`
            // at t+0 in every run.
            panel.make_key_window();
            let ns = panel.as_panel();
            // A second `invalidateShadow` after ordering in — `apply_frame` made one while hidden.
            // Same insurance as there (P2 unfalsified); the shadow a hidden window has is moot.
            ns.invalidateShadow();
            log::info!(
                "panel show reason={} effective=true generation={generation} trigger={trigger} \
                 after_ms={after_ms} class={} key={} thread={:?}",
                reason.as_str(),
                ns.class().name().to_string_lossy(),
                ns.isKeyWindow(),
                std::thread::current().name()
            );
            log_settled_occlusion(handle, &panel);
        }
        LayoutKind::Resize => {
            apply_frame(&panel, pending.position, pending.size);
            log::info!(
                "panel resize effective=true generation={generation} trigger={trigger} \
                 after_ms={after_ms} size_points={:?} thread={:?}",
                pending.size,
                std::thread::current().name()
            );
        }
    }
    log_tray_screen_check(&panel, pending.rect);
}

/// Decision D3's fallback: after `LAYOUT_FALLBACK`, complete `generation` if the page has not.
/// The pending check runs off the main thread first (`PanelState` is a `Mutex`), so on a healthy
/// page — the commit landed 2–8 ms in — nothing is hopped to main and nothing is logged at INFO;
/// only a fallback that will actually complete something reaches `complete_layout`.
fn arm_fallback<R: Runtime>(handle: &AppHandle<R>, generation: u32) {
    let what = format!("layout fallback generation={generation}");
    after_delay_on_main(
        handle,
        "ondar-layout-fallback",
        LAYOUT_FALLBACK,
        what,
        move |handle| {
            if !handle.state::<PanelState>().is_pending(generation) {
                log::debug!(
                    "panel layout fallback generation={generation} not pending; nothing to do"
                );
                return None;
            }
            Some(move |handle: &AppHandle<R>| complete_layout(handle, generation, "fallback"))
        },
    );
}

/// Sleep `delay` on a named thread, then run `decide` there; if it returns a closure, run that on
/// the main thread. The shape `log_settled_occlusion` and `arm_fallback` share: one thread per
/// event (a show or a click, not a stream), both failures logged with `what`, never panicking.
fn after_delay_on_main<R, D, F>(
    handle: &AppHandle<R>,
    thread_name: &str,
    delay: Duration,
    what: String,
    decide: D,
) where
    R: Runtime,
    D: FnOnce(&AppHandle<R>) -> Option<F> + Send + 'static,
    F: FnOnce(&AppHandle<R>) + Send + 'static,
{
    let handle = handle.clone();
    let what_for_thread = what.clone();
    let spawned = std::thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            std::thread::sleep(delay);
            let Some(on_main) = decide(&handle) else {
                return;
            };
            let for_main = handle.clone();
            if let Err(e) = handle.run_on_main_thread(move || on_main(&for_main)) {
                log::warn!("panel {what_for_thread} not queued: {e}");
            }
        });
    if let Err(e) = spawned {
        log::warn!("panel {what} thread not started: {e}");
    }
}

/// Set the panel's frame — origin **and** size — in one synchronous `setFrame:display:` (M2d
/// route S, decision D2), then invalidate the shadow. Main thread only; off it the call is
/// logged and skipped, never attempted.
///
/// Route S over Tauri's `set_size` + `set_position`: those are two *asynchronous* main-queue
/// dispatches (tao `util/async.rs`), measured at M2d Step 0 with a 2–5 ms window in which the frame
/// has changed and the position has not, or vice versa; `setFrame:display:` lands both in
/// 0.6–1.7 ms with tao's `Resized` delivered inside the call. Both routes then show the same one
/// 60 Hz frame of unpainted material (P1b) — the round trip (D3) addresses that, not the route.
///
/// `invalidateShadow()` is **insurance, not a fix**: P2 measured the shadow following the frame
/// change without it, and the control could not manufacture a stale shadow, so the result is
/// unfalsified. 58–107 µs. See ONDAR.md, "M2d: resize in place".
fn apply_frame<R: Runtime>(panel: &PanelHandle<R>, position: (f64, f64), size: (f64, f64)) {
    let Some(mtm) = foundation::MainThreadMarker::new() else {
        log::warn!(
            "panel apply_frame position={position:?} size={size:?} skipped: not on the main thread"
        );
        return;
    };
    let Some(h0) = main_screen_height(mtm) else {
        log::warn!("panel apply_frame position={position:?} size={size:?} skipped: no NSScreen");
        return;
    };
    let (x, y, w, h) = cocoa_frame(position, size, h0);
    let ns = panel.as_panel();
    ns.setFrame_display(
        foundation::NSRect::new(
            foundation::NSPoint::new(x, y),
            foundation::NSSize::new(w, h),
        ),
        true,
    );
    ns.invalidateShadow();
    let now = ns.frame();
    log::info!(
        "panel apply_frame top_left_points={position:?} size_points={size:?} \
         cocoa=[{x},{y} {w}x{h}] frame_now=[{},{} {}x{}] thread={:?}",
        now.origin.x,
        now.origin.y,
        now.size.width,
        now.size.height,
        std::thread::current().name()
    );
}

/// Height of `NSScreen::screens()[0]`'s frame — the menu-bar display, and the quantity every
/// bottom-left ↔ top-left conversion is made against (tao's `bottom_left_to_top_left` uses the
/// same screen). Not `mainScreen`, which is the key window's screen (ONDAR.md, instrument five).
fn main_screen_height(mtm: foundation::MainThreadMarker) -> Option<f64> {
    NSScreen::screens(mtm)
        .iter()
        .next()
        .map(|s| s.frame().size.height)
}

/// A top-left-origin point rect (this module's space) as a Cocoa frame `(x, y, w, h)`, bottom-left
/// origin, given the menu-bar screen's height `h0`: `y_cocoa = h0 − y_tl − h`. Pure.
///
/// Pinned by measurement: the panel anchored at (758,39) 360×720 on the 982 pt built-in had
/// Cocoa frame `[758,223 360x720]` (M2d Step 0, `run-11-p3-keepopen.log:22`).
fn cocoa_frame(top_left: (f64, f64), size: (f64, f64), h0: f64) -> (f64, f64, f64, f64) {
    (top_left.0, h0 - top_left.1 - size.1, size.0, size.1)
}

/// A Cocoa frame as this module's top-left `PointRect`, given the menu-bar screen's height —
/// `cocoa_frame`'s inverse.
fn top_left_rect(frame: foundation::NSRect, h0: f64) -> PointRect {
    PointRect::new(
        frame.origin.x,
        h0 - (frame.origin.y + frame.size.height),
        frame.size.width,
        frame.size.height,
    )
}

/// Log the M2d placement check after a frame change or a show: is the panel inside the visible
/// frame of the **tray icon's** screen, and how far below the icon is its top edge.
///
/// The tray-screen form, by rule (ONDAR.md, "M2d: resize in place", R3). The obvious alternative —
/// the panel inside its *own* `screen().visibleFrame()` — read `true` on both of Step 0's forced
/// failures (a 720 pt panel sitting on the wrong display entirely) and cannot catch displacement
/// onto another display. The tray screen is the `NSScreen` whose frame contains the icon's
/// centre, the same acceptor test `resolve_display` makes on Tauri's monitors, done here on AppKit's.
///
/// `gap_below_icon` is the sensitive readout for the D1 cap: 6 (`TRAY_GAP`) means `clamp_into` was
/// idle; 0 means it fired, i.e. the panel did not fit below the icon. A log line, not an
/// assertion: nothing here may panic, and no unit test can reach a live `NSScreen`.
fn log_tray_screen_check<R: Runtime>(panel: &PanelHandle<R>, rect: Rect) {
    let Some(mtm) = foundation::MainThreadMarker::new() else {
        log::warn!("panel placed: check skipped, not on the main thread");
        return;
    };
    let (Position::Physical(p), Size::Physical(sz)) = (rect.position, rect.size) else {
        log::warn!("panel placed: check skipped, tray rect is not Physical");
        return;
    };
    let Some(h0) = main_screen_height(mtm) else {
        log::warn!("panel placed: check skipped, no NSScreen");
        return;
    };
    let (px, py, pw, ph) = (
        f64::from(p.x),
        f64::from(p.y),
        f64::from(sz.width),
        f64::from(sz.height),
    );
    // First acceptor wins, and index 0 is the menu-bar screen, so it wins any tie. The same
    // half-open test `resolve_display` makes on Tauri's monitors, on AppKit's screens.
    let screens = NSScreen::screens(mtm);
    let tray_screen = screens.iter().enumerate().find(|(_, s)| {
        let scale = s.backingScaleFactor();
        top_left_rect(s.frame(), h0).contains((px + pw / 2.0) / scale, (py + ph / 2.0) / scale)
    });
    let frame = panel.as_panel().frame();
    match tray_screen {
        Some((i, s)) => {
            let icon_bottom_tl = (py + ph) / s.backingScaleFactor();
            let tl = top_left_rect(frame, h0);
            log::info!(
                "panel placed inside_tray_screen_visible={} gap_below_icon={} tray_screen=[{i}] {:?} \
                 frame_tl=[{},{} {}x{}]",
                foundation::NSContainsRect(s.visibleFrame(), frame),
                tl.y - icon_bottom_tl,
                s.localizedName().to_string(),
                tl.x,
                tl.y,
                tl.width,
                tl.height
            );
        }
        None => log::warn!(
            "panel placed: no NSScreen contains the tray icon centre; rect=({px},{py} {pw}x{ph})"
        ),
    }
}

/// The popover's `PanelHandle`, or a warning naming the caller (`what`) and `None`. One message
/// shape for every site; the spike once logged one `window not found` for two different failure
/// sites and the log could not say which had fired.
fn panel_handle<R: Runtime>(handle: &AppHandle<R>, what: &str) -> Option<PanelHandle<R>> {
    match handle.get_webview_panel(PANEL_LABEL) {
        Ok(panel) => Some(panel),
        Err(_) => {
            log::warn!("panel {what}: panel not in the tauri-nspanel store");
            None
        }
    }
}

/// The popover's `PanelHandle` and its Tauri window (the latter for the monitor list), or a
/// warning and `None`.
fn panel_and_window<R: Runtime>(
    handle: &AppHandle<R>,
    what: &str,
) -> Option<(PanelHandle<R>, WebviewWindow<R>)> {
    let panel = panel_handle(handle, what)?;
    match handle.get_webview_window(PANEL_LABEL) {
        Some(window) => Some((panel, window)),
        None => {
            log::warn!("panel {what}: panel has no tauri webview window");
            None
        }
    }
}

/// Emit the layout the page should render, logging either way.
fn emit_layout<R: Runtime>(handle: &AppHandle<R>, layout: PanelLayout, reason: &str) {
    match handle.emit(crate::events::PANEL_LAYOUT, layout) {
        Ok(()) => log::info!(
            "panel layout transition={:?} view={:?} state={:?} size_points=({}, {}) expandable={} \
             reason={reason}",
            layout.transition,
            layout.view,
            layout.state,
            layout.width,
            layout.height,
            layout.expandable
        ),
        Err(e) => log::warn!("panel layout {layout:?} reason={reason} not emitted: {e}"),
    }
}

/// The page's expand/collapse request (`commands::panel::panel_set_expanded`). The page reports a
/// click; Rust decides: a **refusal** when decision D1's floor says the capped height would not
/// exceed the collapsed one (`expandable=false` — the page's control is disabled then, decision
/// D4, but the decision is made here regardless), a logged no-op when the panel is hidden, and
/// otherwise a layout for the new height against a **fresh tray rect** — never a size-only change,
/// which leaves a panel on the wrong display (M2d Step 0 P5) — applied as one frame change.
///
/// Main-thread discipline and logging as in [`hide`].
pub fn set_expanded<R: Runtime>(handle: &AppHandle<R>, expanded: bool) {
    let on_main = handle.clone();
    let queued = handle.run_on_main_thread(move || {
        let want = if expanded {
            PanelHeight::Expanded
        } else {
            PanelHeight::Collapsed
        };
        // No control on the About pane, so no resize from it: refused here whatever the page
        // renders (decided 2026-09-21). Back restores the choice through `view_back`.
        if let Some(reason) = on_main.state::<PanelState>().resize_refusal() {
            log::info!("panel resize want={want:?} effective=false reason={reason}");
            return;
        }
        resize_on_main(&on_main, want, "resize");
    });
    if let Err(e) = queued {
        log::warn!("panel resize expanded={expanded} not queued: {e}");
    }
}

/// The resize path proper, on the main thread: lay out for `want` against a fresh tray rect,
/// refuse (D1's floor) or start the round trip. `reason` is the log's: `resize` for the control,
/// `back` for the restore after About.
fn resize_on_main<R: Runtime>(on_main: &AppHandle<R>, want: PanelHeight, reason: &str) {
    let Some((panel, window)) = panel_and_window(on_main, "resize") else {
        return;
    };
    if !panel.is_visible() {
        log::info!("panel resize want={want:?} effective=false reason=hidden");
        return;
    }
    let Some(rect) = crate::tray::rect(on_main) else {
        return;
    };
    let laid = match layout_for(&window, rect, want) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("panel resize want={want:?} layout failed: {e}");
            return;
        }
    };
    let state = on_main.state::<PanelState>();
    if laid.state != want {
        log::info!(
            "panel resize want={want:?} effective=false reason=refused expandable={} \
             capped_height={}",
            laid.expandable,
            laid.size.1
        );
        // Same generation, so the page's commit effect does not fire; only the flag changes.
        let refreshed = state.refresh_expandable(laid.expandable);
        emit_layout(on_main, refreshed, "refused");
        return;
    }
    let view = state.layout().view;
    let emitted = state.request(view, &laid, LayoutKind::Resize, rect);
    emit_layout(on_main, emitted, reason);
    // The frame changes when the page has committed the new layout (decision D3), or when
    // the fallback fires — `complete_layout`, either way.
    arm_fallback(on_main, emitted.generation);
    log::info!(
        "panel layout pending generation={} kind=resize want={want:?} capped={} reason={reason}",
        emitted.generation,
        laid.capped
    );
}

/// The page's Back button (`commands::panel::panel_view_back`): the About pane gave way to the
/// transport. The transition itself is the page's — it is made inside an already-shown popover
/// and re-asserted by Rust on the next show anyway — but the pane on show is Rust's state, so the
/// page reports it; otherwise the next resize's `panel:layout` would carry `About` and the
/// listener would put the About pane back with no gesture on it (`/code-review` C1).
pub fn view_back<R: Runtime>(handle: &AppHandle<R>) {
    let on_main = handle.clone();
    let queued = handle.run_on_main_thread(move || {
        let state = on_main.state::<PanelState>();
        let before = state.set_view(PanelView::Transport);
        let chosen = state.chosen();
        log::info!("panel view back from={before:?} to=Transport chosen={chosen:?}");
        // About showed at the collapsed height whatever the choice; the transport gets it back.
        if chosen == PanelHeight::Expanded {
            resize_on_main(&on_main, PanelHeight::Expanded, "back");
        }
    });
    if let Err(e) = queued {
        log::warn!("panel view back not queued: {e}");
    }
}

/// Tray click: hide if shown **or about to be shown**, otherwise show under the icon. Decides
/// only; the ordering in and out is [`hide`] and [`show_at`], so a tray close is one effective hide
/// like every other close.
///
/// "About to be shown" is D3's window: a show requested and waiting for the page's commit (or the
/// 250 ms fallback), during which `is_visible()` is false. Deciding on visibility alone there
/// turned the second click into another show request — harmless on a healthy page (the window is
/// 2–8 ms, no human lands in it) but, with the frozen page the fallback exists for, clicks faster
/// than 250 ms each superseded the pending show and pushed it out while the user was trying to
/// close it (`/code-review` C3, confirmed on that sub-scenario). A click during a pending show
/// now hides, which cancels it.
pub fn toggle<R: Runtime>(handle: &AppHandle<R>, rect: Rect) {
    let on_main = handle.clone();
    let queued = handle.run_on_main_thread(move || {
        let visible = on_main
            .get_webview_panel(PANEL_LABEL)
            .is_ok_and(|p| p.is_visible());
        if visible || on_main.state::<PanelState>().show_pending() {
            hide(&on_main, HideReason::Toggle);
        } else {
            show_at(&on_main, rect, ShowReason::Toggle);
        }
    });
    if let Err(e) = queued {
        log::warn!("panel toggle not queued: {e}");
    }
}

/// Log the decoded occlusion state `OCCLUSION_SETTLE` after show.
///
/// `NSWindowOcclusionState::Visible` is `1 << 1`, and the raw value carries undocumented high
/// bits (8192 = `1 << 13` is the common one), so a non-zero raw value is *not* "visible". The
/// decoded bit is the evidence; the raw value is printed beside it only for diagnosis.
fn log_settled_occlusion<R: Runtime>(handle: &AppHandle<R>, panel: &PanelHandle<R>) {
    let panel = panel.clone();
    let shown_at = Instant::now();
    after_delay_on_main(
        handle,
        "ondar-occlusion",
        OCCLUSION_SETTLE,
        "occlusion read".into(),
        move |_| {
            Some(move |_: &AppHandle<R>| {
                let ns = panel.as_panel();
                let state = ns.occlusionState();
                log::info!(
                    "panel occlusion settled_read_after_ms={} settled_visible={} settled_raw={} key={}",
                    shown_at.elapsed().as_millis(),
                    state.contains(NSWindowOcclusionState::Visible),
                    state.0,
                    ns.isKeyWindow()
                );
            })
        },
    );
}

/// A rectangle in **global logical points, top-left origin** — the space every quantity in this
/// module is converted into before anything is compared.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PointRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl PointRect {
    fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Half-open on both axes, so a point on the seam between two displays belongs to one of them.
    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// One display in points, plus the scale Tauri multiplied its own values by.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Display {
    bounds: PointRect,
    work_area: PointRect,
    scale: f64,
}

/// The result of anchoring: where the panel goes, in points, which display was chosen, every
/// display that accepted the tray rect, and how the choice was reached.
#[derive(Debug, PartialEq)]
struct Anchored {
    position: (f64, f64),
    display: Option<usize>,
    accepted: Vec<usize>,
    resolution: Resolution,
}

/// How `Anchored::display` was arrived at. The caller logs it; the last three are the paths that
/// used to be silent.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Resolution {
    /// Exactly one display accepted the rect.
    Unambiguous,
    /// Several accepted and the primary is among them. With a single menu bar the status item is on
    /// the primary display, so that is the one — rather than whichever display the list happened to
    /// return first.
    PrimaryAmongAcceptors,
    /// Several accepted and the primary is not among them: genuinely ambiguous. The first acceptor
    /// is used, clamped, and the caller warns.
    AmbiguousWithoutPrimary,
    /// Nothing accepted. The primary display's scale and work area are used, clamped, because a
    /// rect that fits nowhere is still a rect from a menu bar.
    PrimaryFallback,
    /// Nothing accepted and no primary known. The rect is assumed to be points and nothing is
    /// clamped — last resort, and the caller warns.
    Unresolved,
}

/// The second half of placing the panel: the tray rect into the chosen display's points, centred
/// below the icon, clamped into that display's work area (no clamp without a display). Pure;
/// shared with `layout`, which resolves the display once for the cap and the placement.
fn place(
    tray: PointRect,
    panel: (f64, f64),
    display: Option<usize>,
    displays: &[Display],
) -> (f64, f64) {
    let scale = display.map_or(1.0, |i| displays[i].scale);
    let tray = PointRect::new(
        tray.x / scale,
        tray.y / scale,
        tray.width / scale,
        tray.height / scale,
    );
    let position = centred_below(tray, panel);
    match display {
        Some(i) => clamp_into(position, panel, displays[i].work_area),
        None => position,
    }
}

/// Which display hosts the tray rect: the acceptors, the choice among them, and how it was made —
/// the first half of placing the panel (`place` is the second; `layout` runs both). **Pure, and
/// entirely in points.**
///
/// `tray` arrives exactly as `TrayIconEvent::Click` gives it: global points multiplied by the
/// *status item display's* scale (tray-icon 0.24.2 `platform_impl/macos/mod.rs:515-528`), top-left
/// origin. The panel's size is in **points**, owned by Rust (`PANEL_WIDTH` by the collapsed or
/// the capped expanded height) — never read back from the window. Until M2d the
/// size arrived as the window's `outer_size` with its own `scale_factor`, and the division into
/// points happened beside this so a test could reach it (`/code-review` finding 3, 2026-09-16); measured
/// equal to the constant on a 1x and a 2x panel alike (M2d Step 0, `run-01-p5.log:34`,
/// `run-02-p4.log:21`). With the size a constant, the quantity that division was about no longer
/// exists, and neither does the test that pinned it (see the 1x test below).
///
/// **Why points, decided 2026-09-16 (route B).** Tauri reports each monitor's
/// `position`/`size`/`work_area` as global points multiplied by *that monitor's own* scale
/// (tao `monitor.rs:225-231`; `work_area` likewise, tauri-runtime-wry `monitor/macos.rs:16-27`).
/// On a mixed-scale layout there is therefore no common physical space: two monitors' values are
/// not comparable, and a tray rect made at one display's scale cannot be compared with a work area
/// made at another's. Measured failure before this change (2026-09-16, panel forced onto the 2x
/// built-in, icon on the 1x BenQ): the panel landed 649 pt left of the icon and 12 pt inside the
/// menu bar band.
///
/// **How point space is entered.** The rect does not carry its own scale, so it has to be
/// recovered. This divides the icon's centre by each display's scale and keeps the displays whose
/// point bounds then contain it. The alternative was to read the status item's own
/// `NSWindow.screen()` through `tray-icon`'s internals: exact, but it needs a live AppKit window
/// and the main thread, which would move this decision out of unit tests entirely, and the bounds
/// test is needed for clamping anyway. `PointRect::contains` is half-open, so a display seam
/// belongs to exactly one display.
///
/// **Ties are broken towards the primary display, not towards list order** (`/code-review`
/// finding 1, 2026-09-16). Two displays of different scale can both accept the same rect — a 2x
/// primary with a 1x display to its right does so for essentially every icon position — and taking
/// `accepted.first()` meant taking whatever `CGGetActiveDisplayList` returned first, an ordering
/// nothing here relies on deliberately. The status item sits on the display that owns the menu bar,
/// which with a single menu bar is the primary display — the same display `tray-icon`'s own y-flip
/// measures against (`CGMainDisplayID`, `mod.rs:610-612`).
///
/// **That invariant is a user setting, not a property of macOS.** With "Displays have separate
/// Spaces" on (System Settings → Desktop & Dock) every display gets its own menu bar, and a status
/// item can then sit on a non-primary display. So the primary is a *preference* among the
/// acceptors, never an assumption: when it is not among them, the first acceptor is used and
/// clamped, and the caller warns. See ONDAR.md, "M2b: coordinates are logical points".
fn resolve_display(
    tray: PointRect,
    displays: &[Display],
    primary: Option<usize>,
) -> (Option<usize>, Vec<usize>, Resolution) {
    let accepted: Vec<usize> = displays
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            let centre_x = (tray.x + tray.width / 2.0) / d.scale;
            let centre_y = (tray.y + tray.height / 2.0) / d.scale;
            d.bounds.contains(centre_x, centre_y)
        })
        .map(|(i, _)| i)
        .collect();

    let primary = primary.filter(|i| *i < displays.len());
    let (display, resolution) = match accepted.as_slice() {
        // Nothing accepted: a layout change between the click and this call, or a display list that
        // disagrees with it. The primary display's scale and work area are the best guess available
        // and clamping to it keeps the panel on screen — assuming points and skipping the clamp put
        // it off screen entirely on a 2x display (`/code-review` finding 2).
        [] => match primary {
            Some(i) => (Some(i), Resolution::PrimaryFallback),
            None => (None, Resolution::Unresolved),
        },
        [one] => (Some(*one), Resolution::Unambiguous),
        many => match primary {
            Some(i) if many.contains(&i) => (Some(i), Resolution::PrimaryAmongAcceptors),
            _ => (Some(many[0]), Resolution::AmbiguousWithoutPrimary),
        },
    };
    (display, accepted, resolution)
}

/// The result of laying the panel out for a tray rect and a wanted height: the placement's
/// answer for the size that came out of decision D1's cap, plus the cap's own outputs. `state` is
/// the height **actually laid out** — `Collapsed` when `Expanded` was asked for and refused.
#[derive(Debug, PartialEq)]
struct Layout {
    state: PanelHeight,
    /// Points.
    size: (f64, f64),
    anchored: Anchored,
    /// `Expanded`, and shorter than `EXPANDED_HEIGHT_NOMINAL` because the display is.
    capped: bool,
    /// Decision D1's floor: the capped expanded height exceeds `COLLAPSED_HEIGHT`. Reported on
    /// every layout, collapsed ones included, so the page can disable its control (decision D4).
    expandable: bool,
}

/// Size **and** position for a tray rect and a wanted height, in one pure function (M2d Step 0
/// P4: the cap belongs where the size is chosen, before the anchor, and needs the chosen display's
/// work area, which `resolve_display` gives; the refusal falls out of the same computation).
///
/// **The cap (decision D1).** The expanded height is `min(EXPANDED_HEIGHT_NOMINAL, usable)`, where
/// `usable` is the height that fits between the panel's top edge — `TRAY_GAP` below the icon's
/// bottom edge, which is where `centred_below` puts it — and `EDGE_MARGIN` above the bottom of the
/// chosen display's work area. On every arrangement measured the icon's bottom edge coincides with
/// the work-area top, so this equals D1's recorded `work-area height − TRAY_GAP − EDGE_MARGIN`:
/// 610 − 6 − 6 = **598 pt on the ANMITE, measured** (`run-02-p4.log:32`, landed `[226,36 360×598]`).
/// Measuring from the icon rather than the work-area top keeps `clamp_into` idle by construction
/// whenever the two ever differ; the readout is the gap below the icon (6 = idle, 0 = fired).
///
/// **The floor (D1, provisional).** Expansion is refused when the capped height would not exceed
/// `COLLAPSED_HEIGHT`: `expandable` is false and the collapsed layout is returned. No attached
/// display can exercise this branch, so the unit tests drive it from a synthetic work area on both
/// sides of the boundary — an inverted comparison would refuse on *every* display.
///
/// With no chosen display (`Resolution::Unresolved`) there is no work area to cap against; the
/// nominal height is used uncapped and reported expandable, and the caller warns as it does today.
fn layout(
    tray: PointRect,
    want: PanelHeight,
    displays: &[Display],
    primary: Option<usize>,
) -> Layout {
    let (display, accepted, resolution) = resolve_display(tray, displays, primary);
    let (expanded_height, expandable) = match display {
        Some(i) => {
            let d = displays[i];
            let icon_bottom = (tray.y + tray.height) / d.scale;
            let usable =
                (d.work_area.y + d.work_area.height - EDGE_MARGIN) - (icon_bottom + TRAY_GAP);
            let h = EXPANDED_HEIGHT_NOMINAL.min(usable);
            (h, h > COLLAPSED_HEIGHT)
        }
        None => (EXPANDED_HEIGHT_NOMINAL, true),
    };
    let (state, height) = match want {
        PanelHeight::Expanded if expandable => (PanelHeight::Expanded, expanded_height),
        PanelHeight::Expanded | PanelHeight::Collapsed => {
            (PanelHeight::Collapsed, COLLAPSED_HEIGHT)
        }
    };
    let size = (PANEL_WIDTH, height);
    Layout {
        state,
        size,
        anchored: Anchored {
            position: place(tray, size, display, displays),
            display,
            accepted,
            resolution,
        },
        capped: state == PanelHeight::Expanded && height < EXPANDED_HEIGHT_NOMINAL,
        expandable,
    }
}

/// Centre the panel horizontally on the tray icon, top edge `TRAY_GAP` below the icon's bottom.
/// Points in, points out.
///
/// `y` grows downward, so the icon's height is *added*: getting that sign backwards is the classic
/// "panel appears above the menu bar / at the bottom of the screen" bug.
fn centred_below(tray: PointRect, panel: (f64, f64)) -> (f64, f64) {
    (
        tray.x + tray.width / 2.0 - panel.0 / 2.0,
        tray.y + tray.height + TRAY_GAP,
    )
}

/// Push a panel position inside a work area, keeping `EDGE_MARGIN` from its left and right edges.
///
/// Menu bar items live at the right edge of a display, so a centred panel runs off it once the icon
/// is within half a panel width of the edge. Clamping rather than flipping to right-aligned keeps
/// the panel centred on the icon whenever it fits and slides it only as far as needed, which is how
/// the system's own status item menus behave.
///
/// Vertically the panel is only pulled up if it would run past the bottom, and never above the work
/// area's top — which is what keeps it out of the menu bar band, and out of the notch band on a
/// display that has one (measured 2026-09-16: the built-in reports a 32 pt `safeAreaInsets.top` and
/// its work area already excludes that band, whether or not it hosts the menu bar).
///
/// Written without `f64::clamp`, which panics when min > max — a panel wider than the area would do
/// that; here it pins to the left margin instead.
fn clamp_into(position: (f64, f64), panel: (f64, f64), area: PointRect) -> (f64, f64) {
    let right = area.x + area.width - panel.0 - EDGE_MARGIN;
    let left = area.x + EDGE_MARGIN;
    let bottom = area.y + area.height - panel.1;
    (
        position.0.min(right).max(left),
        position.1.min(bottom).max(area.y),
    )
}

/// Gather the live display state, lay the panel out for `want` under `rect`, and log what was
/// chosen. The layout is for the size the panel is *about to have*, not the size it has: an expand
/// or collapse lays out for the new height before the frame changes (Step 0 P5 — a size-only
/// change leaves a panel on the wrong display).
///
/// The position is the panel's top-left in global points; `apply_frame` converts it to a Cocoa
/// frame against the menu-bar screen. Nothing in the path reads or divides by the panel window's
/// own scale, which is what made the pre-M2b code depend on which display the panel happened to be
/// sitting on.
fn layout_for<R: Runtime>(
    window: &WebviewWindow<R>,
    rect: Rect,
    want: PanelHeight,
) -> tauri::Result<Layout> {
    // `tray-icon` always sends `Physical` on macOS (`mod.rs:515-528` builds it with
    // `to_physical`). A `Logical` rect would already be points; it is passed through with a
    // warning rather than silently scaled, because guessing a scale is how this defect started.
    let tray = match (rect.position, rect.size) {
        (Position::Physical(p), Size::Physical(s)) => PointRect::new(
            f64::from(p.x),
            f64::from(p.y),
            f64::from(s.width),
            f64::from(s.height),
        ),
        (position, size) => {
            log::warn!("tray rect is not Physical ({position:?}, {size:?}); treating it as points");
            let p = position.to_logical::<f64>(1.0);
            let s = size.to_logical::<f64>(1.0);
            PointRect::new(p.x, p.y, s.width, s.height)
        }
    };

    // Each monitor converted by its *own* scale, which is the only conversion that is correct for
    // it (see `resolve_display`).
    let monitors = window.available_monitors()?;
    let displays: Vec<Display> = monitors
        .iter()
        .map(|m| {
            let scale = m.scale_factor();
            let position = m.position();
            let size = m.size();
            let work = m.work_area();
            Display {
                bounds: PointRect::new(
                    f64::from(position.x) / scale,
                    f64::from(position.y) / scale,
                    f64::from(size.width) / scale,
                    f64::from(size.height) / scale,
                ),
                work_area: PointRect::new(
                    f64::from(work.position.x) / scale,
                    f64::from(work.position.y) / scale,
                    f64::from(work.size.width) / scale,
                    f64::from(work.size.height) / scale,
                ),
                scale,
            }
        })
        .collect();

    // Which entry is the primary display. `primary_monitor()` is `CGDisplay::main()` (tao
    // `monitor.rs:158-160`) — the same display `tray-icon` flips its rect against — but it comes
    // back as a `Monitor`, not an index, so it is matched by name and position. `None` here is
    // survivable: `resolve_display` then treats ambiguity as ambiguity.
    let primary = window.primary_monitor()?.and_then(|p| {
        monitors
            .iter()
            .position(|m| m.name() == p.name() && m.position() == p.position())
    });

    let laid = layout(tray, want, &displays, primary);
    let anchored = &laid.anchored;

    match anchored.resolution {
        Resolution::AmbiguousWithoutPrimary => log::warn!(
            "panel anchor: {} displays accepted the tray rect {tray:?} and the primary ({primary:?}) \
             is not among them; using {:?}. accepted={:?} displays={:?}",
            anchored.accepted.len(),
            anchored.display,
            anchored.accepted,
            displays
        ),
        Resolution::PrimaryFallback => log::warn!(
            "panel anchor: no display accepted the tray rect {tray:?}; falling back to the primary \
             display {:?} and clamping. displays={displays:?}",
            anchored.display
        ),
        Resolution::Unresolved => log::warn!(
            "panel anchor: no display accepted the tray rect {tray:?} and no primary display is \
             known; assuming points and not clamping. displays={displays:?}"
        ),
        Resolution::Unambiguous | Resolution::PrimaryAmongAcceptors => {}
    }

    let chosen = anchored.display.map(|i| displays[i]);
    log::info!(
        "panel anchor tray_physical={tray:?} want={want:?} state={:?} size_points={:?} capped={} \
         expandable={} display={:?} primary={primary:?} accepted={:?} resolution={:?} \
         position_points={:?} work_area={:?}",
        laid.state,
        laid.size,
        laid.capped,
        laid.expandable,
        anchored.display,
        anchored.accepted,
        anchored.resolution,
        anchored.position,
        chosen.map(|d| d.work_area)
    );

    Ok(laid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three displays as measured on 2026-09-16 with the menu bar on the BenQ, converted to
    /// points by each monitor's own scale (`_handover/m2b-step0-logs/`):
    ///   BenQ    Tauri (0,0) 1920x1080 scale 1, work_area (0,30) 1920x1050
    ///   built-in      (486,2160) 3024x1964 scale 2, work_area (486,2224) 3024x1900
    /// The built-in's 32 pt inset there is the **notch band**, not a menu bar — it did not host the
    /// menu bar in that arrangement. 2224 + 1900 = 2160 + 1964 reconciles exactly. (An earlier
    /// draft wrote 1898/949, which are the arrangement-2 figures: `/code-review` finding 4.)
    ///   ANMITE        (3840,880) 1920x1280 scale 2, work_area = full frame
    fn menubar_on_benq() -> Vec<Display> {
        vec![
            Display {
                bounds: PointRect::new(0.0, 0.0, 1920.0, 1080.0),
                work_area: PointRect::new(0.0, 30.0, 1920.0, 1050.0),
                scale: 1.0,
            },
            Display {
                bounds: PointRect::new(243.0, 1080.0, 1512.0, 982.0),
                work_area: PointRect::new(243.0, 1112.0, 1512.0, 950.0),
                scale: 2.0,
            },
            Display {
                bounds: PointRect::new(1920.0, 440.0, 960.0, 640.0),
                work_area: PointRect::new(1920.0, 440.0, 960.0, 640.0),
                scale: 2.0,
            },
        ]
    }

    /// The built-in as measured on 2026-09-16 while it hosted the menu bar: Tauri (0,0) 3024x1964
    /// scale 2, work_area (0,66) 3024x1898 — 33 pt of menu bar and notch band excluded.
    fn menubar_on_builtin() -> Vec<Display> {
        vec![Display {
            bounds: PointRect::new(0.0, 0.0, 1512.0, 982.0),
            work_area: PointRect::new(0.0, 33.0, 1512.0, 949.0),
            scale: 2.0,
        }]
    }

    /// `/code-review` finding 1's layout, with the 1x display listed first: a 1x display to the
    /// right of a 2x primary that hosts the menu bar. Artificial only in its ordering.
    fn ambiguous_pair() -> Vec<Display> {
        vec![
            Display {
                bounds: PointRect::new(1512.0, 0.0, 1920.0, 1080.0),
                work_area: PointRect::new(1512.0, 0.0, 1920.0, 1080.0),
                scale: 1.0,
            },
            Display {
                bounds: PointRect::new(0.0, 0.0, 1512.0, 982.0),
                work_area: PointRect::new(0.0, 33.0, 1512.0, 949.0),
                scale: 2.0,
            },
        ]
    }

    /// The collapsed panel's size in points.
    fn panel() -> (f64, f64) {
        (PANEL_WIDTH, COLLAPSED_HEIGHT)
    }

    /// The M2b anchoring tests' entry: the collapsed layout's placement through the production
    /// `layout` — display resolution, centring, clamp — with the cap along for the ride (idle at
    /// 420 pt on every fixture here).
    fn anchor(tray: PointRect, displays: &[Display], primary: Option<usize>) -> Anchored {
        layout(tray, PanelHeight::Collapsed, displays, primary).anchored
    }

    /// The ANMITE hosting the menu bar, as measured 2026-09-18 (M2d Step 0 P4, `run-02-p4.log:5,8`):
    /// Tauri (0,0) 1920x1280 scale 2, work_area (0,60) 1920x1220 → (0,30) 960x610 pt.
    fn menubar_on_anmite() -> Vec<Display> {
        vec![Display {
            bounds: PointRect::new(0.0, 0.0, 960.0, 640.0),
            work_area: PointRect::new(0.0, 30.0, 960.0, 610.0),
            scale: 2.0,
        }]
    }

    /// The tray rect measured in that arrangement (`run-02-p4.log:21`): (788,0) 48x60 physical at 2x
    /// = (394,0) 24x30 pt, so the icon's bottom edge is at 30 pt — the work-area top.
    fn anmite_tray() -> PointRect {
        PointRect::new(788.0, 0.0, 48.0, 60.0)
    }

    /// A synthetic 1x display `height` pt tall with a 30 pt menu bar, hosting an icon whose bottom
    /// edge is at 30 pt. `usable` for the cap is then `(height − 6) − (30 + 6) = height − 42`, which
    /// is what lets a test put the cap exactly on decision D1's floor.
    fn short_display(height: f64) -> Vec<Display> {
        vec![Display {
            bounds: PointRect::new(0.0, 0.0, 1000.0, height),
            work_area: PointRect::new(0.0, 30.0, 1000.0, height - 30.0),
            scale: 1.0,
        }]
    }

    fn short_display_tray() -> PointRect {
        PointRect::new(100.0, 0.0, 24.0, 30.0)
    }

    /// The assertion the M2b gate settled on: the panel rect must lie inside the work area of the
    /// display it was placed on. Not "the right display" — on 2026-09-16 the forced mixed-scale
    /// case landed on the *correct* display, 649 pt from the icon and 12 pt inside the menu bar,
    /// so a display-identity assertion would have passed it.
    fn assert_inside_work_area(got: &Anchored, displays: &[Display]) {
        assert_size_inside_work_area(got, panel(), displays);
    }

    /// The same assertion for a panel of any size — the expanded layouts below.
    fn assert_size_inside_work_area(got: &Anchored, size: (f64, f64), displays: &[Display]) {
        let i = got.display.expect("a display should have been chosen");
        let area = displays[i].work_area;
        let (x, y) = got.position;
        let (w, h) = size;
        assert!(
            x >= area.x && x + w <= area.x + area.width,
            "panel x {x}..{} outside work area {area:?}",
            x + w
        );
        assert!(
            y >= area.y && y + h <= area.y + area.height,
            "panel y {y}..{} outside work area {area:?}",
            y + h
        );
    }

    /// Measured 2026-09-16, menu bar on the 1x BenQ: rect (1286,0) 24x30 landed the panel at
    /// points (1118,36) — `[1118,624 360x420]` in Cocoa, 1080 − (624+420) = 36.
    ///   x = 1286 + 24/2 − 360/2 = 1118    y = 0 + 30 + 6 = 36
    ///
    /// Until M2d a second test, `mixed_scale_does_not_change_the_answer`, fed the same rect with the
    /// panel's size as a 2x `outer_size` (720x840) and scale 2, pinning the division into points
    /// inside `anchor_points` (`/code-review` finding 3, 2026-09-16). M2d made the size a Rust
    /// constant in points that is never read from the window, so that division — and the quantity
    /// it was about — no longer exists; the test's inputs would be identical to this one's and it
    /// could not fail on its own, which is the very condition its own comment gave for rewriting
    /// it. Retired 2026-09-18 rather than kept as coverage that is not.
    #[test]
    fn measured_1x_tray_rect_matches_the_observed_landing() {
        let displays = menubar_on_benq();
        let got = anchor(PointRect::new(1286.0, 0.0, 24.0, 30.0), &displays, Some(0));
        assert_eq!(got.position, (1118.0, 36.0));
        assert_eq!(got.display, Some(0));
        assert_eq!(got.accepted, vec![0]);
        assert_inside_work_area(&got, &displays);
    }

    /// Measured 2026-09-16 with the menu bar on the 2x built-in: rect (1760,0) 48x66 = (880,0)
    /// 24x33 in points. x = 880 + 12 − 180 = 712, y = 0 + 33 + 6 = 39. (The landing measured that
    /// day was y = 36 with the old 6-physical-pixel gap, i.e. 3 pt; the gap is now 6 pt.)
    #[test]
    fn measured_2x_tray_rect_is_converted_by_its_own_display_scale() {
        let displays = menubar_on_builtin();
        let got = anchor(PointRect::new(1760.0, 0.0, 48.0, 66.0), &displays, Some(0));
        assert_eq!(got.position, (712.0, 39.0));
        assert_inside_work_area(&got, &displays);
    }

    /// The 2026-09-12 fixture, now in point terms: rect (1932,0) 48x66 on the 2x built-in =
    /// (966,0) 24x33 points. x = 966 + 12 − 180 = 798, y = 33 + 6 = 39.
    #[test]
    fn the_2026_09_12_rect_still_centres_under_the_icon() {
        let displays = menubar_on_builtin();
        let got = anchor(PointRect::new(1932.0, 0.0, 48.0, 66.0), &displays, Some(0));
        assert_eq!(got.position, (798.0, 39.0));
        assert_eq!(got.position.0 + panel().0 / 2.0, 966.0 + 12.0);
    }

    /// A non-zero, **negative** icon `y` — the case the M2a fixture could not reach, since a
    /// menu-bar display is always at the origin. Only a display whose bounds start above the origin
    /// can host one, so this is the arrangement-2 BenQ (points (−243,−1080) 1920x1080, scale 1)
    /// pretending to host the icon.
    ///   y = −1080 + 30 + 6 = −1044, so a flipped icon-height term (−1080 − 30 − 6 = −1116) or a
    ///   flipped `tray.y` (1080 + 30 + 6 = 1116) both fail here.
    #[test]
    fn negative_origin_display_keeps_the_panel_below_the_icon() {
        let displays = vec![Display {
            bounds: PointRect::new(-243.0, -1080.0, 1920.0, 1080.0),
            work_area: PointRect::new(-243.0, -1080.0, 1920.0, 1080.0),
            scale: 1.0,
        }];
        let got = anchor(
            PointRect::new(100.0, -1080.0, 24.0, 30.0),
            &displays,
            Some(0),
        );
        assert_eq!(got.position, (-68.0, -1044.0));
        assert_inside_work_area(&got, &displays);
    }

    /// Clamping on a negative-origin display: an icon near its right edge. That display spans
    /// x −243..1677, so the icon must be on it to be accepted — an earlier draft used x = 1880,
    /// which is off the display, and correctly got no acceptor and no clamp (the test below keeps
    /// that case). At x = 1600: centred x = 1600 + 12 − 180 = 1432; right limit
    /// −243 + 1920 − 360 − 6 = 1311.
    #[test]
    fn right_edge_icon_is_pulled_back_on_a_negative_origin_display() {
        let displays = vec![Display {
            bounds: PointRect::new(-243.0, -1080.0, 1920.0, 1080.0),
            work_area: PointRect::new(-243.0, -1080.0, 1920.0, 1080.0),
            scale: 1.0,
        }];
        let got = anchor(
            PointRect::new(1600.0, -1080.0, 24.0, 30.0),
            &displays,
            Some(0),
        );
        assert_eq!(got.accepted, vec![0]);
        assert_eq!(got.position.0, 1311.0);
        assert_inside_work_area(&got, &displays);
    }

    /// Clamping on an off-origin display: the ANMITE, points (1920,440) 960x640. An icon at its
    /// right edge: centred x = 2860 + 12 − 180 = 2692; right limit = 1920 + 960 − 360 − 6 = 2514.
    #[test]
    fn right_edge_icon_is_pulled_back_on_an_off_origin_display() {
        let displays = vec![Display {
            bounds: PointRect::new(1920.0, 440.0, 960.0, 640.0),
            work_area: PointRect::new(1920.0, 470.0, 960.0, 610.0),
            scale: 2.0,
        }];
        // Physical at that display's scale 2: (5720, 880) 48x60 = (2860, 440) 24x30 in points.
        let got = anchor(
            PointRect::new(5720.0, 880.0, 48.0, 60.0),
            &displays,
            Some(0),
        );
        assert_eq!(got.position, (2514.0, 476.0));
        assert_inside_work_area(&got, &displays);
    }

    /// An icon centred exactly on the seam between two displays belongs to one of them, because
    /// `PointRect::contains` is half-open. Two 1x displays meeting at x = 1000, icon centre exactly
    /// 1000: the left display must *not* accept it. With closed bounds both accept and the result
    /// is a reported ambiguity where there is none — this test is what makes that claim
    /// load-bearing rather than a comment.
    #[test]
    fn a_seam_belongs_to_exactly_one_display() {
        let displays = vec![
            Display {
                bounds: PointRect::new(0.0, 0.0, 1000.0, 1000.0),
                work_area: PointRect::new(0.0, 30.0, 1000.0, 970.0),
                scale: 1.0,
            },
            Display {
                bounds: PointRect::new(1000.0, 0.0, 1000.0, 1000.0),
                work_area: PointRect::new(1000.0, 30.0, 1000.0, 970.0),
                scale: 1.0,
            },
        ];
        let got = anchor(PointRect::new(988.0, 0.0, 24.0, 30.0), &displays, None);
        assert_eq!(got.accepted, vec![1], "the seam is the right display's");
    }

    /// An icon off every display: no acceptor, so no clamp, and the caller warns. A real
    /// possibility when the layout changes between the click and the anchor.
    #[test]
    fn icon_off_every_display_is_not_clamped() {
        let displays = vec![Display {
            bounds: PointRect::new(-243.0, -1080.0, 1920.0, 1080.0),
            work_area: PointRect::new(-243.0, -1080.0, 1920.0, 1080.0),
            scale: 1.0,
        }];
        let got = anchor(PointRect::new(1880.0, -1080.0, 24.0, 30.0), &displays, None);
        assert_eq!(got.display, None);
        assert_eq!(got.resolution, Resolution::Unresolved);
        assert_eq!(got.position, (1712.0, -1044.0));
    }

    /// Two displays accepting the same candidate is reported, not hidden. Artificial geometry: a
    /// 1x display and a 2x display sharing the origin, where (610,10) and (305,5) both land inside.
    #[test]
    fn ambiguous_candidate_is_reported() {
        let displays = vec![
            Display {
                bounds: PointRect::new(0.0, 0.0, 2000.0, 1000.0),
                work_area: PointRect::new(0.0, 30.0, 2000.0, 970.0),
                scale: 1.0,
            },
            Display {
                bounds: PointRect::new(0.0, 0.0, 1000.0, 500.0),
                work_area: PointRect::new(0.0, 30.0, 1000.0, 470.0),
                scale: 2.0,
            },
        ];
        let got = anchor(PointRect::new(600.0, 0.0, 20.0, 20.0), &displays, None);
        assert_eq!(got.accepted, vec![0, 1]);
        assert_eq!(got.display, Some(0), "the first acceptor is used");
    }

    /// No display accepting **and no primary known**: the last resort — the rect is assumed to be
    /// points and nothing is clamped, with `Unresolved` for the caller to warn about. With a
    /// primary known the fallback clamps instead (test above).
    #[test]
    fn no_display_accepting_still_yields_a_position() {
        let got = anchor(PointRect::new(1286.0, 0.0, 24.0, 30.0), &[], None);
        assert_eq!(got.display, None);
        assert_eq!(got.accepted, Vec::<usize>::new());
        assert_eq!(got.resolution, Resolution::Unresolved);
        assert_eq!(got.position, (1118.0, 36.0));
    }

    /// `/code-review` finding 1's layout: a 2x primary hosting the menu bar, point bounds
    /// (0,0,1512,982), and a 1x display to its right, (1512,0,1920,1080) — **listed first**, as
    /// `CGGetActiveDisplayList` may. An icon at point x 1400 arrives as physical (2800,0,48,66);
    /// the centre is (2824,33), which lands inside the 2x display when divided by 2 (1412,16.5)
    /// *and* inside the 1x one when divided by 1. Both accept.
    ///
    /// Preferring the primary gives (1146,39): centred would be 1400 + 12 − 180 = 1232, but the
    /// panel would then end at 1592 on a 1512 pt display, so the clamp pulls it to
    /// 1512 − 360 − 6 = 1146; y = 33 + 6. (The review's write-up, and my first draft of this test,
    /// both stopped at 1232 and forgot the clamp — the code was right and the expectation was
    /// wrong.) Taking the first acceptor instead gives (2644,72): the wrong display, ~1244 pt from
    /// the icon, which is what the test below pins.
    #[test]
    fn ambiguity_prefers_the_primary_display() {
        let displays = ambiguous_pair();
        let got = anchor(PointRect::new(2800.0, 0.0, 48.0, 66.0), &displays, Some(1));
        assert_eq!(got.accepted, vec![0, 1], "both displays accept this rect");
        assert_eq!(got.display, Some(1), "the primary, not the first acceptor");
        assert_eq!(got.resolution, Resolution::PrimaryAmongAcceptors);
        assert_eq!(got.position, (1146.0, 39.0));
        assert_inside_work_area(&got, &displays);
    }

    /// The same layout with no primary known — which is what a per-display menu bar ("Displays have
    /// separate Spaces") or a failed `primary_monitor()` looks like. The first acceptor is used and
    /// still clamped, and the resolution says so, so the caller can warn: this is the pre-fix
    /// answer, kept as a pinned regression rather than a claim that it is right.
    #[test]
    fn ambiguity_without_a_primary_is_reported_and_still_clamped() {
        let displays = ambiguous_pair();
        let got = anchor(PointRect::new(2800.0, 0.0, 48.0, 66.0), &displays, None);
        assert_eq!(got.display, Some(0));
        assert_eq!(got.resolution, Resolution::AmbiguousWithoutPrimary);
        assert_eq!(got.position, (2644.0, 72.0));
        assert_inside_work_area(&got, &displays);
    }

    /// `/code-review` finding 2: nothing accepts, because the display list changed between the
    /// click and the anchor — here a 2x primary narrowed to 1280 pt while a rect at physical
    /// x 2800 was in flight, so (2800+24)/2 = 1412 > 1280 is rejected. The primary's scale and work
    /// area are used and the panel is clamped: 1400 + 12 − 180 = 1232, clamped to
    /// 1280 − 360 − 6 = 914. Assuming points and skipping the clamp gave (2644,72) — off screen.
    #[test]
    fn no_acceptor_falls_back_to_the_primary_and_clamps() {
        let displays = vec![Display {
            bounds: PointRect::new(0.0, 0.0, 1280.0, 982.0),
            work_area: PointRect::new(0.0, 33.0, 1280.0, 949.0),
            scale: 2.0,
        }];
        let got = anchor(PointRect::new(2800.0, 0.0, 48.0, 66.0), &displays, Some(0));
        assert_eq!(got.accepted, Vec::<usize>::new());
        assert_eq!(got.resolution, Resolution::PrimaryFallback);
        assert_eq!(got.position, (914.0, 39.0));
        assert_inside_work_area(&got, &displays);
    }

    /// A panel wider than the work area pins to the left margin instead of panicking, which
    /// `f64::clamp` would do with min > max.
    #[test]
    fn panel_wider_than_area_pins_left_without_panicking() {
        let area = PointRect::new(0.0, 30.0, 300.0, 900.0);
        let got = clamp_into((100.0, 36.0), panel(), area);
        assert_eq!(got.0, EDGE_MARGIN);
    }

    /// Decision D1's number, measured: on the ANMITE the expanded panel is **598 pt** tall —
    /// `min(720, (30 + 610 − 6) − (30 + 6))` — capped, expandable, landing at (226,36) with its
    /// bottom at 634 on a 640 pt display (`run-02-p4.log:32,40`). A cap that forgot the bottom
    /// margin reads 604, one that forgot the gap 604, one that forgot both 610; one measured from
    /// the display's frame instead of its work area reads 628.
    #[test]
    fn cap_on_the_anmite_is_the_measured_598() {
        let displays = menubar_on_anmite();
        let got = layout(anmite_tray(), PanelHeight::Expanded, &displays, Some(0));
        assert_eq!(got.state, PanelHeight::Expanded);
        assert_eq!(got.size, (360.0, 598.0));
        assert!(got.capped, "598 < 720 is a cap");
        assert!(got.expandable, "598 > 420 clears the floor");
        assert_eq!(got.anchored.position, (226.0, 36.0));
        assert_eq!(got.anchored.position.1 + got.size.1, 634.0);
        assert_size_inside_work_area(&got.anchored, got.size, &displays);
    }

    /// Where the nominal height fits — the built-in hosting the menu bar, `run-01-p5.log:38`: 39 +
    /// 720 = 759 ≤ 982 — the cap is idle: 720 pt, `capped=false`, the same (758,39) the collapsed
    /// panel gets. A cap applied where it is not needed, or an inverted floor comparison (which
    /// would refuse here, on every ordinary display), fails this.
    #[test]
    fn cap_is_idle_where_720_fits() {
        let displays = menubar_on_builtin();
        let got = layout(
            PointRect::new(1852.0, 0.0, 48.0, 66.0),
            PanelHeight::Expanded,
            &displays,
            Some(0),
        );
        assert_eq!(got.state, PanelHeight::Expanded);
        assert_eq!(got.size, (360.0, 720.0));
        assert!(!got.capped);
        assert!(got.expandable);
        assert_eq!(got.anchored.position, (758.0, 39.0));
        assert_size_inside_work_area(&got.anchored, got.size, &displays);
    }

    /// The readout the cap is judged by: with the cap the panel's top sits `TRAY_GAP` below the
    /// icon (the clamp was idle); the nominal 720 through the same placement is pinned to the
    /// work-area top by `clamp_into` — gap 0 — and still ends 110 pt below the display (P4 arm 1,
    /// `run-02-p4.log:26`: `frame_tl=[226,30 360x720]`, bottom 750 on 640). Executed here rather
    /// than described, so the two numbers cannot drift apart from the code.
    #[test]
    fn cap_keeps_the_clamp_idle() {
        let displays = menubar_on_anmite();
        let icon_bottom = 60.0 / 2.0;
        let capped = layout(anmite_tray(), PanelHeight::Expanded, &displays, Some(0));
        assert_eq!(capped.anchored.position.1 - icon_bottom, TRAY_GAP);

        let uncapped = place(
            anmite_tray(),
            (PANEL_WIDTH, EXPANDED_HEIGHT_NOMINAL),
            Some(0),
            &displays,
        );
        assert_eq!(uncapped.1 - icon_bottom, 0.0, "the clamp fired");
        assert_eq!(uncapped.1 + EXPANDED_HEIGHT_NOMINAL, 750.0);
    }

    /// Decision D1's provisional floor, refusing side, driven from a synthetic display because no
    /// attached one can reach it (`run-02-p4.log:32`: `refuse_expand=false` on the shortest). A
    /// display where the capped height would *equal* the collapsed height refuses: `Collapsed` is
    /// laid out, `expandable=false`, and the collapsed layout reports the same flag for the
    /// control. Split from the expanding side below (review finding 2): one function asserting
    /// both sides fails identically for an inversion and for an off-by-one, and the failure should
    /// say which. An inverted comparison fails `cap_is_idle_where_720_fits` instead — it would
    /// refuse on every ordinary display.
    #[test]
    fn refuses_when_the_cap_would_not_exceed_collapsed() {
        let at_floor = short_display(462.0);
        let refused = layout(
            short_display_tray(),
            PanelHeight::Expanded,
            &at_floor,
            Some(0),
        );
        assert!(!refused.expandable);
        assert_eq!(refused.state, PanelHeight::Collapsed);
        assert_eq!(refused.size, (360.0, 420.0));
        assert!(
            !refused.capped,
            "a refused layout is the collapsed one, not a capped one"
        );
        let collapsed = layout(
            short_display_tray(),
            PanelHeight::Collapsed,
            &at_floor,
            Some(0),
        );
        assert!(
            !collapsed.expandable,
            "the collapsed layout carries the flag for the control"
        );
        assert_eq!(collapsed.size, (360.0, 420.0));
    }

    /// The floor's expanding side: one point taller than the refusing display above, and the panel
    /// expands — to 421 pt, capped. A `>=` where `>` belongs fails the refusing test; a cap that is
    /// off by one in the other direction fails this one.
    #[test]
    fn expands_one_point_above_the_floor() {
        let just_over = short_display(463.0);
        let allowed = layout(
            short_display_tray(),
            PanelHeight::Expanded,
            &just_over,
            Some(0),
        );
        assert!(allowed.expandable);
        assert_eq!(allowed.state, PanelHeight::Expanded);
        assert_eq!(allowed.size, (360.0, 421.0));
        assert!(allowed.capped);
        assert_size_inside_work_area(&allowed.anchored, allowed.size, &just_over);
    }

    fn any_rect() -> Rect {
        Rect {
            position: Position::Physical(tauri::PhysicalPosition::new(0, 0)),
            size: Size::Physical(tauri::PhysicalSize::new(1, 1)),
        }
    }

    fn request(rt: &mut RoundTrip, kind: LayoutKind) -> u32 {
        rt.request(
            kind,
            (0.0, 0.0),
            (PANEL_WIDTH, COLLAPSED_HEIGHT),
            any_rect(),
        )
    }

    /// A commit for a generation that is not the pending one applies nothing — here the page
    /// reports the layout it was given *before* the latest request. Without the generation check
    /// the stale report would apply the newer layout early, or apply it twice.
    #[test]
    fn stale_commit_is_a_no_op() {
        let mut rt = RoundTrip::default();
        let first = request(&mut rt, LayoutKind::Resize);
        let second = request(&mut rt, LayoutKind::Resize);
        assert_ne!(first, second);
        assert!(rt.complete(first).is_none(), "the superseded generation");
        assert!(
            rt.complete(second + 1).is_none(),
            "a generation never issued"
        );
        let done = rt
            .complete(second)
            .expect("the pending generation completes");
        assert_eq!(done.generation, second);
    }

    /// A newer request replaces the pending one: only the newest completes, exactly once.
    #[test]
    fn a_newer_request_supersedes() {
        let mut rt = RoundTrip::default();
        let g1 = request(&mut rt, LayoutKind::Show(ShowReason::Toggle));
        let g2 = request(&mut rt, LayoutKind::Show(ShowReason::About));
        assert!(rt.complete(g1).is_none());
        let done = rt.complete(g2).expect("the newest completes");
        assert_eq!(done.kind, LayoutKind::Show(ShowReason::About));
        assert!(rt.complete(g2).is_none(), "and only once");
    }

    /// A hide cancels the pending layout, so a commit that arrives after the hide — the page was
    /// slow, the user clicked away — cannot order the panel back in.
    #[test]
    fn hide_cancels_a_pending_layout() {
        let mut rt = RoundTrip::default();
        let g = request(&mut rt, LayoutKind::Show(ShowReason::Toggle));
        assert!(rt.cancel(), "there was something to cancel");
        assert!(rt.complete(g).is_none(), "the late commit is a no-op");
        assert!(!rt.cancel(), "nothing left to cancel");
    }

    /// `toggle`'s question: is a show pending? Yes between its request and its completion or
    /// cancellation; no for a pending resize, and no once the show completed.
    #[test]
    fn a_show_is_pending_only_between_request_and_completion() {
        fn show_pending(rt: &RoundTrip) -> bool {
            matches!(
                rt.pending,
                Some(Pending {
                    kind: LayoutKind::Show(_),
                    ..
                })
            )
        }
        let mut rt = RoundTrip::default();
        assert!(!show_pending(&rt));
        let g = request(&mut rt, LayoutKind::Show(ShowReason::Toggle));
        assert!(show_pending(&rt));
        rt.complete(g);
        assert!(!show_pending(&rt), "completed");
        request(&mut rt, LayoutKind::Resize);
        assert!(!show_pending(&rt), "a pending resize is not a pending show");
        let g = request(&mut rt, LayoutKind::Show(ShowReason::Reopen));
        assert!(rt.cancel());
        assert!(!show_pending(&rt), "cancelled");
        assert!(rt.complete(g).is_none());
    }

    /// The fallback and the page's commit race for the same generation; whichever runs first
    /// applies, the other is a no-op — so a slow page never produces a second apply.
    #[test]
    fn fallback_applies_once_and_the_late_commit_is_a_no_op() {
        let mut rt = RoundTrip::default();
        let g = request(&mut rt, LayoutKind::Resize);
        let by_fallback = rt
            .complete(g)
            .expect("the fallback completes the pending layout");
        assert_eq!(by_fallback.kind, LayoutKind::Resize);
        assert!(
            rt.complete(g).is_none(),
            "the page's late commit applies nothing"
        );
    }

    /// Decided 2026-09-21: the About pane shows at the collapsed height whatever the user chose,
    /// and the transport shows the choice. An inverted match, or one that ignores the view, fails.
    #[test]
    fn about_shows_collapsed_and_the_transport_shows_the_choice() {
        assert_eq!(
            show_height(PanelView::About, PanelHeight::Expanded),
            PanelHeight::Collapsed
        );
        assert_eq!(
            show_height(PanelView::About, PanelHeight::Collapsed),
            PanelHeight::Collapsed
        );
        assert_eq!(
            show_height(PanelView::Transport, PanelHeight::Expanded),
            PanelHeight::Expanded
        );
        assert_eq!(
            show_height(PanelView::Transport, PanelHeight::Collapsed),
            PanelHeight::Collapsed
        );
    }

    /// The choice survives an About show: expand (a resize), then About (a collapsed show), then
    /// Back — the transport lays out expanded again. A state that let the About show overwrite the
    /// choice — as `last.state` did before `chosen` existed — fails at the last assertion.
    #[test]
    fn the_choice_survives_an_about_show() {
        let displays = menubar_on_builtin();
        let tray = PointRect::new(1852.0, 0.0, 48.0, 66.0);
        let state = PanelState::default();
        assert_eq!(state.chosen(), PanelHeight::Collapsed);

        let expanded = layout(tray, PanelHeight::Expanded, &displays, Some(0));
        state.request(
            PanelView::Transport,
            &expanded,
            LayoutKind::Resize,
            any_rect(),
        );
        assert_eq!(state.chosen(), PanelHeight::Expanded);

        assert_eq!(
            state.wanted_height_for(PanelView::About),
            PanelHeight::Collapsed
        );
        let collapsed = layout(tray, PanelHeight::Collapsed, &displays, Some(0));
        let shown = state.request(
            PanelView::About,
            &collapsed,
            LayoutKind::Show(ShowReason::About),
            any_rect(),
        );
        assert_eq!(shown.state, PanelHeight::Collapsed);
        assert_eq!(shown.view, PanelView::About);
        assert_eq!(
            state.chosen(),
            PanelHeight::Expanded,
            "a show does not touch the choice"
        );

        state.set_view(PanelView::Transport);
        assert_eq!(
            state.wanted_height_for(PanelView::Transport),
            PanelHeight::Expanded
        );
    }

    /// A resize request is refused on the About pane before any layout is computed, and allowed
    /// on the transport — the Rust-side guard behind the page hiding its control.
    #[test]
    fn a_resize_is_refused_on_the_about_pane() {
        let state = PanelState::default();
        assert_eq!(state.resize_refusal(), None);
        state.set_view(PanelView::About);
        assert_eq!(state.resize_refusal(), Some("view"));
        state.set_view(PanelView::Transport);
        assert_eq!(state.resize_refusal(), None);
    }

    /// Top-left points → Cocoa frame, pinned by the P3 measurement: anchor (758,39), 360×720, on the
    /// built-in whose frame is 982 pt tall → `[758,223 360x720]` (`run-11-p3-keepopen.log:22`).
    /// A dropped `− h` gives y = 943; a flipped sign gives −223; using the panel's own display's
    /// height instead of the menu-bar screen's gives a different y on every other display.
    #[test]
    fn cocoa_frame_matches_the_measured_p3_frame() {
        assert_eq!(
            cocoa_frame((758.0, 39.0), (360.0, 720.0), 982.0),
            (758.0, 223.0, 360.0, 720.0)
        );
    }

    /// The same conversion where the panel runs off the bottom: P4 arm 1, the 720 pt panel pinned
    /// to the 640 pt ANMITE's work-area top at (226,30) → Cocoa `[226,-110 360x720]`
    /// (`run-02-p4.log:26`). Negative Cocoa y is legitimate — it is how "110 pt off the bottom of
    /// the display" reads in that space.
    #[test]
    fn cocoa_frame_can_run_below_the_menu_bar_screen() {
        assert_eq!(
            cocoa_frame((226.0, 30.0), (360.0, 720.0), 640.0),
            (226.0, -110.0, 360.0, 720.0)
        );
    }

    /// The radius the effect view is rounded to and the radius the page clips itself to are the
    /// same measurement written in two places — `PANEL_CORNER_RADIUS` here and `--radius-panel`
    /// in `tokens.css` (CSS px are points inside the webview). Read the stylesheet at test time
    /// so the agreement is executed rather than asserted in a comment: change either number
    /// alone and this fails. (`include_str!` also makes the stylesheet a compile-time input of
    /// this crate, which is the intended coupling.)
    #[test]
    fn tokens_css_panel_radius_matches_the_effects_radius() {
        const TOKENS: &str = include_str!("../../src/styles/tokens.css");
        let declaration = TOKENS
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("--radius-panel:"))
            .expect("tokens.css declares --radius-panel");
        let value = declaration
            .trim_start_matches("--radius-panel:")
            .trim()
            .trim_end_matches(';')
            .trim();
        let px = value
            .strip_suffix("px")
            .unwrap_or_else(|| panic!("--radius-panel should be in px, found `{value}`"));
        let token: f64 = px
            .parse()
            .unwrap_or_else(|_| panic!("--radius-panel should be a number, found `{value}`"));
        assert_eq!(
            token, PANEL_CORNER_RADIUS,
            "tokens.css --radius-panel is {value} but PANEL_CORNER_RADIUS is {PANEL_CORNER_RADIUS}"
        );
    }
}
