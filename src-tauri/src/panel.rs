//! M2 spike: the menu bar tray icon and the non-activating NSPanel popover.
//!
//! Everything here is spike scaffolding on the `m2-spike` branch. It exists to answer one
//! question the published docs cannot — does `WebviewWindow::set_effects` still apply
//! vibrancy after `tauri-nspanel` subclasses the NSWindow? — and is rewritten, not extended,
//! at M2 proper.
//!
//! Vibrancy and tray positioning are independent questions, so they are testable
//! independently. `ONDA_SPIKE_SHOW_PANEL=1` shows the panel at the centre of the main
//! display at launch, with no tray click involved: same `PanelBuilder` config, same
//! `to_window()` + `set_effects`, same `panel.html`. A broken anchor calculation then cannot
//! masquerade as broken vibrancy.

use tauri::{
    ActivationPolicy, App, AppHandle, LogicalSize, Manager, PhysicalPosition, Rect, Runtime, Size,
    WebviewUrl, WebviewWindow,
    image::Image,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    window::{Effect, EffectState, EffectsBuilder},
};
use tauri_nspanel::{ManagerExt, PanelBuilder, PanelHandle, PanelLevel, tauri_panel};

/// Label of the popover window.
///
/// Deliberately absent from `capabilities/default.json`: `panel.html` invokes nothing, so it
/// needs no permissions, and a page that calls nothing has nothing to be denied. Widening
/// the capability to this window is a real permission decision for M2 proper.
const PANEL_LABEL: &str = "panel";

const PANEL_SIZE: LogicalSize<f64> = LogicalSize::new(360.0, 420.0);

/// Physical pixels between the bottom edge of the tray icon and the top edge of the panel.
const TRAY_GAP: f64 = 6.0;

/// Set to `1` to show the panel centred on the main display at launch, bypassing the tray
/// entirely. The vibrancy screenshot is taken this way so that positioning cannot confound it.
const SHOW_AT_LAUNCH: &str = "ONDA_SPIKE_SHOW_PANEL";

tauri_panel! {
    panel!(OndaPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
}

/// Build the tray icon and the popover. Call from `setup`.
pub fn setup(app: &mut App) -> tauri::Result<()> {
    // Must precede `PanelBuilder::build()`. With `no_activate(true)` the builder forces
    // `NSApplicationActivationPolicy::Prohibited` around window creation and afterwards
    // restores whatever the policy was *before* — so setting Accessory later would be undone
    // and the Dock icon would come back.
    app.set_activation_policy(ActivationPolicy::Accessory);

    let panel = PanelBuilder::<_, OndaPanel<_>>::new(app.handle(), PANEL_LABEL)
        .url(WebviewUrl::App("panel.html".into()))
        .size(Size::Logical(PANEL_SIZE))
        .level(PanelLevel::PopUpMenu)
        .floating(true)
        .no_activate(true)
        .hides_on_deactivate(true)
        .has_shadow(true)
        // Two different transparencies, and both are needed. This one is the NSWindow:
        // `set_transparent` sets `backgroundColor` to `clearColor` and `opaque` to false.
        .transparent(true)
        // ...and this one is the webview. wry decides the WKWebView's own opacity when it
        // creates it, so the window-level call above cannot reach it retroactively. Without
        // it the page renders on an opaque layer that hides the NSVisualEffectView.
        .with_window(|w| w.decorations(false).resizable(false).transparent(true))
        .build()?;

    // Hidden until the first tray click; `build()` leaves the window ordered in.
    panel.hide();

    // Vibrancy, applied after the conversion to a panel — the whole point of the spike.
    // `apply_effects` walks the effect list for a macOS variant and returns silently if it
    // finds none, so the screenshot, not this `?`, is what proves it worked.
    let window = panel_window(&panel)?;
    window.set_effects(
        EffectsBuilder::new()
            .effect(Effect::Popover)
            // `Active`, not `FollowsWindowActiveState`. Under an Accessory activation policy
            // with a non-activating panel, the app is never active and the panel is never
            // the key window, so "follows" resolves to permanently inactive — and an
            // inactive NSVisualEffectView paints nothing at all. The view is inserted, sized
            // correctly and reported present in the hierarchy, and the panel is still
            // invisible: the third way this can fail while looking like success.
            .state(EffectState::Active)
            .build(),
    )?;

    let handle = app.handle().clone();
    TrayIconBuilder::with_id("onda-tray")
        .icon(tray_icon())
        // macOS renders a template image from its alpha channel alone, tinting it to match a
        // light or dark menu bar and the highlight state.
        .icon_as_template(true)
        .on_tray_icon_event(move |_tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                rect,
                ..
            } = event
            {
                // Logged before the filter below, and on *every* `Click`, because the shape
                // of this log is what separates the three ways this path can fail:
                //   two lines per physical click  → press and release both arrived
                //   one line, coords off-display  → the anchor maths is wrong
                //   no line at all                → the handler is not on this event
                log::info!(
                    "tray click: button={button:?} state={button_state:?} rect.position={:?} rect.size={:?}",
                    rect.position,
                    rect.size
                );

                // `Click` fires on both press and release. Toggling on every one of them
                // shows the panel on press and hides it again on release, so nothing is ever
                // visible and there is no flicker to notice — a failure that looks like a
                // handler that never ran. Deleting this filter silently resurrects it.
                if button == MouseButton::Left
                    && button_state == MouseButtonState::Up
                    && let Err(e) = toggle(&handle, rect)
                {
                    log::warn!("tray toggle failed: {e}");
                }
            }
        })
        .build(app)?;

    if std::env::var(SHOW_AT_LAUNCH).is_ok_and(|v| v == "1") {
        show_centred(&panel, &window)?;
        watch(panel, window);
    }

    Ok(())
}

/// Re-log the panel's state every two seconds while the spike screenshot is being taken.
///
/// A single line at launch cannot distinguish "never rendered" from "rendered, then hidden
/// again", and `hides_on_deactivate` makes the second one likely. This also reports the view
/// tree, which is the spike's real question: `apply_effects` inserts an
/// `NSVisualEffectViewTagged` tagged 91376254, so its presence or absence in the log answers
/// "did vibrancy apply" without anyone having to look at a picture.
fn watch<R: Runtime>(panel: PanelHandle<R>, window: WebviewWindow<R>) {
    let spawned = std::thread::Builder::new()
        .name("onda-spike-watch".into())
        .spawn(move || {
            for tick in 0..20u32 {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let panel = panel.clone();
                let win = window.clone();
                let queued = window.run_on_main_thread(move || {
                    let position = match win.outer_position() {
                        Ok(p) => format!("{p:?}"),
                        Err(e) => format!("err({e})"),
                    };
                    log::info!(
                        "spike watch {tick}: visible={} position={position} {} views={}",
                        panel.is_visible(),
                        describe_window(&panel),
                        describe_view_tree(&panel)
                    );
                });
                if queued.is_err() {
                    break;
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("spike watch thread not started: {e}");
    }
}

/// The NSWindow-level state that decides whether a window that reports `isVisible == true`
/// actually reaches the screen. `on_active_space` is the one that catches a panel stranded on
/// the Space it was created on — a floating panel does not follow the user between Spaces
/// unless its collection behaviour says it may. Must run on the main thread.
fn describe_window<R: Runtime>(panel: &PanelHandle<R>) -> String {
    let w = panel.as_panel();
    format!(
        "alpha={:.2} opaque={} key={} level={} on_active_space={} occlusion={:?}",
        w.alphaValue(),
        w.isOpaque(),
        w.isKeyWindow(),
        w.level(),
        w.isOnActiveSpace(),
        w.occlusionState()
    )
}

/// The tag `window-vibrancy` stamps on the `NSVisualEffectView` it inserts
/// (`window_vibrancy::macos::internal::NS_VIEW_TAG_BLUR_VIEW`). Finding it in the tree is
/// proof the effect was applied to a subclassed panel window.
const NS_VIEW_TAG_BLUR_VIEW: isize = 91_376_254;

/// One-line dump of the panel content view, its siblings and its children: class names plus
/// `tag` for each. Must run on the main thread.
fn describe_view_tree<R: Runtime>(panel: &PanelHandle<R>) -> String {
    use tauri_nspanel::objc2_app_kit::NSView;

    fn describe(view: &NSView) -> String {
        let tag = view.tag();
        let name = view.class().name().to_string_lossy().into_owned();
        // The frame matters as much as the presence: `apply_vibrancy` sizes the effect view
        // from the host view's bounds *at the moment it is called*, so an effect applied
        // before layout inserts a real view that paints nothing.
        let f = view.frame();
        let geom = format!(
            "@{:.0},{:.0} {:.0}x{:.0}",
            f.origin.x, f.origin.y, f.size.width, f.size.height
        );
        if tag == NS_VIEW_TAG_BLUR_VIEW {
            format!("{name}#BLUR{geom}")
        } else if tag == 0 {
            format!("{name}{geom}")
        } else {
            format!("{name}#{tag}{geom}")
        }
    }

    fn children(view: &NSView) -> String {
        let subviews = view.subviews();
        let listed: Vec<String> = subviews.iter().map(|v| describe(&v)).collect();
        format!("{}[{}]", describe(view), listed.join(", "))
    }

    let content = panel.content_view();
    // SAFETY: called inside `run_on_main_thread`, and `superview` only reads an AppKit
    // pointer that the window owns for as long as `content` is retained.
    match unsafe { content.superview() } {
        Some(parent) => {
            let siblings: Vec<String> = parent.subviews().iter().map(|v| children(&v)).collect();
            format!("{}<{}>", describe(&parent), siblings.join(" | "))
        }
        None => children(&content),
    }
}

/// Show the panel centred on the main display, with the tray out of the picture.
fn show_centred<R: Runtime>(
    panel: &PanelHandle<R>,
    window: &WebviewWindow<R>,
) -> tauri::Result<()> {
    let panel_size = window.outer_size()?;
    let monitor = window.primary_monitor()?;
    let (origin, screen) = match &monitor {
        Some(m) => (*m.position(), *m.size()),
        None => (PhysicalPosition::new(0, 0), panel_size),
    };

    let position = PhysicalPosition::new(
        f64::from(origin.x) + (f64::from(screen.width) - f64::from(panel_size.width)) / 2.0,
        f64::from(origin.y) + (f64::from(screen.height) - f64::from(panel_size.height)) / 2.0,
    );

    window.set_position(position)?;
    panel.show();
    // `hides_on_deactivate` is still set, and the app is never activated under an Accessory
    // policy, so order the panel in explicitly rather than trusting `show()` alone.
    panel.order_front_regardless();

    log::info!(
        "spike: panel shown centred at {position:?}; panel={panel_size:?} \
         main display origin={origin:?} size={screen:?} visible={}",
        panel.is_visible()
    );
    Ok(())
}

fn toggle<R: Runtime>(handle: &AppHandle<R>, rect: Rect) -> tauri::Result<()> {
    let panel = handle
        .get_webview_panel(PANEL_LABEL)
        .map_err(|_| tauri::Error::WindowNotFound)?;

    if panel.is_visible() {
        log::info!("tray toggle: panel was visible, hiding");
        panel.hide();
        return Ok(());
    }

    let window = panel_window(&panel)?;
    window.set_position(anchor(&window, rect)?)?;
    panel.show();
    log::info!("tray toggle: shown, visible={}", panel.is_visible());
    Ok(())
}

fn panel_window<R: Runtime>(panel: &PanelHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    // `Panel` exposes no positioning or effects of its own; both live on the Tauri window.
    panel.to_window().ok_or(tauri::Error::WindowNotFound)
}

/// Centre the panel beneath the tray icon.
///
/// `rect.position` is already the tray icon's **top-left corner in top-left-origin physical
/// pixels**: `tray-icon`'s `get_tray_rect` flips macOS's bottom-left origin and subtracts the
/// icon height before handing the value over, which is the convention `PhysicalPosition`
/// already uses. So there is no flip here and no unit conversion at all — everything below
/// stays physical rather than being converted and hoped to be right.
///
/// `y` grows downward, so adding the icon's height puts the panel's top edge at the icon's
/// bottom edge, hanging below the menu bar. Getting that sign backwards is the classic
/// "panel appears at the bottom of the screen" bug.
///
/// Known upstream limitation: that flip uses `CGDisplayPixelsHigh(CGMainDisplayID())` — the
/// *main* display's height, not the height of the display the tray icon is actually on. With
/// the menu bar on a secondary display the result is vertically wrong. That is `tray-icon`'s
/// to fix, not ours. The log line below prints the display bounds alongside the computed
/// position precisely so one click tells you whether the result landed off-screen.
fn anchor<R: Runtime>(
    window: &WebviewWindow<R>,
    rect: Rect,
) -> tauri::Result<PhysicalPosition<f64>> {
    // The rect arrives as `Position::Physical`/`Size::Physical`, so the scale factor is
    // ignored; passing the real one keeps this correct if that ever changes.
    let scale = window.scale_factor()?;
    let tray_pos = rect.position.to_physical::<f64>(scale);
    let tray_size = rect.size.to_physical::<f64>(scale);

    // Safe before the window has ever been shown: tao implements `outer_size` as
    // `NSWindow::frame`, which is real from creation and unaffected by visibility. (It is
    // *not* safe straight after a programmatic resize — `set_inner_size` is async — but the
    // spike never resizes. That race is M2 proper's problem.)
    let panel_size = window.outer_size()?;

    let position = PhysicalPosition::new(
        tray_pos.x + tray_size.width / 2.0 - f64::from(panel_size.width) / 2.0,
        tray_pos.y + tray_size.height + TRAY_GAP,
    );

    let monitor = window.primary_monitor()?;
    let (origin, screen) = match &monitor {
        Some(m) => (format!("{:?}", m.position()), format!("{:?}", m.size())),
        None => ("none".into(), "none".into()),
    };
    log::info!(
        "tray anchor: scale={scale} tray_pos={tray_pos:?} tray_size={tray_size:?} \
         panel={panel_size:?} -> position={position:?} | main display origin={origin} size={screen}"
    );

    Ok(position)
}

/// Spike-only tray icon: a plain filled circle, black with an alpha coverage mask, so
/// `icon_as_template(true)` lets macOS tint it for light and dark menu bars.
///
/// Generated rather than committed on purpose. The app name is still undecided, and shipping
/// an icon now would freeze an identity that may change. The real artwork replaces
/// `src-tauri/icons/icon.png` — today a 70-byte 1×1 placeholder — and must exist before any
/// bundle. Delete this function then.
fn tray_icon() -> Image<'static> {
    // 36 px for an 18 pt menu bar slot at @2x.
    const SIZE: usize = 36;
    const RADIUS: f64 = 8.0;

    let centre = SIZE as f64 / 2.0;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f64 + 0.5 - centre;
            let dy = y as f64 + 0.5 - centre;
            // One pixel of linear feathering at the edge, so the circle is not jagged.
            let coverage = (RADIUS + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0);
            // RGB stays 0: a template image is read from its alpha channel alone.
            rgba[(y * SIZE + x) * 4 + 3] = (coverage * 255.0).round() as u8;
        }
    }

    Image::new_owned(rgba, SIZE as u32, SIZE as u32)
}
