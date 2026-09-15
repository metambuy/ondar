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
    ActivationPolicy, App, AppHandle, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Rect,
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

/// Label of the popover window.
///
/// Deliberately absent from `capabilities/default.json` (which, being JSON, cannot say so
/// itself): `panel.html` invokes nothing, so it needs no permissions. Widening the capability
/// to this window is a real permission decision for when the popover gains commands.
const PANEL_LABEL: &str = "panel";

const PANEL_SIZE: LogicalSize<f64> = LogicalSize::new(360.0, 420.0);

/// Physical pixels between the bottom edge of the tray icon and the top edge of the panel.
const TRAY_GAP: f64 = 6.0;

/// Physical pixels kept between the panel and the edge of the display's visible frame when the
/// panel has to be pushed back on screen. Equal to `TRAY_GAP` so the gaps read as one spacing —
/// an aesthetic choice, not a measured one.
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
    let on_event = panel.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Focused(false) = event {
            log::info!(
                "panel resigned key -> hide visible_before={}",
                on_event.is_visible()
            );
            on_event.hide();
        }
    });

    Ok(())
}

/// Tray click: hide if shown, otherwise anchor under the tray icon and show as key.
pub fn toggle<R: Runtime>(handle: &AppHandle<R>, rect: Rect) -> tauri::Result<()> {
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
        log::info!("panel toggle -> hide");
        panel.hide();
        return Ok(());
    }

    window.set_position(anchor(&window, rect)?)?;
    panel.show();
    // `show()` is `orderFrontRegardless` alone, and a panel that is never key can never resign
    // key. Not `show_and_make_key()`: that also makes the content view (`WryWebViewParent`)
    // first responder, taking it from the WKWebView.
    panel.make_key_window();

    let ns = panel.as_panel();
    log::info!(
        "panel shown class={} key={}",
        ns.class().name().to_string_lossy(),
        ns.isKeyWindow()
    );
    log_settled_occlusion(handle, &panel);
    Ok(())
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

/// Where the panel goes for a tray click: centred under the icon, then pushed inside the
/// visible frame of the display the icon is on.
///
/// `rect.position` is already the tray icon's **top-left corner in top-left-origin physical
/// pixels**: `tray-icon`'s `get_tray_rect` flips macOS's bottom-left origin and subtracts the
/// icon height before handing the value over (tray-icon 0.24.2 `platform_impl/macos/mod.rs:515`),
/// which is the convention `PhysicalPosition` already uses. So there is no flip here and no unit
/// conversion. Measured once, 2026-09-12: `(1932, 0)` `48x66` on a 3024x1964 display.
///
/// Known upstream limitation: that flip uses the *main* display's height, not the height of the
/// display the icon is on, so for a status item on a non-main display that is not top-aligned
/// with the main one, `y` is wrong before it reaches us.
///
/// Known limitation of ours, untested: `rect` is "physical" at the *status item's* display scale,
/// but everything below uses the *panel window's* scale — the point lookup for
/// `monitor_from_point`, and `set_position`'s conversion back — while `work_area` is at the chosen
/// monitor's scale. Invisible when all agree. This machine has a 1x display beside the 2x
/// built-in, where they would not (ONDAR.md, "Multi-monitor caveat"). The log line prints the
/// chosen display's work area beside the result so one click shows where it landed.
fn anchor<R: Runtime>(
    window: &WebviewWindow<R>,
    rect: Rect,
) -> tauri::Result<PhysicalPosition<f64>> {
    // The rect arrives as `Position::Physical`/`Size::Physical`, so the scale factor is
    // ignored; passing the real one keeps this correct if that ever changes.
    let scale = window.scale_factor()?;
    let tray_pos = rect.position.to_physical::<f64>(scale);
    let tray_size = rect.size.to_physical::<f64>(scale);

    // tao implements `outer_size` as `NSWindow::frame`, real from creation and unaffected by
    // visibility. Not safe straight after a programmatic resize (`set_inner_size` is async) —
    // M2b's problem, not this one's.
    let panel_size = window.outer_size()?.cast::<f64>();

    let centred = centred_below(tray_pos, tray_size, panel_size);

    // `monitor_from_point` tests against `CGDisplayBounds`, which is in *points* (tao 0.35.3
    // `platform_impl/macos/monitor.rs:163-170`), so the icon centre is passed in logical units.
    let centre_x = (tray_pos.x + tray_size.width / 2.0) / scale;
    let centre_y = (tray_pos.y + tray_size.height / 2.0) / scale;
    let monitor = match window.monitor_from_point(centre_x, centre_y)? {
        Some(m) => Some(m),
        None => window.primary_monitor()?,
    };

    // `work_area` is `NSScreen.visibleFrame` — excluding the menu bar and Dock — converted to
    // top-left-origin physical pixels (tauri-runtime-wry 2.11.4 `src/monitor/macos.rs:8-28`).
    let position = match &monitor {
        Some(m) => {
            let area = m.work_area();
            clamp_into(
                centred,
                panel_size,
                PhysicalPosition::new(f64::from(area.position.x), f64::from(area.position.y)),
                area.size.cast::<f64>(),
            )
        }
        None => centred,
    };

    let work_area = monitor
        .as_ref()
        .map(|m| format!("{:?}", m.work_area()))
        .unwrap_or_else(|| "none".into());
    log::info!(
        "panel anchor scale={scale} tray_pos={tray_pos:?} tray_size={tray_size:?} \
         panel={panel_size:?} centred={centred:?} position={position:?} work_area={work_area}"
    );

    Ok(position)
}

/// Centre the panel horizontally on the tray icon, top edge `TRAY_GAP` below the icon's bottom.
///
/// `y` grows downward, so the icon's height is *added*: getting that sign backwards is the
/// classic "panel appears above the menu bar / at the bottom of the screen" bug.
fn centred_below(
    tray_pos: PhysicalPosition<f64>,
    tray_size: PhysicalSize<f64>,
    panel_size: PhysicalSize<f64>,
) -> PhysicalPosition<f64> {
    PhysicalPosition::new(
        tray_pos.x + tray_size.width / 2.0 - panel_size.width / 2.0,
        tray_pos.y + tray_size.height + TRAY_GAP,
    )
}

/// Push a panel position inside an area, keeping `EDGE_MARGIN` from its left and right edges.
///
/// Menu bar items live at the right edge of the display, so a centred panel runs off it once
/// the icon is within half a panel width of the edge: on a 3024 px display with a 720 px panel,
/// past x ≈ 2640. Clamping rather than flipping to right-aligned keeps the panel centred on the
/// icon whenever it fits and slides it only as far as needed, which is how the system's own
/// status item menus behave.
///
/// Vertically, the panel is only pulled up if it would run past the bottom, and never above
/// the area's top. Written without `f64::clamp`, which panics when min > max — a panel wider
/// than the area would do that; here it pins to the left margin instead.
fn clamp_into(
    position: PhysicalPosition<f64>,
    panel_size: PhysicalSize<f64>,
    area_pos: PhysicalPosition<f64>,
    area_size: PhysicalSize<f64>,
) -> PhysicalPosition<f64> {
    let right = area_pos.x + area_size.width - panel_size.width - EDGE_MARGIN;
    let left = area_pos.x + EDGE_MARGIN;
    let bottom = area_pos.y + area_size.height - panel_size.height;
    PhysicalPosition::new(
        position.x.min(right).max(left),
        position.y.min(bottom).max(area_pos.y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: f64 = 2.0;

    /// The tray rect measured on 2026-09-12 (`_handover/m2-spike-app.log`): a 24x33 pt status
    /// item at physical (1932, 0) on a 3024x1964 display.
    fn measured_tray() -> (PhysicalPosition<f64>, PhysicalSize<f64>) {
        (
            PhysicalPosition::new(1932.0, 0.0),
            PhysicalSize::new(48.0, 66.0),
        )
    }

    fn panel() -> PhysicalSize<f64> {
        PANEL_SIZE.to_physical(SCALE)
    }

    /// Display with a 33 pt menu bar; the visible frame starts below it.
    fn work_area() -> (PhysicalPosition<f64>, PhysicalSize<f64>) {
        (
            PhysicalPosition::new(0.0, 66.0),
            PhysicalSize::new(3024.0, 1898.0),
        )
    }

    /// Fixture: (1932, 0) 48x66, panel 360x420 logical at scale 2 → (1596, 72).
    /// x = 1932 + 48/2 − 720/2 = 1596; y = 0 + 66 + 6 = 72.
    #[test]
    fn measured_rect_centres_panel_under_icon() {
        let (pos, size) = measured_tray();
        let got = centred_below(pos, size, panel());
        assert_eq!(got, PhysicalPosition::new(1596.0, 72.0));
    }

    /// Fails if either `y` term changes sign. With the measured rect `tray_pos.y` is 0, so a
    /// flipped `tray_pos.y` cannot show there; a non-zero icon `y` is needed as well.
    ///   `y - height - gap` gives −72 (fixture) / 28 (y = 100)
    ///   `−y + height + gap` gives 72 (fixture, indistinguishable) / −28 (y = 100)
    #[test]
    fn panel_hangs_below_icon_for_any_icon_y() {
        let (pos, size) = measured_tray();
        assert_eq!(centred_below(pos, size, panel()).y, 72.0);

        let lower = PhysicalPosition::new(pos.x, 100.0);
        assert_eq!(centred_below(lower, size, panel()).y, 172.0);
    }

    /// Fails if the centring is dropped or half of it is.
    ///   no centring (`x = tray x`)         gives 1932
    ///   icon half-width dropped            gives 1572
    ///   panel half-width dropped           gives 1956
    #[test]
    fn panel_is_centred_not_left_aligned() {
        let (pos, size) = measured_tray();
        let got = centred_below(pos, size, panel());
        assert_eq!(got.x, 1596.0);
        assert_eq!(got.x + panel().width / 2.0, pos.x + size.width / 2.0);
    }

    /// The measured rect fits, so clamping must not move it.
    #[test]
    fn clamp_leaves_a_fitting_panel_alone() {
        let (pos, size) = measured_tray();
        let (area_pos, area_size) = work_area();
        let centred = centred_below(pos, size, panel());
        assert_eq!(clamp_into(centred, panel(), area_pos, area_size), centred);
    }

    /// Icon near the right edge: centred x = 2950 + 24 − 360 = 2614, right edge 3334 > 3024.
    /// Clamped x = 3024 − 720 − 6 = 2298. Fails without the clamp (2614) or with the margin
    /// dropped (2304).
    #[test]
    fn right_edge_icon_is_pulled_back_on_screen() {
        let pos = PhysicalPosition::new(2950.0, 0.0);
        let size = PhysicalSize::new(48.0, 66.0);
        let (area_pos, area_size) = work_area();
        let centred = centred_below(pos, size, panel());
        assert_eq!(centred.x, 2614.0);

        let got = clamp_into(centred, panel(), area_pos, area_size);
        assert_eq!(got, PhysicalPosition::new(2298.0, 72.0));
        assert!(got.x + panel().width <= area_pos.x + area_size.width - EDGE_MARGIN);
    }

    /// A panel wider than the area pins to the left margin instead of panicking, which
    /// `f64::clamp` would do with min > max.
    #[test]
    fn panel_wider_than_area_pins_left_without_panicking() {
        let got = clamp_into(
            PhysicalPosition::new(500.0, 72.0),
            panel(),
            PhysicalPosition::new(0.0, 66.0),
            PhysicalSize::new(600.0, 1898.0),
        );
        assert_eq!(got.x, EDGE_MARGIN);
    }
}
