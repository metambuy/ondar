//! The decoder build's clock (defect B, review fixes F1): what the decode thread knows about a
//! build, stamped where it happens, for the engine thread to read on every tick.
//!
//! Why (the `/code-review` of 2026-09-29, findings 1–3 and 6): the first bound timed a build
//! from `stream::open` and took "starved" from stream-download's `on_reconnect` count. The
//! count moves only on a reconnect that **completes** inside `retry_timeout` (0.24.4
//! `source/mod.rs:272–287`), so a hung reconnect read as a format failure; `open` returns before
//! the prefetch is met (`lib.rs:343`, published at `source/mod.rs:323`), so the time included a
//! prefetch sized from a user-entered bitrate; and on HLS (`retry_timeout` ≥ 55 s) the count
//! never moved at all. Here the build is timed from its **first byte** and "starved" is read
//! from the bytes' **arrival**.
//!
//! - [`ClockedReader`] sits at the bottom of the decoder's chain, under `IcyReader`, on both
//!   source kinds, and stamps the first byte the decoder receives. That starts the format clock.
//! - [`Arrivals`] (review 2, G1) stamps network arrival: the gaps between the chunks the
//!   download task writes, from stream-download's `Settings::on_progress` ([`on_progress`]).
//!   A read is **not** an arrival: stream-download returns a read only once the whole requested
//!   block has arrived (0.24.4 `lib.rs:556–577`), and Symphonia asks for up to 32 KiB, so a
//!   per-read "gap" was the block's fill time — 8.2 s at 32 kbit/s — and every unsyncable build
//!   below ~52 kbit/s read `Starved` (review 2, finding 1). One `Arrivals` per open, written only
//!   by that open's download task: a previous open's task, which can write one more chunk after
//!   its reader is dropped, writes only its own, which nothing reads.
//! - [`BuildClock::begin_build`] stamps each build's start, installs its open's `Arrivals`, and
//!   publishes both with the phase, in one word that also holds a build sequence number. The
//!   engine keeps no build state of its own, so a build can never read another build's clock.
//! - The engine's bound is one compare-and-swap from the exact word it read to a `BOUND_*`
//!   phase that **is** the cause; the decode thread reads the cause from the swap it loses, and
//!   with it the gap and reconnect count the engine decided on (review 2, finding 4).
//!
//! Orderings: the word is Release/Acquire and publishes the build's reset stamps and its
//! `Arrivals`; `first_byte_ms` is Release/Acquire; an `Arrivals`' `last_ms` is Release/Acquire
//! and publishes its `max_gap_ms`, stored before it and loaded after it; the decided figures are
//! stored before the engine's winning swap (AcqRel) and loaded after the decode thread's losing
//! one (Acquire); everything else is `Relaxed`. Any remaining staleness makes a gap longer,
//! never shorter, so it errs towards `Network`, the non-terminal cause.

use std::io::{self, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stream_download::{StreamPhase, StreamState};
use tokio_util::sync::CancellationToken;

use crate::stream;

/// The format bound's floor: how long a build may run on bytes that keep arriving, counted from
/// the first byte the decoder received. 4× the healthy maximum measured in Step 0 (5.05 s,
/// n = 32). See [`BuildBounds::for_prefetch`] for the whole rule and what fails it.
pub(crate) const FORMAT_BOUND: Duration = Duration::from_secs(20);

/// The no-bytes bound's floor: how long a build may wait for its first byte (the prefetch).
pub(crate) const NO_BYTES_BOUND: Duration = Duration::from_secs(60);

/// The lowest recorded bitrate, 10 kbit/s, as bytes per second (the M3 Step 0 census: 2 of
/// 20 658 records with a bitrate; none below). The no-bytes bound covers the prefetch at it.
const SLOWEST_BYTES_PER_SEC: u64 = 1_250;

/// The three durations a build is bounded by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuildBounds {
    /// From the first byte: bytes arrived and nothing synced.
    pub format: Duration,
    /// A gap between network arrivals this long, or a completed internal reconnect, makes the format
    /// bound's cause the network.
    pub starved: Duration,
    /// From the build's start with no byte: the prefetch was never met. Always the network.
    pub no_bytes: Duration,
}

impl BuildBounds {
    /// The production bounds for a session whose prefetch is `prefetch_bytes`:
    ///
    /// - **`format` = max(20 s, 3 × `retry_timeout`)** from the first byte — 20 s at the
    ///   defaults. At least 3 × `retry_timeout` so a silent server's re-feed shows reconnects
    ///   before the bound and a gap of `starved` fits inside it, under `ONDAR_RETRY_TIMEOUT_SECS`
    ///   too. **What fails it:** a healthy stream whose first sync needs more than 20 s of its
    ///   own bytes after the prefetch — 40 KB at 16 kbit/s; the largest healthy pull in Step 0
    ///   was 64 512 B (OGG, from a burst).
    /// - **`starved` = `retry_timeout`** (5 s), both source kinds, on network **arrival**
    ///   ([`Arrivals`]; review 2, G1): stream-download's own "this connection is unhealthy"
    ///   threshold — it reconnects when no chunk comes within it, so a completed reconnect always
    ///   follows an arrival gap at least this long. On HLS a 5 s gap between segments is normal,
    ///   so an HLS build unsynced at the bound reads `Network` — right, since HLS format failures
    ///   are refused before the build (M3c's sniff). **What fails it:** an HTTP stream whose
    ///   bytes stop arriving for ≥ 5 s during its build and that never syncs backs off instead
    ///   of ending, and ends as `Error { Network }` after five attempts: the wrong code, never a
    ///   wrong terminal. A download task blocked 5 s on a full buffer would read the same, but
    ///   during a build the probe reads continuously and the prefetch (≤ 128 KiB) is below the
    ///   256 KiB buffer.
    /// - **`no_bytes` = max(60 s, ⌈1.1 × prefetch ÷ 1 250 B/s⌉)**: the prefetch's time at
    ///   10 kbit/s plus a 10 % margin, rounded up to the millisecond — 60 s at the 32 768 B floor,
    ///   70.4 s for a 320 kbit/s record, 115.344 s at the 131 072 B ceiling. The margin is the
    ///   connection's latency and the first tick after the bound (review 2, finding 3: without
    ///   it, a stream at exactly 10 kbit/s met its prefetch 0.6 ms after the bound, and the
    ///   integer division cut the bound itself). **What fails it:** a stream with no burst on
    ///   connect below 4.8 kbit/s at the floor, or below 9.1 kbit/s with a ceiling record, or at
    ///   exactly 10 kbit/s with more than ~10 s of connect latency; none is recorded. It is always
    ///   `Network`: a trickle cannot be told from a slow healthy stream (decided 2026-09-29, D1:
    ///   a body stalled before its prefetch takes 60–105 s per attempt; `main` never ended).
    pub(crate) fn for_prefetch(prefetch_bytes: u64) -> Self {
        let retry = stream::retry_timeout();
        // ms = ⌈prefetch × 1 000 × 1.1 ÷ 1 250⌉, in integers.
        let at_slowest = Duration::from_millis(
            prefetch_bytes
                .saturating_mul(11_000)
                .div_ceil(SLOWEST_BYTES_PER_SEC * 10),
        );
        Self {
            format: (retry * 3).max(FORMAT_BOUND),
            starved: retry,
            no_bytes: at_slowest.max(NO_BYTES_BOUND),
        }
    }
}

/// Why the engine bounded a build: the phase it swapped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildCause {
    /// Bytes kept arriving for the whole format bound and nothing synced.
    Format,
    /// The format bound passed with a gap of `starved` or more, or a completed reconnect.
    Starved,
    /// No byte reached the decoder within the no-bytes bound.
    NoBytes,
}

impl BuildCause {
    fn phase(self) -> u8 {
        match self {
            BuildCause::Format => phase::BOUND_FORMAT,
            BuildCause::Starved => phase::BOUND_STARVED,
            BuildCause::NoBytes => phase::BOUND_NO_BYTES,
        }
    }
}

/// The phases in the low byte of [`BuildClock`]'s word.
pub(crate) mod phase {
    /// No build yet in this session.
    pub const IDLE: u8 = 0;
    /// `build()` is running on an open stream; the engine checks the bounds each tick.
    pub const PROBING: u8 = 1;
    /// `build()` returned first: the decode thread owns what it got.
    pub const BUILT: u8 = 2;
    /// The engine bounded the build; the cause is the phase. The download is cancelled and
    /// whatever `build()` returns is dropped.
    pub const BOUND_FORMAT: u8 = 3;
    pub const BOUND_STARVED: u8 = 4;
    pub const BOUND_NO_BYTES: u8 = 5;
}

fn pack(seq: u64, phase: u8) -> u64 {
    (seq << 8) | u64::from(phase)
}

/// The phase in a word.
pub(crate) fn phase_of(word: u64) -> u8 {
    (word & 0xFF) as u8
}

/// The build sequence number in a word.
pub(crate) fn seq_of(word: u64) -> u64 {
    word >> 8
}

/// What the engine reads of a build on one tick, for `decide_tick`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuildInputs {
    /// Since `begin_build`.
    pub since_start: Duration,
    /// Since the first byte the decoder received; `None` before it.
    pub since_first_byte: Option<Duration>,
    /// The longest gap between network arrivals on this build's open, the open one (since the
    /// last arrival) included, the first counted from the download's start; zero before a
    /// build has begun.
    pub longest_gap: Duration,
    /// Completed internal reconnects during the build.
    pub reconnects: u64,
    pub bounds: BuildBounds,
}

/// Network arrival on one open (review 2, G1; see the module doc): written only by that open's
/// download task through [`on_progress`], read by the engine through [`BuildClock::inputs`].
/// Created by the decode thread once per `stream::open`; never reset — a fresh one per open is
/// what keeps the prefetch's arrivals (which can land before `begin_build`) and keeps a
/// previous open's last stamp out (which would read the backoff as a gap).
pub(crate) struct Arrivals {
    origin: Instant,
    /// The longest gap between arrivals so far, in ms; published by `last_ms`.
    max_gap_ms: AtomicU64,
    /// The last arrival, in ms since `origin` plus one; 0 is "none yet".
    last_ms: AtomicU64,
}

impl Arrivals {
    pub(crate) fn new() -> Self {
        Self {
            origin: Instant::now(),
            max_gap_ms: AtomicU64::new(0),
            last_ms: AtomicU64::new(0),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1)
    }

    /// The longest gap, the open one included: `last_ms` first (Acquire), then `max_gap_ms`,
    /// then now. With no arrival yet the open gap runs from `origin`, the open's start — which
    /// cannot happen once the decoder has a first byte, and errs towards `Network` if it did.
    fn longest_gap(&self) -> Duration {
        let last = self.last_ms.load(Ordering::Acquire);
        let max_gap = self.max_gap_ms.load(Ordering::Relaxed);
        let open = self
            .now_ms()
            .saturating_sub(if last == 0 { 1 } else { last });
        Duration::from_millis(max_gap.max(open))
    }

    /// Whether any chunk has arrived (a test's check that an open is wired).
    #[cfg(test)]
    pub(crate) fn any(&self) -> bool {
        self.last_ms.load(Ordering::Acquire) != 0
    }
}

/// The download task's side of [`Arrivals`]: the gap state as plain fields (the callback is
/// `FnMut`, called with `&mut` from the one download task, never concurrently), published to
/// the atomics on every arrival.
pub(crate) struct ArrivalWriter {
    arrivals: Arc<Arrivals>,
    /// The previous arrival's `elapsed`; `None` before the first.
    prev: Option<Duration>,
    max_gap: Duration,
}

impl ArrivalWriter {
    pub(crate) fn new(arrivals: Arc<Arrivals>) -> Self {
        Self {
            arrivals,
            prev: None,
            max_gap: Duration::ZERO,
        }
    }

    /// A chunk of `bytes` was written at `elapsed` on the download's clock (stream-download's
    /// `StreamState::elapsed`, from the download's start, continuous across its internal
    /// reconnects). 0 bytes stamps nothing. The first arrival's gap is `elapsed` itself: a wait
    /// from the start is a gap too, as stream-download reconnects on it at `retry_timeout`.
    pub(crate) fn arrive(&mut self, elapsed: Duration, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let gap = elapsed.saturating_sub(self.prev.unwrap_or(Duration::ZERO));
        self.prev = Some(elapsed);
        if gap > self.max_gap {
            self.max_gap = gap;
            self.arrivals.max_gap_ms.store(
                u64::try_from(gap.as_millis()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        // Release: an engine that loads this stamp sees the gap stored above.
        self.arrivals
            .last_ms
            .store(self.arrivals.now_ms(), Ordering::Release);
    }
}

/// The `Settings::on_progress` callback for one open (0.24.4 `settings.rs:149`, called by the
/// download task after each chunk is written, `source/mod.rs:477`): the chunk's size from the
/// `Prefetching` and `Downloading` phases, 0 (nothing) from `Complete` and any later phase.
pub(crate) fn on_progress<S>(
    arrivals: Arc<Arrivals>,
) -> impl FnMut(&S, StreamState, &CancellationToken) + Send + Sync + 'static {
    let mut writer = ArrivalWriter::new(arrivals);
    move |_stream: &S, state: StreamState, _token: &CancellationToken| {
        let bytes = match state.phase {
            StreamPhase::Prefetching { chunk_size, .. }
            | StreamPhase::Downloading { chunk_size, .. } => chunk_size,
            _ => 0,
        };
        writer.arrive(state.elapsed, bytes);
    }
}

/// A build the engine bounded, as the decode thread learns it from the swap it lost: the cause,
/// and the longest gap and reconnect count the engine decided on — the figures its log line
/// printed, not a later clock's (review 2, finding 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Bounded {
    pub cause: BuildCause,
    pub longest_gap: Duration,
    pub reconnects: u64,
}

/// One session's build clock: the phase word and the stamps (see the module doc). Shared by the
/// decode thread, which writes it, and the engine thread, which reads it and swaps the word.
pub(crate) struct BuildClock {
    origin: Instant,
    bounds: BuildBounds,
    /// `seq << 8 | phase`. The decode thread stores a new seq with `PROBING` at each build's
    /// start and swaps `PROBING → BUILT` when `build()` returns; the engine swaps
    /// `PROBING → BOUND_*` from the word it read. Nothing else writes it.
    word: AtomicU64,
    /// The stamps are milliseconds since `origin` plus one, so 0 is "none".
    start_ms: AtomicU64,
    /// `reconnect_count` at the build's start.
    reconnect_base: AtomicU64,
    first_byte_ms: AtomicU64,
    /// The build's open's arrivals, installed by `begin_build` (decode thread, the only writer).
    arrivals: Mutex<Option<Arc<Arrivals>>>,
    /// What the engine decided a bound on, stored before its swap (engine, the only writer).
    decided_gap_ms: AtomicU64,
    decided_reconnects: AtomicU64,
}

impl BuildClock {
    pub(crate) fn new(bounds: BuildBounds) -> Self {
        Self {
            origin: Instant::now(),
            bounds,
            word: AtomicU64::new(pack(0, phase::IDLE)),
            start_ms: AtomicU64::new(0),
            reconnect_base: AtomicU64::new(0),
            first_byte_ms: AtomicU64::new(0),
            arrivals: Mutex::new(None),
            decided_gap_ms: AtomicU64::new(0),
            decided_reconnects: AtomicU64::new(0),
        }
    }

    /// Now, as a stamp: milliseconds since the session's origin, plus one.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1)
    }

    /// The word, for the engine's tick (Acquire: publishes the build's reset stamps).
    pub(crate) fn word(&self) -> u64 {
        self.word.load(Ordering::Acquire)
    }

    /// A build begins (decode thread, after the download's token is in `SessionCtx::download`).
    /// Resets the build's stamps (start, reconnect base, first byte), installs this open's
    /// `arrivals` **without resetting them** — the prefetch's arrivals may already be there,
    /// and a fresh `Arrivals` per open holds nothing older — then publishes all of it with a new
    /// seq and `PROBING` (Release). A plain store is safe: the engine swaps only **from**
    /// `PROBING`, and here the word is `IDLE`, `BUILT` or `BOUND_*`, so no engine write can land
    /// in between. Returns the new word.
    pub(crate) fn begin_build(&self, reconnect_count: u64, arrivals: Arc<Arrivals>) -> u64 {
        self.start_ms.store(self.now_ms(), Ordering::Relaxed);
        self.reconnect_base
            .store(reconnect_count, Ordering::Relaxed);
        self.first_byte_ms.store(0, Ordering::Relaxed);
        *self.arrivals.lock().unwrap() = Some(arrivals);
        let word = pack(
            seq_of(self.word.load(Ordering::Relaxed)).wrapping_add(1),
            phase::PROBING,
        );
        self.word.store(word, Ordering::Release);
        word
    }

    /// `build()` returned (decode thread): swap `PROBING → BUILT` from `word`, the one
    /// `begin_build` returned. `Err` means the engine bounded the build first, with the cause
    /// the phase names and the figures it decided on (loaded after the losing swap's Acquire;
    /// no later decision can overwrite them first, since only this thread begins a build).
    pub(crate) fn finish_build(&self, word: u64) -> Result<(), Bounded> {
        match self.word.compare_exchange(
            word,
            pack(seq_of(word), phase::BUILT),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(now) => Err(Bounded {
                cause: match phase_of(now) {
                    phase::BOUND_FORMAT => BuildCause::Format,
                    phase::BOUND_NO_BYTES => BuildCause::NoBytes,
                    // `BOUND_STARVED`; and, never written by anyone, any other phase — read as
                    // the non-terminal cause rather than a panic.
                    _ => BuildCause::Starved,
                },
                longest_gap: Duration::from_millis(self.decided_gap_ms.load(Ordering::Relaxed)),
                reconnects: self.decided_reconnects.load(Ordering::Relaxed),
            }),
        }
    }

    /// The engine bounds a build: swap from the **exact** `word` its tick read to the cause.
    /// Fails if the decode thread swapped first (`BUILT`) or a new build began since (another
    /// seq), so a decision about one build cannot bound the next. `decided` is the tick's
    /// inputs: its gap and reconnect count are stored before the swap, for the decode thread's
    /// message (a failed swap leaves them unread).
    pub(crate) fn bound(&self, word: u64, cause: BuildCause, decided: &BuildInputs) -> bool {
        self.decided_gap_ms.store(
            u64::try_from(decided.longest_gap.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.decided_reconnects
            .store(decided.reconnects, Ordering::Relaxed);
        self.word
            .compare_exchange(
                word,
                pack(seq_of(word), cause.phase()),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// The build's open's arrivals, for a test's check that an open is wired.
    #[cfg(test)]
    pub(crate) fn arrivals(&self) -> Option<Arc<Arrivals>> {
        self.arrivals.lock().unwrap().clone()
    }

    /// The build as it stands, for `decide_tick` (engine). The stamps first, then the
    /// arrivals (their own lock, taken and released here: never nested with `download`), and
    /// `now` last, so it is never before a stamp it is compared with (the subtractions saturate
    /// regardless).
    pub(crate) fn inputs(&self, reconnect_count: u64) -> BuildInputs {
        let first = self.first_byte_ms.load(Ordering::Acquire);
        let start = self.start_ms.load(Ordering::Relaxed);
        let base = self.reconnect_base.load(Ordering::Relaxed);
        let longest_gap = self
            .arrivals
            .lock()
            .unwrap()
            .as_ref()
            .map_or(Duration::ZERO, |a| a.longest_gap());
        let now = self.now_ms();
        let ms = |m: u64| Duration::from_millis(m);
        BuildInputs {
            since_start: ms(now.saturating_sub(start)),
            since_first_byte: (first != 0).then(|| ms(now.saturating_sub(first))),
            longest_gap,
            reconnects: reconnect_count.saturating_sub(base),
            bounds: self.bounds,
        }
    }
}

/// The decoder's first-byte reader (see the module doc): `inner` unchanged, and the first read
/// that returns bytes stamped into the clock — the format clock's start. A read of 0 bytes (end
/// of stream) or an `Err` stamps nothing. The gaps are not taken here: a read is not an arrival
/// (review 2, finding 1; [`Arrivals`]). One per open, moved into the decoder's chain and dropped
/// with it before the next open, so one reader writes a session's first-byte stamp at a time.
pub(crate) struct ClockedReader<R: Read> {
    inner: R,
    clock: Arc<BuildClock>,
    /// The prefetch this open waits for, for the first-byte line.
    prefetch_bytes: u64,
    stamped: bool,
}

impl<R: Read> ClockedReader<R> {
    pub(crate) fn new(inner: R, clock: Arc<BuildClock>, prefetch_bytes: u64) -> Self {
        Self {
            inner,
            clock,
            prefetch_bytes,
            stamped: false,
        }
    }

    fn stamp(&mut self) {
        self.stamped = true;
        let now = self.clock.now_ms();
        self.clock.first_byte_ms.store(now, Ordering::Release);
        let start = self.clock.start_ms.load(Ordering::Relaxed);
        log::info!(
            "build: first byte {:.3} s after open (prefetch {} B)",
            Duration::from_millis(now.saturating_sub(start)).as_secs_f64(),
            self.prefetch_bytes
        );
    }
}

impl<R: Read> Read for ClockedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 && !self.stamped {
            self.stamp();
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::thread;

    use super::*;

    fn bounds() -> BuildBounds {
        BuildBounds {
            format: Duration::from_secs(20),
            starved: Duration::from_secs(5),
            no_bytes: Duration::from_secs(60),
        }
    }

    /// Fails if the seq and the phase share bits, or the phase byte is misread.
    #[test]
    fn pack_and_unpack() {
        for seq in [0, 1, 255, 256, u64::MAX >> 8] {
            for p in [
                phase::IDLE,
                phase::PROBING,
                phase::BUILT,
                phase::BOUND_FORMAT,
                phase::BOUND_STARVED,
                phase::BOUND_NO_BYTES,
            ] {
                let w = pack(seq, p);
                assert_eq!((seq_of(w), phase_of(w)), (seq, p));
            }
        }
    }

    /// The production bounds: the no-bytes bound at the floor, a 320 kbit/s record and the
    /// ceiling; fails on a missing `max` (the floor's 28.8 s) or a wrong rate.
    #[test]
    fn the_production_bounds() {
        let floor = BuildBounds::for_prefetch(stream::PREFETCH_FLOOR_BYTES);
        assert_eq!(floor.no_bytes, Duration::from_secs(60));
        assert_eq!(
            BuildBounds::for_prefetch(80_000).no_bytes,
            Duration::from_millis(70_400)
        );
        assert_eq!(
            BuildBounds::for_prefetch(stream::PREFETCH_CEILING_BYTES).no_bytes,
            Duration::from_millis(115_344)
        );
        assert!(
            BuildBounds::for_prefetch(u64::MAX).no_bytes > NO_BYTES_BOUND,
            "saturates"
        );
        assert_eq!(floor.starved, stream::retry_timeout());
        assert_eq!(
            floor.format,
            (stream::retry_timeout() * 3).max(FORMAT_BOUND)
        );
    }

    /// The no-bytes bound at exactly 10 kbit/s (review 2, finding 3): at the floor, a 320 kbit/s
    /// record and the ceiling, the bound covers the prefetch's time at 1 250 B/s with a 10 %
    /// margin, rounded up. Fails on `688c9fd`, where the ceiling's 104 857 ms (integer division)
    /// is below the 104 857.6 ms the prefetch takes; and with the margin's factor at 1.0.
    #[test]
    fn the_no_bytes_bound_keeps_a_margin_at_10_kbit() {
        for (prefetch, pinned) in [
            (stream::PREFETCH_CEILING_BYTES, 115_344),
            (80_000, 70_400),
            (stream::PREFETCH_FLOOR_BYTES, 60_000),
        ] {
            let no_bytes = BuildBounds::for_prefetch(prefetch).no_bytes;
            let at_10_kbit = prefetch as f64 / SLOWEST_BYTES_PER_SEC as f64;
            assert!(
                no_bytes.as_secs_f64() >= 1.1 * at_10_kbit,
                "{prefetch} B: {no_bytes:?} < 1.1 × {at_10_kbit} s"
            );
            assert_eq!(no_bytes, Duration::from_millis(pinned), "{prefetch} B");
        }
    }

    /// A reader that returns `chunks` in turn: bytes, an empty read, an error.
    struct Script(Vec<io::Result<usize>>);

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Ok(0);
            }
            match self.0.remove(0) {
                Ok(n) => {
                    let n = n.min(buf.len());
                    buf[..n].fill(0xAA);
                    Ok(n)
                }
                Err(e) => Err(e),
            }
        }
    }

    /// The decoder's first read that returns bytes stamps the first byte, once: a 0-byte read
    /// or an `Err` stamps nothing, and a later read does not move it. Fails if `n == 0` stamps
    /// (a first byte from the end of stream), if an `Err` is swallowed, or if every read
    /// re-stamps (the format clock would restart on each read).
    #[test]
    fn the_reader_stamps_only_the_first_byte() {
        let clock = Arc::new(BuildClock::new(bounds()));
        clock.begin_build(0, Arc::new(Arrivals::new()));
        let mut r = ClockedReader::new(
            Script(vec![
                Ok(0),
                Err(io::Error::other("x")),
                Ok(10),
                Ok(0),
                Err(io::Error::other("y")),
                Ok(5),
            ]),
            clock.clone(),
            0,
        );
        let mut buf = [0u8; 64];
        assert_eq!(r.read(&mut buf).unwrap(), 0);
        assert!(r.read(&mut buf).is_err());
        assert_eq!(clock.inputs(0).since_first_byte, None);
        assert_eq!(clock.first_byte_ms.load(Ordering::Relaxed), 0);

        assert_eq!(r.read(&mut buf).unwrap(), 10);
        let first = clock.first_byte_ms.load(Ordering::Relaxed);
        assert_ne!(first, 0);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(r.read(&mut buf).unwrap(), 0);
        assert!(r.read(&mut buf).is_err());
        assert_eq!(r.read(&mut buf).unwrap(), 5);
        assert_eq!(
            clock.first_byte_ms.load(Ordering::Relaxed),
            first,
            "moved by a later read"
        );
    }

    /// Arrival gaps (review 2, G1), on the download's clock: 0 bytes stamps nothing; the first
    /// gap is `elapsed` itself (the wait from the download's start); a recorded gap is kept
    /// after arrivals resume; the open gap counts. Fails if a 0-byte chunk stamps, if the first
    /// gap is dropped, if only the open gap is read (a stall that resumed reads as zero), or if
    /// the open gap is not read (a hung reconnect reads as zero).
    #[test]
    fn arrivals_record_gaps_and_the_open_gap_counts() {
        let a = Arc::new(Arrivals::new());
        let mut w = ArrivalWriter::new(a.clone());
        w.arrive(Duration::from_secs(9), 0);
        assert!(!a.any(), "0 bytes stamped");
        assert_eq!(a.max_gap_ms.load(Ordering::Relaxed), 0);

        w.arrive(Duration::from_millis(700), 1_024);
        assert!(a.any());
        assert_eq!(a.max_gap_ms.load(Ordering::Relaxed), 700, "the first gap");
        w.arrive(Duration::from_millis(760), 1_024);
        w.arrive(Duration::from_millis(1_960), 1_024);
        w.arrive(Duration::from_millis(1_990), 1_024);
        assert_eq!(
            a.max_gap_ms.load(Ordering::Relaxed),
            1_200,
            "kept after resuming"
        );
        assert!(a.longest_gap() >= Duration::from_millis(1_200));

        let b = Arc::new(Arrivals::new());
        let mut w = ArrivalWriter::new(b.clone());
        w.arrive(Duration::from_millis(1), 1);
        w.arrive(Duration::from_millis(2), 1);
        thread::sleep(Duration::from_millis(250));
        let open = b.longest_gap();
        assert!(open >= Duration::from_millis(250), "{open:?}");
    }

    /// `begin_build` resets the build's stamps and moves the seq, installs the open's arrivals
    /// **as they are** — the prefetch's arrivals, stamped before it, are kept — and a new open's
    /// arrivals hold nothing of the previous one's. Fails if `begin_build` resets the arrivals
    /// (the 2 s prefetch gap is lost), if the first byte survives into the next build, or if the
    /// arrivals are one slot per session (the first open's 9 s gap reaches the second).
    #[test]
    fn begin_build_keeps_the_opens_arrivals_and_resets_the_rest() {
        let clock = Arc::new(BuildClock::new(bounds()));
        let first_open = Arc::new(Arrivals::new());
        ArrivalWriter::new(first_open.clone()).arrive(Duration::from_secs(9), 1);
        let w1 = clock.begin_build(3, first_open);
        let mut r = ClockedReader::new(Cursor::new(vec![0u8; 8]), clock.clone(), 0);
        let mut b = [0u8; 4];
        r.read_exact(&mut b).unwrap();
        let i = clock.inputs(5);
        assert!(i.since_first_byte.is_some());
        assert_eq!(i.reconnects, 2);
        assert!(
            i.longest_gap >= Duration::from_secs(9),
            "{:?}",
            i.longest_gap
        );
        assert_eq!(clock.finish_build(w1), Ok(()));

        let second_open = Arc::new(Arrivals::new());
        ArrivalWriter::new(second_open.clone()).arrive(Duration::from_secs(2), 1);
        let w2 = clock.begin_build(5, second_open);
        assert_eq!(seq_of(w2), seq_of(w1) + 1);
        assert_eq!(phase_of(clock.word()), phase::PROBING);
        let i = clock.inputs(5);
        assert_eq!(i.since_first_byte, None);
        assert_eq!(i.reconnects, 0);
        assert!(
            i.longest_gap >= Duration::from_secs(2) && i.longest_gap < Duration::from_secs(9),
            "{:?}",
            i.longest_gap
        );
    }

    fn decided(gap_ms: u64, reconnects: u64) -> BuildInputs {
        BuildInputs {
            since_start: Duration::ZERO,
            since_first_byte: None,
            longest_gap: Duration::from_millis(gap_ms),
            reconnects,
            bounds: bounds(),
        }
    }

    /// One swap wins. The engine's bound from a stale word (an earlier build's seq) fails and
    /// leaves the new build probing; the decode thread's swap after a bound reads the cause,
    /// with the gap and reconnect count that bound was decided on (review 2, finding 4). Fails
    /// if the swap compares the phase alone, or if the figures are not the winning bound's.
    #[test]
    fn a_swap_from_a_stale_word_fails() {
        let clock = BuildClock::new(bounds());
        let a = || Arc::new(Arrivals::new());
        let n = clock.begin_build(0, a());
        assert!(clock.bound(n, BuildCause::NoBytes, &decided(0, 0)));
        assert_eq!(
            clock.finish_build(n),
            Err(Bounded {
                cause: BuildCause::NoBytes,
                longest_gap: Duration::ZERO,
                reconnects: 0
            })
        );
        let n1 = clock.begin_build(0, a());
        assert!(
            !clock.bound(n, BuildCause::Format, &decided(1, 1)),
            "N's word bounded N+1"
        );
        assert_eq!(clock.word(), n1);
        assert_eq!(clock.finish_build(n1), Ok(()));
        assert!(
            !clock.bound(n1, BuildCause::Format, &decided(1, 1)),
            "bounded after BUILT"
        );
        let n2 = clock.begin_build(0, a());
        assert!(clock.bound(n2, BuildCause::Starved, &decided(5_400, 3)));
        assert_eq!(
            clock.finish_build(n2),
            Err(Bounded {
                cause: BuildCause::Starved,
                longest_gap: Duration::from_millis(5_400),
                reconnects: 3
            })
        );
        let n3 = clock.begin_build(0, a());
        assert!(clock.bound(n3, BuildCause::Format, &decided(80, 0)));
        assert_eq!(
            clock.finish_build(n3).map_err(|b| b.cause),
            Err(BuildCause::Format)
        );
    }
}
