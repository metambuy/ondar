//! The map commands (M4b commit 6): thin. `map_select` reports the page's country; `map_pull`
//! hands the page's accumulated inputs to the session and answers with the frame as JSON (Step 0's
//! measured path: 14 ms invoke → commit for RU at the real pane) or `null` when nothing changed.
//! The band comes from the layout Rust last emitted, never from the page.

use std::time::Instant;

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
    let t_entry = Instant::now();
    let band = panel.layout().band;
    let step = {
        let store = state.0.get().and_then(|s| s.as_ref());
        session.0.lock().unwrap().pull(store, band, inputs)
    };
    let t_step = Instant::now();
    let (reply, frame_marks) = match step {
        Step::Reply(r) => (r, None),
        Step::Frame {
            seq,
            country,
            pane,
            view,
            band,
        } => {
            let shared = state.0.clone();
            let computed = tauri::async_runtime::spawn_blocking(move || {
                let t_f0 = Instant::now();
                let store = shared.get().and_then(|s| s.as_ref())?;
                let reply = map::frame_reply(store, seq, country, &pane, view, band);
                Some((reply, t_f0, Instant::now()))
            })
            .await
            .ok()
            .flatten();
            match computed {
                Some((r, t_f0, t_f1)) => (Some(r), Some((t_f0, t_f1))),
                None => (None, None),
            }
        }
    };
    decomposition::log(&reply, t_entry, t_step, frame_marks);
    Ok(reply)
}

/// The acceptance review's A3 (2026-10-06): one `measure[map] pull …` line per framed pull, debug
/// builds under the measurement harness only — `step_ms` (the lock step), `hop_in_ms` (the await
/// until the blocking thread starts), `frame_ms`, `hop_out_ms` (the blocking thread's end until the
/// command resumes), `serialize_ms` and `bytes` (the reply serialised once more, as Tauri will —
/// `serde_json::to_string`, ipc/mod.rs:181 — and dropped: the one cost the instrument adds, in
/// measure mode only), `rust_ms` (entry → return, without that extra serialisation). The page's
/// `frame` line carries the other half; the two join on `seq`.
#[cfg(debug_assertions)]
mod decomposition {
    use std::time::Instant;

    use crate::map::MapReply;

    pub fn log(
        reply: &Option<MapReply>,
        t_entry: Instant,
        t_step: Instant,
        frame: Option<(Instant, Instant)>,
    ) {
        let Some(r) = reply else { return };
        let Some((t_f0, t_f1)) = frame else { return };
        if crate::measure::mode().is_none() {
            return;
        }
        let t_ret = Instant::now();
        let ms = |a: Instant, b: Instant| b.duration_since(a).as_secs_f64() * 1e3;
        let t_s = Instant::now();
        let bytes = serde_json::to_string(r).map(|s| s.len()).unwrap_or(0);
        let serialize_ms = ms(t_s, Instant::now());
        log::info!(
            "measure[map] pull seq={} step_ms={:.2} hop_in_ms={:.2} frame_ms={:.2} hop_out_ms={:.2} serialize_ms={:.2} bytes={} rust_ms={:.2}",
            r.seq,
            ms(t_entry, t_step),
            ms(t_step, t_f0),
            ms(t_f0, t_f1),
            ms(t_f1, t_ret),
            serialize_ms,
            bytes,
            ms(t_entry, t_ret),
        );
    }
}

#[cfg(not(debug_assertions))]
mod decomposition {
    use std::time::Instant;

    use crate::map::MapReply;

    pub fn log(_: &Option<MapReply>, _: Instant, _: Instant, _: Option<(Instant, Instant)>) {}
}
