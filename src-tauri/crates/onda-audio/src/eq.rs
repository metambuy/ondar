//! Ten-band graphic equalizer implemented as a [`rodio::Source`] adapter.
//!
//! Each band is one biquad peaking filter (RBJ cookbook, via the `biquad` crate) with a fixed
//! centre frequency and a gain that can be changed from any thread through [`EqGains`].
//! Coefficients are recomputed on the audio thread only when a gain actually changes, so the
//! per-sample cost is ten multiply-adds per channel.
//!
//! The adapter sits in the real-time path (audio callback), which is why gains are atomics
//! and no locks or allocations happen inside `next()` after construction.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use rodio::{ChannelCount, SampleRate, Source};

use crate::types::EqBand;

pub const BAND_COUNT: usize = 10;

/// ISO 266 octave centres, as used by virtually every 10-band graphic EQ.
pub const BAND_CENTERS_HZ: [f32; BAND_COUNT] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Q for a one-octave-wide peaking band. Q = f0 / bandwidth = 1 / (2^(1/2) - 2^(-1/2)) ≈ 1.414.
pub const BAND_Q: f32 = 1.414;

pub const MAX_GAIN_DB: f32 = 12.0;

/// Shared, lock-free gain table. Cloning shares the same underlying values.
#[derive(Clone, Debug)]
pub struct EqGains(Arc<[AtomicU32; BAND_COUNT]>);

impl Default for EqGains {
    fn default() -> Self {
        Self(Arc::new(std::array::from_fn(|_| {
            AtomicU32::new(0f32.to_bits())
        })))
    }
}

impl EqGains {
    pub fn set(&self, band: usize, gain_db: f32) {
        if band < BAND_COUNT {
            let g = gain_db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
            self.0[band].store(g.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn get(&self, band: usize) -> f32 {
        f32::from_bits(self.0[band].load(Ordering::Relaxed))
    }

    pub fn snapshot(&self) -> [f32; BAND_COUNT] {
        std::array::from_fn(|i| self.get(i))
    }

    pub fn bands(&self) -> Vec<EqBand> {
        BAND_CENTERS_HZ
            .iter()
            .enumerate()
            .map(|(i, &c)| EqBand {
                index: i as u8,
                center_hz: c,
                gain_db: self.get(i),
            })
            .collect()
    }
}

type Filter = DirectForm2Transposed<f32>;

/// Identity coefficients, used for bands whose centre is above Nyquist for the current
/// sample rate (e.g. 16 kHz at 22.05 kHz streams) — the band is simply bypassed.
fn identity() -> Coefficients<f32> {
    Coefficients {
        a1: 0.0,
        a2: 0.0,
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
    }
}

fn coefficients(center_hz: f32, gain_db: f32, sample_rate: f32) -> Coefficients<f32> {
    // Keep a margin below Nyquist; the RBJ formulas degrade close to fs/2.
    if center_hz >= sample_rate * 0.45 {
        return identity();
    }
    Coefficients::<f32>::from_params(
        Type::PeakingEQ(gain_db),
        sample_rate.hz(),
        center_hz.hz(),
        BAND_Q,
    )
    .unwrap_or_else(|_| identity())
}

/// How often (in frames) the adapter checks whether a gain changed.
const GAIN_CHECK_INTERVAL: u32 = 64;

pub struct Equalizer<S: Source> {
    inner: S,
    gains: EqGains,
    /// The gain values the current coefficients were computed from.
    applied: [f32; BAND_COUNT],
    /// `filters[channel][band]`
    filters: Vec<[Filter; BAND_COUNT]>,
    sample_rate: SampleRate,
    channels: ChannelCount,
    channel_cursor: usize,
    frames_since_check: u32,
}

impl<S: Source> Equalizer<S> {
    pub fn new(inner: S, gains: EqGains) -> Self {
        let sample_rate = inner.sample_rate();
        let channels = inner.channels();
        let mut eq = Self {
            inner,
            gains,
            applied: [f32::NAN; BAND_COUNT],
            filters: Vec::new(),
            sample_rate,
            channels,
            channel_cursor: 0,
            frames_since_check: 0,
        };
        eq.rebuild();
        eq
    }

    /// Recreate every filter (format change) — resets filter state.
    fn rebuild(&mut self) {
        let fs = self.sample_rate.get() as f32;
        let gains = self.gains.snapshot();
        let make_row =
            || std::array::from_fn(|b| Filter::new(coefficients(BAND_CENTERS_HZ[b], gains[b], fs)));
        self.filters = (0..self.channels.get()).map(|_| make_row()).collect();
        self.applied = gains;
        self.channel_cursor = 0;
        self.frames_since_check = 0;
    }

    /// Recompute only the bands whose gain changed, keeping filter state (no click).
    fn refresh_changed_gains(&mut self) {
        let fs = self.sample_rate.get() as f32;
        let gains = self.gains.snapshot();
        for b in 0..BAND_COUNT {
            if gains[b] != self.applied[b] {
                let c = coefficients(BAND_CENTERS_HZ[b], gains[b], fs);
                for row in &mut self.filters {
                    row[b].update_coefficients(c);
                }
                self.applied[b] = gains[b];
            }
        }
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: Source> Iterator for Equalizer<S> {
    type Item = rodio::Sample;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;

        if self.channel_cursor == 0 {
            // Frame boundary: cheap place to notice format changes and gain edits.
            let sr = self.inner.sample_rate();
            let ch = self.inner.channels();
            if sr != self.sample_rate || ch != self.channels {
                self.sample_rate = sr;
                self.channels = ch;
                self.rebuild();
            }
            self.frames_since_check += 1;
            if self.frames_since_check >= GAIN_CHECK_INTERVAL {
                self.frames_since_check = 0;
                self.refresh_changed_gains();
            }
        }

        let row = &mut self.filters[self.channel_cursor];
        let mut x = sample;
        for f in row.iter_mut() {
            x = f.run(x);
        }

        self.channel_cursor += 1;
        if self.channel_cursor >= self.channels.get() as usize {
            self.channel_cursor = 0;
        }
        Some(x)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S: Source> Source for Equalizer<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZero;

    /// Minimal mono sine source; `rodio::source::SineWave` exists but we want control over
    /// sample rate and length without pulling in feature flags.
    struct Sine {
        freq: f32,
        rate: u32,
        n: usize,
        i: usize,
    }
    impl Iterator for Sine {
        type Item = f32;
        fn next(&mut self) -> Option<f32> {
            if self.i >= self.n {
                return None;
            }
            let t = self.i as f32 / self.rate as f32;
            self.i += 1;
            Some((2.0 * std::f32::consts::PI * self.freq * t).sin())
        }
    }
    impl Source for Sine {
        fn current_span_len(&self) -> Option<usize> {
            None
        }
        fn channels(&self) -> ChannelCount {
            NonZero::new(1).unwrap()
        }
        fn sample_rate(&self) -> SampleRate {
            NonZero::new(self.rate).unwrap()
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    const RATE: u32 = 44_100;
    const LEN: usize = 44_100; // 1 s

    fn sine(freq: f32) -> Sine {
        Sine {
            freq,
            rate: RATE,
            n: LEN,
            i: 0,
        }
    }

    /// RMS of the second half of the signal, after filters have settled.
    fn settled_rms(samples: &[f32]) -> f32 {
        let tail = &samples[samples.len() / 2..];
        (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
    }

    fn gain_db(out: &[f32], reference: &[f32]) -> f32 {
        20.0 * (settled_rms(out) / settled_rms(reference)).log10()
    }

    #[test]
    fn flat_eq_is_transparent() {
        let reference: Vec<f32> = sine(1000.0).collect();
        let out: Vec<f32> = Equalizer::new(sine(1000.0), EqGains::default()).collect();
        assert_eq!(out.len(), reference.len());
        let max_err = out
            .iter()
            .zip(&reference)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-4, "flat EQ altered samples by up to {max_err}");
    }

    #[test]
    fn boost_at_1khz_raises_1khz_by_about_6db() {
        let gains = EqGains::default();
        gains.set(5, 6.0); // 1 kHz band
        let reference: Vec<f32> = sine(1000.0).collect();
        let out: Vec<f32> = Equalizer::new(sine(1000.0), gains).collect();
        let g = gain_db(&out, &reference);
        assert!(
            (g - 6.0).abs() < 0.3,
            "expected ≈ +6 dB at 1 kHz, got {g:.2} dB"
        );
    }

    #[test]
    fn boost_at_1khz_leaves_100hz_alone() {
        let gains = EqGains::default();
        gains.set(5, 6.0);
        let reference: Vec<f32> = sine(100.0).collect();
        let out: Vec<f32> = Equalizer::new(sine(100.0), gains).collect();
        let g = gain_db(&out, &reference);
        assert!(g.abs() < 0.5, "100 Hz should be unaffected, got {g:.2} dB");
    }

    #[test]
    fn cut_at_1khz_lowers_1khz_by_about_12db() {
        let gains = EqGains::default();
        gains.set(5, -12.0);
        let reference: Vec<f32> = sine(1000.0).collect();
        let out: Vec<f32> = Equalizer::new(sine(1000.0), gains).collect();
        let g = gain_db(&out, &reference);
        assert!((g + 12.0).abs() < 0.3, "expected ≈ −12 dB, got {g:.2} dB");
    }

    #[test]
    fn gains_are_clamped() {
        let gains = EqGains::default();
        gains.set(0, 40.0);
        gains.set(1, -40.0);
        gains.set(99, 3.0); // out of range: ignored, must not panic
        assert_eq!(gains.get(0), MAX_GAIN_DB);
        assert_eq!(gains.get(1), -MAX_GAIN_DB);
    }

    #[test]
    fn gain_change_mid_stream_is_picked_up() {
        let gains = EqGains::default();
        let mut eq = Equalizer::new(sine(1000.0), gains.clone());
        let mut out = Vec::with_capacity(LEN);
        for i in 0..LEN {
            if i == LEN / 4 {
                gains.set(5, 6.0);
            }
            out.push(eq.next().unwrap());
        }
        let reference: Vec<f32> = sine(1000.0).collect();
        let g = gain_db(&out, &reference);
        assert!(
            (g - 6.0).abs() < 0.3,
            "expected ≈ +6 dB after change, got {g:.2} dB"
        );
    }

    #[test]
    fn bands_above_nyquist_are_bypassed_not_panicking() {
        let gains = EqGains::default();
        gains.set(9, 12.0); // 16 kHz band on a 22.05 kHz stream
        let src = Sine {
            freq: 1000.0,
            rate: 22_050,
            n: 22_050,
            i: 0,
        };
        let out: Vec<f32> = Equalizer::new(src, gains).collect();
        assert_eq!(out.len(), 22_050);
        assert!(out.iter().all(|x| x.is_finite()));
    }
}
