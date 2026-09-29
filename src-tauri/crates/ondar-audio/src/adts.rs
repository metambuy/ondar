//! The ADTS front end (defect B): a streaming reader that realigns an Icecast `audio/aac*` body
//! to a walked ADTS header and normalises every frame before the decoder sees it.
//!
//! Why (defect B Step 0): 23.8 % of Icecast ADTS bodies send `FFF9` headers (the MPEG-2 ID bit),
//! which Symphonia's ADTS reader never syncs on, and 8 % start mid-frame; in 36 of 42 `FFF9`
//! bodies the probe met a false MP3 marker first and resynced for ever. The HLS path already
//! fixes the same bytes per segment ([`crate::hls::segment`]); this is the same walk over a
//! stream, with **no second parser**: [`normalise_adts`] does the work, this module only feeds it
//! across read boundaries.
//!
//! Three states:
//!
//! 1. **Realigning.** Bytes accumulate in a head buffer. Leading ID3 tags are skipped
//!    ([`id3_end`]); the first offset `k` with a candidate header — layer 0, a reserved-free
//!    sample-rate index, `frame_length` at least the header — **followed by two more** at
//!    `k + len` and `k + len + len₂` with the same profile, sample-rate index and channel
//!    configuration is the alignment. `[0, k)` is dropped and the rest is normalised. If no
//!    chain starts below [`REALIGN_MAX_BYTES`] **before the stream has ever aligned**, the
//!    reader **passes through**: it never refuses — a mount that is not ADTS reaches the
//!    decoder as before, and the engine's build bound covers the rest. **After** an alignment
//!    (a realign after a sync loss) it never passes through: the bytes below the limit are
//!    dropped and the scan goes on over the next window (review fixes F3, finding 4 — an
//!    `FFF9` mount passed through after a long splice fed a built decoder headers it never
//!    syncs on). Whether and where it aligns depends on the bytes alone, never on how the
//!    inner reads were chunked.
//! 2. **Normalising.** Each read runs [`normalise_adts`] on the carry plus the new bytes and
//!    emits the whole frames (ID bit cleared, CRC dropped, `frame_length` fixed); the trailing
//!    partial frame, or a header split at any offset, is kept as the carry for the next read. A
//!    sync loss (a splice from stream-download's internal reconnect, garbage) drops back to
//!    realigning from that offset with a fresh budget.
//! 3. **Pass-through.** Terminal: every byte unchanged. Reached only before the first
//!    alignment.
//!
//! EOF while normalising drops the partial carry. EOF while realigning passes the head through
//! if the stream never aligned (a short non-ADTS body is not eaten) and drops it after a sync
//! loss (the tail of a splice). An inner `Err` propagates with the state intact.
//!
//! No `unwrap`, `expect`, panic or unchecked index outside the tests (the release profile is
//! `panic = "abort"`: a panic on network bytes is a whole-app abort); `tests` scans for them.

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};

use crate::hls::segment::{ADTS_HEADER_LEN, frame_length, id3_end, is_adts_sync, normalise_adts};

/// The alignment must start below this offset, else the reader passes through: two maximum
/// ADTS frames (8 191 B each) over the measured maximum of 736 B before the first header
/// (Step 0, S4). The head may grow past it while a candidate below it waits for the bytes
/// that decide its chain (at most three frames more). **What fails it:** an `audio/aac*` mount
/// whose first chained header is 16 KiB or more in — a leading tag over 16 KiB, 0 of 225 bodies
/// seen — which then passes through to `main`'s behaviour, bounded by the build bound.
pub const REALIGN_MAX_BYTES: usize = 16 * 1024;

/// The most one inner read asks for.
const READ_CHUNK: usize = 16 * 1024;

/// The CRC that follows a header whose `protection_absent` is 0.
const ADTS_CRC_LEN: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Realigning,
    Normalising,
    PassThrough,
}

/// What a header at an offset declares, for the chain check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate {
    profile: u8,
    sri: u8,
    channel_config: u8,
    frame_len: usize,
}

/// The chain check at one offset.
enum Chain {
    /// Three headers chain from here.
    Found,
    /// Not an alignment.
    No,
    /// Could be one; the bytes to decide have not arrived.
    NeedMore,
}

/// The front end over `R` (after `IcyReader`, before the decoder). See the module doc.
pub struct AdtsReader<R: Read> {
    inner: R,
    state: State,
    /// Realigning: the head since the realign began. Normalising: the carry. Pass-through:
    /// empty.
    buf: Vec<u8>,
    /// Realigning: offsets below this were already rejected (the bytes before them never
    /// change, so neither does the answer).
    scan_from: usize,
    /// Bytes ready for the caller.
    out: VecDeque<u8>,
    scratch: Vec<u8>,
    /// Whether the stream has aligned once; a realign after that is a resync, and the reader
    /// never passes through again.
    aligned_once: bool,
    /// Resyncs so far: the first is logged at `info`, the rest at `debug`.
    realigns: u32,
    eof: bool,
}

impl<R: Read> AdtsReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            state: State::Realigning,
            buf: Vec::new(),
            scan_from: 0,
            out: VecDeque::new(),
            scratch: vec![0; READ_CHUNK],
            aligned_once: false,
            realigns: 0,
            eof: false,
        }
    }

    /// Move what the new bytes allow from `buf` to `out`.
    fn advance(&mut self) {
        loop {
            match self.state {
                State::PassThrough => {
                    self.out.extend(self.buf.drain(..));
                    return;
                }
                State::Normalising => {
                    let n = normalise_adts(&self.buf);
                    self.out.extend(n.bytes.iter());
                    if let Some((off, _)) = n.sync_lost {
                        log::debug!(
                            "adts front end: sync lost after {} frames; realigning",
                            n.frames
                        );
                        self.buf.drain(..off.min(self.buf.len()));
                        self.state = State::Realigning;
                        self.scan_from = 0;
                        continue;
                    }
                    let keep = n.partial_dropped.min(self.buf.len());
                    let consumed = self.buf.len() - keep;
                    self.buf.drain(..consumed);
                    return;
                }
                State::Realigning => {
                    // Only offsets below the limit can start the alignment; the answer at each
                    // depends on the bytes alone, never on how the reads were chunked.
                    let limit = REALIGN_MAX_BYTES.min(self.buf.len());
                    let mut k = self.scan_from.max(id3_end(&self.buf));
                    let mut found = false;
                    while k < limit {
                        match chain_at(&self.buf, k) {
                            Chain::Found => {
                                found = true;
                                break;
                            }
                            Chain::No => k += 1,
                            Chain::NeedMore => break,
                        }
                    }
                    if found {
                        if self.aligned_once {
                            self.realigns = self.realigns.saturating_add(1);
                            if self.realigns == 1 {
                                log::info!("adts front end: realigned at {k} B after a sync loss");
                            } else {
                                log::debug!(
                                    "adts front end: realigned at {k} B after a sync loss \
                                     ({} so far)",
                                    self.realigns
                                );
                            }
                        } else {
                            log::info!("adts front end: aligned at {k} B");
                        }
                        self.aligned_once = true;
                        self.buf.drain(..k);
                        self.scan_from = 0;
                        self.state = State::Normalising;
                        continue;
                    }
                    self.scan_from = k;
                    if k >= REALIGN_MAX_BYTES && self.aligned_once {
                        // After an alignment, slide: no chain starts below `k`, and the bytes
                        // before it never change, so drop them and scan the next window.
                        log::debug!(
                            "adts front end: no chained header in {k} B after a sync loss; \
                             dropped, still realigning"
                        );
                        self.buf.drain(..k.min(self.buf.len()));
                        self.scan_from = 0;
                        continue;
                    }
                    if k >= REALIGN_MAX_BYTES {
                        log::warn!(
                            "adts front end: no chained ADTS header in {} KiB; passing through",
                            REALIGN_MAX_BYTES / 1024
                        );
                        self.state = State::PassThrough;
                        continue;
                    }
                    return;
                }
            }
        }
    }

    /// The inner reader ended: settle what is left (see the module doc).
    fn finish(&mut self) {
        match self.state {
            State::Realigning if !self.aligned_once => self.out.extend(self.buf.drain(..)),
            State::PassThrough => self.out.extend(self.buf.drain(..)),
            State::Realigning | State::Normalising => self.buf.clear(),
        }
    }
}

/// The header at `p`, if it is a candidate: an ADTS sync with layer 0, a sample-rate index
/// below 13 and a `frame_length` of at least its own header. `None` also when fewer than a
/// header's bytes are there (the caller tells the two apart by length).
fn candidate(b: &[u8], p: usize) -> Option<Candidate> {
    let h = b.get(p..p.checked_add(ADTS_HEADER_LEN)?)?;
    if !is_adts_sync(h) {
        return None;
    }
    let (b1, b2, b3) = (*h.get(1)?, *h.get(2)?, *h.get(3)?);
    let sri = (b2 >> 2) & 0x0F;
    if sri >= 13 {
        return None;
    }
    let header_len = if b1 & 0x01 == 1 {
        ADTS_HEADER_LEN
    } else {
        ADTS_HEADER_LEN + ADTS_CRC_LEN
    };
    let frame_len = frame_length(h);
    if frame_len < header_len {
        return None;
    }
    Some(Candidate {
        profile: b2 >> 6,
        sri,
        channel_config: ((b2 & 0x01) << 2) | (b3 >> 6),
        frame_len,
    })
}

/// Three chained headers from `k`?
fn chain_at(b: &[u8], k: usize) -> Chain {
    let enough = |p: usize| p.checked_add(ADTS_HEADER_LEN).is_some_and(|e| e <= b.len());
    if !enough(k) {
        return Chain::NeedMore;
    }
    let Some(first) = candidate(b, k) else {
        return Chain::No;
    };
    let mut p = k;
    let mut len = first.frame_len;
    for _ in 0..2 {
        let Some(next) = p.checked_add(len) else {
            return Chain::No;
        };
        p = next;
        if !enough(p) {
            return Chain::NeedMore;
        }
        match candidate(b, p) {
            Some(c)
                if c.profile == first.profile
                    && c.sri == first.sri
                    && c.channel_config == first.channel_config =>
            {
                len = c.frame_len;
            }
            _ => return Chain::No,
        }
    }
    Chain::Found
}

impl<R: Read> Read for AdtsReader<R> {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        if dst.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.out.is_empty() {
                let n = dst.len().min(self.out.len());
                for (d, s) in dst.iter_mut().zip(self.out.drain(..n)) {
                    *d = s;
                }
                return Ok(n);
            }
            if self.eof {
                return Ok(0);
            }
            let n = self.inner.read(&mut self.scratch)?;
            if n == 0 {
                self.eof = true;
                self.finish();
                continue;
            }
            self.buf
                .extend_from_slice(self.scratch.get(..n).unwrap_or_default());
            self.advance();
        }
    }
}

/// Position-only, as `IcyReader`'s: the decoder is built non-seekable.
impl<R: Read> Seek for AdtsReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match pos {
            SeekFrom::Current(0) => Ok(0),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "seeking is not supported on a live ADTS stream",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    //! T-B3 of the defect B plan. The module is new in C3, so every row is mutation-checked;
    //! each assertion names the mutation that fails it. Nothing here is committed as a fixture:
    //! the synthetic frames come from `segment::test_support::frame`, the census heads from
    //! `fixtures/hls/`.

    use super::*;
    use crate::hls::segment::normalise;
    use crate::hls::segment::test_support::frame;

    macro_rules! head {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/hls/", $name)) as &[u8]
        };
    }
    /// Station 10: ID3 then 16 `FFF1` frames, 48 000/2.
    const HEAD_10: &[u8] = head!("10-seg-head.aac");
    /// Station 02: ID3 then 16 `FFF9` frames, 24 000/1.
    const HEAD_02: &[u8] = head!("02-seg-head.aac");

    /// An inner reader that hands out `data` in reads of the sizes `sizes` yields (cycled),
    /// optionally failing once with `Interrupted` before its first byte.
    struct Chunked {
        data: Vec<u8>,
        at: usize,
        sizes: Box<dyn FnMut() -> usize>,
        interrupt_first: bool,
    }

    impl Read for Chunked {
        fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
            if self.interrupt_first {
                self.interrupt_first = false;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            let n = (self.sizes)()
                .max(1)
                .min(dst.len())
                .min(self.data.len() - self.at);
            dst[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    fn chunked(data: &[u8], sizes: impl FnMut() -> usize + 'static) -> Chunked {
        Chunked {
            data: data.to_vec(),
            at: 0,
            sizes: Box::new(sizes),
            interrupt_first: false,
        }
    }

    fn fixed(n: usize) -> impl FnMut() -> usize {
        move || n
    }

    /// Seeded sizes in 1..=max.
    fn random_sizes(seed: u64, max: usize) -> impl FnMut() -> usize {
        let mut x = seed.max(1);
        move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % max as u64) as usize + 1
        }
    }

    /// Two reads: `data[..cut]`, then the rest.
    fn split_at(data: &[u8], cut: usize) -> Chunked {
        let mut first = true;
        let rest = data.len() - cut;
        chunked(data, move || {
            if std::mem::take(&mut first) {
                cut.max(1)
            } else {
                rest.max(1)
            }
        })
    }

    /// Everything the front end yields over `inner`, reading with a 4 KiB buffer. An
    /// `Interrupted` is retried, as `read_to_end` does.
    fn through(inner: Chunked) -> Vec<u8> {
        let mut r = AdtsReader::new(inner);
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match r.read(&mut buf) {
                Ok(0) => return out,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
    }

    /// Builds a series of read sizes, fresh for each run.
    type MakeSizes = fn() -> Box<dyn FnMut() -> usize>;

    /// Builds the inner reader for one chunking.
    type MakeInner = Box<dyn Fn(&[u8]) -> Chunked>;

    /// Every chunking the rows use: 1..=64 and four seeded random series.
    fn chunkings() -> Vec<(String, MakeInner)> {
        let mut v: Vec<(String, MakeInner)> = Vec::new();
        for n in 1..=64 {
            v.push((
                format!("chunks of {n}"),
                Box::new(move |d| chunked(d, fixed(n))),
            ));
        }
        for seed in 1..=4u64 {
            v.push((
                format!("random chunks, seed {seed}"),
                Box::new(move |d| chunked(d, random_sizes(seed, 5000))),
            ));
        }
        v
    }

    fn frames(n: usize, sri: u8, ch: u8, id: bool, crc: bool) -> Vec<u8> {
        (0..n)
            .flat_map(|i| frame(sri, ch, 100 + i, i as u8, id, crc))
            .collect()
    }

    /// Seeded `0x00–0x3F`: no `0xFF`, so nothing in it can sync.
    fn junk(len: usize, seed: u64) -> Vec<u8> {
        let mut next = random_sizes(seed, 64);
        (0..len).map(|_| (next() - 1) as u8).collect()
    }

    // ---- (1), (2): the census heads, whatever the chunking

    /// (1) An `FFF1` head passes byte-identical after its ID3 tag, in every chunking; and an
    /// ID3 tag whose payload holds three chained ADTS headers is skipped whole, not aligned
    /// on. Fails if the carry is dropped at a read boundary (frames lost), if the realign drops
    /// a whole frame, or if the ID3 skip is off (the tag's frames come out first — station
    /// 10's own tag holds no chain, so only the second half pins the skip).
    #[test]
    fn t_b3_1_an_fff1_head_passes_byte_identical_in_every_chunking() {
        let body = &HEAD_10[id3_end(HEAD_10)..];
        assert_eq!(
            normalise(HEAD_10).bytes,
            body,
            "the fixture is 16 whole FFF1 frames"
        );
        for (name, make) in chunkings() {
            assert_eq!(through(make(HEAD_10)), body, "{name}");
        }

        let decoy = frames(3, 4, 2, false, false);
        let size = decoy.len();
        let mut tagged = b"ID3\x04\x00\x00".to_vec();
        tagged.extend([(size >> 21) as u8 & 0x7F, (size >> 14) as u8 & 0x7F]);
        tagged.extend([(size >> 7) as u8 & 0x7F, size as u8 & 0x7F]);
        tagged.extend(&decoy);
        tagged.extend(body);
        assert_eq!(id3_end(&tagged), 10 + size);
        for (name, make) in chunkings() {
            assert_eq!(
                through(make(&tagged)),
                body,
                "a tag holding a chain, {name}"
            );
        }
    }

    /// (2) An `FFF9` head comes out as `normalise` makes it, in every chunking. Fails if the
    /// wrapper passes frames unwalked (`FFF9` survives) or loses the carry.
    #[test]
    fn t_b3_2_an_fff9_head_is_normalised_in_every_chunking() {
        let want = normalise(HEAD_02).bytes;
        assert_eq!(normalise(HEAD_02).rewritten, 16);
        for (name, make) in chunkings() {
            assert_eq!(through(make(HEAD_02)), want, "{name}");
        }
    }

    // ---- (3): a read boundary inside a header

    /// (3) A read boundary at every offset 1..6 inside the first header and inside the 9th.
    /// Fails if a header split across reads is treated as a sync loss (the 9th frame on is
    /// dropped or realigned) or if the realign rejects a candidate it cannot yet read.
    #[test]
    fn t_b3_3_a_read_boundary_inside_a_header_changes_nothing() {
        let want = normalise(HEAD_02).bytes;
        let first = id3_end(HEAD_02);
        let mut ninth = first;
        for _ in 0..8 {
            ninth += frame_length(&HEAD_02[ninth..]);
        }
        for base in [first, ninth] {
            for off in 1..=6 {
                assert_eq!(
                    through(split_at(HEAD_02, base + off)),
                    want,
                    "boundary {off} B into the header at {base}"
                );
            }
        }
    }

    // ---- (4): mid-frame start, with a false MP3 pair

    /// (4) The body starts 100 B into 02's first frame, with `FF FB 90 C4` planted 10 B into
    /// that partial (S2's shape, planted as T-B2b plants it). The output starts at the first
    /// whole header, rewritten. Fails if the realign is off (the partial and the MP3 pair reach
    /// the decoder) or if it takes a single header as the alignment (a false start).
    #[test]
    fn t_b3_4_a_mid_frame_start_realigns_to_the_first_whole_header() {
        let first = id3_end(HEAD_02);
        let second = first + frame_length(&HEAD_02[first..]);
        let mut body = HEAD_02[first + 100..].to_vec();
        body[10..14].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC4]);
        let want = normalise(&HEAD_02[second..]).bytes;
        for (name, make) in chunkings() {
            assert_eq!(through(make(&body)), want, "{name}");
        }
    }

    // ---- (5), (6): CRC and a sync loss

    /// (5) Frames with a CRC lose its two bytes: 7-byte headers, `protection_absent` set.
    /// Fails if the wrapper emits frames without the walk.
    #[test]
    fn t_b3_5_a_crc_frame_gets_a_7_byte_header() {
        let input = frames(4, 3, 2, false, true);
        let out = through(chunked(&input, fixed(13)));
        assert_eq!(
            out,
            frames(4, 3, 2, false, false),
            "CRC dropped, lengths fixed"
        );
        assert_eq!(out.len(), input.len() - 4 * 2);
    }

    /// (6) Four frames, 100 junk bytes, four frames: the junk is dropped and the stream
    /// realigned. Fails if a sync loss ends the walk (the last four frames lost) or passes the
    /// junk on.
    #[test]
    fn t_b3_6_a_sync_loss_mid_stream_is_dropped_and_realigned() {
        let a = frames(4, 3, 2, false, false);
        let mut input = a.clone();
        input.extend(junk(100, 9));
        input.extend(&a);
        let mut want = a.clone();
        want.extend(&a);
        for (name, make) in chunkings() {
            assert_eq!(through(make(&input)), want, "{name}");
        }
    }

    // ---- (7): the limit

    /// (7) The limit is where the alignment starts. Frames starting at 16 KiB − 1 align;
    /// starting at 16 KiB the reader passes through, and stays through: the `FFF9` frames
    /// after the junk are not rewritten (output == input). Fails if pass-through is not
    /// terminal, if the limit moves by a byte, or if it depends on the chunking (a limit on
    /// the head's length rather than the start).
    #[test]
    fn t_b3_7_the_realign_limit_is_a_start_offset() {
        let fff9 = frames(6, 6, 1, true, false);
        let mut inside = junk(REALIGN_MAX_BYTES - 1, 3);
        inside.extend(&fff9);
        let mut past = junk(REALIGN_MAX_BYTES, 3);
        past.extend(&fff9);
        let aligned = normalise_adts(&fff9).bytes;
        for (name, make) in chunkings() {
            assert_eq!(
                through(make(&inside)),
                aligned,
                "16 KiB − 1: aligned, {name}"
            );
            assert_eq!(through(make(&past)), past, "16 KiB: passed through, {name}");
        }
    }

    // ---- (10): after the first alignment, never pass through (review fixes F3)

    /// (10) Review finding 4: a stream that has aligned once never passes through. Station 02's
    /// `FFF9` frames, 20 KiB of junk (a splice, a restarting source: no chain in more than the
    /// limit), then 02's frames again: every frame comes out `FFF1` and no junk byte does, in
    /// every chunking, and the head stays bounded while the window slides. Fails on `da36489`:
    /// 28 660 B out where 8 180 were due (chunks of 1) — the first run normalised, then all
    /// 20 480 B of junk and the second run's 4 090 B raw `FFF9`, because the realign after the
    /// sync loss gave up at 16 KiB and passed through for the rest of the stream. Mutation: the slide removed (pass-through after alignment)
    /// reads the same.
    #[test]
    fn t_b3_10_after_the_first_alignment_it_never_passes_through() {
        let f02 = &HEAD_02[id3_end(HEAD_02)..];
        let mut input = f02.to_vec();
        input.extend(junk(20 * 1024, 11));
        input.extend(f02);
        let once = normalise_adts(f02).bytes;
        let mut want = once.clone();
        want.extend(&once);
        let bound = REALIGN_MAX_BYTES + 3 * 8191 + READ_CHUNK;
        for (name, make) in chunkings() {
            let mut r = AdtsReader::new(make(&input));
            let (mut out, mut head_max) = (Vec::new(), 0);
            let mut buf = [0u8; 4096];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => out.extend_from_slice(&buf[..n]),
                    Err(e) => panic!("unexpected error: {e}"),
                }
                head_max = head_max.max(r.buf.len());
            }
            assert_eq!(out.len(), want.len(), "{name}");
            assert!(
                out == want,
                "{name}: the frames after the junk were not normalised"
            );
            assert!(head_max <= bound, "{name}: head grew to {head_max} B");
        }
    }

    // ---- (8): hostile input

    fn header(frame_len: usize, sri: u8, crc: bool) -> Vec<u8> {
        let mut h = frame(sri, 2, 0, 0, false, crc);
        h[3] = (h[3] & !0x03) | ((frame_len >> 11) & 0x03) as u8;
        h[4] = ((frame_len >> 3) & 0xFF) as u8;
        h[5] = (h[5] & 0x1F) | ((frame_len & 0x07) << 5) as u8;
        h
    }

    /// (8) The no-panic table (§ 2.3), each row in 1-, 7- and 4 096-byte and seeded random
    /// chunks: no panic, output no longer than input, and every row that cannot align comes out
    /// unchanged. Fails on any unchecked index or subtraction reached by these shapes (a panic),
    /// on a `frame_length` below its header looping, and on a front end that eats a body it
    /// did not align.
    #[test]
    fn t_b3_8_hostile_input_never_panics_and_unaligned_bodies_pass_unchanged() {
        let mut falsesync = junk(64 * 1024, 5);
        falsesync[4096..4103].copy_from_slice(&[0xFF, 0xF1, 0x58, 0x40, 0x02, 0x1F, 0xFC]);
        let mp3: Vec<u8> = (0..400)
            .flat_map(|i| {
                let mut f = vec![0xFF, 0xFB, 0x90, 0xC4];
                f.extend(std::iter::repeat_n(i as u8 & 0x3F, 413));
                f
            })
            .collect();
        let reserved: Vec<u8> = (13..=15u8)
            .flat_map(|sri| frame(sri, 2, 50, 1, false, false))
            .collect();
        let fl = |len| {
            let mut v = header(len, 3, false);
            v.extend(junk(200, len as u64 + 1));
            v
        };
        let rows: Vec<(&str, Vec<u8>)> = vec![
            ("empty", vec![]),
            ("[FF]", vec![0xFF]),
            ("[FF F1]", vec![0xFF, 0xF1]),
            ("frame_length 0", fl(0)),
            ("frame_length 1", fl(1)),
            ("frame_length 6", fl(6)),
            ("frame_length 8191 then EOF", header(8191, 3, false)),
            ("a CRC header, 9 B, then EOF", header(9, 3, true)),
            ("all FF", vec![0xFF; 64 * 1024]),
            ("alternating FF F1", [0xFF, 0xF1].repeat(32 * 1024)),
            ("reserved sri 13–15", reserved),
            ("F-falsesync head", falsesync),
            ("2 MiB of 0x00–0x3F", junk(2 * 1024 * 1024, 7)),
            ("an MP3 body", mp3),
        ];
        let sizes: [(&str, MakeSizes); 4] = [
            ("1", || Box::new(fixed(1))),
            ("7", || Box::new(fixed(7))),
            ("4096", || Box::new(fixed(4096))),
            ("random", || Box::new(random_sizes(11, 9000))),
        ];
        for (row, input) in &rows {
            for (size, make) in &sizes {
                if input.len() > 256 * 1024 && *size != "4096" {
                    continue; // 2 MiB a byte at a time adds nothing but minutes
                }
                let sizes = make();
                let out = through(chunked(input, sizes));
                assert_eq!(out, *input, "{row}, chunks {size}: unaligned, so unchanged");
            }
        }

        // A seeded random 1 MiB: whatever it aligns on, no panic and never longer.
        let mut next = random_sizes(13, 256);
        let random: Vec<u8> = (0..1024 * 1024).map(|_| (next() - 1) as u8).collect();
        for (size, make) in &sizes {
            let sizes = make();
            let out = through(chunked(&random, sizes));
            assert!(out.len() <= random.len(), "random 1 MiB, chunks {size}");
        }

        // An inner `Interrupted` reaches the caller once, and nothing is lost behind it.
        let body = frames(5, 3, 2, true, false);
        let mut inner = chunked(&body, fixed(33));
        inner.interrupt_first = true;
        let mut r = AdtsReader::new(inner);
        let mut buf = [0u8; 4096];
        assert_eq!(
            r.read(&mut buf).map_err(|e| e.kind()),
            Err(io::ErrorKind::Interrupted),
            "an inner error propagates"
        );
        let mut out = Vec::new();
        r.read_to_end(&mut out).expect("read");
        assert_eq!(
            out,
            normalise_adts(&body).bytes,
            "the state survived the error"
        );

        // A zero-length read consumes nothing.
        let mut r = AdtsReader::new(chunked(&body, fixed(64)));
        assert_eq!(r.read(&mut []).expect("read"), 0);
        let mut out = Vec::new();
        r.read_to_end(&mut out).expect("read");
        assert_eq!(out, normalise_adts(&body).bytes);
    }

    // ---- (9): the source scan

    /// (9) No panic shape outside the tests: the scan `hls::tests` runs on `hls/`, plus
    /// indexing (`x[..]`, where `[` directly follows an identifier, `)` or `]`), since an index
    /// out of range is a panic too. Fails on any of them added above `#[cfg(test)]`.
    #[test]
    fn t_b3_9_the_front_end_has_no_panic_shape_outside_tests() {
        const SHAPES: [&str; 6] = [
            "unreachable!",
            "panic!",
            "todo!",
            "unimplemented!",
            ".unwrap()",
            ".expect(",
        ];
        let src = include_str!("adts.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        for (n, line) in code.lines().enumerate() {
            let line = line.split("//").next().unwrap_or(line);
            for shape in SHAPES {
                assert!(
                    !line.contains(shape),
                    "adts.rs:{}: `{shape}`: {}",
                    n + 1,
                    line.trim()
                );
            }
            let b = line.as_bytes();
            for i in 1..b.len() {
                let prev = b[i - 1];
                assert!(
                    !(b[i] == b'['
                        && (prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b')' | b']'))),
                    "adts.rs:{}: an index: {}",
                    n + 1,
                    line.trim()
                );
            }
        }
    }
}
