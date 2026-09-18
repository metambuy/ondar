//! Commands from the popover page about the popover itself. The webview is an input device
//! here, never the owner of the decision: it reports what happened (a key), and Rust decides
//! what that means (a hide with a logged reason) — CLAUDE.md, "The one rule".

use tauri::{AppHandle, State};

use crate::panel::{self, HideReason, PanelLayout, PanelState};

/// The page saw an Escape `keydown`. Rust hides the popover through the single hide path, so the
/// close reads in the log as `reason=esc effective=true` followed by the resign-key no-op.
/// Whether this runs on the main thread depends on where WebKit delivers the message;
/// `panel::hide` hops there regardless and logs the thread it ran on.
#[tauri::command]
pub fn panel_escape(app: AppHandle) {
    panel::hide(&app, HideReason::Esc);
}

/// The layout the popover last emitted — pane, height state, size, expandable — for the page to
/// mirror on mount: the counterpart of the `panel:layout` event, exactly as `get_playback_state`
/// is the counterpart of `playback:state`.
#[tauri::command]
pub fn get_panel_layout(state: State<'_, PanelState>) -> PanelLayout {
    state.layout()
}

/// The page has committed the DOM for the layout with this generation (a React effect after the
/// render that used it). Rust completes the visible change then — orders a pending show in, or
/// changes a visible panel's frame — if that generation is still the pending one; otherwise a
/// logged no-op (decision D3). The fallback timer completes it without this call.
#[tauri::command]
pub fn panel_layout_committed(app: AppHandle, generation: u32) {
    panel::layout_committed(&app, generation);
}

/// The page's expand/collapse control was clicked. Rust decides what that means — a layout for
/// the new height against a fresh tray rect, or a logged refusal (decision D1's floor) — and the
/// page learns the outcome from `panel:layout`, never from this call's return.
#[tauri::command]
pub fn panel_set_expanded(app: AppHandle, expanded: bool) {
    panel::set_expanded(&app, expanded);
}
