//! Opening a radio stream over HTTP.
//!
//! `stream-download` gives us a `Read + Seek` handle backed by a bounded in-memory ring
//! (the writer blocks when it is full, so a live stream never grows memory), with prefetch
//! and its own transient-error retries. We add the ICY request header and read the ICY
//! response headers before handing the reader to the decoder.

use std::num::NonZeroUsize;

use reqwest::Url;
use stream_download::http::HttpStream;
use stream_download::source::DecodeError;
use stream_download::storage::bounded::BoundedStorageProvider;
use stream_download::storage::memory::MemoryStorageProvider;
use stream_download::{Settings, StreamDownload};

use crate::types::ErrorCode;

pub type Reader = StreamDownload<BoundedStorageProvider<MemoryStorageProvider>>;

/// Bytes to buffer before the decoder is allowed to start. At 128 kbit/s this is ~3 s.
pub const PREFETCH_BYTES: u64 = 48 * 1024;
/// Size of the in-memory ring the HTTP body is written into (~16 s at 128 kbit/s; also the
/// maximum look-back Symphonia can use while probing, which needs only a few KB).
pub const BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StreamError {
    pub code: ErrorCode,
    pub message: String,
}

pub struct OpenedStream {
    pub reader: Reader,
    pub metaint: Option<usize>,
    pub content_type: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub station_name: Option<String>,
}

/// Build the one HTTP client the engine uses for its lifetime.
pub fn build_client(user_agent: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Icy-MetaData",
        reqwest::header::HeaderValue::from_static("1"),
    );
    reqwest::Client::builder()
        .user_agent(user_agent)
        .default_headers(headers)
        .connect_timeout(std::time::Duration::from_secs(10))
        // Without this, a dead connection that never sends a byte and never resets (common
        // when the network drops mid-stream) leaves the decode thread's read blocked
        // forever, so starvation is detected but the session never reconnects.
        .read_timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("reqwest client with static configuration")
}

pub fn parse_url(url: &str) -> Result<Url, StreamError> {
    Url::parse(url).map_err(|e| StreamError {
        code: ErrorCode::InvalidUrl,
        message: format!("invalid stream URL: {e}"),
    })
}

/// Connect and return a reader once `PREFETCH_BYTES` have arrived.
pub async fn open(client: &reqwest::Client, url: Url) -> Result<OpenedStream, StreamError> {
    let stream = match HttpStream::new(client.clone(), url).await {
        Ok(s) => s,
        Err(e) => return Err(classify_http_error(e.decode_error().await)),
    };

    let metaint = stream
        .header("icy-metaint")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&m| m > 0);
    let bitrate_kbps = stream
        .header("icy-br")
        .and_then(|v| v.trim().parse::<u32>().ok());
    let station_name = stream.header("icy-name").map(|s| s.trim().to_string());
    let content_type = stream
        .content_type()
        .as_ref()
        .map(|ct| format!("{}/{}", ct.r#type, ct.subtype));

    let storage = BoundedStorageProvider::new(
        MemoryStorageProvider,
        NonZeroUsize::new(BUFFER_BYTES).expect("non-zero buffer"),
    );
    let settings = Settings::default().prefetch_bytes(PREFETCH_BYTES);

    let reader = match StreamDownload::from_stream(stream, storage, settings).await {
        Ok(r) => r,
        Err(e) => {
            let message = e.decode_error().await;
            return Err(classify_http_error(message));
        }
    };

    Ok(OpenedStream {
        reader,
        metaint,
        content_type,
        bitrate_kbps,
        station_name,
    })
}

/// Best-effort mapping from an error string to a user-facing code. `hyper` rejects the
/// legacy `ICY 200 OK` status line used by Shoutcast v1 servers; that surfaces as an
/// "invalid HTTP version" style parse error, which we report as `Http`, not `Network`.
fn classify_http_error(message: String) -> StreamError {
    let lower = message.to_ascii_lowercase();
    let code = if lower.contains("status")
        || lower.contains("invalid http")
        || lower.contains("parse")
        || lower.contains("version")
    {
        ErrorCode::Http
    } else {
        ErrorCode::Network
    };
    StreamError { code, message }
}
