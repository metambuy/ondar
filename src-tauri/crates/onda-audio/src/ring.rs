//! The bridge between the decode thread and the audio callback.
//!
//! [`RingSource`] is a [`rodio::Source`] that pops interleaved samples from an SPSC ring
//! buffer. It never blocks: on underrun it emits silence and raises a flag that the decode
//! thread uses to report `Buffering`. This keeps blocking network reads and Symphonia work
//! off the real-time thread entirely.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};
use rtrb::{Consumer, Producer, RingBuffer};

/// Seconds of audio the ring can hold. Latency to live is bounded by this.
pub const RING_SECONDS: usize = 2;

pub struct RingHandle {
    pub producer: Producer<f32>,
    /// Set by the consumer when it had to emit silence; cleared by the decode thread once
    /// the ring has refilled.
    pub starved: Arc<AtomicBool>,
    pub capacity: usize,
}

pub struct RingSource {
    consumer: Consumer<f32>,
    starved: Arc<AtomicBool>,
    sample_rate: SampleRate,
    channels: ChannelCount,
    /// Once the producer is gone and the ring is drained, the source ends so rodio can
    /// drop it and the `Player` queue can move on to a replacement.
    finished: bool,
}

pub fn ring(sample_rate: SampleRate, channels: ChannelCount) -> (RingHandle, RingSource) {
    let capacity = sample_rate.get() as usize * channels.get() as usize * RING_SECONDS;
    let (producer, consumer) = RingBuffer::<f32>::new(capacity);
    let starved = Arc::new(AtomicBool::new(false));
    (
        RingHandle {
            producer,
            starved: starved.clone(),
            capacity,
        },
        RingSource {
            consumer,
            starved,
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
                self.starved.store(true, Ordering::Relaxed);
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
        assert!(!h.starved.load(Ordering::Relaxed));
        assert_eq!(src.next(), Some(0.0));
        assert!(h.starved.load(Ordering::Relaxed));
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
