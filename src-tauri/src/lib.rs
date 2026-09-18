//! Tauri shell for Ondar. Owns the audio engine and forwards its events to the webview.
//! No business logic lives here or in the webview; see `crates/ondar-audio`.

mod commands;
mod error;
mod log_rate_limit;
mod panel;
mod tray;

use std::thread;

use tauri::Emitter;

use ondar_audio::{AudioEngine, EngineEvent, PlaybackState};

pub struct AppState {
    pub engine: AudioEngine,
}

/// Event names — the only strings the webview needs to know.
pub mod events {
    pub const STATE: &str = "playback:state";
    pub const STREAM_INFO: &str = "playback:stream_info";
    pub const METADATA: &str = "playback:metadata";
    pub const RECONNECT: &str = "playback:reconnect";
    /// What the popover page renders (`panel::PanelLayout`: pane, height state, size in points,
    /// expandable) — emitted on every effective show and every resize, so the page mirrors it and
    /// never decides it. The page asks `get_panel_layout` on mount for the same value. M2d;
    /// supersedes M2c's `panel:view`.
    pub const PANEL_LAYOUT: &str = "panel:layout";
}

pub fn run() {
    // `stream-download` logs via `tracing`, not `log`; `tracing_subscriber::fmt`'s `init()`
    // installs a `LogTracer` itself (its default `tracing-log` feature), which is what lets
    // `ondar_audio`'s own `log::` call sites still show up here too — one `RUST_LOG` drives
    // both. Same default filter `env_logger` used, so behaviour when `RUST_LOG` is unset is
    // unchanged.
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::prelude::*;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ondar_audio=debug"));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(filter)
                .with_filter(log_rate_limit::RateLimit::new()),
        )
        .init();

    let user_agent = format!("Ondar/{}", env!("CARGO_PKG_VERSION"));
    let (engine, engine_events) = AudioEngine::start(user_agent);

    let app = tauri::Builder::default()
        // Registered first, as the plugin's own docs require: its setup is where a second
        // process notifies the first and exits, before anything else is built. The callback
        // runs on a tokio worker (`async_runtime::spawn`, plugin 2.4.4 `macos.rs:100`), and
        // everything it does to the panel or tray hops to the main thread — `PanelHandle` is
        // `Send`, but its operations are not safe off main (M2c Step 0, R6).
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            log::info!(
                "single-instance callback thread={:?} argv={argv:?} cwd={cwd:?}",
                std::thread::current().name()
            );
            let on_main = app.clone();
            if let Err(e) = app.run_on_main_thread(move || {
                if let Some(rect) = tray::rect(&on_main) {
                    panel::show_at(&on_main, rect, panel::ShowReason::SecondInstance);
                }
            }) {
                log::warn!("single-instance: main-thread hop failed: {e}");
            }
        }))
        // Manages the panel store `PanelBuilder::build()` registers into; without it the
        // builder's internal `to_panel` panics on missing state.
        .plugin(tauri_nspanel::init())
        .manage(AppState { engine })
        // Tray menu items. Listeners run in the event loop, on the main thread
        // (tauri 2.11.5 `app.rs:2588-2598`).
        .on_menu_event(tray::on_menu_event)
        .setup(move |app| {
            panel::setup(app)?;
            tray::setup(app)?;

            let handle = app.handle().clone();
            thread::Builder::new()
                .name("ondar-events".into())
                .spawn(move || {
                    // The glyph currently shown; `tray::setup` starts on idle. State events are
                    // emitted on every change, and most changes (Connecting → Buffering, each
                    // Reconnecting attempt) are not an idle/playing flip, so only a flip swaps.
                    let mut tray_playing = false;
                    for ev in engine_events {
                        let result = match ev {
                            EngineEvent::State(s) => {
                                let playing = matches!(s, PlaybackState::Playing);
                                if playing != tray_playing && tray::set_playing(&handle, playing) {
                                    tray_playing = playing;
                                }
                                handle.emit(events::STATE, s)
                            }
                            EngineEvent::StreamInfo(i) => handle.emit(events::STREAM_INFO, i),
                            EngineEvent::Metadata(m) => handle.emit(events::METADATA, m),
                            EngineEvent::Reconnect(r) => handle.emit(events::RECONNECT, r),
                        };
                        if let Err(e) = result {
                            log::warn!("failed to emit engine event: {e}");
                        }
                    }
                })
                .expect("spawn event forwarder");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::audio::play,
            commands::audio::pause,
            commands::audio::resume,
            commands::audio::stop,
            commands::audio::set_volume,
            commands::audio::set_eq_gain,
            commands::audio::get_eq,
            commands::audio::get_playback_state,
            commands::panel::panel_escape,
            commands::panel::get_panel_layout,
            commands::panel::panel_set_expanded,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Ondar");

    // `.run(callback)` rather than `.run(context)`: `RunEvent::Reopen` is the only way to see
    // `open Ondar.app` or a Finder double-click against the running app — LaunchServices
    // starts no second process for either, so the single-instance plugin cannot — and
    // `.run(context)` discards it. Delivered on the main thread (measured, Step 0 item 5).
    app.run(|handle, event| {
        if let tauri::RunEvent::Reopen {
            has_visible_windows,
            ..
        } = event
        {
            log::info!("reopen has_visible_windows={has_visible_windows}");
            if let Some(rect) = tray::rect(handle) {
                panel::show_at(handle, rect, panel::ShowReason::Reopen);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    /// `pnpm tauri:dev` merges `tauri.dev.conf.json` over `tauri.conf.json` so the dev instance
    /// gets its own identifier — its own single-instance socket, and from M3 its own data dir —
    /// and can run beside a bundled build (M2c Step 0, case (f): with one identifier the dev
    /// instance handed off and exited). The overlay is a literal, so this executes the rule it
    /// stands for: the dev identifier is the real identifier plus `.dev`, and nothing else is
    /// overlaid. Renaming the real identifier (OPEN.md's row) without the overlay fails here.
    #[test]
    fn dev_identifier_is_the_real_identifier_plus_dev() {
        let real: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json parses");
        let dev: serde_json::Value = serde_json::from_str(include_str!("../tauri.dev.conf.json"))
            .expect("tauri.dev.conf.json parses");
        let real_id = real["identifier"]
            .as_str()
            .expect("tauri.conf.json has a string identifier");
        let dev_id = dev["identifier"]
            .as_str()
            .expect("tauri.dev.conf.json has a string identifier");
        assert_eq!(
            dev_id,
            format!("{real_id}.dev"),
            "the dev identifier must be the real one plus `.dev`"
        );
        assert_eq!(
            dev.as_object().map(|o| o.len()),
            Some(1),
            "the dev overlay carries the identifier and nothing else, so dev cannot silently \
             diverge from the real config"
        );
    }
}
