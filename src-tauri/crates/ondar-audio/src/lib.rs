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
