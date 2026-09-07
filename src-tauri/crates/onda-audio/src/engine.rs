//! The audio engine.
//!
//! Threads:
//!
//! * **engine thread** (`onda-audio`): owns the output device (`MixerDeviceSink`), the
//!   `Player`, and the Tokio runtime that `stream-download` needs. It receives
//!   [`AudioCommand`]s over a channel and never blocks on network or decoding.
//! * **one decode thread per session** (`onda-decode`): opens the HTTP stream, probes it with
//!   Symphonia, decodes into the ring buffer, manages buffering/reconnect state, and exits when
//!   its session is cancelled.
//! * **audio callback** (cpal, owned by rodio): pulls from `Equalizer<RingSource>`. Never
//!   blocks, never allocates.
//!
//! The UI only ever sees [`EngineEvent`]s and the [`PlaybackState`] snapshot.

use std::io::{Read, Seek};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
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
use crate::ring;
use crate::stream;
use crate::types::{EngineEvent, ErrorCode, IcyMetadata, PlaybackState, StreamInfo};

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

/// Per-session cancellation. Cloned into the decode thread.
#[derive(Clone)]
struct SessionCtx {
    cancel: Arc<AtomicBool>,
    /// The download task's token, set by the decode thread once a stream is open, so `Stop`
    /// can unblock a read that is waiting on the network.
    download: Arc<Mutex<Option<CancellationToken>>>,
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
}

struct Engine {
    shared: Shared,
    client: reqwest::Client,
    rt: tokio::runtime::Runtime,
    sink: Option<MixerDeviceSink>,
    player: Option<Arc<Player>>,
    session: Option<SessionCtx>,
    volume: f32,
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
        }
    }

    fn run(mut self, rx: Receiver<AudioCommand>) {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                AudioCommand::Play { url, station_id } => self.play(url, station_id),
                AudioCommand::Pause => self.pause(),
                AudioCommand::Resume => self.resume(),
                AudioCommand::Stop => self.stop(),
                AudioCommand::SetVolume(v) => self.set_volume(v),
            }
        }
        // Handle dropped: shut down cleanly.
        self.stop();
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
                // The decode thread will downgrade this to Buffering if the ring is starved.
                self.shared.set_state(PlaybackState::Playing);
            }
            // If we are Buffering, the decode thread resumes output once the ring refills.
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
    let mut backoff = Backoff::default();

    loop {
        if ctx.cancelled() {
            return;
        }

        // 1. Connect.
        let opened = match rt.block_on(stream::open(&client, url.clone())) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("connect failed: {}", e.message);
                if !retry_or_fail(&ctx, &mut backoff, e.code, e.message) {
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
                if !retry_or_fail(&ctx, &mut backoff, ErrorCode::Decode, e.to_string()) {
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
        //    From then on: an underrun pauses the player and reports `Buffering`; refilling to
        //    the target resumes it. The user's own pause is tracked separately (`paused`).
        let (mut ring, source) = ring::ring(sample_rate, channels);
        let mut source = Some(source);
        let fill_target = ring.capacity / 2;
        let mut playing_since: Option<Instant> = None;
        let mut samples_since_check: u32 = 0;

        loop {
            if ctx.cancelled() {
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

            let filled = ring.capacity - ring.producer.slots();
            let paused = ctx.shared.paused.load(Ordering::Relaxed);

            if let Some(src) = source.take_if(|_| filled >= fill_target) {
                player.clear();
                player.append(Equalizer::new(src, ctx.shared.gains.clone()));
                ring.starved.store(false, Ordering::Relaxed);
                if paused {
                    ctx.set_state(PlaybackState::Paused);
                } else {
                    player.play();
                    ctx.set_state(PlaybackState::Playing);
                    playing_since = Some(Instant::now());
                }
                continue;
            }
            if source.is_some() {
                continue; // still pre-filling
            }

            if ring.starved.load(Ordering::Relaxed) {
                if filled >= fill_target {
                    ring.starved.store(false, Ordering::Relaxed);
                    if paused {
                        ctx.set_state(PlaybackState::Paused);
                    } else {
                        player.play();
                        ctx.set_state(PlaybackState::Playing);
                        playing_since = Some(Instant::now());
                    }
                } else if ctx.shared.state() != PlaybackState::Buffering {
                    log::debug!("underrun: pausing output until the ring refills");
                    player.pause();
                    ctx.set_state(PlaybackState::Buffering);
                    playing_since = None;
                }
            }

            if playing_since.is_some_and(|t| t.elapsed() >= STABLE_AFTER) && backoff.attempt() > 0 {
                backoff.reset();
            }
        }

        // Dropping the producer lets the RingSource drain and end, which empties the Player
        // queue; the reconnect path attaches a fresh source.
        drop(ring);
        if ctx.cancelled() {
            return;
        }
        log::warn!("stream ended or read failed; reconnecting");
        if !retry_or_fail(
            &ctx,
            &mut backoff,
            ErrorCode::Network,
            "the stream ended unexpectedly".to_string(),
        ) {
            return;
        }
    }
}

fn retry_or_fail(
    ctx: &SessionCtx,
    backoff: &mut Backoff,
    code: ErrorCode,
    message: String,
) -> bool {
    match backoff.next() {
        Some((attempt, delay)) => {
            ctx.set_state(PlaybackState::Reconnecting { attempt });
            ctx.sleep_cancellable(delay);
            !ctx.cancelled()
        }
        None => {
            ctx.set_state(PlaybackState::Error {
                code,
                message: format!("{message} (gave up after {} attempts)", backoff.attempt()),
            });
            false
        }
    }
}
