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

    fn emit(&self, ev: EngineEvent) {
        let _ = self.events.send(ev);
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
            session.shared.emit(EngineEvent::Reconnect(ReconnectInfo {
                count: reconnect_count,
            }));
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

        let ctx = SessionCtx {
            cancel: Arc::new(AtomicBool::new(false)),
            download: Arc::new(Mutex::new(None)),
            ring: Arc::new(Mutex::new(None)),
            backoff: Arc::new(Mutex::new(Backoff::default())),
            reconnect_count: Arc::new(AtomicU64::new(0)),
            generation,
            shared: self.shared.clone(),
        };
        self.session = Some(ctx.clone());
        self.shared.set_state(PlaybackState::Connecting);

        let client = self.client.clone();
        let handle = self.rt.handle().clone();
        let prefetch = stream::prefetch_bytes(bitrate_kbps);
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
    loop {
        if ctx.cancelled() {
            return;
        }

        // 1. Connect.
        let opened = match rt.block_on(stream::open(
            &client,
            url.clone(),
            ctx.reconnect_count.clone(),
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

        // 2. Probe / build the decoder.
        let title_shared = ctx.shared.clone();
        let reader: BoxedReader = Box::new(IcyReader::new(
            opened.reader,
            opened.metaint,
            Box::new(move |title| {
                title_shared.emit(EngineEvent::Metadata(IcyMetadata { title: Some(title) }));
            }),
        ));
        let mut builder = DecoderBuilder::new()
            .with_data(reader)
            .with_seekable(false)
            .with_gapless(false);
        if let Some(ct) = &opened.content_type {
            builder = builder.with_mime_type(ct);
        }
        let mut decoder = match builder.build() {
            Ok(d) => d,
            Err(DecoderError::UnrecognizedFormat) => {
                ctx.set_state(PlaybackState::Error {
                    code: ErrorCode::UnsupportedFormat,
                    message: format!(
                        "could not identify the audio format ({})",
                        opened.content_type.as_deref().unwrap_or("no content-type")
                    ),
                });
                return;
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
        ctx.shared.emit(EngineEvent::StreamInfo(StreamInfo {
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

/// A server's `Retry-After` lengthens the backoff's delay up to this; past it the session
/// would look dead to the user, and the backoff's own 16 s is already the longest wait shown.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);

/// The retry policy, by cause. A **terminal** failure (`StreamError::terminal` — a non-HTTP
/// answer such as `ICY 200 OK`, or 401/403/404/410 — and only while the session has never
/// opened, see `run_session`) fails the session on the spot: retrying cannot change what the
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

// Resume/dwell/instability thresholds below are placeholders pending measurement against
// `scripts/stall-server.py`. Do not treat these numbers as tuned.

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
    } = inputs;
    let mut out = TickOutcome {
        transition: None,
        reset_backoff: stable,
    };

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
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                let n = seen.fetch_add(1, Ordering::SeqCst);
                let response = &responses[n.min(responses.len() - 1)];
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(response);
                let _ = sock.flush();
                thread::sleep(Duration::from_millis(200));
            }
        });
        (format!("http://{addr}/stream"), count)
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
        /// Every `Started` id seen while draining, in order.
        started: Mutex<Vec<String>>,
        // Dropping the runtime while `run_session` still holds its handle would abort the
        // open; kept for the harness's lifetime.
        _rt: tokio::runtime::Runtime,
    }

    /// Start `run_session` against `url` on its own thread, as `Engine::play` does, minus the
    /// device: the `Player` is connected to a bare mixer whose output a thread pulls at about
    /// twice real time — the device's stand-in. Without that pull nothing ever drops a queued
    /// source, and `Player::clear()` on a second open (a reconnect after playback) waits for
    /// the mixer forever (found by `started_is_sent_once_per_session_across_a_reconnect`,
    /// M3b commit 5: the second stream reached its fill target and never left `clear()`).
    fn start_session(url: &str) -> Harness {
        let (ev_tx, ev_rx) = mpsc::channel();
        let shared = Shared {
            state: Arc::new(Mutex::new(PlaybackState::Connecting)),
            events: ev_tx,
            gains: EqGains::default(),
            paused: Arc::new(AtomicBool::new(false)),
            session: Arc::new(Mutex::new(Session::default())),
        };
        let generation = shared.begin_session("u1".into());
        let ctx = SessionCtx {
            cancel: Arc::new(AtomicBool::new(false)),
            download: Arc::new(Mutex::new(None)),
            ring: Arc::new(Mutex::new(None)),
            backoff: Arc::new(Mutex::new(Backoff::default())),
            reconnect_count: Arc::new(AtomicU64::new(0)),
            generation,
            shared,
        };
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
        let prefetch = stream::prefetch_bytes(None);
        let output = OutputFormat {
            channels: std::num::NonZero::new(2).expect("2"),
            sample_rate: std::num::NonZero::new(44_100).expect("44100"),
        };
        thread::spawn(move || run_session(session, url, client, handle, player, prefetch, output));
        Harness {
            ctx,
            events: ev_rx,
            started: Mutex::new(Vec::new()),
            _rt: rt,
        }
    }

    /// Wait up to `within` for the shared state to satisfy `done`; returns every state seen.
    fn states_until(
        h: &Harness,
        within: Duration,
        done: impl Fn(&PlaybackState) -> bool,
    ) -> Vec<PlaybackState> {
        let deadline = Instant::now() + within;
        let mut seen = Vec::new();
        let drain = |seen: &mut Vec<PlaybackState>| {
            while let Ok(ev) = h.events.try_recv() {
                match ev {
                    EngineEvent::State(s) => seen.push(s),
                    EngineEvent::Started { station_id } => {
                        h.started.lock().unwrap().push(station_id)
                    }
                    _ => {}
                }
            }
        };
        loop {
            drain(&mut seen);
            if done(&h.ctx.shared.state()) || Instant::now() >= deadline {
                // The state is set before its event is sent; pick up the one for it.
                thread::sleep(Duration::from_millis(20));
                drain(&mut seen);
                return seen;
            }
            thread::sleep(Duration::from_millis(20));
        }
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

    /// Fails on a cause-blind policy: the state would be `Reconnecting { 1 }` inside the 3 s
    /// window (the terminal `Error` only arrives after 31 s) and the server would see a
    /// second request at ~1.2 s.
    #[test]
    fn an_icy_server_fails_the_session_on_the_first_attempt() {
        let (url, requests) = counting_server(
            b"ICY 200 OK\r\nicy-name: synthetic shoutcast v1\r\ncontent-type: audio/mpeg\r\n\r\n\
              0123456789abcdef0123456789abcdef",
        );
        let h = start_session(&url);
        let seen = states_until(&h, Duration::from_secs(3), is_error);
        let state = h.ctx.shared.state();
        assert!(
            matches!(
                &state,
                PlaybackState::Error {
                    code: ErrorCode::Http,
                    ..
                }
            ),
            "expected Error {{ Http }} within 3 s, got {state:?}"
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

    /// Same shape for a 404 on the first open: the resource is not there for us.
    #[test]
    fn a_404_fails_the_session_on_the_first_attempt() {
        let (url, requests) = counting_server(
            b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        let h = start_session(&url);
        let seen = states_until(&h, Duration::from_secs(3), is_error);
        let state = h.ctx.shared.state();
        assert!(
            matches!(
                &state,
                PlaybackState::Error {
                    code: ErrorCode::Http,
                    ..
                }
            ),
            "expected Error {{ Http }} within 3 s, got {state:?}"
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

    /// A 5xx keeps the backoff. Fails if 5xx were made terminal (one request, an `Error`
    /// state inside the window) or if the backoff's first delay were not ~1 s.
    #[test]
    fn a_503_is_retried_through_the_backoff() {
        let (url, requests) = counting_server(
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        let h = start_session(&url);
        let seen = states_until(&h, Duration::from_secs(3), |_| {
            requests.load(Ordering::SeqCst) >= 2
        });
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "a second request within 3 s (saw {})",
            requests.load(Ordering::SeqCst)
        );
        assert_eq!(
            reconnecting(&seen).first(),
            Some(&1),
            "Reconnecting {{ 1 }} was emitted"
        );
        assert!(!is_error(&h.ctx.shared.state()), "not failed after one 5xx");
        h.ctx.cancel();
    }

    /// A 429 on the first open keeps the backoff, and `Retry-After` stretches its delay.
    /// Fails on the "every 4xx is terminal" rule (`Error { Http }` at once, one request) and
    /// if the header is ignored (the backoff alone sends the second request at ~1.1 s; the
    /// assertion at 2 s would see two).
    #[test]
    fn a_429_is_retried_after_its_retry_after() {
        let (url, requests) = counting_server(
            b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 3\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        let h = start_session(&url);
        let seen = states_until(&h, Duration::from_secs(2), |_| {
            requests.load(Ordering::SeqCst) >= 2
        });
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "no second request inside the header's 3 s"
        );
        assert_eq!(
            reconnecting(&seen).first(),
            Some(&1),
            "Reconnecting {{ 1 }} was emitted"
        );
        assert!(!is_error(&h.ctx.shared.state()), "not failed on a 429");
        let _ = states_until(&h, Duration::from_secs(4), |_| {
            requests.load(Ordering::SeqCst) >= 2
        });
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "a second request once Retry-After elapsed"
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
        let seen = states_until(&h, Duration::from_secs(8), |s| {
            matches!(s, PlaybackState::Reconnecting { attempt: 2 }) || is_error(s)
        });
        let state = h.ctx.shared.state();
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
        let seen = states_until(&h, Duration::from_secs(10), |s| {
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
    /// thread. Returns `frames` frames of the mixer's output from the second session's
    /// `Playing` on. The pull runs at about twice real time, like `start_session`'s.
    fn two_sessions(
        first: Vec<u8>,
        second: Vec<u8>,
        mixer_rate: u32,
        mixer_ch: u16,
        frames: usize,
    ) -> Vec<f32> {
        let (ev_tx, _ev_rx) = mpsc::channel();
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
                let pulled: Vec<f32> = out.by_ref().take(chunk).collect();
                if let Some(v) = cap.lock().unwrap().as_mut() {
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
        let wait_playing = |what: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while shared.state() != PlaybackState::Playing {
                assert!(Instant::now() < deadline, "{what} never reached Playing");
                thread::sleep(Duration::from_millis(10));
            }
        };

        let one = start(first, "first");
        wait_playing("the first session");
        thread::sleep(Duration::from_millis(300));
        one.cancel();
        let _two = start(second, "second");
        wait_playing("the second session");
        *capture.lock().unwrap() = Some(Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let got = capture.lock().unwrap().as_ref().map_or(0, Vec::len);
            if got >= frames * mixer_ch as usize {
                break;
            }
            assert!(Instant::now() < deadline, "captured only {got} samples");
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

    /// `/code-review` finding 1 (2026-09-23): the engine thread's `cancel` + `begin_session`
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
