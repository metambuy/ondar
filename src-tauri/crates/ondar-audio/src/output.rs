//! The output owner's seam (0.1.2, F1). The engine opens its output through an
//! [`OutputOpener`], so the session tests can stand a bare `rodio::mixer::mixer` and a puller
//! thread in for the device, and count the opens; production opens the default device.
//!
//! What the engine keeps of an open is the [`Mixer`] (each attach connects its own `Player` to
//! it, `engine.rs::run_session`), the [`OutputFormat`] every session converts to, and a guard
//! whose drop closes the device. Nothing here is on the audio callback's path.

use std::sync::mpsc::Sender;

use rodio::DeviceSinkBuilder;
use rodio::mixer::Mixer;

use crate::engine::{OutputFormat, log_output_error};

/// One open output.
pub(crate) struct Opened {
    pub(crate) mixer: Mixer,
    pub(crate) format: OutputFormat,
    /// Keeps the output open; dropping it closes the device (production: the
    /// `MixerDeviceSink`, whose drop disposes the `AudioUnit`, 74-289 ms on a live device,
    /// measured 2026-10-30).
    pub(crate) guard: Box<dyn Send>,
}

/// How an open output tells the engine it is gone: the open's generation, on a channel the
/// engine drains every wake (`Engine::check_output_lost`). The generation lets the engine ignore
/// a late signal from an output it has already replaced. Sending never blocks (an unbounded
/// `mpsc`), so it is safe from cpal's error-callback thread.
#[derive(Clone)]
pub(crate) struct LossSignal {
    tx: Sender<u64>,
    generation: u64,
}

impl LossSignal {
    pub(crate) fn new(tx: Sender<u64>, generation: u64) -> Self {
        Self { tx, generation }
    }

    pub(crate) fn fire(&self) {
        // A send error means the engine is gone; there is nothing to tell.
        let _ = self.tx.send(self.generation);
    }
}

/// How the engine opens its output. `lost` is fired when the output reports that its device is
/// gone.
pub(crate) trait OutputOpener: Send {
    fn open(&mut self, lost: LossSignal) -> Result<Opened, String>;
}

/// The default output device.
///
/// The device's own buffer (`BufferSize::Default`), not rodio's: rodio 0.23.0 asks for a `Fixed`
/// ~50 ms (2048 frames at 44.1 kHz, `stream.rs:207-217`), which cpal 0.18.2 refuses on a device
/// whose range is lower (`14..=1024` on an Audio Pro A15 W, measured 2026-10-30), so
/// `open_default_sink` failed on every device. `Default` gave 512 frames on both devices measured
/// (11.6 ms at 44.1 kHz, 10.7 ms at 48 kHz). No fallback to another device: an open on an output
/// nobody hears is worse than an honest `Error { code: Device }` (rodio PR #914, which clamps the
/// buffer, is unreleased; re-check at each rodio release). ONDAR.md, "0.1.2: decisions", 1.
///
/// The loss: on rodio 0.23.0 / cpal 0.18.2 a stream on the default device follows a new default
/// by itself (`DeviceChanged`, measured 5 of 5 Bluetooth losses and 10 of 10 switches,
/// 2026-10-30); `DeviceNotAvailable` means no output was left to follow (with no output device
/// at all, cpal pauses the stream, the F1 shape), so it fires `lost` (decision 4).
pub(crate) struct DeviceOpener;

impl OutputOpener for DeviceOpener {
    fn open(&mut self, lost: LossSignal) -> Result<Opened, String> {
        let mut sink = DeviceSinkBuilder::from_default_device()
            .map_err(|e| e.to_string())?
            .with_buffer_size(rodio::cpal::BufferSize::Default)
            .with_error_callback(move |e| {
                log_output_error(&e);
                if e.kind() == rodio::CpalErrorKind::DeviceNotAvailable {
                    lost.fire();
                }
            })
            .open_stream()
            .map_err(|e| e.to_string())?;
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
}

// ---------------------------------------------------------------------------------------------
// The owner's state machine (beta item 1, merge A, commit A2 of `_handover/output-owner-plan.md`).
// Pure: the engine feeds it every output event and applies the actions it returns, in order
// (`Engine::drain_output_events`, A3). Nothing here touches a device or a thread.
// ---------------------------------------------------------------------------------------------

/// The owner's state machine; see the banner above.
// Commit A2 defines the machine; A3 wires it into the engine and removes this allow.
#[allow(dead_code)]
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

    /// The watcher's last `DefaultChanged`, kept while the owner is `Closed` (review R2): a `Play`
    /// inside a fresh device's window waits for it instead of starting inside F9's window.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Change {
        pub(crate) uid: String,
        pub(crate) fresh: bool,
        pub(crate) at: Instant,
    }

    /// What the owner does once the releasing output's drop is reported.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Then {
        /// Open `uid`, not before `not_before` (the fresh-device wait counts from the change).
        Open { uid: String, not_before: Instant },
        /// Nothing: the session was paused at the loss (merge B; unused by merge A's table).
        Pause,
        /// Nothing: the idle close, or a Stop while an open was pending.
        Close,
    }

    /// The owner's state. The open itself is synchronous on the engine thread (9–318 ms measured
    /// 2026-11-19), so there is no `Opening`: an `Action::Open` is answered by `Opened` or
    /// `OpenFailed` before the next event, and `Waiting { until <= now }` is "the open is due".
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum OutputState {
        /// No output. `last` is the watcher's last default change, if it is running.
        Closed { last: Option<Change> },
        /// The normal state: one output, pinned to `uid`.
        Open { uid: String },
        /// The guard is dropping off the engine thread; `Dropped` moves on to `then`.
        Releasing { then: Then },
        /// The old unit is gone; `uid` is opened at `until`. `retry` marks the one retry after a
        /// failed start: a second failure is final.
        Waiting {
            uid: String,
            until: Instant,
            retry: bool,
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
        /// The tick found no session for the idle limit: the output is to be released.
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
        pub(crate) previous_present: bool,
        /// `default_output_device()`'s UID right now, for the cells that open "the current default".
        pub(crate) current_default: Option<String>,
        pub(crate) fresh_wait: Duration,
        pub(crate) retry_delay: Duration,
    }

    /// The owner's table (plan §2, with reviews R1–R5), pure. Merge A's loss column: a device that
    /// is gone is a switch to the current default with no wait (today's F1 outcome through the new
    /// path); merge B replaces it with the pause (`Then::Pause`, `Action::SessionPause`).
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
        let no_output = (
            S::closed(),
            vec![A::Fail {
                message: NO_OUTPUT_MESSAGE.into(),
            }],
        );
        let open_now = |uid: String| {
            (
                S::Waiting {
                    uid: uid.clone(),
                    until: env.now,
                    retry: false,
                },
                vec![A::Open { uid }],
            )
        };
        let unchanged = (state.clone(), Vec::new());

        match (state, ev) {
            // ---- Closed ---------------------------------------------------------------------
            (S::Closed { .. }, E::DefaultChanged { uid, fresh }) => (
                S::Closed {
                    last: Some(Change {
                        uid,
                        fresh,
                        at: env.now,
                    }),
                },
                Vec::new(),
            ),
            (S::Closed { .. }, E::NoDefault) => (S::closed(), Vec::new()),
            (S::Closed { last }, E::Play) => {
                let Some(uid) = last
                    .as_ref()
                    .map(|c| c.uid.clone())
                    .or_else(|| env.current_default.clone())
                else {
                    return no_output;
                };
                match last {
                    Some(c) if c.fresh && env.now < c.at + env.fresh_wait => (
                        S::Waiting {
                            uid,
                            until: c.at + env.fresh_wait,
                            retry: false,
                        },
                        vec![A::SessionBuffering],
                    ),
                    _ => open_now(uid),
                }
            }
            (S::Closed { .. }, _) => unchanged,

            // ---- Open -----------------------------------------------------------------------
            (S::Open { uid }, E::DefaultChanged { uid: d, .. }) if *uid == d => unchanged,
            (S::Open { .. }, E::DefaultChanged { uid: d, fresh }) => {
                // Merge A: present or gone (`env.previous_present`), the new default is opened; only
                // the wait differs. Merge B pauses on gone.
                (
                    S::Releasing {
                        then: Then::Open {
                            uid: d,
                            not_before: env.now + wait(fresh),
                        },
                    },
                    vec![A::Release, A::SessionBuffering],
                )
            }
            (S::Open { .. }, E::NoDefault) => (
                S::Releasing { then: Then::Close },
                vec![
                    A::Release,
                    A::Fail {
                        message: NO_OUTPUT_MESSAGE.into(),
                    },
                ],
            ),
            (S::Open { .. }, E::DeviceGone) => match env.current_default.clone() {
                Some(d) => (
                    S::Releasing {
                        then: Then::Open {
                            uid: d,
                            not_before: env.now,
                        },
                    },
                    vec![A::Release, A::SessionBuffering],
                ),
                None => (
                    S::Releasing { then: Then::Close },
                    vec![
                        A::Release,
                        A::Fail {
                            message: NO_OUTPUT_MESSAGE.into(),
                        },
                    ],
                ),
            },
            (S::Open { uid }, E::Reopen) => (
                S::Releasing {
                    then: Then::Open {
                        uid: uid.clone(),
                        not_before: env.now,
                    },
                },
                vec![A::Release, A::SessionBuffering],
            ),
            (S::Open { .. }, E::IdleClose) => {
                (S::Releasing { then: Then::Close }, vec![A::Release])
            }
            (S::Open { .. }, _) => unchanged,

            // ---- Releasing ------------------------------------------------------------------
            (S::Releasing { .. }, E::DefaultChanged { uid: d, fresh }) => (
                S::Releasing {
                    then: Then::Open {
                        uid: d,
                        not_before: env.now + wait(fresh),
                    },
                },
                vec![A::SessionBuffering],
            ),
            (S::Releasing { .. }, E::NoDefault) => (
                S::Releasing { then: Then::Close },
                vec![A::Fail {
                    message: NO_OUTPUT_MESSAGE.into(),
                }],
            ),
            (S::Releasing { then }, E::Dropped) => match then {
                Then::Open { uid, not_before } if *not_before <= env.now => open_now(uid.clone()),
                Then::Open { uid, not_before } => (
                    S::Waiting {
                        uid: uid.clone(),
                        until: *not_before,
                        retry: false,
                    },
                    Vec::new(),
                ),
                Then::Pause | Then::Close => (S::closed(), Vec::new()),
            },
            (S::Releasing { then }, E::Play) => match then {
                Then::Open { .. } => unchanged,
                Then::Pause | Then::Close => match env.current_default.clone() {
                    Some(d) => (
                        S::Releasing {
                            then: Then::Open {
                                uid: d,
                                not_before: env.now,
                            },
                        },
                        Vec::new(),
                    ),
                    None => (
                        S::Releasing { then: Then::Close },
                        vec![A::Fail {
                            message: NO_OUTPUT_MESSAGE.into(),
                        }],
                    ),
                },
            },
            (S::Releasing { .. }, E::Stop | E::IdleClose) => {
                (S::Releasing { then: Then::Close }, Vec::new())
            }
            (S::Releasing { .. }, _) => unchanged,

            // ---- Waiting --------------------------------------------------------------------
            (S::Waiting { uid, .. }, E::DefaultChanged { uid: d, .. }) if *uid == d => unchanged,
            (S::Waiting { .. }, E::DefaultChanged { uid: d, fresh }) => (
                S::Waiting {
                    uid: d,
                    until: env.now + wait(fresh),
                    retry: false,
                },
                Vec::new(),
            ),
            (S::Waiting { .. }, E::NoDefault) => no_output,
            (S::Waiting { uid, until, .. }, E::Tick) if *until <= env.now => {
                (state.clone(), vec![A::Open { uid: uid.clone() }])
            }
            (S::Waiting { uid, .. }, E::Opened { uid: o }) if *uid == o => {
                (S::Open { uid: o }, vec![A::SessionReattach])
            }
            (S::Waiting { uid, retry, .. }, E::OpenFailed { uid: o, message }) if *uid == o => {
                if *retry {
                    (S::closed(), vec![A::Fail { message }])
                } else {
                    (
                        S::Waiting {
                            uid: o,
                            until: env.now + env.retry_delay,
                            retry: true,
                        },
                        Vec::new(),
                    )
                }
            }
            (S::Waiting { .. }, E::Stop | E::IdleClose) => (S::closed(), Vec::new()),
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

    fn releasing_open(uid: &str, not_before: Instant) -> S {
        S::Releasing {
            then: Then::Open {
                uid: uid.into(),
                not_before,
            },
        }
    }

    fn waiting(uid: &str, until: Instant, retry: bool) -> S {
        S::Waiting {
            uid: uid.into(),
            until,
            retry,
        }
    }

    fn fail_no_output() -> A {
        A::Fail {
            message: NO_OUTPUT_MESSAGE.into(),
        }
    }

    /// Closed row. **Fails if** a default change is not kept (mutation: `Closed { last: None }` on
    /// `DefaultChanged`: the fresh-window cell below cannot wait), if `NoDefault` keeps a stale
    /// `last` (a later `Play` would open a device that is gone), or if a device event or a
    /// command other than `Play` changes anything with no output to act on.
    #[test]
    fn closed_keeps_the_last_change_and_ignores_the_rest() {
        let t = Instant::now();
        let kept = S::Closed {
            last: Some(Change {
                uid: "hp".into(),
                fresh: true,
                at: t,
            }),
        };
        assert_eq!(
            decide_output(&S::closed(), changed("hp", true), &env(t)),
            (kept.clone(), vec![])
        );
        assert_eq!(
            decide_output(&kept, E::NoDefault, &env(t)),
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
            E::IdleClose,
        ] {
            assert_eq!(
                decide_output(&kept, ev.clone(), &env(t)),
                (kept.clone(), vec![]),
                "{ev:?}"
            );
        }
    }

    /// Closed + Play (R2, R5). **Fails if** a Play inside a fresh device's window opens at once
    /// (mutation: drop the `fresh && now < at + wait` arm: F9's window, the plan's exit line), if
    /// the wait is counted from the Play instead of the change (`until` would be `now + wait`), if
    /// a Play outside the window or on a device that existed waits, if `last` is ignored in favour
    /// of the current default, or if no device at all is anything but `Fail` with R5's text.
    #[test]
    fn closed_play_respects_a_fresh_device_and_no_device() {
        let t0 = Instant::now();
        let fresh = S::Closed {
            last: Some(Change {
                uid: "hp".into(),
                fresh: true,
                at: t0,
            }),
        };
        // Inside the window: wait until the change + 3 s, the session in Buffering.
        let (s, a) = decide_output(&fresh, E::Play, &env(t0 + Duration::from_secs(1)));
        assert_eq!(s, waiting("hp", t0 + WAIT, false));
        assert_eq!(a, vec![A::SessionBuffering]);
        // At the window's edge: open now.
        let (s, a) = decide_output(&fresh, E::Play, &env(t0 + WAIT));
        assert_eq!(s, waiting("hp", t0 + WAIT, false));
        assert_eq!(a, vec![A::Open { uid: "hp".into() }]);
        // A device that existed: open now, whatever the time.
        let existed = S::Closed {
            last: Some(Change {
                uid: "hp".into(),
                fresh: false,
                at: t0,
            }),
        };
        let (s, a) = decide_output(&existed, E::Play, &env(t0));
        assert_eq!(s, waiting("hp", t0, false));
        assert_eq!(a, vec![A::Open { uid: "hp".into() }]);
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
                releasing_open("hp", t + WAIT),
                vec![A::Release, A::SessionBuffering]
            )
        );
        assert_eq!(
            decide_output(&spk, changed("bt", false), &env(t)),
            (
                releasing_open("bt", t),
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
                S::Releasing { then: Then::Close },
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
                S::Releasing { then: Then::Close },
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
            (S::Releasing { then: Then::Close }, vec![A::Release])
        );
    }

    /// Releasing row. **Fails if** `Dropped` opens before `not_before` (mutation: `open_now`
    /// unconditionally: "a new device waits"), if it does not open at once when the wait has
    /// passed, if a second default change during the release does not restart the wait for the
    /// newest default, if a `Dropped` after a Close or Pause does not end `Closed`, if a Play
    /// while nothing was going to be opened does not schedule the current default, or if Stop
    /// leaves a pending open for nobody.
    #[test]
    fn releasing_moves_on_at_dropped_and_restarts_on_a_newer_default() {
        let t = Instant::now();
        let later = t + WAIT;
        let pending = releasing_open("hp", later);
        assert_eq!(
            decide_output(&pending, E::Dropped, &env(t + Duration::from_millis(20))),
            (waiting("hp", later, false), vec![])
        );
        assert_eq!(
            decide_output(&pending, E::Dropped, &env(later)),
            (
                waiting("hp", later, false),
                vec![A::Open { uid: "hp".into() }]
            )
        );
        assert_eq!(
            decide_output(
                &pending,
                changed("bt", false),
                &env(t + Duration::from_secs(1))
            ),
            (
                releasing_open("bt", t + Duration::from_secs(1)),
                vec![A::SessionBuffering]
            )
        );
        assert_eq!(
            decide_output(
                &pending,
                changed("hp2", true),
                &env(t + Duration::from_secs(1))
            ),
            (
                releasing_open("hp2", t + Duration::from_secs(1) + WAIT),
                vec![A::SessionBuffering]
            )
        );
        for then in [Then::Close, Then::Pause] {
            assert_eq!(
                decide_output(&S::Releasing { then: then.clone() }, E::Dropped, &env(t)),
                (S::closed(), vec![]),
                "{then:?}"
            );
            assert_eq!(
                decide_output(&S::Releasing { then }, E::Play, &env(t)),
                (releasing_open("spk", t), vec![])
            );
        }
        assert_eq!(
            decide_output(&pending, E::Play, &env(t)),
            (pending.clone(), vec![])
        );
        assert_eq!(
            decide_output(&pending, E::Stop, &env(t)),
            (S::Releasing { then: Then::Close }, vec![])
        );
        assert_eq!(
            decide_output(&pending, E::NoDefault, &env(t)),
            (S::Releasing { then: Then::Close }, vec![fail_no_output()])
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
            (waiting("spk", t, false), vec![])
        );
        assert_eq!(
            decide_output(&w, E::NoDefault, &env(t)),
            (S::closed(), vec![fail_no_output()])
        );
        for ev in [E::Stop, E::IdleClose] {
            assert_eq!(
                decide_output(&w, ev.clone(), &env(t)),
                (S::closed(), vec![]),
                "{ev:?}"
            );
        }
        for ev in [E::DeviceGone, E::Reopen, E::Dropped, E::Pause] {
            assert_eq!(
                decide_output(&w, ev.clone(), &env(t)),
                (w.clone(), vec![]),
                "{ev:?}"
            );
        }
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
