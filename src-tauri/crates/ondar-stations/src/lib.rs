//! The station directory: radio-browser.info as Ondar sees it.
//!
//! No Tauri dependency, so everything here is unit-testable with `cargo test --workspace`.
//! The shell wires commands and events around [`model`]'s types.
//!
//! What the M3 Step 0 census (2026-09-21, `_handover/m3-step0-report.md`) established and
//! this crate is built on: one API server; `bycountrycodeexact` truncates silently at 1000
//! without an explicit `limit`; `hidebroken=true` *is* the `lastcheckok == 1` filter; a zero
//! bitrate is "unknown", not "broken" (kept, sorted last); the per-country cap is 750.

pub mod client;
pub mod filter;
pub mod model;
pub mod normalise;
pub mod srv;

pub use model::{CacheSource, Codec, Country, ListedCountries, ListedStations, Station};
