//! Tells a running Sona that `sona work` changed its data from another process.
//!
//! The command line runs in a process of its own, so the events its writes
//! emit reach nobody. It leaves a stamp in the data folder instead, and the
//! app's core, which watches the folder, raises the same events the app's own
//! writes do. A settings write leaves a stamp of its own: the app holds its
//! settings in memory, so it reads the file again and follows the new values
//! the way it follows a change made in its own window.

use crate::planning::work::types::WorkChanged;
use crate::settings::AppSettings;
use serde_json::json;
use std::io;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};
use tauri_specta::Event;

const STAMP: &str = ".external-write";
const SETTINGS_STAMP: &str = ".external-settings-write";
/// In the order the watcher follows them: settings first.
const STAMPS: [&str; 2] = [SETTINGS_STAMP, STAMP];

fn now_nanos() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |elapsed| elapsed.as_nanos().to_string())
}

/// Written beside the stamp and renamed over it: a rename changes the folder,
/// which is what the watcher waits on, and a reader never sees half a stamp.
fn write_stamp(directory: &Path, name: &str) -> io::Result<()> {
    let staged = directory.join(format!("{name}.{}", std::process::id()));
    std::fs::write(&staged, now_nanos())?;
    std::fs::rename(&staged, directory.join(name))
}

fn stamp(app: &AppHandle, name: &str) {
    let Ok(directory) = crate::portable::app_data_dir(app) else {
        return;
    };
    if let Err(error) = write_stamp(&directory, name) {
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
    let Ok(directory) = crate::portable::app_data_dir(&app) else {
        return;
    };
    let watcher = match StampWatcher::open(directory) {
        Ok(watcher) => watcher,
        Err(error) => {
            log::warn!("The change stamp watcher did not start: {error}");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("external-write-watch".into())
        .spawn(move || {
            let stopped = watcher.run(|name| {
                if name == SETTINGS_STAMP {
                    follow_settings(&app);
                } else {
                    announce(&app);
                }
            });
            if let Err(error) = stopped {
                log::warn!("The change stamp watcher stopped: {error}");
            }
        });
    if let Err(error) = spawned {
        log::warn!("The change stamp watcher did not start: {error}");
    }
}

struct StampWatcher {
    directory: PathBuf,
    seen: [Option<String>; 2],
    folder: FolderChanges,
}

impl StampWatcher {
    fn open(directory: PathBuf) -> io::Result<Self> {
        let folder = FolderChanges::open(&directory)?;
        let seen = STAMPS.map(|name| std::fs::read_to_string(directory.join(name)).ok());
        Ok(Self {
            directory,
            seen,
            folder,
        })
    }

    /// Calls `changed` with each stamp whose contents moved, for as long as
    /// the folder can be watched.
    fn run(mut self, mut changed: impl FnMut(&str)) -> io::Result<()> {
        loop {
            self.folder.wait()?;
            for (name, seen) in STAMPS.into_iter().zip(&mut self.seen) {
                let stamp = std::fs::read_to_string(self.directory.join(name)).ok();
                if stamp != *seen {
                    *seen = stamp;
                    changed(name);
                }
            }
        }
    }
}

/// Wakes when an entry in one folder is added, removed or renamed.
#[cfg(target_os = "macos")]
struct FolderChanges {
    queue: std::os::fd::OwnedFd,
    _folder: std::fs::File,
}

#[cfg(target_os = "macos")]
impl FolderChanges {
    fn open(directory: &Path) -> io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let folder = std::fs::File::open(directory)?;
        // SAFETY: kqueue takes no pointers and returns a new descriptor.
        let descriptor = unsafe { libc::kqueue() };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this fresh, nonnegative descriptor has no other owner.
        let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let change = libc::kevent {
            ident: folder
                .as_raw_fd()
                .try_into()
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: the change outlives this synchronous call; no events are read.
        let result = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            queue,
            _folder: folder,
        })
    }

    fn wait(&self) -> io::Result<()> {
        use std::os::fd::AsRawFd;

        loop {
            // SAFETY: a zeroed kevent is a valid value of this plain C struct.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            // SAFETY: one writable event; a null timeout waits for it.
            let result = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    std::ptr::null(),
                )
            };
            if result >= 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

/// Without kqueue the folder is read again on a timer.
#[cfg(not(target_os = "macos"))]
struct FolderChanges;

#[cfg(not(target_os = "macos"))]
impl FolderChanges {
    fn open(_directory: &Path) -> io::Result<Self> {
        Ok(Self)
    }

    fn wait(&self) -> io::Result<()> {
        std::thread::sleep(std::time::Duration::from_millis(700));
        Ok(())
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
    if before.selected_microphone != after.selected_microphone
        || before.clamshell_microphone != after.clamshell_microphone
        || before.microphone_preferences != after.microphone_preferences
    {
        if let Some(recorder) =
            app.try_state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
        {
            if let Err(error) = recorder.update_selected_device() {
                log::warn!("Microphone preferences will apply to the next capture: {error}");
            }
        }
    }
    crate::shortcut::reconcile_mode_shortcuts(app, &before.bindings, &after.bindings);
    crate::modes::emit_modes_changed(app, &crate::modes::mode_settings_snapshot(&after));
    if before.theme != after.theme {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        crate::shortcut::apply_window_theme(app, after.theme);
        let _ = app.emit("theme-changed", after.theme);
    }
    if before.appearance_material != after.appearance_material {
        let effective = crate::shortcut::apply_window_material(app, after.appearance_material);
        let _ = app.emit(
            "settings-changed",
            json!({ "setting": "appearance_material", "value": effective.as_str() }),
        );
    }
    if before.hud_pill_enabled != after.hud_pill_enabled
        || before.hud_pill_position != after.hud_pill_position
        || before.hud_pill_hidden_until_ms != after.hud_pill_hidden_until_ms
    {
        crate::overlay::sync_hud_pill(app);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_stamp_written_by_another_process_wakes_the_watcher() {
        let directory = tempfile::TempDir::new().expect("a data folder");
        write_stamp(directory.path(), STAMP).expect("an earlier stamp");
        let watcher = StampWatcher::open(directory.path().to_path_buf()).expect("a watched folder");
        let (changed, heard) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = watcher.run(|name| {
                let _ = changed.send(name.to_owned());
            });
        });

        write_stamp(directory.path(), STAMP).expect("a newer stamp over the earlier one");
        assert_eq!(
            heard.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok(STAMP)
        );
        write_stamp(directory.path(), SETTINGS_STAMP).expect("a first settings stamp");
        assert_eq!(
            heard.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok(SETTINGS_STAMP)
        );
    }
}
