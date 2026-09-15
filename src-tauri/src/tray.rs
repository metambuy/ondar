//! The menu bar tray icon: template glyphs, click logging, and the idle/playing swap.

use tauri::{
    App, AppHandle, Runtime,
    image::Image,
    include_image,
    tray::{TrayIconBuilder, TrayIconEvent},
};

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

/// Build the tray icon. Call from `setup`.
pub fn setup(app: &mut App) -> tauri::Result<()> {
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
                // Logged on every `Click`, press and release both: the popover toggle that
                // arrives with the panel filters these, and this line is the evidence of what
                // arrived before any filtering.
                log::info!(
                    "tray click button={button:?} state={button_state:?} rect.position={:?} rect.size={:?}",
                    rect.position,
                    rect.size
                );
            }
        })
        .build(app)?;
    Ok(())
}

/// Swap between the idle and playing glyphs. The difference is a shape (hollow vs filled cap
/// dot), never a colour — a template image discards colour.
pub fn set_playing<R: Runtime>(handle: &AppHandle<R>, playing: bool) {
    let Some(tray) = handle.tray_by_id(TRAY_ID) else {
        log::warn!("tray icon {TRAY_ID} not found; playing={playing} not shown");
        return;
    };
    let icon = if playing { PLAYING } else { IDLE };
    // `set_icon` rebuilds the NSImage with `is_template = false` hard-coded (tray-icon 0.24.2
    // `platform_impl/macos/mod.rs:115-123`), so template mode must be re-asserted every time.
    let result = tray
        .set_icon(Some(icon))
        .and_then(|()| tray.set_icon_as_template(true));
    if let Err(e) = result {
        log::warn!("tray icon swap failed (playing={playing}): {e}");
    }
}
