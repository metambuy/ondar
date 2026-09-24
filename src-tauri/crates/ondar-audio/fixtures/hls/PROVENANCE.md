# HLS fixture provenance

Every file here is a slice of a response recorded by the M3 Step 0 census on **2026-09-21**
(`_handover/m3-step0-logs/p4-*`, `User-Agent: Ondar/0.1.0`; the sample, `p4-sample.txt`, is the
top-voted `hls == 1 && lastcheckok == 1` stations of eight countries with one station per stream
host, numbered 01–10 there and here). The playlists are byte-for-byte copies. **No segment is
committed whole**: an audio fixture is the leading ID3v2 tag(s) plus the first **16 ADTS frames**
(0.34–0.74 s), a TS fixture the first **4 packets** (752 B). That is M3c decision D6 as amended by
the plan review's R3 (2026-09-24): a head is enough for every test that reads bytes (the sniff,
the ID3 walk, the ADTS walk, the decoder's format read), and the engine tests play segments the
test server synthesises by repeating a head's frames under successive sequence numbers, so no
test needs a second of broadcast audio. The full segments stay under the gitignored `_handover/`
as Step 0's record.

Cut by `_handover/m3c-step0/fixture-heads.py <census-dir> <out-dir>` (stdlib; it verifies every
ADTS head with a frame walk and every TS head's sync bytes, and prints the table below). The
script lives beside its gitignored inputs rather than in `scripts/`, because nobody without the
census captures can run it.

| nn | station (radio-browser `stationuuid`) | cc | shape (census P4) |
|---|---|---|---|
| 01 | VOA Chinese Radio (`819e0ecb-…`) | US | master → media, ADTS-AAC 48 000 / 2, **two** ID3 tags per segment, TD 10, 10-segment window |
| 02 | iHeart Radio Café (`52e142ff-…`) | US | master (one redirect) → media with `EXT-X-DISCONTINUITY` and quoted commas in `EXTINF` titles; ADTS with the **MPEG-2 ID bit (`FFF9`)**, 24 000 / 1, TD 10, 3-segment window |
| 03 | Fox News (`3da2910e-…`) | US | master, two `avc1.42c020` **video-only** variants, MPEG-TS |
| 04 | DW TV (`8b792693-…`) | DE | master, five muxed `avc1…,mp4a.40.2` variants + an `EXT-X-MEDIA` `SUBTITLES` rendition, MPEG-TS |
| 06 | France Inter (`33960c43-…`) | FR | the station URL **is** a media playlist; MPEG-TS (AAC + timed ID3), TD 4 |
| 07 | ONDA CERO (`b8601593-…`) | ES | master (one redirect) → media, ADTS 48 000 / 2, one 1 122 B ID3 tag, TD 10 |
| 08 | Rádio Bandeirantes São Paulo (`ad039169-…`) | BR | master (one redirect) → media, **HE-AAC** `mp4a.40.5`, ADTS core 22 050 / 2, TD 10 |
| 09 | Известия ТВ, sound (`1d13fc30-…`) | RU | the station URL **is** a media playlist; MPEG-TS, audio only, TD 7 |
| 10 | Antena 1 (`dbc018ae-…`) | PT | master with a **relative** variant URI (`chunklist.m3u8`) → media served **gzip** (`content-encoding: gzip`, unrequested), ADTS 48 000 / 2, TD 5, 20-segment window |

05 (DW English HD) is the same shape as 04 and is not mirrored.

| file | bytes | sha256[:12] | what |
|---|---|---|---|
| `01-master.m3u8` | 209 | `45078e91574b` | master playlist — byte copy of `p4-01-819e0ecb-playlist.m3u8` |
| `02-master.m3u8` | 133 | `095ee9cfa1fe` | master playlist — byte copy of `p4-02-52e142ff-playlist.m3u8` |
| `03-master.m3u8` | 445 | `216242f26536` | master playlist — byte copy of `p4-03-3da2910e-playlist.m3u8` |
| `04-master.m3u8` | 1176 | `15e275014016` | master playlist — byte copy of `p4-04-8b792693-playlist.m3u8` |
| `07-master.m3u8` | 184 | `70b2753f71ec` | master playlist — byte copy of `p4-07-b8601593-playlist.m3u8` |
| `08-master.m3u8` | 198 | `249d3bb74945` | master playlist — byte copy of `p4-08-ad039169-playlist.m3u8` |
| `10-master.m3u8` | 94 | `0e5bb22db227` | master playlist — byte copy of `p4-10-dbc018ae-playlist.m3u8` |
| `01-media.m3u8` | 871 | `c606ae8c8f72` | media playlist — byte copy of `p4-01-819e0ecb-media.m3u8` |
| `01-media-refresh.m3u8` | 871 | `32be17f1e30e` | media playlist, refreshed after 20 s — byte copy of `p4-01-819e0ecb-media-refresh.m3u8` |
| `02-media.m3u8` | 1535 | `44f613585cac` | media playlist — byte copy of `p4-02-52e142ff-media.m3u8` |
| `06-media.m3u8` | 1205 | `adaeb25fcd57` | media playlist (the station URL is one) — byte copy of `p4-06-33960c43-playlist.m3u8` |
| `07-media.m3u8` | 187 | `e6a36a9938c5` | media playlist — byte copy of `p4-07-b8601593-media.m3u8` |
| `08-media.m3u8` | 193 | `d1d6bee8d137` | media playlist — byte copy of `p4-08-ad039169-media.m3u8` |
| `09-media.m3u8` | 497 | `b669420567d1` | media playlist (the station URL is one) — byte copy of `p4-09-1d13fc30-playlist.m3u8` |
| `10-media.m3u8.gz` | 186 | `053252b1dddc` | media playlist **as served**: `content-encoding: gzip` — byte copy of `p4-10-dbc018ae-media.m3u8` |
| `10-media-refresh.m3u8.gz` | 187 | `15c0b8ddc045` | media playlist refreshed after 10 s, as served (gzip) — byte copy of `p4-10-dbc018ae-media-refresh.m3u8` |
| `01-seg-head.aac` | 5615 | `aac01e751b2b` | ID3 tags 73 + 80 B + the first 16 ADTS frames (5 462 B, first header `FFF1`) of `p4-01-819e0ecb-seg.bin` (160 239 B, 469 frames) |
| `02-seg-head.aac` | 4849 | `9e388ba6818f` | ID3 tags 73 + 686 B + the first 16 ADTS frames (4 090 B, first header **`FFF9`**, kept as sent) of `p4-02-52e142ff-seg.bin` (60 666 B, 234 frames) |
| `07-seg-head.aac` | 6506 | `71ecbcb2b642` | ID3 tag 1 122 B + the first 16 ADTS frames (5 384 B, `FFF1`) of `p4-07-b8601593-seg.bin` (160 794 B, 468 frames) |
| `08-seg-head.aac` | 7403 | `f7631091a240` | ID3 tag 1 122 B + the first 16 ADTS frames (6 281 B, `FFF1`, `sri 7`) of `p4-08-ad039169-seg.bin` (82 450 B, 215 frames) |
| `10-seg-head.aac` | 7148 | `fa50c58b32f2` | ID3 tag 73 B + the first 16 ADTS frames (7 075 B, `FFF1`) of `p4-10-dbc018ae-seg.bin` (89 797 B, 186 frames) |
| `03-seg-head.mpegts` | 752 | `1b2f23450e8a` | the first 4 TS packets of `p4-03-3da2910e-seg.bin` (413 976 B) |
| `04-seg-head.mpegts` | 752 | `5b35f96dd40e` | the first 4 TS packets of `p4-04-8b792693-seg.bin` (657 060 B) |
| `06-seg-head.mpegts` | 752 | `3fcc4a32b014` | the first 4 TS packets of `p4-06-33960c43-seg.bin` (129 908 B) |
| `09-seg-head.mpegts` | 752 | `c3965fb0c5ce` | the first 4 TS packets of `p4-09-1d13fc30-seg.bin` (114 304 B) |

25 files, 42 700 B. **Two things about the bytes:** the TS heads are named `.mpegts`, not the
`.ts` the servers use, because `eslint .` parses a `.ts` file as TypeScript and the local lint
gate failed on the four of them (the extension is only ever logged by the HLS layer, never
matched — plan §2.4); and `04-master.m3u8` is the one playlist that arrived with **CRLF** line
endings, kept as served — a parser has to take both, and `git diff --check` reports them as
trailing whitespace by design.

The captured HTTP headers (content types `application/x-mpegURL`,
`application/vnd.apple.mpegurl[; charset=UTF-8]`, `audio/aac`, `audio/aacp`, `audio/x-aac`,
`video/MP2T`; 10's `content-encoding: gzip`) are not files here: the tests that need them state
them inline, from `p4-shape.tsv` and the `*.headers` files in the census directory.

What the playlists contain is what the servers sent on 2026-09-21: segment URIs with that day's
sequence numbers and, for 02 and 09, the CDN's own query parameters (`rj-org=`,
`hls_proxy_host=`) and 02's programme metadata in `EXTINF` titles. None of it is a credential;
all of it is what any listener's player received. Station names and identifiers are
radio-browser.info data, mirrored under the API's stated freedoms — see
`crates/ondar-stations/fixtures/PROVENANCE.md`. The audio heads are under a second of each
broadcast, kept to the minimum a byte-level test needs; the "US federal work" claim for VOA is
not made (VOA carries third-party material) and is not needed.
