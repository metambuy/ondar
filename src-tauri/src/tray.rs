//! The menu bar tray icon: template glyphs, the click that toggles the popover, and the
//! idle/playing swap.

use tauri::{
    App, AppHandle, Runtime,
    image::Image,
    include_image,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::panel;

const TRAY_ID: &str = "ondar-tray";

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
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(IDLE)
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
            }
        })
        .build(app)?;
    Ok(())
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
