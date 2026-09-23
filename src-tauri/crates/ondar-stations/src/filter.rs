//! Filter, dedupe, sort, cap — the one place a station is dropped or ordered (G1: locally,
//! never by the server's `limit`, which would cap before the filter).

use std::collections::HashMap;

use crate::model::Station;

/// Per-country cap, decided 2026-09-21 by Martín from the census's coverage curve: 750 keeps
/// 56.9 % (US) to 100 % (PT) of a country's clicks — ONDAR.md, "M3 Step 0: the live data,
/// measured".
pub const CAP: usize = 750;

/// Drop what cannot play, merge duplicates, order, cap.
///
/// - drop `!last_check_ok` (8.7 % of the census; belt and braces — the request already sends
///   `hidebroken=true`);
/// - drop an empty `url` (210 of 25 236 — nothing to play);
/// - dedupe on (case-folded trimmed name, url), keeping the higher-votes row (1–22 % per
///   country; FR 832 rows, mostly the same stream under name variants);
/// - sort by votes desc, then **known bitrate before unknown** (a zero bitrate is unknown, not
///   broken — kept and sorted last, decided 2026-09-21), then click trend desc, then uuid for a
///   stable order;
/// - truncate to `cap`.
pub fn rank(stations: Vec<Station>, cap: usize) -> Vec<Station> {
    let mut best: HashMap<(String, String), Station> = HashMap::new();
    for s in stations {
        if !s.last_check_ok || s.url.is_empty() {
            continue;
        }
        let key = (fold(&s.name), s.url.clone());
        match best.get(&key) {
            Some(kept) if kept.votes >= s.votes => {}
            _ => {
                best.insert(key, s);
            }
        }
    }
    let mut out: Vec<Station> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.votes
            .cmp(&a.votes)
            .then_with(|| a.bitrate_kbps.is_none().cmp(&b.bitrate_kbps.is_none()))
            .then_with(|| b.click_trend.cmp(&a.click_trend))
            .then_with(|| a.uuid.cmp(&b.uuid))
    });
    out.truncate(cap);
    out
}

/// The dedupe key's name half: trimmed, Unicode-lowercased. `Rádio` and `RÁDIO` fold together,
/// which is what the census's duplicate pairs look like.
fn fold(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Codec;

    fn st(
        uuid: &str,
        name: &str,
        url: &str,
        votes: i64,
        bitrate: Option<u32>,
        trend: i64,
    ) -> Station {
        Station {
            uuid: uuid.into(),
            name: name.into(),
            url: url.into(),
            homepage: String::new(),
            favicon: String::new(),
            country_code: "PT".into(),
            codec: Codec::Mp3,
            codec_raw: "MP3".into(),
            bitrate_kbps: bitrate,
            hls: false,
            video: false,
            votes,
            click_count: 0,
            click_trend: trend,
            geo: None,
            last_check_ok: true,
        }
    }

    #[test]
    fn bitrate_zero_sorts_last_among_equal_votes() {
        // Equal votes, the unknown-bitrate row must come second. Fails if `Option` ordering puts
        // `None` first (its natural `Ord` does) or bitrate is not a sort key at all.
        let out = rank(
            vec![
                st("a", "A", "http://a", 10, None, 99),
                st("b", "B", "http://b", 10, Some(128), 0),
            ],
            CAP,
        );
        assert_eq!(
            out.iter().map(|s| s.uuid.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        // …but votes still win over bitrate.
        let out = rank(
            vec![
                st("a", "A", "http://a", 11, None, 0),
                st("b", "B", "http://b", 10, Some(128), 0),
            ],
            CAP,
        );
        assert_eq!(out[0].uuid, "a");
    }

    #[test]
    fn dedupe_keeps_the_higher_votes_row() {
        // Same stream, name differing in case and whitespace: one row, the higher votes — even
        // when the lower-votes row comes first. Fails if the key is not folded or the kept row is
        // the first seen.
        let out = rank(
            vec![
                st("first", " Rádio X ", "http://x", 3, Some(128), 0),
                st("second", "RÁDIO X", "http://x", 9, Some(128), 0),
            ],
            CAP,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].uuid, "second");
        // A different url is a different station, whatever the name.
        let out = rank(
            vec![
                st("a", "Same", "http://a", 1, None, 0),
                st("b", "same", "http://b", 1, None, 0),
            ],
            CAP,
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn broken_and_empty_url_rows_are_dropped() {
        let mut broken = st("x", "X", "http://x", 100, Some(128), 0);
        broken.last_check_ok = false;
        let empty = st("y", "Y", "", 100, Some(128), 0);
        let ok = st("z", "Z", "http://z", 1, Some(128), 0);
        let out = rank(vec![broken, empty, ok], CAP);
        assert_eq!(
            out.iter().map(|s| s.uuid.as_str()).collect::<Vec<_>>(),
            ["z"]
        );
    }

    #[test]
    fn the_cap_truncates_after_sorting() {
        let many: Vec<Station> = (0..900)
            .map(|i| {
                st(
                    &format!("u{i:04}"),
                    &format!("N{i}"),
                    &format!("http://{i}"),
                    i,
                    Some(128),
                    0,
                )
            })
            .collect();
        let out = rank(many, CAP);
        assert_eq!(out.len(), CAP);
        assert_eq!(out[0].votes, 899, "highest votes first, then cut");
        assert_eq!(out[CAP - 1].votes, 899 - (CAP as i64 - 1));
    }

    #[test]
    fn the_pt60_fixture_ranks_to_its_playable_rows() {
        let ss = crate::normalise::stations(include_bytes!("../fixtures/stations-PT-60.json"))
            .expect("parses");
        assert_eq!(ss.len(), 56);
        let out = rank(ss, CAP);
        // Pinned by an independent Python pass over the fixture (2026-09-22): 56 rows, 6
        // broken (the empty-url row is one of them), 50 playable, 44 distinct
        // (folded name, url) keys among the playable. A drop rule that stops firing, or a dedupe
        // key that stops folding, moves this number.
        assert_eq!(out.len(), 44);
        assert!(out.windows(2).all(|w| w[0].votes >= w[1].votes));
        assert!(out.iter().all(|s| s.last_check_ok && !s.url.is_empty()));
    }
}
