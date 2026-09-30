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
//! from the bytes themselves.
//!
//! - [`ClockedReader`] sits at the bottom of the decoder's chain, under `IcyReader`, on both
//!   source kinds, and stamps the first byte, the longest gap between two reads that returned
//!   bytes, and the last byte.
//! - [`BuildClock::begin_build`] stamps each build's start and publishes it with the phase, in
//!   one word that also holds a build sequence number. The engine keeps no build state of its
//!   own, so a build can never read another build's clock.
//! - The engine's bound is one compare-and-swap from the exact word it read to a `BOUND_*`
//!   phase that **is** the cause; the decode thread reads the cause from the swap it loses.
//!
//! Orderings (b-fix-plan.md § 1.2): the word is Release/Acquire and publishes the build's reset
//! stamps; `last_byte_ms` is Release/Acquire and publishes `first_byte_ms` and `max_gap_ms`,
//! which are stored before it and loaded after it; everything else is `Relaxed`. Any remaining
//! staleness errs towards `Network`, the non-terminal cause.

use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
    /// A gap between bytes this long, or a completed internal reconnect, makes the format
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
    /// - **`starved` = `retry_timeout`** (5 s), both source kinds: stream-download's own "this
    ///   connection is unhealthy" threshold. On HLS a 5 s gap between segments is normal, so an
    ///   HLS build unsynced at the bound reads `Network` — right, since HLS format failures are
    ///   refused before the build (M3c's sniff). **What fails it:** an HTTP stream that pauses
    ///   ≥ 5 s during its build and never syncs backs off instead of ending, and ends as
    ///   `Error { Network }` after five attempts: the wrong code, never a wrong terminal.
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
    /// The longest gap between bytes during the build, the open one (since the last byte)
    /// included; zero before the first byte.
    pub longest_gap: Duration,
    /// Completed internal reconnects during the build.
    pub reconnects: u64,
    pub bounds: BuildBounds,
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
    max_gap_ms: AtomicU64,
    last_byte_ms: AtomicU64,
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
            max_gap_ms: AtomicU64::new(0),
            last_byte_ms: AtomicU64::new(0),
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
    /// Resets the stamps, then publishes them with a new seq and `PROBING` (Release). A plain
    /// store is safe: the engine swaps only **from** `PROBING`, and here the word is `IDLE`,
    /// `BUILT` or `BOUND_*`, so no engine write can land in between. Returns the new word.
    pub(crate) fn begin_build(&self, reconnect_count: u64) -> u64 {
        self.start_ms.store(self.now_ms(), Ordering::Relaxed);
        self.reconnect_base
            .store(reconnect_count, Ordering::Relaxed);
        self.first_byte_ms.store(0, Ordering::Relaxed);
        self.max_gap_ms.store(0, Ordering::Relaxed);
        self.last_byte_ms.store(0, Ordering::Relaxed);
        let word = pack(
            seq_of(self.word.load(Ordering::Relaxed)).wrapping_add(1),
            phase::PROBING,
        );
        self.word.store(word, Ordering::Release);
        word
    }

    /// `build()` returned (decode thread): swap `PROBING → BUILT` from `word`, the one
    /// `begin_build` returned. `Err` means the engine bounded the build first, with the cause
    /// the phase names.
    pub(crate) fn finish_build(&self, word: u64) -> Result<(), BuildCause> {
        match self.word.compare_exchange(
            word,
            pack(seq_of(word), phase::BUILT),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(now) => Err(match phase_of(now) {
                phase::BOUND_FORMAT => BuildCause::Format,
                phase::BOUND_NO_BYTES => BuildCause::NoBytes,
                // `BOUND_STARVED`; and, never written by anyone, any other phase — read as the
                // non-terminal cause rather than a panic.
                _ => BuildCause::Starved,
            }),
        }
    }

    /// The engine bounds a build: swap from the **exact** `word` its tick read to the cause.
    /// Fails if the decode thread swapped first (`BUILT`) or a new build began since (another
    /// seq), so a decision about one build cannot bound the next.
    pub(crate) fn bound(&self, word: u64, cause: BuildCause) -> bool {
        self.word
            .compare_exchange(
                word,
                pack(seq_of(word), cause.phase()),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// The build as it stands, for `decide_tick` (engine) or the bound's message (decode
    /// thread). `last_byte_ms` is loaded first (Acquire), then the rest; `now` last, so it is
    /// never before a stamp it is compared with (the subtractions saturate regardless).
    pub(crate) fn inputs(&self, reconnect_count: u64) -> BuildInputs {
        let last = self.last_byte_ms.load(Ordering::Acquire);
        let first = self.first_byte_ms.load(Ordering::Relaxed);
        let max_gap = self.max_gap_ms.load(Ordering::Relaxed);
        let start = self.start_ms.load(Ordering::Relaxed);
        let base = self.reconnect_base.load(Ordering::Relaxed);
        let now = self.now_ms();
        let ms = |m: u64| Duration::from_millis(m);
        BuildInputs {
            since_start: ms(now.saturating_sub(start)),
            since_first_byte: (first != 0).then(|| ms(now.saturating_sub(first))),
            longest_gap: if last == 0 {
                Duration::ZERO
            } else {
                ms(max_gap.max(now.saturating_sub(last)))
            },
            reconnects: reconnect_count.saturating_sub(base),
            bounds: self.bounds,
        }
    }
}

/// The byte clock's reader (see the module doc): `inner` unchanged, each read that returns
/// bytes stamped into the clock. A read of 0 bytes (end of stream) or an `Err` stamps nothing.
/// One per open, moved into the decoder's chain and dropped with it before the next open, so
/// one reader writes a session's stamps at a time.
pub(crate) struct ClockedReader<R: Read> {
    inner: R,
    clock: Arc<BuildClock>,
    /// The prefetch this open waits for, for the first-byte line.
    prefetch_bytes: u64,
    /// This reader's last stamp (0: none yet) and longest gap, so a read needs no load.
    last_ms: u64,
    max_gap_ms: u64,
}

impl<R: Read> ClockedReader<R> {
    pub(crate) fn new(inner: R, clock: Arc<BuildClock>, prefetch_bytes: u64) -> Self {
        Self {
            inner,
            clock,
            prefetch_bytes,
            last_ms: 0,
            max_gap_ms: 0,
        }
    }

    fn stamp(&mut self) {
        let now = self.clock.now_ms();
        if self.last_ms == 0 {
            self.clock.first_byte_ms.store(now, Ordering::Relaxed);
            let start = self.clock.start_ms.load(Ordering::Relaxed);
            log::info!(
                "build: first byte {:.3} s after open (prefetch {} B)",
                Duration::from_millis(now.saturating_sub(start)).as_secs_f64(),
                self.prefetch_bytes
            );
        } else {
            let gap = now.saturating_sub(self.last_ms);
            if gap > self.max_gap_ms {
                self.max_gap_ms = gap;
                self.clock.max_gap_ms.store(gap, Ordering::Relaxed);
            }
        }
        self.last_ms = now;
        // Release: an engine that loads this stamp sees the first-byte and gap stores above.
        self.clock.last_byte_ms.store(now, Ordering::Release);
    }
}

impl<R: Read> Read for ClockedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
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

    /// No stamp on a 0-byte read or an `Err`, the first stamp on the first bytes, then gaps and
    /// the last stamp. Fails if `n == 0` stamps (a first byte from the end of stream), or if an
    /// `Err` is swallowed or stamps.
    #[test]
    fn the_reader_stamps_only_reads_that_return_bytes() {
        let clock = Arc::new(BuildClock::new(bounds()));
        clock.begin_build(0);
        let mut r = ClockedReader::new(
            Script(vec![
                Ok(0),
                Err(io::Error::other("x")),
                Ok(10),
                Ok(0),
                Err(io::Error::other("y")),
            ]),
            clock.clone(),
            0,
        );
        let mut buf = [0u8; 64];
        assert_eq!(r.read(&mut buf).unwrap(), 0);
        assert!(r.read(&mut buf).is_err());
        let i = clock.inputs(0);
        assert_eq!((i.since_first_byte, i.longest_gap), (None, Duration::ZERO));
        assert_eq!(clock.last_byte_ms.load(Ordering::Relaxed), 0);

        assert_eq!(r.read(&mut buf).unwrap(), 10);
        let first = clock.first_byte_ms.load(Ordering::Relaxed);
        assert_ne!(first, 0);
        assert_eq!(clock.last_byte_ms.load(Ordering::Relaxed), first);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(r.read(&mut buf).unwrap(), 0);
        assert!(r.read(&mut buf).is_err());
        assert_eq!(
            clock.last_byte_ms.load(Ordering::Relaxed),
            first,
            "0 or Err"
        );
        assert_eq!(clock.max_gap_ms.load(Ordering::Relaxed), 0, "0 or Err");
    }

    /// A recorded gap is kept after bytes resume, and the open gap counts too. Fails if only
    /// the open gap is read (a stall that resumed reads as zero).
    #[test]
    fn gaps_are_recorded_and_the_open_gap_counts() {
        let clock = Arc::new(BuildClock::new(bounds()));
        clock.begin_build(0);
        let mut r = ClockedReader::new(Cursor::new(vec![0u8; 64]), clock.clone(), 0);
        let mut b = [0u8; 8];
        r.read_exact(&mut b).unwrap();
        thread::sleep(Duration::from_millis(120));
        r.read_exact(&mut b).unwrap();
        let recorded = clock.max_gap_ms.load(Ordering::Relaxed);
        assert!(recorded >= 120, "{recorded}");
        assert!(clock.inputs(0).longest_gap >= Duration::from_millis(120));
        thread::sleep(Duration::from_millis(250));
        let open = clock.inputs(0).longest_gap;
        assert!(open >= Duration::from_millis(250), "{open:?}");
    }

    /// `begin_build` resets every stamp and moves the seq; `inputs` counts reconnects from the
    /// build's base. Fails if a stamp survives into the next build.
    #[test]
    fn begin_build_resets_the_stamps() {
        let clock = Arc::new(BuildClock::new(bounds()));
        let w1 = clock.begin_build(3);
        let mut r = ClockedReader::new(Cursor::new(vec![0u8; 8]), clock.clone(), 0);
        let mut b = [0u8; 4];
        r.read_exact(&mut b).unwrap();
        r.read_exact(&mut b).unwrap();
        assert!(clock.inputs(5).since_first_byte.is_some());
        assert_eq!(clock.inputs(5).reconnects, 2);
        assert_eq!(clock.finish_build(w1), Ok(()));
        let w2 = clock.begin_build(5);
        assert_eq!(seq_of(w2), seq_of(w1) + 1);
        assert_eq!(phase_of(clock.word()), phase::PROBING);
        let i = clock.inputs(5);
        assert_eq!(i.since_first_byte, None);
        assert_eq!(i.longest_gap, Duration::ZERO);
        assert_eq!(i.reconnects, 0);
        assert_eq!(clock.max_gap_ms.load(Ordering::Relaxed), 0);
    }

    /// One swap wins. The engine's bound from a stale word (an earlier build's seq) fails and
    /// leaves the new build probing; the decode thread's swap after a bound reads the cause.
    /// Fails if the swap compares the phase alone.
    #[test]
    fn a_swap_from_a_stale_word_fails() {
        let clock = BuildClock::new(bounds());
        let n = clock.begin_build(0);
        assert!(clock.bound(n, BuildCause::NoBytes));
        assert_eq!(clock.finish_build(n), Err(BuildCause::NoBytes));
        let n1 = clock.begin_build(0);
        assert!(!clock.bound(n, BuildCause::Format), "N's word bounded N+1");
        assert_eq!(clock.word(), n1);
        assert_eq!(clock.finish_build(n1), Ok(()));
        assert!(!clock.bound(n1, BuildCause::Format), "bounded after BUILT");
        let n2 = clock.begin_build(0);
        assert!(clock.bound(n2, BuildCause::Starved));
        assert_eq!(clock.finish_build(n2), Err(BuildCause::Starved));
        let n3 = clock.begin_build(0);
        assert!(clock.bound(n3, BuildCause::Format));
        assert_eq!(clock.finish_build(n3), Err(BuildCause::Format));
    }
}
