//! Ondar audio engine.
//!
//! ```text
//! HTTP (stream-download, bounded ring; on_progress stamps each open's network arrival)
//!   → ClockedReader (stamps the decoder build's first byte)
//!   → IcyReader (strips in-band titles)
//!   → AdtsReader (HTTP audio/aac* and audio/x-aac* only: realign, FFF9 → FFF1)
//!   → rodio::Decoder (Symphonia)          decode thread
//!   → rtrb ring buffer  ───────────────────────────────  audio callback
//!   → Equalizer (10 × biquad peaking)  → Player → MixerDeviceSink (cpal)
//! ```
//!
//! Public surface: [`AudioEngine`] (commands + state snapshot), [`EngineEvent`] (what the UI
//! listens to), and the shared [`types`].
// Tests assert by panicking and print what they measured; the lib build still checks every
// non-test line against the workspace table (src-tauri/Cargo.toml).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::float_cmp,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::items_after_statements,
        clippy::match_wildcard_for_single_variants,
        clippy::match_wild_err_arm,
        clippy::case_sensitive_file_extension_comparisons,
        clippy::format_push_string,
        clippy::manual_let_else,
        clippy::similar_names,
        clippy::unreadable_literal,
        clippy::unnecessary_wraps,
        clippy::format_collect,
        clippy::default_trait_access
    )
)]

pub mod adts;
mod build;
pub mod engine;
pub mod eq;
pub mod hls;
pub mod icy;
pub mod reconnect;
pub mod ring;
pub mod stream;
mod sync;
pub mod types;

pub use engine::{AudioCommand, AudioEngine};
pub use eq::{BAND_CENTERS_HZ, BAND_COUNT, EqGains, MAX_GAIN_DB};
pub use types::*;

/// `.claude/rules/audio.md` rule 15, as a test: no `log::` or `tracing::` call site in the Source
/// chain the audio callback pulls. The chain is `ring.rs` (the consumer side, `RingSource`),
/// `eq.rs` (`Equalizer`, the last stage) and the per-session converter adapter,
/// `engine.rs::output_chain` (the converter itself is rodio's `UniformSourceIterator`). Both log
/// sinks, stderr and the daily file, write synchronously, so one line in that chain is a blocking
/// call on the real-time thread. No lint can say "not in these functions", so a source scan does:
/// the two files outside their `#[cfg(test)]` modules, and `output_chain`'s body cut from
/// `engine.rs` by its signature (the cut itself is asserted, so a rename fails here, not silently).
/// Fails on a `log::` or `tracing::` token added to any of the three: a planted
/// `log::debug!("eq")` in `Equalizer::next` failed it before this commit (see its message).
#[cfg(test)]
mod callback_log_scan {
    fn non_test(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }

    #[test]
    fn the_callback_chain_has_no_log_call_site() {
        let engine = include_str!("engine.rs");
        let start = engine
            .find("fn output_chain(")
            .expect("engine.rs::output_chain exists (audio.md rule 15 names it)");
        let body_end = engine[start..]
            .find("\n}\n")
            .expect("output_chain closes at column 0");
        let adapter = &engine[start..start + body_end];
        for (name, text) in [
            ("ring.rs", non_test(include_str!("ring.rs"))),
            ("eq.rs", non_test(include_str!("eq.rs"))),
            ("engine.rs::output_chain", adapter),
        ] {
            for token in ["log::", "tracing::"] {
                assert!(
                    !text.contains(token),
                    "{name} has a `{token}` call site: the audio callback pulls this chain and both \
                     log sinks write synchronously (.claude/rules/audio.md, rule 15)"
                );
            }
        }
    }
}
