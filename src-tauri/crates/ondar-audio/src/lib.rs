//! Ondar audio engine.
//!
//! ```text
//! HTTP (stream-download, bounded ring)
//!   → IcyReader (strips in-band titles)
//!   → rodio::Decoder (Symphonia)          decode thread
//!   → rtrb ring buffer  ───────────────────────────────  audio callback
//!   → Equalizer (10 × biquad peaking)  → Player → MixerDeviceSink (cpal)
//! ```
//!
//! Public surface: [`AudioEngine`] (commands + state snapshot), [`EngineEvent`] (what the UI
//! listens to), and the shared [`types`].

pub mod engine;
pub mod eq;
pub mod hls;
pub mod icy;
pub mod reconnect;
pub mod ring;
pub mod stream;
pub mod types;

pub use engine::{AudioCommand, AudioEngine};
pub use eq::{BAND_CENTERS_HZ, BAND_COUNT, EqGains, MAX_GAIN_DB};
pub use types::*;
