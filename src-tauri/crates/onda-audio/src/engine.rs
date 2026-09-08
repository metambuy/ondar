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

use std::io::{Read, Seek};
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::types::{EngineEvent, ErrorCode, IcyMetadata, PlaybackState, StreamInfo};

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
        let Some(stats) = session.ring_stats() else {
            return;
        };

        let starved = stats.starved.load(Ordering::Relaxed);
        let fill = stats.fill.load(Ordering::Relaxed);
        let user_paused = self.shared.paused.load(Ordering::Relaxed);
        let stable = self
            .playing_since
            .is_some_and(|t| t.elapsed() >= STABLE_AFTER);

        for action in decide_tick(starved, fill, stats.capacity, &current, user_paused, stable) {
            match action {
                TickAction::PauseAndBuffer => {
                    log::debug!("underrun: pausing output until the ring refills");
                    if let Some(p) = &self.player {
                        p.pause();
                    }
                    session.set_state(PlaybackState::Buffering);
                }
                TickAction::ResumePlaying => {
                    stats.starved.store(false, Ordering::Relaxed);
                    if let Some(p) = &self.player {
                        p.play();
                    }
                    session.set_state(PlaybackState::Playing);
                }
                TickAction::ResumePaused => {
                    stats.starved.store(false, Ordering::Relaxed);
                    session.set_state(PlaybackState::Paused);
                }
                TickAction::ResetBackoff => {
                    session.backoff.lock().unwrap().reset();
                }
            }
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
        let opened = match rt.block_on(stream::open(&client, url.clone())) {
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
                ring.stats.starved.store(false, Ordering::Relaxed);
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

/// What `Engine::tick()` should do, decided in isolation from the real `Player`/`SessionCtx`
/// so this logic is unit-testable without a live audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TickAction {
    PauseAndBuffer,
    ResumePlaying,
    ResumePaused,
    ResetBackoff,
}

/// Pure decision function behind [`Engine::tick`]. `state` is the state observed at the start
/// of the tick (before any of the returned actions are applied).
fn decide_tick(
    starved: bool,
    fill: usize,
    capacity: usize,
    state: &PlaybackState,
    user_paused: bool,
    stable: bool,
) -> Vec<TickAction> {
    let mut actions = Vec::new();
    if starved {
        if *state != PlaybackState::Buffering {
            actions.push(TickAction::PauseAndBuffer);
        }
        if fill >= capacity / 2 {
            actions.push(if user_paused {
                TickAction::ResumePaused
            } else {
                TickAction::ResumePlaying
            });
        }
    }
    if stable {
        actions.push(TickAction::ResetBackoff);
    }
    actions
}

#[cfg(test)]
mod tick_tests {
    use super::*;

    const CAP: usize = 1000;

    #[test]
    fn not_starved_is_a_no_op() {
        assert_eq!(
            decide_tick(false, 0, CAP, &PlaybackState::Playing, false, false),
            vec![]
        );
    }

    #[test]
    fn starved_while_playing_pauses_and_buffers() {
        assert_eq!(
            decide_tick(true, 100, CAP, &PlaybackState::Playing, false, false),
            vec![TickAction::PauseAndBuffer]
        );
    }

    #[test]
    fn starved_already_buffering_does_not_repeat_pause() {
        assert_eq!(
            decide_tick(true, 100, CAP, &PlaybackState::Buffering, false, false),
            vec![]
        );
    }

    #[test]
    fn starved_with_enough_fill_resumes_playing() {
        assert_eq!(
            decide_tick(true, 600, CAP, &PlaybackState::Buffering, false, false),
            vec![TickAction::ResumePlaying]
        );
    }

    #[test]
    fn starved_with_enough_fill_resumes_paused_if_user_paused() {
        assert_eq!(
            decide_tick(true, 600, CAP, &PlaybackState::Buffering, true, false),
            vec![TickAction::ResumePaused]
        );
    }

    #[test]
    fn both_conditions_can_fire_in_the_same_tick() {
        // Starvation just detected (state hasn't caught up to Buffering yet) but the ring has
        // already refilled past target by the time this tick runs.
        assert_eq!(
            decide_tick(true, 600, CAP, &PlaybackState::Playing, false, false),
            vec![TickAction::PauseAndBuffer, TickAction::ResumePlaying]
        );
    }

    #[test]
    fn stability_resets_backoff_independent_of_starvation() {
        assert_eq!(
            decide_tick(false, 0, CAP, &PlaybackState::Playing, false, true),
            vec![TickAction::ResetBackoff]
        );
    }
}
