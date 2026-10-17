//! The log's two sinks (M6-lite): stderr, and a daily file in the app log dir. A Finder launch
//! discards stderr, so the file is what a tester sends. Both sit behind the same `EnvFilter` and
//! the same `RateLimit`, so the file holds exactly what the terminal shows. No redaction: the
//! log carries station names and stream URLs (ONDAR.md, "M6-lite before M5", "Log file").
//!
//! Two `tracing-subscriber` 0.3.23 mechanics shape the layout (commit 3b, both observed in the
//! release log of 2026-10-13 and pinned by tests below):
//!
//! - **The `EnvFilter` is a global layer, not a per-layer filter.** A per-layer filter records
//!   its verdict on an event in a thread-local (`FilterState`) between the event's `enabled`
//!   pass and its dispatch; a span opened while the event's *field values* are evaluated runs
//!   its own `enabled` pass and overwrites that verdict. `stream-download`'s `read` does exactly
//!   that (a DEBUG event with an `#[instrument]`ed `stream_position()` as a field), so DEBUG
//!   lines reached the file under `info`. A global layer answers `enabled` before the fields
//!   exist, and the `RateLimit` behind it only ever counts events the filter admitted: as a
//!   per-layer filter *after* the rate limit, the filter let the limiter count, and report at
//!   WARN once a second, DEBUG and TRACE events it was about to discard anyway.
//! - **The file layer has its own field-formatter type.** A span's fields are formatted once
//!   per formatter type and stored on the span (`FormattedFields<N>`); two `fmt` layers that
//!   share `N` share the slot, so stderr's coloured rendering was reused by the file.

use std::path::{Path, PathBuf};

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::{DefaultFields, Writer};
use tracing_subscriber::prelude::*;

use crate::log_rate_limit::RateLimit;

/// The file layer's field formatter: `DefaultFields`' output, under a type of its own so the
/// file layer keeps its own `FormattedFields<PlainFields>` slot on every span and formats the
/// span's fields itself, with the layer's `with_ansi(false)` honoured. Sharing `DefaultFields`
/// with the stderr layer shares the slot, and whichever layer sees the span first (stderr) wins.
struct PlainFields(DefaultFields);

impl<'writer> FormatFields<'writer> for PlainFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        writer: Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        self.0.format_fields(writer, fields)
    }
    // `add_fields` (a span's `record` after creation) keeps the trait's default, which appends
    // through `format_fields` above.
}

/// Files kept, today's included; older ones are pruned when the appender opens at startup (and
/// at each rollover). `tracing-appender` 0.2.5 deletes down to `KEEP - 1` before it creates the
/// new day's file.
pub const KEEP: usize = 7;
const PREFIX: &str = "ondar";
const SUFFIX: &str = "log";

/// The app log dir, derived the way Tauri 2.11.5 derives `app_log_dir()` on macOS
/// (`path/desktop.rs:278`: `dirs::home_dir()/Library/Logs/<identifier>`). Derived rather than
/// asked of the app handle because the subscriber is installed before the `Builder` exists:
/// the engine and the single-instance plugin log first. `setup` compares this with
/// `app.path().app_log_dir()` and warns if they differ, so the derivation is checked on every
/// launch.
pub fn dir(identifier: &str) -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join("Library/Logs").join(identifier))
}

/// The subscriber: stderr always, and the daily file in `dir` when the directory can be created
/// and the file opened. A failure warns on stderr and leaves the file out; it never panics.
/// The appender writes synchronously (no `non_blocking` worker), so a `panic = "abort"` or
/// `process::exit` loses no buffered line. The `EnvFilter` is the global layer under the sinks
/// and the `RateLimit` a per-layer filter on the pair; the module doc says why that order.
pub fn subscriber(
    dir: Option<&Path>,
    filter: EnvFilter,
) -> impl tracing::Subscriber + Send + Sync + 'static {
    let file = dir.and_then(open);
    tracing_subscriber::registry().with(filter).with(
        tracing_subscriber::fmt::layer()
            .and_then(file.map(|f| {
                tracing_subscriber::fmt::layer()
                    .fmt_fields(PlainFields(DefaultFields::new()))
                    .with_ansi(false)
                    .with_writer(f)
            }))
            .with_filter(RateLimit::new()),
    )
}

// The logger is not up yet: these two lines are the only report an unopenable log dir gets.
#[allow(clippy::print_stderr, reason = "the logger is not up yet")]
fn open(dir: &Path) -> Option<RollingFileAppender> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("ondar: no log file, cannot create {}: {e}", dir.display());
        return None;
    }
    match RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(PREFIX)
        .filename_suffix(SUFFIX)
        .max_log_files(KEEP)
        .build(dir)
    {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("ondar: no log file in {}: {e}", dir.display());
            None
        }
    }
}

/// The first line of every launch, in both sinks: the version and where the file is. `tracing::`,
/// not `log::`: it reaches a subscriber without the `LogTracer` bridge, so the test sees it.
pub fn startup(dir: Option<&Path>) {
    if let Some(d) = dir {
        tracing::info!(
            "Ondar {} starting; log dir {}",
            env!("CARGO_PKG_VERSION"),
            d.display()
        );
    } else {
        tracing::info!(
            "Ondar {} starting; no log dir (no home directory)",
            env!("CARGO_PKG_VERSION")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ondar-log-sink-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    /// Today's UTC date as `YYYY-MM-DD`, the appender's daily name (it dates in UTC). Howard
    /// Hinnant's `civil_from_days`.
    fn utc_today() -> String {
        let days = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 86_400) as i64;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}")
    }

    fn log_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("ondar.") && n.ends_with(".log"))
            .collect();
        names.sort();
        names
    }

    fn build_and_start(dir: &Path) {
        tracing::subscriber::with_default(subscriber(Some(dir), EnvFilter::new("info")), || {
            startup(Some(dir));
        });
    }

    /// Today's file in `dir`, under either date read around the call (a run across midnight).
    fn todays_file(dir: &Path, dates: &[String; 2]) -> PathBuf {
        dates
            .iter()
            .map(|d| dir.join(format!("ondar.{d}.log")))
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("no file for today in {:?}", log_files(dir)))
    }

    /// Pins the sink: after the subscriber is built on a log dir that does not exist, the dir
    /// exists and today's `ondar.<UTC date>.log` holds the startup line, with no ANSI escapes.
    /// Fails if the file layer is dropped or not given the dir (no file, or no line), if the
    /// name's prefix, suffix or rotation changes (today's name not found), or if the file layer
    /// keeps stderr's colours.
    #[test]
    fn startup_line_lands_in_the_days_file() {
        let root = scratch("ok");
        let dir = root.join("Logs").join("eu.ondar.radio.test");
        assert!(!dir.exists());

        let before = utc_today();
        build_and_start(&dir);
        let file = todays_file(&dir, &[before, utc_today()]);

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains(&format!(
                "Ondar {} starting; log dir",
                env!("CARGO_PKG_VERSION")
            )),
            "startup line missing from {}: {text:?}",
            file.display()
        );
        assert!(
            !text.contains('\u{1b}'),
            "ANSI escapes in the file: {text:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Pins the pruning: with eight older daily files present, opening the sink leaves at most
    /// `KEEP` files, today's among them. Fails if `max_log_files` is removed or raised (nine).
    #[test]
    fn opening_the_sink_keeps_at_most_seven_files() {
        let root = scratch("prune");
        let dir = root.join("Logs");
        std::fs::create_dir_all(&dir).unwrap();
        // Created before the appender opens, so they are the oldest by creation time.
        for day in 1..=8 {
            std::fs::write(dir.join(format!("ondar.2026-01-0{day}.log")), b"old\n").unwrap();
        }

        let before = utc_today();
        build_and_start(&dir);
        todays_file(&dir, &[before, utc_today()]);

        let names = log_files(&dir);
        assert!(names.len() <= KEEP, "{} files kept: {names:?}", names.len());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Pins the file's span fields: an event inside a span that has a field lands in the file
    /// with the span's fields and no ANSI escape. `tracing-subscriber` 0.3.23 formats a span's
    /// fields once per field-formatter type `N` and stores them on the span
    /// (`fmt_layer.rs:878`, `FormattedFields<N>`); two `fmt` layers that share `N` share that
    /// slot, stderr fills it first, in colour, and the file layer reuses it, so `new{url=…}`
    /// carried `\x1b[3m` in the release log (2026-10-13). Fails if the file layer goes back to
    /// the default field formatter (same `N` as stderr).
    #[test]
    fn span_fields_in_the_file_carry_no_ansi_escapes() {
        let root = scratch("span-ansi");
        let dir = root.join("Logs");

        let before = utc_today();
        tracing::subscriber::with_default(subscriber(Some(&dir), EnvFilter::new("info")), || {
            let span = tracing::info_span!("new", url = "http://example.invalid/s");
            let _enter = span.enter();
            tracing::warn!(target: "stream_download::http", "content length header missing");
        });
        let file = todays_file(&dir, &[before, utc_today()]);

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("new{url=\"http://example.invalid/s\"}"),
            "span fields missing from {}: {text:?}",
            file.display()
        );
        assert!(
            !text.contains('\u{1b}'),
            "ANSI escapes in the file's span fields: {text:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Pins where the `EnvFilter` sits: an event the filter rejects is never written, even when
    /// one of its field values opens a span the filter accepts. `stream-download` 0.24.4's
    /// `read` does exactly that (`lib.rs:579`: a DEBUG event with
    /// `self.output_reader.stream_position()?` as a field, which runs an `#[instrument]`ed INFO
    /// `seek`, `storage/bounded.rs:237`). With the filter applied per layer
    /// (`.with_filter(filter)`), the span's `enabled` pass overwrote the thread's per-layer
    /// verdict on the event (`layer_filters/mod.rs`, `FilterState`), and the DEBUG line reached
    /// both sinks (one in the release log, 2026-10-13). As a global layer the filter answers
    /// before the fields are evaluated. Fails if the filter moves back onto the sink layer.
    #[test]
    fn a_rejected_event_with_a_span_in_its_fields_stays_out() {
        let root = scratch("leak");
        let dir = root.join("Logs");

        let before = utc_today();
        tracing::subscriber::with_default(subscriber(Some(&dir), EnvFilter::new("info")), || {
            tracing::debug!(
                target: "stream_download",
                output_stream_position = tracing::info_span!("seek").in_scope(|| 7u64),
                "reached requested position"
            );
            tracing::info!("still writing");
        });
        let file = todays_file(&dir, &[before, utc_today()]);

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("still writing"), "sink dead: {text:?}");
        assert!(
            !text.contains("reached requested position"),
            "a DEBUG event passed an `info` filter: {text:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Pins the fallback: a log dir that cannot be created (its parent is a file) builds a
    /// subscriber that logs without a file and does not panic. Fails if `open` unwraps the
    /// directory or the appender's error.
    #[test]
    fn an_uncreatable_dir_runs_without_the_file() {
        let root = scratch("blocked");
        std::fs::create_dir_all(&root).unwrap();
        let blocker = root.join("not-a-dir");
        std::fs::write(&blocker, b"").unwrap();
        let dir = blocker.join("Logs");

        build_and_start(&dir);

        assert!(!dir.exists());
        std::fs::remove_dir_all(&root).ok();
    }
}
