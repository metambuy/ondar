//! Cross-checks the shipped soft-clip stage against the curve that chose its threshold, and
//! reports what `Equalizer` actually bounds its output to.
//!
//! Usage: `cargo run -p onda-audio --example eq_headroom_sweep --release`
//!
//! **This is no longer the block A sweep.** That run — the t = [0.80, 0.85, 0.90, 0.95] tables
//! over transparency cost and bound, which is what chose t = 0.95 — was taken at `9ad668f`,
//! before the shaper existed, and is recorded in ONDA.md. It cannot be reproduced here any
//! more: `soft_clip` now lives inside `Equalizer`, so `through_eq` returns already-bounded
//! output and re-shaping it would print numbers that look like measurements but are shaped
//! twice. Those tables were removed rather than left with a caveat, because the trap is
//! someone pasting them into ONDA.md later.
//!
//! What remains is the part that stays true: the example keeps its **own** implementation of
//! the curve, written independently of `eq.rs`, and checks the two agree. That is the useful
//! invariant — if the shipped constant or the shipped formula drifts from what was swept, this
//! catches it.
//!
//! Everything is measured over the settled tail — the second half of each signal — the same
//! convention `eq.rs`'s `settled_rms` uses, so the biquads' startup transient is excluded.

use std::num::NonZero;
use std::process::Command;
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};

use onda_audio::eq::{self, Equalizer};
use onda_audio::{BAND_COUNT, EqGains};

const RATE: u32 = 44_100;
/// 1 s per signal, matching `eq.rs`'s tests so Table 2's pre-shaper peaks are comparable to
/// the recorded M1 baselines.
const LEN: usize = 44_100;
/// The threshold block A's sweep selected.
const T: f32 = 0.95;
const MULTI_TONE_HZ: [f32; 5] = [62.5, 250.0, 1000.0, 4000.0, 8000.0];

/// The swept curve, kept in the form block A used — `s/(1+s)`, where `eq.rs` ships the
/// algebraically identical `1 - 1/(1+s)`. Written independently on purpose: agreement between
/// two separate expressions of the same curve is the check, so this must not call `soft_clip`.
fn shape(x: f32, t: f32) -> f32 {
    let a = x.abs();
    if a <= t {
        return x;
    }
    let w = 1.0 - t;
    let s = (a - t) / w;
    (t + w * (s / (1.0 + s))).copysign(x)
}

/// Mono `Source` over a precomputed buffer. `eq.rs`'s test `Sine` is `#[cfg(test)]` and so not
/// reachable from an example; buffering also lets the multi-tone be normalised to an exact peak
/// before it is fed to the EQ.
struct Samples {
    data: Vec<f32>,
    i: usize,
}

impl Samples {
    fn new(data: Vec<f32>) -> Self {
        Self { data, i: 0 }
    }
}

impl Iterator for Samples {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let s = self.data.get(self.i).copied();
        self.i += 1;
        s
    }
}

impl Source for Samples {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> ChannelCount {
        NonZero::new(1).expect("1 is non-zero")
    }
    fn sample_rate(&self) -> SampleRate {
        NonZero::new(RATE).expect("RATE is non-zero")
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

fn sine(freq: f32, amp: f32) -> Vec<f32> {
    (0..LEN)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            amp * (2.0 * std::f32::consts::PI * freq * t).sin()
        })
        .collect()
}

/// Equal-amplitude sines summed, then scaled so the whole buffer peaks at `target_peak`.
fn multi_tone(target_peak: f32) -> Vec<f32> {
    let mut v: Vec<f32> = (0..LEN)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            MULTI_TONE_HZ
                .iter()
                .map(|f| (2.0 * std::f32::consts::PI * f * t).sin())
                .sum()
        })
        .collect();
    let p = peak(&v);
    if p > 0.0 {
        let k = target_peak / p;
        for s in &mut v {
            *s *= k;
        }
    }
    v
}

/// The second half, after the biquads have settled.
fn settled(v: &[f32]) -> &[f32] {
    &v[v.len() / 2..]
}

fn rms(v: &[f32]) -> f32 {
    (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0f32, |m, &x| m.max(x.abs()))
}

fn db(ratio: f32) -> String {
    if ratio > 0.0 {
        format!("{:9.4}", 20.0 * ratio.log10())
    } else {
        // Exactly transparent: the shaper never engaged, so the residual is identically zero.
        format!("{:>9}", "-inf")
    }
}

/// Residual after removing the best-fit broadband gain, as a fraction of the input. A THD
/// **proxy**, not THD: it is a time-domain residual with no FFT, so it lumps harmonic
/// distortion together with any waveform change the gain fit cannot absorb.
fn residual_db(y: &[f32], x: &[f32]) -> String {
    let dot_yx: f32 = y.iter().zip(x).map(|(a, b)| a * b).sum();
    let dot_xx: f32 = x.iter().map(|b| b * b).sum();
    let g = if dot_xx > 0.0 { dot_yx / dot_xx } else { 0.0 };
    let resid: Vec<f32> = y.iter().zip(x).map(|(a, b)| a - g * b).collect();
    db(rms(&resid) / rms(x))
}

fn through_eq(signal: &[f32], gains: EqGains) -> Vec<f32> {
    Equalizer::new(Samples::new(signal.to_vec()), gains).collect()
}

fn shaped(v: &[f32], t: f32) -> Vec<f32> {
    v.iter().map(|&x| shape(x, t)).collect()
}

fn capture(args: &[&str]) -> String {
    Command::new(args[0])
        .args(&args[1..])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn main() {
    println!(
        "eq_headroom_sweep — rev {} — {} — {} Hz, {} s per signal, mono, settled tail only",
        capture(&["git", "rev-parse", "--short", "HEAD"]),
        capture(&["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        RATE,
        LEN as f32 / RATE as f32
    );

    // ---- Cross-check: the shipped curve against the swept one ----
    //
    // Compared within a tolerance rather than exactly: `s/(1+s)` and `1 - 1/(1+s)` are
    // algebraically identical but round differently in the last ulp at large `s`, so `==`
    // would fail spuriously. The range spans everything the band bank can produce — the
    // measured worst case is 7.45.
    let mut worst = 0.0f32;
    let mut worst_at = 0.0f32;
    for i in 0..=200_000 {
        let x = 20.0 * (i as f32 / 200_000.0) - 10.0; // -10 ..= 10
        let d = (shape(x, T) - onda_audio::eq::soft_clip(x)).abs();
        if d > worst {
            worst = d;
            worst_at = x;
        }
    }
    println!();
    println!("CROSS-CHECK — example's own curve vs the shipped eq::soft_clip, over [-10, 10]");
    println!(
        "  shipped SOFT_CLIP_THRESHOLD = {}",
        eq::SOFT_CLIP_THRESHOLD
    );
    println!("  swept threshold             = {T}");
    println!("  worst |difference|          = {worst:.3e} at x = {worst_at:.4}");
    if worst < 1e-6 && eq::SOFT_CLIP_THRESHOLD == T {
        println!("  -> ok: the shipped stage matches the curve the threshold was chosen from");
    } else {
        println!(
            "  -> MISMATCH: the shipped stage has drifted from the swept curve. Re-run the \
             block A sweep before trusting ONDA.md's threshold justification."
        );
    }

    // ---- What Equalizer actually bounds its output to, for block A's three cases ----
    println!();
    println!("BOUND — peak out of Equalizer (soft-clip included) for block A's cases");
    println!("  {:<44} {:>10}", "case", "peak");

    let band1_12 = EqGains::default();
    band1_12.set(1, 12.0);
    let band1_4 = EqGains::default();
    band1_4.set(1, 4.0);
    let all_bands_12 = EqGains::default();
    for b in 0..BAND_COUNT {
        all_bands_12.set(b, 12.0);
    }

    let cases: [(&str, Vec<f32>, EqGains); 3] = [
        (
            "1  62.5 Hz @0.70, band 1 +12 dB",
            sine(62.5, 0.70),
            band1_12,
        ),
        ("2  62.5 Hz @0.95, band 1 +4 dB", sine(62.5, 0.95), band1_4),
        (
            "3  multi-tone @0.95, all 10 bands +12 dB",
            multi_tone(0.95),
            all_bands_12,
        ),
    ];

    for (label, signal, gains) in cases {
        let out = through_eq(&signal, gains);
        println!("  {:<44} {:10.5}", label, peak(settled(&out)));
    }

    // ---- Transparency: at broadcast peak the stage must not engage at all ----
    println!();
    println!("TRANSPARENCY — flat EQ at the broadcast peak the threshold was chosen for");
    for (label, signal) in [
        ("62.5 Hz sine, peak 0.95", sine(62.5, 0.95)),
        ("multi-tone, peak 0.95", multi_tone(0.95)),
    ] {
        let out = through_eq(&signal, EqGains::default());
        let max_err = out
            .iter()
            .zip(&signal)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        println!("  {label:<44} max |out - in| = {max_err:.3e}");
    }
}
