//! The popover: a non-activating `NSPanel` anchored under the tray icon, dismissed when it
//! resigns key.
//!
//! Two traps in `tauri-nspanel` shape this file (verified against rev c9ec213, ONDAR.md):
//! - `Panel::to_window()` is a *conversion back*: it removes the panel from the plugin store,
//!   clears the delegate and resets the NSWindow's class to `TaoWindow`. It is never called
//!   here. The Tauri window is reached through `get_webview_window`, which touches neither.
//! - `PanelBuilder::no_activate(true)` does not make the panel non-activating; it only swaps
//!   the activation policy around window creation. That is the style mask's job.

use std::time::{Duration, Instant};

use tauri::{
    ActivationPolicy, App, AppHandle, LogicalPosition, LogicalSize, Manager, Position, Rect,
    Runtime, Size, WebviewUrl, WebviewWindow, WindowEvent,
    window::{Effect, EffectState, EffectsBuilder},
};
use tauri_nspanel::{
    ManagerExt,
    PanelBuilder,
    PanelHandle,
    PanelLevel,
    // `doc(hidden)` re-exports; used instead of a direct `objc2-app-kit` dependency so the
    // version can never diverge from the one `tauri-nspanel` links.
    objc2_app_kit::{NSWindowOcclusionState, NSWindowStyleMask},
    tauri_panel,
};

/// Label of the popover window — the only window, and the one `capabilities/default.json` is
/// scoped to since the M1 bench window retired (M2c). The page invokes, so the capability is a
/// real permission decision: `core:default` (events, app name/version) and nothing else.
/// App-defined commands need no ACL entry from a local page (tauri 2.11.5
/// `webview/mod.rs:1823`: the ACL gates plugin commands, a declared app manifest, and remote
/// origins).
const PANEL_LABEL: &str = "panel";

const PANEL_SIZE: LogicalSize<f64> = LogicalSize::new(360.0, 420.0);

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

tauri_panel! {
    panel!(OndarPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
}

/// Build the popover, hidden. Call from `setup`, before `tray::setup`.
pub fn setup(app: &mut App) -> tauri::Result<()> {
    // Must precede `PanelBuilder::build()`. With `no_activate(true)` the builder forces
    // `Prohibited` around window creation and afterwards restores whatever the policy was
    // *before* — so setting Accessory later would be undone and the Dock icon would return.
    app.set_activation_policy(ActivationPolicy::Accessory);

    let panel = PanelBuilder::<_, OndarPanel<_>>::new(app.handle(), PANEL_LABEL)
        .url(WebviewUrl::App("panel.html".into()))
        .size(Size::Logical(PANEL_SIZE))
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
}

impl HideReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::ResignKey => "resign_key",
            Self::Esc => "esc",
        }
    }
}

/// Why the popover is being shown. Logged like [`HideReason`].
#[derive(Clone, Copy, Debug)]
pub enum ShowReason {
    /// The tray icon was clicked while the popover was hidden.
    Toggle,
}

impl ShowReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
        }
    }
}

/// **The one hide path.** Every close goes through here — the tray toggle and resign-key today,
/// and whatever M2c adds — so that when a hide gains side effects (M2d collapses the expanded
/// state) they run once per close, not once per caller.
///
/// Always executed on the main thread. `PanelHandle` is `Send`, but its methods are bare
/// `msg_send!` with no dispatch of their own (tauri-nspanel c9ec213 `panel.rs:14-17`: "all actual
/// panel operations must be performed on the main thread"). `run_on_main_thread` runs the closure
/// inline when already on main (tauri-runtime-wry 2.11.4 `lib.rs:235-255`) and posts it otherwise,
/// so every caller gets the same path, and the log line records which thread it ran on.
///
/// Hiding an already-hidden panel is a logged no-op — `effective=false` — never silence.
pub fn hide<R: Runtime>(handle: &AppHandle<R>, reason: HideReason) {
    let on_main = handle.clone();
    let queued = handle.run_on_main_thread(move || {
        let Ok(panel) = on_main.get_webview_panel(PANEL_LABEL) else {
            log::warn!(
                "panel hide reason={} panel not in the tauri-nspanel store",
                reason.as_str()
            );
            return;
        };
        let effective = panel.is_visible();
        // `orderOut:` on a window that is already out is itself a no-op, so this is called
        // unconditionally: the log line is what distinguishes the two cases, not a branch.
        panel.hide();
        log::info!(
            "panel hide reason={} effective={effective} thread={:?}",
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
    // Two distinct messages: the spike logged one `window not found` for two different failure
    // sites and the log could not say which had fired.
    let Ok(panel) = handle.get_webview_panel(PANEL_LABEL) else {
        log::warn!("panel not in the tauri-nspanel store");
        return Err(tauri::Error::WindowNotFound);
    };
    let Some(window) = handle.get_webview_window(PANEL_LABEL) else {
        log::warn!("panel has no tauri webview window");
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

    window.set_position(anchor(&window, rect)?)?;
    panel.show();
    // `show()` is `orderFrontRegardless` alone, and a panel that is never key can never resign
    // key. Not `show_and_make_key()`: that also makes the content view (`WryWebViewParent`)
    // first responder, taking it from the WKWebView — measured 2026-09-16: after plain
    // `make_key_window()` the first responder is the `WryWebView` at t+0 in every run.
    panel.make_key_window();

    let ns = panel.as_panel();
    log::info!(
        "panel show reason={} effective=true class={} key={} thread={:?}",
        reason.as_str(),
        ns.class().name().to_string_lossy(),
        ns.isKeyWindow(),
        std::thread::current().name()
    );
    log_settled_occlusion(handle, &panel);
    Ok(())
}

/// Tray click: hide if shown, otherwise show under the icon. Decides only; the ordering in and
/// out is [`hide`] and [`show_at`], so a tray close is one effective hide like every other close.
pub fn toggle<R: Runtime>(handle: &AppHandle<R>, rect: Rect) {
    let on_main = handle.clone();
    let queued = handle.run_on_main_thread(move || {
        let visible = on_main
            .get_webview_panel(PANEL_LABEL)
            .is_ok_and(|p| p.is_visible());
        if visible {
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
    let handle = handle.clone();
    let panel = panel.clone();
    let shown_at = Instant::now();
    let spawned = std::thread::Builder::new()
        .name("ondar-occlusion".into())
        .spawn(move || {
            std::thread::sleep(OCCLUSION_SETTLE);
            let queued = handle.run_on_main_thread(move || {
                let ns = panel.as_panel();
                let state = ns.occlusionState();
                log::info!(
                    "panel occlusion settled_read_after_ms={} settled_visible={} settled_raw={} key={}",
                    shown_at.elapsed().as_millis(),
                    state.contains(NSWindowOcclusionState::Visible),
                    state.0,
                    ns.isKeyWindow()
                );
            });
            if let Err(e) = queued {
                log::warn!("panel occlusion read not queued: {e}");
            }
        });
    if let Err(e) = spawned {
        log::warn!("panel occlusion thread not started: {e}");
    }
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

/// Where the panel goes for a tray click: centred under the icon, then pushed inside the work area
/// of the display the icon is on. **Pure, and entirely in points.**
///
/// `tray` arrives exactly as `TrayIconEvent::Click` gives it: global points multiplied by the
/// *status item display's* scale (tray-icon 0.24.2 `platform_impl/macos/mod.rs:515-528`), top-left
/// origin. `panel_physical` is the panel window's `outer_size` and `panel_scale` its own
/// `scale_factor`, and **the division that turns them into points happens here**: that division is
/// the quantity this whole change is about, and while it lived in the caller no test could reach it
/// (`/code-review` finding 3, 2026-09-16).
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
fn anchor_points(
    tray: PointRect,
    panel_physical: (f64, f64),
    panel_scale: f64,
    displays: &[Display],
    primary: Option<usize>,
) -> Anchored {
    // Points, from the panel window's own scale — correct for its own size, and for nothing else.
    let panel = (
        panel_physical.0 / panel_scale,
        panel_physical.1 / panel_scale,
    );

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
    let scale = display.map_or(1.0, |i| displays[i].scale);
    let tray = PointRect::new(
        tray.x / scale,
        tray.y / scale,
        tray.width / scale,
        tray.height / scale,
    );

    let position = centred_below(tray, panel);
    let position = match display {
        Some(i) => clamp_into(position, panel, displays[i].work_area),
        None => position,
    };

    Anchored {
        position,
        display,
        accepted,
        resolution,
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

/// Gather the live state, anchor in points, and log what was chosen.
///
/// The returned position is a `LogicalPosition`, i.e. points: `set_position` then hands it to tao,
/// whose `set_outer_position` does `position.to_logical(scale)` (`window.rs:728-734`) — the
/// identity on a logical value. Nothing in the path divides by the panel window's own scale, which
/// is what made the old code depend on which display the panel happened to be sitting on.
fn anchor<R: Runtime>(
    window: &WebviewWindow<R>,
    rect: Rect,
) -> tauri::Result<LogicalPosition<f64>> {
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

    // Handed to `anchor_points` unconverted: the division into points happens there, where a test
    // can reach it.
    let panel_scale = window.scale_factor()?;
    let outer = window.outer_size()?;
    let panel_physical = (f64::from(outer.width), f64::from(outer.height));

    // Each monitor converted by its *own* scale, which is the only conversion that is correct for
    // it (see `anchor_points`).
    let displays: Vec<Display> = window
        .available_monitors()?
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
    // survivable: `anchor_points` then treats ambiguity as ambiguity.
    let primary = window.primary_monitor()?.and_then(|p| {
        window
            .available_monitors()
            .ok()?
            .iter()
            .position(|m| m.name() == p.name() && m.position() == p.position())
    });

    let anchored = anchor_points(tray, panel_physical, panel_scale, &displays, primary);

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
        "panel anchor tray_physical={tray:?} panel_physical={panel_physical:?} \
         panel_scale={panel_scale} display={:?} primary={primary:?} accepted={:?} \
         resolution={:?} position_points={:?} work_area={:?}",
        anchored.display,
        anchored.accepted,
        anchored.resolution,
        anchored.position,
        chosen.map(|d| d.work_area)
    );

    Ok(LogicalPosition::new(
        anchored.position.0,
        anchored.position.1,
    ))
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

    /// The panel's logical size, and the same size as `outer_size` would report it on a display of
    /// the given scale — the pair `anchor_points` now takes, so the division into points is under
    /// test.
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

    fn panel() -> (f64, f64) {
        (PANEL_SIZE.width, PANEL_SIZE.height)
    }

    fn panel_physical(scale: f64) -> (f64, f64) {
        (PANEL_SIZE.width * scale, PANEL_SIZE.height * scale)
    }

    /// The assertion the M2b gate settled on: the panel rect must lie inside the work area of the
    /// display it was placed on. Not "the right display" — on 2026-09-16 the forced mixed-scale
    /// case landed on the *correct* display, 649 pt from the icon and 12 pt inside the menu bar,
    /// so a display-identity assertion would have passed it.
    fn assert_inside_work_area(got: &Anchored, displays: &[Display]) {
        let i = got.display.expect("a display should have been chosen");
        let area = displays[i].work_area;
        let (x, y) = got.position;
        let (w, h) = panel();
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
    #[test]
    fn measured_1x_tray_rect_matches_the_observed_landing() {
        let displays = menubar_on_benq();
        let got = anchor_points(
            PointRect::new(1286.0, 0.0, 24.0, 30.0),
            panel_physical(1.0),
            1.0,
            &displays,
            Some(0),
        );
        assert_eq!(got.position, (1118.0, 36.0));
        assert_eq!(got.display, Some(0));
        assert_eq!(got.accepted, vec![0]);
        assert_inside_work_area(&got, &displays);
    }

    /// The forced mixed-scale case, measured 2026-09-16: icon on the 1x BenQ, panel window on the
    /// 2x built-in — so `outer_size` reports **720x840** and the window's own scale is 2, while the
    /// rect is 1x. The panel is 360x420 points either way, so the answer must be the same
    /// (1118,36) as the 1x test above, and not the (469,18) the old physical-space code produced
    /// (649 pt left of the icon, 12 pt inside the menu bar band).
    ///
    /// **This is the test that pins the panel-scale division**, which is why it takes the physical
    /// size and the scale separately: dropping the division gives (938,36) and multiplying instead
    /// of dividing gives (398,36). Until `/code-review` finding 3, the division lived in `anchor`
    /// and this test had inputs identical to the 1x one — it could not fail on its own.
    #[test]
    fn mixed_scale_does_not_change_the_answer() {
        let displays = menubar_on_benq();
        let got = anchor_points(
            PointRect::new(1286.0, 0.0, 24.0, 30.0),
            panel_physical(2.0),
            2.0,
            &displays,
            Some(0),
        );
        assert_eq!(got.accepted, vec![0], "only the 1x display hosts this icon");
        assert_eq!(got.position, (1118.0, 36.0));
        assert_inside_work_area(&got, &displays);
    }

    /// Measured 2026-09-16 with the menu bar on the 2x built-in: rect (1760,0) 48x66 = (880,0)
    /// 24x33 in points. x = 880 + 12 − 180 = 712, y = 0 + 33 + 6 = 39. (The landing measured that
    /// day was y = 36 with the old 6-physical-pixel gap, i.e. 3 pt; the gap is now 6 pt.)
    #[test]
    fn measured_2x_tray_rect_is_converted_by_its_own_display_scale() {
        let displays = menubar_on_builtin();
        let got = anchor_points(
            PointRect::new(1760.0, 0.0, 48.0, 66.0),
            panel_physical(2.0),
            2.0,
            &displays,
            Some(0),
        );
        assert_eq!(got.position, (712.0, 39.0));
        assert_inside_work_area(&got, &displays);
    }

    /// The 2026-09-12 fixture, now in point terms: rect (1932,0) 48x66 on the 2x built-in =
    /// (966,0) 24x33 points. x = 966 + 12 − 180 = 798, y = 33 + 6 = 39.
    #[test]
    fn the_2026_09_12_rect_still_centres_under_the_icon() {
        let displays = menubar_on_builtin();
        let got = anchor_points(
            PointRect::new(1932.0, 0.0, 48.0, 66.0),
            panel_physical(2.0),
            2.0,
            &displays,
            Some(0),
        );
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
        let got = anchor_points(
            PointRect::new(100.0, -1080.0, 24.0, 30.0),
            panel_physical(1.0),
            1.0,
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
        let got = anchor_points(
            PointRect::new(1600.0, -1080.0, 24.0, 30.0),
            panel_physical(1.0),
            1.0,
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
        let got = anchor_points(
            PointRect::new(5720.0, 880.0, 48.0, 60.0),
            panel_physical(2.0),
            2.0,
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
        let got = anchor_points(
            PointRect::new(988.0, 0.0, 24.0, 30.0),
            panel_physical(1.0),
            1.0,
            &displays,
            None,
        );
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
        let got = anchor_points(
            PointRect::new(1880.0, -1080.0, 24.0, 30.0),
            panel_physical(1.0),
            1.0,
            &displays,
            None,
        );
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
        let got = anchor_points(
            PointRect::new(600.0, 0.0, 20.0, 20.0),
            panel_physical(1.0),
            1.0,
            &displays,
            None,
        );
        assert_eq!(got.accepted, vec![0, 1]);
        assert_eq!(got.display, Some(0), "the first acceptor is used");
    }

    /// No display accepting **and no primary known**: the last resort — the rect is assumed to be
    /// points and nothing is clamped, with `Unresolved` for the caller to warn about. With a
    /// primary known the fallback clamps instead (test above).
    #[test]
    fn no_display_accepting_still_yields_a_position() {
        let got = anchor_points(
            PointRect::new(1286.0, 0.0, 24.0, 30.0),
            panel_physical(1.0),
            1.0,
            &[],
            None,
        );
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
        let got = anchor_points(
            PointRect::new(2800.0, 0.0, 48.0, 66.0),
            panel_physical(2.0),
            2.0,
            &displays,
            Some(1),
        );
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
        let got = anchor_points(
            PointRect::new(2800.0, 0.0, 48.0, 66.0),
            panel_physical(2.0),
            2.0,
            &displays,
            None,
        );
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
        let got = anchor_points(
            PointRect::new(2800.0, 0.0, 48.0, 66.0),
            panel_physical(2.0),
            2.0,
            &displays,
            Some(0),
        );
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
}
