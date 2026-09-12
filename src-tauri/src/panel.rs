//! M2 spike: the menu bar tray icon and the non-activating NSPanel popover.
//!
//! Everything here is spike scaffolding on the `m2-spike` branch. It exists to answer one
//! question the published docs cannot — does `WebviewWindow::set_effects` still apply
//! vibrancy after `tauri-nspanel` subclasses the NSWindow? — and is rewritten, not extended,
//! at M2 proper.

use tauri::{
    ActivationPolicy, App, AppHandle, LogicalSize, Manager, PhysicalPosition, Rect, Runtime, Size,
    WebviewUrl, WebviewWindow,
    image::Image,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    window::{Effect, EffectState, EffectsBuilder},
};
use tauri_nspanel::{ManagerExt, PanelBuilder, PanelLevel, tauri_panel};

/// Label of the popover window.
///
/// Deliberately absent from `capabilities/default.json`: `panel.html` invokes nothing, so it
/// needs no permissions, and a page that calls nothing has nothing to be denied. Widening
/// the capability to this window is a real permission decision for M2 proper.
const PANEL_LABEL: &str = "panel";

const PANEL_SIZE: LogicalSize<f64> = LogicalSize::new(360.0, 420.0);

/// Physical pixels between the bottom edge of the tray icon and the top edge of the panel.
const TRAY_GAP: f64 = 6.0;

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
            .state(EffectState::FollowsWindowActiveState)
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
                rect,
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
                && let Err(e) = toggle(&handle, rect)
            {
                log::warn!("tray click: {e}");
            }
        })
        .build(app)?;

    Ok(())
}

fn toggle<R: Runtime>(handle: &AppHandle<R>, rect: Rect) -> tauri::Result<()> {
    let panel = handle
        .get_webview_panel(PANEL_LABEL)
        .map_err(|_| tauri::Error::WindowNotFound)?;

    if panel.is_visible() {
        panel.hide();
        return Ok(());
    }

    let window = panel_window(&panel)?;
    window.set_position(anchor(&window, rect)?)?;
    panel.show();
    Ok(())
}

fn panel_window<R: Runtime>(
    panel: &tauri_nspanel::PanelHandle<R>,
) -> tauri::Result<WebviewWindow<R>> {
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
/// to fix, not ours.
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

    Ok(PhysicalPosition::new(
        tray_pos.x + tray_size.width / 2.0 - f64::from(panel_size.width) / 2.0,
        tray_pos.y + tray_size.height + TRAY_GAP,
    ))
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
