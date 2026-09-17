//! Commands from the popover page about the popover itself. The webview is an input device
//! here, never the owner of the decision: it reports what happened (a key), and Rust decides
//! what that means (a hide with a logged reason) — CLAUDE.md, "The one rule".

use tauri::AppHandle;

use crate::panel::{self, HideReason};

/// The page saw an Escape `keydown`. Rust hides the popover through the single hide path, so the
/// close reads in the log as `reason=esc effective=true` followed by the resign-key no-op.
/// Whether this runs on the main thread depends on where WebKit delivers the message;
/// `panel::hide` hops there regardless and logs the thread it ran on.
#[tauri::command]
pub fn panel_escape(app: AppHandle) {
    panel::hide(&app, HideReason::Esc);
}
