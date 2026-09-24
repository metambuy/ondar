//! HLS, the ADTS half (M3c). A live playlist is refreshed, its ADTS segments are normalised and
//! concatenated into one byte stream, and that stream is fed to the same reader, decoder, ring,
//! converter and EQ the Icecast path uses. Nothing downstream of the reader knows the
//! difference.
//!
//! This module is built up one pure layer at a time:
//! - [`playlist`]: the parser for the nine tags the player needs, the variant choice, and the
//!   refresh planner that decides which sequence numbers to fetch and how long to wait. No I/O,
//!   no clock of its own — instants are injected — so every decision is unit-tested.
//! - [`segment`]: per-segment normalisation — the ID3 skip, the ADTS walk that clears the
//!   MPEG-2 ID bit and drops a CRC, the format guard, the container sniff, and gunzip.
//!
//! What follows in a later commit: the fetch task that turns the planner's steps into HTTP
//! requests and a `SourceStream` below `stream-download`. See `_handover/m3c-plan.md`.

pub mod playlist;
pub mod segment;
