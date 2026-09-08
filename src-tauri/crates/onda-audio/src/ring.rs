//! The bridge between the decode thread and the audio callback.
//!
//! [`RingSource`] is a [`rodio::Source`] that pops interleaved samples from an SPSC ring
//! buffer. It never blocks: on underrun it emits silence and raises a flag that the decode
//! thread uses to report `Buffering`. This keeps blocking network reads and Symphonia work
//! off the real-time thread entirely.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};
use rtrb::{Consumer, Producer, RingBuffer};

/// Seconds of audio the ring can hold. Latency to live is bounded by this.
pub const RING_SECONDS: usize = 2;

/// Shared, cross-thread view of one ring's occupancy. The audio callback (consumer) sets
/// `starved`; the decode thread (producer) reports `fill` on its own cadence. The engine
/// thread polls both to drive buffering supervision (see `engine::tick`) — this is what lets
/// starvation be noticed even while the decode thread is blocked on a stalled network read.
pub struct RingStats {
    pub starved: AtomicBool,
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
    /// Once the producer is gone and the ring is drained, the source ends so rodio can
    /// drop it and the `Player` queue can move on to a replacement.
    finished: bool,
}

pub fn ring(sample_rate: SampleRate, channels: ChannelCount) -> (RingHandle, RingSource) {
    let capacity = sample_rate.get() as usize * channels.get() as usize * RING_SECONDS;
    let (producer, consumer) = RingBuffer::<f32>::new(capacity);
    let stats = Arc::new(RingStats {
        starved: AtomicBool::new(false),
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
            Ok(s) => Some(s),
            Err(_) => {
                if self.consumer.is_abandoned() {
                    self.finished = true;
                    return None;
                }
                self.stats.starved.store(true, Ordering::Relaxed);
                // The decode thread only reports `fill` every 1024 samples and may be
                // blocked on a stalled network read for many seconds; stamping 0 here (from
                // the thread that just observed the ring is actually empty) stops the
                // engine's tick() from reading a stale high `fill` and immediately
                // "recovering" out of Buffering while nothing has actually changed.
                self.stats.fill.store(0, Ordering::Relaxed);
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
    fn underrun_emits_silence_and_flags_starvation() {
        let (mut h, mut src) = ring(NonZero::new(100).unwrap(), NonZero::new(1).unwrap());
        h.producer.push(0.5).unwrap();
        assert_eq!(src.next(), Some(0.5));
        assert!(!h.stats.starved.load(Ordering::Relaxed));
        assert_eq!(src.next(), Some(0.0));
        assert!(h.stats.starved.load(Ordering::Relaxed));
    }

    #[test]
    fn underrun_zeroes_stale_fill() {
        let (h, mut src) = ring(NonZero::new(100).unwrap(), NonZero::new(1).unwrap());
        // As if the decode thread reported a near-full ring just before hanging.
        h.stats.fill.store(80, Ordering::Relaxed);
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(h.stats.fill.load(Ordering::Relaxed), 0);
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
