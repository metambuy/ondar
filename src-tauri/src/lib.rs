//! Tauri shell for Onda. Owns the audio engine and forwards its events to the webview.
//! No business logic lives here or in the webview; see `crates/onda-audio`.

mod commands;
mod error;

use std::thread;

use tauri::Emitter;

use onda_audio::{AudioEngine, EngineEvent};

pub struct AppState {
    pub engine: AudioEngine,
}

/// Event names — the only strings the webview needs to know.
pub mod events {
    pub const STATE: &str = "playback:state";
    pub const STREAM_INFO: &str = "playback:stream_info";
    pub const METADATA: &str = "playback:metadata";
}

pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,onda_audio=debug"))
        .init();

    let user_agent = format!("Onda/{}", env!("CARGO_PKG_VERSION"));
    let (engine, engine_events) = AudioEngine::start(user_agent);

    tauri::Builder::default()
        .manage(AppState { engine })
        .setup(move |app| {
            let handle = app.handle().clone();
            thread::Builder::new()
                .name("onda-events".into())
                .spawn(move || {
                    for ev in engine_events {
                        let result = match ev {
                            EngineEvent::State(s) => handle.emit(events::STATE, s),
                            EngineEvent::StreamInfo(i) => handle.emit(events::STREAM_INFO, i),
                            EngineEvent::Metadata(m) => handle.emit(events::METADATA, m),
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
        .expect("error while running Onda");
}
