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

/// How a background refresh ended — carried by `stations:updated` / `countries:updated`, so
/// the page can tell "re-request, the list changed" from "the expired list stays; stop saying
/// refreshing". Added 2026-09-22 (M3a acceptance item 6): before it, a failed refresh emitted
/// nothing and the page's `refreshing…` never cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RefreshOutcome {
    /// The fetch landed and the stored list was replaced.
    Landed,
    /// Every attempt failed; whatever was stored stays as it was.
    Failed,
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
    /// `i64` in Rust; a JSON number on the wire and a `number` in TypeScript — the counts
    /// are far below 2^53 (the census maximum was 10 832 votes).
    #[ts(type = "number")]
    pub votes: i64,
    #[ts(type = "number")]
    pub click_count: i64,
    #[ts(type = "number")]
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
    /// `refreshing` on the listing). A refresh that failed leaves the list `Cached` with its
    /// age; the `failed` outcome on `stations:updated` is how the page learns of it — there is
    /// no third source (a `StaleAfterFailure` variant nothing constructed was removed on
    /// 2026-09-22, review cleanup: a dead variant in the IPC contract).
    Cached,
}

/// The countries list with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListedCountries {
    pub items: Vec<Country>,
    /// Unix seconds when the network answered.
    #[ts(type = "number")]
    pub fetched_at: i64,
    #[ts(type = "number")]
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
    #[ts(type = "number")]
    pub fetched_at: i64,
    #[ts(type = "number")]
    pub age_secs: u64,
    pub source: CacheSource,
    pub refreshing: bool,
}
