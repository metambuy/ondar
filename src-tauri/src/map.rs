//! The map resource (M4a): loaded once at startup, on its own thread, from
//! `resource_dir()/map/world.ondarmap` — beside the binary in `pnpm tauri:dev` (tauri-build copies
//! `bundle.resources` there) and in `Contents/Resources` in the bundle. One log line either way:
//! `map resource loaded path=… bytes=… units=… countries=… ms=…`, or `map resource unavailable
//! reason=…` — the app runs without a map (M4b's command will answer "no map"). No command reads
//! it yet.

use ondar_map::format::Store;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tauri::Manager;

/// The loaded store, set once by the loader thread; `None` inside if the load failed.
#[derive(Clone, Default)]
pub struct MapState(pub Arc<OnceLock<Option<Store>>>);

/// The resource's path under a resource directory.
pub fn resource_path(resource_dir: &Path) -> PathBuf {
    resource_dir.join("map").join("world.ondarmap")
}

/// Reads and loads the resource: the store and its byte count, or why not.
pub fn load(path: &Path) -> Result<(Store, usize), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let n = bytes.len();
    Store::load(&bytes)
        .map(|s| (s, n))
        .map_err(|e| e.to_string())
}

/// Manages the state and starts the loader thread.
pub fn setup(app: &tauri::App) {
    let state = MapState::default();
    app.manage(state.clone());
    let dir = app.path().resource_dir();
    let spawned = std::thread::Builder::new()
        .name("ondar-map-load".into())
        .spawn(move || {
            let t0 = Instant::now();
            let result = dir
                .map_err(|e| format!("no resource directory: {e}"))
                .map(|d| resource_path(&d))
                .and_then(|p| load(&p).map(|ok| (p, ok)));
            match result {
                Ok((path, (store, bytes))) => {
                    log::info!(
                        "map resource loaded path={} bytes={bytes} units={} countries={} ms={:.1}",
                        path.display(),
                        store.units.len(),
                        store.countries.len(),
                        t0.elapsed().as_secs_f64() * 1e3
                    );
                    let _ = state.0.set(Some(store));
                }
                Err(reason) => {
                    log::warn!("map resource unavailable reason={reason}");
                    let _ = state.0.set(None);
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("map resource unavailable reason=no loader thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped resource loads through the shell's path rule; a missing file and a file that
    /// is not a resource are reasons, not panics.
    #[test]
    fn the_resource_loads_and_failures_are_reasons() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        let (store, bytes) = load(&resource_path(&dir)).unwrap();
        assert_eq!((store.units.len(), store.countries.len()), (267, 248));
        assert!(bytes > 1_000_000);
        let missing = load(Path::new("/nonexistent/map/world.ondarmap")).unwrap_err();
        assert!(missing.starts_with("read:"), "{missing}");
        let bad = load(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("Cargo.toml")
                .as_path(),
        )
        .unwrap_err();
        assert!(bad.contains("bad magic"), "{bad}");
    }
}
