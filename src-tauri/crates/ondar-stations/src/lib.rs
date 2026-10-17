//! The station directory: radio-browser.info as Ondar sees it.
//!
//! No Tauri dependency, so everything here is unit-testable with `cargo test --workspace`.
//! The shell wires commands and events around [`model`]'s types.
//!
//! What the M3 Step 0 census (2026-09-21, `_handover/m3-step0-report.md`) established and
//! this crate is built on: one API server; `bycountrycodeexact` truncates silently at 1000
//! without an explicit `limit`; `hidebroken=true` *is* the `lastcheckok == 1` filter; a zero
//! bitrate is "unknown", not "broken" (kept, sorted last); the per-country cap is 750.
// Tests assert by panicking and print what they measured; the lib build still checks every
// non-test line against the workspace table (src-tauri/Cargo.toml).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::float_cmp,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::items_after_statements,
        clippy::match_wildcard_for_single_variants,
        clippy::match_wild_err_arm,
        clippy::case_sensitive_file_extension_comparisons,
        clippy::format_push_string,
        clippy::manual_let_else,
        clippy::similar_names,
        clippy::unreadable_literal,
        clippy::unnecessary_wraps,
        clippy::format_collect,
        clippy::default_trait_access
    )
)]

pub mod cache;
pub mod client;
pub mod filter;
pub mod model;
pub mod normalise;
pub mod service;
pub mod srv;
pub mod store;
mod sync;

pub use model::{CacheSource, Codec, Country, ListedCountries, ListedStations, Station};
pub use service::{Event, EventSink, ServiceError, StationsHandle, StationsService};
