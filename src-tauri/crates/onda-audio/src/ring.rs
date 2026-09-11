//! The bridge between the decode thread and the audio callback.
//!
//! [`RingSource`] is a [`rodio::Source`] that pops interleaved samples from an SPSC ring
//! buffer. It never blocks: on underrun it emits silence and counts the event. This keeps
//! blocking network reads and Symphonia work off the real-time thread entirely.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};
use rtrb::{Consumer, Producer, RingBuffer};

/// Seconds of audio the ring can hold. This does *not* bound latency-to-live — two different
/// figures, both driven by `max(prefetch_secs, burst_secs)` (whichever hands the decoder more
/// audio up front), not this ring:
///
/// - **First audible sample** ≈ `max(prefetch_secs, burst_secs)` — how long until there's
///   enough buffered to start decoding at all.
/// - **`IcyMetadata` freshness** ≈ `max(prefetch_secs, burst_secs) − ring_occupancy`, where
///   `ring_occupancy = min(RING_SECONDS, max(prefetch_secs, burst_secs))` — once decoding
///   starts, the decoder drains that head start faster than real time until the ring is full
///   and it blocks on ring space; whatever didn't fit in the ring is the residual lag behind
///   the server's real-time position, which is where in-band ICY metadata lives. The occupancy
///   is capped by the ring but is *not* always the full `RING_SECONDS` — a head start smaller
///   than the ring never fills it, which is why the subtrahend is the `min`, not a flat
///   `RING_SECONDS`.
///
/// Measured via `scripts/stall-server.py --mode metaint` (README "Stall testing"): fits data
/// from prefetch 8192/49152B and burst 0/65536/131072B to within ~0.3 s. At the smallest point
/// (8192B prefetch, no burst) the formula floors at 0 against a measured 0.49–0.50 s; that
/// miss is **unexplained** — it is close to `prefetch_secs` itself there, but nothing here
/// attributes it to that or to connect/decode-startup overhead. Note the harness cannot
/// resolve it either way: at `--icy-metaint 4000` and 16000 B/s, title timing quantises to
/// 0.25 s steps and nothing finer than ~0.5 s is resolvable. See ONDA.md's latency table.
pub const RING_SECONDS: usize = 2;

/// Shared, cross-thread view of one ring's occupancy. The audio callback (consumer) advances
/// `underruns`; the decode thread (producer) reports `fill` on its own cadence. The engine
/// thread polls both to drive buffering supervision (see `engine::tick`) — this is what lets
/// starvation be noticed even while the decode thread is blocked on a stalled network read.
pub struct RingStats {
    /// Monotonic count of underrun *events* (contiguous silent runs), not silent samples —
    /// see [`RingSource::next`]. Never reset; the engine derives "did anything new happen"
    /// by snapshotting and comparing, not by clearing this back to zero.
    pub underruns: AtomicU64,
    pub fill: AtomicUsize,
    pub capacity: usize,
}

pub struct RingHandle {
    pub producer: Producer<f32>,
    pub stats: Arc<RingStats>,
}

pub struct RingSource {
    consumer: Consumer<f32>,
    stats: Arc<RingStats>,
    sample_rate: SampleRate,
    channels: ChannelCount,
    /// Whether the current silent run has already been counted, so a contiguous run of
    /// underrun samples advances `stats.underruns` and zeroes `stats.fill` exactly once, not
    /// once per silent sample.
    in_underrun: bool,
    /// Once the producer is gone and the ring is drained, the source ends so rodio can
    /// drop it and the `Player` queue can move on to a replacement.
    finished: bool,
}

pub fn ring(sample_rate: SampleRate, channels: ChannelCount) -> (RingHandle, RingSource) {
    let capacity = sample_rate.get() as usize * channels.get() as usize * RING_SECONDS;
    let (producer, consumer) = RingBuffer::<f32>::new(capacity);
    let stats = Arc::new(RingStats {
        underruns: AtomicU64::new(0),
        fill: AtomicUsize::new(0),
        capacity,
    });
    (
        RingHandle {
            producer,
            stats: stats.clone(),
        },
        RingSource {
            consumer,
            stats,
            sample_rate,
            channels,
            in_underrun: false,
            finished: false,
        },
    )
}

impl Iterator for RingSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.finished {
            return None;
        }
        match self.consumer.pop() {
            Ok(s) => {
                self.in_underrun = false;
                Some(s)
            }
            Err(_) => {
                if self.consumer.is_abandoned() {
                    self.finished = true;
                    return None;
                }
                if !self.in_underrun {
                    self.in_underrun = true;
                    self.stats.underruns.fetch_add(1, Ordering::Relaxed);
                    // The decode thread only reports `fill` every 1024 samples and may be
                    // blocked on a stalled network read for many seconds; stamping 0 here
                    // (from the thread that just observed the ring is actually empty) stops
                    // the engine's tick() from reading a stale high `fill` and immediately
                    // "recovering" while nothing has actually changed.
                    self.stats.fill.store(0, Ordering::Relaxed);
                }
                Some(0.0)
            }
        }
    }
}

impl Source for RingSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> ChannelCount {
        self.channels
    }
    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZero;

    #[test]
    fn underrun_run_increments_underruns_once() {
        let (mut h, mut src) = ring(NonZero::new(100).unwrap(), NonZero::new(1).unwrap());
        h.producer.push(0.5).unwrap();
        assert_eq!(src.next(), Some(0.5));
        assert_eq!(h.stats.underruns.load(Ordering::Relaxed), 0);
        // A run of three consecutive underrun pops must count as one event, not three.
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(h.stats.underruns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn underrun_zeroes_stale_fill_and_counts_separate_runs() {
        let (mut h, mut src) = ring(NonZero::new(100).unwrap(), NonZero::new(1).unwrap());
        // As if the decode thread reported a near-full ring just before hanging.
        h.stats.fill.store(80, Ordering::Relaxed);
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(h.stats.fill.load(Ordering::Relaxed), 0);
        assert_eq!(h.stats.underruns.load(Ordering::Relaxed), 1);

        // A push+pop ends the run; a second, separate underrun run increments again.
        h.producer.push(0.25).unwrap();
        assert_eq!(src.next(), Some(0.25));
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(h.stats.underruns.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn ends_when_producer_dropped_and_drained() {
        let (h, mut src) = ring(NonZero::new(100).unwrap(), NonZero::new(2).unwrap());
        let mut p = h.producer;
        p.push(1.0).unwrap();
        drop(p);
        assert_eq!(src.next(), Some(1.0));
        assert_eq!(src.next(), None);
        assert_eq!(src.next(), None);
    }
}
