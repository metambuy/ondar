//! `ondar-map`: the drawn map's pure core (M4a). The projection, the frame rules shared with the
//! build tool (`ondar-map-build`), the bundled resource's format and its loader, framing, and
//! (M4c) stations gathered on the ground (`gather`).
//!
//! No Tauri dependency and no network dependency: the resource is built at build time from
//! Natural Earth and bundled; this crate only reads it.
// Tests assert by panicking and print what they measured; the lib build still checks every
// non-test line against the workspace table (src-tauri/Cargo.toml).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::float_cmp,
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
// Projection and geometry arithmetic casts by design: a quantised coordinate to its integer
// cell, a vertex count to the f64 it is averaged in. The lints stay on in every other crate.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "projection and geometry arithmetic by design"
)]

pub mod clip;
pub mod codec;
pub mod format;
pub mod frame;
pub mod gather;
pub mod index;
pub mod laea;
pub mod rules;
