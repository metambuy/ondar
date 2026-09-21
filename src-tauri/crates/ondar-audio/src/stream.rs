//! Opening a radio stream over HTTP.
//!
//! `stream-download` gives us a `Read + Seek` handle backed by a bounded in-memory ring
//! (the writer blocks when it is full, so a live stream never grows memory), with prefetch
//! and its own transient-error retries. We add the ICY request header and read the ICY
//! response headers before handing the reader to the decoder.

use std::error::Error as _;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::Url;
use stream_download::http::{HttpStream, HttpStreamError};
use stream_download::source::DecodeError;
use stream_download::storage::bounded::BoundedStorageProvider;
use stream_download::storage::memory::MemoryStorageProvider;
use stream_download::{Settings, StreamDownload};
use tokio_util::sync::CancellationToken;

use crate::types::ErrorCode;

pub type Reader = StreamDownload<BoundedStorageProvider<MemoryStorageProvider>>;

/// Bytes to buffer before the decoder is allowed to start — 2.05 s at 128 kbit/s.
///
/// Bounded from both sides, and at 128 kbit/s the two bounds coincide:
///
/// - **Floor: one decoder read.** The decoder asks for 32768 B at a time (see
///   [`crate::icy::MAX_OBSERVED_READ`]). With less prefetch than that, it asks for a full read,
///   `stream-download` holds only part of it, and the decode thread blocks for the remainder at
///   1x while the ring drains. Measured at 16 KB, burst-less: the stream underruns 2.0 s into
///   playback **with no network fault at all**, and again on a cycle — an audible dropout after
///   every station start. This floor is a fixed byte count; it does not scale with bitrate.
/// - **Ceiling: the knee, `RING_SECONDS * byte_rate`.** Where the head start exactly fills the
///   ring. Above it the surplus cannot fit and becomes a standing offset behind the live edge:
///   measured ICY lag +0.194 s at 32 KB against +1.214 s at 48 KB.
///
/// At 128 kbit/s the floor is 32768 B and the knee is 32001 B, so 32 KB is at once the smallest
/// value that does not starve the decoder and the largest that costs no freshness.
///
/// Jitter tolerance is the third axis and it favours more prefetch: measured time from a stall
/// to the underrun is +0.41 s at 32 KB and +1.40 s at 48 KB. 32 KB is the deliberate trade —
/// the knee — not the maximum.
///
/// **The bounds scale differently, so a fixed value cannot be right everywhere:**
///
/// | bitrate | one decoder read | knee | with a fixed 32 KB |
/// |---|---|---|---|
/// | 64 kbit/s | 4.10 s | 16 KB | floor *exceeds* the knee; ~2.1 s of lag is structural |
/// | 128 kbit/s | 2.05 s | 31 KB | they coincide; optimal |
/// | 320 kbit/s | 0.82 s | 78 KB | safe, but only 0.82 s of buffer where the ring holds 2.0 s |
///
/// M3 refinement: radio-browser's station record carries `bitrate`, making
/// `prefetch_bytes = max(one_decoder_read, RING_SECONDS * bitrate / 8)` computable before
/// `open`. The `max` matters — the knee alone would starve the decoder at 64 kbit/s. Note the
/// first term is pinned to a dependency's internal behaviour and **must be re-verified on any
/// rodio or symphonia bump**.
///
/// Overridable via `ONDAR_PREFETCH_BYTES` (see [`prefetch_bytes`]) so stall testing can trade
/// startup latency against burst-size realism without a rebuild.
pub const PREFETCH_BYTES: u64 = 32 * 1024;
/// Size of the in-memory ring the HTTP body is written into (~16 s at 128 kbit/s; also the
/// maximum look-back Symphonia can use while probing, which needs only a few KB).
pub const BUFFER_BYTES: usize = 256 * 1024;
/// `reqwest`'s per-read timeout — also covers the wait for a first connect's response headers
/// (see `PendingRequest::poll` in `reqwest`), not just body reads. A backstop for a reconnect
/// that connects and then never delivers a byte. Overridable via `ONDAR_READ_TIMEOUT_SECS`.
const READ_TIMEOUT_SECS: u64 = 20;
/// `stream-download`'s own idle-reconnect timeout: no new data for this long triggers an
/// internal reconnect (see `Settings::retry_timeout`). Overridable via
/// `ONDAR_RETRY_TIMEOUT_SECS`.
const RETRY_TIMEOUT_SECS: u64 = 5;

fn env_duration_secs(var: &str, default_secs: u64) -> Duration {
    Duration::from_secs(
        std::env::var(var)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(default_secs),
    )
}

/// `read_timeout` must stay strictly greater than `retry_timeout`: `stream-download`'s own
/// idle reconnect (`handle_reconnect`) only runs when its outer `timeout(retry_timeout, ..)`
/// elapses, which requires the read to hang rather than error. If `reqwest`'s `read_timeout`
/// fires first, the body stream yields a fast `Err` on every subsequent poll instead of
/// hanging (`reqwest`'s `ReadTimeoutBody` never resets its sleep on that path), so
/// `stream-download`'s `handle_bytes` logs and retries in a tight loop forever — a real spin,
/// not just a slow reconnect. See README "Stall testing" (Run A) for the measured case.
/// Resolved and clamped once per process; both `read_timeout()` and `retry_timeout()` read
/// from the same memoized pair so they can never observe different env snapshots.
fn resolved_timeouts() -> (Duration, Duration) {
    static TIMEOUTS: OnceLock<(Duration, Duration)> = OnceLock::new();
    *TIMEOUTS.get_or_init(|| {
        let read = env_duration_secs("ONDAR_READ_TIMEOUT_SECS", READ_TIMEOUT_SECS);
        let retry = env_duration_secs("ONDAR_RETRY_TIMEOUT_SECS", RETRY_TIMEOUT_SECS);
        if read <= retry {
            let clamped = retry * 2;
            log::warn!(
                "read_timeout ({read:?}) <= retry_timeout ({retry:?}); this can never let \
                 stream-download's idle reconnect run and spins instead on a fast read error \
                 (see README \"Stall testing\"). Clamping read_timeout to {clamped:?}."
            );
            (clamped, retry)
        } else {
            (read, retry)
        }
    })
}

fn read_timeout() -> Duration {
    resolved_timeouts().0
}

pub fn retry_timeout() -> Duration {
    resolved_timeouts().1
}

pub fn prefetch_bytes() -> u64 {
    std::env::var("ONDAR_PREFETCH_BYTES")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(PREFETCH_BYTES)
}

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
        .connect_timeout(Duration::from_secs(10))
        // Without this, a dead connection that never sends a byte and never resets (common
        // when the network drops mid-stream) leaves the decode thread's read blocked
        // forever, so starvation is detected but the session never reconnects. Also covers
        // the wait for a first connect's response headers, not just body reads.
        .read_timeout(read_timeout())
        .build()
        .expect("reqwest client with static configuration")
}

pub fn parse_url(url: &str) -> Result<Url, StreamError> {
    Url::parse(url).map_err(|e| StreamError {
        code: ErrorCode::InvalidUrl,
        message: format!("invalid stream URL: {e}"),
    })
}

/// Connect and return a reader once `PREFETCH_BYTES` have arrived. `reconnect_count` is
/// advanced every time `stream-download` reconnects internally (idle `retry_timeout`, not one
/// of our own external retries) — see `Settings::on_reconnect` below and `SessionCtx` in
/// `engine.rs`, which is what actually surfaces it as an event.
pub async fn open(
    client: &reqwest::Client,
    url: Url,
    reconnect_count: Arc<AtomicU64>,
) -> Result<OpenedStream, StreamError> {
    let stream = match HttpStream::new(client.clone(), url).await {
        Ok(s) => s,
        Err(e) => return Err(classify_open_error(e)),
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
    let settings = Settings::default()
        .prefetch_bytes(prefetch_bytes())
        .retry_timeout(retry_timeout())
        .on_reconnect(
            move |_stream: &HttpStream<reqwest::Client>, _token: &CancellationToken| {
                let n = reconnect_count.fetch_add(1, Ordering::Relaxed) + 1;
                log::debug!("stream-download internal reconnect (session count now {n})");
            },
        );

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

/// Classify a failed open. The `reqwest::Error` is inspected **as a type**, not through its
/// `Display`: reqwest renders a rejected status line as "error sending request for url (…)"
/// and keeps the cause — hyper's parse error — in the `source()` chain, so classifying the
/// top-level string (what this did until M3a) reported a Shoutcast v1 `ICY 200 OK` server as
/// `Network` and sent the session into the reconnect loop (measured 2026-09-21, M3 Step 0
/// P5, against a synthetic ICY server). The rule, in order:
///
/// - `ResponseFailure` (a status the server did send, 4xx/5xx after `into_result`) → `Http`;
/// - a `hyper::Error` anywhere in the chain with `is_parse()` (the legacy `ICY` status line,
///   or any other non-HTTP answer) → `Http`;
/// - a root message mentioning an invalid HTTP version or status (the fallback if a reqwest
///   bump changes the chain's shape so the hyper error is no longer reachable) → `Http`;
/// - everything else (DNS, TCP, TLS, timeouts) → `Network`.
///
/// The message carried to the UI is the whole chain, root last, so a log line shows the
/// cause and not just "error sending request".
fn classify_open_error(err: HttpStreamError<reqwest::Client>) -> StreamError {
    match err {
        // `into_result` already ran `error_for_status`: the server answered, with a status we
        // cannot play. `FetchError` wraps the reqwest error and the response.
        HttpStreamError::ResponseFailure(e) => StreamError {
            code: ErrorCode::Http,
            message: e.to_string(),
        },
        HttpStreamError::FetchFailure(e) => {
            let code = if e.is_status() || chain_has_parse_error(&e) || root_looks_like_parse(&e) {
                ErrorCode::Http
            } else {
                ErrorCode::Network
            };
            StreamError {
                code,
                message: chain_message(&e),
            }
        }
    }
}

fn chain_has_parse_error(e: &reqwest::Error) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(h) = err.downcast_ref::<hyper::Error>()
            && h.is_parse()
        {
            return true;
        }
        cur = err.source();
    }
    false
}

fn root_looks_like_parse(e: &reqwest::Error) -> bool {
    let mut root: &(dyn std::error::Error + 'static) = e;
    while let Some(next) = root.source() {
        root = next;
    }
    let lower = root.to_string().to_ascii_lowercase();
    lower.contains("invalid http") || lower.contains("http version") || lower.contains("status")
}

/// "top: cause: root" — every link of the `source()` chain, so the UI and the log see the
/// reason and not reqwest's outer wrapper alone.
fn chain_message(e: &reqwest::Error) -> String {
    let mut parts = vec![e.to_string()];
    let mut cur = e.source();
    while let Some(err) = cur {
        parts.push(err.to_string());
        cur = err.source();
    }
    parts.join(": ")
}

/// Best-effort mapping from an error *string* to a user-facing code — the second open
/// stage (`StreamDownload::from_stream`) only exposes its error as text through
/// `decode_error()`. Kept for that path; the first stage classifies the typed error above.
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

#[cfg(test)]
mod tests {
    //! The classifier is pinned against real sockets, not strings: a listener on 127.0.0.1
    //! answers what a Shoutcast v1 server, an HTTP server and a dead port answer, and the
    //! assertion is on the `code` the UI branches on. `icy_status_line_is_http_not_network`
    //! fails if the classifier goes back to reading the top-level `Display` (which says
    //! "error sending request" and classifies as `Network`) — the defect M3 Step 0 measured.

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use super::*;

    /// One-shot server: accept once, read the request head, write `response`, hold the
    /// socket briefly so the client sees a complete answer, close.
    fn serve_once(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(response);
            let _ = sock.flush();
            std::thread::sleep(Duration::from_millis(200));
        });
        format!("http://{addr}/stream")
    }

    fn open_code(url: &str) -> ErrorCode {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let client = build_client("Ondar/test");
        let url = parse_url(url).expect("url");
        match rt.block_on(open(&client, url, Arc::new(AtomicU64::new(0)))) {
            Ok(_) => panic!("open succeeded against a server that cannot be played"),
            Err(e) => e.code,
        }
    }

    #[test]
    fn icy_status_line_is_http_not_network() {
        let url = serve_once(
            b"ICY 200 OK\r\nicy-name: synthetic shoutcast v1\r\nicy-br: 128\r\ncontent-type: audio/mpeg\r\n\r\n\
              0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        );
        assert_eq!(open_code(&url), ErrorCode::Http);
    }

    #[test]
    fn a_5xx_status_is_http() {
        let url = serve_once(
            b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        assert_eq!(open_code(&url), ErrorCode::Http);
    }

    #[test]
    fn a_refused_connection_is_network() {
        // Bind to learn a free port, then drop the listener so the connect is refused.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        assert_eq!(
            open_code(&format!("http://{addr}/stream")),
            ErrorCode::Network
        );
    }
}
