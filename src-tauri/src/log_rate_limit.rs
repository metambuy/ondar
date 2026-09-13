//! A rate limit for one known-pathological log target.
//!
//! `stream-download` 0.24.4 can enter a tight retry loop that logs at ERROR on every
//! iteration — measured at 1,585,143 lines / 382 MB in a 40 s run against a server that 416s
//! a retried range request. The engine watchdog now bounds how long that can run, but within
//! its window the rate is unchanged, so the flood still needs bounding on its own.
//!
//! An `EnvFilter` directive cannot do this. A directive sets a target's *maximum* level, so
//! the only setting that suppresses an ERROR flood is `off` — which also discards the first
//! occurrence. That line matters: `stream-download` swallows the underlying error and never
//! returns it to the decode thread, so Ondar cannot report the cause itself. It is the only
//! evidence of *why* a stream died, and dropping it is a real loss.
//!
//! So: keep the first few per second, drop the rest, and say how many were dropped.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Filter};

/// The one target this limits. Deliberately not configurable — a general knob would invite
/// hiding floods rather than fixing them.
const TARGET: &str = "stream_download::source";
/// Events allowed through per window.
const BURST: u32 = 20;
const WINDOW: Duration = Duration::from_secs(1);

struct State {
    window_start: Instant,
    allowed: u32,
    suppressed: u64,
}

impl State {
    /// Returns whether to admit this event, and any suppression count owed from the window
    /// that just closed. Pure apart from the clock, which is passed in, so it is testable.
    fn admit(&mut self, now: Instant) -> (bool, Option<u64>) {
        let mut report = None;
        if now.duration_since(self.window_start) >= WINDOW {
            if self.suppressed > 0 {
                report = Some(self.suppressed);
            }
            self.window_start = now;
            self.allowed = 0;
            self.suppressed = 0;
        }
        let allow = self.allowed < BURST;
        if allow {
            self.allowed += 1;
        } else {
            self.suppressed += 1;
        }
        (allow, report)
    }
}

pub struct RateLimit {
    state: Mutex<State>,
}

impl RateLimit {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                window_start: Instant::now(),
                allowed: 0,
                suppressed: 0,
            }),
        }
    }
}

impl<S: Subscriber> Filter<S> for RateLimit {
    /// Level filtering is `EnvFilter`'s job; this filter only ever decides per-event, so
    /// callsite interest must stay unconditional.
    fn enabled(&self, _meta: &tracing::Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        true
    }

    fn event_enabled(&self, event: &Event<'_>, _cx: &Context<'_, S>) -> bool {
        if event.metadata().target() != TARGET {
            return true;
        }
        // The lock is released before the report is emitted: that emit re-enters the
        // subscriber, and holding the lock across it would deadlock against this same method.
        let (allow, report) = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.admit(Instant::now())
        };
        if let Some(n) = report {
            tracing::warn!(target: "ondar", "log rate limit: suppressed {n} `{TARGET}` events");
        }
        allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(now: Instant) -> State {
        State {
            window_start: now,
            allowed: 0,
            suppressed: 0,
        }
    }

    /// Admits `BURST` events per window, then suppresses and counts the rest.
    ///
    /// **The name is deliberate and is doing a second job.** A bare `cargo test` runs only this
    /// crate — `src-tauri/Cargo.toml` has a real `[package]` at the workspace root, so cargo
    /// does not default to every member — which skips all 48 tests in `ondar-audio` while
    /// exiting 0. It used to report `0 passed`, which at least looked empty; since this crate
    /// gained tests it reports `3 passed`, which reads like a successful run. The three names
    /// are the only thing a bare run prints, so one of them says what happened. See CLAUDE.md.
    #[test]
    fn bare_cargo_test_runs_only_the_shell_crate_see_claude_md() {
        let now = Instant::now();
        let mut st = state(now);
        for i in 0..BURST {
            assert_eq!(st.admit(now), (true, None), "event {i} should pass");
        }
        assert_eq!(st.admit(now), (false, None));
        assert_eq!(st.suppressed, 1);
    }

    #[test]
    fn window_rollover_resets_and_reports_suppressed() {
        let now = Instant::now();
        let mut st = state(now);
        for _ in 0..BURST + 5 {
            st.admit(now);
        }
        assert_eq!(st.suppressed, 5);

        // First event of the next window passes and carries the closed window's count.
        let (allow, report) = st.admit(now + WINDOW);
        assert!(allow);
        assert_eq!(report, Some(5));

        // The count is not reported twice.
        assert_eq!(st.admit(now + WINDOW), (true, None));
    }

    /// End-to-end: proves the `Filter` is actually consulted by a real layered subscriber,
    /// which unit-testing `State::admit` alone would not establish.
    #[test]
    fn filter_suppresses_only_its_target_in_a_real_subscriber() {
        use std::io;
        use std::sync::{Arc, Mutex as StdMutex};
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::prelude::*;

        #[derive(Clone, Default)]
        struct Buf(Arc<StdMutex<Vec<u8>>>);
        impl io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for Buf {
            type Writer = Buf;
            fn make_writer(&'a self) -> Buf {
                self.clone()
            }
        }

        let buf = Buf::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(buf.clone())
                .with_filter(RateLimit::new()),
        );

        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..100 {
                tracing::error!(target: "stream_download::source", "flood");
            }
            tracing::error!(target: "something::else", "kept");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            out.matches("flood").count(),
            BURST as usize,
            "the limited target should be capped at BURST per window"
        );
        assert_eq!(
            out.matches("kept").count(),
            1,
            "every other target must pass through untouched"
        );
    }
}
