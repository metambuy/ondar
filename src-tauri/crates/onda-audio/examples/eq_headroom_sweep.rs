//! Sweeps a candidate soft-clip threshold for the EQ headroom fix (Phase 1 item 4), so the
//! constant is chosen from data rather than taste. Measures the two things that trade against
//! each other: what the shaper costs when nothing needs clipping, and what it bounds when
//! something does.
//!
//! Usage: `cargo run -p onda-audio --example eq_headroom_sweep --release`
//!
//! Measurement only — nothing here changes playback, and `eq.rs` is untouched. The shaper is
//! defined **locally** and parameterised by `t` on purpose: no constant for it exists in
//! `eq.rs` yet, and once one does, this example stays the cross-check that the shipped value
//! matches the curve that was actually swept.
//!
//! Everything is measured over the **settled tail** — the second half of each signal — the same
//! convention `eq.rs`'s `settled_rms` uses, so the biquads' startup transient is excluded.
//! Output is fixed-width and carries its own provenance line, so a run can be pasted into
//! ONDA.md verbatim.

use std::num::NonZero;
use std::process::Command;
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};

use onda_audio::eq::Equalizer;
use onda_audio::{BAND_COUNT, EqGains};

const RATE: u32 = 44_100;
/// 1 s per signal, matching `eq.rs`'s tests so Table 2's pre-shaper peaks are comparable to
/// the recorded M1 baselines.
const LEN: usize = 44_100;
const THRESHOLDS: [f32; 4] = [0.80, 0.85, 0.90, 0.95];
const MULTI_TONE_HZ: [f32; 5] = [62.5, 250.0, 1000.0, 4000.0, 8000.0];

/// The shaper under test: linear below `t`, then a `s/(1+s)` knee that is continuous at `t` and
/// asymptotic to 1.0, so it cannot produce an output above unity however hard it is driven.
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
    println!(
        "shaper: linear below t, then t + (1-t)*s/(1+s) where s = (|x|-t)/(1-t); asymptotic to 1.0"
    );

    // ---- Table 1: what the shaper costs when nothing needs clipping ----
    println!();
    println!("TABLE 1 — transparency cost (flat EQ, so the shaper is the only stage acting)");
    println!(
        "  {:<44} {:>6} {:>11} {:>11}",
        "signal", "t", "peak dB", "resid dB"
    );
    println!("  {:<44} {:>6} {:>11} {:>11}", "", "", "", "(THD proxy)");

    let signals: [(&str, Vec<f32>); 4] = [
        ("A  62.5 Hz sine, peak 0.95", sine(62.5, 0.95)),
        ("B  62.5 Hz sine, peak 1.00", sine(62.5, 1.00)),
        ("C  multi-tone 5 x sine, peak 0.95", multi_tone(0.95)),
        ("D  multi-tone 5 x sine, peak 1.00", multi_tone(1.00)),
    ];

    for (label, signal) in &signals {
        // Flat EQ: the shaper's input, and the reference the residual is fitted against.
        let pre = through_eq(signal, EqGains::default());
        let x = settled(&pre);
        for t in THRESHOLDS {
            let post = shaped(&pre, t);
            let y = settled(&post);
            let peak_ratio = if peak(x) > 0.0 {
                peak(y) / peak(x)
            } else {
                0.0
            };
            println!(
                "  {:<44} {:>6.2} {} {}",
                label,
                t,
                db(peak_ratio),
                residual_db(y, x)
            );
        }
    }

    // ---- Table 2: what the shaper bounds when the EQ drives it past unity ----
    println!();
    println!("TABLE 2 — what it bounds (EQ engaged, shaper on the EQ output)");
    println!(
        "  {:<40} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "case", "pre-shaper", "t=0.80", "t=0.85", "t=0.90", "t=0.95"
    );

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
        let pre = through_eq(&signal, gains);
        let pre_peak = peak(settled(&pre));
        let posts: Vec<String> = THRESHOLDS
            .iter()
            .map(|&t| format!("{:10.5}", peak(settled(&shaped(&pre, t)))))
            .collect();
        println!("  {:<40} {:10.5}{}", label, pre_peak, posts.concat());
    }

    println!();
    println!(
        "note: table 2's pre-shaper peaks for cases 1 and 2 are the recorded M1 baselines\n      \
         (2.787 and 1.5058). A mismatch means the EQ stage moved, not the shaper."
    );
}
