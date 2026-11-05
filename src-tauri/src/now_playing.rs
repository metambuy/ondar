//! "The station playing" (M5e1, decision 5): the last station a play named in this process, held
//! by the shell beside the engine. Not in `ondar-audio`: the engine has no `Station` type and no
//! dependency on `ondar-stations`, and keeps it that way. No domain logic: a holder, and the one
//! play path the page's command and (commit 5) a media key both take.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};

use ondar_audio::AudioCommand;
use ondar_stations::model::Station;

use crate::sync::lock;
use crate::{AppState, events};

#[derive(Default)]
pub struct NowPlaying {
    /// Short locks only, never held across a send that waits (`AudioEngine::send` is an
    /// unbounded `mpsc` send).
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The last station a play named, this process; never cleared (after Stop the popover's
    /// Play replays it). `None` until the first play.
    station: Option<Station>,
}

impl NowPlaying {
    /// Store `station` and send its `Play` through `send`, under one lock, so the stored station
    /// and the engine's command order agree whoever plays (the page, a key).
    pub fn play(&self, send: impl FnOnce(AudioCommand), station: Station) {
        let mut inner = lock(&self.inner);
        send(AudioCommand::Play {
            url: station.url.clone(),
            station_id: station.uuid.clone(),
            bitrate_kbps: station.bitrate_kbps,
        });
        inner.station = Some(station);
    }

    /// The station playing, for the page's mount read (`get_station`).
    pub fn station(&self) -> Option<Station> {
        lock(&self.inner).station.clone()
    }
}

/// The one play path: store and send ([`NowPlaying::play`]), then tell the page which station
/// it is (`playback:station`). The page names that station, never the row it clicked.
pub fn play(app: &AppHandle, station: Station) {
    let state = app.state::<AppState>();
    state
        .now_playing
        .play(|cmd| state.engine.send(cmd), station.clone());
    if let Err(e) = app.emit(events::STATION, station) {
        log::warn!("failed to emit {}: {e}", events::STATION);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ondar_stations::model::Codec;

    use super::*;

    fn station(uuid: &str) -> Station {
        Station {
            uuid: uuid.into(),
            name: uuid.into(),
            url: format!("http://127.0.0.1/{uuid}"),
            homepage: String::new(),
            favicon: String::new(),
            country_code: "PT".into(),
            codec: Codec::Mp3,
            codec_raw: "MP3".into(),
            bitrate_kbps: Some(128),
            hls: false,
            video: false,
            votes: 0,
            click_count: 0,
            click_trend: 0,
            geo: None,
            state: String::new(),
            last_check_ok: true,
        }
    }

    /// M5e1 commit 4: the holder and the engine see plays in one order. **What it pins:** two
    /// plays through a fake engine channel: the stored station is the second, and the channel
    /// carries the first's `Play` then the second's, each with the station's url, uuid and
    /// bitrate. **Fails if** `play` stores without sending, sends another station's arguments,
    /// or keeps the first station.
    #[test]
    fn play_stores_the_station_and_sends_its_play_in_order() {
        let now = NowPlaying::default();
        assert!(now.station().is_none(), "nothing before the first play");
        let (tx, rx) = mpsc::channel();
        now.play(|c| tx.send(c).unwrap(), station("a"));
        now.play(|c| tx.send(c).unwrap(), station("b"));
        assert_eq!(now.station().map(|s| s.uuid), Some("b".to_string()));
        let sent: Vec<_> = rx
            .try_iter()
            .map(|c| match c {
                AudioCommand::Play {
                    url,
                    station_id,
                    bitrate_kbps,
                } => (url, station_id, bitrate_kbps),
                other => panic!("not a Play: {other:?}"),
            })
            .collect();
        assert_eq!(
            sent,
            vec![
                ("http://127.0.0.1/a".into(), "a".into(), Some(128)),
                ("http://127.0.0.1/b".into(), "b".into(), Some(128)),
            ]
        );
    }
}
