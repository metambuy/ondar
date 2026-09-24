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
use stream_download::storage::bounded::BoundedStorageProvider;
use stream_download::storage::memory::MemoryStorageProvider;
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
    (target_duration * 2).max(Duration::from_secs(10))
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
) -> Result<(FetchedPlaylist, Instant), StreamError> {
    let started = Instant::now();
    let response = client
        .get(url.clone())
        .timeout(PLAYLIST_TIMEOUT)
        .send()
        .await
        .map_err(|e| {
            log_request(
                Kind::Master,
                url,
                &format!("err:{}", stream::chain_message(&e)),
                "0",
                started,
            );
            classify(Kind::Master, url, &e)
        })?;
    if !response.status().is_success() {
        let e = status_error(Kind::Master, &response);
        log_request(Kind::Master, url, response.status().as_str(), "0", started);
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
        segment::gunzip(&body).map_err(|e| {
            unsupported(format!(
                "HLS playlist {final_url}: content-encoding gzip but {e}"
            ))
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
fn parse_fetched(fetched: &FetchedPlaylist, started: Instant) -> Result<Playlist, StreamError> {
    match playlist::parse(&fetched.text, &fetched.url) {
        Ok(p) => {
            let kind = match &p {
                Playlist::Master(_) => Kind::Master,
                Playlist::Media(_) => Kind::Media,
            };
            log_request(
                kind,
                &fetched.url,
                "200",
                &fetched.text.len().to_string(),
                started,
            );
            Ok(p)
        }
        Err(e) => {
            log_request(
                Kind::Master,
                &fetched.url,
                "200",
                &fetched.text.len().to_string(),
                started,
            );
            Err(playlist_refused(e))
        }
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
}

/// GET a segment. With `sniff_first`, only the ID3 tags plus [`segment::SNIFF_LEN`] bytes are
/// read before the container is named, and a segment that is not ADTS is dropped there — the
/// body is never downloaded. The `Content-Encoding` is honoured for segments too.
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
    let mut sniffed = !sniff_first || gzipped; // a gzipped segment is sniffed after inflating
    loop {
        if !sniffed {
            let tags = segment::id3_end(&body);
            // The tags are complete once `id3_end` lands inside the buffer, and the sniff
            // needs `SNIFF_LEN` bytes after them.
            if tags < body.len() && body.len() >= tags + segment::SNIFF_LEN {
                sniffed = true;
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
        segment::gunzip(&body)
            .map_err(|e| network(format!("HLS segment {}: gzip: {e}", seg.uri)))?
    } else {
        body
    };
    if !sniffed {
        // The body ended before the sniff had its bytes (a short segment, or gzipped): sniff
        // whatever there is.
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
    let (fetched, started) = fetch_playlist(client, &url).await?;
    let mut bitrate_kbps = None;
    let media_url;
    let media = match parse_fetched(&fetched, started)? {
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
            bitrate_kbps = variant.bandwidth.map(|b| (b / 1000) as u32);
            let (fetched, started) = fetch_playlist(client, &variant.uri).await?;
            match parse_fetched(&fetched, started)? {
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
    let (mut pending, then_wait) = match step {
        Step::Fetch {
            segments,
            then_wait,
            ..
        } => (segments, then_wait),
        // `start` always returns Fetch; the arms below exist for the type.
        Step::Wait(d) => (Vec::new(), d),
        Step::End(_) => (Vec::new(), media.target_duration),
    };
    log::info!(
        "hls media url={media_url} td={:?} seq={}..{} start={}",
        media.target_duration,
        media.media_sequence,
        media.last_seq().unwrap_or(media.media_sequence),
        pending.first().map(|s| s.seq).unwrap_or(0)
    );

    // The first segment decides the container and the session's format.
    let first = pending.remove(0);
    let (first_bytes, content_type) =
        match fetch_segment(client, &first, media.target_duration, true).await? {
            SegmentFetch::Refused(c) => return Err(refused_container(c)),
            SegmentFetch::Body {
                bytes,
                content_type,
            } => (bytes, content_type),
        };
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
        target_duration: media.target_duration,
        planner,
        guard,
        tx,
        pending,
        then_wait,
    };
    let first_payload = Bytes::from(normalised.bytes);
    tokio::spawn(async move { task.run(first_payload).await });

    let storage = BoundedStorageProvider::new(
        MemoryStorageProvider,
        std::num::NonZeroUsize::new(stream::BUFFER_BYTES).expect("non-zero buffer"),
    );
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
            r = fetch_playlist(&self.client, &self.media_url) => r,
        };
        match fetched {
            Ok((fetched, started)) => match parse_fetched(&fetched, started) {
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

    /// Fetch one segment (retrying within its target duration), normalise it, check its
    /// format, and send it. `Err` ends the task.
    async fn fetch_and_send(&mut self, seg: &Segment) -> Result<(), Ended> {
        if seg.discontinuity {
            log::info!("hls discontinuity before seq={}", seg.seq);
        }
        let first_try = Instant::now();
        let bytes = loop {
            let fetched = tokio::select! {
                _ = self.tx.closed() => return Err(Ended::Closed),
                r = fetch_segment(&self.client, seg, self.target_duration, false) => r,
            };
            match fetched {
                Ok(SegmentFetch::Body { bytes, .. }) => break Some(bytes),
                Ok(SegmentFetch::Refused(c)) => {
                    // Only the first segment is sniffed; this arm is unreachable while
                    // `sniff_first` is false, kept for the type.
                    log::warn!("hls segment seq={} refused: {c:?}", seg.seq);
                    break None;
                }
                Err(e) => {
                    if first_try.elapsed() + SEGMENT_RETRY_STEP <= self.target_duration {
                        log::warn!(
                            "hls segment seq={} failed, retrying: {}",
                            seg.seq,
                            e.message
                        );
                        if self.wait(SEGMENT_RETRY_STEP).await.is_err() {
                            return Err(Ended::Closed);
                        }
                        continue;
                    }
                    log::warn!("hls gap skipped=1 seq={} cause={}", seg.seq, e.message);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
