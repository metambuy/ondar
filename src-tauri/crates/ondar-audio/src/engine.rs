//! The audio engine.
//!
//! Threads:
//!
//! * **engine thread** (`ondar-audio`): owns the output device (`MixerDeviceSink`), the
//!   `Player`, and the Tokio runtime that `stream-download` needs. It receives
//!   [`AudioCommand`]s over a channel, polling on a short timeout so it can also supervise
//!   ring buffering (see [`Engine::tick`]) — it never blocks on network or decoding itself.
//! * **one decode thread per session** (`ondar-decode`): opens the HTTP stream, probes it with
//!   Symphonia, decodes into the ring buffer, and exits when its session is cancelled. It no
//!   longer drives buffering/reconnect *state* — it only reports ring occupancy — because it
//!   can be blocked for many seconds inside a network read (see `stream::build_client`'s
//!   `read_timeout`) and must not be the sole thing detecting starvation.
//! * **audio callback** (cpal, owned by rodio): pulls from `Equalizer<RingSource>`. Never
//!   blocks, never allocates.
//!
//! The UI only ever sees [`EngineEvent`]s and the [`PlaybackState`] snapshot.

use std::collections::VecDeque;
use std::io::{Read, Seek};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rodio::decoder::{DecoderBuilder, DecoderError};
use rodio::source::UniformSourceIterator;
use rodio::{ChannelCount, DeviceSinkBuilder, MixerDeviceSink, Player, SampleRate, Source};
use rtrb::PushError;
use tokio_util::sync::CancellationToken;

use crate::adts::AdtsReader;
use crate::build::{
    self, Arrivals, Bounded, BuildBounds, BuildCause, BuildClock, BuildInputs, ClockedReader,
};
use crate::eq::{EqGains, Equalizer};
use crate::icy::IcyReader;
use crate::reconnect::{Backoff, STABLE_AFTER};
use crate::ring::{self, RingSource, RingStats};
use crate::stream;
use crate::types::{EngineEvent, ErrorCode, IcyMetadata, PlaybackState, ReconnectInfo, StreamInfo};

/// How often the engine thread wakes up (absent a command) to run [`Engine::tick`].
const TICK_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub enum AudioCommand {
    /// `bitrate_kbps`: the station record's, for the prefetch (M3b commit 6); `None` when the
    /// record has none (the floor applies).
    Play {
        url: String,
        station_id: String,
        bitrate_kbps: Option<u32>,
    },
    Pause,
    Resume,
    Stop,
    SetVolume(f32),
}

/// Handle held by the application. Cheap to clone.
#[derive(Clone)]
pub struct AudioEngine {
    tx: Sender<AudioCommand>,
    shared: Shared,
}

impl AudioEngine {
    /// Spawn the engine thread. `user_agent` goes on every HTTP request (`Ondar/<version>`).
    pub fn start(user_agent: String) -> (AudioEngine, Receiver<EngineEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let shared = Shared {
            state: Arc::new(Mutex::new(PlaybackState::Idle)),
            events: ev_tx,
            session: Arc::new(Mutex::new(Session::default())),
            gains: EqGains::default(),
            paused: Arc::new(AtomicBool::new(false)),
        };
        let engine_shared = shared.clone();
        thread::Builder::new()
            .name("ondar-audio".into())
            .spawn(move || Engine::new(engine_shared, user_agent).run(cmd_rx))
            .expect("spawn audio engine thread");
        (AudioEngine { tx: cmd_tx, shared }, ev_rx)
    }

    pub fn send(&self, cmd: AudioCommand) {
        // A send error means the engine thread is gone; there is nothing useful to do.
        let _ = self.tx.send(cmd);
    }

    pub fn state(&self) -> PlaybackState {
        self.shared.state.lock().unwrap().clone()
    }

    pub fn eq(&self) -> &EqGains {
        &self.shared.gains
    }
}

/// State shared between the engine thread, decode threads, and the public handle.
#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<PlaybackState>>,
    events: Sender<EngineEvent>,
    gains: EqGains,
    paused: Arc<AtomicBool>,
    /// The session the last `Play` began — its generation, the id `Started` will carry, and
    /// whether it has reached `Playing` yet — under **one** lock with every state write that
    /// comes from a session, so the write and the decision it feeds cannot interleave with
    /// the engine thread's `cancel` + `begin_session` (`/code-review` finding 1, 2026-09-23).
    session: Arc<Mutex<Session>>,
}

/// The engine's record of the live session. `generation` counts every `begin_session` and
/// every session end; a [`SessionCtx`] carries the generation it was born with, and a write
/// through it is dropped once the number has moved on. Compared under the lock that also
/// decides `started`, so a stale decode thread's `Playing` can neither overwrite the successor's
/// state nor consume its `Started` (a vote and a recent for a station that has not opened).
#[derive(Default)]
struct Session {
    generation: u64,
    station_id: Option<String>,
    /// `Started` is emitted on the first `Playing` of a session — whatever route led there —
    /// and never again until the next `Play` (M3b commit 5, the click endpoint's rule: once per
    /// `play` call, on the first `Playing` after it; not on a resume or a reconnect of a session
    /// that already played).
    started: bool,
}

impl Shared {
    /// A new session, on `Play`: the id `Started` will carry, the flag down, and a fresh
    /// generation, returned for the session's [`SessionCtx`]. Called on the engine thread after
    /// the previous session is cancelled and before `Connecting`; a write from the previous
    /// session's decode thread carries the old generation and is dropped by [`Self::write_state`]
    /// however late it lands.
    fn begin_session(&self, station_id: String) -> u64 {
        let mut session = self.session.lock().unwrap();
        session.generation += 1;
        session.station_id = Some(station_id);
        session.started = false;
        session.generation
    }

    /// The session `generation` is over: its writes are dropped from here on, even before a
    /// successor begins (a stale `Playing` between `stop`'s cancel and its `Idle`). A generation
    /// that is already not the live one changes nothing.
    fn end_session(&self, generation: u64) {
        let mut session = self.session.lock().unwrap();
        if session.generation == generation {
            session.generation += 1;
        }
    }

    /// The engine thread's own state writes: `Connecting`, `Paused`, `Idle`, `Error` — never
    /// gated on a session.
    fn set_state(&self, s: PlaybackState) {
        self.write_state(None, s);
    }

    /// One state write. With `from`, the write belongs to a session and lands only while that
    /// generation is the live one — decided under the same lock as the write, the `State` event
    /// and the `Started` decision, so nothing from the engine thread can slip between them.
    /// `State(Playing)` is sent before `Started`, so a listener sees the state first; every
    /// entry into `Playing` after the first (an underrun's refill, a resume, a reconnect after
    /// playback) finds `started` set.
    fn write_state(&self, from: Option<u64>, s: PlaybackState) {
        let mut session = self.session.lock().unwrap();
        if from.is_some_and(|generation| generation != session.generation) {
            return;
        }
        {
            let mut guard = self.state.lock().unwrap();
            if *guard == s {
                return;
            }
            *guard = s.clone();
        }
        let playing = s == PlaybackState::Playing;
        let _ = self.events.send(EngineEvent::State(s));
        if playing && !session.started {
            session.started = true;
            let station_id = session.station_id.clone().unwrap_or_default();
            let _ = self.events.send(EngineEvent::Started { station_id });
        }
    }

    fn state(&self) -> PlaybackState {
        self.state.lock().unwrap().clone()
    }

    /// A session event (`StreamInfo`, an ICY title, an internal `Reconnect`), sent only while
    /// `generation` is the live session — decided under the session lock, with the send inside
    /// it, as [`Self::write_state`] decides a state (review 2, finding 2: a check before the
    /// send left a window for the engine thread's `cancel` + `begin_session`, and a stale
    /// decode thread's `StreamInfo` or title landed on the next station's row). The channel is
    /// an unbounded `mpsc`, so the send never blocks under the lock.
    ///
    /// There is no ungated `emit`: every engine event today belongs to a session. An event
    /// from the audio path (M5's spectrum) must not come through here — it would take the
    /// session lock from the audio callback, which never blocks.
    fn emit_from(&self, generation: u64, ev: EngineEvent) {
        let session = self.session.lock().unwrap();
        if session.generation != generation {
            return;
        }
        let _ = self.events.send(ev);
        drop(session);
    }
}

/// Per-session cancellation and cross-thread state. Cloned into the decode thread; also held
/// by the engine thread (`Engine::session`) so `tick()` can supervise it.
#[derive(Clone)]
struct SessionCtx {
    cancel: Arc<AtomicBool>,
    /// The download task's token, set by the decode thread once a stream is open, so `Stop`
    /// can unblock a read that is waiting on the network.
    download: Arc<Mutex<Option<CancellationToken>>>,
    /// Set by the decode thread once a ring is attached, cleared at teardown. `None` means
    /// there is no live ring to supervise (connecting, reconnecting, or between sessions).
    ring: Arc<Mutex<Option<Arc<RingStats>>>>,
    /// Exponential backoff for this session's connection attempts. Shared because the decode
    /// thread advances it on failure but the engine thread resets it once playback has been
    /// stable for a while (`Engine::tick`) — that reset used to happen in the decode thread,
    /// but the trigger condition now lives in the engine.
    backoff: Arc<Mutex<Backoff>>,
    /// `stream-download`'s internal reconnect count for this session (its own idle-
    /// `retry_timeout` recovery, not one of `backoff`'s external attempts) — advanced by the
    /// `Settings::on_reconnect` callback attached in `stream::open`, read by `Engine::tick`
    /// to emit `EngineEvent::Reconnect` when it changes.
    reconnect_count: Arc<AtomicU64>,
    /// The [`Session`] generation this context was born with (`Shared::begin_session`); every
    /// state write through it is checked against the live one, under the lock.
    generation: u64,
    /// The decoder build's clock and phase (defect B; the review fixes' F1): stamped by the
    /// decode thread, read and swapped by `Engine::tick`. Its bounds are fixed for the session:
    /// [`BuildBounds::for_prefetch`] in production, shorter ones in the tests.
    clock: Arc<BuildClock>,
    shared: Shared,
}

impl SessionCtx {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(token) = self.download.lock().unwrap().take() {
            token.cancel();
        }
        self.shared.end_session(self.generation);
    }

    /// A state update from this session, dropped once the session is over so a stale decode
    /// thread can neither overwrite the state of its successor nor take its `Started`. The
    /// decision is `Shared::write_state`'s, under its lock — a flag read here and a write there
    /// left a window for the engine thread's `cancel` + `begin_session` between the two
    /// (`/code-review` finding 1, 2026-09-23).
    fn set_state(&self, s: PlaybackState) {
        self.shared.write_state(Some(self.generation), s);
    }

    /// A session event from this session, dropped once the session is over
    /// ([`Shared::emit_from`]).
    fn emit(&self, ev: EngineEvent) {
        self.shared.emit_from(self.generation, ev);
    }

    fn sleep_cancellable(&self, d: Duration) {
        let deadline = Instant::now() + d;
        while Instant::now() < deadline && !self.cancelled() {
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn ring_stats(&self) -> Option<Arc<RingStats>> {
        self.ring.lock().unwrap().clone()
    }
}

/// The one format every session is converted to before the EQ: the output sink's own rate and
/// channel count, read from `MixerDeviceSink::config()` when the sink opens (rodio 0.22.2 builds
/// the mixer from that config, `stream.rs:497`). Fixed for the life of the sink.
///
/// Why a session converts itself (defect A, measured 2026-09-24, ONDAR.md): rodio's mixer wraps
/// the `Player`'s whole queue in **one** `UniformSourceIterator` (`mixer.rs:62`), which reads its
/// input's rate and channels only when a span ends. `RingSource` has no span end, so that
/// converter kept the first station's format for the whole process, and every later station
/// played at the wrong speed. Every chain now reports this format, so the mixer's converter
/// is an identity whatever it locked to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OutputFormat {
    channels: ChannelCount,
    sample_rate: SampleRate,
}

/// A session's source chain: the ring, converted to the output's fixed format, then the EQ. The
/// converter sits **before** the EQ so the EQ (and, from M5, the spectrum) always runs at one
/// rate. Built per ring, at attach, on the decode thread: `UniformSourceIterator` bootstraps once
/// from the ring's rate and channels (its span is `None`), which is right precisely because it
/// is per session; when the rates differ, its construction reads two frames, and the ring is at
/// `fill_target` here, so it never reads an empty ring or counts a false underrun.
fn output_chain(
    src: RingSource,
    out: OutputFormat,
    gains: EqGains,
) -> Equalizer<UniformSourceIterator<RingSource>> {
    Equalizer::new(
        UniformSourceIterator::new(src, out.channels, out.sample_rate),
        gains,
    )
}

struct Engine {
    shared: Shared,
    client: reqwest::Client,
    rt: tokio::runtime::Runtime,
    sink: Option<MixerDeviceSink>,
    player: Option<Arc<Player>>,
    /// The sink's format, set with `sink`; see [`OutputFormat`].
    output: Option<OutputFormat>,
    session: Option<SessionCtx>,
    volume: f32,
    /// When the current session last transitioned into `Playing`, as observed by `tick()`.
    /// Drives the "reset backoff after `STABLE_AFTER` of continuous playback" rule.
    playing_since: Option<Instant>,
    /// State as of the last `tick()`, used only to detect transitions into `Playing`.
    last_state: PlaybackState,
    /// The `RingStats` `tick()` is currently tracking. Compared by pointer each tick: a
    /// change (including from `None`) means a fresh ring just attached (first connect or a
    /// reconnect), and `last_underruns`/`ready_ticks`/`underrun_ticks` all reset — a stale
    /// ring's counts must never leak into a new one's.
    current_ring: Option<Arc<RingStats>>,
    /// `stats.underruns` as of the last tick for `current_ring`. `new_underrun` is derived by
    /// comparing against this, not by clearing a flag — see `RingStats::underruns`.
    last_underruns: u64,
    /// Consecutive ticks with the ring refilled past threshold and no new underrun; reset to
    /// 0 on any tick that fails either condition. Must reach the dwell threshold before
    /// `tick()` resumes output.
    ready_ticks: u32,
    /// The dwell chosen when the current `Buffering` wait began, held for its duration so a
    /// rolling prune cannot shorten it mid-wait. `None` outside `Buffering`. See
    /// [`dwell_for_tick`].
    latched_dwell: Option<u32>,
    /// `RingStats::pushed` as of the last tick, and how many consecutive ticks it has not
    /// advanced. Drives the watchdog; see [`advance_progress`].
    last_pushed: u64,
    ticks_since_progress: u32,
    /// Cached at construction — see [`watchdog_ticks`].
    watchdog_ticks: u32,
    /// Monotonic tick counter for `current_ring`'s lifetime, used to window `underrun_ticks`.
    tick_index: u64,
    /// `tick_index` of each underrun event still within `UNDERRUN_WINDOW_TICKS`, oldest
    /// first. Its length is `recent_underruns`, which lengthens the resume dwell after
    /// repeated underruns rather than only reacting to the most recent one.
    underrun_ticks: VecDeque<u64>,
    /// The session's `reconnect_count` `tick()` is currently tracking, compared by pointer
    /// each tick (same idiom as `current_ring`) — a change means a new `Play`, so
    /// `last_reconnect_count` resets rather than leaking a previous session's count forward.
    current_reconnect_counter: Option<Arc<AtomicU64>>,
    /// `reconnect_count` as of the last tick for `current_reconnect_counter`; a change from
    /// this is what triggers `EngineEvent::Reconnect`.
    last_reconnect_count: u64,
}

impl Engine {
    fn new(shared: Shared, user_agent: String) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ondar-net")
            .enable_all()
            .build()
            .expect("tokio runtime");
        Self {
            shared,
            client: stream::build_client(&user_agent),
            rt,
            sink: None,
            player: None,
            output: None,
            session: None,
            volume: 1.0,
            playing_since: None,
            last_state: PlaybackState::Idle,
            current_ring: None,
            last_underruns: 0,
            ready_ticks: 0,
            latched_dwell: None,
            last_pushed: 0,
            ticks_since_progress: 0,
            watchdog_ticks: watchdog_ticks(),
            tick_index: 0,
            underrun_ticks: VecDeque::new(),
            current_reconnect_counter: None,
            last_reconnect_count: 0,
        }
    }

    fn run(mut self, rx: Receiver<AudioCommand>) {
        loop {
            match rx.recv_timeout(TICK_INTERVAL) {
                Ok(AudioCommand::Play {
                    url,
                    station_id,
                    bitrate_kbps,
                }) => self.play(url, station_id, bitrate_kbps),
                Ok(AudioCommand::Pause) => self.pause(),
                Ok(AudioCommand::Resume) => self.resume(),
                Ok(AudioCommand::Stop) => self.stop(),
                Ok(AudioCommand::SetVolume(v)) => self.set_volume(v),
                Err(RecvTimeoutError::Timeout) => {}
                // Handle dropped: shut down cleanly.
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
        }
        self.stop();
    }

    /// Buffering supervision. Runs on every wake-up of the engine thread (every
    /// `TICK_INTERVAL`, or right after handling a command), so starvation is noticed even
    /// while the decode thread is blocked on a stalled network read — unlike the old design,
    /// which detected it inside the decode loop and so never noticed a hang at all.
    fn tick(&mut self) {
        let current = self.shared.state();
        if current == PlaybackState::Playing {
            if self.last_state != PlaybackState::Playing {
                self.playing_since = Some(Instant::now());
            }
        } else {
            self.playing_since = None;
        }
        self.last_state = current.clone();

        let Some(session) = self.session.clone() else {
            return;
        };

        // A different counter than last tick means a new `Play` session; reset so a previous
        // session's count can't leak into this one's first delta.
        let is_new_session = !self
            .current_reconnect_counter
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &session.reconnect_count));
        if is_new_session {
            self.current_reconnect_counter = Some(session.reconnect_count.clone());
            self.last_reconnect_count = session.reconnect_count.load(Ordering::Relaxed);
        }
        let reconnect_count = session.reconnect_count.load(Ordering::Relaxed);
        if reconnect_count != self.last_reconnect_count {
            self.last_reconnect_count = reconnect_count;
            session.emit(EngineEvent::Reconnect(ReconnectInfo {
                count: reconnect_count,
            }));
        }

        // The build bound (defect B), before the ring's early return: a session that is
        // building has no ring yet, and a bound behind that return would never run. The engine
        // keeps no build state: every duration comes from the decode thread's stamps for the
        // build the word names (review fixes F1, finding 6), so no build can read another's.
        let word = session.clock.word();
        if build::phase_of(word) == build::phase::PROBING {
            let inputs = session.clock.inputs(reconnect_count);
            let outcome = decide_tick(
                &current,
                TickInputs {
                    build: Some(inputs),
                    ..Default::default()
                },
            );
            if let Some(Transition::FailBuild(cause)) = outcome.transition {
                Self::fail_build(&session, word, cause, &inputs);
            }
            return;
        }

        let Some(stats) = session.ring_stats() else {
            return;
        };

        self.tick_index += 1;

        // A different ring than last tick (including no ring at all last tick) means a fresh
        // connection just attached — first connect or a reconnect. Its RingStats starts at
        // underruns == 0; carrying over a previous ring's counts would misfire new_underrun.
        let is_new_ring = !self
            .current_ring
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(r, &stats));
        let current_underruns = stats.underruns.load(Ordering::Relaxed);
        if is_new_ring {
            self.current_ring = Some(stats.clone());
            self.last_underruns = current_underruns;
            self.ready_ticks = 0;
            self.latched_dwell = None;
            self.last_pushed = stats.pushed.load(Ordering::Relaxed);
            self.ticks_since_progress = 0;
            self.underrun_ticks.clear();
        }
        let new_underrun = current_underruns > self.last_underruns;
        self.last_underruns = current_underruns;

        if new_underrun {
            self.underrun_ticks.push_back(self.tick_index);
        }
        while self
            .underrun_ticks
            .front()
            .is_some_and(|&t| self.tick_index - t >= UNDERRUN_WINDOW_TICKS as u64)
        {
            self.underrun_ticks.pop_front();
        }
        let recent_underruns = self.underrun_ticks.len() as u32;

        let fill = stats.fill.load(Ordering::Relaxed);
        if is_refilled(fill, stats.capacity) && !new_underrun {
            self.ready_ticks = self.ready_ticks.saturating_add(1);
        } else {
            self.ready_ticks = 0;
        }

        let user_paused = self.shared.paused.load(Ordering::Relaxed);
        let stable = self
            .playing_since
            .is_some_and(|t| t.elapsed() >= STABLE_AFTER);

        advance_progress(
            &mut self.last_pushed,
            &mut self.ticks_since_progress,
            stats.pushed.load(Ordering::Relaxed),
        );

        let dwell = dwell_for_tick(
            &mut self.latched_dwell,
            current == PlaybackState::Buffering,
            recent_underruns,
        );

        let outcome = decide_tick(
            &current,
            TickInputs {
                new_underrun,
                fill,
                capacity: stats.capacity,
                user_paused,
                stable,
                ready_ticks: self.ready_ticks,
                dwell,
                ticks_since_progress: self.ticks_since_progress,
                watchdog_ticks: self.watchdog_ticks,
                build: None,
            },
        );

        match outcome.transition {
            Some(Transition::PauseAndBuffer) => {
                log::debug!("underrun: pausing output until the ring refills");
                if let Some(p) = &self.player {
                    p.pause();
                }
                session.set_state(PlaybackState::Buffering);
            }
            Some(Transition::ResumePlaying) => {
                if let Some(p) = &self.player {
                    p.play();
                }
                session.set_state(PlaybackState::Playing);
            }
            Some(Transition::ResumePaused) => {
                session.set_state(PlaybackState::Paused);
            }
            // Returned only while probing, which is handled above and returns.
            Some(Transition::FailBuild(_)) => {}
            Some(Transition::FailSession) => {
                log::warn!(
                    "watchdog: {} ticks buffering with no decode progress; failing the session",
                    self.ticks_since_progress
                );
                // Cancel in place, never `take()`. Whether this actually unblocks a read parked
                // in `stream-download` is the one unverified link in the chain, and `take()`
                // would remove the token that `SessionCtx::cancel` needs — so a watchdog that
                // failed to work would also have removed the user's Stop as an escape hatch.
                // `CancellationToken::cancel` takes `&self`; there is no reason to remove it.
                if let Some(t) = session.download.lock().unwrap().as_ref() {
                    t.cancel();
                }
                // Give the recovery a full window before firing again.
                self.ticks_since_progress = 0;
            }
            None => {}
        }
        if outcome.reset_backoff {
            session.backoff.lock().unwrap().reset();
        }
    }

    /// The build bound fired: take the build from the decode thread with the one swap, from the
    /// exact `word` this tick read, and only if that won, cancel the download in place (as
    /// `FailSession` does, never `take()`, so a later Stop still finds the token). The cancel
    /// returns a blocked `build()` in about 1 ms (Step 0, S1); the decode thread's own swap then
    /// fails and reads the cause from the phase. A lost swap means `build()` returned first, or
    /// a new build began (another seq): nothing to do.
    ///
    /// **Lock scope and order:** `download` is the only lock taken, and it is held from the swap
    /// through the cancel. The decode thread stores the next build's token under the same lock,
    /// and only after its own swap for this build has failed, so the token cancelled here is
    /// this build's — never the next open's (finding 6, which the ≥ 1 s backoff used to cover).
    /// Nothing inside blocks: the swap, and `CancellationToken::cancel`, which is synchronous.
    /// The log line waits until the lock is dropped.
    fn fail_build(session: &SessionCtx, word: u64, cause: BuildCause, inputs: &BuildInputs) {
        let download = session.download.lock().unwrap();
        if !session.clock.bound(word, cause, inputs) {
            return;
        }
        if let Some(t) = download.as_ref() {
            t.cancel();
        }
        drop(download);
        log::warn!(
            "build bound: {cause:?} after {:.2} s (first byte {}, longest gap {:.2} s, {} \
             reconnects); cancelling the download",
            inputs.since_start.as_secs_f64(),
            inputs.since_first_byte.map_or_else(
                || "none".to_string(),
                |d| format!("{:.2} s ago", d.as_secs_f64())
            ),
            inputs.longest_gap.as_secs_f64(),
            inputs.reconnects
        );
    }

    /// Open the output device on first use so a missing device is reported as a playback
    /// error rather than a crash at startup.
    fn ensure_player(&mut self) -> Result<(Arc<Player>, OutputFormat), String> {
        if let (Some(p), Some(out)) = (&self.player, self.output) {
            return Ok((p.clone(), out));
        }
        let mut sink = DeviceSinkBuilder::open_default_sink().map_err(|e| e.to_string())?;
        sink.log_on_drop(false);
        let out = OutputFormat {
            channels: sink.config().channel_count(),
            sample_rate: sink.config().sample_rate(),
        };
        log::info!(
            "output opened sample_rate={} channels={}",
            out.sample_rate,
            out.channels
        );
        let player = Arc::new(Player::connect_new(sink.mixer()));
        player.set_volume(self.volume);
        self.sink = Some(sink);
        self.player = Some(player.clone());
        self.output = Some(out);
        Ok((player, out))
    }

    fn play(&mut self, url: String, station_id: String, bitrate_kbps: Option<u32>) {
        self.cancel_session();
        let generation = self.shared.begin_session(station_id.clone());
        self.shared.paused.store(false, Ordering::Relaxed);
        if let Some(p) = &self.player {
            // Silence the previous station now, not when the new one has buffered.
            p.clear();
        }

        let url = match stream::parse_url(&url) {
            Ok(u) => u,
            Err(e) => {
                self.shared.set_state(PlaybackState::Error {
                    code: e.code,
                    message: e.message,
                });
                return;
            }
        };
        let (player, output) = match self.ensure_player() {
            Ok(p) => p,
            Err(message) => {
                self.shared.set_state(PlaybackState::Error {
                    code: ErrorCode::Device,
                    message,
                });
                return;
            }
        };

        let prefetch = stream::prefetch_bytes(bitrate_kbps);
        let ctx = SessionCtx {
            cancel: Arc::new(AtomicBool::new(false)),
            download: Arc::new(Mutex::new(None)),
            ring: Arc::new(Mutex::new(None)),
            backoff: Arc::new(Mutex::new(Backoff::default())),
            reconnect_count: Arc::new(AtomicU64::new(0)),
            generation,
            clock: Arc::new(BuildClock::new(BuildBounds::for_prefetch(prefetch))),
            shared: self.shared.clone(),
        };
        self.session = Some(ctx.clone());
        self.shared.set_state(PlaybackState::Connecting);

        let client = self.client.clone();
        let handle = self.rt.handle().clone();
        log::info!(
            "play station_id={station_id} bitrate_kbps={bitrate_kbps:?} prefetch_bytes={prefetch}"
        );
        thread::Builder::new()
            .name(format!("ondar-decode:{station_id}"))
            .spawn(move || run_session(ctx, url, client, handle, player, prefetch, output))
            .expect("spawn decode thread");
    }

    fn pause(&mut self) {
        if let (Some(p), Some(_)) = (&self.player, &self.session) {
            self.shared.paused.store(true, Ordering::Relaxed);
            p.pause();
            self.shared.set_state(PlaybackState::Paused);
        }
    }

    fn resume(&mut self) {
        if let (Some(p), Some(_)) = (&self.player, &self.session) {
            self.shared.paused.store(false, Ordering::Relaxed);
            if self.shared.state() == PlaybackState::Paused {
                p.play();
                // tick() will downgrade this to Buffering if the ring is starved.
                self.shared.set_state(PlaybackState::Playing);
            }
            // If we are Buffering, tick() resumes output once the ring refills.
        }
    }

    fn stop(&mut self) {
        self.cancel_session();
        self.shared.paused.store(false, Ordering::Relaxed);
        if let Some(p) = &self.player {
            p.clear();
        }
        self.shared.set_state(PlaybackState::Idle);
    }

    fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if let Some(p) = &self.player {
            p.set_volume(self.volume);
        }
    }

    fn cancel_session(&mut self) {
        if let Some(s) = self.session.take() {
            s.cancel();
        }
    }
}

/// `Read + Seek` can't be combined directly in a trait object (only one non-auto trait is
/// allowed), so this supertrait bundles them.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

type BoxedReader = Box<dyn ReadSeek + Send + Sync + 'static>;

/// Body of a decode thread: connect → probe → decode into the ring, reconnecting on drop.
fn run_session(
    ctx: SessionCtx,
    url: reqwest::Url,
    client: reqwest::Client,
    rt: tokio::runtime::Handle,
    player: Arc<Player>,
    prefetch_bytes: u64,
    output: OutputFormat,
) {
    // Whether this session has ever opened its stream. A terminal answer ends the session
    // only before that: afterwards the same 404 is a mount mid-restart, and the backoff
    // carries it (`/code-review` finding 4, 2026-09-22).
    let mut opened_once = false;
    // Whether this session has pushed a decoded sample into a ring — "has produced audio"
    // (defect B, B2; review round 7, P1). An unrecognised format is terminal only before
    // that: a mount that once played had a valid format, so garbage on a reconnect is a
    // source restarting, as a 404 there is. Not `opened_once` (set before the build), and not
    // "has built a decoder": `build()` returns `Ok` with an empty decoder after a false header
    // when the read ends (defect B Step 0, S3), which would let a mount that sends a false
    // header and closes back off for ever instead of ending.
    let mut produced_audio = false;
    loop {
        if ctx.cancelled() {
            return;
        }

        // 1. Connect. Each open has its own arrival clock (review 2, G1), stamped by its
        // download task from the moment `open` spawns it — so the prefetch's arrivals, which can
        // land before `begin_build`, are kept, and no earlier open's stamp is in it.
        let arrivals = Arc::new(Arrivals::new());
        let opened = match rt.block_on(stream::open(
            &client,
            url.clone(),
            ctx.reconnect_count.clone(),
            arrivals.clone(),
            prefetch_bytes,
        )) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("connect failed: {}", e.message);
                if !retry_or_fail(
                    &ctx,
                    e.code,
                    e.message,
                    e.terminal && !opened_once,
                    e.retry_after,
                ) {
                    return;
                }
                continue;
            }
        };
        opened_once = true;
        *ctx.download.lock().unwrap() = Some(opened.reader.cancellation_token());
        if ctx.cancelled() {
            opened.reader.cancel_download();
            return;
        }
        // The build is supervised from here (defect B): the token the bound cancels is in
        // `download` before the engine can see `PROBING`, stored under the lock `fail_build`
        // holds from its swap to its cancel. `begin_build` stamps the build's start and the
        // internal reconnect count the cause compares against, installs this open's arrivals,
        // then publishes `PROBING`.
        let word = ctx
            .clock
            .begin_build(ctx.reconnect_count.load(Ordering::Relaxed), arrivals);

        // 2. Probe / build the decoder. The first-byte reader (review fixes F1) is the bottom of
        // the chain, under `IcyReader`, on both source kinds: HLS's reader is the same
        // `stream::Reader`. Its first read returns once the prefetch is met.
        let icy = IcyReader::new(
            ClockedReader::new(opened.reader, ctx.clock.clone(), prefetch_bytes),
            opened.metaint,
            title_sink(&ctx),
        );
        // The ADTS front end (defect B C4) sits **after** `IcyReader` — a metadata block can hold
        // `FF F9`, and inside the frames it would read as a sync loss — and applies to an HTTP
        // stream labelled `audio/aac*` or `audio/x-aac*` only (review P3: HLS segments are
        // normalised already).
        let reader: BoxedReader =
            if wants_adts_front_end(opened.kind, opened.content_type.as_deref()) {
                Box::new(AdtsReader::new(icy))
            } else {
                Box::new(icy)
            };
        let mut builder = DecoderBuilder::new()
            .with_data(reader)
            .with_seekable(false)
            .with_gapless(false);
        if let Some(ct) = &opened.content_type {
            builder = builder.with_mime_type(ct);
        }
        let built = builder.build();
        // A user's cancel (Stop, or `play` of another station) during the build: whatever
        // `build()` returned belongs to a session that is over. After a cancel it can return an
        // empty `Ok` (Step 0, S3); returning here spares a dead session its ring. Since review 2's
        // G2 this is control flow only: `StreamInfo` is generation-gated under the session lock,
        // which also closes the window between this check and the emit. The bound's own cancel
        // does not set this flag; it is decided by the phase below.
        if ctx.cancelled() {
            return;
        }
        // The phase, never the variant: after the cancel, `build()` has returned
        // `UnrecognizedFormat`, `IoError` and a decoder that yields nothing (Step 0, S3), so
        // the bound is checked on `Ok` and `Err` alike, before any arm reads the result. The
        // engine's swap wrote the cause into the phase.
        if let Err(bounded) = ctx.clock.finish_build(word) {
            // A decoder on a cancelled download is dead.
            drop(built);
            let (code, message, terminal) = build_bound_cause(
                bounded.cause,
                &decided_inputs(
                    &ctx.clock,
                    &bounded,
                    ctx.reconnect_count.load(Ordering::Relaxed),
                ),
                opened.content_type.as_deref(),
                produced_audio,
            );
            // The engine's `fail_build` line is the bound's one warning, with the figures it
            // decided on; this is the page's message, for the record (review 2, finding 5).
            log::info!("build bound: {message}");
            if !retry_or_fail(&ctx, code, message, terminal, None) {
                return;
            }
            continue;
        }
        let mut decoder = match built {
            Ok(d) => d,
            Err(DecoderError::UnrecognizedFormat) => {
                let message = format!(
                    "could not identify the audio format ({})",
                    opened.content_type.as_deref().unwrap_or("no content-type")
                );
                if !retry_or_fail(
                    &ctx,
                    ErrorCode::UnsupportedFormat,
                    message,
                    !produced_audio,
                    None,
                ) {
                    return;
                }
                continue;
            }
            Err(e) => {
                log::warn!("decoder failed to open: {e}");
                if !retry_or_fail(&ctx, ErrorCode::Decode, e.to_string(), false, None) {
                    return;
                }
                continue;
            }
        };

        let sample_rate = decoder.sample_rate();
        let channels = decoder.channels();
        ctx.emit(EngineEvent::StreamInfo(StreamInfo {
            content_type: opened.content_type.clone(),
            bitrate_kbps: opened.bitrate_kbps,
            station_name: opened.station_name.clone(),
            sample_rate: sample_rate.get(),
            channels: channels.get(),
        }));
        ctx.set_state(PlaybackState::Buffering);

        // 3. Pre-fill half the ring, then attach the source so playback starts without a gap.
        //    From then on, this thread only reports occupancy (`stats.fill`, on the same
        //    cadence as before); the engine thread's `tick()` is what pauses/resumes output
        //    and flips `Buffering`/`Playing` in response, since it isn't at risk of blocking
        //    on the network the way this thread is.
        let (mut ring, source) = ring::ring(sample_rate, channels);
        *ctx.ring.lock().unwrap() = Some(ring.stats.clone());
        let mut source = Some(source);
        let fill_target = ring.stats.capacity / 2;
        let mut samples_since_check: u32 = 0;

        loop {
            if ctx.cancelled() {
                *ctx.ring.lock().unwrap() = None;
                return;
            }
            let Some(sample) = decoder.next() else {
                // EOF, or a read error (including our own cancellation, checked above).
                break;
            };

            // Push, waiting while the ring is full.
            let mut pending = sample;
            loop {
                match ring.producer.push(pending) {
                    Ok(()) => break,
                    Err(PushError::Full(v)) => {
                        pending = v;
                        if ctx.cancelled() {
                            *ctx.ring.lock().unwrap() = None;
                            return;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            }
            produced_audio = true;

            samples_since_check += 1;
            if samples_since_check < 1024 {
                continue;
            }
            samples_since_check = 0;

            let filled = ring.stats.capacity - ring.producer.slots();
            ring.stats.fill.store(filled, Ordering::Relaxed);
            // Only reached after 1024 successful pushes, so it stops dead while this thread
            // is blocked in a read — which is exactly the watchdog's signal.
            ring.stats.pushed.fetch_add(1024, Ordering::Relaxed);

            if let Some(src) = source.take_if(|_| filled >= fill_target) {
                player.clear();
                player.append(output_chain(src, output, ctx.shared.gains.clone()));
                if ctx.shared.paused.load(Ordering::Relaxed) {
                    ctx.set_state(PlaybackState::Paused);
                } else {
                    player.play();
                    ctx.set_state(PlaybackState::Playing);
                }
            }
        }

        // Dropping the producer lets the RingSource drain and end, which empties the Player
        // queue; the reconnect path attaches a fresh source.
        *ctx.ring.lock().unwrap() = None;
        drop(ring);
        if ctx.cancelled() {
            return;
        }
        log::warn!("stream ended or read failed; reconnecting");
        if !retry_or_fail(
            &ctx,
            ErrorCode::Network,
            "the stream ended unexpectedly".to_string(),
            false,
            None,
        ) {
            return;
        }
    }
}

/// The bound's outcome (defect B; the review fixes' F1), from the cause the engine swapped into
/// the phase and the build as the decode thread reads it after the cancel. The rule that chose
/// the cause is `decide_tick`'s first arm; this only names it:
///
/// - **`Format`** → `UnsupportedFormat`: bytes kept arriving for the whole format bound, from
///   the first one, and nothing synced. Terminal only while the session has never produced
///   audio, as the `UnrecognizedFormat` arm is.
/// - **`Starved`** → `Network`, never terminal: the format bound passed with a gap of `starved`
///   or more between network arrivals, or a completed internal reconnect.
/// - **`NoBytes`** → `Network`, never terminal: no byte reached the decoder within the
///   no-bytes bound — a trickle cannot be told from a slow healthy stream.
///
/// Returns the code, the message and whether it is terminal.
fn build_bound_cause(
    cause: BuildCause,
    build: &BuildInputs,
    content_type: Option<&str>,
    produced_audio: bool,
) -> (ErrorCode, String, bool) {
    match cause {
        BuildCause::Format => (
            ErrorCode::UnsupportedFormat,
            format!(
                "no decodable audio in the first {} of the stream ({})",
                format_secs(build.bounds.format),
                content_type.unwrap_or("no content-type")
            ),
            !produced_audio,
        ),
        BuildCause::Starved => {
            let reconnected = match build.reconnects {
                0 => String::new(),
                1 => "; re-established once".to_string(),
                n => format!("; re-established {n} times"),
            };
            (
                ErrorCode::Network,
                format!(
                    "no audio arrived while starting: the connection stalled ({:.1} s without \
                     data{reconnected})",
                    build.longest_gap.as_secs_f64()
                ),
                false,
            )
        }
        BuildCause::NoBytes => (
            ErrorCode::Network,
            format!(
                "no audio arrived within {} of connecting",
                format_secs(build.bounds.no_bytes)
            ),
            false,
        ),
    }
}

/// The ICY title callback for `ctx`'s session: each title is a session event, dropped once the
/// session is over ([`Shared::emit_from`]; review 2, finding 2 — a read a cancel unblocks returns
/// what is buffered, and if that completes a metadata block the stale title would land on the
/// next station's row). It runs on the decode thread inside a read, holding no lock.
fn title_sink(ctx: &SessionCtx) -> crate::icy::TitleCallback {
    let (shared, generation) = (ctx.shared.clone(), ctx.generation);
    Box::new(move |title| {
        shared.emit_from(
            generation,
            EngineEvent::Metadata(IcyMetadata { title: Some(title) }),
        );
    })
}

/// The inputs the bound's message is written from: the clock as it stands for the rest, and
/// the gap and reconnect count **the engine decided on** — not the clock read again after the
/// cancel, whose open gap has grown since (review 2, finding 4: the page and the engine's log
/// line could disagree).
fn decided_inputs(clock: &BuildClock, bounded: &Bounded, reconnect_count: u64) -> BuildInputs {
    BuildInputs {
        longest_gap: bounded.longest_gap,
        reconnects: bounded.reconnects,
        ..clock.inputs(reconnect_count)
    }
}

/// `20 s`, `2 s`, `115.3 s`: a duration as the bound's messages print it.
fn format_secs(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

/// Whether a stream gets the ADTS front end (defect B, B1): an HTTP stream whose content type's
/// essence starts with `audio/aac` — `audio/aac` and `audio/aacp`, under which every `FFF9` body
/// in Step 0 was served (S4) — or `audio/x-aac`, which real servers send too (review fixes F4,
/// finding 7: Antena 1's HLS segments; the front end passes a non-ADTS body through, so a
/// mislabelled stream costs at most a 16 KiB head delay); and never the HLS source, whose
/// segments `hls::segment` has already walked (review P3).
fn wants_adts_front_end(kind: stream::SourceKind, content_type: Option<&str>) -> bool {
    kind == stream::SourceKind::Http
        && content_type.is_some_and(|ct| {
            let essence = stream::mime_essence(ct);
            stream::starts_with_ignore_ascii_case(essence, "audio/aac")
                || stream::starts_with_ignore_ascii_case(essence, "audio/x-aac")
        })
}

/// A server's `Retry-After` lengthens the backoff's delay up to this; past it the session
/// would look dead to the user, and the backoff's own 16 s is already the longest wait shown.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);

/// The retry policy, by cause. A **terminal** failure (`StreamError::terminal` — a non-HTTP
/// answer such as `ICY 200 OK`, or 401/403/404/410 — and only while the session has never
/// opened; or an unrecognised format while the session has never produced audio; see
/// `run_session`) fails the session on the spot: retrying cannot change what the
/// server says. Everything else (network, 5xx, 408/429, decoder, a stream that ended, any
/// answer on a reconnect) goes through the 1/2/4/8/16 s backoff, each delay stretched to the
/// server's `Retry-After` when it sent one (capped), and fails with the code of the last
/// attempt once the five are spent. Until 2026-09-22 the cause was ignored here and only
/// labelled the final error; M3a acceptance item 8 measured an ICY server at
/// `Reconnecting { attempt: 4 }` after 12 s and `Error { code: Http }` only at 31 s, six
/// requests — `session_tests` pins the request counts. The same day's review (finding 4)
/// narrowed the terminal set from "any 4xx" and confined it to the first open.
fn retry_or_fail(
    ctx: &SessionCtx,
    code: ErrorCode,
    message: String,
    terminal: bool,
    retry_after: Option<Duration>,
) -> bool {
    if terminal {
        log::warn!("not retrying, the answer would not change: {message}");
        ctx.set_state(PlaybackState::Error { code, message });
        return false;
    }
    let mut backoff = ctx.backoff.lock().unwrap();
    match backoff.next() {
        Some((attempt, delay)) => {
            drop(backoff);
            let delay = match retry_after {
                Some(ra) => delay.max(ra.min(RETRY_AFTER_CAP)),
                None => delay,
            };
            ctx.set_state(PlaybackState::Reconnecting { attempt });
            ctx.sleep_cancellable(delay);
            !ctx.cancelled()
        }
        None => {
            let attempts = backoff.attempt();
            drop(backoff);
            ctx.set_state(PlaybackState::Error {
                code,
                message: format!("{message} (gave up after {attempts} attempts)"),
            });
            false
        }
    }
}

// Resume/dwell/instability thresholds below are unmeasured. `UNDERRUN_WINDOW_TICKS` and
// `UNDERRUN_PANIC_COUNT` were investigated and kept (ONDAR.md, "The unstable dwell was selected
// and then cancelled"); `RESUME_FILL_NUM/DEN` is observed and deliberately not swept. The dwell
// lengths and the fill threshold remain untuned.

/// Resume once the ring is at least this fraction full (3/4 = 75%).
const RESUME_FILL_NUM: usize = 3;
const RESUME_FILL_DEN: usize = 4;
/// Ticks the ring must stay refilled with no new underrun before resuming (1.0 s at
/// `TICK_INTERVAL`).
const DWELL_TICKS: u32 = 10;
/// Longer dwell applied once a connection has shown `UNDERRUN_PANIC_COUNT`+ underruns
/// recently (4.0 s) — an unstable connection gets more time to prove itself before resuming.
const DWELL_TICKS_UNSTABLE: u32 = 40;
/// Window (in ticks; 30 s at `TICK_INTERVAL`) over which underrun events count toward
/// `DWELL_TICKS_UNSTABLE` kicking in.
const UNDERRUN_WINDOW_TICKS: u32 = 300;
const UNDERRUN_PANIC_COUNT: u32 = 3;

/// Shared by `decide_tick` and `Engine::tick`'s `ready_ticks` bookkeeping.
fn is_refilled(fill: usize, capacity: usize) -> bool {
    fill * RESUME_FILL_DEN >= capacity * RESUME_FILL_NUM
}

/// How long `Buffering` may run with no decode progress before the session is failed and handed
/// to the existing `Backoff`.
///
/// Derived from `stream::retry_timeout` rather than hardcoded, because that value is
/// env-overridable (`ONDAR_RETRY_TIMEOUT_SECS`) and a fixed constant would silently become wrong
/// the moment it is raised. The bound to clear is the longest *legitimate* no-progress interval:
/// bytes stop, `stream-download`'s idle reconnect fires after `retry_timeout`, and the new
/// connection delivers. Measured at ~5.0 s with the 5 s default, so 3x plus a 15 s floor leaves
/// ample margin over a recovery that would have succeeded on its own.
fn watchdog_ticks() -> u32 {
    let base = (stream::retry_timeout() * 3).max(Duration::from_secs(15));
    (base.as_millis() / TICK_INTERVAL.as_millis()) as u32
}

/// Progress bookkeeping for the watchdog. Separated from `Engine::tick` so it is testable
/// without a `Player`.
fn advance_progress(last_pushed: &mut u64, ticks_since_progress: &mut u32, pushed: u64) {
    if pushed != *last_pushed {
        *last_pushed = pushed;
        *ticks_since_progress = 0;
    } else {
        *ticks_since_progress = ticks_since_progress.saturating_add(1);
    }
}

/// How long a connection with this many recent underruns must dwell before resuming. Selected
/// once, on entry to `Buffering`, and then latched — see [`dwell_for_tick`].
fn select_dwell(recent_underruns: u32) -> u32 {
    if recent_underruns >= UNDERRUN_PANIC_COUNT {
        DWELL_TICKS_UNSTABLE
    } else {
        DWELL_TICKS
    }
}

/// The dwell latch. Returns the dwell to apply this tick, updating `latched`.
///
/// This exists because `recent_underruns` is recomputed from scratch every tick, and the
/// window prunes on a rolling basis (`Engine::tick`) — so without latching, an underrun ageing
/// out of `UNDERRUN_WINDOW_TICKS` *midway through a wait* drops the count below
/// `UNDERRUN_PANIC_COUNT` and collapses the dwell from `DWELL_TICKS_UNSTABLE` back to
/// `DWELL_TICKS`. Since `ready_ticks` has been accumulating the whole time, the resume then
/// fires immediately. Measured before this fix: a wait that selected 40 ticks resumed at 11,
/// and at a 13.3 s stall cadence *every* unstable wait was cut short this way, making the
/// unstable path 100% ineffective. `UNDERRUN_WINDOW_TICKS` and `UNDERRUN_PANIC_COUNT` are
/// unchanged; the latch is what makes them mean what their names say.
///
/// Latches on the first tick observed in `Buffering`, not in the `PauseAndBuffer` arm, which
/// keeps it pure. That relies on no prune landing in the one-tick gap between the underrun
/// registering (state still `Playing`) and the first `Buffering` tick — possible only if an
/// underrun is exactly `UNDERRUN_WINDOW_TICKS` old at that instant. If a latched dwell ever
/// looks wrong, that gap is the first place to look, and the fix is to latch in the
/// `PauseAndBuffer` arm instead.
fn dwell_for_tick(latched: &mut Option<u32>, is_buffering: bool, recent_underruns: u32) -> u32 {
    if !is_buffering {
        *latched = None;
        return select_dwell(recent_underruns);
    }
    *latched.get_or_insert_with(|| select_dwell(recent_underruns))
}

/// What `Engine::tick()` should do, decided in isolation from the real `Player`/`SessionCtx`
/// so this logic is unit-testable without a live audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transition {
    PauseAndBuffer,
    ResumePlaying,
    ResumePaused,
    /// Buffering has lasted too long with no decode progress. Fail the session so the
    /// existing `Backoff` + a fresh `stream::open()` can recover it.
    FailSession,
    /// The decoder's build has reached a bound (defect B), for the cause named: take the build
    /// with the swap and cancel the download ([`Engine::fail_build`]).
    FailBuild(BuildCause),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TickOutcome {
    transition: Option<Transition>,
    reset_backoff: bool,
}

/// Pure decision function behind [`Engine::tick`]. `state` is the state observed at the start
/// of the tick (before the returned outcome is applied). `new_underrun` is whether the ring's
/// underrun counter advanced since the previous tick (not raw "is silent right now" — see
/// `RingStats::underruns`); `ready_ticks` is the caller's count of consecutive good ticks, and
/// `dwell` the latched threshold it must reach, chosen by [`dwell_for_tick`] rather than here.
///
/// Pause and resume can never be emitted in the same tick: once a fresh underrun is seen while
/// not already `Buffering`, this returns immediately with `PauseAndBuffer`, even if the ring
/// looks fully refilled by the time this tick runs. That refilled-on-arrival case is real, not
/// hypothetical — an Icecast burst-on-connect (default burst-size 64 KB, ~4 s at 128 kbps) can
/// fill the entire 2 s ring within one 100 ms tick, and `stream-download`'s own
/// `retry_timeout` reconnect (default 5 s idle) fires on every stall — so without the
/// early-return, a normal internal reconnect would flash `Buffering` then `Playing` back to
/// back on every stall.
/// Inputs to [`decide_tick`], as named fields rather than a positional list. The list had
/// reached eight arguments — including two adjacent `bool`s and two adjacent `u32`s, where a
/// transposition at any of the call sites would still compile and silently change behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TickInputs {
    new_underrun: bool,
    fill: usize,
    capacity: usize,
    user_paused: bool,
    stable: bool,
    ready_ticks: u32,
    /// Latched on entry to `Buffering` by [`dwell_for_tick`]; this function does not derive it.
    dwell: u32,
    /// Consecutive ticks with no advance in `RingStats::pushed`.
    ticks_since_progress: u32,
    /// Threshold for the above; see [`watchdog_ticks`].
    watchdog_ticks: u32,
    /// The session's build, while it is `PROBING` (defect B): the decode thread's stamps as the
    /// engine read them this tick (review fixes F1). When set, only the build bound is decided;
    /// there is no ring yet.
    build: Option<BuildInputs>,
}

fn decide_tick(state: &PlaybackState, inputs: TickInputs) -> TickOutcome {
    let TickInputs {
        new_underrun,
        fill,
        capacity,
        user_paused,
        stable,
        ready_ticks,
        dwell,
        ticks_since_progress,
        watchdog_ticks,
        build,
    } = inputs;
    let mut out = TickOutcome {
        transition: None,
        reset_backoff: stable,
    };

    // The build bound (defect B), before the pause arm: a pause during `Connecting` sets
    // `Paused` while the decode thread is still building, and the build is no less stuck.
    // Review fixes F1: before the first byte only the no-bytes bound runs, always the network;
    // from the first byte the format bound, whose cause is the network if the bytes ever
    // stopped for `starved` (the longest gap, the open one included — a stall that resumed
    // just before the bound had seconds of bytes, not the bound's) or an internal reconnect
    // completed, and the format otherwise.
    if let Some(b) = build {
        out.transition = match b.since_first_byte {
            None if b.since_start >= b.bounds.no_bytes => {
                Some(Transition::FailBuild(BuildCause::NoBytes))
            }
            Some(since) if since >= b.bounds.format => Some(Transition::FailBuild(
                if b.reconnects > 0 || b.longest_gap >= b.bounds.starved {
                    BuildCause::Starved
                } else {
                    BuildCause::Format
                },
            )),
            _ => None,
        };
        return out;
    }

    // User-paused and not buffering: the callback isn't pulling, so nothing is starving in
    // any sense the user cares about. Refill silently, stay Paused.
    if user_paused && *state != PlaybackState::Buffering {
        return out;
    }

    if *state != PlaybackState::Buffering {
        if new_underrun {
            out.transition = Some(Transition::PauseAndBuffer);
        }
        return out; // never pause and resume in the same tick
    }

    // Watchdog. Exempt while user-paused: a paused session stops pulling, the ring fills, and
    // the decode thread parks on a full ring, so "no bytes arriving" is normal there and a
    // pause is unbounded in length. A longer threshold would only postpone a false positive,
    // never remove one.
    if !user_paused && ticks_since_progress >= watchdog_ticks {
        out.transition = Some(Transition::FailSession);
        return out;
    }

    if is_refilled(fill, capacity) && !new_underrun && ready_ticks >= dwell {
        out.transition = Some(if user_paused {
            Transition::ResumePaused
        } else {
            Transition::ResumePlaying
        });
    }
    out
}

#[cfg(test)]
mod session_tests {
    //! The retry policy, pinned at the level a unit test on `stream::open` could not reach:
    //! `run_session` itself, against real sockets on 127.0.0.1 that count the requests they
    //! get. No audio device — the `Player` hangs off a device-less `rodio::mixer::mixer`, so
    //! these run on a CI runner with no output hardware. Each test says what makes it fail.

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::Receiver;

    use super::*;

    /// Accept forever, count every request; connection `i` is answered with `responses[i]`
    /// (the last one repeated), the socket held briefly so the client sees a complete answer,
    /// then closed.
    fn scripted_server(responses: Vec<Vec<u8>>) -> (String, Arc<AtomicUsize>) {
        let (url, count, _times) = timed_scripted_server(responses);
        (url, count)
    }

    /// `scripted_server`, also recording when each connection was accepted, on the server's
    /// own clock: a gap between two requests measured there can only grow on a slow runner,
    /// never shrink, so a lower bound on it cannot be broken by slowness.
    fn timed_scripted_server(
        responses: Vec<Vec<u8>>,
    ) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<Instant>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let times = Arc::new(Mutex::new(Vec::new()));
        let at = times.clone();
        thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                at.lock().unwrap().push(Instant::now());
                let n = seen.fetch_add(1, Ordering::SeqCst);
                let response = &responses[n.min(responses.len() - 1)];
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(response);
                let _ = sock.flush();
                thread::sleep(Duration::from_millis(200));
            }
        });
        (format!("http://{addr}/stream"), count, times)
    }

    /// One fixed answer for every connection.
    fn counting_server(response: &'static [u8]) -> (String, Arc<AtomicUsize>) {
        scripted_server(vec![response.to_vec()])
    }

    /// A playable stream that ends: an HTTP 200 carrying `secs` of 16-bit mono 44.1 kHz WAV
    /// silence, then the connection closes. Shorter than the 2 s ring, so the decode loop
    /// reaches EOF without depending on the harness's drain thread to make room.
    fn wav_response(secs: f32) -> Vec<u8> {
        let rate = 44_100u32;
        let data_len = (rate as f32 * secs) as u32 * 2;
        let mut w = Vec::new();
        w.extend_from_slice(
            b"HTTP/1.0 200 OK\r\ncontent-type: audio/wav\r\nconnection: close\r\n\r\n",
        );
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&1u16.to_le_bytes()); // mono
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
        w.extend_from_slice(&2u16.to_le_bytes()); // block align
        w.extend_from_slice(&16u16.to_le_bytes()); // bits
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        w.resize(w.len() + data_len as usize, 0);
        w
    }

    struct Harness {
        ctx: SessionCtx,
        events: Receiver<EngineEvent>,
        /// A state event received during a helper's grace period, handed to the next helper
        /// call first so no state is lost between calls.
        held: Mutex<Option<PlaybackState>>,
        /// Every `Started` id seen while draining, in order.
        started: Mutex<Vec<String>>,
        /// Every `StreamInfo` seen while draining, as (sample_rate, channels), in order.
        infos: Mutex<Vec<(u32, u16)>>,
        /// Every ICY title seen while draining, in order.
        titles: Mutex<Vec<String>>,
        /// The decode thread's name, unique per harness: the captured log lines are keyed by it
        /// ([`adts_lines`]).
        decode_thread: String,
        /// Every `Reconnect` count seen while draining (stream-download's internal reconnects,
        /// emitted by `Engine::tick` — only a supervised harness has one), in order.
        reconnects: Mutex<Vec<u64>>,
        /// Set by `Drop`: stops the supervising engine thread of
        /// [`start_session_supervised`]; `None` for the tick-less harness.
        supervisor: Option<Arc<AtomicBool>>,
        /// The supervisor's own record of a build bound (defect B C4b; review fixes F1): the
        /// time from the build's first byte (the decode thread's stamp) to the tick whose swap
        /// bounded it, and how many ticks saw that build `PROBING` with a first byte meanwhile.
        /// `None` until a bound fires.
        bound_seen: Arc<Mutex<Option<(Duration, u32)>>>,
        /// The longest gap in the inputs of the tick that bounded the build, read just before
        /// that tick (review 2, G1): the figure the bound was decided on. `None` until a bound.
        bound_gap: Arc<Mutex<Option<Duration>>>,
        // Dropping the runtime while `run_session` still holds its handle would abort the
        // open; kept for the harness's lifetime.
        _rt: tokio::runtime::Runtime,
    }

    /// A session context for station `u1` whose events go to `ev_tx`.
    fn test_ctx(ev_tx: mpsc::Sender<EngineEvent>) -> SessionCtx {
        let shared = Shared {
            state: Arc::new(Mutex::new(PlaybackState::Connecting)),
            events: ev_tx,
            gains: EqGains::default(),
            paused: Arc::new(AtomicBool::new(false)),
            session: Arc::new(Mutex::new(Session::default())),
        };
        let generation = shared.begin_session("u1".into());
        SessionCtx {
            cancel: Arc::new(AtomicBool::new(false)),
            download: Arc::new(Mutex::new(None)),
            ring: Arc::new(Mutex::new(None)),
            backoff: Arc::new(Mutex::new(Backoff::default())),
            reconnect_count: Arc::new(AtomicU64::new(0)),
            generation,
            clock: Arc::new(BuildClock::new(BuildBounds::for_prefetch(
                stream::prefetch_bytes(None),
            ))),
            shared,
        }
    }

    /// Start `run_session` against `url` on its own thread, as `Engine::play` does, minus the
    /// device: the `Player` is connected to a bare mixer whose output a thread pulls at about
    /// twice real time — the device's stand-in. Without that pull nothing ever drops a queued
    /// source, and `Player::clear()` on a second open (a reconnect after playback) waits for
    /// the mixer forever (found by `started_is_sent_once_per_session_across_a_reconnect`,
    /// M3b commit 5: the second stream reached its fill target and never left `clear()`).
    fn start_session(url: &str) -> Harness {
        start_session_with(url, None, None, None)
    }

    /// `start_session`, plus the engine thread's supervision: a thread builds its own
    /// [`Engine`] (on that thread, so nothing `!Send` moves), gives it the session and calls
    /// `tick()` every `TICK_INTERVAL` until the harness drops — the build bound, the
    /// `Reconnect` events and the ring's transitions run as in production, with no `Player`
    /// (every `if let Some(p)` arm skips it). The session's bounds are [`test_bounds`] with a
    /// 2 s format bound: the production 20 s would make every test 20 s. Opt-in: the other
    /// tests keep the tick-less harness.
    fn start_session_supervised(url: &str) -> Harness {
        start_session_bounded(url, None, test_bounds(TEST_FORMAT))
    }

    /// `start_session_supervised` with the prefetch and every bound given (review fixes F1).
    /// `prefetch` `None` is production's for a station with no bitrate (the floor).
    fn start_session_bounded(url: &str, prefetch: Option<u64>, bounds: BuildBounds) -> Harness {
        start_session_with(url, Some(Duration::ZERO), prefetch, Some(bounds))
    }

    /// `start_session_supervised` with every tick slowed by `extra` (defect B C4b): a tick
    /// period that is not `TICK_INTERVAL`, as a loaded machine gives.
    fn start_session_supervised_slow(url: &str, extra: Duration) -> Harness {
        start_session_with(url, Some(extra), None, Some(test_bounds(TEST_FORMAT)))
    }

    /// The defect B tests' format bound: 2 s. At [`PACE`] that is 256 KiB, a quarter of
    /// Symphonia's 1 MiB search, so the two cannot be confused.
    const TEST_FORMAT: Duration = Duration::from_secs(2);

    /// Bounds with `format` and production's `starved`; `no_bytes` 60 s, production's floor,
    /// which a test that does not time the prefetch never reaches.
    fn test_bounds(format: Duration) -> BuildBounds {
        BuildBounds {
            format,
            starved: stream::retry_timeout(),
            no_bytes: Duration::from_secs(60),
        }
    }

    /// `ONDAR_TEST_TICK_DELAY_MS`: a sleep added to every supervised tick, standing in for a
    /// slow runner (or a loaded Mac: acceptance X1 measured a ~103 ms tick).
    fn tick_delay() -> Duration {
        Duration::from_millis(
            std::env::var("ONDAR_TEST_TICK_DELAY_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0),
        )
    }

    /// `supervised`: `Some(extra)` runs the engine's ticks, each slowed by `extra`. `bounds`
    /// replaces the session's production bounds.
    fn start_session_with(
        url: &str,
        supervised: Option<Duration>,
        prefetch: Option<u64>,
        bounds: Option<BuildBounds>,
    ) -> Harness {
        let (ev_tx, ev_rx) = mpsc::channel();
        let mut ctx = test_ctx(ev_tx);
        if let Some(b) = bounds {
            ctx.clock = Arc::new(BuildClock::new(b));
        }
        let bound_seen: Arc<Mutex<Option<(Duration, u32)>>> = Arc::new(Mutex::new(None));
        let bound_gap: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));
        let supervisor = supervised.map(|extra| {
            let stop = Arc::new(AtomicBool::new(false));
            let (session, halt, seen) = (ctx.clone(), stop.clone(), bound_seen.clone());
            let gap_seen = bound_gap.clone();
            let clock = ctx.clock.clone();
            let pause = TICK_INTERVAL + extra.max(tick_delay());
            thread::spawn(move || {
                let mut engine = Engine::new(session.shared.clone(), "Ondar/test".into());
                engine.session = Some(session);
                // (the build's seq, ticks that saw it probing with a first byte)
                let mut probing: Option<(u64, u32)> = None;
                while !halt.load(Ordering::SeqCst) {
                    let before = clock.word();
                    let pre = clock.inputs(0);
                    let first_byte = pre.since_first_byte.is_some();
                    if build::phase_of(before) == build::phase::PROBING && first_byte {
                        let seq = build::seq_of(before);
                        match &mut probing {
                            Some((s, n)) if *s == seq => *n += 1,
                            _ => probing = Some((seq, 1)),
                        }
                    }
                    engine.tick();
                    let after = clock.word();
                    let bounded = build::phase_of(before) == build::phase::PROBING
                        && build::seq_of(after) == build::seq_of(before)
                        && build::phase_of(after) >= build::phase::BOUND_FORMAT;
                    if bounded {
                        *gap_seen.lock().unwrap() = Some(pre.longest_gap);
                    }
                    if bounded
                        && let Some(since) = clock.inputs(0).since_first_byte
                        && let Some((_, n)) = probing.take()
                    {
                        *seen.lock().unwrap() = Some((since, n));
                    }
                    thread::sleep(pause);
                }
            });
            stop
        });
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let (mixer, mixer_out) = rodio::mixer::mixer(
            std::num::NonZero::new(2).expect("2"),
            std::num::NonZero::new(44_100).expect("44100"),
        );
        let player = Arc::new(Player::connect_new(&mixer));
        thread::spawn(move || {
            let mut out = mixer_out;
            loop {
                for _ in out.by_ref().take(4410) {}
                thread::sleep(Duration::from_millis(50));
            }
        });
        let url = stream::parse_url(url).expect("url");
        let client = stream::build_client("Ondar/test");
        let handle = rt.handle().clone();
        let session = ctx.clone();
        let prefetch = prefetch.unwrap_or_else(|| stream::prefetch_bytes(None));
        let output = OutputFormat {
            channels: std::num::NonZero::new(2).expect("2"),
            sample_rate: std::num::NonZero::new(44_100).expect("44100"),
        };
        capture_logs();
        static SESSIONS: AtomicUsize = AtomicUsize::new(0);
        let decode_thread = format!(
            "ondar-decode:test-{}",
            SESSIONS.fetch_add(1, Ordering::SeqCst)
        );
        thread::Builder::new()
            .name(decode_thread.clone())
            .spawn(move || run_session(session, url, client, handle, player, prefetch, output))
            .expect("spawn decode thread");
        Harness {
            ctx,
            events: ev_rx,
            held: Mutex::new(None),
            started: Mutex::new(Vec::new()),
            infos: Mutex::new(Vec::new()),
            titles: Mutex::new(Vec::new()),
            decode_thread,
            reconnects: Mutex::new(Vec::new()),
            supervisor,
            bound_seen,
            bound_gap,
            _rt: rt,
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            if let Some(stop) = &self.supervisor {
                stop.store(true, Ordering::SeqCst);
            }
        }
    }

    /// The test binary's logger (defect B C4): keeps the ADTS front end's lines, and since F1
    /// the build's, with the name of the thread that logged them and their level, so a test
    /// reads its own session's lines however many sessions run in parallel. Installed once;
    /// the other lines are dropped.
    struct Capture;

    static CAPTURED: Mutex<Vec<(String, log::Level, String)>> = Mutex::new(Vec::new());

    impl log::Log for Capture {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            let msg = r.args().to_string();
            if msg.starts_with("adts front end") || msg.starts_with("build") {
                let name = thread::current().name().unwrap_or_default().to_string();
                CAPTURED.lock().unwrap().push((name, r.level(), msg));
            }
        }
        fn flush(&self) {}
    }

    fn capture_logs() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            if log::set_logger(&Capture).is_ok() {
                log::set_max_level(log::LevelFilter::Info);
            }
        });
    }

    /// The `adts front end` lines this harness's decode thread logged, in order.
    fn adts_lines(h: &Harness) -> Vec<String> {
        lines_with_prefix(h, "adts front end")
    }

    /// The `build…` lines this harness's decode thread logged, in order: the first byte, and
    /// the bound's outcome (review fixes F1).
    fn build_lines(h: &Harness) -> Vec<String> {
        lines_with_prefix(h, "build")
    }

    fn lines_with_prefix(h: &Harness, prefix: &str) -> Vec<String> {
        CAPTURED
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _, m)| *t == h.decode_thread && m.starts_with(prefix))
            .map(|(_, _, m)| m.clone())
            .collect()
    }

    /// The `warn`-level lines with `prefix` this harness's decode thread logged (review 2,
    /// finding 5: a bound is one warning, the engine's `fail_build` line with the figures it
    /// decided on; the decode thread's line carrying the page's message is `info`).
    fn decode_warn_lines(h: &Harness, prefix: &str) -> Vec<String> {
        CAPTURED
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, level, m)| {
                *t == h.decode_thread && *level == log::Level::Warn && m.starts_with(prefix)
            })
            .map(|(_, _, m)| m.clone())
            .collect()
    }

    /// Record one engine event: a state is returned, `Started` and `StreamInfo` are kept on
    /// the harness in order.
    fn record(h: &Harness, ev: EngineEvent) -> Option<PlaybackState> {
        match ev {
            EngineEvent::State(s) => return Some(s),
            EngineEvent::Started { station_id } => h.started.lock().unwrap().push(station_id),
            EngineEvent::StreamInfo(info) => h
                .infos
                .lock()
                .unwrap()
                .push((info.sample_rate, info.channels)),
            EngineEvent::Reconnect(info) => h.reconnects.lock().unwrap().push(info.count),
            EngineEvent::Metadata(IcyMetadata { title: Some(t) }) => {
                h.titles.lock().unwrap().push(t)
            }
            _ => {}
        }
        None
    }

    /// After a helper's stopping point: pick up the events the engine sends right behind a
    /// state — `Started` follows `State(Playing)` under the same lock — until the next state,
    /// which is held for the next call, or 50 ms of quiet. Nothing to do while a state is
    /// already held: what the stopping state brought behind it was picked up before that
    /// state, and reading on would overwrite it (round-3 review, finding 3).
    fn grace(h: &Harness) {
        if h.held.lock().unwrap().is_some() {
            return;
        }
        while let Ok(ev) = h.events.recv_timeout(Duration::from_millis(50)) {
            if let Some(s) = record(h, ev) {
                *h.held.lock().unwrap() = Some(s);
                return;
            }
        }
    }

    /// The next engine event within `left`, the held state first. What is already queued is
    /// taken at once; `poll_delay` runs before each wait on an empty channel — a consumer that
    /// wakes late, as the old sampling loop did per poll. A late consumer loses nothing: the
    /// channel keeps every event, in order.
    fn next_event(h: &Harness, left: Duration) -> Option<EngineEvent> {
        if let Some(s) = h.held.lock().unwrap().take() {
            return Some(EngineEvent::State(s));
        }
        if let Ok(ev) = h.events.try_recv() {
            return Some(ev);
        }
        poll_delay();
        h.events.recv_timeout(left).ok()
    }

    /// Every state the session emitted, in order, up to and including the first one `done`
    /// accepts — or until `within` elapses. Decided on the **event stream**, not on a sample of
    /// the current state: a state that begins and ends between two samples cannot be missed
    /// (review 2, 2026-09-25: on a slow CI runner T18's `Playing` did, and the test read
    /// `Reconnecting`). A test reads the state it stopped at as `seen.last()`.
    fn states_until(
        h: &Harness,
        within: Duration,
        done: impl Fn(&PlaybackState) -> bool,
    ) -> Vec<PlaybackState> {
        let deadline = Instant::now() + within;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(ev) = next_event(h, left) else {
                return seen;
            };
            if let Some(s) = record(h, ev) {
                let hit = done(&s);
                seen.push(s);
                if hit {
                    grace(h);
                    return seen;
                }
            }
            if Instant::now() >= deadline {
                return seen;
            }
        }
    }

    /// Every state emitted, in order, until `cond` — a condition outside the event stream,
    /// such as a server's request count, which only grows — holds, or `within` elapses.
    fn states_while_waiting_for(
        h: &Harness,
        within: Duration,
        cond: impl Fn() -> bool,
    ) -> Vec<PlaybackState> {
        let deadline = Instant::now() + within;
        let mut seen = Vec::new();
        loop {
            if cond() {
                grace(h);
                return seen;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return seen;
            }
            if let Some(ev) = next_event(h, left.min(Duration::from_millis(20)))
                && let Some(s) = record(h, ev)
            {
                seen.push(s);
            }
        }
    }

    /// The state a helper stopped at.
    fn last(seen: &[PlaybackState]) -> PlaybackState {
        seen.last().cloned().unwrap_or(PlaybackState::Connecting)
    }

    /// How long a test waits for an event it expects. A hang guard, not a claim: nothing is
    /// asserted about when the event came, so a slow runner only makes the test slower.
    const GUARD: Duration = Duration::from_secs(20);

    /// Wait for the session's `Started`, returning the states seen meanwhile. A test that
    /// stopped **at** a `Playing` reads `started` only after this: `write_state` sends
    /// `Started` right behind `State(Playing)` under the session lock, so a test that stopped at
    /// any later state has it already, but one stopped at that `Playing` may not.
    fn await_started(h: &Harness) -> Vec<PlaybackState> {
        states_while_waiting_for(h, GUARD, || !h.started.lock().unwrap().is_empty())
    }

    /// The harness's own promise, "no state is lost between calls" (round-3 review, finding
    /// 3). No session: the events are queued by hand, all before the first call,
    /// so the order the helpers see is fixed. `states_until(Playing)` stops at `Playing`, and
    /// its grace picks up `Started` and holds `Buffering`. `await_started` finds `Started`
    /// already recorded, so its condition holds on entry and it runs `grace` again, which
    /// reads `Reconnecting` within its 50 ms. Fails if that grace overwrites the held
    /// `Buffering`: the next call then sees `[Reconnecting]` only.
    #[test]
    fn grace_never_overwrites_a_held_state() {
        let (ev_tx, ev_rx) = mpsc::channel();
        let h = Harness {
            ctx: test_ctx(ev_tx.clone()),
            events: ev_rx,
            held: Mutex::new(None),
            started: Mutex::new(Vec::new()),
            infos: Mutex::new(Vec::new()),
            titles: Mutex::new(Vec::new()),
            decode_thread: String::new(),
            reconnects: Mutex::new(Vec::new()),
            supervisor: None,
            bound_seen: Arc::new(Mutex::new(None)),
            bound_gap: Arc::new(Mutex::new(None)),
            _rt: tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime"),
        };
        for ev in [
            EngineEvent::State(PlaybackState::Playing),
            EngineEvent::Started {
                station_id: "u1".into(),
            },
            EngineEvent::State(PlaybackState::Buffering),
            EngineEvent::State(PlaybackState::Reconnecting { attempt: 1 }),
        ] {
            ev_tx.send(ev).expect("send");
        }
        let mut seen = states_until(&h, GUARD, |s| *s == PlaybackState::Playing);
        seen.extend(await_started(&h));
        seen.extend(states_until(&h, GUARD, |s| {
            matches!(s, PlaybackState::Reconnecting { .. })
        }));
        assert_eq!(
            seen,
            [
                PlaybackState::Playing,
                PlaybackState::Buffering,
                PlaybackState::Reconnecting { attempt: 1 },
            ]
        );
        assert_eq!(*h.started.lock().unwrap(), ["u1"]);
    }

    /// `ONDAR_TEST_POLL_DELAY_MS`: a sleep added to every step of a waiting helper, standing
    /// in for a slow CI runner.
    fn poll_delay() {
        if let Some(ms) = std::env::var("ONDAR_TEST_POLL_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            thread::sleep(Duration::from_millis(ms));
        }
    }

    /// `ONDAR_TEST_SERVER_DELAY_MS`: a sleep before every answer of `routed_server`, standing
    /// in for a slow runner's open chain.
    fn server_delay() {
        if let Some(ms) = std::env::var("ONDAR_TEST_SERVER_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            thread::sleep(Duration::from_millis(ms));
        }
    }

    /// `ONDAR_TEST_LATE_REQUEST_DELAY_MS`: a sleep before `routed_server` **records** the third
    /// and every later segment request — the request arrives late, as the log sees it, while
    /// the first two segments (enough for `Playing`) are served at once. It stands in for a
    /// runner slow to schedule the fetch task's next request (`4a388f5`'s CI run read T12's
    /// log at `Playing` without its third start segment).
    fn late_request_delay() {
        if let Some(ms) = std::env::var("ONDAR_TEST_LATE_REQUEST_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            thread::sleep(Duration::from_millis(ms));
        }
    }

    /// A segment request as the test servers name them.
    fn is_segment_path(path: &str) -> bool {
        let p = path.split('?').next().unwrap_or(path);
        p.ends_with(".aac") || p.ends_with(".ts")
    }

    fn is_error(s: &PlaybackState) -> bool {
        matches!(s, PlaybackState::Error { .. })
    }

    fn reconnecting(seen: &[PlaybackState]) -> Vec<u32> {
        seen.iter()
            .filter_map(|s| match s {
                PlaybackState::Reconnecting { attempt } => Some(*attempt),
                _ => None,
            })
            .collect()
    }

    /// Fails on a cause-blind policy: the first state after the open would be
    /// `Reconnecting { 1 }` (the terminal `Error` only arrives after 31 s) and the server would
    /// see a second request at ~1.2 s. The request count after 1.3 s is a window that a slow
    /// runner cannot break: after a terminal `Error` no request is ever made, so lateness can
    /// only delay the check, never add a request.
    #[test]
    fn an_icy_server_fails_the_session_on_the_first_attempt() {
        let (url, requests) = counting_server(
            b"ICY 200 OK\r\nicy-name: synthetic shoutcast v1\r\ncontent-type: audio/mpeg\r\n\r\n\
              0123456789abcdef0123456789abcdef",
        );
        let h = start_session(&url);
        // Stops at the first `Error` or `Reconnecting`, whichever the engine emits first: the
        // cause-blind policy's `Reconnecting { 1 }` is caught by order, not by a window.
        let seen = states_until(&h, GUARD, |s| {
            is_error(s) || matches!(s, PlaybackState::Reconnecting { .. })
        });
        let state = last(&seen);
        assert!(
            matches!(
                &state,
                PlaybackState::Error {
                    code: ErrorCode::Http,
                    ..
                }
            ),
            "expected Error {{ Http }} before any Reconnecting, got {state:?}"
        );
        assert_eq!(
            reconnecting(&seen),
            Vec::<u32>::new(),
            "no Reconnecting state"
        );
        // Let a would-be second attempt (1 s backoff) show itself before counting.
        thread::sleep(Duration::from_millis(1300));
        assert_eq!(requests.load(Ordering::SeqCst), 1, "one request, no retry");
        h.ctx.cancel();
    }

    /// Same shape for a 404 on the first open: the resource is not there for us — `Error {
    /// Http }` with one request and no `Reconnecting`. Fails with the terminal branch disabled,
    /// as the ICY test does: a `Reconnecting` state and a second request.
    #[test]
    fn a_404_fails_the_session_on_the_first_attempt() {
        let (url, requests) = counting_server(
            b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        let h = start_session(&url);
        // Stops at the first `Error` or `Reconnecting`, whichever the engine emits first: the
        // cause-blind policy's `Reconnecting { 1 }` is caught by order, not by a window.
        let seen = states_until(&h, GUARD, |s| {
            is_error(s) || matches!(s, PlaybackState::Reconnecting { .. })
        });
        let state = last(&seen);
        assert!(
            matches!(
                &state,
                PlaybackState::Error {
                    code: ErrorCode::Http,
                    ..
                }
            ),
            "expected Error {{ Http }} before any Reconnecting, got {state:?}"
        );
        assert_eq!(
            reconnecting(&seen),
            Vec::<u32>::new(),
            "no Reconnecting state"
        );
        thread::sleep(Duration::from_millis(1300));
        assert_eq!(requests.load(Ordering::SeqCst), 1, "one request, no retry");
        h.ctx.cancel();
    }

    /// A 5xx keeps the backoff. Fails if 5xx were made terminal (an `Error` before any
    /// `Reconnecting`, one request) or if the backoff's first delay were under 1 s (the gap
    /// between the two requests, on the server's clock — slowness only lengthens it).
    #[test]
    fn a_503_is_retried_through_the_backoff() {
        let (url, requests, times) = timed_scripted_server(vec![
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_vec(),
        ]);
        let h = start_session(&url);
        let mut seen = states_until(&h, GUARD, |s| {
            is_error(s) || matches!(s, PlaybackState::Reconnecting { .. })
        });
        assert_eq!(
            reconnecting(&seen).first(),
            Some(&1),
            "Reconnecting {{ 1 }} before any Error: {seen:?}"
        );
        seen.extend(states_while_waiting_for(&h, GUARD, || {
            requests.load(Ordering::SeqCst) >= 2
        }));
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "a second request (saw {})",
            requests.load(Ordering::SeqCst)
        );
        let t = times.lock().unwrap().clone();
        assert!(
            t[1] - t[0] >= Duration::from_secs(1),
            "the backoff's first delay: {:?}",
            t[1] - t[0]
        );
        assert!(
            !seen.iter().any(is_error),
            "not failed after one 5xx: {seen:?}"
        );
        h.ctx.cancel();
    }

    /// A 429 on the first open keeps the backoff, and `Retry-After` stretches its delay.
    /// Fails on the "every 4xx is terminal" rule (an `Error { Http }` before any
    /// `Reconnecting`, one request) and if the header is ignored (the backoff alone sends the
    /// second request ~1.1 s after the first; the gap, on the server's clock, must be at least
    /// the header's 3 s — a slow runner only lengthens it). An earlier form, "no second request
    /// by 2 s", a late check could break on a correct build.
    #[test]
    fn a_429_is_retried_after_its_retry_after() {
        let (url, requests, times) = timed_scripted_server(vec![
            b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 3\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_vec(),
        ]);
        let h = start_session(&url);
        let mut seen = states_until(&h, GUARD, |s| {
            is_error(s) || matches!(s, PlaybackState::Reconnecting { .. })
        });
        assert_eq!(
            reconnecting(&seen).first(),
            Some(&1),
            "Reconnecting {{ 1 }} before any Error: {seen:?}"
        );
        seen.extend(states_while_waiting_for(&h, GUARD, || {
            requests.load(Ordering::SeqCst) >= 2
        }));
        assert!(!seen.iter().any(is_error), "not failed on a 429: {seen:?}");
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "a second request once Retry-After elapsed"
        );
        let t = times.lock().unwrap().clone();
        assert!(
            t[1] - t[0] >= Duration::from_secs(3),
            "the second request waited for Retry-After: {:?}",
            t[1] - t[0]
        );
        h.ctx.cancel();
    }

    /// A 404 on a reconnect keeps the backoff: the first connection serves 1.5 s of WAV that
    /// ends (`Reconnecting { 1 }`), every later one answers 404 — a mount mid-restart. Fails on
    /// the "every 4xx is terminal" rule: `Error { Http }` right after the reconnect's 404 and
    /// never `Reconnecting { 2 }`.
    #[test]
    fn a_404_on_a_reconnect_keeps_the_backoff() {
        let (url, requests) = scripted_server(vec![
            wav_response(1.5),
            b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec(),
        ]);
        let h = start_session(&url);
        let seen = states_until(&h, GUARD, |s| {
            matches!(s, PlaybackState::Reconnecting { attempt: 2 }) || is_error(s)
        });
        let state = last(&seen);
        assert!(!is_error(&state), "ended on the reconnect's 404: {state:?}");
        assert_eq!(
            reconnecting(&seen),
            vec![1, 2],
            "the 404 after playback went through the backoff ({seen:?})"
        );
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "the reconnect asked the server (saw {})",
            requests.load(Ordering::SeqCst)
        );
        h.ctx.cancel();
    }

    /// M3b commit 5, the click rule at session level: a stream that plays, ends and plays
    /// again through the backoff is **one** session, so `Started` is sent once — on the first
    /// `Playing` — with the id `begin_session` was given. Fails if `Started` is per-`Playing`
    /// (two ids), or if the id is lost (an empty string).
    #[test]
    fn started_is_sent_once_per_session_across_a_reconnect() {
        let (url, requests) = counting_server_owned(wav_response(1.5));
        let h = start_session(&url);
        let seen = states_until(&h, GUARD, |s| {
            matches!(s, PlaybackState::Reconnecting { attempt: 2 }) || is_error(s)
        });
        let playing = seen
            .iter()
            .filter(|s| **s == PlaybackState::Playing)
            .count();
        assert!(
            playing >= 2,
            "the WAV played twice across the reconnect ({seen:?})"
        );
        assert!(requests.load(Ordering::SeqCst) >= 2, "two requests");
        assert_eq!(
            *h.started.lock().unwrap(),
            vec!["u1".to_string()],
            "one Started, with the session's id, across {playing} Playing states"
        );
        h.ctx.cancel();
    }

    /// `counting_server` for a response built at runtime (a WAV is not a `&'static [u8]`).
    fn counting_server_owned(response: Vec<u8>) -> (String, Arc<AtomicUsize>) {
        scripted_server(vec![response])
    }

    // ---- Defect B: the build phase ----

    /// What a paced answer does once its body is sent.
    #[derive(Clone, Copy, Debug)]
    enum After {
        /// Close the connection: the stream ends.
        Close,
        /// Keep the connection open and send nothing more: a silent server.
        Hold,
    }

    /// One connection's answer: `head` at once, then `body` at `bytes_per_sec` (0: as fast as
    /// the client reads), then `after`.
    #[derive(Clone)]
    struct Answer {
        head: Vec<u8>,
        body: Arc<Vec<u8>>,
        bytes_per_sec: u64,
        after: After,
    }

    fn answer(content_type: &str, body: Vec<u8>, bytes_per_sec: u64, after: After) -> Answer {
        Answer {
            head: format!(
                "HTTP/1.0 200 OK\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n"
            )
            .into_bytes(),
            body: Arc::new(body),
            bytes_per_sec,
            after,
        }
    }

    /// A server that paces its bodies on its own clock and counts what it did: `connections`
    /// accepted and body bytes `sent`, both only growing, so a wait bounded by either cannot
    /// be broken by a slow runner (the server's pace is fixed; slowness only means the client
    /// read less of it). Connection `i` gets `answers[i]`, the last one repeated; each
    /// connection has its own thread, so a held one never delays the next accept. Dropping it
    /// ends every held connection.
    struct Paced {
        url: String,
        connections: Arc<AtomicUsize>,
        sent: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
    }

    impl Drop for Paced {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    fn paced_server(answers: Vec<Answer>) -> Paced {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let connections = Arc::new(AtomicUsize::new(0));
        let sent = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (conns, total, halt) = (connections.clone(), sent.clone(), stop.clone());
        thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                if halt.load(Ordering::SeqCst) {
                    break;
                }
                let n = conns.fetch_add(1, Ordering::SeqCst);
                let a = answers[n.min(answers.len() - 1)].clone();
                let (total, halt) = (total.clone(), halt.clone());
                thread::spawn(move || {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf);
                    if sock.write_all(&a.head).is_err() {
                        return;
                    }
                    let started = Instant::now();
                    let mut at = 0usize;
                    while at < a.body.len() {
                        if halt.load(Ordering::SeqCst) {
                            return;
                        }
                        // At most 4 KiB at a time, so `sent` tracks what the client took; paced,
                        // no more than the pace allows by now.
                        let mut chunk = (a.body.len() - at).min(4096);
                        if a.bytes_per_sec > 0 {
                            let due =
                                (started.elapsed().as_secs_f64() * a.bytes_per_sec as f64) as usize;
                            if due <= at {
                                thread::sleep(Duration::from_millis(5));
                                continue;
                            }
                            chunk = chunk.min(due - at);
                        }
                        if sock.write_all(&a.body[at..at + chunk]).is_err() {
                            return;
                        }
                        at += chunk;
                        total.fetch_add(chunk as u64, Ordering::SeqCst);
                    }
                    match a.after {
                        After::Close => {
                            let _ = sock.flush();
                            let _ = sock.shutdown(std::net::Shutdown::Write);
                            thread::sleep(Duration::from_millis(200));
                        }
                        After::Hold => {
                            while !halt.load(Ordering::SeqCst) {
                                thread::sleep(Duration::from_millis(50));
                            }
                        }
                    }
                });
            }
        });
        Paced {
            url: format!("http://{addr}/stream"),
            connections,
            sent,
            stop,
        }
    }

    /// A seeded xorshift64 stream, for fixtures generated in the test (nothing new is
    /// committed).
    fn xorshift(seed: u64) -> impl FnMut() -> u64 {
        let mut x = seed.max(1);
        move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        }
    }

    /// F-nosync (defect B Step 0, A7): `len` seeded bytes drawn from `0x00–0x3F`. Every probe
    /// marker of the formats rodio 0.22 registers holds a byte ≥ `0x40` or `0xFF`, so no reader
    /// can sync on it by construction, and Symphonia gives up only after its 1 MiB search.
    fn f_nosync(len: usize) -> Vec<u8> {
        let mut next = xorshift(0x0DDB_0B5E);
        (0..len).map(|_| (next() & 0x3F) as u8).collect()
    }

    /// The false ADTS header of F-falsesync (review A8): LC, sample-rate index 6 (24 000),
    /// channel configuration 1, `frame_length` 16 — a header whose frame is followed by junk.
    const FALSE_ADTS: [u8; 7] = [0xFF, 0xF1, 0x58, 0x40, 0x02, 0x1F, 0xFC];

    /// F-falsesync: F-nosync with [`FALSE_ADTS`] planted at 4 096.
    fn f_falsesync(len: usize) -> Vec<u8> {
        let mut b = f_nosync(len);
        b[4096..4096 + FALSE_ADTS.len()].copy_from_slice(&FALSE_ADTS);
        b
    }

    /// 128 KiB/s, the pace of the defect B tests (S1's stuck mounts pull 10–28 KB/s; faster
    /// only shortens the tests, and the bound is on time, not bytes).
    const PACE: u64 = 128 * 1024;

    /// Every state emitted, in order, up to the first one `done` accepts, or until the server
    /// has sent `limit` body bytes, or `within` elapses. The byte count is the defect B tests'
    /// bound: on `main` a stuck build keeps reading, so the wait ends on the count and the
    /// assertion on the state fails; the count only grows, and at the server's fixed pace a
    /// slow runner only reaches it later.
    fn states_until_sent(
        h: &Harness,
        sent: &AtomicU64,
        limit: u64,
        within: Duration,
        done: impl Fn(&PlaybackState) -> bool,
    ) -> Vec<PlaybackState> {
        let deadline = Instant::now() + within;
        let mut seen = Vec::new();
        loop {
            if sent.load(Ordering::SeqCst) >= limit {
                grace(h);
                return seen;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return seen;
            }
            if let Some(ev) = next_event(h, left.min(Duration::from_millis(20)))
                && let Some(s) = record(h, ev)
            {
                let hit = done(&s);
                seen.push(s);
                if hit {
                    grace(h);
                    return seen;
                }
            }
        }
    }

    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;

    /// "The bound fired below 512 KiB": 2 s at [`PACE`] plus the prefetch (32 KiB) is about
    /// 290 KiB; the slack (almost 2 s at the pace) is for late ticks on a slow runner. The
    /// unbounded paths read 1 MiB (the search) or never stop.
    const BOUNDED_BELOW: u64 = 512 * KIB;

    fn ends(s: &PlaybackState) -> bool {
        is_error(s) || matches!(s, PlaybackState::Reconnecting { .. })
    }

    /// The final state as `(code, message)`, or a panic naming what the session did instead.
    fn final_error(seen: &[PlaybackState], sent: u64) -> (ErrorCode, String) {
        match last(seen) {
            PlaybackState::Error { code, message } => (code, message),
            other => panic!("no Error: stopped at {other:?} with {sent} B sent ({seen:?})"),
        }
    }

    /// T-B1a′ (defect B C1, at the bound since C2): a session that **produced audio** — 1.5 s
    /// of WAV, played, then ended — and whose every reconnect answers F-nosync keeps the
    /// backoff on the reconnect's format failure, as a 404 there does: a mount that once played
    /// had a valid format. Since C2 the failure is the build bound's `UnsupportedFormat` cause
    /// (fewer than 512 KiB of F-nosync sent), not Symphonia's 1 MiB search. Fails on `main`'s
    /// rule, where the arm is terminal whatever came before: `Error { UnsupportedFormat }`
    /// after the reconnect's search, never `Reconnecting { 2 }`.
    #[test]
    fn t_b1a_prime_an_unrecognised_format_after_audio_keeps_the_backoff() {
        let wav = wav_response(1.5);
        let wav_len = wav.len() as u64;
        let server = paced_server(vec![
            Answer {
                head: Vec::new(),
                body: Arc::new(wav),
                bytes_per_sec: 0,
                after: After::Close,
            },
            answer("audio/mpeg", f_nosync(2 * MIB as usize), PACE, After::Close),
        ]);
        let h = start_session_supervised(&server.url);
        let seen = states_until_sent(&h, &server.sent, wav_len + MIB, GUARD, |s| {
            matches!(s, PlaybackState::Reconnecting { attempt: 2 }) || is_error(s)
        });
        let nosync_sent = server.sent.load(Ordering::SeqCst) - wav_len;
        let state = last(&seen);
        assert!(
            !is_error(&state),
            "the reconnect's unrecognised format ended the session: {state:?} ({seen:?})"
        );
        assert_eq!(
            reconnecting(&seen),
            vec![1, 2],
            "the WAV's end, then the reconnect's format failure, through the backoff ({seen:?})"
        );
        assert!(
            nosync_sent < BOUNDED_BELOW,
            "the reconnect failed at the bound, not the search: {nosync_sent} B of F-nosync sent"
        );
        assert_eq!(
            *h.started.lock().unwrap(),
            ["u1"],
            "one Started, from the WAV"
        );
        h.ctx.cancel();
    }

    /// Review round 7, P1: the B2 fact is "has produced audio", not "has built a decoder".
    /// The first connection sends a false ADTS header in 8 KiB of junk and closes: `build()`
    /// returns `Ok` with a decoder that yields nothing (S3's empty `Ok` — the `StreamInfo`
    /// below is the proof it built), the stream "ends", `Reconnecting { 1 }`. The reconnect
    /// answers F-falsesync, whose build never ends on its own: the bound's `UnsupportedFormat`
    /// cause must end the session, since no audio was ever produced. Fails on `main`, where the
    /// reconnect's build runs unbounded (the wait ends at 1 MiB sent, in `Reconnecting { 1 }`),
    /// and on a flag set at `build()`'s `Ok`, which reads `Reconnecting { 2 }`.
    #[test]
    fn p1_a_decoder_that_built_but_never_produced_audio_does_not_protect_the_session() {
        let server = paced_server(vec![
            answer("audio/aac", f_falsesync(8 * KIB as usize), 0, After::Close),
            answer(
                "audio/aac",
                f_falsesync(2 * MIB as usize),
                PACE,
                After::Close,
            ),
        ]);
        let h = start_session_supervised(&server.url);
        let seen = states_until_sent(&h, &server.sent, 8 * KIB + MIB, GUARD, |s| {
            matches!(s, PlaybackState::Reconnecting { attempt: 2 }) || is_error(s)
        });
        let sent = server.sent.load(Ordering::SeqCst);
        assert!(
            !h.infos.lock().unwrap().is_empty(),
            "the first connection built a decoder (S3's empty Ok): no StreamInfo in {seen:?}"
        );
        assert_eq!(
            reconnecting(&seen),
            vec![1],
            "only the empty stream's end went through the backoff ({seen:?})"
        );
        let (code, message) = final_error(&seen, sent);
        assert_eq!(code, ErrorCode::UnsupportedFormat, "{message}");
        assert!(
            message.contains("first 2 s"),
            "the bound's cause: {message}"
        );
        assert!(h.started.lock().unwrap().is_empty(), "no audio, no Started");
        h.ctx.cancel();
    }

    /// T-B1a (defect B C2): F-falsesync as `audio/aac` — S1's shape, a false ADTS header the
    /// ADTS and MP3 readers resync on for ever (S1: 2.49 MB read, then `Ok` on Stop). The bound
    /// ends it: `Error { UnsupportedFormat }` naming the bound, one connection, no `Started`.
    /// Fails on `main`: `Connecting` when the server has sent 1 MiB. Mutations: the bound never
    /// firing reads the same; the phase checked on `Err` only (not on `Ok`) reads `Buffering`
    /// with `StreamInfo` 24 000/1 and then `Reconnecting { 1 }` (S3's empty `Ok`); the cause
    /// rule inverted reads `Reconnecting { 1 }`; the engine's bound check placed behind the
    /// ring's early return fails this and the other five bound tests.
    #[test]
    fn t_b1a_a_false_adts_header_is_bounded() {
        let server = paced_server(vec![answer(
            "audio/aac",
            f_falsesync(2 * MIB as usize),
            PACE,
            After::Close,
        )]);
        let h = start_session_supervised(&server.url);
        let seen = states_until_sent(&h, &server.sent, MIB, GUARD, ends);
        let sent = server.sent.load(Ordering::SeqCst);
        let (code, message) = final_error(&seen, sent);
        assert_eq!(code, ErrorCode::UnsupportedFormat, "{message}");
        assert!(
            message.contains("no decodable audio in the first 2 s")
                && message.contains("audio/aac"),
            "the message names the bound and the content type: {message}"
        );
        assert!(sent < BOUNDED_BELOW, "{sent} B sent before the bound");
        assert_eq!(
            server.connections.load(Ordering::SeqCst),
            1,
            "one connection"
        );
        assert!(
            h.infos.lock().unwrap().is_empty(),
            "no decoder: {:?}",
            h.infos
        );
        assert!(h.started.lock().unwrap().is_empty(), "no Started");
        // Review 2, finding 5: the bound's message line is logged (`build_lines`) but is not a
        // second warning beside the engine's `fail_build` line (on `dc9200d` it was `warn`).
        assert!(
            build_lines(&h)
                .iter()
                .any(|l| l.starts_with("build bound: no decodable audio")),
            "{:?}",
            build_lines(&h)
        );
        assert_eq!(
            decode_warn_lines(&h, "build bound"),
            Vec::<String>::new(),
            "a second warn for one bound"
        );
    }

    /// C4b (defect B, review round 10), measured since F1 from the build's first byte: the
    /// bound counts **elapsed time**, not ticks. Each supervised tick is slowed by 50 ms, a
    /// 150 ms tick period, and the format bound is 2 s (20 ticks at `TICK_INTERVAL`). The
    /// supervisor reads the time from the decode thread's first-byte stamp to the tick whose
    /// swap bounded the build, and counts the ticks that saw it probing with a first byte.
    ///
    /// - An elapsed-time bound fires at the first tick at or past 2 s after the stamp: at least
    ///   2 s and at most one tick period over, after about 14 ticks.
    /// - A tick-counted bound fires on the 20th tick, about 2.9 s in (fails so: 2.908 s after
    ///   20 ticks).
    ///
    /// Asserted: at least the bound, below 1.25 × it (2.5 s), and fewer ticks than 20. The tick
    /// count cannot be broken by a slow runner, which only makes each tick longer and the count
    /// smaller.
    #[test]
    fn c4b_the_build_bound_counts_elapsed_time_not_ticks() {
        let server = paced_server(vec![answer(
            "audio/aac",
            f_falsesync(2 * MIB as usize),
            PACE,
            After::Close,
        )]);
        let h = start_session_supervised_slow(&server.url, Duration::from_millis(50));
        let seen = states_until_sent(&h, &server.sent, MIB, GUARD, ends);
        let (code, message) = final_error(&seen, server.sent.load(Ordering::SeqCst));
        assert_eq!(code, ErrorCode::UnsupportedFormat, "{message}");
        let _ = states_while_waiting_for(&h, GUARD, || h.bound_seen.lock().unwrap().is_some());
        let (took, ticks) = h
            .bound_seen
            .lock()
            .unwrap()
            .expect("the supervisor saw the bound");
        let ticks_at_interval = (TEST_FORMAT.as_millis() / TICK_INTERVAL.as_millis()) as u32;
        assert!(
            took >= TEST_FORMAT,
            "fired early: {took:?} < {TEST_FORMAT:?} ({ticks} ticks)"
        );
        assert!(
            took < TEST_FORMAT * 5 / 4,
            "fired at {took:?} after {ticks} slowed ticks: the bound counted ticks, not \
             {TEST_FORMAT:?}"
        );
        assert!(
            ticks < ticks_at_interval,
            "{ticks} ticks: a tick-counted bound fires on the {ticks_at_interval}th"
        );
    }

    /// Review fixes F1 (e), finding 6: each build carries its own clock, stamped by the decode
    /// thread, so nothing separates two builds but their seq — not a tick that sees the phase
    /// between them, not the backoff's ≥ 1 s sleep. A real `Engine` ticked by hand, the builds
    /// driven through the clock as `run_session` drives it, a 1 s format bound.
    ///
    /// 1. Build N gets a first byte, a tick sees it, and it ages past the bound.
    /// 2. Build N returns (`BUILT`) and build N+1 begins, with **no tick in between**; N+1 is
    ///    ticked before and after its first byte: still `PROBING` (fails too if `begin_build`
    ///    keeps N's first-byte stamp). Fails with the build's start kept on the engine and
    ///    reset only by a tick that saw another phase: `left: 3` (`BOUND`) — N+1 read build
    ///    N's `probing_since`.
    /// 3. A bound decided from N's word, after N+1 began: N+1 is still `PROBING` and its
    ///    download's token is not cancelled. Fails on a swap that compares the phase alone.
    #[test]
    fn f1_each_build_carries_its_own_clock() {
        let format = Duration::from_secs(1);
        let (ev_tx, _ev_rx) = mpsc::channel();
        let mut ctx = test_ctx(ev_tx);
        ctx.clock = Arc::new(BuildClock::new(test_bounds(format)));
        let mut engine = Engine::new(ctx.shared.clone(), "Ondar/test".into());
        engine.session = Some(ctx.clone());
        let first_byte = |ctx: &SessionCtx| {
            let mut r =
                ClockedReader::new(std::io::Cursor::new(vec![0u8; 4]), ctx.clock.clone(), 0);
            let mut b = [0u8; 4];
            r.read_exact(&mut b).expect("read");
        };

        let n = ctx.clock.begin_build(0, Arc::new(Arrivals::new()));
        first_byte(&ctx);
        engine.tick();
        thread::sleep(format + Duration::from_millis(150));
        assert_eq!(
            ctx.clock.finish_build(n),
            Ok(()),
            "N returned before any tick bounded it"
        );
        let n1 = ctx.clock.begin_build(0, Arc::new(Arrivals::new()));
        engine.tick();
        assert_eq!(
            ctx.clock.word(),
            n1,
            "build N+1, before its first byte, read N's stamps"
        );
        first_byte(&ctx);
        engine.tick();
        assert_eq!(ctx.clock.word(), n1, "build N+1 read build N's clock");

        let token = CancellationToken::new();
        *ctx.download.lock().unwrap() = Some(token.clone());
        let inputs = ctx.clock.inputs(0);
        Engine::fail_build(&ctx, n, BuildCause::Format, &inputs);
        assert_eq!(ctx.clock.word(), n1, "a decision about N bounded N+1");
        assert!(
            !token.is_cancelled(),
            "a decision about N cancelled N+1's download"
        );
    }

    /// T-B1b (defect B C2): F-nosync as `audio/mpeg`. On `main` Symphonia refuses it only after
    /// its 1 MiB search, 8 s at the pace; the bound sits below that. Fails on `main`: the wait
    /// ends at 1 MiB sent, before the search's `Error` (and that error does not name the
    /// bound). Mutations: the bound never firing reads the same; so does the phase checked
    /// after the `UnrecognizedFormat` arm.
    #[test]
    fn t_b1b_no_marker_is_bounded_below_the_search() {
        let server = paced_server(vec![answer(
            "audio/mpeg",
            f_nosync(2 * MIB as usize),
            PACE,
            After::Close,
        )]);
        let h = start_session_supervised(&server.url);
        let seen = states_until_sent(&h, &server.sent, MIB, GUARD, ends);
        let sent = server.sent.load(Ordering::SeqCst);
        let (code, message) = final_error(&seen, sent);
        assert_eq!(code, ErrorCode::UnsupportedFormat, "{message}");
        assert!(
            message.contains("first 2 s"),
            "the bound's cause: {message}"
        );
        assert!(sent < BOUNDED_BELOW, "{sent} B sent before the bound");
        assert_eq!(
            server.connections.load(Ordering::SeqCst),
            1,
            "one connection"
        );
    }

    /// T-B1c (defect B C2): a silent server — 96 KiB of F-nosync, then nothing, and every
    /// connection re-sent from byte 0 (S1 (iii): stream-download's internal reconnect re-feeds
    /// it every `retry_timeout`, so bytes keep "arriving" and `read_timeout` never fires). The
    /// cause is the network, not the format: at the bound the internal reconnect count has
    /// moved (and the bytes stopped for `retry_timeout` between re-feeds) → `Reconnecting { 1 }`,
    /// no `Error`. The bound here is the production relation,
    /// `max(2 s, 3 × retry_timeout)` = 15 s at the default 5 s (review P2), because the cause
    /// needs the re-feeds; `ONDAR_RETRY_TIMEOUT_SECS` is resolved once per process, so it
    /// cannot be shortened per test. Fails on `main`: `Error { UnsupportedFormat }` when the
    /// re-fed scan reaches 1 MiB, about 50 s in (the wrong cause). Mutations: a cause rule
    /// "bytes flowing ⇒ format" reads `Error { UnsupportedFormat }`; so does the phase checked
    /// after the `UnrecognizedFormat` arm. The bound must have been decided on a gap ≥
    /// `retry_timeout` (review 2, G1): with the gap dropped from the rule, "decided on a 0ns
    /// gap".
    #[test]
    fn t_b1c_a_silent_server_is_a_network_cause() {
        let bound = (stream::retry_timeout() * 3).max(Duration::from_secs(2));
        let server = paced_server(vec![answer(
            "audio/mpeg",
            f_nosync(96 * KIB as usize),
            0,
            After::Hold,
        )]);
        let h = start_session_bounded(&server.url, None, test_bounds(bound));
        let seen = states_until_sent(&h, &server.sent, MIB, Duration::from_secs(90), ends);
        let sent = server.sent.load(Ordering::SeqCst);
        let state = last(&seen);
        assert_eq!(
            state,
            PlaybackState::Reconnecting { attempt: 1 },
            "{sent} B sent ({seen:?})"
        );
        assert!(
            !h.reconnects.lock().unwrap().is_empty(),
            "the internal reconnect ran during the build"
        );
        assert!(
            server.connections.load(Ordering::SeqCst) >= 2,
            "the server saw the re-feed"
        );
        // Review 2, G1: the bound was decided on a real arrival gap, the server's silence up to
        // stream-download's idle timeout — not on a read's fill time.
        let gap = h.bound_gap.lock().unwrap().expect("the bound's tick");
        assert!(gap >= stream::retry_timeout(), "decided on a {gap:?} gap");
        h.ctx.cancel();
    }

    /// T-B1d (defect B C2): F-nosync with an MP3 frame header `FF FB 90 C4` planted at 4 096,
    /// as `audio/mpeg` — the MP3 demuxer's unbounded "skipping junk" resync (Amendment 3; every
    /// stuck live mount in S2 was caught this way). The bound is reader-agnostic. Fails on
    /// `main`: `Connecting` at 1 MiB sent. Mutation: the bound never firing reads the same.
    #[test]
    fn t_b1d_an_mp3_resync_is_bounded() {
        let mut body = f_nosync(2 * MIB as usize);
        body[4096..4100].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC4]);
        let server = paced_server(vec![answer("audio/mpeg", body, PACE, After::Close)]);
        let h = start_session_supervised(&server.url);
        let seen = states_until_sent(&h, &server.sent, MIB, GUARD, ends);
        let sent = server.sent.load(Ordering::SeqCst);
        let (code, message) = final_error(&seen, sent);
        assert_eq!(code, ErrorCode::UnsupportedFormat, "{message}");
        assert!(
            message.contains("first 2 s"),
            "the bound's cause: {message}"
        );
        assert!(sent < BOUNDED_BELOW, "{sent} B sent before the bound");
    }

    // ---- Defect B, review fixes F1: the build clock ----

    /// A connection accepted, its request read, and never answered: a reconnect that hangs.
    fn mute() -> Answer {
        Answer {
            head: Vec::new(),
            body: Arc::new(Vec::new()),
            bytes_per_sec: 0,
            after: After::Hold,
        }
    }

    /// F1 (a), finding 1: a reconnect that **hangs** is the network, not the format. The first
    /// connection sends 40 KiB of F-nosync as `audio/mpeg` (above the 32 KiB prefetch, so the
    /// build has a first byte) and holds; every later connection is accepted and never answered.
    /// stream-download's reconnect is cut by its own `retry_timeout` and never calls
    /// `on_reconnect` (0.24.4 `source/mod.rs:272–287`), so the count stays at 0 — the gap
    /// decides. The format bound is T-B1c's 15 s, since the gap must reach `retry_timeout`
    /// inside it. Asserted: `Reconnecting { 1 }`; no `Reconnect` event (this is the hung case,
    /// not T-B1c's); a reconnect was attempted. Fails with the longest gap dropped from the
    /// rule: `Error { UnsupportedFormat, "no decodable audio in the first 15 s of the stream
    /// (audio/mpeg)" }`, terminal. ~16 s.
    #[test]
    fn f1_a_hung_reconnect_is_a_network_cause() {
        let format = (stream::retry_timeout() * 3).max(Duration::from_secs(2));
        let server = paced_server(vec![
            answer("audio/mpeg", f_nosync(40 * KIB as usize), 0, After::Hold),
            mute(),
        ]);
        let h = start_session_bounded(&server.url, None, test_bounds(format));
        let seen = states_until(&h, Duration::from_secs(60), ends);
        assert_eq!(
            last(&seen),
            PlaybackState::Reconnecting { attempt: 1 },
            "{seen:?}"
        );
        assert!(
            h.reconnects.lock().unwrap().is_empty(),
            "an internal reconnect completed: {:?}",
            h.reconnects
        );
        assert!(
            server.connections.load(Ordering::SeqCst) >= 2,
            "a reconnect was attempted"
        );
        assert!(
            build_lines(&h)
                .iter()
                .any(|l| l.contains("the connection stalled")),
            "{:?}",
            build_lines(&h)
        );
        // Review 2, finding 5: one warning per bound (the network cause's line too).
        assert_eq!(
            decode_warn_lines(&h, "build bound"),
            Vec::<String>::new(),
            "a second warn for one bound"
        );
        h.ctx.cancel();
    }

    // ---- Defect B, review 2, G1: starvation on network arrival ----

    /// G1, review 2 finding 1: an unsyncable body served **steadily** at 32 kbit/s (4 000 B/s,
    /// in the paced server's small writes), no ICY metadata, never pausing, is a format cause:
    /// terminal `Error { UnsupportedFormat }` at the format bound, no `Reconnecting`, one
    /// connection, and the bound decided on a longest gap under 1 s. A 4 KiB prefetch, the
    /// production `starved` (5 s) and T-B1c's 15 s format bound. Fails with the gaps measured
    /// per decoder read instead of by `on_progress` on the open — `on_progress` off on the HTTP
    /// open, or each decoder read stamping the arrival: Symphonia's 32 KiB read takes 8.2 s at
    /// this rate, so the build reads `Starved` and backs off, `Reconnecting { 1 }`, "the
    /// connection stalled (8.1 s without data)". ~16 s.
    #[test]
    fn g1_a_steady_32_kbit_unsyncable_is_a_format_cause() {
        const RATE: u64 = 4_000;
        let server = paced_server(vec![answer(
            "audio/mpeg",
            f_nosync(256 * KIB as usize),
            RATE,
            After::Hold,
        )]);
        g1_assert_format_cause(&server, "audio/mpeg");
    }

    /// G1, review 2 finding 1, behind ICY metadata: the same body at 20 kbit/s (2 500 B/s) with
    /// `icy-metaint: 16000`, where `IcyReader` caps a read at 16 000 B — 6.4 s per read at this
    /// rate (at the finding's ~26 kbit/s a read takes 4.9 s and the per-read mutation would
    /// pass, proving nothing). The same assertions as (a); fails as (a) does, on a 6.4 s "gap".
    /// ~16 s.
    #[test]
    fn g1_b_icy_16000_below_26_kbit_is_a_format_cause() {
        let server = paced_server(vec![Answer {
            bytes_per_sec: 2_500,
            ..icy_answer(
                "audio/mpeg",
                16_000,
                with_metaint(&f_nosync(256 * KIB as usize), 16_000, "Artist - Title"),
            )
        }]);
        g1_assert_format_cause(&server, "audio/mpeg");
    }

    fn g1_assert_format_cause(server: &Paced, content_type: &str) {
        let format = (stream::retry_timeout() * 3).max(Duration::from_secs(2));
        let h = start_session_bounded(&server.url, Some(4 * KIB), test_bounds(format));
        let seen = states_until(&h, Duration::from_secs(60), ends);
        let sent = server.sent.load(Ordering::SeqCst);
        let (code, message) = final_error(&seen, sent);
        assert_eq!(code, ErrorCode::UnsupportedFormat, "{message} ({seen:?})");
        assert_eq!(
            message,
            format!(
                "no decodable audio in the first {} of the stream ({content_type})",
                format_secs(format)
            )
        );
        assert!(
            !seen
                .iter()
                .any(|s| matches!(s, PlaybackState::Reconnecting { .. })),
            "{seen:?}"
        );
        assert_eq!(
            server.connections.load(Ordering::SeqCst),
            1,
            "one connection"
        );
        let gap = h.bound_gap.lock().unwrap().expect("the bound's tick");
        assert!(gap < Duration::from_secs(1), "decided on a {gap:?} gap");
        h.ctx.cancel();
    }

    /// F1 (b), finding 2: a slow healthy stream whose prefetch outlasts the format bound plays,
    /// because the bound counts from the first byte, not from `open`. The finding's shape
    /// scaled by 1/10: there, an 80 000 B prefetch at 3 KB/s is 26.7 s against 20 s; here a
    /// tone WAV at 8 kHz mono 16-bit (16 000 B/s) paced at that rate with no burst, a 64 000 B
    /// prefetch (4 s to meet), a 2 s format bound and an 8 s no-bytes bound. Fails with the
    /// format bound run from the build's start, before any first byte: `Error {
    /// UnsupportedFormat, "no decodable audio in the first 2 s of the stream (audio/wav)" }`,
    /// terminal, never `Playing`. (The format bound read from the build's start only once a
    /// first byte exists survives here —
    /// the WAV builds within microseconds of its first byte, before any tick — and is pinned by
    /// `format_fires_at_its_bound_from_the_first_byte_and_not_before`.)
    #[test]
    fn f1_an_overstated_bitrate_on_a_slow_stream_plays() {
        const RATE: u64 = 16_000;
        let wav = tone_wav_response(8_000, 1, 440.0, 30.0);
        let server = paced_server(vec![Answer {
            head: Vec::new(),
            body: Arc::new(wav),
            bytes_per_sec: RATE,
            after: After::Hold,
        }]);
        let h = start_session_bounded(
            &server.url,
            Some(4 * RATE),
            BuildBounds {
                format: TEST_FORMAT,
                starved: stream::retry_timeout(),
                no_bytes: Duration::from_secs(8),
            },
        );
        let seen = states_until(&h, GUARD, is_playing_or_ended);
        assert_eq!(last(&seen), PlaybackState::Playing, "{seen:?}");
        assert_eq!(*h.infos.lock().unwrap(), [(8_000, 1)]);
        let _ = await_started(&h);
        assert_eq!(*h.started.lock().unwrap(), ["u1"], "one Started");
        h.ctx.cancel();
    }

    /// F1 (c), finding 3: an HLS session whose segments stall before the prefetch is met is the
    /// network. A static live window of five 10 s segments; the first fetched (seq 3, three
    /// from the end) is station 10's head alone, below the 32 KiB prefetch; every later segment
    /// request sleeps 15 s (the routed server is serial, so the host stalls as a dead network
    /// does). No byte reaches the decoder, and HLS's `retry_timeout` (≥ 55 s) means no internal
    /// reconnect either: the no-bytes bound (3 s here) ends the build as the network. Fails
    /// with `NoBytes` mapped to `Format`, or the format bound run before the first byte:
    /// `Error { UnsupportedFormat, "no decodable audio in the first 2 s of the stream
    /// (audio/aac)" }`, terminal.
    #[test]
    fn f1_an_hls_stall_before_the_first_byte_is_a_network_cause() {
        let head = fixture("10-seg-head.aac");
        assert!(
            (head.len() as u64) < stream::PREFETCH_FLOOR_BYTES,
            "the first segment must not meet the prefetch"
        );
        let playlist = live_playlist(10, 10.0, 1, 5, Duration::from_secs(40));
        let (base, paths) = routed_server(move |path| {
            if path == "/live/playlist.m3u8" {
                return Some(routed(
                    "application/vnd.apple.mpegurl",
                    playlist.clone().into_bytes(),
                ));
            }
            let seq = seg_seq(path)?;
            if seq > 3 {
                thread::sleep(Duration::from_secs(15));
            }
            Some(routed("audio/aac", head.clone()))
        });
        let h = start_session_bounded(
            &format!("{base}/live/playlist.m3u8"),
            None,
            BuildBounds {
                format: TEST_FORMAT,
                starved: stream::retry_timeout(),
                no_bytes: Duration::from_secs(3),
            },
        );
        let seen = states_until(&h, GUARD, ends);
        assert_eq!(
            last(&seen),
            PlaybackState::Reconnecting { attempt: 1 },
            "{seen:?}"
        );
        assert!(
            paths
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.ends_with("seg-3.aac")),
            "the open fetched the first segment"
        );
        assert!(
            build_lines(&h)
                .iter()
                .any(|l| l.contains("no audio arrived within 3 s of connecting")),
            "{:?}",
            build_lines(&h)
        );
        assert!(h.infos.lock().unwrap().is_empty(), "no decoder");
        h.ctx.cancel();
    }

    // ---- Defect B, review fixes F2: a cancelled build emits nothing ----

    /// F2 (finding 5): a build the **user** cancelled (Stop, or `play` of another station)
    /// emits nothing. Session A builds on 8 KiB of F-falsesync (served as `audio/mpeg`, then
    /// held) with a 4 KiB prefetch, so its decoder has read the false header and waits for
    /// more. (Reshaped before the fix: as `audio/aac` the ADTS front end holds the 8 KiB, the
    /// decoder has read nothing, and the cancel makes `build()` return `UnrecognizedFormat`,
    /// not the empty `Ok`; `application/octet-stream` does the same, `audio/mpeg` and an
    /// unknown type give the `Ok`.) Then B is
    /// played as `Engine::play` does it — A cancelled, B's session begun, `Connecting`. After a
    /// cancel `build()` returns an empty `Ok` (Step 0, S3), and `StreamInfo` is not
    /// generation-gated, so A's would land on B's row. B never builds, as a server that never
    /// answers, so any `StreamInfo` after B's play is A's. (B's `Connecting` sends no event
    /// here: the state is already `Connecting`, as it is in production when A was still
    /// connecting; the boundary is read at the cancel.) The events are read after A's decode
    /// thread has exited (its handle on the context dropped), never inside a window. Fails
    /// with the check placed after the `StreamInfo` emit, or absent: A's 24 000/1
    /// `StreamInfo` after B's play — S3's empty `Ok` after the cancel.
    #[test]
    fn f2_a_cancelled_build_emits_no_stream_info() {
        let server = paced_server(vec![answer(
            "audio/mpeg",
            f_falsesync(8 * KIB as usize),
            0,
            After::Hold,
        )]);
        let h = start_session_with(&server.url, None, Some(4 * KIB), None);
        let _ = states_while_waiting_for(&h, GUARD, || {
            server.sent.load(Ordering::SeqCst) >= 8 * KIB
                && build::phase_of(h.ctx.clock.word()) == build::phase::PROBING
        });
        // Shape only, never the claim: let the decoder read the 8 KiB it was sent, so the
        // cancel meets a decoder past the false header (S3's empty `Ok`).
        thread::sleep(Duration::from_millis(300));
        assert!(
            h.infos.lock().unwrap().is_empty(),
            "A had not built before B"
        );
        // Everything A sent so far is before B; A cannot emit `StreamInfo` without `build()`
        // returning, which on this held body takes the cancel.
        while h.events.try_recv().is_ok() {}

        h.ctx.cancel();
        let _b = h.ctx.shared.begin_session("u2".into());
        h.ctx.shared.set_state(PlaybackState::Connecting);

        let deadline = Instant::now() + GUARD;
        while Arc::strong_count(&h.ctx.cancel) > 1 {
            assert!(Instant::now() < deadline, "A's decode thread did not exit");
            thread::sleep(Duration::from_millis(10));
        }
        let after_b: Vec<String> = h.events.try_iter().map(|ev| format!("{ev:?}")).collect();
        assert!(
            !after_b.iter().any(|e| e.starts_with("StreamInfo")),
            "A's StreamInfo landed after B's play: {after_b:?}"
        );
    }

    // ---- Defect B C4: the ADTS front end, wired ----

    /// Station 02's head after its ID3 tag: 16 whole `FFF9` frames, 24 000/1.
    fn frames_02() -> Vec<u8> {
        let head = fixture("02-seg-head.aac");
        let body = head[test_id3_end(&head)..].to_vec();
        assert_eq!(crate::hls::segment::normalise_adts(&body).frames, 16);
        body
    }

    /// `audio` with an ICY metadata block after every `metaint` bytes: `StreamTitle` in the
    /// first, empty ones after (a length byte of 0), none after a short tail.
    fn with_metaint(audio: &[u8], metaint: usize, title: &str) -> Vec<u8> {
        let mut block = format!("StreamTitle='{title}';").into_bytes();
        block.resize(block.len().div_ceil(16) * 16, 0);
        let mut out = Vec::new();
        for (i, chunk) in audio.chunks(metaint).enumerate() {
            out.extend_from_slice(chunk);
            if chunk.len() == metaint {
                if i == 0 {
                    out.push((block.len() / 16) as u8);
                    out.extend_from_slice(&block);
                } else {
                    out.push(0);
                }
            }
        }
        out
    }

    fn icy_answer(content_type: &str, metaint: usize, body: Vec<u8>) -> Answer {
        Answer {
            head: format!(
                "HTTP/1.0 200 OK\r\ncontent-type: {content_type}\r\nicy-metaint: {metaint}\r\n\r\n"
            )
            .into_bytes(),
            body: Arc::new(body),
            bytes_per_sec: 0,
            after: After::Hold,
        }
    }

    /// How long the `FFF9` body is: 64 × 16 frames, about 44 s of audio and above the 32 KiB
    /// prefetch, so the open returns while the server holds the connection, as a live mount.
    const REPEATS_02: usize = 64;

    fn is_playing_or_ended(s: &PlaybackState) -> bool {
        *s == PlaybackState::Playing || ends(s)
    }

    /// T-B2a (defect B C4): an Icecast `audio/aac` mount sending `FFF9` frames (station 02's,
    /// repeated), with `icy-metaint: 1024` and a `StreamTitle` in the first block. The front
    /// end aligns at 0 and rewrites every header: `Playing`, `StreamInfo` 24 000/1, one
    /// `Started`, one title. Without the front end the build never ends on its own (S1 (i):
    /// `Connecting` for 150 s live). Mutations: the front end off → the build bound's `Error {
    /// UnsupportedFormat }`; the front end placed **before** `IcyReader` (review P4) → not
    /// both a `Metadata` event and `Playing` (`[Buffering, Reconnecting { 1 }]`).
    #[test]
    fn t_b2a_an_fff9_icecast_mount_plays_through_the_front_end() {
        let audio = frames_02().repeat(REPEATS_02);
        let server = paced_server(vec![icy_answer(
            "audio/aac",
            1024,
            with_metaint(&audio, 1024, "Artist - Title"),
        )]);
        let h = start_session_supervised(&server.url);
        let seen = states_until(&h, GUARD, is_playing_or_ended);
        assert_eq!(last(&seen), PlaybackState::Playing, "{seen:?}");
        let _ = await_started(&h);
        assert_eq!(*h.started.lock().unwrap(), ["u1"], "one Started");
        assert_eq!(
            *h.infos.lock().unwrap(),
            [(24_000, 1)],
            "the rewritten headers' format"
        );
        assert_eq!(
            *h.titles.lock().unwrap(),
            ["Artist - Title"],
            "the metadata block reached IcyReader"
        );
        assert_eq!(adts_lines(&h), ["adts front end: aligned at 0 B"]);
        h.ctx.cancel();
    }

    /// T-B2b (defect B C4): the same `FFF9` body, starting 100 B into its first frame, with an
    /// MP3 frame header `FF FB 90 C4` planted 10 B into that partial — S2's live shape, where
    /// the probe met a false MP3 marker first and resynced for ever. The front end realigns to
    /// the first whole header: `Playing` at 24 000/1, one `Started`, `aligned at` the partial's
    /// length. Without the front end: `Connecting`, never `Playing` (the MP3 marker wins).
    /// Mutation: realign off (a rewrite only at offset 0) → the build bound's `Error`.
    #[test]
    fn t_b2b_a_mid_frame_fff9_start_with_a_false_mp3_marker_realigns() {
        let frames = frames_02();
        let len1 = crate::hls::segment::frame_length(frames.first_chunk().expect("a header"));
        let mut body = frames.repeat(REPEATS_02)[100..].to_vec();
        body[10..14].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC4]);
        let server = paced_server(vec![answer("audio/aac", body, 0, After::Hold)]);
        let h = start_session_supervised(&server.url);
        let seen = states_until(&h, GUARD, is_playing_or_ended);
        assert_eq!(last(&seen), PlaybackState::Playing, "{seen:?}");
        let _ = await_started(&h);
        assert_eq!(*h.started.lock().unwrap(), ["u1"], "one Started");
        assert_eq!(*h.infos.lock().unwrap(), [(24_000, 1)]);
        assert_eq!(
            adts_lines(&h),
            [format!("adts front end: aligned at {} B", len1 - 100)]
        );
        h.ctx.cancel();
    }

    /// F4 (finding 7): T-B2b's shape — station 02's `FFF9` frames, held open — served as
    /// `audio/x-aac`, a type real servers send (Antena 1's HLS segments, `m3c-plan.md:769`).
    /// The front end aligns at 0: `Playing` at 24 000/1, one `Started`. Fails without the
    /// front end on `audio/x-aac`: the build bound's `Error { UnsupportedFormat, "…first 2
    /// s…(audio/x-aac)" }`.
    #[test]
    fn f4_an_x_aac_fff9_mount_plays() {
        let body = frames_02().repeat(REPEATS_02);
        let server = paced_server(vec![answer("audio/x-aac", body, 0, After::Hold)]);
        let h = start_session_supervised(&server.url);
        let seen = states_until(&h, GUARD, is_playing_or_ended);
        assert_eq!(last(&seen), PlaybackState::Playing, "{seen:?}");
        let _ = await_started(&h);
        assert_eq!(*h.started.lock().unwrap(), ["u1"], "one Started");
        assert_eq!(*h.infos.lock().unwrap(), [(24_000, 1)]);
        assert_eq!(adts_lines(&h), ["adts front end: aligned at 0 B"]);
        h.ctx.cancel();
    }

    /// Which streams get the front end: HTTP `audio/aac`, `audio/aacp` and (review fixes F4,
    /// finding 7) `audio/x-aac`, in any case and with parameters; never another type — not
    /// `audio/x-aiff`, whose prefix `audio/x-a` a loose match would take — a missing one, or
    /// the HLS source (review P3). Fails on an exact `audio/aac` match, on a filter that
    /// ignores the source kind, on `audio/x-aac` dropped and on a loose `audio/x-a` prefix.
    #[test]
    fn the_front_end_applies_to_http_aac_only() {
        use stream::SourceKind::{Hls, Http};
        for ct in [
            "audio/aac",
            "audio/aacp",
            "Audio/AAC",
            "audio/aacp; charset=x",
            "audio/x-aac",
            "AUDIO/X-AAC; charset=x",
        ] {
            assert!(wants_adts_front_end(Http, Some(ct)), "{ct}");
            assert!(!wants_adts_front_end(Hls, Some(ct)), "HLS, {ct}");
        }
        for ct in [
            Some("audio/mpeg"),
            Some("audio/x-aiff"),
            Some("application/ogg"),
            None,
        ] {
            assert!(!wants_adts_front_end(Http, ct), "{ct:?}");
        }
    }

    // ---- Defect A: every session plays at its own rate and channel count ----

    /// An HTTP 200 carrying `secs` of a 16-bit sine at `hz`, amplitude 0.5, `channels` identical
    /// channels at `rate`, then the connection closes.
    fn tone_wav_response(rate: u32, channels: u16, hz: f32, secs: f32) -> Vec<u8> {
        let frames = (rate as f32 * secs) as u32;
        let block = 2 * channels as u32;
        let data_len = frames * block;
        let mut w = Vec::new();
        w.extend_from_slice(
            b"HTTP/1.0 200 OK\r\ncontent-type: audio/wav\r\nconnection: close\r\n\r\n",
        );
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * block).to_le_bytes()); // byte rate
        w.extend_from_slice(&(block as u16).to_le_bytes()); // block align
        w.extend_from_slice(&16u16.to_le_bytes()); // bits
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let v = (16_384.0 * (2.0 * std::f32::consts::PI * hz * t).sin()) as i16;
            for _ in 0..channels {
                w.extend_from_slice(&v.to_le_bytes());
            }
        }
        w
    }

    /// Two stations in a row on **one** `Player` and one bare mixer, as `Engine::play` does for
    /// the second: the first session cancelled, a new generation, `clear()`, a new decode
    /// thread. Returns the mixer's output from the second session's creation on, holding at
    /// least `frames` frames from its tone's onset. The pull runs at about twice real time,
    /// like `start_session`'s.
    fn two_sessions(
        first: Vec<u8>,
        second: Vec<u8>,
        mixer_rate: u32,
        mixer_ch: u16,
        frames: usize,
    ) -> Vec<f32> {
        let (ev_tx, ev_rx) = mpsc::channel();
        let shared = Shared {
            state: Arc::new(Mutex::new(PlaybackState::Idle)),
            events: ev_tx,
            gains: EqGains::default(),
            paused: Arc::new(AtomicBool::new(false)),
            session: Arc::new(Mutex::new(Session::default())),
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let (mixer, mixer_out) = rodio::mixer::mixer(
            std::num::NonZero::new(mixer_ch).expect("channels"),
            std::num::NonZero::new(mixer_rate).expect("rate"),
        );
        let player = Arc::new(Player::connect_new(&mixer));
        let capture: Arc<Mutex<Option<Vec<f32>>>> = Arc::new(Mutex::new(None));
        let cap = capture.clone();
        let chunk = mixer_rate as usize * mixer_ch as usize / 20;
        thread::spawn(move || {
            let mut out = mixer_out;
            loop {
                // Only a chunk begun while the capture was on is kept: one pulled before could
                // carry the first station's tail, cleared by the time the capture starts.
                let on = cap.lock().unwrap().is_some();
                let pulled: Vec<f32> = out.by_ref().take(chunk).collect();
                if on && let Some(v) = cap.lock().unwrap().as_mut() {
                    v.extend_from_slice(&pulled);
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        let client = stream::build_client("Ondar/test");
        let start = |body: Vec<u8>, station: &str| -> SessionCtx {
            let (url, _) = scripted_server(vec![body]);
            let generation = shared.begin_session(station.into());
            player.clear();
            let ctx = SessionCtx {
                cancel: Arc::new(AtomicBool::new(false)),
                download: Arc::new(Mutex::new(None)),
                ring: Arc::new(Mutex::new(None)),
                backoff: Arc::new(Mutex::new(Backoff::default())),
                reconnect_count: Arc::new(AtomicU64::new(0)),
                generation,
                clock: Arc::new(BuildClock::new(BuildBounds::for_prefetch(
                    stream::prefetch_bytes(None),
                ))),
                shared: shared.clone(),
            };
            shared.set_state(PlaybackState::Connecting);
            let session = ctx.clone();
            let url = stream::parse_url(&url).expect("url");
            let client = client.clone();
            let handle = rt.handle().clone();
            let player = player.clone();
            let prefetch = stream::prefetch_bytes(None);
            let output = OutputFormat {
                channels: std::num::NonZero::new(mixer_ch).expect("channels"),
                sample_rate: std::num::NonZero::new(mixer_rate).expect("rate"),
            };
            thread::spawn(move || {
                run_session(session, url, client, handle, player, prefetch, output)
            });
            ctx
        };
        // A session's first `Playing` is read off the event stream as its `Started`, which
        // names the station: a 1.5 s tone pulled at twice real time plays for ~0.75 s, short
        // enough for a sample of the state to miss on a slow runner, and a sample could not
        // tell the two sessions apart.
        let wait_started = |station: &str| {
            let deadline = Instant::now() + GUARD;
            loop {
                let ev = match ev_rx.try_recv() {
                    Ok(ev) => Ok(ev),
                    Err(_) => {
                        poll_delay();
                        let left = deadline.saturating_duration_since(Instant::now());
                        ev_rx.recv_timeout(left)
                    }
                };
                match ev {
                    Ok(EngineEvent::Started { station_id }) if station_id == station => return,
                    Ok(_) => {}
                    Err(_) => panic!("the {station} session never reached Playing"),
                }
            }
        };

        let one = start(first, "first");
        wait_started("first");
        thread::sleep(Duration::from_millis(300));
        one.cancel();
        let _two = start(second, "second");
        // Captured from here: `start` has already cleared the first station's source, and the
        // second cannot sound before its prefetch. Starting at its `Started` instead made the
        // capture depend on how soon the test woke — a late wake missed the tone.
        *capture.lock().unwrap() = Some(Vec::new());
        wait_started("second");
        let ch = mixer_ch as usize;
        let deadline = Instant::now() + GUARD;
        loop {
            let after_onset = capture.lock().unwrap().as_ref().map_or(0, |v| {
                v.iter()
                    .step_by(ch)
                    .position(|x| x.abs() > 0.05)
                    .map_or(0, |onset| v.len() / ch - onset)
            });
            if after_onset >= frames {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "captured only {after_onset} frames of the tone"
            );
            thread::sleep(Duration::from_millis(20));
        }
        capture.lock().unwrap().take().expect("capture")
    }

    const WINDOW: usize = 12_000;

    /// The window the tone is read over: `channel`'s samples from the first non-silent frame
    /// (found on channel 0, so every channel's window is aligned) plus 1 000 frames, past any
    /// converter onset, `WINDOW` frames long.
    fn tone_window(out: &[f32], ch: usize, channel: usize) -> Vec<f32> {
        let frames: Vec<f32> = out.iter().skip(channel).step_by(ch).copied().collect();
        let onset = out
            .iter()
            .step_by(ch)
            .position(|v| v.abs() > 0.05)
            .expect("no tone in the capture");
        assert!(
            frames.len() >= onset + 1_000 + WINDOW,
            "capture too short: onset {onset}, {} frames",
            frames.len()
        );
        frames[onset + 1_000..onset + 1_000 + WINDOW].to_vec()
    }

    /// The tone's frequency from rising zero crossings over the window, in Hz at `rate`.
    /// 1 kHz at 48 kHz is 250 cycles in 12 000 frames; ±1 crossing is ±4 Hz (0.4 %).
    fn tone_hz(window: &[f32], rate: u32) -> f64 {
        let rising = window
            .windows(2)
            .filter(|p| p[0] < 0.0 && p[1] >= 0.0)
            .count();
        rising as f64 * rate as f64 / window.len() as f64
    }

    /// Defect A. The second station's output tone over its source tone is the pulled-to-output
    /// ratio, read as heard. Fails at ~0.50 when the mixer's one resampler keeps the first
    /// station's 22 050 Hz: 44 100 Hz content consumed at 22 050 frames/s plays at half speed,
    /// 1 000 Hz heard as 500. The tolerance (±5 %) is ten times the window's resolution and a
    /// tenth of the failing error.
    #[test]
    fn a_later_session_plays_at_its_own_rate() {
        let out = two_sessions(
            tone_wav_response(22_050, 2, 440.0, 1.5),
            tone_wav_response(44_100, 2, 1_000.0, 1.5),
            48_000,
            2,
            28_800,
        );
        let ratio = tone_hz(&tone_window(&out, 2, 0), 48_000) / 1_000.0;
        assert!(
            (ratio - 1.0).abs() < 0.05,
            "second station's output/source frequency ratio {ratio:.3}, expected 1.00"
        );
    }

    /// Defect A, the channel count. A mono station after a stereo one: fails at ~2.0 per
    /// channel, with L ≠ R, when the mixer's resampler keeps the first station's two channels
    /// and reads the mono samples as interleaved pairs, each channel taking every other one.
    #[test]
    fn a_mono_session_after_stereo_is_not_read_as_stereo() {
        let out = two_sessions(
            tone_wav_response(48_000, 2, 440.0, 1.5),
            tone_wav_response(48_000, 1, 1_000.0, 1.5),
            48_000,
            2,
            28_800,
        );
        let left = tone_window(&out, 2, 0);
        let right = tone_window(&out, 2, 1);
        let l = tone_hz(&left, 48_000) / 1_000.0;
        let r = tone_hz(&right, 48_000) / 1_000.0;
        let worst = left
            .iter()
            .zip(&right)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            (l - 1.0).abs() < 0.05 && (r - 1.0).abs() < 0.05 && worst < 1e-6,
            "mono after stereo: ratios L {l:.3} R {r:.3}, max |L-R| {worst:.4}; expected 1.00, 1.00, 0"
        );
    }

    // ---------------------------------------------------------------------------------------
    // HLS (M3c commit 4): T12–T17 against a path-routed server serving the census fixtures.
    //
    // The server hands out playlists byte for byte (10's media as the gzip bytes it was
    // served) and synthesises segments from the fixture heads by repeating a head's 16 ADTS
    // frames to the `EXTINF` duration (plan §4, R3): the decoder sees ordinary segments, and the
    // prefetch, the fill target and the pacing are production's. Nothing in this block names
    // `crate::hls`, so the same text ran on `b7e050a` for the recorded failures.

    const HLS_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/hls/");

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{HLS_FIXTURES}{name}"))
            .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    struct Routed {
        status: u16,
        content_type: &'static str,
        gzip: bool,
        /// Extra header lines, each ending in `\r\n`.
        headers: &'static str,
        body: Vec<u8>,
    }

    fn routed(content_type: &'static str, body: Vec<u8>) -> Routed {
        Routed {
            status: 200,
            content_type,
            gzip: false,
            headers: "",
            body,
        }
    }

    /// A path-routed HTTP/1.0 server on 127.0.0.1: `handler(path_and_query)` answers each
    /// request (or `None` → 404), and every path is recorded in order. Connections close after
    /// one response, as `scripted_server`'s do.
    fn routed_server(
        handler: impl Fn(&str) -> Option<Routed> + Send + Sync + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let paths = Arc::new(Mutex::new(Vec::new()));
        let seen = paths.clone();
        let mut segments_seen = 0usize;
        thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                if is_segment_path(&path) {
                    if segments_seen >= 2 {
                        late_request_delay();
                    }
                    segments_seen += 1;
                }
                seen.lock().unwrap().push(path.clone());
                server_delay();
                let response = handler(&path).unwrap_or(Routed {
                    status: 404,
                    content_type: "text/plain",
                    gzip: false,
                    headers: "",
                    body: b"not found".to_vec(),
                });
                let mut out = format!(
                    "HTTP/1.0 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
                    response.status,
                    response.content_type,
                    response.body.len()
                )
                .into_bytes();
                if response.gzip {
                    out.extend_from_slice(b"content-encoding: gzip\r\n");
                }
                out.extend_from_slice(response.headers.as_bytes());
                out.extend_from_slice(b"\r\n");
                out.extend_from_slice(&response.body);
                let _ = sock.write_all(&out);
                let _ = sock.flush();
                thread::sleep(Duration::from_millis(20));
            }
        });
        (format!("http://{addr}"), paths)
    }

    /// The offset after every leading ID3v2 tag (the test-side copy; the crate's own lives in
    /// `hls::segment`, which this block must not name).
    fn test_id3_end(b: &[u8]) -> usize {
        let mut off = 0;
        while b.len() >= off + 10 && &b[off..off + 3] == b"ID3" {
            let size = ((b[off + 6] as usize & 0x7F) << 21)
                | ((b[off + 7] as usize & 0x7F) << 14)
                | ((b[off + 8] as usize & 0x7F) << 7)
                | (b[off + 9] as usize & 0x7F);
            off += 10 + size + if b[off + 5] & 0x10 != 0 { 10 } else { 0 };
        }
        off.min(b.len())
    }

    /// A segment of `secs` seconds synthesised from a fixture head: its ID3 tag(s), then its
    /// 16 frames repeated `ceil(secs × rate / 1024 / 16)` times. Repeated ADTS frames are a
    /// valid stream — each frame is self-contained.
    fn synth_segment(head: &[u8], secs: f64) -> Vec<u8> {
        const RATES: [f64; 13] = [
            96000., 88200., 64000., 48000., 44100., 32000., 24000., 22050., 16000., 12000., 11025.,
            8000., 7350.,
        ];
        let tags = test_id3_end(head);
        let frames = &head[tags..];
        let sri = ((frames[2] >> 2) & 0x0F) as usize;
        let rate = RATES[sri];
        let k = (secs * rate / 1024.0 / 16.0).ceil() as usize;
        let mut out = head[..tags].to_vec();
        for _ in 0..k {
            out.extend_from_slice(frames);
        }
        out
    }

    /// A live media playlist as a server would publish it at `elapsed` since it started: one
    /// segment of `dur` seconds per `dur` elapsed, a sliding window of `window` segments ending
    /// at the current sequence number, URIs `seg-<seq>.aac`.
    fn live_playlist(
        td: u64,
        dur: f64,
        first_seq: u64,
        window: usize,
        elapsed: Duration,
    ) -> String {
        let now_seq = first_seq + (elapsed.as_secs_f64() / dur).floor() as u64;
        let from = now_seq.saturating_sub(window as u64 - 1).max(first_seq);
        let mut s = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{td}\n#EXT-X-MEDIA-SEQUENCE:{from}\n"
        );
        for seq in from..=now_seq {
            s.push_str(&format!("#EXTINF:{dur:.3},\nseg-{seq}.aac\n"));
        }
        s
    }

    fn seg_seq(path: &str) -> Option<u64> {
        path.rsplit('/')
            .next()?
            .strip_prefix("seg-")?
            .strip_suffix(".aac")?
            .parse()
            .ok()
    }

    fn is_playing(s: &PlaybackState) -> bool {
        matches!(s, PlaybackState::Playing)
    }

    fn error_message(s: &PlaybackState) -> String {
        match s {
            PlaybackState::Error { message, .. } => message.clone(),
            other => format!("{other:?}"),
        }
    }

    fn count_prefix(paths: &[String], prefix: &str) -> usize {
        paths.iter().filter(|p| p.starts_with(prefix)).count()
    }

    /// T12: Antena 1's shape as captured — a master with a relative variant URI, a media
    /// playlist served gzip, ADTS segments — reaches `Playing` with `StreamInfo` 48 000 / 2
    /// and one `Started`. The master is requested **twice** (the `HttpStream` GET, then
    /// `hls::open`'s own — review R1), then the media playlist, then the start segments
    /// 97880–97882 (three from the end, D5). Without the HLS path (F7): `Error {
    /// UnsupportedFormat }` after one request, no `Playing`. Fails without gunzip — the parser
    /// refuses the media playlist's compressed bytes as `NotPlaylist` ("not a playlist (no
    /// #EXTM3U)"), so the session ends `Error { UnsupportedFormat }` (the census probe's run-1
    /// bug was the other shape: garbage URIs, because its parser had no `#EXTM3U` check) — or
    /// without the dispatch.
    #[test]
    fn t12_adts_hls_behind_a_gzipped_media_playlist_plays() {
        let master = fixture("10-master.m3u8");
        let media_gz = fixture("10-media.m3u8.gz");
        let head = fixture("10-seg-head.aac");
        let (base, paths) = routed_server(move |path| match path {
            "/liveradio/antena180a/playlist.m3u8" => {
                Some(routed("application/vnd.apple.mpegurl", master.clone()))
            }
            "/liveradio/antena180a/chunklist.m3u8" => Some(Routed {
                status: 200,
                content_type: "application/vnd.apple.mpegurl",
                gzip: true,
                headers: "",
                body: media_gz.clone(),
            }),
            p if p.starts_with("/liveradio/antena180a/media_") && p.ends_with(".aac") => {
                Some(routed("audio/x-aac", synth_segment(&head, 4.0)))
            }
            _ => None,
        });
        let h = start_session(&format!("{base}/liveradio/antena180a/playlist.m3u8"));
        let seen = states_until(&h, GUARD, is_playing);
        let state = last(&seen);
        assert_eq!(state, PlaybackState::Playing, "states: {seen:?}");
        assert!(reconnecting(&seen).is_empty(), "no backoff: {seen:?}");
        assert_eq!(*h.infos.lock().unwrap(), vec![(48_000, 2)], "StreamInfo");
        // Stopped at `Playing`: its `Started` is awaited, not assumed.
        let _ = await_started(&h);
        assert_eq!(
            *h.started.lock().unwrap(),
            vec!["u1".to_string()],
            "one Started"
        );
        // Review 2, G1: `hls::open` attaches the arrival clock (no other test sees its wiring;
        // fails with its `on_progress` removed).
        assert!(
            h.ctx.clock.arrivals().is_some_and(|a| a.any()),
            "no arrival stamped on the HLS open"
        );
        let h_paths = paths;
        let paths = h_paths.lock().unwrap().clone();
        assert_eq!(
            &paths[..4],
            &[
                "/liveradio/antena180a/playlist.m3u8".to_string(),
                "/liveradio/antena180a/playlist.m3u8".to_string(),
                "/liveradio/antena180a/chunklist.m3u8".to_string(),
                "/liveradio/antena180a/media_97880.aac".to_string(),
            ],
            "two requests for the master (R1), then the media playlist, then the start segment: {paths:?}"
        );
        // The third start segment is requested once the task's two-segment channel has room —
        // after the reader has taken one — so it is waited for, not sampled at `Playing`
        // (4a388f5's CI run read the log with 97880 and 97881 only).
        let log = h_paths.clone();
        let _ = states_while_waiting_for(&h, GUARD, move || {
            log.lock()
                .unwrap()
                .contains(&"/liveradio/antena180a/media_97882.aac".to_string())
        });
        let paths = h_paths.lock().unwrap().clone();
        assert!(
            paths.contains(&"/liveradio/antena180a/media_97882.aac".to_string()),
            "the three start segments are fetched: {paths:?}"
        );
        h.ctx.cancel();
    }

    /// T13: a media playlist given directly whose segments are MPEG-TS (Известия, 09) → one
    /// terminal `Error { UnsupportedFormat, "…MPEG-TS…" }`, no `Reconnecting`, **3** requests
    /// (the playlist twice — R1 — and one segment, read only to its head), no `Started`.
    /// Without the HLS path the state passes, but the count is 1 and the message the generic
    /// one (F7).
    /// Fails if the refusal is not terminal (`Reconnecting { 1 }` inside the window), or if a
    /// third playlist request is made.
    #[test]
    fn t13_mpeg_ts_segments_are_refused_terminally_after_one_segment_head() {
        let media = fixture("09-media.m3u8");
        let ts = fixture("09-seg-head.mpegts");
        let (base, paths) = routed_server(move |path| match path {
            "/igi/radio1/tracks-a1/mono.m3u8" => {
                Some(routed("application/vnd.apple.mpegurl", media.clone()))
            }
            p if p.contains("-06016.ts?hls_proxy_host=") => Some(routed("video/MP2T", ts.clone())),
            _ => None,
        });
        let h = start_session(&format!("{base}/igi/radio1/tracks-a1/mono.m3u8"));
        let seen = states_until(&h, GUARD, is_error);
        let state = last(&seen);
        assert!(
            matches!(
                &state,
                PlaybackState::Error {
                    code: ErrorCode::UnsupportedFormat,
                    ..
                }
            ),
            "state: {state:?}"
        );
        assert_eq!(
            error_message(&state),
            "HLS with MPEG-TS segments is not supported yet"
        );
        assert!(
            reconnecting(&seen).is_empty(),
            "terminal, no backoff: {seen:?}"
        );
        assert!(h.started.lock().unwrap().is_empty(), "no vote");
        thread::sleep(Duration::from_millis(300));
        let paths = paths.lock().unwrap().clone();
        assert_eq!(paths.len(), 3, "2 playlist + 1 segment head: {paths:?}");
        assert_eq!(count_prefix(&paths, "/igi/radio1/tracks-a1/mono.m3u8"), 2);
        assert!(paths[2].contains(".ts?hls_proxy_host="), "{paths:?}");
    }

    /// T14: a master whose variants are all video (Fox, 03) → `Error { UnsupportedFormat,
    /// "…video only…" }` after **2** requests (the master twice, R1) and none for a media
    /// playlist. Without the HLS path: 1 request, the generic message. Fails with the
    /// video-only filter dropped: a media playlist (on the real host — the fixture's URIs are
    /// absolute) and its segment would be fetched.
    #[test]
    fn t14_a_video_only_master_is_refused_after_two_requests() {
        let master = fixture("03-master.m3u8");
        let (base, paths) = routed_server(move |path| match path {
            "/hls/live/2020027/fncv3preview/primary.m3u8" => {
                Some(routed("application/x-mpegURL", master.clone()))
            }
            _ => None,
        });
        let h = start_session(&format!(
            "{base}/hls/live/2020027/fncv3preview/primary.m3u8"
        ));
        let seen = states_until(&h, GUARD, is_error);
        let state = last(&seen);
        assert_eq!(
            error_message(&state),
            "HLS stream has no audio variant (video only: avc1.42c020)",
            "state: {state:?}"
        );
        assert!(reconnecting(&seen).is_empty(), "{seen:?}");
        assert!(h.started.lock().unwrap().is_empty());
        thread::sleep(Duration::from_millis(300));
        let paths = paths.lock().unwrap().clone();
        assert_eq!(paths.len(), 2, "the master twice, nothing else: {paths:?}");
    }

    /// A live server: `live_playlist` at the elapsed time (frozen at `freeze_after` if set),
    /// segments synthesised from the head `head_for(seq)` picks.
    fn live_server(
        td: u64,
        dur: f64,
        window: usize,
        freeze_after: Option<Duration>,
        head_for: impl Fn(u64) -> Vec<u8> + Send + Sync + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let started = Instant::now();
        routed_server(move |path| {
            if path == "/live/playlist.m3u8" {
                let mut elapsed = started.elapsed();
                if let Some(f) = freeze_after {
                    elapsed = elapsed.min(f);
                }
                return Some(routed(
                    "application/vnd.apple.mpegurl",
                    live_playlist(td, dur, 1, window, elapsed).into_bytes(),
                ));
            }
            let seq = seg_seq(path)?;
            Some(routed("audio/aac", synth_segment(&head_for(seq), dur)))
        })
    }

    /// T15 (finding F4): a live playlist with 6 s segments, refreshed every 6 s, plays 13 s
    /// with **no** internal reconnect and no `Reconnecting`. With the ICY `Settings` (a 5 s
    /// `retry_timeout` and its `on_reconnect`) every normal wait between segments would count
    /// as an internal reconnect and bump `reconnect_count` — the mutation. Costs 13 s.
    #[test]
    fn t15_a_normal_hls_wait_is_not_an_internal_reconnect() {
        let head = fixture("10-seg-head.aac");
        let (base, paths) = live_server(6, 6.0, 5, None, move |_| head.clone());
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        // A claim window: over 13 s of a healthy live playlist, no `Reconnecting`, no `Error`
        // and no internal reconnect. Slowness cannot add one of those to a correct build; it
        // can only delay events, which the window then does not see — never a false failure.
        let seen = states_until(&h, Duration::from_secs(13), |_| false);
        assert!(
            seen.contains(&PlaybackState::Playing),
            "it played: {seen:?}"
        );
        assert!(!seen.iter().any(is_error), "{seen:?}");
        assert!(reconnecting(&seen).is_empty(), "{seen:?}");
        assert_eq!(
            h.ctx.reconnect_count.load(Ordering::Relaxed),
            0,
            "stream-download's idle timeout fired during a normal HLS wait"
        );
        // An upper bound read at the window's end — slowness only delays reloads — then the
        // lower bound awaited: the first refresh (the third playlist request) is waited for,
        // not expected inside the window.
        let reloads = count_prefix(&paths.lock().unwrap(), "/live/playlist.m3u8");
        assert!(
            reloads <= 5,
            "2 for the open + at most a refresh per ~6 s in 13 s at TD 6: {reloads}"
        );
        let log = paths.clone();
        let _ = states_while_waiting_for(&h, GUARD, move || {
            count_prefix(&log.lock().unwrap(), "/live/playlist.m3u8") >= 3
        });
        assert!(
            count_prefix(&paths.lock().unwrap(), "/live/playlist.m3u8") >= 3,
            "the media playlist was refreshed"
        );
        let _ = await_started(&h);
        assert_eq!(*h.started.lock().unwrap(), vec!["u1".to_string()]);
        // Review P3 (defect B C4): the segments are `audio/aac`, so a content-type filter would
        // put the front end on this HLS session too; the source kind keeps it off. Fails with
        // `wants_adts_front_end` ignoring the kind ("adts front end: aligned at 0 B").
        assert_eq!(
            adts_lines(&h),
            Vec::<String>::new(),
            "an HLS session gets no ADTS front end"
        );
        h.ctx.cancel();
    }

    /// T16: the window stops advancing → the stall bound (3 × TD) ends the source → the
    /// session's own backoff, `Reconnecting { 1 }` → the playlist is requested again → `Playing`
    /// — and **one `Started` in total**: a reopen is inside the session. Without the HLS path:
    /// `Error`, 0 `Started`. Fails without the stall bound (the task waits for ever; nothing
    /// ends the source, and the watchdog does not see a full ring under `Playing`).
    #[test]
    fn t16_a_stalled_window_reopens_inside_the_session_with_one_started() {
        let head = fixture("10-seg-head.aac");
        let (base, paths) = live_server(1, 1.0, 4, Some(Duration::from_secs(2)), move |_| {
            head.clone()
        });
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let seen = states_until(&h, GUARD, is_playing);
        assert_eq!(last(&seen), PlaybackState::Playing, "first play: {seen:?}");
        let opens_before = count_prefix(&paths.lock().unwrap(), "/live/playlist.m3u8");
        // The stall → Reconnecting → Playing again, read in order off the event stream: the
        // reopen's `Playing` lasts only until the still-stalled window ends it again, and a
        // sample of the state could miss it and see `Reconnecting { 2 }`.
        let mut all = seen;
        all.extend(states_until(&h, GUARD, |s| {
            matches!(s, PlaybackState::Reconnecting { .. })
        }));
        all.extend(states_until(&h, GUARD, is_playing));
        assert_eq!(last(&all), PlaybackState::Playing, "no reopen: {all:?}");
        assert_eq!(reconnecting(&all), vec![1], "one backoff step: {all:?}");
        assert_eq!(
            *h.started.lock().unwrap(),
            vec!["u1".to_string()],
            "one Started across the reopen"
        );
        let paths = paths.lock().unwrap().clone();
        let opens_after = count_prefix(&paths, "/live/playlist.m3u8");
        assert!(
            opens_after >= opens_before + 2,
            "the reopen requested the playlist again (twice, R1): {opens_before} → {opens_after}"
        );
        h.ctx.cancel();
    }

    /// T17: a segment whose ADTS format differs from the session's first (48 000 / 2, then
    /// 08's 22 050 / 2) ends the source; the reopen builds a new decoder, ring and converter,
    /// so `StreamInfo` is emitted twice — 48 000 then 22 050 — and `Started` once. The
    /// guard-off mutation still passes: Symphonia's ADTS reader ends the stream on a header
    /// whose rate differs, so the reopen happens either way; the guard is the first line (T10
    /// pins it), the decoder the second, and this test pins the outcome.
    #[test]
    fn t17_a_format_change_reopens_with_a_new_stream_info_and_one_started() {
        let head48 = fixture("10-seg-head.aac");
        let head22 = fixture("08-seg-head.aac");
        // A one-segment window, so the reopen (three from the end = the newest) lands past the
        // switch at seq 4; with a wider window it starts inside the old 48 000 segments and
        // sees a third `StreamInfo` (measured: `[48000, 48000, 22050]` with a window of 4).
        let (base, _paths) = live_server(1, 1.0, 1, None, move |seq| {
            if seq < 4 {
                head48.clone()
            } else {
                head22.clone()
            }
        });
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        // First play, the format change's reopen, the reopened play — in order.
        let mut all = states_until(&h, GUARD, is_playing);
        all.extend(states_until(&h, GUARD, |s| {
            matches!(s, PlaybackState::Reconnecting { .. })
        }));
        all.extend(states_until(&h, GUARD, is_playing));
        assert_eq!(
            last(&all),
            PlaybackState::Playing,
            "no reopened play in time: {all:?} infos {:?}",
            h.infos.lock().unwrap()
        );
        assert_eq!(*h.infos.lock().unwrap(), vec![(48_000, 2), (22_050, 2)]);
        assert_eq!(reconnecting(&all), vec![1], "{all:?}");
        assert_eq!(*h.started.lock().unwrap(), vec!["u1".to_string()]);
        h.ctx.cancel();
    }

    /// A static media playlist of `n` one-second segments from `first_seq`, segments
    /// synthesised from `head` unless `status_for(seq, playlist_requests)` says otherwise —
    /// `playlist_requests` is how many times the playlist had been asked for, so a test can
    /// key an answer to the open it belongs to rather than to a clock.
    fn eviction_server(
        first_seq: u64,
        n: usize,
        head: Vec<u8>,
        status_for: impl Fn(u64, usize) -> Option<u16> + Send + Sync + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let playlist_requests = AtomicUsize::new(0);
        routed_server(move |path| {
            if path == "/live/playlist.m3u8" {
                playlist_requests.fetch_add(1, Ordering::SeqCst);
                return Some(routed(
                    "application/vnd.apple.mpegurl",
                    live_playlist(1, 1.0, first_seq, n, Duration::from_secs(n as u64 - 1))
                        .into_bytes(),
                ));
            }
            let seq = seg_seq(path)?;
            match status_for(seq, playlist_requests.load(Ordering::SeqCst)) {
                Some(status) => Some(Routed {
                    status,
                    content_type: "text/plain",
                    gzip: false,
                    headers: "",
                    body: b"gone".to_vec(),
                }),
                None => Some(routed("audio/aac", synth_segment(&head, 1.0))),
            }
        })
    }

    /// Review finding 4 (fix B): a 404 on the **first** segment means the origin evicted it —
    /// the next pending segment is tried and plays. Fails with the playlist policy applied to
    /// a segment: the session ends `Error { Http, "…answered HTTP 404 Not Found" }`, terminal,
    /// after one chain.
    #[test]
    fn t18_an_evicted_first_segment_is_skipped_for_the_next_one() {
        let head = fixture("10-seg-head.aac");
        // Window 1..5; the start is three from the end = 3; 3 is gone, 4 and 5 are there.
        let (base, paths) = eviction_server(1, 5, head, |seq, _| (seq == 3).then_some(404));
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let seen = states_until(&h, GUARD, is_playing);
        assert_eq!(last(&seen), PlaybackState::Playing, "states: {seen:?}");
        assert!(
            reconnecting(&seen).is_empty(),
            "no backoff for one evicted segment: {seen:?}"
        );
        // Stopped at `Playing`: its `Started` is awaited, not assumed.
        let _ = await_started(&h);
        assert_eq!(*h.started.lock().unwrap(), vec!["u1".to_string()]);
        let paths = paths.lock().unwrap().clone();
        let segs: Vec<u64> = paths.iter().filter_map(|p| seg_seq(p)).collect();
        assert_eq!(&segs[..2], &[3, 4], "3 tried (404), then 4: {paths:?}");
        h.ctx.cancel();
    }

    /// Fix B, the other half: every start segment evicted → not terminal — the session's
    /// backoff reopens (`Reconnecting { 1 }`), and once the window has moved on it plays. Fails
    /// with the playlist policy applied to a segment: `Error { Http }` at once, no
    /// `Reconnecting`. The segments are gone for the
    /// **first open** — while the playlist has been asked for at most twice (R1: the
    /// `HttpStream` GET, then `hls::open`'s own) — and back for the reopen, the third and
    /// fourth requests. Keyed to requests, not to a clock (review 2, finding 6: an 800 ms window
    /// from before `start_session` read `[]` once the open chain took longer — reproduced with
    /// `ONDAR_TEST_SERVER_DELAY_MS=300`).
    #[test]
    fn t19_all_start_segments_evicted_reopens_through_the_backoff() {
        let head = fixture("10-seg-head.aac");
        let (base, _paths) = eviction_server(1, 5, head, |_, playlist_requests| {
            (playlist_requests <= 2).then_some(410)
        });
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let seen = states_until(&h, GUARD, is_playing);
        assert_eq!(last(&seen), PlaybackState::Playing, "states: {seen:?}");
        assert_eq!(
            reconnecting(&seen),
            vec![1],
            "one backoff step, then it plays: {seen:?}"
        );
        // Stopped at `Playing`: its `Started` is awaited, not assumed.
        let _ = await_started(&h);
        assert_eq!(*h.started.lock().unwrap(), vec!["u1".to_string()]);
        h.ctx.cancel();
    }

    /// Review 2, finding 5: every start segment gone is an answer the server sent, so the
    /// open's error is `Http` — retriable (`terminal: false`, the backoff reopens on a fresher
    /// window) and carrying the server's `Retry-After` — not `Network`, which put `network: …
    /// 404 …` on the page after the fifth attempt, code and message disagreeing. `hls::open`
    /// is called directly: `run_session` shows its error only after 31 s of backoff. Fails with
    /// the gone segments read as a transport failure: `(Network, false, None)`.
    #[test]
    fn t26_start_segments_gone_is_a_retriable_http_error_with_its_retry_after() {
        let (base, _paths) = routed_server(|path| {
            if path == "/live/playlist.m3u8" {
                return Some(routed(
                    "application/vnd.apple.mpegurl",
                    live_playlist(1, 1.0, 1, 5, Duration::from_secs(4)).into_bytes(),
                ));
            }
            seg_seq(path).map(|_| Routed {
                status: 410,
                content_type: "text/plain",
                gzip: false,
                headers: "retry-after: 7\r\n",
                body: b"gone".to_vec(),
            })
        });
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let client = stream::build_client("Ondar/test");
        let url = stream::parse_url(&format!("{base}/live/playlist.m3u8")).expect("url");
        let e = match rt.block_on(crate::hls::open(
            &client,
            url,
            Arc::new(AtomicU64::new(0)),
            Arc::new(Arrivals::new()),
            stream::PREFETCH_FLOOR_BYTES,
        )) {
            Ok(_) => panic!("open succeeded with every start segment gone"),
            Err(e) => e,
        };
        assert_eq!(
            (e.code, e.terminal, e.retry_after),
            (ErrorCode::Http, false, Some(Duration::from_secs(7))),
            "{}",
            e.message
        );
        assert!(e.message.contains("410"), "{}", e.message);
    }

    /// Fix B keeps 401/403 terminal: access denial (a geo-block) does not change with a retry,
    /// and five backoff attempts before the same answer would be worse than the honest error
    /// now. One chain: the playlist twice, the segment once. Unchanged by fix B.
    #[test]
    fn t20_a_forbidden_first_segment_is_terminal_after_one_chain() {
        let head = fixture("10-seg-head.aac");
        let (base, paths) = eviction_server(1, 5, head, |_, _| Some(403));
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let seen = states_until(&h, GUARD, is_error);
        let state = last(&seen);
        assert!(
            matches!(
                &state,
                PlaybackState::Error {
                    code: ErrorCode::Http,
                    ..
                }
            ),
            "state: {state:?}"
        );
        assert!(error_message(&state).contains("403"), "{state:?}");
        assert!(reconnecting(&seen).is_empty(), "{seen:?}");
        assert!(h.started.lock().unwrap().is_empty());
        thread::sleep(Duration::from_millis(300));
        let paths = paths.lock().unwrap().clone();
        assert_eq!(paths.len(), 3, "playlist ×2 + one segment: {paths:?}");
    }

    /// Review finding 5 (fix C): a **gzip-encoded** first segment must still be sniffed after
    /// inflating. Fails with one flag serving both the early sniff and the post-inflate one —
    /// a gzipped body gets neither — so a gzipped MPEG-TS segment reads the generic "HLS
    /// segment format not recognised" instead of the MPEG-TS message T13 pins for the plain
    /// case.
    #[test]
    fn t21_a_gzipped_ts_first_segment_is_refused_as_mpeg_ts() {
        let media = fixture("09-media.m3u8");
        let ts = fixture("09-seg-head.mpegts");
        let gz = {
            use std::io::Write;
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&ts).unwrap();
            e.finish().unwrap()
        };
        let (base, paths) = routed_server(move |path| match path {
            "/igi/radio1/tracks-a1/mono.m3u8" => {
                Some(routed("application/vnd.apple.mpegurl", media.clone()))
            }
            p if p.contains("-06016.ts?hls_proxy_host=") => Some(Routed {
                status: 200,
                content_type: "video/MP2T",
                gzip: true,
                headers: "",
                body: gz.clone(),
            }),
            _ => None,
        });
        let h = start_session(&format!("{base}/igi/radio1/tracks-a1/mono.m3u8"));
        let seen = states_until(&h, GUARD, is_error);
        let state = last(&seen);
        assert_eq!(
            error_message(&state),
            "HLS with MPEG-TS segments is not supported yet",
            "state: {state:?}"
        );
        assert!(reconnecting(&seen).is_empty(), "{seen:?}");
        assert!(h.started.lock().unwrap().is_empty());
        thread::sleep(Duration::from_millis(300));
        assert_eq!(paths.lock().unwrap().len(), 3);
    }

    /// Review 2, finding 1, through a caller: a gzip-encoded media playlist whose body is a
    /// few KB but inflates past `PLAYLIST_MAX_BYTES` (a 2 MiB comment line) is refused as the
    /// over-cap body already was — terminal, after the two requests of R1. Fails with the
    /// inflate unbounded (the session plays: `[Buffering, Playing]`), or if a caller passes a
    /// cap other than the playlist's.
    #[test]
    fn t22_a_gzipped_playlist_that_inflates_past_the_cap_is_refused() {
        let head = fixture("10-seg-head.aac");
        let text = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:1\n#{}\n#EXTINF:1.0,\nseg-1.aac\n#EXTINF:1.0,\nseg-2.aac\n#EXTINF:1.0,\nseg-3.aac\n",
            " ".repeat(2 * 1024 * 1024)
        );
        let gz = {
            use std::io::Write;
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            e.write_all(text.as_bytes()).unwrap();
            e.finish().unwrap()
        };
        assert!(gz.len() < 64 * 1024, "compressed {} bytes", gz.len());
        let (base, paths) = routed_server(move |path| match path {
            "/live/playlist.m3u8" => Some(Routed {
                status: 200,
                content_type: "application/vnd.apple.mpegurl",
                gzip: true,
                headers: "",
                body: gz.clone(),
            }),
            p if seg_seq(p).is_some() => Some(routed("audio/aac", synth_segment(&head, 1.0))),
            _ => None,
        });
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let seen = states_until(&h, GUARD, |s| is_error(s) || is_playing(s));
        let state = last(&seen);
        assert!(
            matches!(
                &state,
                PlaybackState::Error {
                    code: ErrorCode::UnsupportedFormat,
                    ..
                }
            ),
            "state: {state:?} after {seen:?}"
        );
        assert!(
            error_message(&state).contains("over 1048576 bytes"),
            "{state:?}"
        );
        assert!(reconnecting(&seen).is_empty(), "{seen:?}");
        thread::sleep(Duration::from_millis(300));
        assert_eq!(paths.lock().unwrap().len(), 2, "the playlist twice (R1)");
        h.ctx.cancel();
    }

    /// A static window 1..=5 of one-second segments under `TARGETDURATION:10`: the open takes
    /// seq 3 (three from the end), the fetch task seq 4 then 5. `status_for(seq, n)` answers
    /// the `n`th request (from 1) for `seq`, `None` for a segment.
    fn task_segment_server(
        status_for: impl Fn(u64, usize) -> Option<u16> + Send + Sync + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let head = fixture("10-seg-head.aac");
        let counts: Mutex<std::collections::HashMap<u64, usize>> = Mutex::default();
        routed_server(move |path| {
            if path == "/live/playlist.m3u8" {
                return Some(routed(
                    "application/vnd.apple.mpegurl",
                    live_playlist(10, 1.0, 1, 5, Duration::from_secs(4)).into_bytes(),
                ));
            }
            let seq = seg_seq(path)?;
            let n = {
                let mut c = counts.lock().unwrap();
                let n = c.entry(seq).or_insert(0);
                *n += 1;
                *n
            };
            match status_for(seq, n) {
                Some(status) => Some(Routed {
                    status,
                    content_type: "text/plain",
                    gzip: false,
                    headers: "",
                    body: b"gone".to_vec(),
                }),
                None => Some(routed("audio/aac", synth_segment(&head, 1.0))),
            }
        })
    }

    /// The segment sequence numbers requested once seq 5 has been, or after `within`.
    fn segs_until_seq5(paths: &Arc<Mutex<Vec<String>>>, within: Duration) -> Vec<u64> {
        let deadline = Instant::now() + within;
        loop {
            let segs: Vec<u64> = paths
                .lock()
                .unwrap()
                .iter()
                .filter_map(|p| seg_seq(p))
                .collect();
            if segs.contains(&5) || Instant::now() >= deadline {
                return segs;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Review 2, finding 4 as triaged: a **410** in the fetch task is permanent — one request,
    /// then the next segment. Fails with a 410 on the transient path: retried at 1 s steps for
    /// the whole TD (10 s here) before the gap, ten requests for seq 4 (`[3, 4 ×10, 5]`).
    #[test]
    fn t23_a_gone_segment_in_the_task_is_skipped_after_one_request() {
        let (base, paths) = task_segment_server(|seq, _| (seq == 4).then_some(410));
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let segs = segs_until_seq5(&paths, GUARD);
        h.ctx.cancel();
        assert!(segs.contains(&5), "seq 5 was never requested: {segs:?}");
        assert_eq!(
            segs.iter().filter(|&&s| s == 4).count(),
            1,
            "410 → one request: {segs:?}"
        );
    }

    /// Finding 4, the 404 half: a 404 at the live edge is often CDN propagation lag, so it is
    /// retried — at most twice, at 1 s. Answered 404 twice then 200, the segment plays: three
    /// requests, no gap. Fails if a 404 is skipped at once (one request).
    #[test]
    fn t24_a_404_that_heals_on_the_second_retry_is_not_a_gap() {
        let (base, paths) = task_segment_server(|seq, n| (seq == 4 && n <= 2).then_some(404));
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let segs = segs_until_seq5(&paths, GUARD);
        h.ctx.cancel();
        assert!(segs.contains(&5), "seq 5 was never requested: {segs:?}");
        assert_eq!(
            segs.iter().filter(|&&s| s == 4).count(),
            3,
            "404, 404, 200: {segs:?}"
        );
    }

    /// Round-3 review, finding 2: the two 404 retries are counted apart from other failures.
    /// Seq 4 answers 503, 503, then 404 for good: two transient retries, then the first 404 and
    /// its **two** retries — five requests — then the gap and seq 5. Fails with one counter
    /// serving both: the first 404 finds it at 2 and skips at once — `[3, 4, 4, 4, 5]`, three
    /// requests, and no retry for a CDN edge that was about to have the segment.
    #[test]
    fn t27_404_retries_are_not_used_up_by_earlier_transient_failures() {
        let (base, paths) = task_segment_server(|seq, n| match (seq, n) {
            (4, 1 | 2) => Some(503),
            (4, _) => Some(404),
            _ => None,
        });
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let segs = segs_until_seq5(&paths, GUARD);
        h.ctx.cancel();
        assert!(segs.contains(&5), "seq 5 was never requested: {segs:?}");
        assert_eq!(
            segs.iter().filter(|&&s| s == 4).count(),
            5,
            "503, 503, 404 + two 404 retries: {segs:?}"
        );
    }

    /// Finding 4: a 404 that does not heal is a gap after the two retries — three requests,
    /// then the next segment — never a whole-TD retry. Fails with the 404 on the whole-TD
    /// transient path: ten requests over the TD of 10 s (`[3, 4 ×10, 5]`).
    #[test]
    fn t25_a_404_that_stays_is_a_gap_after_two_retries() {
        let (base, paths) = task_segment_server(|seq, _| (seq == 4).then_some(404));
        let h = start_session(&format!("{base}/live/playlist.m3u8"));
        let segs = segs_until_seq5(&paths, GUARD);
        h.ctx.cancel();
        assert!(segs.contains(&5), "seq 5 was never requested: {segs:?}");
        assert_eq!(
            segs.iter().filter(|&&s| s == 4).count(),
            3,
            "404 ×3, then the gap: {segs:?}"
        );
    }
}

#[cfg(test)]
mod started_tests {
    //! The click rule's pure part (M3b commit 5): `Shared::write_state` sends `Started` on the
    //! first `Playing` of a session and never again until `begin_session`, and only for a
    //! write from the live session (`/code-review` finding 1, 2026-09-23). Each test names
    //! what makes it fail.
    use std::sync::mpsc::Receiver;

    use super::*;

    fn shared() -> (Shared, Receiver<EngineEvent>) {
        let (tx, rx) = mpsc::channel();
        let s = Shared {
            state: Arc::new(Mutex::new(PlaybackState::Idle)),
            events: tx,
            gains: EqGains::default(),
            paused: Arc::new(AtomicBool::new(false)),
            session: Arc::new(Mutex::new(Session::default())),
        };
        (s, rx)
    }

    /// A decode thread's handle on the session `generation`, as `Engine::play` builds it.
    fn ctx(s: &Shared, generation: u64) -> SessionCtx {
        SessionCtx {
            cancel: Arc::new(AtomicBool::new(false)),
            download: Arc::new(Mutex::new(None)),
            ring: Arc::new(Mutex::new(None)),
            backoff: Arc::new(Mutex::new(Backoff::default())),
            reconnect_count: Arc::new(AtomicU64::new(0)),
            generation,
            clock: Arc::new(BuildClock::new(BuildBounds::for_prefetch(
                stream::prefetch_bytes(None),
            ))),
            shared: s.clone(),
        }
    }

    fn drive(s: &Shared, states: &[PlaybackState]) {
        for st in states {
            s.set_state(st.clone());
        }
    }

    fn started_ids(rx: &Receiver<EngineEvent>) -> Vec<String> {
        let mut ids = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let EngineEvent::Started { station_id } = ev {
                ids.push(station_id);
            }
        }
        ids
    }

    use PlaybackState::{Buffering, Connecting, Paused, Playing, Reconnecting};

    /// A `StreamInfo` as the decode thread emits it, tagged by `station_name` for the reader.
    fn info(tag: &str) -> EngineEvent {
        EngineEvent::StreamInfo(StreamInfo {
            content_type: None,
            bitrate_kbps: None,
            station_name: Some(tag.into()),
            sample_rate: 48_000,
            channels: 2,
        })
    }

    /// Every `StreamInfo` and `Metadata` on the channel, as `info:<tag>` and `title:<title>`.
    fn session_events(rx: &Receiver<EngineEvent>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev {
                EngineEvent::StreamInfo(i) => {
                    out.push(format!("info:{}", i.station_name.unwrap_or_default()))
                }
                EngineEvent::Metadata(m) => {
                    out.push(format!("title:{}", m.title.unwrap_or_default()))
                }
                _ => {}
            }
        }
        out
    }

    /// Review 2, G2 (finding 2): a session event from a session that is over never reaches the
    /// channel — decided under the session lock, as a state write is. Deterministic: the
    /// interleaving the finding names (a cancel between F2's check and the emit) leaves the
    /// decode thread emitting with a generation that is no longer live, and that is what is
    /// driven here, for `StreamInfo` and for the ICY title callback. Session 1's events after
    /// session 2 began, and session 2's after its own cancel with no successor (a Stop), are
    /// dropped; session 2's while live land. Fails with the generation check removed from
    /// `emit_from` — `SessionCtx::emit` and `title_sink` as pass-throughs to an ungated
    /// `Shared::emit`: `["info:A", "title:A"]` on the channel. (F2's session test passes with
    /// its `cancelled()` check removed — the gate alone covers it.)
    #[test]
    fn g2_a_stale_session_emits_nothing() {
        let (s, rx) = shared();
        let a = ctx(&s, s.begin_session("u1".into()));
        let mut a_title = title_sink(&a);
        let b = ctx(&s, s.begin_session("u2".into()));
        let mut b_title = title_sink(&b);

        a.emit(info("A"));
        a_title("A".into());
        assert_eq!(
            session_events(&rx),
            Vec::<String>::new(),
            "session 1 is over"
        );

        b.emit(info("B"));
        b_title("B".into());
        assert_eq!(
            session_events(&rx),
            ["info:B", "title:B"],
            "session 2 is live"
        );

        b.cancel();
        b.emit(info("B2"));
        b_title("B2".into());
        assert_eq!(
            session_events(&rx),
            Vec::<String>::new(),
            "session 2 was stopped"
        );
    }

    /// Fails if the flag is missing (four `Started`), or if the decision looks at the previous
    /// state instead of the flag (a resume or a reconnect's `Playing` would count).
    #[test]
    fn started_fires_once_per_session_whatever_the_route_back_to_playing() {
        let (s, rx) = shared();
        s.begin_session("u1".into());
        drive(&s, &[Connecting, Buffering, Playing]);
        assert_eq!(started_ids(&rx), vec!["u1"]);
        drive(&s, &[Buffering, Playing]); // an underrun's refill
        drive(&s, &[Paused, Playing]); // a resume
        drive(&s, &[Reconnecting { attempt: 1 }, Buffering, Playing]); // a reconnect
        assert_eq!(started_ids(&rx), Vec::<String>::new());
    }

    /// Fails if `begin_session` skips the reset when the id is unchanged.
    #[test]
    fn a_second_play_for_the_same_station_starts_again() {
        let (s, rx) = shared();
        s.begin_session("u1".into());
        drive(&s, &[Connecting, Buffering, Playing, Paused]);
        assert_eq!(started_ids(&rx), vec!["u1"]);
        s.begin_session("u1".into());
        drive(&s, &[Connecting, Buffering, Playing]);
        assert_eq!(started_ids(&rx), vec!["u1"]);
    }

    /// Fails if `Reconnecting` anywhere in the history suppresses the click.
    #[test]
    fn a_session_that_reconnected_before_ever_playing_starts_on_its_first_playing() {
        let (s, rx) = shared();
        s.begin_session("u1".into());
        drive(
            &s,
            &[Connecting, Reconnecting { attempt: 1 }, Buffering, Playing],
        );
        assert_eq!(started_ids(&rx), vec!["u1"]);
    }

    /// Fails if `Paused → Playing` is excluded categorically, or if `Started` is sent before
    /// `State(Playing)`.
    #[test]
    fn paused_while_buffering_starts_on_resume() {
        let (s, rx) = shared();
        s.begin_session("u1".into());
        drive(&s, &[Connecting, Buffering, Paused, Playing]);
        let events: Vec<EngineEvent> = rx.try_iter().collect();
        let playing_at = events
            .iter()
            .position(|e| matches!(e, EngineEvent::State(Playing)))
            .expect("State(Playing)");
        let started_at = events
            .iter()
            .position(|e| matches!(e, EngineEvent::Started { .. }))
            .expect("one Started");
        assert!(started_at > playing_at, "Started after State(Playing)");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, EngineEvent::Started { .. }))
                .count(),
            1
        );
    }

    /// Fails if the setter's no-op return is bypassed for the flag check (a second `State`
    /// or `Started` for the same state).
    #[test]
    fn a_repeated_playing_is_a_no_op() {
        let (s, rx) = shared();
        s.begin_session("u1".into());
        drive(&s, &[Playing, Playing]);
        let events: Vec<EngineEvent> = rx.try_iter().collect();
        assert_eq!(events.len(), 2, "one State and one Started: {events:?}");
    }

    /// `/code-review` finding 1: the engine thread's `cancel` + `begin_session`
    /// can run between a decode thread's cancel check and its write. Modelled from the
    /// writer's side — its check passed, so the write reaches `Shared` — with the next session
    /// already begun. Fails if the write is gated on a flag read before the lock (the code
    /// before this test: the stale `Playing` lands, `Started { "u2" }` goes out for a station
    /// that has not opened, and the real first `Playing` then sends nothing), or if
    /// `begin_session` does not move the generation.
    #[test]
    fn a_stale_sessions_playing_cannot_take_the_new_sessions_started() {
        let (s, rx) = shared();
        let stale = ctx(&s, s.begin_session("u1".into()));
        stale.set_state(Connecting);
        let live = ctx(&s, s.begin_session("u2".into()));
        s.set_state(Connecting); // the engine thread, as `play` does
        stale.set_state(Playing); // the race: its cancel check passed before `begin_session`
        assert_eq!(s.state(), Connecting, "the stale write is dropped");
        assert_eq!(started_ids(&rx), Vec::<String>::new());
        live.set_state(Buffering);
        live.set_state(Playing);
        assert_eq!(started_ids(&rx), vec!["u2"]);
    }

    /// A session that ended with no successor (`stop`: cancel, then the engine's `Idle`): its
    /// late write is dropped too. Fails if only `begin_session` moves the generation.
    #[test]
    fn a_cancelled_sessions_write_is_dropped_before_any_successor() {
        let (s, rx) = shared();
        let old = ctx(&s, s.begin_session("u1".into()));
        old.set_state(Connecting);
        old.cancel();
        s.set_state(PlaybackState::Idle);
        old.set_state(Playing);
        assert_eq!(s.state(), PlaybackState::Idle, "the late write is dropped");
        assert_eq!(started_ids(&rx), Vec::<String>::new());
    }
}

#[cfg(test)]
mod tick_tests {
    use super::*;

    const CAP: usize = 1000;
    // Comfortably above the 75% resume threshold at CAP = 1000.
    const REFILLED: usize = 800;
    // Stand-in for watchdog_ticks(), which is 150 at the default retry_timeout.
    const WD: u32 = 150;

    /// Baseline inputs: only `capacity` set. Each test overrides the two or three fields it
    /// actually cares about, which is the point of the struct.
    fn ti() -> TickInputs {
        TickInputs {
            capacity: CAP,
            dwell: DWELL_TICKS,
            watchdog_ticks: WD,
            ..Default::default()
        }
    }

    #[test]
    fn no_new_underrun_is_a_no_op() {
        let out = decide_tick(&PlaybackState::Playing, ti());
        assert_eq!(
            out,
            TickOutcome {
                transition: None,
                reset_backoff: false
            }
        );
    }

    #[test]
    fn new_underrun_while_playing_pauses_and_buffers() {
        let out = decide_tick(
            &PlaybackState::Playing,
            TickInputs {
                new_underrun: true,
                fill: 100,
                ..ti()
            },
        );
        assert_eq!(out.transition, Some(Transition::PauseAndBuffer));
    }

    #[test]
    fn new_underrun_while_playing_pauses_even_if_ring_already_refilled() {
        // Replaces the old both_conditions_can_fire_in_the_same_tick, which asserted the
        // opposite. See decide_tick's doc comment for why that behaviour was wrong.
        let out = decide_tick(
            &PlaybackState::Playing,
            TickInputs {
                new_underrun: true,
                fill: CAP,
                ready_ticks: 100,
                ..ti()
            },
        );
        assert_eq!(out.transition, Some(Transition::PauseAndBuffer));
    }

    #[test]
    fn new_underrun_while_buffering_is_not_repeated() {
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                new_underrun: true,
                fill: 100,
                ..ti()
            },
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn buffering_refilled_and_dwelled_resumes_playing() {
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                fill: REFILLED,
                ready_ticks: DWELL_TICKS,
                ..ti()
            },
        );
        assert_eq!(out.transition, Some(Transition::ResumePlaying));
    }

    #[test]
    fn buffering_refilled_but_not_dwelled_enough_stays_buffering() {
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                fill: REFILLED,
                ready_ticks: DWELL_TICKS - 1,
                ..ti()
            },
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn buffering_refilled_and_dwelled_but_new_underrun_stays_buffering() {
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                new_underrun: true,
                fill: REFILLED,
                ready_ticks: DWELL_TICKS,
                ..ti()
            },
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn buffering_refilled_and_dwelled_resumes_paused_if_user_paused() {
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                fill: REFILLED,
                user_paused: true,
                ready_ticks: DWELL_TICKS,
                ..ti()
            },
        );
        assert_eq!(out.transition, Some(Transition::ResumePaused));
    }

    #[test]
    fn user_paused_while_playing_suppresses_pause_on_new_underrun() {
        let out = decide_tick(
            &PlaybackState::Playing,
            TickInputs {
                new_underrun: true,
                fill: 100,
                user_paused: true,
                ..ti()
            },
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn unstable_connection_requires_longer_dwell() {
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                fill: REFILLED,
                ready_ticks: DWELL_TICKS,
                dwell: DWELL_TICKS_UNSTABLE,
                ..ti()
            },
        );
        assert_eq!(out.transition, None);

        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                fill: REFILLED,
                ready_ticks: DWELL_TICKS_UNSTABLE,
                dwell: DWELL_TICKS_UNSTABLE,
                ..ti()
            },
        );
        assert_eq!(out.transition, Some(Transition::ResumePlaying));
    }

    #[test]
    fn stability_resets_backoff_regardless_of_transition() {
        let out = decide_tick(
            &PlaybackState::Playing,
            TickInputs {
                stable: true,
                ..ti()
            },
        );
        assert!(out.reset_backoff);
        assert_eq!(out.transition, None);

        let out = decide_tick(
            &PlaybackState::Playing,
            TickInputs {
                new_underrun: true,
                stable: true,
                ..ti()
            },
        );
        assert!(out.reset_backoff);
        assert_eq!(out.transition, Some(Transition::PauseAndBuffer));
    }

    #[test]
    fn select_dwell_at_panic_boundary() {
        assert_eq!(select_dwell(0), DWELL_TICKS);
        assert_eq!(select_dwell(UNDERRUN_PANIC_COUNT - 1), DWELL_TICKS);
        assert_eq!(select_dwell(UNDERRUN_PANIC_COUNT), DWELL_TICKS_UNSTABLE);
        assert_eq!(select_dwell(UNDERRUN_PANIC_COUNT + 1), DWELL_TICKS_UNSTABLE);
    }

    #[test]
    fn latched_dwell_survives_prune_mid_wait() {
        // The measured regression: a wait entered with 3 recent underruns selected 40 ticks,
        // then an underrun aged out of the window mid-wait, recent_underruns fell to 2, and
        // the dwell collapsed to 10 — resuming at ready_ticks 11 instead of 40.
        let mut latched = None;
        assert_eq!(
            dwell_for_tick(&mut latched, true, UNDERRUN_PANIC_COUNT),
            DWELL_TICKS_UNSTABLE
        );
        for _ in 0..50 {
            assert_eq!(
                dwell_for_tick(&mut latched, true, UNDERRUN_PANIC_COUNT - 1),
                DWELL_TICKS_UNSTABLE,
                "a prune mid-wait must not shorten the latched dwell"
            );
        }
    }

    #[test]
    fn latch_clears_on_leaving_buffering() {
        let mut latched = None;
        assert_eq!(
            dwell_for_tick(&mut latched, true, UNDERRUN_PANIC_COUNT),
            DWELL_TICKS_UNSTABLE
        );
        // Resumed: not buffering, so the latch drops.
        dwell_for_tick(&mut latched, false, UNDERRUN_PANIC_COUNT);
        assert_eq!(latched, None);
        // A later, calmer wait gets its own selection rather than inheriting the old one.
        assert_eq!(dwell_for_tick(&mut latched, true, 0), DWELL_TICKS);
    }

    #[test]
    fn no_progress_while_buffering_fails_session_at_threshold() {
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                ticks_since_progress: WD,
                ..ti()
            },
        );
        assert_eq!(out.transition, Some(Transition::FailSession));
    }

    #[test]
    fn no_progress_while_buffering_below_threshold_does_not_fail() {
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                ticks_since_progress: WD - 1,
                ..ti()
            },
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn paused_with_full_ring_never_fails_session() {
        // A paused session stops pulling, so the ring fills and the decode thread parks on a
        // full ring: `pushed` stops advancing on a perfectly healthy connection. A pause is
        // unbounded, so this must be an exemption, not a longer threshold.
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                user_paused: true,
                fill: CAP,
                ticks_since_progress: WD * 10,
                ..ti()
            },
        );
        assert_ne!(out.transition, Some(Transition::FailSession));
    }

    // ---- Defect B: the build bound ----

    // Review fixes F1: the rule's truth table, on durations as `Engine::tick` reads them from
    // the decode thread's stamps. Each boundary is a pair of rows; each row names the mutation
    // that fails it.

    const S: fn(u64) -> Duration = Duration::from_secs;
    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn bounds() -> BuildBounds {
        BuildBounds {
            format: S(20),
            starved: S(5),
            no_bytes: S(60),
        }
    }

    /// A build `since_start` in, with its first byte `since_first_byte` ago, the longest gap
    /// and the completed reconnects given.
    fn building(
        since_start: Duration,
        since_first_byte: Option<Duration>,
        longest_gap: Duration,
        reconnects: u64,
    ) -> TickInputs {
        TickInputs {
            build: Some(BuildInputs {
                since_start,
                since_first_byte,
                longest_gap,
                reconnects,
                bounds: bounds(),
            }),
            ..ti()
        }
    }

    fn decide(inputs: TickInputs) -> Option<Transition> {
        decide_tick(&PlaybackState::Connecting, inputs).transition
    }

    /// Before the first byte, only the no-bytes bound: at it → `NoBytes`; 1 ms before → none.
    /// Fails on `>` for `>=`, or on the bound counted from anything but the build's start.
    #[test]
    fn no_bytes_fires_at_its_bound_and_not_before() {
        assert_eq!(
            decide(building(S(60), None, S(0), 0)),
            Some(Transition::FailBuild(BuildCause::NoBytes))
        );
        assert_eq!(decide(building(S(60) - MS(1), None, S(0), 0)), None);
    }

    /// Before the first byte the format bound does not run, however long the build: the
    /// prefetch is not the format's time (finding 2). Fails if the format bound is read from
    /// the build's start.
    #[test]
    fn nothing_but_no_bytes_runs_before_the_first_byte() {
        assert_eq!(decide(building(S(59), None, S(0), 3)), None);
    }

    /// No-bytes is always the network, even with reconnects and no gap. Fails if the cause is
    /// shared with the format arm.
    #[test]
    fn no_bytes_is_never_the_format() {
        assert_eq!(
            decide(building(S(61), None, S(0), 0)),
            Some(Transition::FailBuild(BuildCause::NoBytes))
        );
    }

    /// From the first byte, the format bound: at it with bytes flowing → `Format`; 1 ms
    /// before → none. Fails on `>` for `>=`, or on the bound counted from the build's start
    /// (these rows start 30 s in: that reading fires in the second).
    #[test]
    fn format_fires_at_its_bound_from_the_first_byte_and_not_before() {
        assert_eq!(
            decide(building(S(50), Some(S(20)), MS(200), 0)),
            Some(Transition::FailBuild(BuildCause::Format))
        );
        assert_eq!(
            decide(building(S(50), Some(S(20) - MS(1)), MS(200), 0)),
            None
        );
    }

    /// At the format bound, a gap of `starved` → `Starved`; 1 ms below → `Format`. Fails on
    /// `>` for `>=`, or with the gap dropped from the rule.
    #[test]
    fn a_gap_of_starved_is_the_network() {
        assert_eq!(
            decide(building(S(20), Some(S(20)), S(5), 0)),
            Some(Transition::FailBuild(BuildCause::Starved))
        );
        assert_eq!(
            decide(building(S(20), Some(S(20)), S(5) - MS(1), 0)),
            Some(Transition::FailBuild(BuildCause::Format))
        );
    }

    /// A completed internal reconnect is the network with no gap (T-B1c's re-feed). Fails with
    /// the reconnect clause dropped.
    #[test]
    fn a_completed_reconnect_is_the_network() {
        assert_eq!(
            decide(building(S(20), Some(S(20)), S(0), 1)),
            Some(Transition::FailBuild(BuildCause::Starved))
        );
    }

    /// `Engine::tick` hands over the longest gap with the open one included, so a stall that
    /// is still open at the bound and one that resumed just before it read alike: here the
    /// rule sees the longest gap whatever its origin — `BuildClock::inputs` takes the max,
    /// pinned in `build::tests`. Fails with the gap clause dropped from the rule, or if the
    /// rule reads only a gap below `starved`.
    #[test]
    fn a_stall_that_resumed_before_the_bound_is_the_network() {
        assert_eq!(
            decide(building(S(22), Some(S(20)), S(10), 0)),
            Some(Transition::FailBuild(BuildCause::Starved))
        );
    }

    /// A pause during `Connecting` sets `Paused` while the decode thread is still building;
    /// the build is no less stuck. Fails with the arm placed after the `user_paused` arm, which
    /// returns first.
    #[test]
    fn build_bound_fires_while_user_paused() {
        let out = decide_tick(
            &PlaybackState::Paused,
            TickInputs {
                user_paused: true,
                ..building(S(20), Some(S(20)), S(0), 0)
            },
        );
        assert_eq!(
            out.transition,
            Some(Transition::FailBuild(BuildCause::Format))
        );
    }

    /// Only a build is bounded: with `build` `None` the ring's arms decide.
    #[test]
    fn build_bound_does_not_fire_without_a_build() {
        assert_eq!(decide(ti()), None);
    }

    /// The outcome the decode thread names: the format is terminal only before audio, the two
    /// network causes never; the messages carry the bound and the cause. Fails on a terminal
    /// network cause, or the format's message changed (acceptance X1 reads it on the page).
    #[test]
    fn the_bound_outcomes() {
        let build = |gap: Duration, reconnects: u64| BuildInputs {
            since_start: S(25),
            since_first_byte: Some(S(20)),
            longest_gap: gap,
            reconnects,
            bounds: bounds(),
        };
        let (code, message, terminal) = build_bound_cause(
            BuildCause::Format,
            &build(S(0), 0),
            Some("audio/aac"),
            false,
        );
        assert_eq!(
            (code, message.as_str(), terminal),
            (
                ErrorCode::UnsupportedFormat,
                "no decodable audio in the first 20 s of the stream (audio/aac)",
                true
            )
        );
        assert!(!build_bound_cause(BuildCause::Format, &build(S(0), 0), None, true).2);
        let (code, message, terminal) =
            build_bound_cause(BuildCause::Starved, &build(MS(5_400), 3), None, false);
        assert_eq!(
            (code, message.as_str(), terminal),
            (
                ErrorCode::Network,
                "no audio arrived while starting: the connection stalled (5.4 s without data; \
                 re-established 3 times)",
                false
            )
        );
        let (code, message, terminal) =
            build_bound_cause(BuildCause::NoBytes, &build(S(0), 0), None, false);
        assert_eq!(
            (code, message.as_str(), terminal),
            (
                ErrorCode::Network,
                "no audio arrived within 60 s of connecting",
                false
            )
        );
    }

    /// Review 2, finding 4: the bound's message is written from the gap and reconnect count the
    /// engine decided on, not from the clock read again after the cancel, whose open gap has
    /// grown. A clock whose open gap is ≥ 300 ms, a bound decided on 50 ms and 2 reconnects →
    /// the message's inputs carry 50 ms and 2. Fails with `decided_inputs` returning the clock's
    /// own inputs.
    #[test]
    fn the_bound_message_carries_the_decided_figures() {
        let clock = BuildClock::new(BuildBounds::for_prefetch(stream::PREFETCH_FLOOR_BYTES));
        let arrivals = Arc::new(Arrivals::new());
        crate::build::ArrivalWriter::new(arrivals.clone()).arrive(Duration::from_millis(1), 1);
        clock.begin_build(0, arrivals);
        std::thread::sleep(Duration::from_millis(300));
        assert!(clock.inputs(0).longest_gap >= Duration::from_millis(300));
        let bounded = Bounded {
            cause: BuildCause::Starved,
            longest_gap: Duration::from_millis(50),
            reconnects: 2,
        };
        let i = decided_inputs(&clock, &bounded, 0);
        assert_eq!(
            (i.longest_gap, i.reconnects),
            (Duration::from_millis(50), 2)
        );
    }

    #[test]
    fn paused_and_buffering_still_resumes_paused() {
        // The exemption must not cost the paused-and-buffering session its resume path.
        let out = decide_tick(
            &PlaybackState::Buffering,
            TickInputs {
                user_paused: true,
                fill: REFILLED,
                ready_ticks: DWELL_TICKS,
                ticks_since_progress: WD * 10,
                ..ti()
            },
        );
        assert_eq!(out.transition, Some(Transition::ResumePaused));
    }

    #[test]
    fn watchdog_does_not_fire_while_playing() {
        // Only Buffering is watched. Playing with no progress underruns into Buffering first,
        // so watching one state is sufficient.
        let out = decide_tick(
            &PlaybackState::Playing,
            TickInputs {
                ticks_since_progress: WD * 10,
                ..ti()
            },
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn progress_resets_the_watchdog() {
        let (mut last, mut ticks) = (0u64, 0u32);
        for _ in 0..5 {
            advance_progress(&mut last, &mut ticks, 1024);
        }
        assert_eq!(
            ticks, 4,
            "first call observes the change, the rest are stalled ticks"
        );
        advance_progress(&mut last, &mut ticks, 2048);
        assert_eq!(ticks, 0, "a push must reset the stall counter");
        advance_progress(&mut last, &mut ticks, 2048);
        assert_eq!(ticks, 1);
    }
}
