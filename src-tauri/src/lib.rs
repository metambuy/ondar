//! Tauri shell for Ondar. Owns the audio engine and forwards its events to the webview.
//! No business logic lives here or in the webview; see `crates/ondar-audio`.

mod commands;
mod error;
mod log_rate_limit;
// The dev-only measurement harness: debug builds only, so a release binary has no trace of it.
#[cfg(debug_assertions)]
mod measure;
mod panel;
mod tray;

use std::thread;

use tauri::Emitter;

use ondar_audio::{AudioEngine, EngineEvent, PlaybackState};
use ondar_stations::model::RefreshOutcome;
use ondar_stations::{Event as StationsEvent, StationsHandle, StationsService};
use tauri::Manager;

pub struct AppState {
    pub engine: AudioEngine,
    /// The station directory (`ondar-stations`): every method answered by its own DB thread,
    /// none blocking the caller. Started in `setup`, once the data directory is known.
    pub stations: StationsHandle,
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
    /// A background refresh of one country's station list ended (stale-while-revalidate,
    /// M3a F5). Payload `StationsUpdated { country_code, outcome }`: `landed` — the page
    /// re-requests `list_stations`; `failed` — the expired list stays, the page clears its
    /// `refreshing` flag and does not re-request (a re-request would start another refresh).
    pub const STATIONS_UPDATED: &str = "stations:updated";
    /// The same for the countries list. Payload `CountriesUpdated { outcome }`.
    pub const COUNTRIES_UPDATED: &str = "countries:updated";
}

/// Payload of `stations:updated`.
#[derive(Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct StationsUpdated {
    pub country_code: String,
    pub outcome: RefreshOutcome,
}

/// Payload of `countries:updated`.
#[derive(Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CountriesUpdated {
    pub outcome: RefreshOutcome,
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
    let (engine, engine_events) = AudioEngine::start(user_agent.clone());

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
        // Tray menu items. Listeners run in the event loop, on the main thread
        // (tauri 2.11.5 `app.rs:2588-2598`).
        .on_menu_event(tray::on_menu_event)
        .setup(move |app| {
            // The station directory lives under the identifier-keyed data directory —
            // `~/Library/Application Support/eu.ondar.radio/` for the bundle, `….dev/` for
            // `pnpm tauri:dev` (its own file, so dev and bundle never share a cache). Its
            // events are forwarded as Tauri events; the sink runs on the service's DB thread,
            // and `emit` is thread-safe. Nothing here can stop the launch: a database that
            // will not open is moved aside and recreated by the service, and if the directory
            // itself is unusable the handle is degraded (every command answers
            // `code: "stations"`) while the tray, the popover and audio come up as usual
            // (`/code-review` finding 2, 2026-09-22).
            let sink_handle = app.handle().clone();
            let sink: ondar_stations::EventSink = std::sync::Arc::new(move |ev| {
                let result = match ev {
                    StationsEvent::StationsUpdated {
                        country_code,
                        outcome,
                    } => sink_handle.emit(
                        events::STATIONS_UPDATED,
                        StationsUpdated {
                            country_code,
                            outcome,
                        },
                    ),
                    StationsEvent::CountriesUpdated { outcome } => {
                        sink_handle.emit(events::COUNTRIES_UPDATED, CountriesUpdated { outcome })
                    }
                };
                if let Err(e) = result {
                    log::warn!("failed to emit stations event: {e}");
                }
            });
            let stations = match app.path().app_data_dir() {
                Ok(data_dir) => {
                    if let Err(e) = std::fs::create_dir_all(&data_dir) {
                        log::warn!("cannot create {}: {e}", data_dir.display());
                    }
                    StationsService::start(data_dir.join("ondar.sqlite"), &user_agent, sink)
                }
                Err(e) => {
                    log::error!("no application data directory: {e}");
                    StationsHandle::unavailable(format!("no application data directory: {e}"))
                }
            };
            app.manage(AppState { engine, stations });

            panel::setup(app)?;
            tray::setup(app)?;
            #[cfg(debug_assertions)]
            measure::setup(app.handle());

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
            commands::panel::panel_layout_committed,
            commands::panel::panel_view_back,
            commands::stations::list_countries,
            commands::stations::list_stations,
            commands::stations::search_stations,
            commands::stations::list_favourites,
            commands::stations::add_favourite,
            commands::stations::remove_favourite,
            commands::stations::list_recents,
            commands::stations::record_played,
            #[cfg(debug_assertions)]
            measure::measure_report,
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
