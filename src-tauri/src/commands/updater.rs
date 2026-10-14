//! The one update command. The page reports a click on the line's button; Rust does the rest
//! (`crate::updater::install`). A success never answers: the app restarts. A failure answers
//! `{ code: "updater", message }`, and the page shows "Update failed, see log".

use tauri::AppHandle;

use crate::error::OndarError;

#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), OndarError> {
    crate::updater::install(&app).await
}
