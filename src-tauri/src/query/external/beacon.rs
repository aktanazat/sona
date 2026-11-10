//! Tells a running Sona that `sona work` changed its data from another process.
//!
//! The command line runs in a process of its own, so the events its writes
//! emit reach nobody. It leaves a stamp in the data folder instead, and the
//! app's core, which polls the stamp, raises the same events the app's own
//! writes do.

use crate::planning::work::types::WorkChanged;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_specta::Event;

const STAMP: &str = ".external-write";
const POLL: Duration = Duration::from_millis(700);

fn stamp_path(app: &AppHandle) -> Option<PathBuf> {
    crate::portable::app_data_dir(app)
        .ok()
        .map(|dir| dir.join(STAMP))
}

fn now_nanos() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |elapsed| elapsed.as_nanos().to_string())
}

pub(super) fn leave(app: &AppHandle) {
    let Some(path) = stamp_path(app) else { return };
    if let Err(error) = std::fs::write(&path, now_nanos()) {
        log::warn!("The change stamp could not be written: {error}");
    }
}

pub(crate) fn watch(app: AppHandle) {
    let Some(path) = stamp_path(&app) else { return };
    let spawned = std::thread::Builder::new()
        .name("external-write-watch".into())
        .spawn(move || {
            let mut seen = std::fs::read_to_string(&path).ok();
            loop {
                std::thread::sleep(POLL);
                let stamp = std::fs::read_to_string(&path).ok();
                if stamp == seen {
                    continue;
                }
                seen = stamp;
                announce(&app);
            }
        });
    if let Err(error) = spawned {
        log::warn!("The change stamp watcher did not start: {error}");
    }
}

fn announce(app: &AppHandle) {
    if (WorkChanged { ids: Vec::new() }).emit(app).is_err() {
        log::warn!("Work change event could not be delivered");
    }
    crate::planning::changed_everywhere(app);
    let _ = app.emit(
        "meeting:artifact-changed",
        json!({ "event_schema_version": 1, "session_id": null, "revision": 0 }),
    );
}
