//! ICY (Shoutcast/Icecast) in-band metadata.
//!
//! When the client sends `Icy-MetaData: 1`, the server answers with `icy-metaint: N` and
//! inserts a metadata block after every `N` bytes of audio:
//!
//! ```text
//! [N audio bytes][1 length byte L][L*16 bytes of metadata, NUL padded] [N audio bytes] ...
//! ```
//!
//! The metadata is `key='value';` pairs; the one we care about is `StreamTitle`. This reader
//! strips the blocks so the decoder sees a clean audio byte stream, and reports titles through
//! a callback.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Largest read length the decoder has asked us for, process-wide. Diagnostic only.
///
/// `PREFETCH_BYTES` must cover at least one of these or the decode thread starves on its first
/// refill — see `stream::PREFETCH_BYTES`. Crucially this size is **not Ondar's to choose**:
/// `ondar-audio` never constructs a `MediaSourceStream`; rodio 0.22.2 does it internally over
/// symphonia-core 0.5.5, and picks the size. Observed at 32768 B on 2026-09-11.
///
/// A rodio or symphonia bump that raises it would reintroduce spontaneous underruns roughly
/// 2 s into every station start, on a perfectly healthy network, with no error raised and
/// nothing in CI to catch it. `examples/stall_bench.rs` checks this against the effective
/// prefetch and fails loudly, which is the cheapest tripwire available and sits where the
/// evidence was originally found.
pub static MAX_OBSERVED_READ: AtomicUsize = AtomicUsize::new(0);

pub type TitleCallback = Box<dyn FnMut(String) + Send + Sync>;

pub struct IcyReader<R: Read> {
    inner: R,
    /// `0` means the server sent no `icy-metaint`: the reader is a transparent pass-through.
    metaint: usize,
    /// Audio bytes left before the next metadata block.
    until_meta: usize,
    last_title: Option<String>,
    on_title: TitleCallback,
}

impl<R: Read> IcyReader<R> {
    pub fn new(inner: R, metaint: Option<usize>, on_title: TitleCallback) -> Self {
        let metaint = metaint.unwrap_or(0);
        Self {
            inner,
            metaint,
            until_meta: metaint,
            last_title: None,
            on_title,
        }
    }

    fn read_metadata_block(&mut self) -> io::Result<()> {
        let mut len = [0u8; 1];
        self.inner.read_exact(&mut len)?;
        let len = len[0] as usize * 16;
        if len > 0 {
            let mut block = vec![0u8; len];
            self.inner.read_exact(&mut block)?;
            if let Some(title) = parse_stream_title(&block)
                && self.last_title.as_deref() != Some(title.as_str())
            {
                self.last_title = Some(title.clone());
                (self.on_title)(title);
            }
        }
        self.until_meta = self.metaint;
        Ok(())
    }
}

impl<R: Read> Read for IcyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        MAX_OBSERVED_READ.fetch_max(buf.len(), Ordering::Relaxed);
        if buf.is_empty() {
            return Ok(0);
        }
        if self.metaint == 0 {
            return self.inner.read(buf);
        }
        if self.until_meta == 0 {
            self.read_metadata_block()?;
        }
        let want = buf.len().min(self.until_meta);
        let n = self.inner.read(&mut buf[..want])?;
        self.until_meta -= n;
        Ok(n)
    }
}

/// The decoder is built with `with_seekable(false)`, so Symphonia never calls this. It exists
/// only to satisfy rodio's `Read + Seek` bound. Rewinding through a live stream with metadata
/// interleaved is not meaningful, so anything but a position query is refused.
impl<R: Read> Seek for IcyReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match pos {
            SeekFrom::Current(0) => Ok(0),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "seeking is not supported on a live ICY stream",
            )),
        }
    }
}

/// Extract `StreamTitle='...';` from a raw metadata block. Titles may legitimately contain
/// `'` (e.g. "Don't Stop"), so the terminator we look for is `';` followed by either end of
/// data or the next `key=`, rather than the first quote.
pub fn parse_stream_title(block: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(block);
    let text = text.trim_end_matches('\0');
    let start = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[start..];
    let end = rest.find("';").unwrap_or(rest.len());
    let title = rest[..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    fn meta_block(text: &str) -> Vec<u8> {
        let mut body = text.as_bytes().to_vec();
        let padded = body.len().div_ceil(16) * 16;
        body.resize(padded, 0);
        let mut out = vec![(padded / 16) as u8];
        out.extend(body);
        out
    }

    #[test]
    fn parses_title() {
        assert_eq!(
            parse_stream_title(b"StreamTitle='Artist - Song';StreamUrl='';\0\0"),
            Some("Artist - Song".into())
        );
        assert_eq!(parse_stream_title(b"StreamTitle='';"), None);
        assert_eq!(
            parse_stream_title(b"StreamTitle='Don't Stop Me Now';"),
            Some("Don't Stop Me Now".into())
        );
    }

    #[test]
    fn strips_metadata_and_reports_titles() {
        let metaint = 8;
        let mut stream = Vec::new();
        stream.extend(b"AAAAAAAA");
        stream.extend(meta_block("StreamTitle='One';"));
        stream.extend(b"BBBBBBBB");
        stream.extend([0u8]); // empty metadata block
        stream.extend(b"CCCCCCCC");
        stream.extend(meta_block("StreamTitle='One';")); // duplicate: must not re-fire
        stream.extend(b"DDDD");

        let titles = Arc::new(Mutex::new(Vec::new()));
        let t = titles.clone();
        let mut reader = IcyReader::new(
            Cursor::new(stream),
            Some(metaint),
            Box::new(move |s| t.lock().unwrap().push(s)),
        );

        // Read in awkward chunk sizes to exercise the boundary logic.
        let mut audio: Vec<u8> = Vec::new();
        let mut buf = [0u8; 5];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            audio.extend(&buf[..n]);
        }
        assert_eq!(audio, b"AAAAAAAABBBBBBBBCCCCCCCCDDDD");
        assert_eq!(*titles.lock().unwrap(), vec!["One".to_string()]);
    }

    #[test]
    fn passthrough_without_metaint() {
        let mut reader = IcyReader::new(Cursor::new(b"hello".to_vec()), None, Box::new(|_| {}));
        let mut out = String::new();
        reader.read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello");
    }
}
