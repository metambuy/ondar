//! The audio engine.
//!
//! Threads:
//!
//! * **engine thread** (`onda-audio`): owns the output device (`MixerDeviceSink`), the
//!   `Player`, and the Tokio runtime that `stream-download` needs. It receives
//!   [`AudioCommand`]s over a channel, polling on a short timeout so it can also supervise
//!   ring buffering (see [`Engine::tick`]) — it never blocks on network or decoding itself.
//! * **one decode thread per session** (`onda-decode`): opens the HTTP stream, probes it with
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
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player, Source};
use rtrb::PushError;
use tokio_util::sync::CancellationToken;

use crate::eq::{EqGains, Equalizer};
use crate::icy::IcyReader;
use crate::reconnect::{Backoff, STABLE_AFTER};
use crate::ring::{self, RingStats};
use crate::stream;
use crate::types::{EngineEvent, ErrorCode, IcyMetadata, PlaybackState, ReconnectInfo, StreamInfo};

/// How often the engine thread wakes up (absent a command) to run [`Engine::tick`].
const TICK_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub enum AudioCommand {
    Play { url: String, station_id: String },
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
    /// Spawn the engine thread. `user_agent` goes on every HTTP request (`Onda/<version>`).
    pub fn start(user_agent: String) -> (AudioEngine, Receiver<EngineEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let shared = Shared {
            state: Arc::new(Mutex::new(PlaybackState::Idle)),
            events: ev_tx,
            gains: EqGains::default(),
            paused: Arc::new(AtomicBool::new(false)),
        };
        let engine_shared = shared.clone();
        thread::Builder::new()
            .name("onda-audio".into())
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
}

impl Shared {
    fn set_state(&self, s: PlaybackState) {
        {
            let mut guard = self.state.lock().unwrap();
            if *guard == s {
                return;
            }
            *guard = s.clone();
        }
        let _ = self.events.send(EngineEvent::State(s));
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
    }

    /// State updates from a cancelled session are dropped so a stale decode thread cannot
    /// overwrite the state of its successor.
    fn set_state(&self, s: PlaybackState) {
        if !self.cancelled() {
            self.shared.set_state(s);
        }
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

struct Engine {
    shared: Shared,
    client: reqwest::Client,
    rt: tokio::runtime::Runtime,
    sink: Option<MixerDeviceSink>,
    player: Option<Arc<Player>>,
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
            .thread_name("onda-net")
            .enable_all()
            .build()
            .expect("tokio runtime");
        Self {
            shared,
            client: stream::build_client(&user_agent),
            rt,
            sink: None,
            player: None,
            session: None,
            volume: 1.0,
            playing_since: None,
            last_state: PlaybackState::Idle,
            current_ring: None,
            last_underruns: 0,
            ready_ticks: 0,
            tick_index: 0,
            underrun_ticks: VecDeque::new(),
            current_reconnect_counter: None,
            last_reconnect_count: 0,
        }
    }

    fn run(mut self, rx: Receiver<AudioCommand>) {
        loop {
            match rx.recv_timeout(TICK_INTERVAL) {
                Ok(AudioCommand::Play { url, station_id }) => self.play(url, station_id),
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

        let outcome = decide_tick(
            new_underrun,
            fill,
            stats.capacity,
            &current,
            user_paused,
            stable,
            self.ready_ticks,
            recent_underruns,
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
            None => {}
        }
        if outcome.reset_backoff {
            session.backoff.lock().unwrap().reset();
        }
    }

    /// Open the output device on first use so a missing device is reported as a playback
    /// error rather than a crash at startup.
    fn ensure_player(&mut self) -> Result<Arc<Player>, String> {
        if let Some(p) = &self.player {
            return Ok(p.clone());
        }
        let mut sink = DeviceSinkBuilder::open_default_sink().map_err(|e| e.to_string())?;
        sink.log_on_drop(false);
        let player = Arc::new(Player::connect_new(sink.mixer()));
        player.set_volume(self.volume);
        self.sink = Some(sink);
        self.player = Some(player.clone());
        Ok(player)
    }

    fn play(&mut self, url: String, station_id: String) {
        self.cancel_session();
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
        let player = match self.ensure_player() {
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
            shared: self.shared.clone(),
        };
        self.session = Some(ctx.clone());
        self.shared.set_state(PlaybackState::Connecting);

        let client = self.client.clone();
        let handle = self.rt.handle().clone();
        thread::Builder::new()
            .name(format!("onda-decode:{station_id}"))
            .spawn(move || run_session(ctx, url, client, handle, player))
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
) {
    loop {
        if ctx.cancelled() {
            return;
        }

        // 1. Connect.
        let opened = match rt.block_on(stream::open(
            &client,
            url.clone(),
            ctx.reconnect_count.clone(),
        )) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("connect failed: {}", e.message);
                if !retry_or_fail(&ctx, e.code, e.message) {
                    return;
                }
                continue;
            }
        };
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
                if !retry_or_fail(&ctx, ErrorCode::Decode, e.to_string()) {
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

            if let Some(src) = source.take_if(|_| filled >= fill_target) {
                player.clear();
                player.append(Equalizer::new(src, ctx.shared.gains.clone()));
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
        ) {
            return;
        }
    }
}

fn retry_or_fail(ctx: &SessionCtx, code: ErrorCode, message: String) -> bool {
    let mut backoff = ctx.backoff.lock().unwrap();
    match backoff.next() {
        Some((attempt, delay)) => {
            drop(backoff);
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

/// What `Engine::tick()` should do, decided in isolation from the real `Player`/`SessionCtx`
/// so this logic is unit-testable without a live audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transition {
    PauseAndBuffer,
    ResumePlaying,
    ResumePaused,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TickOutcome {
    transition: Option<Transition>,
    reset_backoff: bool,
}

/// Pure decision function behind [`Engine::tick`]. `state` is the state observed at the start
/// of the tick (before the returned outcome is applied). `new_underrun` is whether the ring's
/// underrun counter advanced since the previous tick (not raw "is silent right now" — see
/// `RingStats::underruns`); `ready_ticks`/`recent_underruns` are the caller's running counts
/// (consecutive good ticks, and underrun events within `UNDERRUN_WINDOW_TICKS`).
///
/// Pause and resume can never be emitted in the same tick: once a fresh underrun is seen while
/// not already `Buffering`, this returns immediately with `PauseAndBuffer`, even if the ring
/// looks fully refilled by the time this tick runs. That refilled-on-arrival case is real, not
/// hypothetical — an Icecast burst-on-connect (default burst-size 64 KB, ~4 s at 128 kbps) can
/// fill the entire 2 s ring within one 100 ms tick, and `stream-download`'s own
/// `retry_timeout` reconnect (default 5 s idle) fires on every stall — so without the
/// early-return, a normal internal reconnect would flash `Buffering` then `Playing` back to
/// back on every stall.
#[allow(clippy::too_many_arguments)]
fn decide_tick(
    new_underrun: bool,
    fill: usize,
    capacity: usize,
    state: &PlaybackState,
    user_paused: bool,
    stable: bool,
    ready_ticks: u32,
    recent_underruns: u32,
) -> TickOutcome {
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

    let dwell = if recent_underruns >= UNDERRUN_PANIC_COUNT {
        DWELL_TICKS_UNSTABLE
    } else {
        DWELL_TICKS
    };
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
mod tick_tests {
    use super::*;

    const CAP: usize = 1000;
    // Comfortably above the 75% resume threshold at CAP = 1000.
    const REFILLED: usize = 800;

    #[test]
    fn no_new_underrun_is_a_no_op() {
        let out = decide_tick(false, 0, CAP, &PlaybackState::Playing, false, false, 0, 0);
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
        let out = decide_tick(true, 100, CAP, &PlaybackState::Playing, false, false, 0, 0);
        assert_eq!(out.transition, Some(Transition::PauseAndBuffer));
    }

    #[test]
    fn new_underrun_while_playing_pauses_even_if_ring_already_refilled() {
        // Replaces the old both_conditions_can_fire_in_the_same_tick, which asserted the
        // opposite. See decide_tick's doc comment for why that behaviour was wrong.
        let out = decide_tick(
            true,
            CAP,
            CAP,
            &PlaybackState::Playing,
            false,
            false,
            100,
            0,
        );
        assert_eq!(out.transition, Some(Transition::PauseAndBuffer));
    }

    #[test]
    fn new_underrun_while_buffering_is_not_repeated() {
        let out = decide_tick(
            true,
            100,
            CAP,
            &PlaybackState::Buffering,
            false,
            false,
            0,
            0,
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn buffering_refilled_and_dwelled_resumes_playing() {
        let out = decide_tick(
            false,
            REFILLED,
            CAP,
            &PlaybackState::Buffering,
            false,
            false,
            DWELL_TICKS,
            0,
        );
        assert_eq!(out.transition, Some(Transition::ResumePlaying));
    }

    #[test]
    fn buffering_refilled_but_not_dwelled_enough_stays_buffering() {
        let out = decide_tick(
            false,
            REFILLED,
            CAP,
            &PlaybackState::Buffering,
            false,
            false,
            DWELL_TICKS - 1,
            0,
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn buffering_refilled_and_dwelled_but_new_underrun_stays_buffering() {
        let out = decide_tick(
            true,
            REFILLED,
            CAP,
            &PlaybackState::Buffering,
            false,
            false,
            DWELL_TICKS,
            0,
        );
        assert_eq!(out.transition, None);
    }

    #[test]
    fn buffering_refilled_and_dwelled_resumes_paused_if_user_paused() {
        let out = decide_tick(
            false,
            REFILLED,
            CAP,
            &PlaybackState::Buffering,
            true,
            false,
            DWELL_TICKS,
            0,
        );
        assert_eq!(out.transition, Some(Transition::ResumePaused));
    }

    #[test]
    fn user_paused_while_playing_suppresses_pause_on_new_underrun() {
        let out = decide_tick(true, 100, CAP, &PlaybackState::Playing, true, false, 0, 0);
        assert_eq!(out.transition, None);
    }

    #[test]
    fn unstable_connection_requires_longer_dwell() {
        let out = decide_tick(
            false,
            REFILLED,
            CAP,
            &PlaybackState::Buffering,
            false,
            false,
            DWELL_TICKS,
            UNDERRUN_PANIC_COUNT,
        );
        assert_eq!(out.transition, None);

        let out = decide_tick(
            false,
            REFILLED,
            CAP,
            &PlaybackState::Buffering,
            false,
            false,
            DWELL_TICKS_UNSTABLE,
            UNDERRUN_PANIC_COUNT,
        );
        assert_eq!(out.transition, Some(Transition::ResumePlaying));
    }

    #[test]
    fn stability_resets_backoff_regardless_of_transition() {
        let out = decide_tick(false, 0, CAP, &PlaybackState::Playing, false, true, 0, 0);
        assert!(out.reset_backoff);
        assert_eq!(out.transition, None);

        let out = decide_tick(true, 0, CAP, &PlaybackState::Playing, false, true, 0, 0);
        assert!(out.reset_backoff);
        assert_eq!(out.transition, Some(Transition::PauseAndBuffer));
    }
}
