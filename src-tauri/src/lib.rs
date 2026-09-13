//! Tauri shell for Ondar. Owns the audio engine and forwards its events to the webview.
//! No business logic lives here or in the webview; see `crates/ondar-audio`.

mod commands;
mod error;
mod log_rate_limit;

use std::thread;

use tauri::Emitter;

use ondar_audio::{AudioEngine, EngineEvent};

pub struct AppState {
    pub engine: AudioEngine,
}

/// Event names — the only strings the webview needs to know.
pub mod events {
    pub const STATE: &str = "playback:state";
    pub const STREAM_INFO: &str = "playback:stream_info";
    pub const METADATA: &str = "playback:metadata";
    pub const RECONNECT: &str = "playback:reconnect";
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

    tauri::Builder::default()
        .manage(AppState { engine })
        .setup(move |app| {
            let handle = app.handle().clone();
            thread::Builder::new()
                .name("ondar-events".into())
                .spawn(move || {
                    for ev in engine_events {
                        let result = match ev {
                            EngineEvent::State(s) => handle.emit(events::STATE, s),
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ondar");
}
