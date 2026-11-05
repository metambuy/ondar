//! Playback commands. Thin by design: validate, dispatch, return.
//!
//! They dispatch three different ways, and the return type follows the mechanism:
//!
//! * **Channel message, returns `Result`** — [`play`], [`set_volume`]. Send an
//!   `AudioCommand` and return immediately; the `Result` reports argument validation only.
//!   The playback outcome arrives later as a `playback:*` event, never as a return value.
//!   `play` takes the whole `Station` and goes through [`crate::now_playing::play`], which
//!   stores it as the station playing and emits `playback:station` (M5e1).
//! * **Channel message, returns `()`** — [`pause`], [`resume`], [`stop`]. Nothing to
//!   validate.
//! * **Direct engine access, never touches the channel** — [`set_eq_gain`], [`get_eq`],
//!   [`get_playback_state`], and [`get_station`], which reads the shell's holder. These reach into `AudioEngine` through the shared handle.
//!   `set_eq_gain` stores an atomic into `EqGains` that the audio thread picks up at its
//!   next frame-boundary check; the getters read a snapshot synchronously. Because gains
//!   live on the handle rather than in the command stream, they are *not* ordered against
//!   `play`/`stop` and survive a session change.
// Tauri commands take `State` and `AppHandle` by value: the `#[tauri::command]` contract, not a
// choice this module makes.
#![allow(
    clippy::needless_pass_by_value,
    reason = "tauri::command takes State and AppHandle by value"
)]

use tauri::{AppHandle, State};

use ondar_audio::{AudioCommand, BAND_COUNT, EqBand, MAX_GAIN_DB, PlaybackState};
use ondar_stations::model::Station;

use crate::AppState;
use crate::error::OndarError;

/// The station's `bitrate_kbps` sizes the engine's prefetch (M3b commit 6: `max(one decoder
/// read, RING_SECONDS × bitrate / 8)`). M5e1: the whole `Station`, so Rust holds the station
/// playing and the page mirrors it (`playback:station`, `get_station`).
#[tauri::command]
pub fn play(app: AppHandle, station: Station) -> Result<(), OndarError> {
    if station.url.trim().is_empty() {
        return Err(OndarError::InvalidArgument("url is empty".into()));
    }
    // F1 instrument 7a (0.1.1): the page's command reached the shell. The engine logs its own
    // receipt when it takes the command off the channel (`Engine::play`); the pair separates
    // "the page never sent it" from "the shell sent it and the engine never took it".
    log::info!("play command station_id={}", station.uuid);
    crate::now_playing::play(&app, station);
    Ok(())
}

#[tauri::command]
pub fn pause(state: State<'_, AppState>) {
    state.engine.send(AudioCommand::Pause);
}

#[tauri::command]
pub fn resume(state: State<'_, AppState>) {
    state.engine.send(AudioCommand::Resume);
}

#[tauri::command]
pub fn stop(state: State<'_, AppState>) {
    state.engine.send(AudioCommand::Stop);
}

/// `volume` is linear 0.0–1.0.
#[tauri::command]
pub fn set_volume(state: State<'_, AppState>, volume: f32) -> Result<(), OndarError> {
    if !(0.0..=1.0).contains(&volume) {
        return Err(OndarError::InvalidArgument(
            "volume must be within 0.0..=1.0".into(),
        ));
    }
    state.engine.send(AudioCommand::SetVolume(volume));
    Ok(())
}

#[tauri::command]
pub fn set_eq_gain(state: State<'_, AppState>, band: u8, gain_db: f32) -> Result<(), OndarError> {
    if band as usize >= BAND_COUNT {
        return Err(OndarError::InvalidArgument(format!(
            "band must be < {BAND_COUNT}"
        )));
    }
    if !gain_db.is_finite() || gain_db.abs() > MAX_GAIN_DB {
        return Err(OndarError::InvalidArgument(format!(
            "gain_db must be within ±{MAX_GAIN_DB}"
        )));
    }
    state.engine.eq().set(band as usize, gain_db);
    Ok(())
}

#[tauri::command]
pub fn get_eq(state: State<'_, AppState>) -> Vec<EqBand> {
    state.engine.eq().bands()
}

#[tauri::command]
pub fn get_playback_state(state: State<'_, AppState>) -> PlaybackState {
    state.engine.state()
}

/// The station playing (M5e1): the last one a play named in this process, or `None` before the
/// first. The page's mount read, as `get_playback_state` is for `playback:state`.
#[tauri::command]
pub fn get_station(state: State<'_, AppState>) -> Option<Station> {
    state.now_playing.station()
}
