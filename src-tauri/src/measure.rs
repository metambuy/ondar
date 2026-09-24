//! Dev-only measurement harness (M3b commit 1a). `lib.rs` gates this module and its command
//! under `debug_assertions`, so a release binary carries none of it — checked by `strings` on
//! the release binary finding no `measure[` prefix (M3b plan, Verification 4). Everything is
//! env-driven at launch and inert otherwise:
//!
//! - `ONDAR_MEASURE=<mode>[&k=v…]` — the popover loads `panel.html?measure=<value>`; the page
//!   reads the query and runs the measurement the mode names (`fit`: block heights and the rows
//!   that fit, commit 1b; `perf`: mount, show and scroll timing, commit 2). Unset, the URL is the
//!   plain `panel.html` and the page's harness (`src/measure.ts`) does nothing.
//! - `ONDAR_MEASURE_KEEP_OPEN=1` — the resign-key hide is skipped (logged `SUPPRESSED`), so
//!   reaching Terminal or Safari's Web Inspector does not dismiss the popover. M2d Step 0's
//!   `ONDAR_PROBE_KEEP_OPEN`, kept under the harness's name.
//! - `ONDAR_MEASURE_SEQ=show` | `shows:<n>` — a thread drives the **production** show and hide
//!   paths (`tray::rect` + `panel::show_at`, `panel::hide`) with no click: one show at +8 s that
//!   stays, or `n` show/hide cycles from +8 s, 1 s apart. Every step is logged with its offset,
//!   and the `panel show … after_ms=` lines the shows produce are the `LAYOUT_FALLBACK`
//!   distribution the M2d acceptance left at n = 8 (M3b plan, commit 2's m2).
//! - `measure_report`, the page's one command: a log line `measure[<mode>] <kind> <fields>
//!   t_page_ms=… t_ms=…` stamped with this process's clock, so the page's marks and the panel's
//!   own log lines share a timeline (`performance.now()` is a different clock, used only for the
//!   page's own deltas).

use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Runtime, WebviewUrl};

use crate::panel::{self, HideReason, ShowReason};
use crate::tray;

/// Process start as the harness sees it (set in [`setup`], a few ms after the real start —
/// every `t_ms` is relative to this one instant, so offsets between lines are exact).
static START: OnceLock<Instant> = OnceLock::new();

/// The first show of a sequence, after launch: the tray exists by then, and the launcher's
/// `.meta` capture (NSScreen dump, ~1.5 s) is over.
const SEQ_START: Duration = Duration::from_secs(8);
/// Between a show and the hide that follows it, and between that hide and the next show.
const SEQ_STEP: Duration = Duration::from_secs(1);

/// `ONDAR_MEASURE`'s value, if set and non-empty.
pub fn mode() -> Option<String> {
    std::env::var("ONDAR_MEASURE")
        .ok()
        .filter(|m| !m.is_empty())
}

/// `ONDAR_MEASURE_KEEP_OPEN=1`: the resign-key hide is suppressed (`panel::setup`).
pub fn keep_open() -> bool {
    std::env::var("ONDAR_MEASURE_KEEP_OPEN").is_ok_and(|v| v == "1")
}

/// The popover's URL with the mode appended as its query, or `None` when no mode is set. The
/// value is passed verbatim (`fit&cc=FR` → `panel.html?measure=fit&cc=FR`): it is this Mac's
/// operator's own launch line, not input.
pub fn panel_url() -> Option<WebviewUrl> {
    mode().map(|m| WebviewUrl::App(format!("panel.html?measure={m}").into()))
}

/// The log prefix's mode: the value before any `&` — `fit&cc=FR` is the query, `fit` the mode.
fn prefix() -> String {
    mode().map_or_else(
        || "none".into(),
        |m| m.split('&').next().unwrap_or_default().to_owned(),
    )
}

fn t_ms() -> f64 {
    START
        .get()
        .map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0)
}

/// Record the start instant, log what the harness will do, and start the show sequence if one
/// is asked for. Called from `setup` after the panel and the tray exist.
pub fn setup<R: Runtime>(handle: &AppHandle<R>) {
    let _ = START.set(Instant::now());
    let Some(query) = mode() else {
        return;
    };
    let mode = prefix();
    let seq = std::env::var("ONDAR_MEASURE_SEQ").ok();
    log::info!(
        "measure[{mode}] setup keep_open={} seq={seq:?} url=panel.html?measure={query}",
        keep_open()
    );
    let Some(seq) = seq else {
        return;
    };
    let cycles = match seq.as_str() {
        "show" => None,
        s => match s.strip_prefix("shows:").and_then(|n| n.parse::<u32>().ok()) {
            Some(n) => Some(n),
            None => {
                log::warn!("measure[{mode}] ONDAR_MEASURE_SEQ={seq:?} not understood; no sequence");
                return;
            }
        },
    };
    let handle = handle.clone();
    thread::Builder::new()
        .name("ondar-measure-seq".into())
        .spawn(move || run_sequence(&handle, &mode, cycles))
        .expect("spawn measure sequence thread");
}

/// `None`: one show that stays. `Some(n)`: `n` show/hide cycles, ending hidden.
fn run_sequence<R: Runtime>(handle: &AppHandle<R>, mode: &str, cycles: Option<u32>) {
    thread::sleep(SEQ_START);
    let show = |i: u32| {
        let Some(rect) = tray::rect(handle) else {
            log::warn!("measure[{mode}] seq show i={i} no tray rect; skipped");
            return;
        };
        log::info!("measure[{mode}] seq show i={i} t_ms={:.1}", t_ms());
        panel::show_at(handle, rect, ShowReason::Toggle);
    };
    let Some(n) = cycles else {
        show(1);
        return;
    };
    for i in 1..=n {
        show(i);
        thread::sleep(SEQ_STEP);
        log::info!("measure[{mode}] seq hide i={i} t_ms={:.1}", t_ms());
        panel::hide(handle, HideReason::Toggle);
        thread::sleep(SEQ_STEP);
    }
    log::info!("measure[{mode}] seq done cycles={n} t_ms={:.1}", t_ms());
}

/// The page's report line. `fields` is the page's own `k=v k=v …` text, passed through, so a
/// measurement can add a field without a Rust change; `t_page` is the page's `performance.now()`
/// at the report.
#[tauri::command]
pub fn measure_report(kind: String, fields: String, t_page: f64) {
    let mode = prefix();
    log::info!(
        "measure[{mode}] {kind} {fields} t_page_ms={t_page:.1} t_ms={:.1}",
        t_ms()
    );
}
