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
    /// The server answered, and would answer the same way again: a non-HTTP status line
    /// (`ICY 200 OK`) or one of 401/403/404/410. The engine fails the session at once on a
    /// terminal error **on its first open** instead of entering the backoff — decided
    /// 2026-09-22 (M3a acceptance, item 8: the classifier alone left an ICY server in the
    /// reconnect loop for five attempts, 31 s and six requests); on a reconnect nothing is
    /// terminal, since a mount that was playing a minute ago can be 404 while its source
    /// restarts (`/code-review` finding 4, same day). `false` for what a retry can fix: DNS,
    /// TCP, TLS, timeouts, a 5xx, and the 4xx that change with time — 408, 429.
    pub terminal: bool,
    /// The server's `Retry-After`, delta-seconds form only (the HTTP-date form is not worth
    /// parsing for a radio stream). The backoff waits at least this long, capped in the engine.
    pub retry_after: Option<Duration>,
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
    client_builder(user_agent)
        .build()
        .expect("reqwest client with static configuration")
}

/// TCP connect bound. **Measured 2026-09-21 (M3a, G4b): this bound covers DNS resolution
/// too** — with a resolver that never answers, `open` returns `Network` when it elapses
/// (10.01 s at this value; `dns_resolution_is_inside_connect_timeout` pins it at 200 ms).
/// The M3 Step 0 report had derived the opposite from a census-client hang; the derivation
/// was wrong for reqwest 0.13's client, see ONDAR.md "Reconnect ownership and stream
/// timeouts".
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The engine client's configuration, before `build()`, so a test can add a DNS resolver
/// (`ClientBuilder::dns_resolver`) and still exercise the production settings.
pub(crate) fn client_builder(user_agent: &str) -> reqwest::ClientBuilder {
    client_builder_with_connect_timeout(user_agent, CONNECT_TIMEOUT)
}

/// Same, with the connect bound as a parameter: the test that pins "DNS is inside the
/// bound" uses 200 ms rather than waiting out the production 10 s.
pub(crate) fn client_builder_with_connect_timeout(
    user_agent: &str,
    connect_timeout: Duration,
) -> reqwest::ClientBuilder {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Icy-MetaData",
        reqwest::header::HeaderValue::from_static("1"),
    );
    reqwest::Client::builder()
        .user_agent(user_agent)
        .default_headers(headers)
        .connect_timeout(connect_timeout)
        // Without this, a dead connection that never sends a byte and never resets (common
        // when the network drops mid-stream) leaves the decode thread's read blocked
        // forever, so starvation is detected but the session never reconnects. Also covers
        // the wait for a first connect's response headers, not just body reads.
        .read_timeout(read_timeout())
}

pub fn parse_url(url: &str) -> Result<Url, StreamError> {
    Url::parse(url).map_err(|e| StreamError {
        code: ErrorCode::InvalidUrl,
        message: format!("invalid stream URL: {e}"),
        terminal: true,
        retry_after: None,
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
/// - `ResponseFailure` (a status the server did send, 4xx/5xx after `into_result`) → `Http`,
///   **terminal for 401, 403, 404 and 410** (the resource is not there for us; asking again
///   changes nothing), retriable for every other status — 5xx (an overloaded relay may
///   recover) and the 4xx that describe a moment, 408 and 429, whose `Retry-After` is carried;
/// - a `hyper::Error` anywhere in the chain with `is_parse()` (the legacy `ICY` status line,
///   or any other non-HTTP answer) → `Http`, **terminal** (the server will not become HTTP/1.1
///   on the next attempt);
/// - a root message with the non-HTTP wording (`NON_HTTP_WORDING`: the fallback if a reqwest
///   bump changes the chain's shape so the hyper error is no longer reachable) → `Http`,
///   terminal;
/// - everything else (DNS, TCP, TLS, timeouts) → `Network`, retriable.
///
/// The message carried to the UI is the whole chain, root last, so a log line shows the
/// cause and not just "error sending request".
fn classify_open_error(err: HttpStreamError<reqwest::Client>) -> StreamError {
    match err {
        // `into_result` already ran `error_for_status`: the server answered, with a status we
        // cannot play. `FetchError` wraps the reqwest error and the response.
        HttpStreamError::ResponseFailure(e) => {
            // The response is kept on the error: the status and headers are read from it.
            let response = e.response();
            StreamError {
                code: ErrorCode::Http,
                terminal: status_is_terminal(response.status()),
                retry_after: retry_after(response.headers()),
                message: e.to_string(),
            }
        }
        HttpStreamError::FetchFailure(e) => {
            let parse = chain_has_parse_error(&e) || root_looks_like_parse(&e);
            let (code, terminal) = if parse {
                (ErrorCode::Http, true)
            } else if e.is_status() {
                (ErrorCode::Http, is_client_error(&e))
            } else {
                (ErrorCode::Network, false)
            };
            StreamError {
                code,
                message: chain_message(&e),
                terminal,
                retry_after: None,
            }
        }
    }
}

/// The statuses that mean "not for you, not now, not later": no stream at this URL for us.
/// Everything else — 5xx, 408, 429, the rest of 4xx — may read differently on the next
/// attempt, so it keeps the backoff.
fn status_is_terminal(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403 | 404 | 410)
}

/// The same rule read off a `reqwest::Error` that carries a status.
fn is_client_error(e: &reqwest::Error) -> bool {
    e.status().is_some_and(status_is_terminal)
}

/// `Retry-After` in its delta-seconds form; `None` for an absent, HTTP-date or unparsable value.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
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

/// What hyper says about an answer that is not HTTP (`invalid HTTP version parsed` for an
/// `ICY 200 OK` line), lower-cased. The one table both classifier stages read, so the same
/// wording cannot be terminal in one and retriable in the other. `"status"` is **not** here:
/// a transport error whose root mentions a status (a TLS certificate status, a future reqwest
/// that folds a 503 into the fetch error) is not a non-HTTP answer, and the review (finding
/// 8, 2026-09-22) found the old stage-1 table matching it and making the 5xx branch below
/// unreachable.
const NON_HTTP_WORDING: [&str; 3] = ["invalid http", "http version", "parse"];

fn non_http_wording(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    NON_HTTP_WORDING.iter().any(|w| lower.contains(w))
}

fn root_looks_like_parse(e: &reqwest::Error) -> bool {
    let mut root: &(dyn std::error::Error + 'static) = e;
    while let Some(next) = root.source() {
        root = next;
    }
    non_http_wording(&root.to_string())
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
    let parse = non_http_wording(&message);
    let code = if parse || message.to_ascii_lowercase().contains("status") {
        ErrorCode::Http
    } else {
        ErrorCode::Network
    };
    // Only a non-HTTP answer is known to be terminal from text alone; a bare "status" could
    // be a 5xx.
    StreamError {
        code,
        message,
        terminal: parse,
        retry_after: None,
    }
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

    /// The error `open` returns against a server that cannot be played.
    fn open_err(url: &str) -> StreamError {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let client = build_client("Ondar/test");
        let url = parse_url(url).expect("url");
        match rt.block_on(open(&client, url, Arc::new(AtomicU64::new(0)))) {
            Ok(_) => panic!("open succeeded against a server that cannot be played"),
            Err(e) => e,
        }
    }

    /// `(code, terminal)` — the two things the engine branches on.
    fn open_code(url: &str) -> (ErrorCode, bool) {
        let e = open_err(url);
        (e.code, e.terminal)
    }

    /// A resolver that never answers — a stalled DNS server with no system change.
    struct HangingResolver;
    impl reqwest::dns::Resolve for HangingResolver {
        fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            Box::pin(std::future::pending())
        }
    }

    /// G4(b), measured rather than assumed: reqwest's `connect_timeout` bounds the DNS
    /// resolution as well as the TCP connect. With a resolver that never answers and a 200 ms
    /// bound, `open` returns `Network` promptly. **Fails if** DNS sits outside the bound (the
    /// call would hang and the outer 5 s guard would elapse) or if a stalled connect were
    /// classified as anything but `Network`. Measured first against the production 10 s:
    /// `open` returned at 10.01 s (2026-09-21).
    #[test]
    fn dns_resolution_is_inside_connect_timeout() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let client = client_builder_with_connect_timeout("Ondar/test", Duration::from_millis(200))
            .dns_resolver(std::sync::Arc::new(HangingResolver))
            .build()
            .expect("client");
        let url = parse_url("http://stalled.example.com/stream").expect("url");
        let started = std::time::Instant::now();
        let outcome = rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(5),
                open(&client, url, Arc::new(AtomicU64::new(0))),
            )
            .await
        });
        let elapsed = started.elapsed();
        let result = outcome
            .expect("open must return within the 5 s guard: DNS is not inside connect_timeout");
        let err = result
            .err()
            .expect("a stalled resolver cannot yield a stream");
        assert_eq!(err.code, ErrorCode::Network, "{}", err.message);
        assert!(
            elapsed < Duration::from_secs(2),
            "open took {elapsed:?} against a 200 ms connect bound"
        );
    }

    #[test]
    fn icy_status_line_is_http_not_network() {
        let url = serve_once(
            b"ICY 200 OK\r\nicy-name: synthetic shoutcast v1\r\nicy-br: 128\r\ncontent-type: audio/mpeg\r\n\r\n\
              0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        );
        assert_eq!(
            open_code(&url),
            (ErrorCode::Http, true),
            "ICY: Http and terminal"
        );
    }

    #[test]
    fn a_5xx_status_is_http() {
        let url = serve_once(
            b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        assert_eq!(
            open_code(&url),
            (ErrorCode::Http, false),
            "5xx: Http, retriable"
        );
    }

    #[test]
    fn a_4xx_status_is_http_and_terminal() {
        let url =
            serve_once(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        assert_eq!(
            open_code(&url),
            (ErrorCode::Http, true),
            "4xx: Http, terminal"
        );
    }

    /// Finding 4: a 429 describes a moment, not the URL. Fails on the "every 4xx is terminal"
    /// rule, and if `Retry-After` is not read off the response.
    #[test]
    fn a_429_is_http_retriable_and_carries_its_retry_after() {
        let url = serve_once(
            b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 7\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        let e = open_err(&url);
        assert_eq!(
            (e.code, e.terminal),
            (ErrorCode::Http, false),
            "{}",
            e.message
        );
        assert_eq!(e.retry_after, Some(Duration::from_secs(7)));
    }

    #[test]
    fn a_403_is_http_and_terminal() {
        let url =
            serve_once(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        assert_eq!(
            open_code(&url),
            (ErrorCode::Http, true),
            "403: Http, terminal"
        );
    }

    /// Finding 8: the two stages read one table, and "status" is not non-HTTP wording. Fails
    /// if `"status"` is put back in the table (the 503 wording becomes terminal) or if the
    /// hyper wording is dropped from it (the ICY line stops being terminal on the fallback).
    #[test]
    fn status_wording_is_not_a_non_http_answer() {
        assert!(!non_http_wording(
            "HTTP status server error (503 Service Unavailable)"
        ));
        assert!(non_http_wording("invalid HTTP version parsed"));
        let e = classify_http_error("HTTP status server error (503 Service Unavailable)".into());
        assert_eq!((e.code, e.terminal), (ErrorCode::Http, false));
        let e = classify_http_error("invalid HTTP version parsed".into());
        assert_eq!((e.code, e.terminal), (ErrorCode::Http, true));
    }

    #[test]
    fn a_refused_connection_is_network() {
        // Bind to learn a free port, then drop the listener so the connect is refused.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        assert_eq!(
            open_code(&format!("http://{addr}/stream")),
            (ErrorCode::Network, false)
        );
    }
}
