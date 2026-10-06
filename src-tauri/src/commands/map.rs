//! The map commands (M4b commit 6): thin. `map_select` reports the page's country; `map_pull`
//! hands the page's accumulated inputs to the session and answers with the frame as JSON (Step 0's
//! measured path: 14 ms invoke → commit for RU at the real pane) or `null` when nothing changed.
//! The band comes from the layout Rust last emitted, never from the page.

use tauri::State;

use crate::error::OndarError;
use crate::map::{self, MapInputs, MapReply, MapSessionState, MapState, Step};
use crate::panel::PanelState;

/// The page's selected country changed (or the map pane mounted): the session returns to the
/// fit and the next pull frames. One log line per call — `map select code=… lookup=…` — so a
/// normal run (no measure mode) shows the map following the dropdown (acceptance review A2).
#[tauri::command]
pub fn map_select(state: State<'_, MapState>, session: State<'_, MapSessionState>, code: String) {
    let store = state.0.get().and_then(|s| s.as_ref());
    let lookup = session.0.lock().unwrap().select(store, &code);
    log::info!("map select code={code} lookup={lookup}");
}

/// One pull per animation frame while the page has input pending (or a band / country change):
/// the inputs are folded and applied under the lock, the frame is computed off it on a blocking
/// thread (the store is read-only), and the reply carries the sequence number the page compares.
/// The `Result` is Tauri's requirement for an async command that borrows state; it is always `Ok`
/// — the states a page must show (`no_map`, `unavailable`, `no_band`) are replies, not errors.
#[tauri::command]
pub async fn map_pull(
    state: State<'_, MapState>,
    session: State<'_, MapSessionState>,
    panel: State<'_, PanelState>,
    inputs: MapInputs,
) -> Result<Option<MapReply>, OndarError> {
    let band = panel.layout().band;
    let step = {
        let store = state.0.get().and_then(|s| s.as_ref());
        session.0.lock().unwrap().pull(store, band, inputs)
    };
    Ok(match step {
        Step::Reply(r) => r,
        Step::Frame {
            seq,
            country,
            pane,
            view,
            band,
        } => {
            let shared = state.0.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let store = shared.get().and_then(|s| s.as_ref())?;
                Some(map::frame_reply(store, seq, country, &pane, view, band))
            })
            .await
            .ok()
            .flatten()
        }
    })
}
