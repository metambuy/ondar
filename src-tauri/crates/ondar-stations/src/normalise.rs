//! radio-browser's JSON → [`Country`] / [`Station`], with the normalisation rules the M3
//! Step 0 census called for (G5), each with the count that motivated it.

use serde::Deserialize;

use crate::model::{Codec, Country, Station};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("radio-browser JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Deserialize)]
struct RawCountry {
    name: String,
    iso_3166_1: String,
    stationcount: u32,
}

/// `/json/countries` rows → countries. Rules, from the census (250 rows in, 240 out):
///
/// - **9 lowercase codes** (`ch de fr gr nz ru tr us uy`), each one station and a duplicate of
///   its uppercase row's name, are **merged into the uppercase row** (counts added).
/// - **`XX`** (empty name, 1 station) and any code that is not `[A-Z]{2}` after upper-casing
///   are **dropped** — `bycountrycodeexact` could not address them meaningfully.
/// - Order: as served, which is alphabetical by name.
pub fn countries(json: &[u8]) -> Result<Vec<Country>, ParseError> {
    let raw: Vec<RawCountry> = serde_json::from_slice(json)?;
    let mut out: Vec<Country> = Vec::with_capacity(raw.len());
    for r in raw {
        let code = r.iso_3166_1.trim().to_ascii_uppercase();
        if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_uppercase()) || code == "XX" {
            log::warn!("countries: dropping code {:?} ({:?})", r.iso_3166_1, r.name);
            continue;
        }
        if let Some(existing) = out.iter_mut().find(|c| c.code == code) {
            existing.station_count += r.stationcount;
            continue;
        }
        out.push(Country {
            code,
            name: r.name.trim().to_string(),
            station_count: r.stationcount,
        });
    }
    Ok(out)
}

#[derive(Deserialize)]
struct RawStation {
    stationuuid: String,
    name: String,
    #[serde(default)]
    url_resolved: String,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    favicon: String,
    #[serde(default)]
    countrycode: String,
    #[serde(default)]
    codec: String,
    #[serde(default)]
    bitrate: u32,
    #[serde(default)]
    hls: u8,
    #[serde(default)]
    lastcheckok: u8,
    #[serde(default)]
    votes: i64,
    #[serde(default)]
    clickcount: i64,
    #[serde(default)]
    clicktrend: i64,
    #[serde(default)]
    geo_lat: Option<f64>,
    #[serde(default)]
    geo_long: Option<f64>,
    #[serde(default)]
    state: String,
}

/// Station rows → stations. This maps and flags; it drops nothing — dropping (broken, empty
/// URL, duplicates) is [`crate::filter::rank`]'s job so every rule has one home.
pub fn stations(json: &[u8]) -> Result<Vec<Station>, ParseError> {
    let raw: Vec<RawStation> = serde_json::from_slice(json)?;
    Ok(raw.into_iter().map(station).collect())
}

fn station(r: RawStation) -> Station {
    let (codec, video) = codec(&r.codec);
    Station {
        uuid: r.stationuuid,
        name: r.name.trim().to_string(),
        url: r.url_resolved.trim().to_string(),
        homepage: r.homepage,
        favicon: r.favicon,
        country_code: r.countrycode.to_ascii_uppercase(),
        codec,
        codec_raw: r.codec,
        bitrate_kbps: (r.bitrate > 0).then_some(r.bitrate),
        hls: r.hls == 1,
        video,
        votes: r.votes,
        click_count: r.clickcount,
        click_trend: r.clicktrend,
        geo: match (r.geo_lat, r.geo_long) {
            (Some(lat), Some(lng)) if !(lat == 0.0 && lng == 0.0) => Some((lat, lng)),
            _ => None,
        },
        state: r.state.trim().to_string(),
        last_check_ok: r.lastcheckok == 1,
    }
}

/// The census's codec strings, verbatim (pooled, 25 236 stations): `MP3` 16 678, `AAC+` 3 982,
/// `AAC` 3 682, `UNKNOWN` 310, `OGG` 308, empty 210, `AAC,H.264` 41, `MP4` 13. Comma-separated
/// lists name every stream in a container; a video codec anywhere in it flags `video`.
fn codec(raw: &str) -> (Codec, bool) {
    let parts: Vec<String> = raw
        .split(',')
        .map(|p| p.trim().to_ascii_uppercase())
        .filter(|p| !p.is_empty())
        .collect();
    let video = parts.iter().any(|p| {
        matches!(
            p.as_str(),
            "H.264" | "H264" | "H.265" | "HEVC" | "AVC" | "VP8" | "VP9" | "MPEG2"
        )
    });
    let audio = parts.iter().find_map(|p| match p.as_str() {
        "MP3" => Some(Codec::Mp3),
        "AAC" | "MP4" | "AAC-LC" => Some(Codec::Aac),
        "AAC+" | "AACP" | "HE-AAC" => Some(Codec::AacPlus),
        "OGG" | "VORBIS" | "OPUS" => Some(Codec::Ogg),
        "FLAC" => Some(Codec::Flac),
        _ => None,
    });
    (audio.unwrap_or(Codec::Unknown), video)
}

#[cfg(test)]
mod tests {
    //! Against the recorded fixtures (`fixtures/`, provenance in `PROVENANCE.md`): counts pinned
    //! to what the census measured, so a rule that stops firing changes a number.

    use super::*;

    const COUNTRIES: &[u8] = include_bytes!("../fixtures/countries.json");
    const PT60: &[u8] = include_bytes!("../fixtures/stations-PT-60.json");
    const MT: &[u8] = include_bytes!("../fixtures/stations-MT.json");
    const PT_GEO: &[u8] = include_bytes!("../fixtures/stations-PT-geo.json");
    const US_GEO: &[u8] = include_bytes!("../fixtures/stations-US-geo.json");
    const BR_GEO: &[u8] = include_bytes!("../fixtures/stations-BR-geo.json");
    const RU_GEO: &[u8] = include_bytes!("../fixtures/stations-RU-geo.json");

    #[test]
    fn parses_p2_fixture_to_240_countries() {
        let cs = countries(COUNTRIES).expect("parses");
        // 250 rows served: 9 lowercase duplicates merged, XX dropped. Fails if either rule
        // stops firing (249 or 241) or fires too widely.
        assert_eq!(cs.len(), 240);
        assert!(
            cs.iter()
                .all(|c| c.code.len() == 2 && c.code.bytes().all(|b| b.is_ascii_uppercase()))
        );
        assert!(!cs.iter().any(|c| c.code == "XX"));
        assert_eq!(cs.iter().filter(|c| c.code == "DE").count(), 1);
    }

    #[test]
    fn lowercase_codes_are_merged_into_the_uppercase_row_with_their_count() {
        let cs = countries(COUNTRIES).expect("parses");
        // Germany was 6410 as `DE` plus 1 as `de` in the recorded list (p2-oddities.txt).
        let de = cs.iter().find(|c| c.code == "DE").expect("DE");
        assert_eq!(de.station_count, 6410 + 1);
        assert_eq!(de.name, "Germany");
    }

    #[test]
    fn parses_pt60_fixture_with_its_edge_rows() {
        let ss = stations(PT60).expect("parses");
        // Counts pinned by an independent pass over the fixture in Python (scripts/fixture-slice.py's
        // data, 2026-09-22): 56 rows; 6 with lastcheckok == 0 (five among the first 50 as
        // served plus the slicer's extras); 8 with bitrate 0; 25 with hls == 1; 1 empty url.
        assert_eq!(
            ss.len(),
            56,
            "50 as served + the extras the slicer appends (dedupes by uuid)"
        );
        assert!(ss.iter().all(|s| s.country_code == "PT"));
        assert_eq!(ss.iter().filter(|s| !s.last_check_ok).count(), 6);
        assert_eq!(
            ss.iter().filter(|s| s.bitrate_kbps.is_none()).count(),
            8,
            "bitrate 0 → None"
        );
        assert_eq!(ss.iter().filter(|s| s.hls).count(), 25);
        assert_eq!(
            ss.iter().filter(|s| s.url.is_empty()).count(),
            1,
            "one empty url_resolved row"
        );
    }

    #[test]
    fn mt_fixture_is_the_whole_country() {
        let ss = stations(MT).expect("parses");
        assert_eq!(ss.len(), 14);
        assert_eq!(ss[0].name, "89.7 Bay");
        assert_eq!(ss[0].bitrate_kbps, Some(256));
        assert_eq!(ss[0].codec, Codec::Mp3);
        assert_eq!(ss[0].geo, None, "geo_lat null → None");
    }

    /// `state` is carried and trimmed: PT-60 serves `Lisboa` 7 times and `"Lisboa "` once, so
    /// 8 after the trim; 29 rows have none and stay empty; no value keeps edge whitespace. Fails
    /// if the field is not read (0 `Lisboa`, 56 empty), or not trimmed (7, and one untrimmed).
    #[test]
    fn state_is_carried_and_trimmed() {
        let ss = stations(PT60).expect("parses");
        assert_eq!(ss.iter().filter(|s| s.state == "Lisboa").count(), 8);
        assert_eq!(ss.iter().filter(|s| s.state.is_empty()).count(), 29);
        assert!(ss.iter().all(|s| s.state.trim() == s.state));
    }

    /// M4c's four geo slices (`fixtures/stations-{PT,US,BR,RU}-geo.json`, PROVENANCE.md) are
    /// what the map tests draw from: each is exactly the census list's rows that survive
    /// `filter::rank` and carry `geo`. So each normalises to its row count (PT 68, US 169,
    /// BR 220, RU 50, the Step 0 seed's figures), every row has a position and its country's
    /// code, and `rank` over the slice drops nothing. Fails on a slice cut without the rank
    /// (PT 79: the served 371 carry 79 positions, `rank` keeps 68) or with a row of no
    /// position, and if the geo rule or `rank`'s drops change.
    #[test]
    fn the_geo_slices_normalise_to_their_row_counts() {
        for (cc, json, rows) in [
            ("PT", PT_GEO, 68),
            ("US", US_GEO, 169),
            ("BR", BR_GEO, 220),
            ("RU", RU_GEO, 50),
        ] {
            let ss = stations(json).expect("parses");
            assert_eq!(ss.len(), rows, "{cc} rows");
            assert!(
                ss.iter().all(|s| s.geo.is_some() && s.country_code == cc),
                "{cc}"
            );
            assert_eq!(
                crate::filter::rank(ss, crate::filter::CAP).len(),
                rows,
                "{cc}: rank drops nothing"
            );
        }
    }

    #[test]
    fn codec_strings_map_and_video_is_flagged() {
        assert_eq!(codec("MP3"), (Codec::Mp3, false));
        assert_eq!(codec("AAC+"), (Codec::AacPlus, false));
        assert_eq!(codec("AAC"), (Codec::Aac, false));
        assert_eq!(codec("OGG"), (Codec::Ogg, false));
        assert_eq!(codec("UNKNOWN"), (Codec::Unknown, false));
        assert_eq!(codec(""), (Codec::Unknown, false));
        // The 41 TV feeds in the census: audio AAC, video present.
        assert_eq!(codec("AAC,H.264"), (Codec::Aac, true));
        assert_eq!(codec("aac"), (Codec::Aac, false), "case-insensitive");
    }

    /// The geo rule: `(0, 0)` and a missing half are `None`, a real pair is `Some`; the country
    /// code is upper-cased.
    #[test]
    fn geo_both_zero_is_none() {
        let mk = |lat, lng| {
            station(RawStation {
                stationuuid: "u".into(),
                name: "n".into(),
                url_resolved: "http://x".into(),
                homepage: String::new(),
                favicon: String::new(),
                countrycode: "pt".into(),
                codec: "MP3".into(),
                bitrate: 128,
                hls: 0,
                lastcheckok: 1,
                votes: 0,
                clickcount: 0,
                clicktrend: 0,
                geo_lat: lat,
                geo_long: lng,
                state: String::new(),
            })
        };
        assert_eq!(mk(Some(0.0), Some(0.0)).geo, None);
        assert_eq!(mk(Some(38.7), None).geo, None);
        assert_eq!(mk(Some(38.7), Some(-9.1)).geo, Some((38.7, -9.1)));
        assert_eq!(
            mk(None, None).country_code,
            "PT",
            "country code upper-cased"
        );
    }
}
