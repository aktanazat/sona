//! Tells a running Sona that `sona work` changed its data from another process.
//!
//! The command line runs in a process of its own, so the events its writes
//! emit reach nobody. It leaves a stamp in the data folder instead, and the
//! app's core, which polls the stamp, raises the same events the app's own
//! writes do. A settings write leaves a stamp of its own: the app holds its
//! settings in memory, so it reads the file again and follows the new values
//! the way it follows a change made in its own window.

use crate::planning::work::types::WorkChanged;
use crate::settings::AppSettings;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_specta::Event;

const STAMP: &str = ".external-write";
const SETTINGS_STAMP: &str = ".external-settings-write";
const POLL: Duration = Duration::from_millis(700);

fn stamp_path(app: &AppHandle, name: &str) -> Option<PathBuf> {
    crate::portable::app_data_dir(app)
        .ok()
        .map(|dir| dir.join(name))
}

fn now_nanos() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |elapsed| elapsed.as_nanos().to_string())
}

fn stamp(app: &AppHandle, name: &str) {
    let Some(path) = stamp_path(app, name) else { return };
    if let Err(error) = std::fs::write(&path, now_nanos()) {
        log::warn!("The change stamp could not be written: {error}");
    }
}

pub(super) fn leave(app: &AppHandle) {
    stamp(app, STAMP);
}

pub(super) fn leave_settings(app: &AppHandle) {
    stamp(app, SETTINGS_STAMP);
}

pub(crate) fn watch(app: AppHandle) {
    let (Some(work), Some(settings)) = (stamp_path(&app, STAMP), stamp_path(&app, SETTINGS_STAMP))
    else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("external-write-watch".into())
        .spawn(move || {
            let mut seen = std::fs::read_to_string(&work).ok();
            let mut seen_settings = std::fs::read_to_string(&settings).ok();
            loop {
                std::thread::sleep(POLL);
                let stamp = std::fs::read_to_string(&settings).ok();
                if stamp != seen_settings {
                    seen_settings = stamp;
                    follow_settings(&app);
                }
                let stamp = std::fs::read_to_string(&work).ok();
                if stamp != seen {
                    seen = stamp;
                    announce(&app);
                }
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

/// What the app's own setting commands do beside the write, done for the
/// values `sona work` changed.
fn follow_settings(app: &AppHandle) {
    let Some((before, after)) = crate::settings::reload_after_external_write(app) else {
        return;
    };
    crate::shortcut::reconcile_mode_shortcuts(app, &before.bindings, &after.bindings);
    crate::modes::emit_modes_changed(app, &crate::modes::mode_settings_snapshot(&after));
    if before.theme != after.theme {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        crate::shortcut::apply_window_theme(app, after.theme);
        let _ = app.emit("theme-changed", after.theme);
    }
    if before.overlay_style != after.overlay_style {
        crate::overlay::update_overlay_enabled_cache(
            after.overlay_style != crate::settings::OverlayStyle::None,
        );
        crate::overlay::update_overlay_position(app);
    }
    if before.app_language != after.app_language {
        crate::tray::update_tray_menu(app);
    }
    if before.log_level != after.log_level {
        follow_log_level(&after);
    }
    if before.post_process_provider_id != after.post_process_provider_id {
        crate::secure_input::reconcile_fallback(app);
    }
    if before.command_mode_enabled != after.command_mode_enabled {
        crate::shortcut::suspend_all_shortcuts(app);
        crate::shortcut::resume_all_shortcuts(app);
    }
    if before.quiet_speech_enabled != after.quiet_speech_enabled {
        if let Some(recorder) =
            app.try_state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
        {
            recorder.set_quiet_speech_enabled(after.quiet_speech_enabled);
        }
    }
}

fn follow_log_level(settings: &AppSettings) {
    let level: tauri_plugin_log::LogLevel = settings.log_level.into();
    let level: log::Level = level.into();
    crate::FILE_LOG_LEVEL.store(
        crate::level_filter_code(level.to_level_filter()),
        std::sync::atomic::Ordering::Relaxed,
    );
}
