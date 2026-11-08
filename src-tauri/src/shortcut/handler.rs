//! Shared shortcut event handling logic used by both shortcut backends.

use log::warn;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::managers::audio::AudioRecordingManager;
use crate::meeting::open_windows::{MeetingOpenWindowsService, ATTACH_WINDOW_BINDING_ID};
use crate::modes::{parse_mode_shortcut_id, ModeShortcutKind, TranscriptionIntent};
use crate::settings::get_settings;
use crate::TranscriptionCoordinator;

/// The only shortcut behaviors that can reach the application. Start/stop
/// resolution belongs to the coordinator because it owns recording state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ShortcutIntent {
    StartStop(TranscriptionIntent),
    Cancel,
    SwitchMode(String),
    AttachWindow,
}

pub(super) fn shortcut_intent(binding_id: &str) -> Option<ShortcutIntent> {
    // The cancel key is registered once bare and once per modifier prefix a
    // recording can be held by, each under its own id, so that an Escape
    // struck mid-hold is still cancel. Every one of them means cancel.
    if binding_id == "cancel" || binding_id.starts_with("cancel#") {
        return Some(ShortcutIntent::Cancel);
    }
    if binding_id == ATTACH_WINDOW_BINDING_ID {
        return Some(ShortcutIntent::AttachWindow);
    }
    if let Some((mode_id, ModeShortcutKind::Switch)) = parse_mode_shortcut_id(binding_id) {
        return Some(ShortcutIntent::SwitchMode(mode_id));
    }
    TranscriptionIntent::from_binding(binding_id).map(ShortcutIntent::StartStop)
}

/// Handle a shortcut event from either implementation.
pub(super) fn handle_shortcut_event(
    app: &AppHandle,
    binding_id: &str,
    shortcut: &str,
    is_pressed: bool,
) -> Option<ShortcutIntent> {
    let Some(intent) = shortcut_intent(binding_id) else {
        warn!("No typed shortcut intent for '{binding_id}'");
        return None;
    };

    match &intent {
        ShortcutIntent::SwitchMode(mode_id) => {
            if is_pressed {
                if let Err(error) = crate::modes::set_active_mode(app.clone(), mode_id.clone()) {
                    warn!("Could not activate mode from shortcut '{binding_id}': {error}");
                }
            }
        }
        ShortcutIntent::StartStop(transcription_intent) => {
            let settings = get_settings(app);
            if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
                coordinator.send_shortcut_input(
                    transcription_intent.clone(),
                    shortcut,
                    is_pressed,
                    settings.push_to_talk,
                );
            } else {
                warn!("TranscriptionCoordinator is not initialized");
            }
        }
        ShortcutIntent::Cancel => {
            let audio_manager = app.state::<Arc<AudioRecordingManager>>();
            if is_pressed && audio_manager.is_recording() {
                crate::utils::cancel_current_operation(app);
            }
        }
        ShortcutIntent::AttachWindow => {
            if is_pressed {
                if let Some(service) = app.try_state::<Arc<MeetingOpenWindowsService>>() {
                    let service = Arc::clone(service.inner());
                    tauri::async_runtime::spawn(async move { service.attach_front_window().await });
                } else {
                    warn!("MeetingOpenWindowsService is not initialized");
                }
            }
        }
    }

    Some(intent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_shortcut_ids_select_their_coordinator_intents() {
        for (binding_id, expected) in [
            ("transcribe", TranscriptionIntent::ActiveMode),
            ("command", TranscriptionIntent::Command),
            (
                "mode/email/transcribe",
                TranscriptionIntent::Mode {
                    mode_id: "email".to_string(),
                },
            ),
            (
                "mode/meeting/transcribe",
                TranscriptionIntent::Mode {
                    mode_id: "meeting".to_string(),
                },
            ),
            (
                "mode/notes/transcribe",
                TranscriptionIntent::Mode {
                    mode_id: "notes".to_string(),
                },
            ),
        ] {
            assert_eq!(
                shortcut_intent(binding_id),
                Some(ShortcutIntent::StartStop(expected))
            );
        }
    }

    #[test]
    fn cancel_switch_and_unknown_ids_stay_outside_start_dispatch() {
        assert_eq!(
            shortcut_intent("mode/email/switch"),
            Some(ShortcutIntent::SwitchMode("email".to_string()))
        );
        assert_eq!(shortcut_intent("cancel"), Some(ShortcutIntent::Cancel));
        assert_eq!(
            shortcut_intent("cancel#option+shift"),
            Some(ShortcutIntent::Cancel)
        );
        assert_eq!(
            shortcut_intent(ATTACH_WINDOW_BINDING_ID),
            Some(ShortcutIntent::AttachWindow)
        );
        assert_eq!(shortcut_intent("test"), None);
    }
}
