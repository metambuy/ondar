//! Ten-band graphic equalizer implemented as a [`rodio::Source`] adapter.
//!
//! Each band is one biquad peaking filter (RBJ cookbook, via the `biquad` crate) with a fixed
//! centre frequency and a gain that can be changed from any thread through [`EqGains`].
//! Coefficients are recomputed on the audio thread only when a gain actually changes, so the
//! per-sample cost is ten multiply-adds per channel.
//!
//! The adapter sits in the real-time path (audio callback), which is why gains are atomics
//! and no locks or allocations happen inside `next()` after construction.
//!
//! [`soft_clip`] is applied as the last operation of every sample, **inside** this adapter
//! rather than as a separate `Source` wrapper. That makes the bound a property of the EQ stage
//! itself, which no caller can forget to wrap — a boosted band cannot put a sample past ±1.0
//! no matter how the graph is assembled.
//!
//! Non-finite input other than infinity still propagates: a NaN in is a NaN out. That is
//! pre-existing behaviour and is explicitly **not** fixed here. It is harmless to the filters
//! because the shaper is the last stage and nothing feeds back through it — a NaN arriving
//! from upstream poisons the biquad state on its way in, which is a separate concern from
//! anything the shaper does.

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

/// Output magnitude never exceeds this. It is strictly below it across the whole range the
/// audio path can produce — the measured worst case out of the band bank is 7.45 — and
/// saturates to exactly the ceiling above roughly 8e4 (about +98 dBFS), where `1.0 / (1.0 + s)`
/// underflows the f32 mantissa and `1.0 - that` rounds to exactly 1.0. An infinite input
/// saturates here too.
pub const SOFT_CLIP_CEILING: f32 = 1.0;
/// Below this magnitude the shaper is the identity, bit for bit. 0.95 because broadcast radio
/// is limited to sit at roughly that peak — see ONDAR.md for the sweep it came from.
pub const SOFT_CLIP_THRESHOLD: f32 = 0.95;

/// Linear below [`SOFT_CLIP_THRESHOLD`], then a rational knee asymptotic to
/// [`SOFT_CLIP_CEILING`]. Continuous in value and in slope at the threshold, so a signal
/// crossing it does not produce an edge.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= SOFT_CLIP_THRESHOLD {
        return x;
    }
    const W: f32 = SOFT_CLIP_CEILING - SOFT_CLIP_THRESHOLD;
    let s = (a - SOFT_CLIP_THRESHOLD) / W;
    // `1.0 - 1.0 / (1.0 + s)`, NOT `s / (1.0 + s)`. They are algebraically identical for every
    // finite `s`, but at `s = inf` the latter is `inf / inf` = NaN where this form gives
    // `1.0 - 0.0` = 1.0 and saturates to exactly the ceiling. Do not "simplify" it back.
    (SOFT_CLIP_THRESHOLD + W * (1.0 - 1.0 / (1.0 + s))).copysign(x)
}

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
        Some(soft_clip(x))
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
        amp: f32,
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
            Some(self.amp * (2.0 * std::f32::consts::PI * self.freq * t).sin())
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

    /// 0.4 peak so the frequency-response tests stay below the soft-clip knee and measure the
    /// filter rather than the shaper: 0.4 at +6 dB is 0.8, under the 0.95 threshold.
    fn sine(freq: f32) -> Sine {
        sine_peak(freq, 0.4)
    }

    fn sine_peak(freq: f32, amp: f32) -> Sine {
        Sine {
            freq,
            amp,
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

    /// How far the EQ's own peak may drift from a recorded value before a bound test fails,
    /// as a fraction of that value — **in pre-shaper terms**.
    ///
    /// The bound tests do not compare the post-shaper peak against a literal. Near the ceiling
    /// the shaper is nearly flat, so a post-shaper tolerance that looks tight is not: 5e-4 on
    /// case 1's 0.99868 admitted any pre-shaper peak from 2.27 to 3.95, and on case 3 anything
    /// above 3.74. And the rounded post-shaper figures carry rounding error as large as any
    /// useful tolerance (case 1 measures 0.998675, on the edge of rounding to 0.99868). The
    /// quantity of interest is the EQ's gain, so each test inverts the measured peak through
    /// [`implied_pre_shaper`] and compares that against the sweep's pre-shaper column.
    ///
    /// Floor under any value here: the output is f32, so the inverse resolves the pre-shaper
    /// peak only to one output step times the knee's inverse slope, `ulp * (1+s)^2` — about
    /// 1.4e-4 relative at case 3's 7.45, the coarsest of the cases. ±0.1 % sits about seven
    /// output steps above that floor, and is ±0.009 dB of EQ gain.
    const PRE_SHAPER_TOLERANCE: f64 = 0.001;

    /// The algebraic inverse of [`soft_clip`], `y -> x`: the pre-shaper magnitude that produced
    /// a given output. Identity at or below the threshold; above it, `u = (|y|-T)/W` undoes
    /// `1 - 1/(1+s)` as `s = u/(1-u)`. Computed in f64 so the inverse adds no rounding of its
    /// own — the only error left is the f32 quantization of `y`. Undefined at the ceiling,
    /// where the forward curve saturates; the tests assert `peak < SOFT_CLIP_CEILING` first.
    fn implied_pre_shaper(y: f32) -> f64 {
        let t = SOFT_CLIP_THRESHOLD as f64;
        // The f32 subtraction, then widened: the same `W` `soft_clip` itself uses.
        let w = (SOFT_CLIP_CEILING - SOFT_CLIP_THRESHOLD) as f64;
        let a = y.abs() as f64;
        if a <= t {
            return y as f64;
        }
        let u = (a - t) / w;
        (t + w * (u / (1.0 - u))).copysign(y as f64)
    }

    /// Settled-tail peak of `out`, asserted under the ceiling, then inverted and asserted within
    /// [`PRE_SHAPER_TOLERANCE`] of `recorded_pre_shaper`.
    fn assert_pre_shaper_peak(out: &[f32], recorded_pre_shaper: f64) {
        let tail = &out[out.len() / 2..];
        let peak = tail.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        assert!(
            peak < SOFT_CLIP_CEILING,
            "expected the output to stay under the ceiling, got peak {peak:.9}"
        );
        let implied = implied_pre_shaper(peak);
        let drift = implied / recorded_pre_shaper - 1.0;
        assert!(
            drift.abs() <= PRE_SHAPER_TOLERANCE,
            "implied pre-shaper peak {implied:.5} is {:+.4} % from the recorded \
             {recorded_pre_shaper} (tolerance ±{:.4} %; output peak {peak:.9})",
            drift * 100.0,
            PRE_SHAPER_TOLERANCE * 100.0
        );
    }

    #[test]
    fn implied_pre_shaper_inverts_soft_clip() {
        // The bound tests rest on this inverse agreeing with the shipped curve, so it is
        // verified rather than assumed. Grid: ±16 in 1e-4 steps, past case 3's 7.45 and both
        // sides of the knee.
        //
        // Bound: below the knee, exact. Above it, one f32 output step — `f32::EPSILON / 2` is
        // the ulp for values in [0.5, 1) — scaled by the inverse slope `(1+s)^2`, which is how
        // far an output rounded by one step can move the implied input. Anything larger means
        // one of the two forms is wrong, not that f32 is imprecise.
        let t = SOFT_CLIP_THRESHOLD as f64;
        let w = (SOFT_CLIP_CEILING - SOFT_CLIP_THRESHOLD) as f64;
        let ulp = f32::EPSILON as f64 / 2.0;
        for i in -160_000..=160_000 {
            let x = i as f32 * 1e-4;
            let back = implied_pre_shaper(soft_clip(x));
            let err = (back - x as f64).abs();
            if x.abs() <= SOFT_CLIP_THRESHOLD {
                assert_eq!(back, x as f64, "not the identity below the knee at {x}");
            } else {
                let s = (x.abs() as f64 - t) / w;
                let step = ulp * (1.0 + s).powi(2);
                assert!(
                    err <= step,
                    "round trip off by {err:e} at {x}, more than one output step ({step:e})"
                );
            }
        }
    }

    #[test]
    fn flat_eq_is_transparent() {
        // At broadcast peak level the shaper never engages (0.95 is the threshold, and the
        // comparison is `<=`), so a flat EQ is bit-exact. This is the `-inf` residual row of
        // the block A sweep, asserted rather than merely measured.
        let reference: Vec<f32> = sine_peak(1000.0, 0.95).collect();
        let out: Vec<f32> = Equalizer::new(sine_peak(1000.0, 0.95), EqGains::default()).collect();
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
    fn boost_near_full_scale_is_bounded() {
        // Block A's case 1. +12 dB is ×4 linear, and the band bank produces a 2.78707 peak
        // internally (ONDAR.md's pre-shaper column); `soft_clip` is what bounds it on the way
        // out. Asserted in pre-shaper terms — see `PRE_SHAPER_TOLERANCE`.
        let gains = EqGains::default();
        gains.set(1, 12.0); // 62.5 Hz band
        let out: Vec<f32> = Equalizer::new(sine_peak(62.5, 0.7), gains).collect();
        assert_pre_shaper_peak(&out, 2.78707);
    }

    #[test]
    fn boost_at_broadcast_realistic_level_is_bounded() {
        // Block A's case 2. 0.95 peak (broadcast content, heavily limited to sit near full
        // scale) with a more moderate +4 dB boost — not the +12 dB extreme. The EQ stage still
        // reaches 1.50583 internally.
        let gains = EqGains::default();
        gains.set(1, 4.0); // 62.5 Hz band
        let out: Vec<f32> = Equalizer::new(sine_peak(62.5, 0.95), gains).collect();
        assert_pre_shaper_peak(&out, 1.50583);
    }

    #[test]
    fn all_bands_boosted_multi_tone_is_bounded() {
        // Block A's case 3 and the worst of the three: five equal sines (62.5 Hz – 8 kHz)
        // normalised to a 0.95 peak, every band at +12 dB. The band bank reaches 7.45484
        // internally. Built the same way as `examples/eq_headroom_sweep.rs`, so the two
        // measure the same signal.
        const TONES_HZ: [f32; 5] = [62.5, 250.0, 1000.0, 4000.0, 8000.0];
        let mut signal: Vec<f32> = (0..LEN)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                TONES_HZ
                    .iter()
                    .map(|f| (2.0 * std::f32::consts::PI * f * t).sin())
                    .sum()
            })
            .collect();
        let raw_peak = signal.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        for s in &mut signal {
            *s *= 0.95 / raw_peak;
        }

        let gains = EqGains::default();
        for band in 0..BAND_COUNT {
            gains.set(band, 12.0);
        }
        let source = rodio::buffer::SamplesBuffer::new(
            NonZero::new(1).unwrap(),
            NonZero::new(RATE).unwrap(),
            signal,
        );
        let out: Vec<f32> = Equalizer::new(source, gains).collect();
        assert_pre_shaper_peak(&out, 7.45484);
    }

    #[test]
    fn already_above_unity_input_is_bounded() {
        // mp3 and AAC decoders legitimately emit samples past ±1.0 on hot masters — intersample
        // peaks survive the encode and come back out above full scale. Those are bounded even
        // with a flat EQ, which is why the shaper is unconditional rather than bypassed when no
        // band is boosted. With a flat EQ the pre-shaper peak is the input's own 1.5.
        let out: Vec<f32> = Equalizer::new(sine_peak(62.5, 1.5), EqGains::default()).collect();
        assert_pre_shaper_peak(&out, 1.5);
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
            amp: 1.0,
            rate: 22_050,
            n: 22_050,
            i: 0,
        };
        let out: Vec<f32> = Equalizer::new(src, gains).collect();
        assert_eq!(out.len(), 22_050);
        assert!(out.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn soft_clip_is_identity_below_threshold() {
        // Bit-exact, not approximately: below the knee the shaper must not touch the signal.
        for i in 0..=4000 {
            let x = -SOFT_CLIP_THRESHOLD + 2.0 * SOFT_CLIP_THRESHOLD * (i as f32 / 4000.0);
            assert_eq!(soft_clip(x), x, "identity violated at {x}");
        }
        for x in [0.0f32, -0.0, SOFT_CLIP_THRESHOLD, -SOFT_CLIP_THRESHOLD] {
            assert_eq!(soft_clip(x), x, "identity violated at the boundary {x}");
        }
        // `==` cannot see the sign of zero, so check it directly.
        assert!(soft_clip(-0.0f32).is_sign_negative());
        assert!(soft_clip(0.0f32).is_sign_positive());
    }

    #[test]
    fn soft_clip_is_odd() {
        for i in 0..=2000 {
            let x = 3.0 * (i as f32 / 1000.0) - 1.5; // spans both regions, both signs
            assert_eq!(soft_clip(-x), -soft_clip(x), "oddness violated at {x}");
        }
    }

    #[test]
    fn soft_clip_is_bounded_and_monotonic() {
        let mut prev = f32::NEG_INFINITY;
        for i in 0..=20_000 {
            let x = 1e6 * (i as f32 / 10_000.0 - 1.0); // -1e6 ..= 1e6
            let y = soft_clip(x);
            assert!(
                y.abs() <= SOFT_CLIP_CEILING,
                "exceeded the ceiling at {x}: {y}"
            );
            assert!(y >= prev, "not monotonic at {x}: {y} < {prev}");
            prev = y;
        }
        // Strictly inside the ceiling across everything the audio path can produce — the
        // measured worst case out of the band bank is 7.45. Above roughly 8e4 the f32 result
        // rounds to exactly 1.0, which the `<=` assertion above covers.
        for i in 0..=10_000 {
            let x = 1e4 * (i as f32 / 10_000.0);
            assert!(soft_clip(x).abs() < SOFT_CLIP_CEILING, "not strict at {x}");
        }
        assert_eq!(soft_clip(f32::INFINITY), SOFT_CLIP_CEILING);
        assert_eq!(soft_clip(f32::NEG_INFINITY), -SOFT_CLIP_CEILING);
        // Pins the documented pre-existing behaviour; it is not an endorsement of it.
        assert!(soft_clip(f32::NAN).is_nan());
    }

    #[test]
    fn soft_clip_knee_is_continuous() {
        // C0: just past the knee the shaper is still the identity, to within 1e-6. Note this
        // compares against the identity continuation, not against f(T): |f(T+h) - f(T)| is ~h
        // for any function whose slope is ~1, so asserting that is small tests nothing.
        let h = 1e-4;
        let past = soft_clip(SOFT_CLIP_THRESHOLD + h);
        let identity = SOFT_CLIP_THRESHOLD + h;
        assert!(
            (past - identity).abs() < 1e-6,
            "C0 broken at the knee: {past} vs identity {identity}"
        );

        // C1: the slope carries through as 1.0, so a signal crossing the threshold gets no
        // edge. Tested as the *order* of the deviation rather than by finite difference: were
        // the slope discontinuous, the deviation from the identity would grow linearly in h;
        // because it is continuous it grows as h^2/W, so doubling h quadruples it.
        //
        // A direct finite-difference slope cannot settle this in f32. Truncation error is h/W
        // and rounding error is ulp(0.95)/h, so the total floors near 2*sqrt(ulp/W) = 2.2e-3
        // at the best h — a 1e-3 tolerance on the slope sits under that floor and cannot be
        // met at any step size. The ratio below is immune to both.
        let dev = |h: f32| ((SOFT_CLIP_THRESHOLD + h) - soft_clip(SOFT_CLIP_THRESHOLD + h)).abs();
        let ratio = dev(2e-3) / dev(1e-3);
        assert!(
            (3.5..=4.5).contains(&ratio),
            "C1 broken at the knee: dev(2h)/dev(h) = {ratio} (4 = continuous slope, 2 = a kink)"
        );
    }

    #[test]
    fn soft_clip_does_not_engage_at_broadcast_peak() {
        // The product claim the threshold was chosen for: material sitting at the broadcast
        // peak passes through untouched.
        assert_eq!(soft_clip(0.95), 0.95);
        assert_eq!(soft_clip(0.9499), 0.9499);
    }
}
