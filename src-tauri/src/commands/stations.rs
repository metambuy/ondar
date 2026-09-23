//! Station directory commands. Thin: validate nothing the service does not validate itself,
//! forward to `StationsHandle`, map the error. Every one is `async` and awaits a reply from
//! the service's DB thread — a command never blocks the main thread, and a fetch in flight
//! never delays a cache read or a store call (M3a plan, F3). Results carry their provenance
//! (`source`, `age_secs`, `refreshing`), so the page can be honest about an expired list.

use tauri::State;

use ondar_stations::{ListedCountries, ListedStations, Station};

use crate::AppState;
use crate::error::OndarError;

#[tauri::command]
pub async fn list_countries(state: State<'_, AppState>) -> Result<ListedCountries, OndarError> {
    Ok(state.stations.list_countries().await?)
}

/// `country_code`: ISO 3166-1 alpha-2, any case.
#[tauri::command]
pub async fn list_stations(
    state: State<'_, AppState>,
    country_code: String,
) -> Result<ListedStations, OndarError> {
    Ok(state.stations.list_stations(&country_code).await?)
}

/// Server-side search (offline: a local search over the cached lists), ranked, uncapped.
#[tauri::command]
pub async fn search_stations(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<Station>, OndarError> {
    if query.trim().is_empty() {
        return Err(OndarError::InvalidArgument("query is empty".into()));
    }
    Ok(state.stations.search_stations(&query).await?)
}

#[tauri::command]
pub async fn list_favourites(state: State<'_, AppState>) -> Result<Vec<Station>, OndarError> {
    Ok(state.stations.list_favourites().await?)
}

#[tauri::command]
pub async fn add_favourite(state: State<'_, AppState>, station: Station) -> Result<(), OndarError> {
    Ok(state.stations.add_favourite(station).await?)
}

#[tauri::command]
pub async fn remove_favourite(
    state: State<'_, AppState>,
    uuid: String,
) -> Result<bool, OndarError> {
    Ok(state.stations.remove_favourite(&uuid).await?)
}

#[tauri::command]
pub async fn list_recents(state: State<'_, AppState>) -> Result<Vec<Station>, OndarError> {
    Ok(state.stations.list_recents().await?)
}
