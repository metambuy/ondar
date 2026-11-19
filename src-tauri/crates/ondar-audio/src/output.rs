//! The output owner's seam and its machinery (0.1.2 F1; beta item 1, merge A). The engine opens
//! its output through an [`OutputOpener`], so the session tests can stand a bare
//! `rodio::mixer::mixer` and a puller thread in for the device and script devices appearing and
//! leaving; production opens a device **pinned by UID** and runs the default-output watcher.
//!
//! What the engine keeps of an open is the [`Mixer`] (each attach connects its own `Player` to
//! it, `engine.rs::run_session`), the [`OutputFormat`] every session converts to, and a guard
//! whose drop closes the device. The decisions live in [`owner::decide_output`], pure. Nothing
//! here is on the audio callback's path.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rodio::DeviceSinkBuilder;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::mixer::Mixer;

use crate::engine::{OutputFormat, log_output_error};
pub(crate) use owner::OutputEvent;

/// One open output.
pub(crate) struct Opened {
    pub(crate) mixer: Mixer,
    pub(crate) format: OutputFormat,
    /// Keeps the output open; dropping it closes the device (production: the
    /// `MixerDeviceSink`, whose drop disposes the `AudioUnit`, 74-289 ms on a live device,
    /// measured 2026-10-30).
    pub(crate) guard: Box<dyn Send>,
}

/// A message on the engine's output channel, drained every wake (`Engine::drain_output_events`).
/// `generation` is the open the message is about (the open stream's callbacks, the drop thread);
/// the engine drops one from an output it has already replaced. The watcher's messages carry none.
/// Sending never blocks (an unbounded `mpsc`), so it is safe from cpal's callback threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputMsg {
    pub(crate) generation: Option<u64>,
    pub(crate) event: OutputEvent,
}

/// How an open output and its drop thread reach the engine: the open's generation on the channel.
#[derive(Clone)]
pub(crate) struct OutputSignal {
    tx: Sender<OutputMsg>,
    generation: u64,
}

impl OutputSignal {
    pub(crate) fn new(tx: Sender<OutputMsg>, generation: u64) -> Self {
        Self { tx, generation }
    }

    pub(crate) fn fire(&self, event: OutputEvent) {
        // A send error means the engine is gone; there is nothing to tell.
        let _ = self.tx.send(OutputMsg {
            generation: Some(self.generation),
            event,
        });
    }
}

/// An open that failed: `message` is what the `Error { code: Device }` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenError {
    pub(crate) message: String,
}

/// How the engine reaches the output devices. Production is cpal through rodio; the tests script
/// every call.
pub(crate) trait OutputOpener: Send {
    /// Open the device `uid`, pinned (no follow); `signal` carries its `DeviceGone` and `Reopen`.
    fn open(&mut self, uid: &str, signal: OutputSignal) -> Result<Opened, OpenError>;
    /// The current default output's UID, `None` with no output device at all.
    fn default_output(&self) -> Option<String>;
    /// Whether the device `uid` still exists (`device_by_id`).
    fn present(&self, uid: &str) -> bool;
    /// Start the default-output watcher sending on `tx`; a no-op while one runs.
    fn start_watch(&mut self, tx: Sender<OutputMsg>);
    /// Stop the watcher and wait for its thread; a no-op without one.
    fn stop_watch(&mut self);
}

/// How often the watcher polls (R2, 2026-11-19: 35–191 ms per poll in its own thread).
pub(crate) const WATCH_PERIOD: Duration = Duration::from_millis(100);

/// A device that appeared within this long before becoming the default is "fresh" too (the
/// MOMENTUM 4 appears ~60 ms before the default moves; the Audio Pro picked 3.3 s after it
/// reappeared was rightly not fresh, R1 2026-11-19).
pub(crate) const FRESH_WINDOW: Duration = Duration::from_secs(3);

/// What one poll of the host sees.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Snapshot {
    pub(crate) default_uid: Option<String>,
    pub(crate) outputs: Vec<String>,
}

/// The watcher's memory between polls, pure: [`WatchState::observe`] turns a snapshot into at most
/// one event. The first snapshot only seeds it (nothing is known, so nothing is fresh and no
/// change is reported: the review's addendum).
#[derive(Debug, Default)]
pub(crate) struct WatchState {
    last: Option<Snapshot>,
    appeared: HashMap<String, Instant>,
}

impl WatchState {
    pub(crate) fn observe(&mut self, snap: Snapshot, now: Instant) -> Option<OutputEvent> {
        let Some(last) = self.last.take() else {
            self.last = Some(snap);
            return None;
        };
        for uid in &snap.outputs {
            if !last.outputs.contains(uid) {
                self.appeared.insert(uid.clone(), now);
            }
        }
        self.appeared.retain(|uid, _| snap.outputs.contains(uid));
        let event = if snap.default_uid == last.default_uid {
            None
        } else {
            match &snap.default_uid {
                None => Some(OutputEvent::NoDefault),
                Some(uid) => {
                    let recent = self
                        .appeared
                        .get(uid)
                        .is_some_and(|at| now.duration_since(*at) < FRESH_WINDOW);
                    Some(OutputEvent::DefaultChanged {
                        uid: uid.clone(),
                        fresh: !last.outputs.contains(uid) || recent,
                    })
                }
            }
        };
        self.last = Some(snap);
        event
    }
}

/// The watcher thread: `poll` every `period`, [`WatchState::observe`], the events on `tx`.
pub(crate) struct WatchHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WatchHandle {
    pub(crate) fn spawn(
        name: &str,
        period: Duration,
        poll: impl Fn() -> Snapshot + Send + 'static,
        tx: Sender<OutputMsg>,
    ) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let spawned = thread::Builder::new().name(name.into()).spawn(move || {
            let mut state = WatchState::default();
            while !stopping.load(Ordering::Relaxed) {
                if let Some(event) = state.observe(poll(), Instant::now())
                    && tx
                        .send(OutputMsg {
                            generation: None,
                            event,
                        })
                        .is_err()
                {
                    return;
                }
                thread::sleep(period);
            }
        });
        match spawned {
            Ok(join) => Some(Self {
                stop,
                join: Some(join),
            }),
            Err(e) => {
                log::warn!("output watcher thread refused ({e}); the default is not watched");
                None
            }
        }
    }

    /// Stop the thread and wait for it (at most one `period` plus one poll).
    pub(crate) fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The output devices through cpal 0.18.2 (rodio 0.23.0).
///
/// **Pinned by UID:** `device_by_id(uid)` + `DeviceSinkBuilder::from_device` build cpal's
/// `HalOutput` unit on that device (`host/coreaudio/macos/device.rs:243-255`), which does not
/// follow the default; the follow is the engine's (the watcher). Never `from_default_device()`:
/// its `DefaultOutput` unit follows by itself, and on the jack's codec the first start after a
/// plug-in fails inside coreaudiod with no error to cpal, leaving the process silent until the
/// device leaves (F9, 2026-11-11; `_handover/f9-step0-report-2026-11-11.md`).
///
/// The device's own buffer (`BufferSize::Default`), not rodio's: rodio 0.23.0 asks for a `Fixed`
/// ~50 ms (2048 frames at 44.1 kHz, `stream.rs:207-217`), which cpal 0.18.2 refuses on a device
/// whose range is lower (`14..=1024` on an Audio Pro A15 W, measured 2026-10-30). `Default` gave
/// 512 frames on both devices measured. No fallback to another device: an open on an output
/// nobody hears is worse than an honest `Error { code: Device }` (rodio PR #914, which clamps the
/// buffer, is unreleased; re-check at each rodio release). ONDAR.md, "0.1.2: decisions", 1.
///
/// The callbacks on a pinned stream (cpal 0.18.2 `host/coreaudio/macos/mod.rs:136-230`, the
/// `DisconnectManager`): `DeviceNotAvailable` when the device's `IsAlive` drops → `DeviceGone`;
/// `StreamInvalidated` when its nominal sample rate changes (cpal pauses the stream first) →
/// `Reopen`; `Xrun` on a processor overload → logged. A `DeviceChanged` cannot come from a pinned
/// stream; one in a log is a finding.
pub(crate) struct DeviceOpener {
    watch: Option<WatchHandle>,
}

impl DeviceOpener {
    pub(crate) fn new() -> Self {
        Self { watch: None }
    }
}

fn uid_of(device: &rodio::cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.id().to_string())
}

fn device_id(uid: &str) -> rodio::cpal::DeviceId {
    rodio::cpal::DeviceId::new(rodio::cpal::HostId::CoreAudio, uid)
}

fn poll_host() -> Snapshot {
    let host = rodio::cpal::default_host();
    let default_uid = host.default_output_device().and_then(|d| uid_of(&d));
    let outputs = host
        .output_devices()
        .map(|devices| devices.filter_map(|d| uid_of(&d)).collect())
        .unwrap_or_default();
    Snapshot {
        default_uid,
        outputs,
    }
}

impl OutputOpener for DeviceOpener {
    fn open(&mut self, uid: &str, signal: OutputSignal) -> Result<Opened, OpenError> {
        let host = rodio::cpal::default_host();
        let Some(device) = host.device_by_id(&device_id(uid)) else {
            return Err(OpenError {
                message: format!("the output device {uid} is gone"),
            });
        };
        let name = device
            .description()
            .map_or_else(|_| uid.to_string(), |d| d.name().to_string());
        let mut sink = DeviceSinkBuilder::from_device(device)
            .map_err(|e| OpenError {
                message: format!("{name}: {e}"),
            })?
            .with_buffer_size(rodio::cpal::BufferSize::Default)
            .with_error_callback(move |e| {
                log_output_error(&e);
                match e.kind() {
                    rodio::CpalErrorKind::DeviceNotAvailable => {
                        signal.fire(OutputEvent::DeviceGone);
                    }
                    rodio::CpalErrorKind::StreamInvalidated => signal.fire(OutputEvent::Reopen),
                    _ => {}
                }
            })
            .open_stream()
            .map_err(|e| OpenError {
                message: format!("{name}: {e}"),
            })?;
        sink.log_on_drop(false);
        let format = OutputFormat {
            channels: sink.config().channel_count(),
            sample_rate: sink.config().sample_rate(),
        };
        Ok(Opened {
            mixer: sink.mixer().clone(),
            format,
            guard: Box::new(sink),
        })
    }

    fn default_output(&self) -> Option<String> {
        rodio::cpal::default_host()
            .default_output_device()
            .and_then(|d| uid_of(&d))
    }

    fn present(&self, uid: &str) -> bool {
        rodio::cpal::default_host()
            .device_by_id(&device_id(uid))
            .is_some()
    }

    fn start_watch(&mut self, tx: Sender<OutputMsg>) {
        if self.watch.is_none() {
            self.watch = WatchHandle::spawn("ondar-output-watch", WATCH_PERIOD, poll_host, tx);
        }
    }

    fn stop_watch(&mut self) {
        if let Some(watch) = self.watch.take() {
            watch.stop();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The owner's state machine (beta item 1, merge A, commit A2 of `_handover/output-owner-plan.md`).
// Pure: the engine feeds it every output event and applies the actions it returns, in order
// (`Engine::drain_output_events`, A3). Nothing here touches a device or a thread.
// ---------------------------------------------------------------------------------------------

/// The owner's state machine; see the banner above.
pub(crate) mod owner {
    use std::time::{Duration, Instant};

    /// How long the owner waits before opening a device that appeared at the default change (F9:
    /// the jack's codec fails any start inside ~1.2 s of publication, measured 2026-11-11; 2 s and
    /// 3 s started 7 of 7, 1 s failed 2 of 2; 3 s is the margin, decision D4.4).
    pub(crate) const FRESH_DEVICE_WAIT: Duration = Duration::from_secs(3);

    /// The pause before the one retry of a failed start (P2g, 2026-11-11: a retry with the failed
    /// unit gone started 1 of 1).
    pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(1);

    /// The `Error { code: Device }` message when no output device exists at all (review R5).
    pub(crate) const NO_OUTPUT_MESSAGE: &str =
        "No audio output found. Connect one, then press Play.";

    /// The watcher's last `DefaultChanged`, kept while the owner has no open output (`Closed`,
    /// `Releasing`, `Waiting`; review R2, OCR a2-1): a `Play` on that same device inside its fresh
    /// window waits for it instead of starting inside F9's window. It never names the device to
    /// open (the chat's C1: the current default is opened; `last` only supplies the wait).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Change {
        pub(crate) uid: String,
        pub(crate) fresh: bool,
        pub(crate) at: Instant,
    }

    impl Change {
        /// When `uid` may be opened: the change's `at + wait` if this is a fresh change to that
        /// device still inside its window, else now.
        fn not_before(last: Option<&Change>, uid: &str, now: Instant, wait: Duration) -> Instant {
            match last {
                Some(c) if c.fresh && c.uid == uid && now < c.at + wait => c.at + wait,
                _ => now,
            }
        }
    }

    /// What the owner does once the releasing output's drop is reported.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Then {
        /// Open `uid`, not before `not_before` (the fresh-device wait counts from the change).
        Open { uid: String, not_before: Instant },
        /// Nothing: the session was paused at the loss (merge B; unused by merge A's table).
        #[allow(dead_code, reason = "merge B wires the loss pause")]
        Pause,
        /// Nothing: the idle close, or a Stop while an open was pending.
        Close,
    }

    /// The owner's state. The open itself is synchronous on the engine thread (9–318 ms measured
    /// 2026-11-19), so there is no `Opening`: an `Action::Open` is answered by `Opened` or
    /// `OpenFailed` before the next event, and `Waiting { until <= now }` is "the open is due".
    /// Every state without an open output carries `last`, the watcher's last change (OCR a2-1).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum OutputState {
        /// No output.
        Closed { last: Option<Change> },
        /// The normal state: one output, pinned to `uid`.
        Open { uid: String },
        /// The guard is dropping off the engine thread; `Dropped` moves on to `then`.
        Releasing { then: Then, last: Option<Change> },
        /// The old unit is gone; `uid` is opened at `until`. `retry` marks the one retry after a
        /// failed start: a second failure is final.
        Waiting {
            uid: String,
            until: Instant,
            retry: bool,
            last: Option<Change>,
        },
    }

    impl OutputState {
        pub(crate) fn closed() -> Self {
            Self::Closed { last: None }
        }

        /// Set while the owner is anything but `Open` (review R1): the attach arm writes `Buffering`
        /// instead of `Playing`, and `decide_tick` never resumes to `Playing`.
        pub(crate) fn is_switching(&self) -> bool {
            !matches!(self, Self::Open { .. })
        }

        fn last(&self) -> Option<&Change> {
            match self {
                Self::Closed { last }
                | Self::Releasing { last, .. }
                | Self::Waiting { last, .. } => last.as_ref(),
                Self::Open { .. } => None,
            }
        }
    }

    /// Everything that moves the owner. The device events arrive on the engine's output channel
    /// (the watcher, the open guard's callbacks, the drop thread); the commands are the engine's own.
    /// Messages from an output already replaced are dropped by their generation before they get here.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum OutputEvent {
        /// The watcher: the default output is now `uid`; `fresh` if it was not in the previous poll's
        /// list or appeared within the last 3 s.
        DefaultChanged { uid: String, fresh: bool },
        /// The watcher found no output device at all (review R5).
        NoDefault,
        /// cpal's `DeviceNotAvailable` on the open stream: its device is gone.
        DeviceGone,
        /// Reopen the same device: cpal's `StreamInvalidated` on the open stream (its device changed
        /// its nominal sample rate and cpal paused the stream; review R4, cpal 0.18.2
        /// `host/coreaudio/macos/mod.rs:170-181`, `:224-228`), or the stall WARN's backstop reopen.
        Reopen,
        /// The drop thread: the released guard is gone.
        Dropped,
        /// The engine's tick: `Waiting`'s clock.
        Tick,
        /// The opener: the open of `uid` succeeded.
        Opened { uid: String },
        /// The opener: the open or the start of `uid` failed; `message` is the user-facing text.
        OpenFailed { uid: String, message: String },
        /// A `play`: a session needs an output now.
        Play,
        /// A `pause`.
        Pause,
        /// A `stop`.
        Stop,
        /// The tick found no session for the idle limit: the output is to be released. The watcher
        /// stops with it, so `last` is forgotten too.
        IdleClose,
    }

    /// What the engine does for an event, in the order returned.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Action {
        /// Drop the open output's guard off-thread; the thread reports `Dropped`.
        Release,
        /// Open `uid` now, synchronously, and feed back `Opened` or `OpenFailed`.
        Open { uid: String },
        /// A live, unpaused session shows `Buffering` while the output is away (review R1).
        SessionBuffering,
        /// The session is paused (merge B's loss).
        #[allow(dead_code, reason = "merge B wires the loss pause")]
        SessionPause,
        /// The session attaches a new `Player` on the new output (plan §3).
        SessionReattach,
        /// End the session in `Error { code: Device, message }`; the slot is emptied.
        Fail { message: String },
    }

    /// What the engine knows at the moment of the event.
    #[derive(Debug, Clone)]
    pub(crate) struct Env {
        pub(crate) now: Instant,
        /// `device_by_id(previous).is_some()` for the device the owner holds or waits for.
        #[allow(
            dead_code,
            reason = "merge B reads it: a device gone pauses, one present switches"
        )]
        pub(crate) previous_present: bool,
        /// `default_output_device()`'s UID right now, for the cells that open "the current default".
        pub(crate) current_default: Option<String>,
        pub(crate) fresh_wait: Duration,
        pub(crate) retry_delay: Duration,
    }

    /// The owner's table (plan §2, with reviews R1–R5 and the OCR triage of A1–A3), pure. Merge A's
    /// loss column: a device that is gone is a switch to the current default with no wait (today's
    /// F1 outcome through the new path); merge B replaces it with the pause (`Then::Pause`,
    /// `Action::SessionPause`).
    #[allow(
        clippy::match_same_arms,
        reason = "one fall-through arm per row of the table keeps each row readable on its own"
    )]
    pub(crate) fn decide_output(
        state: &OutputState,
        ev: OutputEvent,
        env: &Env,
    ) -> (OutputState, Vec<Action>) {
        use Action as A;
        use OutputEvent as E;
        use OutputState as S;

        let wait = |fresh: bool| {
            if fresh {
                env.fresh_wait
            } else {
                Duration::ZERO
            }
        };
        let fail_no_output = || A::Fail {
            message: NO_OUTPUT_MESSAGE.into(),
        };
        let last = state.last().cloned();
        let changed = |uid: &str, fresh: bool| {
            Some(Change {
                uid: uid.into(),
                fresh,
                at: env.now,
            })
        };
        // Open `uid` at `not_before`: due now, the open action comes with the state.
        let open_at = |uid: String, not_before: Instant, last: Option<Change>| {
            let due = not_before <= env.now;
            (
                S::Waiting {
                    uid: uid.clone(),
                    until: not_before,
                    retry: false,
                    last,
                },
                if due {
                    vec![A::Open { uid }]
                } else {
                    Vec::new()
                },
            )
        };
        // A `Play` with no open output: the current default, after its fresh window if `last` is
        // that device's fresh change (R2, C1).
        let play_target = || {
            env.current_default.as_ref().map(|d| {
                let nb = Change::not_before(last.as_ref(), d, env.now, env.fresh_wait);
                (d.clone(), nb)
            })
        };
        let unchanged = (state.clone(), Vec::new());

        match (state, ev) {
            // ---- Closed ---------------------------------------------------------------------
            (S::Closed { .. }, E::DefaultChanged { uid, fresh }) => (
                S::Closed {
                    last: changed(&uid, fresh),
                },
                Vec::new(),
            ),
            (S::Closed { .. }, E::NoDefault | E::IdleClose) => (S::closed(), Vec::new()),
            (S::Closed { .. }, E::Play) => match play_target() {
                None => (S::closed(), vec![fail_no_output()]),
                Some((uid, nb)) if nb <= env.now => open_at(uid, nb, last),
                Some((uid, nb)) => {
                    let (s, _) = open_at(uid, nb, last);
                    (s, vec![A::SessionBuffering])
                }
            },
            (S::Closed { .. }, _) => unchanged,

            // ---- Open -----------------------------------------------------------------------
            (S::Open { uid }, E::DefaultChanged { uid: d, .. }) if *uid == d => unchanged,
            (S::Open { .. }, E::DefaultChanged { uid: d, fresh }) => {
                // Merge A: present or gone (`env.previous_present`), the new default is opened; only
                // the wait differs. Merge B pauses on gone.
                (
                    S::Releasing {
                        then: Then::Open {
                            uid: d.clone(),
                            not_before: env.now + wait(fresh),
                        },
                        last: changed(&d, fresh),
                    },
                    vec![A::Release, A::SessionBuffering],
                )
            }
            (S::Open { .. }, E::NoDefault) => (
                S::Releasing {
                    then: Then::Close,
                    last: None,
                },
                vec![A::Release, fail_no_output()],
            ),
            (S::Open { .. }, E::DeviceGone) => match env.current_default.clone() {
                Some(d) => (
                    S::Releasing {
                        then: Then::Open {
                            uid: d,
                            not_before: env.now,
                        },
                        last: None,
                    },
                    vec![A::Release, A::SessionBuffering],
                ),
                None => (
                    S::Releasing {
                        then: Then::Close,
                        last: None,
                    },
                    vec![A::Release, fail_no_output()],
                ),
            },
            (S::Open { uid }, E::Reopen) => (
                S::Releasing {
                    then: Then::Open {
                        uid: uid.clone(),
                        not_before: env.now,
                    },
                    last: None,
                },
                vec![A::Release, A::SessionBuffering],
            ),
            (S::Open { .. }, E::IdleClose) => (
                S::Releasing {
                    then: Then::Close,
                    last: None,
                },
                vec![A::Release],
            ),
            (S::Open { .. }, _) => unchanged,

            // ---- Releasing ------------------------------------------------------------------
            // A pending open follows the newest default (the wait restarts); a close or a pause
            // only records the change (OCR a3-5: the slot is empty, nothing may open on it).
            (S::Releasing { then, .. }, E::DefaultChanged { uid: d, fresh }) => match then {
                Then::Open { .. } => (
                    S::Releasing {
                        then: Then::Open {
                            uid: d.clone(),
                            not_before: env.now + wait(fresh),
                        },
                        last: changed(&d, fresh),
                    },
                    vec![A::SessionBuffering],
                ),
                Then::Pause | Then::Close => (
                    S::Releasing {
                        then: then.clone(),
                        last: changed(&d, fresh),
                    },
                    Vec::new(),
                ),
            },
            (S::Releasing { then, .. }, E::NoDefault) => match then {
                Then::Open { .. } => (
                    S::Releasing {
                        then: Then::Close,
                        last: None,
                    },
                    vec![fail_no_output()],
                ),
                Then::Pause | Then::Close => (
                    S::Releasing {
                        then: then.clone(),
                        last: None,
                    },
                    Vec::new(),
                ),
            },
            (S::Releasing { then, last }, E::Dropped) => match then {
                Then::Open { uid, not_before } => open_at(uid.clone(), *not_before, last.clone()),
                Then::Pause | Then::Close => (S::Closed { last: last.clone() }, Vec::new()),
            },
            (S::Releasing { then, .. }, E::Play) => match then {
                Then::Open { .. } => unchanged,
                Then::Pause | Then::Close => match play_target() {
                    Some((uid, not_before)) => (
                        S::Releasing {
                            then: Then::Open { uid, not_before },
                            last,
                        },
                        Vec::new(),
                    ),
                    None => (
                        S::Releasing {
                            then: Then::Close,
                            last: None,
                        },
                        vec![fail_no_output()],
                    ),
                },
            },
            (S::Releasing { .. }, E::Stop) => (
                S::Releasing {
                    then: Then::Close,
                    last,
                },
                Vec::new(),
            ),
            (S::Releasing { .. }, E::IdleClose) => (
                S::Releasing {
                    then: Then::Close,
                    last: None,
                },
                Vec::new(),
            ),
            (S::Releasing { .. }, _) => unchanged,

            // ---- Waiting --------------------------------------------------------------------
            (S::Waiting { uid, .. }, E::DefaultChanged { uid: d, .. }) if *uid == d => unchanged,
            (S::Waiting { .. }, E::DefaultChanged { uid: d, fresh }) => (
                S::Waiting {
                    uid: d.clone(),
                    until: env.now + wait(fresh),
                    retry: false,
                    last: changed(&d, fresh),
                },
                Vec::new(),
            ),
            (S::Waiting { .. }, E::NoDefault) => (S::closed(), vec![fail_no_output()]),
            (S::Waiting { uid, until, .. }, E::Tick) if *until <= env.now => {
                (state.clone(), vec![A::Open { uid: uid.clone() }])
            }
            (S::Waiting { uid, .. }, E::Opened { uid: o }) if *uid == o => {
                (S::Open { uid: o }, vec![A::SessionReattach])
            }
            (
                S::Waiting {
                    uid, retry, last, ..
                },
                E::OpenFailed { uid: o, message },
            ) if *uid == o => {
                if *retry {
                    (S::Closed { last: last.clone() }, vec![A::Fail { message }])
                } else {
                    (
                        S::Waiting {
                            uid: o,
                            until: env.now + env.retry_delay,
                            retry: true,
                            last: last.clone(),
                        },
                        Vec::new(),
                    )
                }
            }
            (S::Waiting { .. }, E::Stop) => (S::Closed { last }, Vec::new()),
            (S::Waiting { .. }, E::IdleClose) => (S::closed(), Vec::new()),
            (S::Waiting { .. }, _) => unchanged,
        }
    }
}

#[cfg(test)]
mod owner_tests {
    //! The owner's table (`_handover/output-owner-plan.md` §2, reviews R1–R5), one assertion per
    //! cell, named `row_column`. Every test says the mutation that fails it. Times are explicit
    //! `Instant`s, so no test sleeps.

    use std::time::{Duration, Instant};

    use super::owner::{
        Action as A, Change, Env, NO_OUTPUT_MESSAGE, OutputEvent as E, OutputState as S, Then,
        decide_output,
    };

    const WAIT: Duration = Duration::from_secs(3);
    const RETRY: Duration = Duration::from_secs(1);

    fn env(now: Instant) -> Env {
        Env {
            now,
            previous_present: true,
            current_default: Some("spk".into()),
            fresh_wait: WAIT,
            retry_delay: RETRY,
        }
    }

    fn open(uid: &str) -> S {
        S::Open { uid: uid.into() }
    }

    fn changed(uid: &str, fresh: bool) -> E {
        E::DefaultChanged {
            uid: uid.into(),
            fresh,
        }
    }

    fn change(uid: &str, fresh: bool, at: Instant) -> Option<Change> {
        Some(Change {
            uid: uid.into(),
            fresh,
            at,
        })
    }

    fn releasing_open(uid: &str, not_before: Instant) -> S {
        releasing_open_last(uid, not_before, None)
    }

    fn releasing_open_last(uid: &str, not_before: Instant, last: Option<Change>) -> S {
        S::Releasing {
            then: Then::Open {
                uid: uid.into(),
                not_before,
            },
            last,
        }
    }

    fn releasing(then: Then, last: Option<Change>) -> S {
        S::Releasing { then, last }
    }

    fn waiting(uid: &str, until: Instant, retry: bool) -> S {
        waiting_last(uid, until, retry, None)
    }

    fn waiting_last(uid: &str, until: Instant, retry: bool, last: Option<Change>) -> S {
        S::Waiting {
            uid: uid.into(),
            until,
            retry,
            last,
        }
    }

    fn fail_no_output() -> A {
        A::Fail {
            message: NO_OUTPUT_MESSAGE.into(),
        }
    }

    /// Closed row. **Fails if** a default change is not kept (mutation: `Closed { last: None }` on
    /// `DefaultChanged`: the fresh-window cell below cannot wait), if `NoDefault` or the idle close
    /// keeps a stale `last`, or if a device event or a command other than `Play` changes anything
    /// with no output to act on.
    #[test]
    fn closed_keeps_the_last_change_and_ignores_the_rest() {
        let t = Instant::now();
        let kept = S::Closed {
            last: change("hp", true, t),
        };
        assert_eq!(
            decide_output(&S::closed(), changed("hp", true), &env(t)),
            (kept.clone(), vec![])
        );
        assert_eq!(
            decide_output(&kept, E::NoDefault, &env(t)),
            (S::closed(), vec![])
        );
        assert_eq!(
            decide_output(&kept, E::IdleClose, &env(t)),
            (S::closed(), vec![])
        );
        for ev in [
            E::DeviceGone,
            E::Reopen,
            E::Dropped,
            E::Tick,
            E::Opened { uid: "hp".into() },
            E::OpenFailed {
                uid: "hp".into(),
                message: "x".into(),
            },
            E::Pause,
            E::Stop,
        ] {
            assert_eq!(
                decide_output(&kept, ev.clone(), &env(t)),
                (kept.clone(), vec![]),
                "{ev:?}"
            );
        }
    }

    /// Closed + Play (R2, R5, C1). **Fails if** a Play inside a fresh device's window opens at once
    /// (mutation: drop the `fresh && …` arm of `Change::not_before`: F9's window, the plan's exit
    /// line), if the wait is counted from the Play instead of the change (`until` would be
    /// `now + wait`), if a Play outside the window or on a device that existed waits, or if no
    /// device at all is anything but `Fail` with R5's text. `last` is kept into `Waiting`.
    #[test]
    fn closed_play_respects_a_fresh_device_and_no_device() {
        let t0 = Instant::now();
        let fresh = S::Closed {
            last: change("spk", true, t0),
        };
        // Inside the window: wait until the change + 3 s, the session in Buffering.
        let (s, a) = decide_output(&fresh, E::Play, &env(t0 + Duration::from_secs(1)));
        assert_eq!(
            s,
            waiting_last("spk", t0 + WAIT, false, change("spk", true, t0))
        );
        assert_eq!(a, vec![A::SessionBuffering]);
        // At the window's edge: open now.
        let (s, a) = decide_output(&fresh, E::Play, &env(t0 + WAIT));
        assert_eq!(
            s,
            waiting_last("spk", t0 + WAIT, false, change("spk", true, t0))
        );
        assert_eq!(a, vec![A::Open { uid: "spk".into() }]);
        // A device that existed: open now, whatever the time.
        let existed = S::Closed {
            last: change("spk", false, t0),
        };
        let (s, a) = decide_output(&existed, E::Play, &env(t0));
        assert_eq!(s, waiting_last("spk", t0, false, change("spk", false, t0)));
        assert_eq!(a, vec![A::Open { uid: "spk".into() }]);
        // No change kept: the current default, now.
        let (s, a) = decide_output(&S::closed(), E::Play, &env(t0));
        assert_eq!(s, waiting("spk", t0, false));
        assert_eq!(a, vec![A::Open { uid: "spk".into() }]);
        // No device at all.
        let mut none = env(t0);
        none.current_default = None;
        assert_eq!(
            decide_output(&S::closed(), E::Play, &none),
            (S::closed(), vec![fail_no_output()])
        );
    }

    /// Open row (R4, R5; merge A's loss column). **Fails if** a change to the held UID moves
    /// anything, if a switch to a fresh device does not wait 3 s from the change (mutation:
    /// `not_before: now`), if one to an existing device waits, if the release does not come
    /// first with the session's `Buffering` (R1), if a rate change does not reopen the same UID at
    /// once, if no default at all waits instead of failing, if a Play, Pause or Stop touches the
    /// open output, or if the idle close does not release it.
    #[test]
    fn open_switches_reopens_on_a_rate_change_and_releases_on_idle() {
        let t = Instant::now();
        let spk = open("spk");
        assert_eq!(
            decide_output(&spk, changed("spk", false), &env(t)),
            (spk.clone(), vec![])
        );
        assert_eq!(
            decide_output(&spk, changed("hp", true), &env(t)),
            (
                releasing_open_last("hp", t + WAIT, change("hp", true, t)),
                vec![A::Release, A::SessionBuffering]
            )
        );
        assert_eq!(
            decide_output(&spk, changed("bt", false), &env(t)),
            (
                releasing_open_last("bt", t, change("bt", false, t)),
                vec![A::Release, A::SessionBuffering]
            )
        );
        assert_eq!(
            decide_output(&spk, E::Reopen, &env(t)),
            (
                releasing_open("spk", t),
                vec![A::Release, A::SessionBuffering]
            )
        );
        assert_eq!(
            decide_output(&spk, E::NoDefault, &env(t)),
            (
                releasing(Then::Close, None),
                vec![A::Release, fail_no_output()]
            )
        );
        // Merge A: the held device gone = a switch to the current default, now.
        let mut gone = env(t);
        gone.previous_present = false;
        gone.current_default = Some("spk".into());
        assert_eq!(
            decide_output(&open("hp"), E::DeviceGone, &gone),
            (
                releasing_open("spk", t),
                vec![A::Release, A::SessionBuffering]
            )
        );
        gone.current_default = None;
        assert_eq!(
            decide_output(&open("hp"), E::DeviceGone, &gone),
            (
                releasing(Then::Close, None),
                vec![A::Release, fail_no_output()]
            )
        );
        for ev in [E::Play, E::Pause, E::Stop, E::Dropped, E::Tick] {
            assert_eq!(
                decide_output(&spk, ev.clone(), &env(t)),
                (spk.clone(), vec![]),
                "{ev:?}"
            );
        }
        assert_eq!(
            decide_output(&spk, E::IdleClose, &env(t)),
            (releasing(Then::Close, None), vec![A::Release])
        );
    }

    /// Releasing row (OCR a2-1, a3-5). **Fails if** `Dropped` opens before `not_before`
    /// (mutation: `due = true` in `open_at`: "a new device waits"), if it does not open at once
    /// when the wait has passed, if a second default change during a pending open does not restart
    /// the wait for the newest default, if a change during a close or a pause turns it into an
    /// open (a3-5) or is not recorded (a2-1), if `Dropped` after a close or a pause loses `last`
    /// (a2-1: a Play then starts inside F9's window), if a Play while nothing was going to be
    /// opened does not schedule the current default after its window, or if Stop leaves a pending
    /// open for nobody.
    #[test]
    fn releasing_moves_on_at_dropped_and_restarts_on_a_newer_default() {
        let t = Instant::now();
        let later = t + WAIT;
        let hp = change("hp", true, t);
        let pending = releasing_open_last("hp", later, hp.clone());
        assert_eq!(
            decide_output(&pending, E::Dropped, &env(t + Duration::from_millis(20))),
            (waiting_last("hp", later, false, hp.clone()), vec![])
        );
        assert_eq!(
            decide_output(&pending, E::Dropped, &env(later)),
            (
                waiting_last("hp", later, false, hp.clone()),
                vec![A::Open { uid: "hp".into() }]
            )
        );
        let t1 = t + Duration::from_secs(1);
        assert_eq!(
            decide_output(&pending, changed("bt", false), &env(t1)),
            (
                releasing_open_last("bt", t1, change("bt", false, t1)),
                vec![A::SessionBuffering]
            )
        );
        assert_eq!(
            decide_output(&pending, changed("hp2", true), &env(t1)),
            (
                releasing_open_last("hp2", t1 + WAIT, change("hp2", true, t1)),
                vec![A::SessionBuffering]
            )
        );
        for then in [Then::Close, Then::Pause] {
            // A change during a close or a pause is recorded, nothing opens (a3-5).
            assert_eq!(
                decide_output(&releasing(then.clone(), None), changed("hp", true), &env(t)),
                (releasing(then.clone(), hp.clone()), vec![]),
                "{then:?}"
            );
            // The drop keeps `last` (a2-1).
            assert_eq!(
                decide_output(&releasing(then.clone(), hp.clone()), E::Dropped, &env(t)),
                (S::Closed { last: hp.clone() }, vec![]),
                "{then:?}"
            );
            // A Play while the drop is in flight: the current default, after its window if it is
            // the fresh device (a2-1), else now.
            let mut on_hp = env(t1);
            on_hp.current_default = Some("hp".into());
            assert_eq!(
                decide_output(&releasing(then.clone(), hp.clone()), E::Play, &on_hp),
                (releasing_open_last("hp", later, hp.clone()), vec![]),
                "{then:?}"
            );
            assert_eq!(
                decide_output(&releasing(then.clone(), hp.clone()), E::Play, &env(t1)),
                (releasing_open_last("spk", t1, hp.clone()), vec![]),
                "{then:?}"
            );
        }
        assert_eq!(
            decide_output(&pending, E::Play, &env(t)),
            (pending.clone(), vec![])
        );
        assert_eq!(
            decide_output(&pending, E::Stop, &env(t)),
            (releasing(Then::Close, hp.clone()), vec![])
        );
        assert_eq!(
            decide_output(&pending, E::IdleClose, &env(t)),
            (releasing(Then::Close, None), vec![])
        );
        assert_eq!(
            decide_output(&pending, E::NoDefault, &env(t)),
            (releasing(Then::Close, None), vec![fail_no_output()])
        );
        assert_eq!(
            decide_output(&releasing(Then::Close, hp.clone()), E::NoDefault, &env(t)),
            (releasing(Then::Close, None), vec![])
        );
        for ev in [E::DeviceGone, E::Reopen, E::Tick, E::Pause] {
            assert_eq!(
                decide_output(&pending, ev.clone(), &env(t)),
                (pending.clone(), vec![]),
                "{ev:?}"
            );
        }
    }

    /// Waiting row (the retry, R5). **Fails if** a Tick before `until` opens (mutation: drop the
    /// guard), if a Tick at `until` does not, if `Opened` does not end `Open` with the session
    /// re-attached, if the first failure does not schedule one retry 1 s later (mutation: `Fail`
    /// at once: F9's P2g recovery lost) or the second does not `Fail` with the opener's message,
    /// if a Play opens early, if a newer default does not restart the wait, or if Stop and the
    /// idle close leave an open pending.
    #[test]
    fn waiting_opens_on_time_retries_once_and_never_early() {
        let t = Instant::now();
        let until = t + WAIT;
        let w = waiting("hp", until, false);
        assert_eq!(
            decide_output(&w, E::Tick, &env(t + Duration::from_millis(2_999))),
            (w.clone(), vec![])
        );
        assert_eq!(
            decide_output(&w, E::Tick, &env(until)),
            (w.clone(), vec![A::Open { uid: "hp".into() }])
        );
        assert_eq!(
            decide_output(&w, E::Opened { uid: "hp".into() }, &env(until)),
            (open("hp"), vec![A::SessionReattach])
        );
        let failed = E::OpenFailed {
            uid: "hp".into(),
            message: "No sound on hp".into(),
        };
        assert_eq!(
            decide_output(&w, failed.clone(), &env(until)),
            (waiting("hp", until + RETRY, true), vec![])
        );
        assert_eq!(
            decide_output(&waiting("hp", until, true), failed, &env(until)),
            (
                S::closed(),
                vec![A::Fail {
                    message: "No sound on hp".into()
                }]
            )
        );
        assert_eq!(decide_output(&w, E::Play, &env(t)), (w.clone(), vec![]));
        assert_eq!(
            decide_output(&w, changed("hp", true), &env(t)),
            (w.clone(), vec![])
        );
        assert_eq!(
            decide_output(&w, changed("spk", false), &env(t)),
            (
                waiting_last("spk", t, false, change("spk", false, t)),
                vec![]
            )
        );
        assert_eq!(
            decide_output(&w, E::NoDefault, &env(t)),
            (S::closed(), vec![fail_no_output()])
        );
        assert_eq!(
            decide_output(&w, E::IdleClose, &env(t)),
            (S::closed(), vec![])
        );
        let hp = change("hp", true, t);
        assert_eq!(
            decide_output(
                &waiting_last("hp", until, false, hp.clone()),
                E::Stop,
                &env(t)
            ),
            (S::Closed { last: hp }, vec![]),
            "Stop lost the last change (a2-1)"
        );
        for ev in [E::DeviceGone, E::Reopen, E::Dropped, E::Pause] {
            assert_eq!(
                decide_output(&w, ev.clone(), &env(t)),
                (w.clone(), vec![]),
                "{ev:?}"
            );
        }
    }

    /// The chat's C1 (2026-11-19): a `Play` opens the **current default**; `last` only supplies
    /// the fresh wait, and only when it names that same device. **Fails if** `last.uid` is opened
    /// (stale after the watcher stopped: a device that is gone or no longer the default).
    #[test]
    fn closed_play_opens_the_current_default_not_the_last_change() {
        let t = Instant::now();
        let last_a = S::Closed {
            last: change("a", true, t),
        };
        let mut e = env(t + Duration::from_secs(1));
        e.current_default = Some("b".into());
        assert_eq!(
            decide_output(&last_a, E::Play, &e),
            (
                waiting_last("b", t + Duration::from_secs(1), false, change("a", true, t)),
                vec![A::Open { uid: "b".into() }]
            )
        );
    }

    /// OCR a3-5: a default change while an idle close or a loss is releasing only records the
    /// change; it does not turn the release into an open (the slot is empty, a Play in the wait
    /// would give the decode thread no output). **Fails if** `Releasing { Close }` +
    /// `DefaultChanged` becomes `Then::Open`.
    #[test]
    fn a_change_during_an_idle_close_is_recorded_not_opened() {
        let t = Instant::now();
        let (s, a) = decide_output(&releasing(Then::Close, None), changed("hp", true), &env(t));
        assert!(
            matches!(
                s,
                S::Releasing {
                    then: Then::Close,
                    ..
                }
            ),
            "the idle close became an open: {s:?}"
        );
        assert_eq!(a, vec![]);
    }

    /// R1's flag: every state but `Open` is "switching". **Fails if** `Waiting` or `Releasing`
    /// reads as not switching (the attach arm would write `Playing` on a mixer nobody pulls).
    #[test]
    fn every_state_but_open_is_switching() {
        let t = Instant::now();
        assert!(S::closed().is_switching());
        assert!(releasing_open("hp", t).is_switching());
        assert!(waiting("hp", t, false).is_switching());
        assert!(!open("spk").is_switching());
    }
}

#[cfg(test)]
mod watch_tests {
    //! The default-output watcher (plan §4): the pure step on scripted snapshots, and the thread's
    //! stop. No device, no sleep but the thread test's period.

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{FRESH_WINDOW, OutputEvent, Snapshot, WatchHandle, WatchState};
    use crate::sync::lock;

    fn snap(default: Option<&str>, outputs: &[&str]) -> Snapshot {
        Snapshot {
            default_uid: default.map(str::to_string),
            outputs: outputs.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn changed(uid: &str, fresh: bool) -> Option<OutputEvent> {
        Some(OutputEvent::DefaultChanged {
            uid: uid.into(),
            fresh,
        })
    }

    /// The first poll seeds the state and reports nothing (the review's addendum: nothing is
    /// known, so nothing is fresh and no change is reported); the same default again reports
    /// nothing. **Fails if** the first snapshot is reported as a change (the engine would release
    /// and reopen its output at every start), or if an unchanged default is.
    #[test]
    fn the_first_poll_seeds_and_an_unchanged_default_is_silent() {
        let t = Instant::now();
        let mut w = WatchState::default();
        assert_eq!(w.observe(snap(Some("spk"), &["spk"]), t), None);
        assert_eq!(w.observe(snap(Some("spk"), &["spk"]), t), None);
        assert_eq!(w.observe(snap(Some("spk"), &["spk", "bt"]), t), None);
    }

    /// A default change to a device that was in the previous list is not fresh; to one that was
    /// not is fresh (the jack: the device and the default change land in one poll). **Fails if**
    /// `fresh` is read from the new list instead of the previous one (both would be "present"),
    /// or if the change is not reported.
    #[test]
    fn a_default_change_is_fresh_only_for_a_device_the_previous_poll_did_not_list() {
        let t = Instant::now();
        let mut w = WatchState::default();
        w.observe(snap(Some("spk"), &["spk", "bt"]), t);
        assert_eq!(
            w.observe(snap(Some("bt"), &["spk", "bt"]), t),
            changed("bt", false)
        );
        assert_eq!(
            w.observe(snap(Some("hp"), &["spk", "bt", "hp"]), t),
            changed("hp", true)
        );
    }

    /// The 3 s clause: a device that appeared within `FRESH_WINDOW` before becoming the default is
    /// fresh (the MOMENTUM 4: listed ~60 ms before the default moves); one that appeared longer
    /// ago is not (the Audio Pro picked 3.3 s after it reappeared, R1). **Fails if** `appeared` is
    /// not kept across polls, or if the window is not applied.
    #[test]
    fn a_device_that_appeared_within_the_window_is_fresh_and_an_older_one_is_not() {
        let t = Instant::now();
        let mut w = WatchState::default();
        w.observe(snap(Some("spk"), &["spk"]), t);
        assert_eq!(w.observe(snap(Some("spk"), &["spk", "bt"]), t), None);
        assert_eq!(
            w.observe(
                snap(Some("bt"), &["spk", "bt"]),
                t + Duration::from_millis(2_999)
            ),
            changed("bt", true)
        );
        let mut w = WatchState::default();
        w.observe(snap(Some("spk"), &["spk"]), t);
        w.observe(snap(Some("spk"), &["spk", "bt"]), t);
        assert_eq!(
            w.observe(snap(Some("bt"), &["spk", "bt"]), t + FRESH_WINDOW),
            changed("bt", false)
        );
    }

    /// No output device at all is `NoDefault` (review R5), and a device that comes back after
    /// that is reported as a fresh change. **Fails if** `None` is treated as "unchanged" or
    /// reported as a change to an empty UID.
    #[test]
    fn no_default_is_its_own_event() {
        let t = Instant::now();
        let mut w = WatchState::default();
        w.observe(snap(Some("spk"), &["spk"]), t);
        assert_eq!(w.observe(snap(None, &[]), t), Some(OutputEvent::NoDefault));
        assert_eq!(w.observe(snap(None, &[]), t), None);
        assert_eq!(
            w.observe(snap(Some("spk"), &["spk"]), t),
            changed("spk", true)
        );
    }

    /// The thread: it polls, reports a change on the channel, and `stop()` joins it. **What it
    /// pins:** one `DefaultChanged` after the scripted snapshot changes, then `stop()` returns
    /// (the join is the claim; the hang guard is the test's own timeout). **Fails if** the stop
    /// flag is not read between polls or the thread is not joined.
    #[test]
    fn the_thread_reports_and_stops() {
        let script = Arc::new(Mutex::new(snap(Some("spk"), &["spk"])));
        let polls = Arc::new(AtomicUsize::new(0));
        let (poll_script, poll_count) = (script.clone(), polls.clone());
        let (tx, rx) = mpsc::channel();
        let handle = WatchHandle::spawn(
            "ondar-audio:test-watch",
            Duration::from_millis(5),
            move || {
                poll_count.fetch_add(1, Ordering::SeqCst);
                lock(&poll_script).clone()
            },
            tx,
        )
        .expect("the watcher thread");
        // The first poll seeds the state; the change comes after it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while polls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        *lock(&script) = snap(Some("bt"), &["spk", "bt"]);
        let msg = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a message from the watcher");
        assert_eq!(msg.generation, None);
        assert_eq!(Some(msg.event), changed("bt", true));
        handle.stop();
    }
}
