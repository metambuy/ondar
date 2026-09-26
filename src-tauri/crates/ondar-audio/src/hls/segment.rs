//! Segment normalisation — the pure per-segment half of HLS (plan §2.4).
//!
//! An HLS audio segment arrives as one or more ID3v2 tags (timed metadata) followed by ADTS
//! frames. Concatenated as sent, Step 0 (c) showed the decoder plays them without loss: its
//! sync scan skips the tags. Two things it cannot do, and this module does:
//!
//! 1. **Rewrite the MPEG-2 ID bit.** Symphonia's ADTS reader syncs only on `0xFFF1`; census
//!    station 02 sends `0xFFF9` (finding F1), and on `b7e050a` that is an IO error at
//!    `build()` after a scan to EOF — on a live mount, a scan that never ends (defect B). The
//!    ID bit changes nothing in the payload, so it is cleared.
//! 2. **Drop a CRC.** A frame with `protection_absent == 0` carries two CRC bytes after the
//!    header; the decoder does not read them. None of the census's five ADTS stations sends
//!    them; the walk handles them so a sixth can.
//!
//! The walk also drops a trailing partial frame and stops at a sync loss, both reported in
//! [`Normalised`] for the fetch loop to log. The **ID3 skip** is hygiene — bounded and
//! deterministic, not relying on a tag's content — rather than a necessity (gate amendment 1).
//! The **format guard** ends the source when a segment's sample-rate index or channel
//! configuration differs from the session's first, because the ring and defect A's converter
//! are built once per session for one format; a change fed into them would play at the wrong
//! speed. The **container sniff** reads the first bytes after the tags and names ADTS, MPEG-TS
//! or fMP4 so the fetch layer can refuse what the decoder cannot play, before downloading it.

use std::io::{self, Read};

/// ADTS header: 7 bytes without a CRC, 9 with one.
pub const ADTS_HEADER_LEN: usize = 7;
const ADTS_CRC_LEN: usize = 2;
/// MPEG-TS packets are 188 bytes and start with `0x47`.
pub const TS_PACKET_LEN: usize = 188;
/// The bytes the sniff needs: two TS packets, so a lone `0x47` is not mistaken for TS.
pub const SNIFF_LEN: usize = 2 * TS_PACKET_LEN;

/// The sampling-frequency index table (ISO 14496-3 Table 1.18). Index 13–15 are reserved.
const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

// ---------------------------------------------------------------------------------------------
// ID3

/// The offset of the first byte after every leading ID3v2 tag (zero when there is none). A
/// tag is `ID3`, two version bytes, one flags byte and a 28-bit syncsafe size, plus a 10-byte
/// footer when flag `0x10` is set. Tags are skipped in a loop: station 01 sends two per segment.
pub fn id3_end(bytes: &[u8]) -> usize {
    let mut off = 0;
    while bytes.len() >= off + 10 && &bytes[off..off + 3] == b"ID3" {
        let flags = bytes[off + 5];
        let size = ((bytes[off + 6] as usize & 0x7F) << 21)
            | ((bytes[off + 7] as usize & 0x7F) << 14)
            | ((bytes[off + 8] as usize & 0x7F) << 7)
            | (bytes[off + 9] as usize & 0x7F);
        let total = 10 + size + if flags & 0x10 != 0 { 10 } else { 0 };
        off += total;
    }
    off.min(bytes.len())
}

// ---------------------------------------------------------------------------------------------
// The container sniff

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Adts,
    MpegTs,
    Fmp4,
    Unknown,
}

/// Name the container from the bytes **after** the ID3 tags (the caller strips them with
/// [`id3_end`]; sniffing the tag itself reads "unknown"). ADTS is a 12-bit sync word with
/// layer 0; MPEG-TS is `0x47` at both 0 and 188 — one packet's worth is not enough, a stray
/// `0x47` at 0 is not TS; fMP4 is `ftyp` at offset 4.
pub fn sniff(bytes: &[u8]) -> Container {
    if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
        return Container::Fmp4;
    }
    if bytes.len() > TS_PACKET_LEN && bytes[0] == 0x47 && bytes[TS_PACKET_LEN] == 0x47 {
        return Container::MpegTs;
    }
    if bytes.len() >= 2 && is_adts_sync(bytes) {
        return Container::Adts;
    }
    Container::Unknown
}

/// `0xFFF` followed by layer bits `00`; the ID bit and `protection_absent` may be either.
fn is_adts_sync(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == 0xFF && (b[1] & 0xF6) == 0xF0
}

// ---------------------------------------------------------------------------------------------
// The ADTS walk

/// What the first ADTS header of a segment declares. `sri` indexes [`SAMPLE_RATES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsFormat {
    pub sri: u8,
    pub channel_config: u8,
}

impl AdtsFormat {
    /// The sample rate the index names, or `None` for a reserved index.
    pub fn sample_rate(&self) -> Option<u32> {
        SAMPLE_RATES.get(self.sri as usize).copied()
    }
}

impl std::fmt::Display for AdtsFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.sample_rate() {
            Some(hz) => write!(f, "{hz}/{}", self.channel_config),
            None => write!(f, "sri{}/{}", self.sri, self.channel_config),
        }
    }
}

/// One segment after the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalised {
    /// The frames, whole, in order, with the ID bit cleared and any CRC removed.
    pub bytes: Vec<u8>,
    pub frames: usize,
    /// The first frame's format; `None` when no frame synced.
    pub format: Option<AdtsFormat>,
    /// Frames whose `FFF9` header was rewritten to `FFF1`.
    pub rewritten: usize,
    /// Frames whose two CRC bytes were removed.
    pub crc_dropped: usize,
    /// Bytes of a trailing frame the segment cut short, dropped.
    pub partial_dropped: usize,
    /// The offset (in the walked bytes) at which sync was lost, and how many bytes followed.
    /// `None` when the walk reached the end cleanly.
    pub sync_lost: Option<(usize, usize)>,
}

/// Strip the leading ID3 tags, then walk the ADTS frames (see the module doc).
pub fn normalise(segment: &[u8]) -> Normalised {
    normalise_adts(&segment[id3_end(segment)..])
}

/// The ADTS walk on bytes that start at a frame header.
pub fn normalise_adts(b: &[u8]) -> Normalised {
    let mut out = Vec::with_capacity(b.len());
    let mut n = Normalised {
        bytes: Vec::new(),
        frames: 0,
        format: None,
        rewritten: 0,
        crc_dropped: 0,
        partial_dropped: 0,
        sync_lost: None,
    };
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        if rest.len() < ADTS_HEADER_LEN {
            // Not even a header: the tail of a frame the segment cut short.
            n.partial_dropped = rest.len();
            break;
        }
        if !is_adts_sync(rest) {
            n.sync_lost = Some((i, rest.len()));
            break;
        }
        let protection_absent = rest[1] & 0x01 == 1;
        let header_len = if protection_absent {
            ADTS_HEADER_LEN
        } else {
            ADTS_HEADER_LEN + ADTS_CRC_LEN
        };
        let frame_len = frame_length(rest);
        if frame_len < header_len {
            n.sync_lost = Some((i, rest.len()));
            break;
        }
        if frame_len > rest.len() {
            n.partial_dropped = rest.len();
            break;
        }
        let frame = &rest[..frame_len];
        if n.format.is_none() {
            n.format = Some(AdtsFormat {
                sri: (frame[2] >> 2) & 0x0F,
                channel_config: ((frame[2] & 0x01) << 2) | (frame[3] >> 6),
            });
        }

        let mut header = [0u8; ADTS_HEADER_LEN];
        header.copy_from_slice(&frame[..ADTS_HEADER_LEN]);
        if header[1] & 0x08 != 0 {
            header[1] &= !0x08; // the MPEG-2 ID bit → MPEG-4 (F1)
            n.rewritten += 1;
        }
        if !protection_absent {
            header[1] |= 0x01; // protection_absent = 1: no CRC follows
            set_frame_length(&mut header, frame_len - ADTS_CRC_LEN);
            n.crc_dropped += 1;
        }
        out.extend_from_slice(&header);
        out.extend_from_slice(&frame[header_len..]);
        n.frames += 1;
        i += frame_len;
    }
    n.bytes = out;
    n
}

/// `frame_length`, 13 bits across bytes 3–5: the whole frame including the header.
fn frame_length(h: &[u8]) -> usize {
    ((h[3] as usize & 0x03) << 11) | ((h[4] as usize) << 3) | ((h[5] as usize) >> 5)
}

fn set_frame_length(h: &mut [u8; ADTS_HEADER_LEN], len: usize) {
    h[3] = (h[3] & !0x03) | ((len >> 11) & 0x03) as u8;
    h[4] = ((len >> 3) & 0xFF) as u8;
    h[5] = (h[5] & 0x1F) | ((len & 0x07) << 5) as u8;
}

// ---------------------------------------------------------------------------------------------
// The format guard

/// A segment's format differs from the session's first. The source ends and the session
/// reopens with a decoder, ring and converter built for the new format (plan §2.4 step 3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("FormatChanged {from} → {to}")]
pub struct FormatChanged {
    pub from: AdtsFormat,
    pub to: AdtsFormat,
}

#[derive(Debug, Clone, Default)]
pub struct FormatGuard {
    first: Option<AdtsFormat>,
}

impl FormatGuard {
    /// Accept the first format seen; refuse any later one that differs.
    pub fn check(&mut self, format: AdtsFormat) -> Result<(), FormatChanged> {
        match self.first {
            None => {
                self.first = Some(format);
                Ok(())
            }
            Some(first) if first == format => Ok(()),
            Some(first) => Err(FormatChanged {
                from: first,
                to: format,
            }),
        }
    }

    pub fn first(&self) -> Option<AdtsFormat> {
        self.first
    }
}

// ---------------------------------------------------------------------------------------------
// gzip

/// Inflate a gzip body, refusing one that inflates past `max` bytes. Decided by the response's
/// `Content-Encoding: gzip`, never requested: station 10 gzips its playlists unasked (finding
/// F6), and asking would change the request for every Icecast station too.
///
/// `max` is the caller's cap on the body — the same cap its compressed bytes already met. The
/// inflate reads at most `max + 1` bytes, so a body of zeros (~1000:1, measured) cannot become
/// a multi-gigabyte `Vec` (review 2, 2026-09-25, finding 1: on `fe120a2` there was no bound).
pub fn gunzip(bytes: &[u8], max: usize) -> Result<Vec<u8>, GunzipError> {
    let mut out = Vec::with_capacity(bytes.len().saturating_mul(4).min(max));
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    flate2::read::GzDecoder::new(bytes)
        .take(limit)
        .read_to_end(&mut out)
        .map_err(GunzipError::Invalid)?;
    if out.len() > max {
        return Err(GunzipError::TooLarge(max));
    }
    Ok(out)
}

/// Why a body did not inflate.
#[derive(Debug)]
pub enum GunzipError {
    /// It inflates past the caller's cap (the cap is carried).
    TooLarge(usize),
    /// It is not gzip, or is truncated.
    Invalid(io::Error),
}

impl std::fmt::Display for GunzipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GunzipError::TooLarge(max) => write!(f, "inflates past {max} bytes"),
            GunzipError::Invalid(e) => write!(f, "not gzip: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! T7–T11 of the M3c plan, on the fixture heads (ID3 tag(s) + 16 ADTS frames; 4 TS
    //! packets). The module does not exist on `b7e050a`, so T7–T10 are mutation-checked; T11's
    //! `b7e050a` record is Step 0 (c)'s: the raw `FFF9` head does not build (an IO error from
    //! the sync scan), and that case is a test here so the mechanism stays pinned.

    use super::*;
    use rodio::Source;
    use rodio::decoder::DecoderBuilder;
    use std::io::Cursor;

    macro_rules! head {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/hls/", $name)) as &[u8]
        };
    }

    const ADTS_HEADS: [(&str, &[u8]); 5] = [
        ("01", head!("01-seg-head.aac")),
        ("02", head!("02-seg-head.aac")),
        ("07", head!("07-seg-head.aac")),
        ("08", head!("08-seg-head.aac")),
        ("10", head!("10-seg-head.aac")),
    ];
    const TS_HEADS: [(&str, &[u8]); 4] = [
        ("03", head!("03-seg-head.mpegts")),
        ("04", head!("04-seg-head.mpegts")),
        ("06", head!("06-seg-head.mpegts")),
        ("09", head!("09-seg-head.mpegts")),
    ];

    /// A synthetic ADTS frame: header + `payload` bytes of `fill`. `id` sets the MPEG-2 bit,
    /// `crc` adds two CRC bytes and clears `protection_absent`.
    fn frame(sri: u8, ch: u8, payload: usize, fill: u8, id: bool, crc: bool) -> Vec<u8> {
        let header_len = if crc { 9 } else { 7 };
        let len = header_len + payload;
        let mut h = vec![0u8; header_len];
        h[0] = 0xFF;
        h[1] = 0xF0 | if id { 0x08 } else { 0 } | if crc { 0 } else { 1 };
        h[2] = (1 << 6) | (sri << 2) | (ch >> 2); // profile LC
        h[3] = ((ch & 0x03) << 6) | ((len >> 11) & 0x03) as u8;
        h[4] = ((len >> 3) & 0xFF) as u8;
        h[5] = ((len & 0x07) << 5) as u8 | 0x1F;
        h[6] = 0xFC;
        if crc {
            h[7] = 0xAB;
            h[8] = 0xCD;
        }
        h.extend(std::iter::repeat_n(fill, payload));
        h
    }

    // ---- T7: the sniff

    #[test]
    fn t7_sniff_names_adts_ts_fmp4_and_unknown_after_the_id3_tags() {
        // Mutation "sniff before the ID3 skip" → every ADTS head reads Unknown (`ID3` is not a
        // sync word).
        for (nn, b) in ADTS_HEADS {
            assert_eq!(sniff(&b[id3_end(b)..]), Container::Adts, "station {nn}");
            assert_eq!(
                sniff(b),
                Container::Unknown,
                "station {nn} sniffed on its ID3 tag"
            );
        }
        for (nn, b) in TS_HEADS {
            assert_eq!(id3_end(b), 0, "station {nn}: TS has no leading ID3");
            assert_eq!(sniff(b), Container::MpegTs, "station {nn}");
        }
        let mut fmp4 = vec![0, 0, 0, 0x18];
        fmp4.extend_from_slice(b"ftypiso5");
        fmp4.extend_from_slice(&[0; 16]);
        assert_eq!(sniff(&fmp4), Container::Fmp4);
        assert_eq!(sniff(b"<html><body>404"), Container::Unknown);
        assert_eq!(sniff(&[]), Container::Unknown);
        assert_eq!(sniff(&[0xFF]), Container::Unknown);
        // `0x47` at 0 only — one packet, or a second packet that does not sync — is not TS.
        let mut one_packet = vec![0u8; TS_PACKET_LEN];
        one_packet[0] = 0x47;
        assert_eq!(sniff(&one_packet), Container::Unknown);
        let mut two = vec![0u8; 2 * TS_PACKET_LEN];
        two[0] = 0x47;
        assert_eq!(sniff(&two), Container::Unknown);
        two[TS_PACKET_LEN] = 0x47;
        assert_eq!(sniff(&two), Container::MpegTs);
        // An `FFF9` head is ADTS too: the sync ignores the ID bit.
        assert_eq!(sniff(&[0xFF, 0xF9, 0x50, 0x40]), Container::Adts);
        // Layer bits set (`FFFB` is an MP3 frame) → not ADTS.
        assert_eq!(sniff(&[0xFF, 0xFB, 0x90, 0x00]), Container::Unknown);
    }

    // ---- T8: the ID3 skip

    #[test]
    fn t8_every_leading_id3_tag_is_skipped_including_a_footer() {
        // 01 has two tags (73 + 80 B) → the first sync word at 153. Mutation "skip one tag"
        // → 73, and the sniff there reads `ID3`.
        let (_, b01) = ADTS_HEADS[0];
        assert_eq!(id3_end(b01), 153);
        assert_eq!(&b01[153..155], &[0xFF, 0xF1]);
        assert_eq!(
            &b01[73..76],
            b"ID3",
            "the second tag starts where the first ends"
        );
        // 07 and 08: one 1 122 B tag. 10: 73 B. 02: 73 + 686 = 759, then `FFF9`.
        assert_eq!(id3_end(ADTS_HEADS[2].1), 1_122);
        assert_eq!(id3_end(ADTS_HEADS[3].1), 1_122);
        assert_eq!(id3_end(ADTS_HEADS[4].1), 73);
        assert_eq!(id3_end(ADTS_HEADS[1].1), 759);
        assert_eq!(&ADTS_HEADS[1].1[759..761], &[0xFF, 0xF9]);

        // A synthetic tag with the footer flag: 10 + size + 10. Mutation "ignore the footer
        // flag" → 30, landing inside the footer.
        let mut tag = b"ID3\x04\x00\x10".to_vec(); // v2.4, flags 0x10 = footer present
        tag.extend_from_slice(&[0, 0, 0, 20]); // syncsafe size 20
        tag.extend_from_slice(&[0xAA; 20]);
        tag.extend_from_slice(b"3DI\x04\x00\x10\x00\x00\x00\x14"); // the footer
        tag.extend_from_slice(&[0xFF, 0xF1, 0x50]);
        assert_eq!(id3_end(&tag), 40);
        // No tag → 0; a truncated tag header → 0 (nothing to skip safely); a tag whose size
        // overruns the buffer clamps to the end.
        assert_eq!(id3_end(&[0xFF, 0xF1]), 0);
        assert_eq!(id3_end(b"ID3\x04"), 0);
        assert_eq!(id3_end(b"ID3\x04\x00\x00\x00\x00\x7F\x7F"), 10);
    }

    // ---- T9: the ADTS walk

    #[test]
    fn t9_the_walk_rewrites_the_id_bit_and_keeps_every_payload_byte() {
        // 02: all sixteen headers are `FFF9` and come out `FFF1`; nothing else changes.
        // Mutation "skip the rewrite" → rewritten 0 and the first two bytes still `FFF9`.
        let (_, b02) = ADTS_HEADS[1];
        let n = normalise(b02);
        assert_eq!(n.frames, 16);
        assert_eq!(n.rewritten, 16);
        assert_eq!(n.crc_dropped, 0);
        assert_eq!(n.partial_dropped, 0);
        assert_eq!(n.sync_lost, None);
        assert_eq!(
            n.format,
            Some(AdtsFormat {
                sri: 6,
                channel_config: 1
            })
        );
        assert_eq!(n.format.unwrap().sample_rate(), Some(24_000));
        let raw = &b02[759..];
        assert_eq!(
            n.bytes.len(),
            raw.len(),
            "the same bytes, one bit each header"
        );
        // Every frame header's ID bit cleared, every other byte identical.
        let mut i = 0;
        let mut headers = 0;
        while i < raw.len() {
            assert_eq!(n.bytes[i], 0xFF);
            assert_eq!(n.bytes[i + 1], raw[i + 1] & !0x08);
            assert_eq!(
                &n.bytes[i + 2..i + frame_length(&raw[i..])],
                &raw[i + 2..i + frame_length(&raw[i..])]
            );
            i += frame_length(&raw[i..]);
            headers += 1;
        }
        assert_eq!(headers, 16);

        // 01, 07, 08, 10: `FFF1` already; sixteen frames each, bytes unchanged.
        for (nn, b) in [ADTS_HEADS[0], ADTS_HEADS[2], ADTS_HEADS[3], ADTS_HEADS[4]] {
            let n = normalise(b);
            assert_eq!(n.frames, 16, "station {nn}");
            assert_eq!(n.rewritten, 0, "station {nn}");
            assert_eq!(n.bytes, &b[id3_end(b)..], "station {nn}");
            assert_eq!((n.partial_dropped, n.sync_lost), (0, None), "station {nn}");
        }
        // The census's formats: 48 000/2 for 01, 07, 10; the HE-AAC core 22 050/2 for 08.
        assert_eq!(
            normalise(ADTS_HEADS[0].1).format.unwrap().to_string(),
            "48000/2"
        );
        assert_eq!(
            normalise(ADTS_HEADS[3].1).format.unwrap().to_string(),
            "22050/2"
        );
    }

    #[test]
    fn t9_a_crc_frame_loses_its_two_bytes_and_a_partial_tail_is_dropped() {
        // A CRC frame: 9-byte header, `protection_absent` 0. Out: 7-byte header, the bit set,
        // `frame_length` two less, the payload intact. Mutation "leave the CRC" → the decoder
        // would read `AB CD` as the first payload bytes.
        let f = frame(3, 2, 100, 0x5A, false, true);
        assert_eq!(f.len(), 109);
        let n = normalise_adts(&f);
        assert_eq!(n.frames, 1);
        assert_eq!(n.crc_dropped, 1);
        assert_eq!(n.bytes.len(), 107);
        assert_eq!(n.bytes[1] & 0x01, 1, "protection_absent set");
        assert_eq!(frame_length(&n.bytes), 107);
        assert_eq!(&n.bytes[7..], &[0x5A; 100][..]);
        assert_eq!(
            n.bytes[2], f[2],
            "profile, sri and the channel bit untouched"
        );

        // An ID-bit frame with a CRC: both fixed.
        let f = frame(7, 2, 50, 0x11, true, true);
        let n = normalise_adts(&f);
        assert_eq!((n.rewritten, n.crc_dropped), (1, 1));
        assert_eq!(&n.bytes[..2], &[0xFF, 0xF1]);

        // Three frames, the third cut short by 10 bytes: two come out, the tail is dropped and
        // counted. Mutation "emit the partial frame" → 3 frames and a decoder that reads into
        // the next segment's ID3 tag.
        let mut three = frame(3, 2, 200, 1, false, false);
        three.extend(frame(3, 2, 200, 2, false, false));
        let last = frame(3, 2, 200, 3, false, false);
        three.extend(&last[..last.len() - 10]);
        let n = normalise_adts(&three);
        assert_eq!(n.frames, 2);
        assert_eq!(n.partial_dropped, 207 - 10);
        assert_eq!(n.bytes.len(), 2 * 207);
        assert_eq!(n.sync_lost, None);
        // A tail shorter than a header counts as partial too.
        let mut short = frame(3, 2, 20, 1, false, false);
        short.extend_from_slice(&[0xFF, 0xF1, 0x50]);
        let n = normalise_adts(&short);
        assert_eq!((n.frames, n.partial_dropped), (1, 3));

        // Garbage after a good frame: sync lost at its offset, the rest not emitted.
        let mut junk = frame(3, 2, 20, 1, false, false);
        junk.extend_from_slice(b"not adts at all, twenty bytes");
        let n = normalise_adts(&junk);
        assert_eq!(n.frames, 1);
        assert_eq!(n.sync_lost, Some((27, 29)));
        assert_eq!(n.bytes.len(), 27);
        // A header claiming a frame shorter than itself is a sync loss, not a zero-length loop.
        let mut zero = frame(3, 2, 0, 0, false, false);
        set_frame_length(&mut <[u8; 7]>::try_from(&zero[..7]).unwrap(), 0);
        zero[3] &= !0x03;
        zero[4] = 0;
        zero[5] &= 0x1F;
        let n = normalise_adts(&zero);
        assert_eq!((n.frames, n.sync_lost), (0, Some((0, 7))));
    }

    // ---- T10: the format guard

    #[test]
    fn t10_a_rate_or_channel_change_ends_the_source() {
        // Mutation "guard off" (always Ok) → a 22 050 segment would be fed into a ring built
        // for 48 000 and play at 2.18×: defect A's symptom by another route.
        let mut g = FormatGuard::default();
        let a = AdtsFormat {
            sri: 3,
            channel_config: 2,
        };
        let b = AdtsFormat {
            sri: 7,
            channel_config: 2,
        };
        let c = AdtsFormat {
            sri: 3,
            channel_config: 1,
        };
        assert_eq!(g.first(), None);
        assert_eq!(g.check(a), Ok(()));
        assert_eq!(g.check(a), Ok(()));
        assert_eq!(g.first(), Some(a));
        let e = g.check(b).unwrap_err();
        assert_eq!(e, FormatChanged { from: a, to: b });
        assert_eq!(e.to_string(), "FormatChanged 48000/2 → 22050/2");
        assert_eq!(
            g.check(c).unwrap_err().to_string(),
            "FormatChanged 48000/2 → 48000/1"
        );
        // The guard keeps the first format after a refusal: the reopen builds a new guard.
        assert_eq!(g.first(), Some(a));
        assert_eq!(
            AdtsFormat {
                sri: 13,
                channel_config: 2
            }
            .to_string(),
            "sri13/2"
        );
    }

    // ---- T11: the decoder, built as run_session builds it

    fn build(bytes: Vec<u8>) -> Result<(u32, u16), String> {
        let d = DecoderBuilder::new()
            .with_data(Cursor::new(bytes))
            .with_seekable(false)
            .with_gapless(false)
            .with_mime_type("audio/aac")
            .build()
            .map_err(|e| e.to_string())?;
        Ok((d.sample_rate().get(), d.channels().get()))
    }

    #[test]
    fn t11_normalised_heads_build_a_decoder_at_the_core_rate() {
        // Step 0 (b): Symphonia reports the ADTS core rate — 22 050 for 08's HE-AAC, not the
        // 44 100 ffprobe synthesises. If a Symphonia bump starts decoding SBR this reads
        // 44 100 and the message says so: it is F2 changing, not a bug.
        for (nn, want) in [
            ("01", (48_000, 2)),
            ("07", (48_000, 2)),
            ("10", (48_000, 2)),
        ] {
            let b = ADTS_HEADS.iter().find(|(n, _)| *n == nn).unwrap().1;
            assert_eq!(build(normalise(b).bytes), Ok(want), "station {nn}");
        }
        let b08 = ADTS_HEADS[3].1;
        assert_eq!(
            build(normalise(b08).bytes),
            Ok((22_050, 2)),
            "08 (HE-AAC): the core rate; 44 100 would mean Symphonia now decodes SBR (F2)"
        );
        // 02 after the rewrite: 24 000 / 1 (Step 0 (c), predicted and measured).
        let b02 = ADTS_HEADS[1].1;
        assert_eq!(build(normalise(b02).bytes), Ok((24_000, 1)));
        // …and with the tags left in, the decoder's own scan still finds the frames (Step 0
        // (c): the strip is hygiene).
        assert_eq!(build(ADTS_HEADS[0].1.to_vec()), Ok((48_000, 2)));
    }

    #[test]
    fn t11_the_raw_fff9_head_does_not_build_without_the_rewrite() {
        // F1: without the rewrite the ADTS reader's sync scan finds no `FFF1` and `build()`
        // fails. **The error's shape depends on how many bytes it scans before EOF** (measured
        // here on 2026-09-24 with this builder, Symphonia 0.5.5): the 4 849 B head, and the
        // first 4 096 / 8 192 B of Step 0's four-segment concatenation, give
        // `UnrecognizedFormat`; from 16 384 B up to the whole 239 629 B it is
        // `IoError("end of stream")` — Step 0 (c)'s reading. In `run_session` the first is the
        // terminal `unsupported_format` arm and the second the retried `Decode` arm, so on
        // `b7e050a` a short `FFF9` file ends at once and a long one runs the backoff; a live
        // mount, whose bytes never end, does neither — the scan continues (defect B). What is
        // pinned here is that it does not build, and that the rewrite is what makes it build
        // (the test above); the shape is Symphonia's, recorded for the report, not asserted.
        // Mutation "skip the rewrite" fails the test above at 02 with the same error.
        let b02 = ADTS_HEADS[1].1;
        let stripped = &b02[id3_end(b02)..];
        assert_eq!(&stripped[..2], &[0xFF, 0xF9]);
        let err = build(stripped.to_vec()).unwrap_err();
        assert!(
            err.contains("not been recognized") || err.contains("IO error"),
            "an unexpected error shape — record it: {err}"
        );
        assert!(
            build(b02.to_vec()).is_err(),
            "raw with its ID3 tags: still no build"
        );
    }

    #[test]
    fn gunzip_inflates_the_gzip_fixture_and_refuses_plain_text() {
        let gz = head!("10-media.m3u8.gz");
        let text = String::from_utf8(gunzip(gz, 1024 * 1024).unwrap()).unwrap();
        assert!(text.starts_with("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:5\n"));
        assert!(matches!(
            gunzip(b"#EXTM3U\n", 1024),
            Err(GunzipError::Invalid(_))
        ));
    }

    /// Review 2 (2026-09-25), finding 1: the inflated body is bounded by the caller's cap, as
    /// the compressed one already was. 64 KiB of zeros gzips to about a hundred bytes; with a
    /// cap one byte short it is refused, at the cap it inflates whole. On `fe120a2` there was no
    /// cap: the same ~1000:1 ratio made a 4 MB compressed segment a ~4 GB `Vec`.
    #[test]
    fn gunzip_refuses_a_body_that_inflates_past_its_cap() {
        use std::io::Write;
        let zeros = vec![0u8; 64 * 1024];
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(&zeros).unwrap();
        let gz = e.finish().unwrap();
        assert!(gz.len() < 1024, "the fixture is a bomb: {} bytes", gz.len());
        assert_eq!(gunzip(&gz, zeros.len()).unwrap().len(), zeros.len());
        assert!(matches!(
            gunzip(&gz, zeros.len() - 1),
            Err(GunzipError::TooLarge(_))
        ));
        assert!(matches!(
            gunzip(&gz, 16 * 1024),
            Err(GunzipError::TooLarge(_))
        ));
    }
}
