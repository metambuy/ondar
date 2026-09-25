//! HLS, the ADTS half (M3c). A live playlist is refreshed, its ADTS segments are normalised and
//! concatenated into one byte stream, and that stream is fed to the same reader, decoder, ring,
//! converter and EQ the Icecast path uses. Nothing downstream of the reader knows the
//! difference.
//!
//! ```text
//! decode thread (unchanged)                  ondar-net tokio runtime (exists, 2 workers)
//! run_session ─ block_on(stream::open) ──►   HttpStream GET (as today)
//!                                             ├ Content-Type not HLS → today's path, byte for byte
//!                                             └ HLS type → drop it → hls::open:
//!                                                 own GET (final URL, gzip) → master → variant
//!                                                 → media → first segment → sniff
//!                                                 ├ refuse → StreamError{UnsupportedFormat, terminal}
//!                                                 └ ADTS → spawn fetch task ─► mpsc(2) ─► HlsSource
//! Reader  ◄──────────  StreamDownload::from_stream(HlsSource, bounded 256 KB, HLS Settings)
//! ```
//!
//! Three pure layers and one that does I/O:
//! - [`playlist`]: the parser for the nine tags the player needs, the variant choice, and the
//!   refresh planner that decides which sequence numbers to fetch and how long to wait.
//! - [`segment`]: per-segment normalisation — the ID3 skip, the ADTS walk that clears the
//!   MPEG-2 ID bit and drops a CRC, the format guard, the container sniff, and gunzip.
//! - this file: [`open`] (the requests that decide whether the station can play at all, and
//!   the first segment), the **fetch task** that executes the planner's steps on the engine's
//!   runtime, and [`HlsSource`], the `SourceStream` that hands the task's bytes to
//!   `stream-download`. Every request the layer makes logs one
//!   `hls request kind= host= status= bytes= ms=` line (plan review R4): TLS hides them from
//!   any proxy on a live run, so the acceptance counts are read from these lines.
//!
//! **Detection is by the response, never by the record's `hls` flag** — two census `.m3u8`
//! URLs flagged `hls == 0` answered plain ADTS. **Every HLS open is two requests for the first
//! playlist** (review R1): `HttpStream` keeps the URL it was given, not the final one after
//! redirects, and relative URIs must join against the final one, so `open` fetches the playlist
//! again itself. **The stream never yields an error into `stream-download`**: a fatal condition
//! ends the source (the channel closes, the cause is logged as `hls task ended reason=`), the
//! decoder sees EOF, and `run_session`'s "stream ended" path — the session's own backoff —
//! reopens. That is why the HLS `Settings` may set `retry_timeout` above `read_timeout`: CLAUDE.md
//! invariant 4 guards the `HttpStream` body, whose spin needs a source that yields `Err` again
//! and again, and this one never does.

pub mod playlist;
pub mod segment;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_core::Stream;
use reqwest::{Client, Url};
use stream_download::source::SourceStream;
use stream_download::{Settings, StreamDownload};
use tokio::sync::mpsc;

use crate::stream::{self, OpenedStream, StreamError};
use crate::types::ErrorCode;
use playlist::{EndCause, MediaPlaylist, Planner, Playlist, PlaylistError, Segment, Step};
use segment::{Container, FormatGuard, Normalised};

/// The content types a server uses for an HLS playlist, compared case-insensitively with any
/// `; charset=` parameter dropped. The census saw two spellings across ten stations.
pub const HLS_CONTENT_TYPES: [&str; 4] = [
    "application/vnd.apple.mpegurl",
    "application/x-mpegurl",
    "audio/mpegurl",
    "audio/x-mpegurl",
];

/// Whole-request bound on a playlist fetch, on top of the client's connect and read bounds
/// (the census lesson: `connect_timeout` alone left a probe waiting 12 min on DNS).
pub const PLAYLIST_TIMEOUT: Duration = Duration::from_secs(10);
/// A playlist body over this is refused; the largest in the census was 42 KB (300 segments).
pub const PLAYLIST_MAX_BYTES: usize = 1024 * 1024;
/// A segment body over this is dropped as a gap; the largest in the census was 657 KB, video.
pub const SEGMENT_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Segments queued between the fetch task and the reader. Two: one being read, one ahead.
const CHANNEL_SEGMENTS: usize = 2;
/// A failed segment fetch is retried at this step while the segment is still within its
/// target duration of the first try; after that it is a logged gap (decision D3).
const SEGMENT_RETRY_STEP: Duration = Duration::from_secs(1);
/// A 404 on a segment the playlist listed is retried this many times at
/// [`SEGMENT_RETRY_STEP`], then skipped: at the live edge it is often a CDN that has not yet
/// received a segment the origin already published (review 2, 2026-09-25, finding 4).
const NOT_FOUND_RETRIES: u32 = 2;
/// Headroom added to the HLS `retry_timeout` above the longest silence the task can be in.
const RETRY_TIMEOUT_HEADROOM: Duration = Duration::from_secs(5);

/// Whether a response's `Content-Type` names an HLS playlist.
pub fn is_hls_content_type(content_type: &str) -> bool {
    let bare = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    HLS_CONTENT_TYPES.contains(&bare.as_str())
}

/// Whole-request bound on a segment fetch: twice the target duration, at least 10 s.
pub fn segment_timeout(target_duration: Duration) -> Duration {
    // On the bounded TD: `Duration * 2` on a raw remote `TARGETDURATION` overflowed (review
    // 2026-09-25, finding 3 — "overflow when multiplying duration by scalar" at u64::MAX).
    (playlist::bounded_target_duration(target_duration) * 2).max(Duration::from_secs(10))
}

/// `stream-download`'s idle timeout for an HLS source: the longest the task can legitimately go
/// without yielding a byte — the stall bound (waiting on a window that has not advanced) plus
/// one segment timeout (the fetch that follows) — plus headroom. Deliberately above
/// `read_timeout`; see the module doc for why invariant 4 does not apply here.
pub fn retry_timeout_for(target_duration: Duration) -> Duration {
    playlist::stall_bound(target_duration)
        + segment_timeout(target_duration)
        + RETRY_TIMEOUT_HEADROOM
}

// ---------------------------------------------------------------------------------------------
// The source

/// The `SourceStream` `stream-download` reads: the fetch task's normalised segments, in order.
/// It never yields an `Err`; the channel closing is the end of the stream.
pub struct HlsSource {
    rx: mpsc::Receiver<Bytes>,
}

/// The error type the trait requires. Never produced by a running source; `create` returns it
/// because an `HlsSource` is only ever built by [`open`].
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HlsSourceError(String);

impl stream_download::source::DecodeError for HlsSourceError {}

impl Stream for HlsSource {
    type Item = Result<Bytes, HlsSourceError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `poll_recv` is cancel-safe: a `timeout(retry_timeout, next())` that elapses loses
        // nothing (finding F4 is about the ICY settings' 5 s, not about this).
        self.rx.poll_recv(cx).map(|item| item.map(Ok))
    }
}

impl SourceStream for HlsSource {
    type Params = ();
    type StreamCreationError = HlsSourceError;

    async fn create(_params: ()) -> Result<Self, HlsSourceError> {
        Err(HlsSourceError(
            "an HlsSource is built by hls::open, not by create".to_string(),
        ))
    }

    fn content_length(&self) -> Option<u64> {
        None
    }

    async fn seek_range(&mut self, _start: u64, _end: Option<u64>) -> std::io::Result<()> {
        // Never called: `supports_seek` is false.
        Ok(())
    }

    async fn reconnect(&mut self, _current_position: u64) -> std::io::Result<()> {
        // Reached only if `retry_timeout` elapses with no byte, which `retry_timeout_for` puts
        // above the task's longest legitimate silence. Nothing to reconnect: the task owns the
        // network. Returning Ok lets the download loop keep polling the channel, and lets
        // `on_reconnect` count the event (see `open`).
        Ok(())
    }

    fn supports_seek(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------------------------
// Requests

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Master,
    Media,
    Segment,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Master => "master",
            Kind::Media => "media",
            Kind::Segment => "segment",
        })
    }
}

/// The one log line per request (review R4). `status` is the code or `err:<cause>`; `bytes`
/// the body after gunzip, or `head` for a sniffed-and-dropped segment.
fn log_request(kind: Kind, url: &Url, status: &str, bytes: &str, started: Instant) {
    log::info!(
        "hls request kind={kind} host={} status={status} bytes={bytes} ms={}",
        url.host_str().unwrap_or("-"),
        started.elapsed().as_millis()
    );
}

fn unsupported(message: impl Into<String>) -> StreamError {
    StreamError {
        code: ErrorCode::UnsupportedFormat,
        message: message.into(),
        terminal: true,
        retry_after: None,
    }
}

fn network(message: impl Into<String>) -> StreamError {
    StreamError {
        code: ErrorCode::Network,
        message: message.into(),
        terminal: false,
        retry_after: None,
    }
}

/// A `reqwest` failure on a request, classified as `stream::open` classifies its own: a
/// status the server sent is `Http` (terminal for 401/403/404/410, its `Retry-After` carried),
/// everything else `Network`.
fn classify(kind: Kind, url: &Url, e: &reqwest::Error) -> StreamError {
    let message = format!(
        "HLS {kind} request to {url} failed: {}",
        stream::chain_message(e)
    );
    match e.status() {
        Some(status) => StreamError {
            code: ErrorCode::Http,
            message,
            terminal: stream::status_is_terminal(status),
            retry_after: None,
        },
        None => network(message),
    }
}

/// A status the server sent that is not a success.
fn status_error(kind: Kind, response: &reqwest::Response) -> StreamError {
    let status = response.status();
    StreamError {
        code: ErrorCode::Http,
        message: format!(
            "HLS {kind} request to {} answered HTTP {status}",
            response.url()
        ),
        terminal: stream::status_is_terminal(status),
        retry_after: stream::retry_after(response.headers()),
    }
}

struct FetchedPlaylist {
    /// The final URL, after redirects: the base for the playlist's relative URIs.
    url: Url,
    text: String,
}

/// GET a playlist: whole-request timeout, body capped, gunzipped by the response's
/// `Content-Encoding`. `kind` is what the caller expects; the log line uses what it turned out
/// to be, so the caller logs after parsing.
async fn fetch_playlist(
    client: &Client,
    url: &Url,
    expected: Kind,
) -> Result<(FetchedPlaylist, Instant), StreamError> {
    let started = Instant::now();
    let response = client
        .get(url.clone())
        .timeout(PLAYLIST_TIMEOUT)
        .send()
        .await
        .map_err(|e| {
            log_request(
                expected,
                url,
                &format!("err:{}", stream::chain_message(&e)),
                "0",
                started,
            );
            classify(expected, url, &e)
        })?;
    if !response.status().is_success() {
        let e = status_error(expected, &response);
        log_request(expected, url, response.status().as_str(), "0", started);
        return Err(e);
    }
    let final_url = response.url().clone();
    let gzipped = is_gzip(response.headers());
    let body = read_body(response, PLAYLIST_MAX_BYTES)
        .await
        .map_err(|e| network(format!("HLS playlist {final_url}: {e}")))?;
    let Some(body) = body else {
        return Err(unsupported(format!(
            "HLS playlist {final_url} is over {PLAYLIST_MAX_BYTES} bytes"
        )));
    };
    let bytes = if gzipped {
        // Capped as the compressed body is: a body that inflates past the cap is refused as
        // an over-cap body is (review 2, finding 1).
        segment::gunzip(&body, PLAYLIST_MAX_BYTES).map_err(|e| match e {
            segment::GunzipError::TooLarge(_) => unsupported(format!(
                "HLS playlist {final_url} is over {PLAYLIST_MAX_BYTES} bytes inflated"
            )),
            segment::GunzipError::Invalid(e) => unsupported(format!(
                "HLS playlist {final_url}: content-encoding gzip but {e}"
            )),
        })?
    } else {
        body
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok((
        FetchedPlaylist {
            url: final_url,
            text,
        },
        started,
    ))
}

fn is_gzip(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("gzip"))
}

/// Read a body up to `max` bytes; `None` when it is longer.
async fn read_body(
    mut response: reqwest::Response,
    max: usize,
) -> reqwest::Result<Option<Vec<u8>>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > max {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// Parse a fetched body as a playlist, logging the request with the kind it turned out to be.
/// Parse a fetched body as a playlist, logging the request with the kind it turned out to be
/// on success and the kind the caller asked for on a refusal (review 2026-09-25, finding 7).
fn parse_fetched(
    fetched: &FetchedPlaylist,
    started: Instant,
    expected: Kind,
) -> Result<Playlist, StreamError> {
    let parsed = playlist::parse(&fetched.text, &fetched.url);
    log_request(
        logged_kind(expected, parsed.as_ref().ok()),
        &fetched.url,
        "200",
        &fetched.text.len().to_string(),
        started,
    );
    parsed.map_err(playlist_refused)
}

/// The `kind=` a playlist request logs: what the body turned out to be when it parsed, and
/// otherwise what the caller asked for — never a fixed `master` for a media reload's failure
/// (review 2026-09-25, finding 7: X6's log showed its media reload failures as `kind=master`).
fn logged_kind(expected: Kind, parsed: Option<&Playlist>) -> Kind {
    match parsed {
        Some(Playlist::Master(_)) => Kind::Master,
        Some(Playlist::Media(_)) => Kind::Media,
        None => expected,
    }
}

fn playlist_refused(e: PlaylistError) -> StreamError {
    // Every variant's Display is the message the page renders (plan §2.5).
    unsupported(e.to_string())
}

/// The outcome of a segment fetch.
enum SegmentFetch {
    /// The whole body, plus its `Content-Type` as served.
    Body {
        bytes: Vec<u8>,
        content_type: Option<String>,
    },
    /// Sniffed on its first bytes and dropped: not ADTS.
    Refused(Container),
    /// 404 or 410: the origin no longer has this segment — the window moved on (or, for a
    /// 404, the edge does not have it yet). Not an error of the station: `open` tries the next
    /// pending segment; the task skips a 410 at once and a 404 after [`NOT_FOUND_RETRIES`]
    /// (review 2026-09-25, finding 4; review 2, finding 4). 401/403 are not this: access
    /// denial is a `StreamError`, terminal at open, as for a playlist.
    Evicted(u16),
}

/// GET a segment. With `sniff_first`, a plain body is read only to the ID3 tags plus
/// [`segment::SNIFF_LEN`] bytes before the container is named, and a segment that is not ADTS
/// is dropped there. A **gzipped** refused segment is downloaded whole, ≤ `SEGMENT_MAX_BYTES`
/// compressed and inflated, before its sniff: the container is only visible after inflating
/// (review 2, 2026-09-25, finding 1 — P4 saw no gzipped segment, so no streaming inflate).
/// The `Content-Encoding` is honoured for segments too.
async fn fetch_segment(
    client: &Client,
    seg: &Segment,
    target_duration: Duration,
    sniff_first: bool,
) -> Result<SegmentFetch, StreamError> {
    let started = Instant::now();
    let mut response = client
        .get(seg.uri.clone())
        .timeout(segment_timeout(target_duration))
        .send()
        .await
        .map_err(|e| {
            log_request(
                Kind::Segment,
                &seg.uri,
                &format!("err:{}", stream::chain_message(&e)),
                "0",
                started,
            );
            classify(Kind::Segment, &seg.uri, &e)
        })?;
    if !response.status().is_success() {
        let status = response.status();
        if matches!(status.as_u16(), 404 | 410) {
            log_request(Kind::Segment, &seg.uri, status.as_str(), "0", started);
            return Ok(SegmentFetch::Evicted(status.as_u16()));
        }
        let e = status_error(Kind::Segment, &response);
        log_request(
            Kind::Segment,
            &seg.uri,
            response.status().as_str(),
            "0",
            started,
        );
        return Err(e);
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string());
    let gzipped = is_gzip(response.headers());

    let mut body: Vec<u8> = Vec::new();
    // A plain body is sniffed as it arrives, so a refused segment is dropped at its head; a
    // gzipped body can only be sniffed after inflating, below (review 2026-09-25, finding 5:
    // one flag served both, and the gzipped case was never sniffed).
    let mut sniffed_early = false;
    loop {
        if sniff_first && !gzipped && !sniffed_early {
            let tags = segment::id3_end(&body);
            // The tags are complete once `id3_end` lands inside the buffer, and the sniff
            // needs `SNIFF_LEN` bytes after them.
            if tags < body.len() && body.len() >= tags + segment::SNIFF_LEN {
                sniffed_early = true;
                let container = segment::sniff(&body[tags..]);
                if container != Container::Adts {
                    log_request(Kind::Segment, &seg.uri, "200", "head", started);
                    drop(response);
                    return Ok(SegmentFetch::Refused(container));
                }
            }
        }
        let chunk = match response.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                log_request(
                    Kind::Segment,
                    &seg.uri,
                    &format!("err:{}", stream::chain_message(&e)),
                    &body.len().to_string(),
                    started,
                );
                return Err(classify(Kind::Segment, &seg.uri, &e));
            }
        };
        if body.len() + chunk.len() > SEGMENT_MAX_BYTES {
            log_request(Kind::Segment, &seg.uri, "200", "over-max", started);
            return Err(network(format!(
                "HLS segment {} is over {SEGMENT_MAX_BYTES} bytes",
                seg.uri
            )));
        }
        body.extend_from_slice(&chunk);
    }
    let body = if gzipped {
        segment::gunzip(&body, SEGMENT_MAX_BYTES)
            .map_err(|e| network(format!("HLS segment {}: gzip: {e}", seg.uri)))?
    } else {
        body
    };
    if sniff_first && !sniffed_early {
        // The body ended before the sniff had its bytes (a short segment), or it was gzipped
        // and is only now inflated: sniff whatever there is.
        let tags = segment::id3_end(&body);
        let container = segment::sniff(&body[tags..]);
        if container != Container::Adts {
            log_request(Kind::Segment, &seg.uri, "200", "head", started);
            return Ok(SegmentFetch::Refused(container));
        }
    }
    log_request(
        Kind::Segment,
        &seg.uri,
        "200",
        &body.len().to_string(),
        started,
    );
    Ok(SegmentFetch::Body {
        bytes: body,
        content_type,
    })
}

fn refused_container(c: Container) -> StreamError {
    unsupported(match c {
        Container::MpegTs => "HLS with MPEG-TS segments is not supported yet",
        Container::Fmp4 => "HLS with fMP4 segments is not supported yet",
        Container::Unknown => "HLS segment format not recognised",
        Container::Adts => unreachable!("an ADTS segment is not refused"),
    })
}

// ---------------------------------------------------------------------------------------------
// Open

/// Open an HLS station whose playlist `HttpStream` has just fetched and dropped. Fetches the
/// playlist again (the final URL is the base for relative URIs), chooses a variant, fetches the
/// media playlist, fetches and sniffs the first segment, and — if it is ADTS — spawns the
/// fetch task on the current runtime and returns the reader `run_session` expects. Every
/// refusal is a terminal `UnsupportedFormat` with the message the page renders; a failed
/// request is classified as the Icecast open classifies its own.
pub async fn open(
    client: &Client,
    url: Url,
    reconnect_count: Arc<AtomicU64>,
    prefetch_bytes: u64,
) -> Result<OpenedStream, StreamError> {
    let (fetched, started) = fetch_playlist(client, &url, Kind::Master).await?;
    let mut bitrate_kbps = None;
    let media_url;
    let media = match parse_fetched(&fetched, started, Kind::Master)? {
        Playlist::Media(m) => {
            media_url = fetched.url.clone();
            m
        }
        Playlist::Master(master) => {
            let variant =
                playlist::choose_variant(&master).map_err(|e| unsupported(e.to_string()))?;
            log::info!(
                "hls variant bandwidth={:?} codecs={:?} of {} listed",
                variant.bandwidth,
                variant.codecs,
                master.variants.len()
            );
            // A remote u64: capped, not truncated, on its way into the u32 the UI shows.
            bitrate_kbps = variant
                .bandwidth
                .map(|b| u32::try_from(b / 1000).unwrap_or(u32::MAX));
            let (fetched, started) = fetch_playlist(client, &variant.uri, Kind::Media).await?;
            match parse_fetched(&fetched, started, Kind::Media)? {
                Playlist::Media(m) => {
                    media_url = fetched.url.clone();
                    m
                }
                Playlist::Master(_) => {
                    return Err(unsupported("HLS variant is another master playlist"));
                }
            }
        }
    };
    if media.segments.is_empty() {
        return Err(network(format!(
            "HLS media playlist {media_url} lists no segment"
        )));
    }

    let now = Instant::now();
    let (planner, step) = Planner::start(&media, now);
    // `start` returns `Fetch` with at least one segment for a non-empty playlist (checked
    // above). Anything else is a typed error, not a panic shape (review 2026-09-25, finding 8:
    // two "for the type" arms yielded an empty list that `remove(0)` would have panicked on).
    let (pending, then_wait) = match step {
        Step::Fetch {
            segments,
            then_wait,
            ..
        } if !segments.is_empty() => (segments, then_wait),
        other => {
            return Err(network(format!(
                "HLS media playlist {media_url} gave no start segment ({other:?})"
            )));
        }
    };
    log::info!(
        "hls media url={media_url} td={:?} seq={}..{} start={}",
        media.target_duration,
        media.media_sequence,
        media.last_seq().unwrap_or(media.media_sequence),
        pending.first().map(|s| s.seq).unwrap_or(0)
    );

    // The first segment decides the container and the session's format. A 404/410 on it is
    // an eviction — the window moved on between the playlist and the request — so the next
    // pending segment is tried; only when every start segment is gone does the open fail, and
    // then as a retriable `Network` error: the session's backoff reopens on a fresher window
    // (review 2026-09-25, finding 4).
    let mut candidates = pending.into_iter();
    let mut last_evicted: Option<(u64, u16)> = None;
    let (first, first_bytes, content_type) = loop {
        // Running out of candidates is the typed "all gone" answer; no index, no panic shape.
        let Some(candidate) = candidates.next() else {
            let (seq, status) = last_evicted.unwrap_or((0, 0));
            return Err(network(format!(
                "HLS start segments are gone ({status} at seq {seq}); the window moved on"
            )));
        };
        match fetch_segment(client, &candidate, media.target_duration, true).await? {
            SegmentFetch::Refused(c) => return Err(refused_container(c)),
            SegmentFetch::Body {
                bytes,
                content_type,
            } => break (candidate, bytes, content_type),
            SegmentFetch::Evicted(status) => {
                log::warn!(
                    "hls start segment seq={} evicted ({status}); trying the next",
                    candidate.seq
                );
                last_evicted = Some((candidate.seq, status));
            }
        }
    };
    let pending: Vec<Segment> = candidates.collect();
    let normalised = segment::normalise(&first_bytes);
    let Some(format) = normalised.format else {
        return Err(unsupported("HLS segment format not recognised"));
    };
    log_normalised(first.seq, &normalised);
    let mut guard = FormatGuard::default();
    let _ = guard.check(format);

    // The reader and the task.
    let (tx, rx) = mpsc::channel::<Bytes>(CHANNEL_SEGMENTS);
    let task = FetchTask {
        client: client.clone(),
        media_url,
        // Bounded as the planner's is: the retry window is compared against it, and a raw
        // `TARGETDURATION:3600` retried a failed segment every second for an hour (review 2,
        // finding 2).
        target_duration: playlist::bounded_target_duration(media.target_duration),
        planner,
        guard,
        tx,
        pending,
        then_wait,
    };
    let first_payload = Bytes::from(normalised.bytes);
    tokio::spawn(async move { task.run(first_payload).await });

    let storage = stream::bounded_storage();
    // The same counter the Icecast path bumps on stream-download's internal reconnect, so if
    // the idle timeout ever fires here it surfaces as `playback:reconnect` too. With
    // `retry_timeout_for` above the task's longest legitimate silence, a count above zero on a
    // healthy station is a defect (T15 pins it at zero over 13 s at TD 6).
    let settings = Settings::<HlsSource>::default()
        .prefetch_bytes(prefetch_bytes)
        .retry_timeout(retry_timeout_for(media.target_duration))
        .on_reconnect(move |_stream: &HlsSource, _token| {
            let n = reconnect_count.fetch_add(1, Ordering::Relaxed) + 1;
            log::warn!("hls source idle past retry_timeout (session count now {n})");
        });
    let reader = StreamDownload::from_stream(HlsSource { rx }, storage, settings)
        .await
        .map_err(|e| network(format!("HLS reader: {e}")))?;

    Ok(OpenedStream {
        reader,
        metaint: None,
        content_type: Some(content_type.unwrap_or_else(|| "audio/aac".to_string())),
        bitrate_kbps,
        station_name: None,
    })
}

fn log_normalised(seq: u64, n: &Normalised) {
    if n.rewritten > 0 || n.crc_dropped > 0 || n.partial_dropped > 0 || n.sync_lost.is_some() {
        log::debug!(
            "hls segment seq={seq} frames={} rewritten={} crc_dropped={} partial_dropped={} sync_lost={:?}",
            n.frames,
            n.rewritten,
            n.crc_dropped,
            n.partial_dropped,
            n.sync_lost
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The fetch task

struct FetchTask {
    client: Client,
    media_url: Url,
    /// The playlist's TD through [`playlist::bounded_target_duration`]: [1 s, 30 s].
    target_duration: Duration,
    planner: Planner,
    guard: FormatGuard,
    tx: mpsc::Sender<Bytes>,
    /// Segments the last step said to fetch, not yet fetched.
    pending: Vec<Segment>,
    /// How long to wait after them before the next reload.
    then_wait: Duration,
}

/// Why the task ended, for the one `hls task ended reason=` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The reader went away: Stop, a new `play`, or the session's watchdog.
    Closed,
    Stall,
    Restarted,
    EndList,
    FormatChanged,
}

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Ended::Closed => "closed",
            Ended::Stall => "stall",
            Ended::Restarted => "restarted",
            Ended::EndList => "end_list",
            Ended::FormatChanged => "format_changed",
        })
    }
}

impl FetchTask {
    async fn run(mut self, first_payload: Bytes) {
        let reason = self.run_inner(first_payload).await;
        log::info!("hls task ended reason={reason}");
        // Dropping `tx` closes the channel: the source ends, the decoder sees EOF.
    }

    async fn run_inner(&mut self, first_payload: Bytes) -> Ended {
        if self.tx.send(first_payload).await.is_err() {
            return Ended::Closed;
        }
        loop {
            // 1. The pending segments, in order.
            let pending = std::mem::take(&mut self.pending);
            for seg in pending {
                match self.fetch_and_send(&seg).await {
                    Ok(()) => {}
                    Err(ended) => return ended,
                }
            }
            // 2. Wait, then reload.
            if self.wait(self.then_wait).await.is_err() {
                return Ended::Closed;
            }
            let reloaded = self.reload().await;
            let step = match &reloaded {
                Ok(media) => self.planner.reload(Some(media), Instant::now()),
                Err(()) => self.planner.reload(None, Instant::now()),
            };
            match step {
                Step::Fetch {
                    segments,
                    skipped,
                    then_wait,
                } => {
                    if skipped > 0 {
                        log::warn!(
                            "hls gap skipped={skipped} next={}",
                            segments.first().map(|s| s.seq).unwrap_or(0)
                        );
                    }
                    let (a, b) = (
                        segments.first().map(|s| s.seq).unwrap_or(0),
                        segments.last().map(|s| s.seq).unwrap_or(0),
                    );
                    log::info!(
                        "hls refresh seq={a}..{b} new={} wait_ms={}",
                        segments.len(),
                        then_wait.as_millis()
                    );
                    self.pending = segments;
                    self.then_wait = then_wait;
                }
                Step::Wait(d) => {
                    log::info!("hls refresh new=0 wait_ms={}", d.as_millis());
                    self.then_wait = d;
                }
                Step::End(cause) => {
                    return match cause {
                        EndCause::Stall => Ended::Stall,
                        EndCause::Restarted => Ended::Restarted,
                        EndCause::EndList => Ended::EndList,
                    };
                }
            }
        }
    }

    /// Sleep unless the reader goes away first.
    async fn wait(&self, d: Duration) -> Result<(), ()> {
        tokio::select! {
            _ = self.tx.closed() => Err(()),
            _ = tokio::time::sleep(d) => Ok(()),
        }
    }

    /// Reload the media playlist. `Err(())` is a failed reload — logged, and the planner
    /// decides the wait (D2: no session backoff for a transient reload failure).
    async fn reload(&self) -> Result<MediaPlaylist, ()> {
        let fetched = tokio::select! {
            _ = self.tx.closed() => return Err(()),
            r = fetch_playlist(&self.client, &self.media_url, Kind::Media) => r,
        };
        match fetched {
            Ok((fetched, started)) => match parse_fetched(&fetched, started, Kind::Media) {
                Ok(Playlist::Media(m)) => Ok(m),
                Ok(Playlist::Master(_)) => {
                    log::warn!(
                        "hls reload of {} answered a master playlist",
                        self.media_url
                    );
                    Err(())
                }
                Err(e) => {
                    log::warn!("hls reload of {} refused: {}", self.media_url, e.message);
                    Err(())
                }
            },
            Err(e) => {
                log::warn!("hls reload of {} failed: {}", self.media_url, e.message);
                Err(())
            }
        }
    }

    /// Fetch one segment (retrying by [`after_failure`]), normalise it, check its format, and
    /// send it. `Err` ends the task.
    async fn fetch_and_send(&mut self, seg: &Segment) -> Result<(), Ended> {
        if seg.discontinuity {
            log::info!("hls discontinuity before seq={}", seg.seq);
        }
        let first_try = Instant::now();
        let mut retries = 0u32;
        let bytes = loop {
            let fetched = tokio::select! {
                _ = self.tx.closed() => return Err(Ended::Closed),
                r = fetch_segment(&self.client, seg, self.target_duration, false) => r,
            };
            let failure = match fetched {
                Ok(SegmentFetch::Body { bytes, .. }) => break Some(bytes),
                Ok(SegmentFetch::Refused(c)) => {
                    // Only the first segment is sniffed; this arm is unreachable while
                    // `sniff_first` is false, kept for the type.
                    log::warn!("hls segment seq={} refused: {c:?}", seg.seq);
                    break None;
                }
                Ok(SegmentFetch::Evicted(410)) => Failure::Gone,
                Ok(SegmentFetch::Evicted(status)) => Failure::NotFound(status),
                Err(e) => Failure::Transient(e.message),
            };
            match after_failure(&failure, retries, first_try.elapsed(), self.target_duration) {
                AfterFailure::Retry => {
                    log::warn!(
                        "hls segment seq={} failed, retrying: {}",
                        seg.seq,
                        failure.cause()
                    );
                    retries += 1;
                    if self.wait(SEGMENT_RETRY_STEP).await.is_err() {
                        return Err(Ended::Closed);
                    }
                }
                AfterFailure::Skip => {
                    log::warn!(
                        "hls gap skipped=1 seq={} cause={}",
                        seg.seq,
                        failure.cause()
                    );
                    break None;
                }
            }
        };
        let Some(bytes) = bytes else {
            return Ok(());
        };
        let normalised = segment::normalise(&bytes);
        log_normalised(seg.seq, &normalised);
        let Some(format) = normalised.format else {
            log::warn!("hls gap skipped=1 seq={} cause=no ADTS frame", seg.seq);
            return Ok(());
        };
        if let Err(changed) = self.guard.check(format) {
            log::warn!("hls {changed} at seq={}; reopening", seg.seq);
            return Err(Ended::FormatChanged);
        }
        if self.tx.send(Bytes::from(normalised.bytes)).await.is_err() {
            return Err(Ended::Closed);
        }
        Ok(())
    }
}

/// A segment fetch that failed, as the retry rule reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Failure {
    /// 410: permanent by definition.
    Gone,
    /// 404: evicted, or not at the edge yet.
    NotFound(u16),
    /// A network error or any other status: D3's transient failure.
    Transient(String),
}

impl Failure {
    fn cause(&self) -> String {
        match self {
            Failure::Gone => "HTTP 410".to_string(),
            Failure::NotFound(status) => format!("HTTP {status}"),
            Failure::Transient(message) => message.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterFailure {
    Retry,
    Skip,
}

/// The fetch task's rule for a failed segment, pure (review 2, 2026-09-25, findings 2 and 4):
/// a 410 is skipped at once; a 404 is retried [`NOT_FOUND_RETRIES`] times, then skipped; any
/// other failure is retried at [`SEGMENT_RETRY_STEP`] while the next try still falls within
/// one target duration of the first (D3). The TD is bounded here, as [`segment_timeout`]
/// bounds its own, so the window is at most 30 s whatever the playlist says.
fn after_failure(
    failure: &Failure,
    retries: u32,
    since_first_try: Duration,
    target_duration: Duration,
) -> AfterFailure {
    let retry = match failure {
        Failure::Gone => false,
        Failure::NotFound(_) => retries < NOT_FOUND_RETRIES,
        Failure::Transient(_) => {
            since_first_try + SEGMENT_RETRY_STEP
                <= playlist::bounded_target_duration(target_duration)
        }
    };
    if retry {
        AfterFailure::Retry
    } else {
        AfterFailure::Skip
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review 2 (2026-09-25), findings 2 and 4: the task's retry rule. A transient failure is
    /// retried within one **bounded** TD — at `TARGETDURATION:3600` the window closes at 30 s.
    /// On `fe120a2` the rule was inline, `elapsed + 1 s <= self.target_duration` on the raw
    /// TD, which answers Retry at 29.001 s (and at 3 599 s); this test fails the same way if
    /// the bound is dropped. A 410 is never retried; a 404 twice, whatever the TD.
    #[test]
    fn after_failure_bounds_every_retry() {
        let td = Duration::from_secs(3600);
        let t = Failure::Transient("HTTP 503".into());
        assert_eq!(
            after_failure(&t, 28, Duration::from_secs(29), td),
            AfterFailure::Retry
        );
        assert_eq!(
            after_failure(&t, 29, Duration::from_millis(29_001), td),
            AfterFailure::Skip
        );
        assert_eq!(
            after_failure(&Failure::Gone, 0, Duration::ZERO, td),
            AfterFailure::Skip
        );
        let nf = Failure::NotFound(404);
        assert_eq!(
            after_failure(&nf, 0, Duration::ZERO, td),
            AfterFailure::Retry
        );
        assert_eq!(
            after_failure(&nf, 1, Duration::from_secs(1), td),
            AfterFailure::Retry
        );
        assert_eq!(
            after_failure(&nf, 2, Duration::from_secs(2), td),
            AfterFailure::Skip
        );
        // A 404 is retried twice even where the TD is shorter than the two steps.
        assert_eq!(
            after_failure(&nf, 1, Duration::from_secs(1), Duration::from_secs(1)),
            AfterFailure::Retry
        );
    }

    #[test]
    fn hls_content_types_match_case_insensitively_without_parameters() {
        for ct in [
            "application/vnd.apple.mpegurl",
            "application/vnd.apple.mpegurl; charset=UTF-8",
            "application/x-mpegURL",
            "Audio/X-MpegURL",
            "audio/mpegurl",
        ] {
            assert!(is_hls_content_type(ct), "{ct}");
        }
        for ct in [
            "audio/aac",
            "audio/mpeg",
            "text/html",
            "",
            "application/octet-stream",
        ] {
            assert!(!is_hls_content_type(ct), "{ct}");
        }
    }

    #[test]
    fn hls_retry_timeout_is_above_the_longest_legitimate_silence() {
        // TD 10: stall 30 s + segment timeout 20 s + 5 s = 55 s; TD 5: 15 + 10 + 5 = 30 s.
        // Both above `read_timeout` (20 s) — invariant 4 does not apply to this source.
        assert_eq!(
            retry_timeout_for(Duration::from_secs(10)),
            Duration::from_secs(55)
        );
        assert_eq!(
            retry_timeout_for(Duration::from_secs(5)),
            Duration::from_secs(30)
        );
        assert_eq!(
            segment_timeout(Duration::from_secs(4)),
            Duration::from_secs(10)
        );
        assert_eq!(
            segment_timeout(Duration::from_secs(13)),
            Duration::from_secs(26)
        );
    }

    /// Review 2026-09-25, finding 7: the `kind=` a playlist request logs is what the body
    /// turned out to be, and on a failure what the caller asked for. On `938944c` every
    /// failure logged `kind=master`, media reloads included (X6's log, 13:11:09 and 13:11:49).
    /// Fails on that rule: `logged_kind(Kind::Media, None)` would read `Master`.
    #[test]
    fn a_failed_playlist_request_logs_the_kind_it_asked_for() {
        let base = Url::parse("http://h/p.m3u8").unwrap();
        let media = playlist::parse(
            "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\ns.aac\n",
            &base,
        )
        .unwrap();
        let master = playlist::parse(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"mp4a.40.2\"\nv.m3u8\n",
            &base,
        )
        .unwrap();
        assert_eq!(logged_kind(Kind::Media, None), Kind::Media);
        assert_eq!(logged_kind(Kind::Master, None), Kind::Master);
        assert_eq!(logged_kind(Kind::Master, Some(&media)), Kind::Media);
        assert_eq!(logged_kind(Kind::Media, Some(&master)), Kind::Master);
    }

    /// Review 2026-09-25, finding 3: a `TARGETDURATION` at u64::MAX reached `Duration * 2`
    /// ("overflow when multiplying duration by scalar" on `102c114`). The timeouts are computed
    /// on the same bounded TD the planner uses, so the largest is 60 s + 90 s + 5 s.
    #[test]
    fn timeouts_on_an_absurd_target_duration_are_bounded() {
        let td = Duration::from_secs(u64::MAX);
        assert_eq!(segment_timeout(td), Duration::from_secs(60));
        assert_eq!(retry_timeout_for(td), Duration::from_secs(90 + 60 + 5));
        assert_eq!(segment_timeout(Duration::ZERO), Duration::from_secs(10));
    }
}
