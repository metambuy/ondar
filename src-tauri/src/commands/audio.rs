//! Playback commands. Thin by design: validate, dispatch, return.
//!
//! They dispatch three different ways, and the return type follows the mechanism:
//!
//! * **Channel message, returns `Result`** — [`play`], [`set_volume`]. Send an
//!   `AudioCommand` and return immediately; the `Result` reports argument validation only.
//!   The playback outcome arrives later as a `playback:*` event, never as a return value.
//! * **Channel message, returns `()`** — [`pause`], [`resume`], [`stop`]. Nothing to
//!   validate.
//! * **Direct engine access, never touches the channel** — [`set_eq_gain`], [`get_eq`],
//!   [`get_playback_state`]. These reach into `AudioEngine` through the shared handle.
//!   `set_eq_gain` stores an atomic into `EqGains` that the audio thread picks up at its
//!   next frame-boundary check; the getters read a snapshot synchronously. Because gains
//!   live on the handle rather than in the command stream, they are *not* ordered against
//!   `play`/`stop` and survive a session change.

use tauri::State;

use onda_audio::{AudioCommand, BAND_COUNT, EqBand, MAX_GAIN_DB, PlaybackState};

use crate::AppState;
use crate::error::OndaError;

#[tauri::command]
pub fn play(state: State<'_, AppState>, url: String, station_id: String) -> Result<(), OndaError> {
    if url.trim().is_empty() {
        return Err(OndaError::InvalidArgument("url is empty".into()));
    }
    state.engine.send(AudioCommand::Play { url, station_id });
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
pub fn set_volume(state: State<'_, AppState>, volume: f32) -> Result<(), OndaError> {
    if !(0.0..=1.0).contains(&volume) {
        return Err(OndaError::InvalidArgument(
            "volume must be within 0.0..=1.0".into(),
        ));
    }
    state.engine.send(AudioCommand::SetVolume(volume));
    Ok(())
}

#[tauri::command]
pub fn set_eq_gain(state: State<'_, AppState>, band: u8, gain_db: f32) -> Result<(), OndaError> {
    if band as usize >= BAND_COUNT {
        return Err(OndaError::InvalidArgument(format!(
            "band must be < {BAND_COUNT}"
        )));
    }
    if !gain_db.is_finite() || gain_db.abs() > MAX_GAIN_DB {
        return Err(OndaError::InvalidArgument(format!(
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
