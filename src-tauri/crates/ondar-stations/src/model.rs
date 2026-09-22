//! Types that cross the Rust ↔ TypeScript boundary. `cargo test --workspace` regenerates the
//! `.ts` files in `src/bindings/` (see `.cargo/config.toml` at the repo root).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A country row from `/json/countries`, after [`crate::normalise::countries`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Country {
    /// ISO 3166-1 alpha-2, upper case — the key `list_stations` takes.
    pub code: String,
    pub name: String,
    /// radio-browser's own count for the code (with `hidebroken=true`, the checked-OK count).
    /// The app lists its own filtered, capped count once a list is cached; this one fills the
    /// dropdown before that.
    pub station_count: u32,
}

/// The audio codec radio-browser reports, normalised from its free-text `codec` field.
/// `raw` on [`Station`] keeps the original string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    Mp3,
    Aac,
    /// `AAC+` (HE-AAC).
    AacPlus,
    Ogg,
    Flac,
    /// `UNKNOWN`, empty, or anything not listed above.
    Unknown,
}

/// A station as the UI needs it, after [`crate::normalise::stations`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Station {
    pub uuid: String,
    /// Trimmed.
    pub name: String,
    /// `url_resolved` — the playable URL, redirects already followed by radio-browser's checker.
    pub url: String,
    pub homepage: String,
    pub favicon: String,
    pub country_code: String,
    pub codec: Codec,
    /// The `codec` string as served (`MP3`, `AAC+`, `AAC,H.264`, `UNKNOWN`, `` …).
    pub codec_raw: String,
    /// `None` when radio-browser reports 0 — unknown, not broken (decided 2026-09-21).
    pub bitrate_kbps: Option<u32>,
    /// radio-browser's `hls == 1` (kept and flagged; playback is M3c's).
    pub hls: bool,
    /// The codec string names a video codec (`AAC,H.264`, 41 of 25 236 in the census): a TV
    /// feed listed as radio. Kept and flagged; whether to hide it is M3c's decision.
    pub video: bool,
    pub votes: i64,
    pub click_count: i64,
    pub click_trend: i64,
    /// `(lat, lng)`; `None` when either is null **or both are 0** (a known default, not a
    /// position — none seen in the census, the rule is a guard).
    pub geo: Option<(f64, f64)>,
    /// `lastcheckok == 1`. With `hidebroken=true` on the request this is always true; kept so a
    /// list from any other source can be filtered the same way.
    pub last_check_ok: bool,
}

/// Where a listed result came from, so the UI can be honest about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CacheSource {
    /// Fetched from the network for this request.
    Fresh,
    /// Served from SQLite. Fresh within its TTL, or expired and being refreshed (see
    /// `refreshing` on the listing).
    Cached,
    /// The network failed for a caller that had no list to fall back on — this variant is not
    /// used for a listing today (a missing list is an error, an expired one is `Cached`), but
    /// the shape is kept so an explicit stale state can be reported later without a boundary
    /// change.
    StaleAfterFailure { error: String },
}

/// The countries list with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListedCountries {
    pub items: Vec<Country>,
    /// Unix seconds when the network answered.
    pub fetched_at: i64,
    pub age_secs: u64,
    pub source: CacheSource,
    /// A background refresh is in flight for this list (stale-while-revalidate); a
    /// `stations:updated` event follows when it lands.
    pub refreshing: bool,
}

/// One country's station list with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListedStations {
    pub country_code: String,
    pub items: Vec<Station>,
    pub fetched_at: i64,
    pub age_secs: u64,
    pub source: CacheSource,
    pub refreshing: bool,
}
