//! M3U8 parsing, variant choice and the refresh planner — the pure half of HLS.
//!
//! **Parsing** covers the tags the player acts on (RFC 8216): `#EXTM3U`, `EXT-X-STREAM-INF`
//! (`BANDWIDTH`, `CODECS`), `EXT-X-TARGETDURATION`, `EXT-X-MEDIA-SEQUENCE`, `EXTINF`,
//! `EXT-X-DISCONTINUITY`, `EXT-X-ENDLIST`, and the three that are refused: `EXT-X-MAP` (fMP4),
//! `EXT-X-KEY` with a method other than `NONE`, and `EXT-X-BYTERANGE`. Every other tag is
//! ignored. Attribute lists are split quote-aware, because a `CODECS="avc1.4D401E,mp4a.40.2"`
//! is one attribute with a comma inside it; an `EXTINF` duration ends at the first comma,
//! because a title can carry quoted commas of its own (census station 02). Relative URIs are
//! joined against the base the caller gives — the URL the playlist was actually fetched from,
//! after redirects — never against the URL that was asked for.
//!
//! A body served with an HLS content type that does not start with `#EXTM3U` is not a
//! playlist; one that starts with it but carries neither `EXT-X-TARGETDURATION` nor
//! `EXT-X-STREAM-INF` is a plain M3U (a file listing an Icecast URL), and is refused as
//! [`PlaylistError::NotHls`] rather than followed (plan review R2).
//!
//! **Variant choice** prefers an audio-only variant (LC before HE, then the highest bandwidth),
//! then a variant that declares no codecs, then a muxed audio + video variant at the lowest
//! bandwidth — never a video-only one (decision D1).
//!
//! **The planner** is a state machine over `MEDIA-SEQUENCE` numbers: it starts three segments
//! behind the live edge (RFC 8216 §6.3.3, decision D5), emits each sequence number exactly
//! once — the identity is "sequence greater than the last emitted", never "URI not yet seen",
//! because a live window keeps listing segments older than the ones already fetched — waits the
//! last segment's duration after a reload that brought new segments and half the target
//! duration after one that did not (§6.3.4), clamped to [1 s, 30 s], and ends the stream when
//! the window has not advanced for three target durations or when the sequence numbers go
//! backwards (a server restart).

use std::time::{Duration, Instant};

use url::Url;

/// The shortest wait between two playlist reloads. Stops a hammer loop on
/// `EXT-X-TARGETDURATION:0` or an `EXTINF` of a few milliseconds.
pub const MIN_WAIT: Duration = Duration::from_secs(1);
/// The longest wait between two reloads, above any target duration measured (max 13 s in the
/// census); a playlist that claims an hour is not believed.
pub const MAX_WAIT: Duration = Duration::from_secs(30);
/// The window has not advanced for this many target durations → the stream has stalled. A
/// server must publish within 1.5 × TD (RFC 8216 §6.2.1), so three is two missed publishes.
pub const STALL_TARGET_DURATIONS: u32 = 3;
/// Start this many segments behind the live edge (RFC 8216 §6.3.3: "no closer than three
/// target durations from the end").
pub const START_BEHIND: usize = 3;

// ---------------------------------------------------------------------------------------------
// Types

#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    Master(MasterPlaylist),
    Media(MediaPlaylist),
}

/// One `EXT-X-STREAM-INF` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub uri: Url,
    pub bandwidth: Option<u64>,
    /// The `CODECS` attribute split on its commas, or `None` when the tag has no `CODECS`.
    pub codecs: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterPlaylist {
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// The media sequence number: the playlist's `EXT-X-MEDIA-SEQUENCE` plus the index.
    pub seq: u64,
    /// The `EXTINF` duration.
    pub duration: Duration,
    pub uri: Url,
    /// An `EXT-X-DISCONTINUITY` preceded this segment. Logged by the fetch loop, nothing else.
    pub discontinuity: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaPlaylist {
    pub target_duration: Duration,
    pub media_sequence: u64,
    pub segments: Vec<Segment>,
    pub end_list: bool,
}

impl MediaPlaylist {
    /// The sequence number of the last listed segment, if any.
    pub fn last_seq(&self) -> Option<u64> {
        self.segments.last().map(|s| s.seq)
    }

    pub fn discontinuities(&self) -> usize {
        self.segments.iter().filter(|s| s.discontinuity).count()
    }
}

/// Why a body could not be used. Every variant's message is what the page renders after
/// `error [unsupported_format]: ` (plan §2.5), so they are written for a listener.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaylistError {
    #[error("not a playlist (no #EXTM3U)")]
    NotPlaylist,
    #[error("playlist is not HLS (no EXT-X tags)")]
    NotHls,
    #[error("HLS with fMP4 segments is not supported yet")]
    Fmp4,
    #[error("encrypted HLS is not supported")]
    Encrypted,
    #[error("HLS byte-range segments are not supported")]
    ByteRange,
    #[error("malformed playlist: {0}")]
    Malformed(String),
}

/// The master lists no variant that carries audio.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("HLS stream has no audio variant (video only: {0})")]
pub struct NoAudio(pub String);

// ---------------------------------------------------------------------------------------------
// Parsing

/// Parse a playlist body. `base` is the URL the body was fetched from — after redirects — and
/// every relative URI is joined against it.
pub fn parse(text: &str, base: &Url) -> Result<Playlist, PlaylistError> {
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r').trim());
    match lines.next() {
        Some(first) if first.starts_with("#EXTM3U") => {}
        _ => return Err(PlaylistError::NotPlaylist),
    }

    let mut variants: Vec<Variant> = Vec::new();
    let mut pending_variant: Option<(Option<u64>, Option<Vec<String>>)> = None;

    let mut target_duration: Option<Duration> = None;
    let mut media_sequence: u64 = 0;
    let mut end_list = false;
    let mut segments: Vec<Segment> = Vec::new();
    let mut pending_duration: Option<Duration> = None;
    let mut discontinuity = false;
    // The first malformed value seen, reported only if the body turns out to be HLS.
    let mut deferred: Option<PlaylistError> = None;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            if !rest.starts_with("EXT") {
                continue; // a comment
            }
            let (tag, value) = match rest.split_once(':') {
                Some((t, v)) => (t, v),
                None => (rest, ""),
            };
            match tag {
                "EXT-X-STREAM-INF" => {
                    let attrs = attribute_list(value);
                    let bandwidth = attrs
                        .iter()
                        .find(|(k, _)| *k == "BANDWIDTH")
                        .and_then(|(_, v)| v.parse().ok());
                    let codecs = attrs
                        .iter()
                        .find(|(k, _)| *k == "CODECS")
                        .map(|(_, v)| {
                            v.split(',')
                                .map(|c| c.trim().to_string())
                                .filter(|c| !c.is_empty())
                                .collect::<Vec<_>>()
                        })
                        .filter(|v| !v.is_empty());
                    pending_variant = Some((bandwidth, codecs));
                }
                "EXT-X-TARGETDURATION" => {
                    let secs: u64 = value.trim().parse().map_err(|_| {
                        PlaylistError::Malformed(format!("EXT-X-TARGETDURATION:{value}"))
                    })?;
                    target_duration = Some(Duration::from_secs(secs));
                }
                "EXT-X-MEDIA-SEQUENCE" => {
                    media_sequence = value.trim().parse().map_err(|_| {
                        PlaylistError::Malformed(format!("EXT-X-MEDIA-SEQUENCE:{value}"))
                    })?;
                }
                "EXTINF" => {
                    // The duration is everything up to the first comma; the title after it may
                    // itself contain commas (station 02's quoted attribute list).
                    let dur = value.split(',').next().unwrap_or("").trim();
                    match dur
                        .parse::<f64>()
                        .ok()
                        .filter(|s| s.is_finite() && *s >= 0.0)
                    {
                        Some(secs) => pending_duration = Some(Duration::from_secs_f64(secs)),
                        // Deferred, not returned: a plain M3U writes `#EXTINF:-1,Name` and
                        // must read as NotHls below, not as a malformed HLS playlist.
                        None => {
                            deferred
                                .get_or_insert(PlaylistError::Malformed(format!("EXTINF:{value}")));
                        }
                    }
                }
                "EXT-X-DISCONTINUITY" => discontinuity = true,
                "EXT-X-ENDLIST" => end_list = true,
                "EXT-X-MAP" => return Err(PlaylistError::Fmp4),
                "EXT-X-KEY" => {
                    let attrs = attribute_list(value);
                    let method = attrs
                        .iter()
                        .find(|(k, _)| *k == "METHOD")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");
                    if !method.eq_ignore_ascii_case("NONE") {
                        return Err(PlaylistError::Encrypted);
                    }
                }
                "EXT-X-BYTERANGE" => return Err(PlaylistError::ByteRange),
                _ => {} // EXT-X-VERSION, EXT-X-MEDIA, EXT-X-PROGRAM-DATE-TIME, … — ignored
            }
            continue;
        }

        // A URI line. It belongs to the tag that preceded it.
        let uri = base
            .join(line)
            .map_err(|e| PlaylistError::Malformed(format!("URI {line:?}: {e}")))?;
        if let Some((bandwidth, codecs)) = pending_variant.take() {
            variants.push(Variant {
                uri,
                bandwidth,
                codecs,
            });
        } else if let Some(duration) = pending_duration.take() {
            let seq = media_sequence + segments.len() as u64;
            segments.push(Segment {
                seq,
                duration,
                uri,
                discontinuity: std::mem::take(&mut discontinuity),
            });
        }
        // A URI with neither tag before it (a plain M3U's line) is ignored here; whether the
        // body is HLS at all is decided below.
    }

    // Is it HLS at all? Decided before any deferred error, so a plain M3U reads as NotHls.
    if variants.is_empty() && target_duration.is_none() {
        return Err(PlaylistError::NotHls);
    }
    if let Some(e) = deferred {
        return Err(e);
    }
    if !variants.is_empty() {
        return Ok(Playlist::Master(MasterPlaylist { variants }));
    }
    Ok(Playlist::Media(MediaPlaylist {
        // `target_duration` is `Some` here: the NotHls check above returned otherwise.
        target_duration: target_duration.unwrap_or_default(),
        media_sequence,
        segments,
        end_list,
    }))
}

/// Split an attribute list (`KEY=value,KEY="quoted, value"`) into pairs, honouring quotes.
/// Quoted values are returned without their quotes.
fn attribute_list(s: &str) -> Vec<(&str, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim();
        let after = &rest[eq + 1..];
        let (value, remainder) = if let Some(q) = after.strip_prefix('"') {
            match q.find('"') {
                Some(end) => {
                    let value = &q[..end];
                    let tail = &q[end + 1..];
                    (value.to_string(), tail.trim_start_matches(',').trim_start())
                }
                None => (q.to_string(), ""),
            }
        } else {
            match after.find(',') {
                Some(end) => (
                    after[..end].trim().to_string(),
                    after[end + 1..].trim_start(),
                ),
                None => (after.trim().to_string(), ""),
            }
        };
        out.push((key, value));
        rest = remainder;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Variant choice

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Every codec is audio. `he` when any is HE-AAC (`mp4a.40.5` / `mp4a.40.29`).
    AudioOnly { he: bool },
    /// No `CODECS` attribute: unknown, so worth trying — the segment sniff decides.
    Unknown,
    /// Audio and something else (video, subtitles).
    Muxed,
    /// No audio codec at all.
    VideoOnly,
}

fn is_audio_codec(c: &str) -> bool {
    let c = c.trim();
    c.len() >= 5 && c[..5].eq_ignore_ascii_case("mp4a.")
        || c.eq_ignore_ascii_case("ac-3")
        || c.eq_ignore_ascii_case("ec-3")
}

/// `mp4a.40.<object type>`: 5 is HE-AAC v1 (SBR), 29 is HE-AAC v2 (SBR + PS).
fn is_he_aac(c: &str) -> bool {
    let c = c.trim();
    c.len() > 8
        && c[..8].eq_ignore_ascii_case("mp4a.40.")
        && matches!(c[8..].parse::<u32>(), Ok(5) | Ok(29))
}

fn kind(v: &Variant) -> Kind {
    match &v.codecs {
        None => Kind::Unknown,
        Some(codecs) => {
            let audio = codecs.iter().filter(|c| is_audio_codec(c)).count();
            if audio == 0 {
                Kind::VideoOnly
            } else if audio == codecs.len() {
                Kind::AudioOnly {
                    he: codecs.iter().any(|c| is_he_aac(c)),
                }
            } else {
                Kind::Muxed
            }
        }
    }
}

/// Pick the variant to play (decision D1). Audio-only first — LC before HE, because the decoder
/// plays the core only (Step 0 (b)), so at equal rate LC sounds better — and the highest
/// bandwidth among those; then a variant with no `CODECS`, highest bandwidth; then a muxed
/// audio + video variant at the **lowest** bandwidth (the bytes are mostly video, and the
/// container sniff refuses TS anyway). A video-only variant is never chosen.
pub fn choose_variant(master: &MasterPlaylist) -> Result<&Variant, NoAudio> {
    let bw = |v: &Variant| v.bandwidth.unwrap_or(0);

    let audio_only = master
        .variants
        .iter()
        .filter_map(|v| match kind(v) {
            Kind::AudioOnly { he } => Some((he, v)),
            _ => None,
        })
        // LC (he == false) before HE, then the highest bandwidth. `max_by_key` on the inverted
        // `he` flag and the bandwidth gives that in one pass; ties keep the last, which is fine.
        .max_by_key(|(he, v)| (!*he, bw(v)))
        .map(|(_, v)| v);
    if let Some(v) = audio_only {
        return Ok(v);
    }

    if let Some(v) = master
        .variants
        .iter()
        .filter(|v| kind(v) == Kind::Unknown)
        .max_by_key(|v| bw(v))
    {
        return Ok(v);
    }

    if let Some(v) = master
        .variants
        .iter()
        .filter(|v| kind(v) == Kind::Muxed)
        .min_by_key(|v| bw(v))
    {
        return Ok(v);
    }

    let mut seen: Vec<&str> = Vec::new();
    for v in &master.variants {
        for c in v.codecs.iter().flatten() {
            if !seen.contains(&c.as_str()) {
                seen.push(c);
            }
        }
    }
    Err(NoAudio(if seen.is_empty() {
        "no variants".to_string()
    } else {
        seen.join(",")
    }))
}

// ---------------------------------------------------------------------------------------------
// The refresh planner

/// What the fetch loop does next. Every step is decided from the playlist and the clock the
/// caller passes in; the planner never sleeps or fetches.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Fetch these segments in order, then reload the playlist after `then_wait`. `skipped` is
    /// how many sequence numbers the window moved past before they could be fetched — an
    /// audible gap the loop logs as `hls gap skipped=<n>`.
    Fetch {
        segments: Vec<Segment>,
        skipped: u64,
        then_wait: Duration,
    },
    /// Nothing new (or the reload failed): reload again after this long.
    Wait(Duration),
    /// The stream is over for this cause; the loop closes the channel and the session's own
    /// backoff takes it from there.
    End(EndCause),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndCause {
    /// No new segment for [`STALL_TARGET_DURATIONS`] target durations.
    Stall,
    /// `EXT-X-MEDIA-SEQUENCE` went backwards: the server restarted its numbering.
    Restarted,
    /// `EXT-X-ENDLIST` and every listed segment emitted.
    EndList,
}

#[derive(Debug, Clone)]
pub struct Planner {
    target_duration: Duration,
    /// The next sequence number to emit. Everything below it has been emitted (or skipped).
    next_seq: u64,
    /// The `EXT-X-MEDIA-SEQUENCE` of the last playlist accepted, to tell a restart from the
    /// ordinary case of a window that still lists older segments.
    window_start: u64,
    /// When the window last brought a new segment.
    last_new_at: Instant,
}

fn clamp_wait(d: Duration) -> Duration {
    d.clamp(MIN_WAIT, MAX_WAIT)
}

/// The stall bound for a target duration: [`STALL_TARGET_DURATIONS`] × the **clamped** TD, so a
/// `TARGETDURATION:0` playlist gets 3 s rather than a bound of zero that would declare a stall
/// on its first reload, and an hour-long one 90 s. The fetch layer sizes its idle timeout on it.
pub fn stall_bound(target_duration: Duration) -> Duration {
    clamp_wait(target_duration) * STALL_TARGET_DURATIONS
}

impl Planner {
    /// Start on a freshly fetched media playlist: emit from [`START_BEHIND`] segments before
    /// the end (or the first, when the window is that short).
    pub fn start(media: &MediaPlaylist, now: Instant) -> (Planner, Step) {
        let from = media.segments.len().saturating_sub(START_BEHIND);
        let segments: Vec<Segment> = media.segments[from..].to_vec();
        let next_seq = media
            .last_seq()
            .map(|s| s + 1)
            .unwrap_or(media.media_sequence);
        let then_wait = clamp_wait(
            segments
                .last()
                .map(|s| s.duration)
                .unwrap_or(media.target_duration / 2),
        );
        let planner = Planner {
            target_duration: media.target_duration,
            next_seq,
            window_start: media.media_sequence,
            last_new_at: now,
        };
        (
            planner,
            Step::Fetch {
                segments,
                skipped: 0,
                then_wait,
            },
        )
    }

    /// The outcome of a playlist reload: `Some` when it was fetched and parsed, `None` when it
    /// failed (the loop retries at the next tick, until the stall bound).
    pub fn reload(&mut self, reloaded: Option<&MediaPlaylist>, now: Instant) -> Step {
        let stall = stall_bound(self.target_duration);
        let Some(media) = reloaded else {
            return if now.duration_since(self.last_new_at) >= stall {
                Step::End(EndCause::Stall)
            } else {
                Step::Wait(clamp_wait(self.target_duration / 2))
            };
        };

        if media.media_sequence < self.window_start {
            return Step::End(EndCause::Restarted);
        }
        self.window_start = media.media_sequence;

        // The identity is the sequence number: emit only what is past the last emitted one.
        // A live window still lists segments older than those already fetched (every reload
        // does), so "not yet seen" would replay them.
        let skipped = media.media_sequence.saturating_sub(self.next_seq);
        let segments: Vec<Segment> = media
            .segments
            .iter()
            .filter(|s| s.seq >= self.next_seq)
            .cloned()
            .collect();

        if segments.is_empty() {
            if media.end_list {
                return Step::End(EndCause::EndList);
            }
            return if now.duration_since(self.last_new_at) >= stall {
                Step::End(EndCause::Stall)
            } else {
                Step::Wait(clamp_wait(self.target_duration / 2))
            };
        }

        self.last_new_at = now;
        self.next_seq = segments.last().map(|s| s.seq + 1).unwrap_or(self.next_seq);
        let then_wait = clamp_wait(segments.last().map(|s| s.duration).unwrap_or(stall));
        Step::Fetch {
            segments,
            skipped,
            then_wait,
        }
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! T1–T6 of the M3c plan, on the census fixtures under `fixtures/hls/` (byte copies of what
    //! the servers sent on 2026-09-21; heads only for segments — see PROVENANCE.md there).
    //! These modules do not exist on `b7e050a`, so each test's recorded failure is a mutation
    //! check rather than a run on the old tree (plan §4); the mutations are named per test.

    use super::*;
    use std::io::Read;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/hls/", $name))
        };
    }
    macro_rules! fixture_bytes {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/hls/", $name))
        };
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn master(text: &str, base: &str) -> MasterPlaylist {
        match parse(text, &url(base)).unwrap() {
            Playlist::Master(m) => m,
            other => panic!("expected a master playlist, got {other:?}"),
        }
    }

    fn media(text: &str, base: &str) -> MediaPlaylist {
        match parse(text, &url(base)).unwrap() {
            Playlist::Media(m) => m,
            other => panic!("expected a media playlist, got {other:?}"),
        }
    }

    fn gunzip(bytes: &[u8]) -> String {
        let mut s = String::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_string(&mut s)
            .unwrap();
        s
    }

    fn secs(d: Duration) -> f64 {
        d.as_secs_f64()
    }

    // The final URLs the census recorded (after redirects), used as join bases.
    const BASE_01: &str =
        "https://voa-ingest.akamaized.net/hls/live/2035206/151_124L/playlist.m3u8";
    const BASE_01_MEDIA: &str =
        "https://voa-ingest.akamaized.net/hls/live/2035206/151_124L/playlist124L.m3u8";
    const BASE_02: &str = "http://n1db-e2.revma.ihrhls.com/zc6951/hls.m3u8";
    const BASE_04: &str =
        "https://dwamdstream104.akamaized.net/hls/live/2015530/dwstream104/index.m3u8";
    const BASE_06: &str =
        "https://stream.radiofrance.fr/franceinter/franceinter_hifi.m3u8?id=radiofrance";
    const BASE_07_MEDIA: &str = "https://20043.live.streamtheworld.com:443/OCAAC/HLS/3b89e087-67f3-43a9-8646-908f163bd29c/0/playlist.m3u8";
    const BASE_09: &str = "https://hls-igi.cdnvideo.ru/igi/radio1/tracks-a1/mono.m3u8";
    /// Station 10's master is at `/liveradio/antena180a/playlist.m3u8` and lists the relative
    /// `chunklist.m3u8`; the census reached it with no redirect, so requested = final here. The
    /// test below gives a *different* base to pin that the join uses the base it is given.
    const BASE_10: &str = "http://streaming-live-app.rtp.pt/liveradio/antena180a/playlist.m3u8";
    const BASE_10_MEDIA: &str =
        "http://streaming-live-app.rtp.pt/liveradio/antena180a/chunklist.m3u8";

    // ---- T1: masters parse; relative URIs join against the given base; CODECS is one value

    #[test]
    fn t1_masters_parse_and_relative_uris_join_against_the_base() {
        // 01: one absolute variant.
        let m = master(fixture!("01-master.m3u8"), BASE_01);
        assert_eq!(m.variants.len(), 1);
        assert_eq!(m.variants[0].bandwidth, Some(140_800));
        assert_eq!(
            m.variants[0].codecs.as_deref(),
            Some(&["mp4a.40.2".to_string()][..])
        );
        assert_eq!(m.variants[0].uri.as_str(), BASE_01_MEDIA);

        // 10: a relative variant URI. Mutation "skip the join, take URIs as written" → this
        // fails at parse (a relative reference is not a URL).
        let m = master(fixture!("10-master.m3u8"), BASE_10);
        assert_eq!(m.variants[0].uri.as_str(), BASE_10_MEDIA);
        // …and the base it is given is the one used: against a redirected base the variant
        // follows the redirect. Which base `hls::open` passes is commit 4's test (T12).
        let redirected = "http://edge-3.rtp.pt/other/path/playlist.m3u8";
        let m = master(fixture!("10-master.m3u8"), redirected);
        assert_eq!(
            m.variants[0].uri.as_str(),
            "http://edge-3.rtp.pt/other/path/chunklist.m3u8"
        );

        // 02, 07, 08: one variant each, behind a redirect in the census.
        for (text, bw, codec) in [
            (fixture!("02-master.m3u8"), 24_000, "mp4a.40.2"),
            (fixture!("07-master.m3u8"), 128_000, "mp4a.40.2"),
            (fixture!("08-master.m3u8"), 64_000, "mp4a.40.5"),
        ] {
            let m = master(text, BASE_02);
            assert_eq!(m.variants.len(), 1);
            assert_eq!(m.variants[0].bandwidth, Some(bw));
            assert_eq!(
                m.variants[0].codecs.as_deref(),
                Some(&[codec.to_string()][..])
            );
        }

        // 03: two variants, video only.
        let m = master(fixture!("03-master.m3u8"), BASE_01);
        assert_eq!(m.variants.len(), 2);
        assert_eq!(m.variants[1].bandwidth, Some(415_296));
    }

    #[test]
    fn t1_codecs_with_a_comma_is_one_attribute_and_crlf_parses() {
        // 04 arrived CRLF (the only fixture that did) and lists five muxed variants plus an
        // EXT-X-MEDIA subtitles rendition, which is ignored. Mutation "split the attribute list
        // on every comma" → CODECS reads `avc1.4D401E` alone and this fails.
        let text = fixture!("04-master.m3u8");
        assert!(text.contains("\r\n"), "the fixture is CRLF as served");
        let m = master(text, BASE_04);
        assert_eq!(m.variants.len(), 5);
        assert_eq!(
            m.variants[0].codecs.as_deref(),
            Some(&["avc1.4D401E".to_string(), "mp4a.40.2".to_string()][..])
        );
        assert_eq!(m.variants[0].bandwidth, Some(1_061_313));
        assert_eq!(
            m.variants[0].uri.as_str(),
            "https://dwamdstream104.akamaized.net/hls/live/2015530/dwstream104/stream01/streamPlaylist.m3u8"
        );
    }

    // ---- T2: choose_variant

    fn variant(bw: u64, codecs: Option<&[&str]>) -> Variant {
        Variant {
            uri: url(&format!("http://h/{bw}.m3u8")),
            bandwidth: Some(bw),
            codecs: codecs.map(|c| c.iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn t2_video_only_master_is_refused_and_muxed_takes_the_lowest_bandwidth() {
        // 03: two avc1 variants, no audio anywhere. Mutation "drop the video-only filter" →
        // returns a variant.
        let m = master(fixture!("03-master.m3u8"), BASE_01);
        assert_eq!(choose_variant(&m), Err(NoAudio("avc1.42c020".to_string())));
        assert_eq!(
            NoAudio("avc1.42c020".to_string()).to_string(),
            "HLS stream has no audio variant (video only: avc1.42c020)"
        );

        // 04: five muxed variants → the lowest bandwidth.
        let m = master(fixture!("04-master.m3u8"), BASE_04);
        assert_eq!(choose_variant(&m).unwrap().bandwidth, Some(1_061_313));
    }

    #[test]
    fn t2_audio_only_prefers_lc_over_he_then_the_highest_bandwidth() {
        // LC at 96 k beats HE at 128 k: no SBR is decoded (Step 0 (b)), so LC sounds better.
        let m = MasterPlaylist {
            variants: vec![
                variant(128_000, Some(&["mp4a.40.5"])),
                variant(96_000, Some(&["mp4a.40.2"])),
            ],
        };
        assert_eq!(choose_variant(&m).unwrap().bandwidth, Some(96_000));
        // Mutation "bandwidth only" → 128 k above; and here 256 k, not 48 k (the brief's
        // "lowest").
        let m = MasterPlaylist {
            variants: vec![
                variant(48_000, Some(&["mp4a.40.2"])),
                variant(256_000, Some(&["mp4a.40.2"])),
                variant(128_000, Some(&["mp4a.40.2"])),
            ],
        };
        assert_eq!(choose_variant(&m).unwrap().bandwidth, Some(256_000));
        // HE-AAC v2 counts as HE; audio-only beats muxed whatever the bandwidth.
        let m = MasterPlaylist {
            variants: vec![
                variant(1_000_000, Some(&["avc1.4D401E", "mp4a.40.2"])),
                variant(64_000, Some(&["mp4a.40.29"])),
            ],
        };
        assert_eq!(choose_variant(&m).unwrap().bandwidth, Some(64_000));
    }

    #[test]
    fn t2_no_codecs_is_tried_before_muxed_and_never_a_video_only_one() {
        let m = MasterPlaylist {
            variants: vec![
                variant(2_000_000, Some(&["avc1.4D401E", "mp4a.40.2"])),
                variant(300_000, None),
                variant(500_000, Some(&["avc1.42c020"])),
            ],
        };
        assert_eq!(choose_variant(&m).unwrap().bandwidth, Some(300_000));
        let m = MasterPlaylist { variants: vec![] };
        assert_eq!(choose_variant(&m), Err(NoAudio("no variants".to_string())));
    }

    // ---- T3: media playlists parse

    #[test]
    fn t3_media_playlists_parse_with_the_duration_up_to_the_first_comma() {
        // 01: TD 10, sequence 244198, ten segments, relative URIs against the media base.
        let m = media(fixture!("01-media.m3u8"), BASE_01_MEDIA);
        assert_eq!(m.target_duration, Duration::from_secs(10));
        assert_eq!(m.media_sequence, 244_198);
        assert_eq!(m.segments.len(), 10);
        assert_eq!(m.segments[0].seq, 244_198);
        assert_eq!(m.last_seq(), Some(244_207));
        assert!((secs(m.segments[0].duration) - 10.00533).abs() < 1e-9);
        assert_eq!(
            m.segments[0].uri.as_str(),
            "https://voa-ingest.akamaized.net/hls/live/2035206/151_124L/20260824T023940/playlist124L/02441/playlist124L4_00098.aac"
        );
        assert!(!m.end_list);
        assert_eq!(m.discontinuities(), 0);

        // 02: a DISCONTINUITY before the first segment and titles that carry quoted commas.
        // Mutation `split(',').last()` → the duration is the tail of the title and this fails.
        let m = media(fixture!("02-media.m3u8"), BASE_02);
        assert_eq!(m.target_duration, Duration::from_secs(10));
        assert_eq!(m.media_sequence, 179_287_668);
        assert_eq!(m.segments.len(), 3);
        assert_eq!(m.discontinuities(), 1);
        assert!(m.segments[0].discontinuity && !m.segments[1].discontinuity);
        for s in &m.segments {
            assert_eq!(secs(s.duration), 10.0);
        }
        assert_eq!(
            m.segments[2].uri.as_str(),
            "http://cloud-proxy-hls.revma.ihrhls.com/zc6951/29_1q6zwms8wshwa02/main/179287668.aac?rj-org=n1db-e2"
        );

        // 07: three segments of 9.984 s, relative `1.aac`.
        let m = media(fixture!("07-media.m3u8"), BASE_07_MEDIA);
        assert_eq!(m.segments.len(), 3);
        assert_eq!(m.media_sequence, 1);
        assert!((secs(m.segments[0].duration) - 9.984).abs() < 1e-9);
        assert!(m.segments[0].uri.as_str().ends_with("/0/1.aac"));
    }

    #[test]
    fn t3_gzipped_media_playlist_parses_after_gunzip() {
        // 10: served `content-encoding: gzip`; the fixture is the compressed bytes as sent.
        let raw = fixture_bytes!("10-media.m3u8.gz");
        assert_eq!(&raw[..2], &[0x1f, 0x8b], "the fixture is gzip as served");
        let m = media(&gunzip(raw), BASE_10_MEDIA);
        assert_eq!(m.target_duration, Duration::from_secs(5));
        assert_eq!(m.media_sequence, 97_863);
        assert_eq!(m.segments.len(), 20);
        assert_eq!(m.last_seq(), Some(97_882));
        assert!((secs(m.segments[0].duration) - 4.032).abs() < 1e-9);
        assert!((secs(m.segments[1].duration) - 3.968).abs() < 1e-9);
        assert_eq!(
            m.segments[0].uri.as_str(),
            "http://streaming-live-app.rtp.pt/liveradio/antena180a/media_97863.aac"
        );
    }

    #[test]
    fn t3_media_playlists_given_directly_parse_with_absolute_path_and_query_uris() {
        // 06: the station URL is the media playlist; absolute-path URIs with a query.
        let m = media(fixture!("06-media.m3u8"), BASE_06);
        assert_eq!(m.target_duration, Duration::from_secs(4));
        assert_eq!(m.media_sequence, 2_099_281);
        assert_eq!(m.segments.len(), 7);
        assert_eq!(
            m.segments[0].uri.as_str(),
            "https://stream.radiofrance.fr/accs3/franceinter/prod1transcoder2/franceinter_aac_hifi_4_2099281_1790008140.ts?id=radiofrance"
        );
        // 09: relative URIs with a query, TD 7.
        let m = media(fixture!("09-media.m3u8"), BASE_09);
        assert_eq!(m.target_duration, Duration::from_secs(7));
        assert_eq!(m.segments.len(), 4);
        assert_eq!(
            m.segments[0].uri.as_str(),
            "https://hls-igi.cdnvideo.ru/igi/radio1/tracks-a1/2026/09/21/16/30/05-06016.ts?hls_proxy_host=e3f8932b050e87d55d9c3dc9bf52d17a"
        );
    }

    // ---- T4: refusals

    const MEDIA_HEAD: &str = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:1\n";

    #[test]
    fn t4_map_key_and_byterange_are_refused_and_key_none_is_accepted() {
        let base = url("http://h/p.m3u8");
        let with = |tag: &str| format!("{MEDIA_HEAD}{tag}\n#EXTINF:6,\ns1.aac\n");

        assert_eq!(
            parse(&with("#EXT-X-MAP:URI=\"init.mp4\""), &base),
            Err(PlaylistError::Fmp4)
        );
        // Mutation "remove the KEY check" → this parses and a DRM stream reaches the decoder
        // as noise.
        assert_eq!(
            parse(&with("#EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV=0x1"), &base),
            Err(PlaylistError::Encrypted)
        );
        assert_eq!(
            parse(&with("#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\""), &base),
            Err(PlaylistError::Encrypted)
        );
        assert_eq!(
            parse(&with("#EXT-X-BYTERANGE:1000@0"), &base),
            Err(PlaylistError::ByteRange)
        );
        let ok = parse(&with("#EXT-X-KEY:METHOD=NONE"), &base).unwrap();
        assert!(matches!(ok, Playlist::Media(ref m) if m.segments.len() == 1));

        assert_eq!(
            PlaylistError::Fmp4.to_string(),
            "HLS with fMP4 segments is not supported yet"
        );
        assert_eq!(
            PlaylistError::Encrypted.to_string(),
            "encrypted HLS is not supported"
        );
        assert_eq!(
            PlaylistError::ByteRange.to_string(),
            "HLS byte-range segments are not supported"
        );
    }

    #[test]
    fn t4_a_plain_m3u_is_not_hls_and_its_url_is_not_followed() {
        // R2: `audio/x-mpegurl` is also served for an ordinary M3U that lists an Icecast URL.
        // It starts with #EXTM3U, has no EXT-X-* tag, and must be refused, not read as a media
        // playlist with one segment. Mutation "drop the EXT-X presence check" → parses as
        // `Media` with `segments.len() == 1` (an Icecast mount fetched as a segment).
        let base = url("http://h/listen.m3u");
        let plain = "#EXTM3U\n#EXTINF:-1,Some Radio\nhttp://icecast.example/stream\n";
        assert_eq!(parse(plain, &base), Err(PlaylistError::NotHls));
        let bare = "#EXTM3U\nhttp://icecast.example/stream\n";
        assert_eq!(parse(bare, &base), Err(PlaylistError::NotHls));
        assert_eq!(
            PlaylistError::NotHls.to_string(),
            "playlist is not HLS (no EXT-X tags)"
        );
    }

    #[test]
    fn t4_a_body_without_extm3u_is_not_a_playlist_and_bad_numbers_are_malformed() {
        let base = url("http://h/p.m3u8");
        assert_eq!(parse("<html>", &base), Err(PlaylistError::NotPlaylist));
        assert_eq!(parse("", &base), Err(PlaylistError::NotPlaylist));
        assert!(matches!(
            parse("#EXTM3U\n#EXT-X-TARGETDURATION:ten\n", &base),
            Err(PlaylistError::Malformed(_))
        ));
        assert!(matches!(
            parse(&format!("{MEDIA_HEAD}#EXTINF:abc,\ns.aac\n"), &base),
            Err(PlaylistError::Malformed(_))
        ));
        // Unknown tags and comments are ignored; `#EXTM3U` may carry trailing whitespace.
        let ok = parse(
            &format!(
                "#EXTM3U \n#EXT-X-VERSION:3\n# a comment\n#EXT-X-FOO:bar\n{}",
                &MEDIA_HEAD[8..]
            ),
            &base,
        );
        assert!(matches!(ok, Ok(Playlist::Media(_))));
    }

    // ---- T5: where playback starts

    fn t0() -> Instant {
        Instant::now()
    }

    fn fetched_seqs(step: &Step) -> Vec<u64> {
        match step {
            Step::Fetch { segments, .. } => segments.iter().map(|s| s.seq).collect(),
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    #[test]
    fn t5_start_is_three_segments_from_the_end_or_the_first_of_a_short_window() {
        // Mutation "start at the last segment" → 244207 / 97882 / 3; "start at the first" →
        // 244198 / 97863 (minutes behind live).
        let m = media(fixture!("01-media.m3u8"), BASE_01_MEDIA);
        let (p, step) = Planner::start(&m, t0());
        assert_eq!(fetched_seqs(&step), vec![244_205, 244_206, 244_207]);
        assert_eq!(p.next_seq(), 244_208);

        let m = media(&gunzip(fixture_bytes!("10-media.m3u8.gz")), BASE_10_MEDIA);
        let (p, step) = Planner::start(&m, t0());
        assert_eq!(fetched_seqs(&step), vec![97_880, 97_881, 97_882]);
        assert_eq!(p.next_seq(), 97_883);

        let m = media(fixture!("07-media.m3u8"), BASE_07_MEDIA);
        let (p, step) = Planner::start(&m, t0());
        assert_eq!(fetched_seqs(&step), vec![1, 2, 3]);
        assert_eq!(p.next_seq(), 4);
        // The wait after the start is the last segment's duration.
        match step {
            Step::Fetch { then_wait, .. } => assert!((secs(then_wait) - 9.984).abs() < 1e-9),
            _ => unreachable!(),
        }
    }

    // ---- T6: the planner

    fn synthetic(td: u64, first_seq: u64, n: usize, dur: f64) -> MediaPlaylist {
        MediaPlaylist {
            target_duration: Duration::from_secs(td),
            media_sequence: first_seq,
            segments: (0..n as u64)
                .map(|i| Segment {
                    seq: first_seq + i,
                    duration: Duration::from_secs_f64(dur),
                    uri: url(&format!("http://h/{}.aac", first_seq + i)),
                    discontinuity: false,
                })
                .collect(),
            end_list: false,
        }
    }

    #[test]
    fn t6_a_reload_emits_exactly_the_new_segments_and_waits_the_last_duration() {
        // 01's real refresh, 20 s after the first: sequence 244198 → 244201, three new
        // segments (244208–244210); seven of its ten URIs repeat the first playlist's.
        let first = media(fixture!("01-media.m3u8"), BASE_01_MEDIA);
        let refresh = media(fixture!("01-media-refresh.m3u8"), BASE_01_MEDIA);
        let t = t0();
        let (mut p, _) = Planner::start(&first, t);
        let step = p.reload(Some(&refresh), t + Duration::from_secs(20));
        assert_eq!(fetched_seqs(&step), vec![244_208, 244_209, 244_210]);
        match &step {
            Step::Fetch {
                skipped, then_wait, ..
            } => {
                assert_eq!(*skipped, 0);
                assert!((secs(*then_wait) - 10.00533).abs() < 1e-9);
            }
            _ => unreachable!(),
        }
        assert_eq!(p.next_seq(), 244_211);
        // The same playlist again: nothing new → TD / 2.
        let step = p.reload(Some(&refresh), t + Duration::from_secs(25));
        assert_eq!(step, Step::Wait(Duration::from_secs(5)));
    }

    #[test]
    fn t6_a_window_that_still_lists_older_segments_emits_only_past_the_last_emitted() {
        // Gate amendment 6. 10's refresh, 10 s after the first: the window moved 97863 → 97866
        // and still lists 97866–97882, all of which are at or before what was already fetched
        // (97880–97882). Only 97883–97885 are new. Mutation "track a set of seen sequence
        // numbers (or URIs) instead of `seq >= next_seq`" → 97866–97879 are emitted again, out
        // of order behind 97882: fourteen already-played seconds replayed.
        let first = media(&gunzip(fixture_bytes!("10-media.m3u8.gz")), BASE_10_MEDIA);
        let refresh = media(
            &gunzip(fixture_bytes!("10-media-refresh.m3u8.gz")),
            BASE_10_MEDIA,
        );
        assert_eq!(refresh.media_sequence, 97_866);
        assert_eq!(
            refresh.segments[0].seq, 97_866,
            "the window starts before next_seq"
        );
        let t = t0();
        let (mut p, _) = Planner::start(&first, t);
        assert_eq!(p.next_seq(), 97_883);
        let step = p.reload(Some(&refresh), t + Duration::from_secs(10));
        assert_eq!(fetched_seqs(&step), vec![97_883, 97_884, 97_885]);
        assert_eq!(p.next_seq(), 97_886);
    }

    #[test]
    fn t6_waits_are_clamped_to_one_and_thirty_seconds() {
        // Mutation "drop the floor" → TD 0 gives a 0 s wait and a hammer loop; "drop the
        // ceiling" → an hour.
        let t = t0();
        let (mut p, step) = Planner::start(&synthetic(0, 1, 3, 0.1), t);
        assert!(matches!(step, Step::Fetch { then_wait, .. } if then_wait == MIN_WAIT));
        assert_eq!(
            p.reload(Some(&synthetic(0, 1, 3, 0.1)), t),
            Step::Wait(MIN_WAIT)
        );
        // …and the stall bound is on the clamped TD too: 3 × 1 s, not 3 × 0 = a stall at once.
        assert_eq!(
            p.reload(
                Some(&synthetic(0, 1, 3, 0.1)),
                t + Duration::from_millis(2999)
            ),
            Step::Wait(MIN_WAIT)
        );
        assert_eq!(
            p.reload(Some(&synthetic(0, 1, 3, 0.1)), t + Duration::from_secs(3)),
            Step::End(EndCause::Stall)
        );

        let (mut p, step) = Planner::start(&synthetic(3600, 1, 3, 3600.0), t);
        assert!(matches!(step, Step::Fetch { then_wait, .. } if then_wait == MAX_WAIT));
        assert_eq!(
            p.reload(Some(&synthetic(3600, 1, 3, 3600.0)), t),
            Step::Wait(MAX_WAIT)
        );
    }

    #[test]
    fn t6_a_window_past_next_seq_is_a_gap_and_a_stalled_window_ends_the_stream() {
        let t = t0();
        let (mut p, _) = Planner::start(&synthetic(5, 100, 6, 5.0), t); // emits 103–105
        assert_eq!(p.next_seq(), 106);
        // The window jumped to 110–115: 106–109 are gone. Mutation "start the fetch at
        // next_seq regardless" cannot fetch them; mutation "skipped = 0" hides the gap.
        let step = p.reload(Some(&synthetic(5, 110, 6, 5.0)), t + Duration::from_secs(5));
        assert_eq!(fetched_seqs(&step), vec![110, 111, 112, 113, 114, 115]);
        assert!(matches!(step, Step::Fetch { skipped: 4, .. }));

        // Unchanged for 3 × TD = 15 s → Stall (checked at 14 s: still waiting; 15 s: ended).
        // Mutation "no stall bound" → waits for ever (the engine watchdog does not see it: the
        // ring is full of nothing new but the state is Playing, not Buffering).
        let same = synthetic(5, 110, 6, 5.0);
        let t_new = t + Duration::from_secs(5);
        assert_eq!(
            p.reload(Some(&same), t_new + Duration::from_secs(14)),
            Step::Wait(Duration::from_millis(2500))
        );
        assert_eq!(
            p.reload(Some(&same), t_new + Duration::from_secs(15)),
            Step::End(EndCause::Stall)
        );
    }

    #[test]
    fn t6_a_failed_reload_waits_half_a_target_duration_until_the_stall_bound() {
        let t = t0();
        let (mut p, _) = Planner::start(&synthetic(10, 1, 5, 10.0), t);
        assert_eq!(
            p.reload(None, t + Duration::from_secs(10)),
            Step::Wait(Duration::from_secs(5))
        );
        assert_eq!(
            p.reload(None, t + Duration::from_secs(29)),
            Step::Wait(Duration::from_secs(5))
        );
        assert_eq!(
            p.reload(None, t + Duration::from_secs(30)),
            Step::End(EndCause::Stall)
        );
        // …and a successful reload with new segments resets the bound.
        let (mut p, _) = Planner::start(&synthetic(10, 1, 5, 10.0), t);
        p.reload(None, t + Duration::from_secs(25));
        let step = p.reload(
            Some(&synthetic(10, 3, 5, 10.0)),
            t + Duration::from_secs(29),
        );
        assert_eq!(fetched_seqs(&step), vec![6, 7]);
        assert_eq!(
            p.reload(None, t + Duration::from_secs(58)),
            Step::Wait(Duration::from_secs(5))
        );
    }

    #[test]
    fn t6_a_sequence_that_goes_backwards_is_a_restart_and_endlist_ends_the_stream() {
        let t = t0();
        let (mut p, _) = Planner::start(&synthetic(5, 500, 6, 5.0), t);
        // Ordinary: the window advances 500 → 502 (older segments still listed) — not a restart.
        assert!(matches!(
            p.reload(Some(&synthetic(5, 502, 6, 5.0)), t + Duration::from_secs(5)),
            Step::Fetch { .. }
        ));
        // The server restarted its numbering at 1. Mutation "compare against next_seq instead
        // of the last window start" cannot tell this from a lagging origin either way; the rule
        // is the plan's: MEDIA-SEQUENCE lower than the last accepted one.
        assert_eq!(
            p.reload(Some(&synthetic(5, 1, 6, 5.0)), t + Duration::from_secs(10)),
            Step::End(EndCause::Restarted)
        );

        // EXT-X-ENDLIST: the remaining segments are emitted, then the stream ends.
        let mut vod = synthetic(5, 1, 4, 5.0);
        vod.end_list = true;
        let (mut p, step) = Planner::start(&vod, t);
        assert_eq!(fetched_seqs(&step), vec![2, 3, 4]);
        assert_eq!(
            p.reload(Some(&vod), t + Duration::from_secs(5)),
            Step::End(EndCause::EndList)
        );
    }

    #[test]
    fn attribute_lists_split_quote_aware() {
        let a = attribute_list(
            r#"BANDWIDTH=1061313,CODECS="avc1.4D401E,mp4a.40.2",RESOLUTION=480x270,SUBTITLES="subs_wvtt""#,
        );
        assert_eq!(
            a,
            vec![
                ("BANDWIDTH", "1061313".to_string()),
                ("CODECS", "avc1.4D401E,mp4a.40.2".to_string()),
                ("RESOLUTION", "480x270".to_string()),
                ("SUBTITLES", "subs_wvtt".to_string()),
            ]
        );
        assert_eq!(attribute_list(""), Vec::<(&str, String)>::new());
        assert_eq!(
            attribute_list("METHOD=NONE"),
            vec![("METHOD", "NONE".to_string())]
        );
    }
}
