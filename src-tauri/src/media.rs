//! Media keys and Now Playing (M5e1): `MPRemoteCommandCenter` and `MPNowPlayingInfoCenter`
//! through `objc2-media-player` 0.3.2 (bought, ONDAR.md "M5e1"). macOS only, as the repo is.
//!
//! The decisions are pure and tested in `now_playing.rs` ([`on_remote`], [`now_playing`]); this
//! module is the FFI around them. **Threading:** every function here that touches `MediaPlayer` or
//! `AppKit` takes a `MainThreadMarker`, which objc2 hands out only on the main thread
//! (`MainThreadMarker::new()` is `None` elsewhere), so the compiler holds the rule. This file never
//! calls `MainThreadMarker::new_unchecked`; a source scan below pins that. The remote handlers run
//! on the main thread (measured, Step 0 C), take two short locks (`engine.state()`, the
//! `NowPlaying` holder) and never wait.
//!
//! **Not claimed before the first play (A1):** the handlers are registered in `setup`, but nothing
//! is published until a play, so a launch leaves the keys with Music or `MediaMonkey`.
//!
//! [`on_remote`]: crate::now_playing::on_remote
//! [`now_playing`]: crate::now_playing::now_playing

use std::cell::{Cell, OnceCell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSMutableDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPMediaItemArtwork, MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork,
    MPMediaItemPropertyTitle, MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyIsLiveStream,
    MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent,
    MPRemoteCommandHandlerStatus,
};
use tauri::{AppHandle, Manager};

use ondar_audio::{AudioCommand, PlaybackState};

use crate::AppState;
use crate::now_playing::{Action, Info, MpState, Remote};

thread_local! {
    /// The app icon as Now Playing's artwork, made once on the main thread at `register`.
    static ARTWORK: OnceCell<Retained<MPMediaItemArtwork>> = const { OnceCell::new() };
    /// The handlers' targets, kept for the process's life (the command centre holds them too).
    static TARGETS: RefCell<Vec<Retained<AnyObject>>> = const { RefCell::new(Vec::new()) };
    /// The newest [`Info::seq`] published: an older one that arrives later is dropped.
    static PUBLISHED: Cell<u64> = const { Cell::new(0) };
}

/// Register the remote commands, from `setup` (the main thread). Enables toggle, play and pause
/// with handlers; disables next, previous, the skips, the seeks, the position and the rate, so
/// Control Centre offers no scrubber or skip (decision 6).
pub fn register(mtm: MainThreadMarker, app: &AppHandle) {
    // SAFETY: the shared command centre and its commands, on the main thread (`mtm`).
    unsafe {
        let cc = MPRemoteCommandCenter::sharedCommandCenter();
        add(mtm, app, Remote::Toggle, &cc.togglePlayPauseCommand());
        add(mtm, app, Remote::Play, &cc.playCommand());
        add(mtm, app, Remote::Pause, &cc.pauseCommand());
        let off: [Retained<MPRemoteCommand>; 8] = [
            cc.nextTrackCommand(),
            cc.previousTrackCommand(),
            Retained::into_super(cc.skipForwardCommand()),
            Retained::into_super(cc.skipBackwardCommand()),
            cc.seekForwardCommand(),
            cc.seekBackwardCommand(),
            Retained::into_super(cc.changePlaybackPositionCommand()),
            Retained::into_super(cc.changePlaybackRateCommand()),
        ];
        for c in &off {
            c.setEnabled(false);
        }
    }
    ARTWORK.with(|a| {
        if let Some(art) = artwork(mtm) {
            let _ = a.set(art);
        }
    });
    log::info!("media keys registered: toggle, play, pause");
}

fn add(_mtm: MainThreadMarker, app: &AppHandle, cmd: Remote, command: &MPRemoteCommand) {
    let app = app.clone();
    let block = RcBlock::new(move |_ev: NonNull<MPRemoteCommandEvent>| on_press(&app, cmd));
    // SAFETY: on the main thread (`_mtm`); the block lives as long as the target, which is kept.
    let target = unsafe {
        command.setEnabled(true);
        command.addTargetWithHandler(&block)
    };
    TARGETS.with(|t| t.borrow_mut().push(target));
}

/// One press: the engine's state, the holder's decision, the action, one INFO line.
fn on_press(app: &AppHandle, cmd: Remote) -> MPRemoteCommandHandlerStatus {
    if MainThreadMarker::new().is_none() {
        log::warn!(
            "remote cmd={} off the main thread; refused",
            remote_label(cmd)
        );
        return MPRemoteCommandHandlerStatus::CommandFailed;
    }
    let state = app.state::<AppState>();
    let before = state.engine.state();
    let action = state.now_playing.remote(cmd, &before);
    log::info!(
        "remote cmd={} state={} action={}",
        remote_label(cmd),
        state_label(&before),
        action_label(&action)
    );
    match action {
        Action::Send(c) => {
            state.engine.send(c);
            MPRemoteCommandHandlerStatus::Success
        }
        Action::PlayStation(station) => {
            crate::now_playing::play(app, station);
            MPRemoteCommandHandlerStatus::Success
        }
        Action::Ignore => MPRemoteCommandHandlerStatus::Success,
        Action::NothingToPlay => MPRemoteCommandHandlerStatus::NoActionableNowPlayingItem,
    }
}

/// Publish `info` from any thread: one hop to the main thread, carrying it owned.
pub fn publish(app: &AppHandle, info: Info) {
    if let Err(e) = app.run_on_main_thread(move || match MainThreadMarker::new() {
        Some(mtm) => publish_on_main(mtm, &info),
        None => log::warn!("now playing: the main-thread hop ran off the main thread"),
    }) {
        log::warn!("now playing: main-thread hop failed: {e}");
    }
}

fn publish_on_main(_mtm: MainThreadMarker, info: &Info) {
    if info.seq <= PUBLISHED.get() {
        return;
    }
    PUBLISHED.set(info.seq);
    let dict: Retained<NSMutableDictionary<NSString, AnyObject>> = NSMutableDictionary::new();
    let title = NSString::from_str(&info.title);
    let live = NSNumber::new_bool(true);
    // SAFETY: the keys are MediaPlayer's constant strings; the centre is used on the main
    // thread (`_mtm`).
    unsafe {
        dict.insert(MPMediaItemPropertyTitle, &title);
        if let Some(subtitle) = &info.subtitle {
            dict.insert(MPMediaItemPropertyArtist, &NSString::from_str(subtitle));
        }
        dict.insert(MPNowPlayingInfoPropertyIsLiveStream, &live);
        ARTWORK.with(|a| {
            if let Some(art) = a.get() {
                dict.insert(MPMediaItemPropertyArtwork, art);
            }
        });
        let center = MPNowPlayingInfoCenter::defaultCenter();
        center.setNowPlayingInfo(Some(&dict));
        center.setPlaybackState(playback_state(info.playback));
    }
}

fn playback_state(s: MpState) -> MPNowPlayingPlaybackState {
    match s {
        MpState::Playing => MPNowPlayingPlaybackState::Playing,
        MpState::Paused => MPNowPlayingPlaybackState::Paused,
        MpState::Stopped => MPNowPlayingPlaybackState::Stopped,
        MpState::Interrupted => MPNowPlayingPlaybackState::Interrupted,
    }
}

/// The app's own icon as artwork: no file read, no fetch (station favicons stay out).
fn artwork(mtm: MainThreadMarker) -> Option<Retained<MPMediaItemArtwork>> {
    let image: Retained<NSImage> = NSApplication::sharedApplication(mtm).applicationIconImage()?;
    let size: CGSize = image.size();
    let handler = RcBlock::new(move |_size: CGSize| NonNull::from(&*image));
    // SAFETY: on the main thread (`mtm`); the block returns a pointer to the image it owns,
    // valid for the block's life, which the artwork holds.
    Some(unsafe {
        MPMediaItemArtwork::initWithBoundsSize_requestHandler(
            MPMediaItemArtwork::alloc(),
            size,
            &handler,
        )
    })
}

fn remote_label(cmd: Remote) -> &'static str {
    match cmd {
        Remote::Toggle => "toggle",
        Remote::Play => "play",
        Remote::Pause => "pause",
    }
}

fn state_label(s: &PlaybackState) -> &'static str {
    match s {
        PlaybackState::Idle => "idle",
        PlaybackState::Connecting => "connecting",
        PlaybackState::Buffering => "buffering",
        PlaybackState::Playing => "playing",
        PlaybackState::Paused => "paused",
        PlaybackState::Reconnecting { .. } => "reconnecting",
        PlaybackState::Error { .. } => "error",
    }
}

fn action_label(a: &Action) -> &'static str {
    match a {
        Action::Send(AudioCommand::TogglePlayPause) => "toggle",
        Action::Send(AudioCommand::Resume) => "resume",
        Action::Send(AudioCommand::Pause) => "pause",
        Action::Send(_) => "send",
        Action::PlayStation(_) => "play_station",
        Action::Ignore => "ignore",
        Action::NothingToPlay => "nothing_to_play",
    }
}

#[cfg(test)]
mod tests {
    /// M5e1: the threading rule is held by `MainThreadMarker` parameters, and the one escape
    /// hatch is `MainThreadMarker::new_unchecked`. **What it pins:** this file's code (outside
    /// this test) never names it. **Fails if** anyone adds it (mutation: a
    /// `new_unchecked()` call in `register`).
    #[test]
    fn media_never_conjures_a_main_thread_marker() {
        let src = include_str!("media.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        let needle = ["new_", "unchecked"].concat();
        let hits: Vec<_> = code
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| l.contains(&needle))
            .collect();
        assert!(hits.is_empty(), "{hits:?}");
    }
}
