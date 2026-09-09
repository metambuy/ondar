//! Types that cross the Rust ↔ TypeScript boundary. `cargo test` regenerates the
//! `.ts` files in `src/bindings/` (see `.cargo/config.toml` at the repo root).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Machine-readable reason for a playback failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// DNS / TCP / TLS failure, or the connection dropped and reconnects were exhausted.
    Network,
    /// The server answered, but not with a usable stream (4xx/5xx, or a non-HTTP status line
    /// such as legacy Shoutcast v1 `ICY 200 OK`, which hyper rejects).
    Http,
    /// Symphonia could not identify the container/codec.
    UnsupportedFormat,
    /// Decoding failed mid-stream.
    Decode,
    /// No output device, or the device rejected every configuration.
    Device,
    /// The URL could not be parsed.
    InvalidUrl,
}

/// The single source of truth for what the UI shows in the transport area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlaybackState {
    Idle,
    Connecting,
    /// Connected and decoding, but the output ring buffer has not reached its fill target
    /// (initial fill or an underrun after a stall).
    Buffering,
    Playing,
    Paused,
    Reconnecting {
        attempt: u32,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

/// Facts about the open stream, emitted once the decoder has probed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StreamInfo {
    /// Container/codec description from the HTTP `Content-Type` (e.g. `audio/mpeg`, `audio/aac`).
    pub content_type: Option<String>,
    /// From the `icy-br` header, if the server sends it.
    pub bitrate_kbps: Option<u32>,
    /// From the `icy-name` header, if the server sends it.
    pub station_name: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// In-band ICY metadata (`StreamTitle`), emitted every time it changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IcyMetadata {
    pub title: Option<String>,
}

/// `stream-download`'s internal reconnect count (its own idle-`retry_timeout` recovery, not
/// one of our external `Backoff` attempts — see `stream::open`'s `Settings::on_reconnect`).
/// Cumulative for the current session (since the last `Play`); emitted whenever it advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReconnectInfo {
    pub count: u64,
}

/// One equalizer band as shown in the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EqBand {
    pub index: u8,
    pub center_hz: f32,
    pub gain_db: f32,
}

/// Everything the engine tells the outside world. The Tauri layer maps each variant to one
/// event name (`playback:state`, `playback:stream_info`, `playback:metadata`).
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    State(PlaybackState),
    StreamInfo(StreamInfo),
    Metadata(IcyMetadata),
    Reconnect(ReconnectInfo),
}
