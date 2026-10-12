//! `ondar-map`: the drawn map's pure core (M4a). The projection, the frame rules shared with the
//! build tool (`ondar-map-build`), the bundled resource's format and its loader, framing, and
//! (M4c) stations gathered on the ground (`gather`).
//!
//! No Tauri dependency and no network dependency: the resource is built at build time from
//! Natural Earth and bundled; this crate only reads it.

pub mod clip;
pub mod codec;
pub mod format;
pub mod frame;
pub mod gather;
pub mod index;
pub mod laea;
pub mod rules;
