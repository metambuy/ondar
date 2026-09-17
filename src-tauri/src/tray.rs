//! The menu bar tray icon: template glyphs, the click that toggles the popover, and the
//! idle/playing swap.

use tauri::{
    App, AppHandle, Rect, Runtime,
    image::Image,
    include_image,
    menu::{MenuBuilder, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::panel::{self, HideReason, ShowReason};

const TRAY_ID: &str = "ondar-tray";

/// Menu id of the About item. Not `PredefinedMenuItem::about`: the standard About panel opens
/// at `NSNormalWindowLevel` while the app is inactive, i.e. behind the frontmost app (M2c Step 0,
/// item 3, measured), so About is a pane inside the popover instead (decision 2, 2026-09-16).
const ABOUT_ID: &str = "about";

// Only the 44 px glyphs are used, and the 22 px files can never render through this API.
// `tray-icon` builds the status item's `NSImage` from one PNG — one representation, no way to
// supply a second — and forces its height to 18 pt (tray-icon 0.24.2
// `platform_impl/macos/mod.rs:283-311`). So the 44 px source is resampled to 36 px at @2x and
// 18 px at @1x. See ONDAR.md, "Tray template glyphs".
//
// `include_image!` decodes the PNG at compile time into raw RGBA, so no `image-png` feature.
const IDLE: Image<'static> = include_image!("icons/tray/ondar-tray-44-idle.png");
const PLAYING: Image<'static> = include_image!("icons/tray/ondar-tray-44-playing.png");

/// Build the tray icon. Call from `setup`, after `panel::setup`.
pub fn setup(app: &mut App) -> tauri::Result<()> {
    let handle = app.handle().clone();

    // About + Quit, nothing else — no Preferences window exists in any milestone. Quit is
    // `terminate:` (muda 0.19.3 `platform_impl/macos/mod.rs:994`): the engine is not shut
    // down, the process is, and the OS reclaims the audio device. Measured with a stream
    // playing (Step 0): exit 0, no panic, no stuck process. Recorded in ONDAR.md as the
    // behaviour, not papered over with a shutdown path.
    let menu = MenuBuilder::new(app)
        .items(&[
            &MenuItem::with_id(app, ABOUT_ID, "About Ondar", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some("Quit Ondar"))?,
        ])
        .build()?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(IDLE)
        .menu(&menu)
        // REQUIRED, not a preference. `tray-icon` defaults `menu_on_left_click` to true, and a
        // left click then opens the menu, whose modal tracking loop swallows `mouseUp:` — so
        // the toggle below, which keys off `Up`, never fires. Measured 2026-09-16 (Step 0,
        // run A): 7 left-clicks → 7 `Click{Left,Down}`, 0 `Click{Left,Up}`, 0 toggles; the
        // tray icon merely showed the menu. Right-click showing the menu is tray-icon's default
        // and has no setter in tauri 2.11.5.
        .show_menu_on_left_click(false)
        // The glyphs are pure black on alpha. As a template image macOS reads shape from alpha
        // alone and tints for light/dark menu bars and the highlight state; without it they
        // render as flat black and vanish on a dark menu bar.
        .icon_as_template(true)
        .on_tray_icon_event(move |_tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                rect,
                ..
            } = event
            {
                // Logged before the filter, on every `Click`, because the shape of this log
                // separates the ways the tray path fails:
                //   two lines per physical click → press and release both arrived
                //   no line at all              → the handler is not on this event
                //   click lines, no `panel show`/`panel hide` line → toggle bailed before ordering
                log::info!(
                    "tray click button={button:?} state={button_state:?} rect.position={:?} rect.size={:?}",
                    rect.position,
                    rect.size
                );

                // `Click` fires on both press and release. Toggling on both shows the panel on
                // press and hides it on release, so nothing is ever visible — a failure that
                // looks like a handler that never ran. This filter is load-bearing.
                if button == MouseButton::Left && button_state == MouseButtonState::Up {
                    panel::toggle(&handle, rect);
                }

                // A right-click opens the menu, and the popover hides first (decision 1,
                // 2026-09-16). `Down` only: the menu's tracking loop swallows the right
                // `mouseUp:` as well, so `Click{Right,Up}` never arrives (Step 0, item 2:
                // 5 right-clicks, 5 `Down`, 0 `Up`). With the popover already hidden this
                // logs `effective=false` on every right-click, which is correct.
                if button == MouseButton::Right && button_state == MouseButtonState::Down {
                    panel::hide(&handle, HideReason::Menu);
                }
            }
        })
        .build(app)?;
    Ok(())
}

/// The tray menu's items. About shows the popover under the icon with the About pane; the view
/// itself is emitted by `panel::show_at` from the reason. Quit is predefined and never reaches
/// here.
pub fn on_menu_event<R: Runtime>(handle: &AppHandle<R>, event: MenuEvent) {
    if event.id().as_ref() != ABOUT_ID {
        return;
    }
    log::info!("tray menu about");
    if let Some(rect) = rect(handle) {
        panel::show_at(handle, rect, ShowReason::About);
    }
}

/// Where the tray icon is, in the same units `TrayIconEvent::Click { rect }` uses — the two are
/// one function in tray-icon 0.24.2 (`get_tray_rect`), and were measured equal on 11 of 11
/// clicks (Step 0, item 6). For a show that has no click to take a rect from: the About item,
/// a re-launch. Safe on the main thread and from a tokio thread (21–198 µs measured); `None`
/// is logged and the caller does nothing (brief decision 3).
pub fn rect<R: Runtime>(handle: &AppHandle<R>) -> Option<Rect> {
    let Some(tray) = handle.tray_by_id(TRAY_ID) else {
        log::warn!("tray icon {TRAY_ID} not found; no rect to anchor to");
        return None;
    };
    match tray.rect() {
        Ok(Some(rect)) => Some(rect),
        Ok(None) => {
            log::warn!("tray rect unavailable; not showing the popover");
            None
        }
        Err(e) => {
            log::warn!("tray rect failed: {e}; not showing the popover");
            None
        }
    }
}

/// Swap between the idle and playing glyphs. The difference is a shape (hollow vs filled cap
/// dot), never a colour — a template image discards colour. Returns whether the swap happened,
/// so the caller can retry on the next event instead of believing a failed swap.
pub fn set_playing<R: Runtime>(handle: &AppHandle<R>, playing: bool) -> bool {
    let Some(tray) = handle.tray_by_id(TRAY_ID) else {
        log::warn!("tray icon {TRAY_ID} not found; playing={playing} not shown");
        return false;
    };
    let icon = if playing { PLAYING } else { IDLE };
    // One call, not `set_icon` + `set_icon_as_template`. `set_icon` rebuilds the NSImage with
    // `is_template = false` hard-coded (tray-icon 0.24.2 `platform_impl/macos/mod.rs:115-123`),
    // and the two calls are separate main-thread tasks, so a flat black glyph can be drawn in
    // between. `set_icon_with_as_template` sets both in one task (tauri 2.11.5
    // `tray/mod.rs:569-591`, whose doc comment names this flicker).
    match tray.set_icon_with_as_template(Some(icon), true) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("tray icon swap failed (playing={playing}): {e}");
            false
        }
    }
}
