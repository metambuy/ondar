//! "The station playing" (M5e1, decision 5): the last station a play named in this process, held
//! by the shell beside the engine. Not in `ondar-audio`: the engine has no `Station` type and no
//! dependency on `ondar-stations`, and keeps it that way. No domain logic: a holder, the one play
//! path the page's command and a media key both take, and two pure functions: what a remote
//! command does ([`on_remote`]) and what Now Playing shows ([`now_playing`]). The FFI that uses
//! them is `media.rs`.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};

use ondar_audio::{AudioCommand, EngineEvent, PlaybackState};
use ondar_stations::model::Station;

use crate::sync::lock;
use crate::{AppState, events};

#[derive(Default)]
pub struct NowPlaying {
    /// Short locks only, never held across a send that waits (`AudioEngine::send` is an
    /// unbounded `mpsc` send).
    inner: Mutex<Inner>,
}

struct Inner {
    /// The last station a play named, this process; never cleared (after Stop the popover's
    /// Play replays it, and so does a key, A1). `None` until the first play: nothing is published
    /// before it, so a launch never takes the media keys from another app.
    station: Option<Station>,
    /// The live ICY title, cleared on `State(Connecting)`: the engine's channel orders that
    /// against the old session's last `Metadata` (a clear in `play` could be overtaken by it).
    icy_title: Option<String>,
    /// The engine's state as the forwarder last saw it.
    state: PlaybackState,
    /// Counts every change of what Now Playing shows, so the main thread can drop an [`Info`]
    /// that a newer one overtook (two threads hop to it: the forwarder and a play).
    seq: u64,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            station: None,
            icy_title: None,
            state: PlaybackState::Idle,
            seq: 0,
        }
    }
}

impl Inner {
    fn info(&mut self) -> Option<Info> {
        self.seq += 1;
        now_playing(
            &self.state,
            self.station.as_ref(),
            self.icy_title.as_deref(),
            self.seq,
        )
    }
}

/// The three remote commands Ondar enables (M5e1, decision 6: next/previous stay disabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    Toggle,
    Play,
    Pause,
}

/// What a remote command does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(AudioCommand),
    /// Replay the current station through [`play`].
    PlayStation(Station),
    /// Handled, nothing to do (`Success`): the key must not pass on to another app while Ondar
    /// is connecting.
    Ignore,
    /// No current station (`NoActionableNowPlayingItem`): the system may pass the key on.
    NothingToPlay,
}

/// A remote command against the engine's state and the current station (A1: the last a play
/// named in this process, or none). `Idle` / `Error` replay the station; `Paused` resumes
/// (Toggle as `TogglePlayPause`, resolved on the engine thread, decision 4); `Playing` pauses;
/// the in-flight states (`Connecting`, `Buffering`, `Reconnecting`) ignore every command, as the
/// transport does (Q3). A `Paused` whose session the paused close ended is the same `Paused`.
pub fn on_remote(cmd: Remote, state: &PlaybackState, station: Option<&Station>) -> Action {
    match (state, cmd) {
        (PlaybackState::Idle | PlaybackState::Error { .. }, Remote::Toggle | Remote::Play) => {
            station.map_or(Action::NothingToPlay, |s| Action::PlayStation(s.clone()))
        }
        (PlaybackState::Paused | PlaybackState::Playing, Remote::Toggle) => {
            Action::Send(AudioCommand::TogglePlayPause)
        }
        (PlaybackState::Paused, Remote::Play) => Action::Send(AudioCommand::Resume),
        (PlaybackState::Playing, Remote::Pause) => Action::Send(AudioCommand::Pause),
        (
            PlaybackState::Idle | PlaybackState::Error { .. } | PlaybackState::Paused,
            Remote::Pause,
        )
        | (PlaybackState::Playing, Remote::Play)
        | (
            PlaybackState::Connecting
            | PlaybackState::Buffering
            | PlaybackState::Reconnecting { .. },
            _,
        ) => Action::Ignore,
    }
}

/// Now Playing's playback state, mapped to `MPNowPlayingPlaybackState` in `media.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpState {
    Playing,
    Paused,
    Stopped,
    Interrupted,
}

/// What Now Playing shows, owned, so the main thread reads nothing shared.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub playback: MpState,
    /// The station's name.
    pub title: String,
    /// The ICY title, shown as the artist line.
    pub subtitle: Option<String>,
    /// [`Inner::seq`] when this was made.
    pub seq: u64,
}

/// The D table: `Connecting`, `Buffering`, `Playing` → Playing; `Paused` → Paused;
/// `Reconnecting` → Interrupted; `Idle` → Stopped with the info kept (decision 3); `Error` →
/// Stopped with no subtitle. The title is the station's name. With no station: `None`, nothing
/// published (A1).
pub fn now_playing(
    state: &PlaybackState,
    station: Option<&Station>,
    icy_title: Option<&str>,
    seq: u64,
) -> Option<Info> {
    let station = station?;
    let playback = match state {
        PlaybackState::Connecting | PlaybackState::Buffering | PlaybackState::Playing => {
            MpState::Playing
        }
        PlaybackState::Paused => MpState::Paused,
        PlaybackState::Reconnecting { .. } => MpState::Interrupted,
        PlaybackState::Idle | PlaybackState::Error { .. } => MpState::Stopped,
    };
    let subtitle = match state {
        PlaybackState::Error { .. } => None,
        _ => icy_title.map(str::to_string),
    };
    Some(Info {
        playback,
        title: station.name.clone(),
        subtitle,
        seq,
    })
}

impl NowPlaying {
    /// Store `station` and send its `Play` through `send`, under one lock, so the stored station
    /// and the engine's command order agree whoever plays (the page, a key). Returns what Now
    /// Playing shows now (the new name).
    pub fn play(&self, send: impl FnOnce(AudioCommand), station: Station) -> Option<Info> {
        let mut inner = lock(&self.inner);
        send(AudioCommand::Play {
            url: station.url.clone(),
            station_id: station.uuid.clone(),
            bitrate_kbps: station.bitrate_kbps,
        });
        inner.station = Some(station);
        inner.info()
    }

    /// An engine event, as the forwarder sees it: a `State` or a `Metadata` updates what Now
    /// Playing shows and returns it (`None` for any other event, and before the first play).
    pub fn on_event(&self, ev: &EngineEvent) -> Option<Info> {
        let mut inner = lock(&self.inner);
        match ev {
            EngineEvent::State(s) => {
                if *s == PlaybackState::Connecting {
                    inner.icy_title = None;
                }
                inner.state = s.clone();
            }
            EngineEvent::Metadata(m) => inner.icy_title.clone_from(&m.title),
            _ => return None,
        }
        inner.info()
    }

    /// A remote command against the engine's `state` and the current station ([`on_remote`]).
    pub fn remote(&self, cmd: Remote, state: &PlaybackState) -> Action {
        on_remote(cmd, state, lock(&self.inner).station.as_ref())
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
    let info = state
        .now_playing
        .play(|cmd| state.engine.send(cmd), station.clone());
    if let Err(e) = app.emit(events::STATION, station) {
        log::warn!("failed to emit {}: {e}", events::STATION);
    }
    if let Some(info) = info {
        crate::media::publish(app, info);
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

    fn error() -> PlaybackState {
        PlaybackState::Error {
            code: ondar_audio::ErrorCode::Network,
            message: "gone".into(),
        }
    }

    /// M5e1 commit 5, `on_remote`: 3 commands × 7 states × {a current station, none}, 42
    /// cases, each row naming both expected actions. **Fails if** any case maps to another
    /// action: a key acting while connecting (the transport would not), Play passing on with a
    /// station to replay (A1), a pause from `Paused`, Toggle sent as Pause/Resume instead of
    /// `TogglePlayPause` (decision 4), or `NothingToPlay` while a session exists.
    #[test]
    fn on_remote_every_command_in_every_state_with_and_without_a_station() {
        use Action::{Ignore, NothingToPlay, PlayStation, Send};
        use Remote::{Pause, Play, Toggle};
        let s = station("cur");
        let replay = || PlayStation(station("cur"));
        let toggle = || Send(AudioCommand::TogglePlayPause);
        // (command, state, with the station, with none)
        let rows: Vec<(Remote, PlaybackState, Action, Action)> = vec![
            (Toggle, PlaybackState::Idle, replay(), NothingToPlay),
            (Play, PlaybackState::Idle, replay(), NothingToPlay),
            (Pause, PlaybackState::Idle, Ignore, Ignore),
            (Toggle, error(), replay(), NothingToPlay),
            (Play, error(), replay(), NothingToPlay),
            (Pause, error(), Ignore, Ignore),
            (Toggle, PlaybackState::Paused, toggle(), toggle()),
            (
                Play,
                PlaybackState::Paused,
                Send(AudioCommand::Resume),
                Send(AudioCommand::Resume),
            ),
            (Pause, PlaybackState::Paused, Ignore, Ignore),
            (Toggle, PlaybackState::Playing, toggle(), toggle()),
            (Play, PlaybackState::Playing, Ignore, Ignore),
            (
                Pause,
                PlaybackState::Playing,
                Send(AudioCommand::Pause),
                Send(AudioCommand::Pause),
            ),
            (Toggle, PlaybackState::Connecting, Ignore, Ignore),
            (Play, PlaybackState::Connecting, Ignore, Ignore),
            (Pause, PlaybackState::Connecting, Ignore, Ignore),
            (Toggle, PlaybackState::Buffering, Ignore, Ignore),
            (Play, PlaybackState::Buffering, Ignore, Ignore),
            (Pause, PlaybackState::Buffering, Ignore, Ignore),
            (
                Toggle,
                PlaybackState::Reconnecting { attempt: 1 },
                Ignore,
                Ignore,
            ),
            (
                Play,
                PlaybackState::Reconnecting { attempt: 1 },
                Ignore,
                Ignore,
            ),
            (
                Pause,
                PlaybackState::Reconnecting { attempt: 1 },
                Ignore,
                Ignore,
            ),
        ];
        assert_eq!(rows.len() * 2, 42);
        for (cmd, state, with, without) in rows {
            assert_eq!(
                on_remote(cmd, &state, Some(&s)),
                with,
                "{cmd:?} {state:?} with a station"
            );
            assert_eq!(
                on_remote(cmd, &state, None),
                without,
                "{cmd:?} {state:?} with none"
            );
        }
    }

    /// M5e1 commit 5, `now_playing` per state (the D table), with a station and an ICY title.
    /// **Fails if** a state maps to another `MpState`, the title is not the station's name, the
    /// subtitle is dropped outside `Error`, or kept in `Error`.
    #[test]
    fn now_playing_follows_the_d_table() {
        let s = station("FIP");
        let rows = [
            (PlaybackState::Connecting, MpState::Playing, Some("T")),
            (PlaybackState::Buffering, MpState::Playing, Some("T")),
            (PlaybackState::Playing, MpState::Playing, Some("T")),
            (PlaybackState::Paused, MpState::Paused, Some("T")),
            (
                PlaybackState::Reconnecting { attempt: 2 },
                MpState::Interrupted,
                Some("T"),
            ),
            (PlaybackState::Idle, MpState::Stopped, Some("T")),
            (error(), MpState::Stopped, None),
        ];
        for (state, playback, subtitle) in rows {
            assert_eq!(
                now_playing(&state, Some(&s), Some("T"), 7),
                Some(Info {
                    playback,
                    title: "FIP".into(),
                    subtitle: subtitle.map(str::to_string),
                    seq: 7,
                }),
                "{state:?}"
            );
        }
    }

    /// M5e1 commit 5: the subtitle is cleared by the new session's `Connecting`, applied through
    /// the holder, so a title from the old station never lands on the new one (the Step 0 probe
    /// cleared it nowhere). And `Error` drops the subtitle with the title held. **What it
    /// pins:** `Metadata("A")` → subtitle A; `State(Connecting)` → none; a later title shows;
    /// `Error` → no subtitle, the name kept, `Stopped`; each `Info` newer than the last.
    /// **Fails if** `on_event` does not clear on `Connecting`, or `seq` stops growing.
    #[test]
    fn connecting_clears_the_subtitle_and_error_drops_it() {
        let now = NowPlaying::default();
        let first = now.play(|_| {}, station("FIP")).unwrap();
        let ev = |t: &str| {
            EngineEvent::Metadata(ondar_audio::IcyMetadata {
                title: Some(t.into()),
            })
        };
        let a = now.on_event(&ev("A")).unwrap();
        assert_eq!(a.subtitle.as_deref(), Some("A"));
        let c = now
            .on_event(&EngineEvent::State(PlaybackState::Connecting))
            .unwrap();
        assert_eq!(c.subtitle, None);
        assert_eq!(c.title, "FIP");
        let b = now.on_event(&ev("B")).unwrap();
        assert_eq!(b.subtitle.as_deref(), Some("B"));
        let e = now.on_event(&EngineEvent::State(error())).unwrap();
        assert_eq!(
            (e.playback, e.subtitle.clone(), e.title.as_str()),
            (MpState::Stopped, None, "FIP")
        );
        assert!(first.seq < a.seq && a.seq < c.seq && c.seq < b.seq && b.seq < e.seq);
    }

    /// A1's pin: before the process's first play the holder yields no `Info`, so `publish` is
    /// never reached and Ondar does not become the Now Playing app at launch. **Fails if** a state
    /// or a title before the first play produces one (mutation: `now_playing` without its
    /// station check).
    #[test]
    fn nothing_is_published_before_the_first_play() {
        let now = NowPlaying::default();
        assert_eq!(
            now.on_event(&EngineEvent::State(PlaybackState::Playing)),
            None
        );
        let title = EngineEvent::Metadata(ondar_audio::IcyMetadata {
            title: Some("A".into()),
        });
        assert_eq!(now.on_event(&title), None);
        assert_eq!(
            now_playing(&PlaybackState::Playing, None, Some("A"), 1),
            None
        );
        assert_eq!(
            now.remote(Remote::Toggle, &PlaybackState::Idle),
            Action::NothingToPlay
        );
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
        let _ = now.play(|c| tx.send(c).unwrap(), station("a"));
        let _ = now.play(|c| tx.send(c).unwrap(), station("b"));
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
