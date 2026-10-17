//! The in-app update (M6-lite commit 4b). One check per launch, in Rust, a few seconds after
//! start; the page renders one line from the `update:available` event and reports a click with
//! `update_install`, which downloads, verifies and swaps the bundle (`tauri-plugin-updater`
//! 2.13.3) and restarts the app. The page decides nothing: not whether to check, not what
//! version it runs, not whether the install worked (a failure is the command's `Err`; a success
//! never returns). A failed check is a `warn!` and nothing on screen (ONDAR.md, "Updater").
//!
//! Debug builds skip the check unless `ONDAR_UPDATE_CHECK=1`: `pnpm tauri:dev` is not a tester,
//! and the dev loop must not depend on the feed. Both paths are logged.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::error::OndarError;
use crate::events;

/// How long after `setup` the check runs: after the tray, the panel and the first station list,
/// so a slow feed never delays what a launch is for.
pub const CHECK_DELAY: Duration = Duration::from_secs(5);

/// Download progress is logged once per this many bytes, not per chunk.
const LOG_EVERY_BYTES: usize = 1 << 20;

/// Payload of `update:available`: the version announced and its notes (empty when the feed
/// carries none). The page shows `Ondar {version} available`.
#[derive(Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct UpdateAvailable {
    pub version: String,
    pub notes: String,
}

/// The update the check found, held for the command. Managed state; `None` until a check
/// succeeds, and again while an install runs (put back if it fails, so a retry is possible).
#[derive(Default)]
pub struct UpdateState {
    pending: Mutex<Option<Update>>,
}

impl UpdateState {
    fn take(&self) -> Option<Update> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    fn put(&self, update: Update) {
        *self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(update);
    }
}

/// Whether this launch checks the feed: always in a release build; in a debug build only with
/// `ONDAR_UPDATE_CHECK=1`.
pub fn check_enabled() -> bool {
    if cfg!(debug_assertions) {
        std::env::var("ONDAR_UPDATE_CHECK").is_ok_and(|v| v == "1")
    } else {
        true
    }
}

/// Manages the state and schedules the one check. Called from `setup`, once.
pub fn setup(app: &AppHandle) {
    app.manage(UpdateState::default());
    if !check_enabled() {
        log::info!("update check skipped: debug build without ONDAR_UPDATE_CHECK=1");
        return;
    }
    log::info!(
        "update check scheduled in {} s (version {})",
        CHECK_DELAY.as_secs(),
        env!("CARGO_PKG_VERSION")
    );
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CHECK_DELAY).await;
        check(&app).await;
    });
}

/// The check. The builder is pre-filled from `tauri.conf.json` (`plugins.updater`: the endpoint
/// with `{{target}}`, `{{arch}}` and `{{current_version}}` replaced by the plugin, and the
/// pubkey). `Ok(Some)` stores the update and tells the page; `Ok(None)` (204, or nothing newer
/// than the running version) and `Err` are log lines only.
async fn check(app: &AppHandle) {
    let updater = match app.updater_builder().build() {
        Ok(u) => u,
        Err(e) => {
            log::warn!("update check not run: {e}");
            return;
        }
    };
    match updater.check().await {
        Ok(Some(update)) => {
            log::info!(
                "update available: {} (running {})",
                update.version,
                update.current_version
            );
            let payload = UpdateAvailable {
                version: update.version.clone(),
                notes: update.body.clone().unwrap_or_default(),
            };
            app.state::<UpdateState>().put(update);
            if let Err(e) = app.emit(events::UPDATE_AVAILABLE, payload) {
                log::warn!("failed to emit {}: {e}", events::UPDATE_AVAILABLE);
            }
        }
        Ok(None) => log::info!("no update: {} is current", env!("CARGO_PKG_VERSION")),
        Err(e) => log::warn!("update check failed: {e}"),
    }
}

/// The install, for the `update_install` command: download (progress logged per MiB), verify
/// the signature, swap the bundle in place, restart. Returns only on failure: no pending update
/// (`code: "updater"`), or the plugin's error, logged at ERROR with the update kept for a retry.
/// `AppHandle::restart` from this task (the async runtime, not the main thread) requests an
/// exit with `RESTART_EXIT_CODE`; the run loop exits, relaunches the bundle and ends the
/// process (tauri 2.12.3 `app.rs:606-629`, measured with `ONDAR_MEASURE_RESTART`).
pub async fn install(app: &AppHandle) -> Result<(), OndarError> {
    let state = app.state::<UpdateState>();
    let Some(update) = state.take() else {
        return Err(OndarError::Updater("no update pending".into()));
    };
    log::info!(
        "update install: {} from {}",
        update.version,
        update.download_url
    );
    let mut received = 0usize;
    let mut logged = 0usize;
    let result = update
        .download_and_install(
            |chunk, total| {
                received += chunk;
                if received - logged >= LOG_EVERY_BYTES {
                    logged = received;
                    log::debug!("update download {received} of {total:?} bytes");
                }
            },
            || log::info!("update downloaded; installing"),
        )
        .await;
    match result {
        Ok(()) => {
            log::info!("update installed; restarting");
            app.restart()
        }
        Err(e) => {
            log::error!("update install failed: {e}");
            state.put(update);
            Err(OndarError::Updater(e.to_string()))
        }
    }
}
