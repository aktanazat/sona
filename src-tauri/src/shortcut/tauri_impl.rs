//! Tauri global-shortcut implementation
//!
//! This module provides shortcut functionality using Tauri's built-in
//! global-shortcut plugin.

use log::{debug, error, warn};
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::settings::{self, ShortcutBinding};

use super::handler::{handle_shortcut_event, ShortcutIntent};

/// Initialize shortcuts using Tauri's global-shortcut plugin
pub fn init_shortcuts(app: &AppHandle) {
    let user_settings = settings::load_or_create_app_settings(app);

    // Persisted bindings are the single source of truth, including mode keys.
    for binding in super::bindings_for_registration(&user_settings) {
        let id = binding.id.clone();
        if let Err(e) = register_shortcut(app, binding) {
            error!("Failed to register shortcut {id} during init: {e}");
        }
    }
}

/// Validate a shortcut string for the Tauri global-shortcut implementation.
/// Tauri requires at least one non-modifier key and doesn't support the fn key.
pub fn validate_shortcut(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("Shortcut cannot be empty".into());
    }

    let modifiers = [
        "ctrl", "control", "shift", "alt", "option", "meta", "command", "cmd", "super", "win",
        "windows",
    ];

    // Check for fn key which Tauri doesn't support
    let parts: Vec<String> = raw.split('+').map(|p| p.trim().to_lowercase()).collect();
    for part in &parts {
        if part == "fn" || part == "function" {
            return Err("The 'fn' key is not supported by Tauri global shortcuts".into());
        }
    }

    // Check for at least one non-modifier key
    let has_non_modifier = parts.iter().any(|part| !modifiers.contains(&part.as_str()));

    if !has_non_modifier {
        return Err("Tauri shortcuts must include a main key (letter, number, F-key, etc.) in addition to modifiers".into());
    }

    // The accelerator parser is the authority on what this backend can
    // register, and it is stricter than any name table: it has no
    // side-specific modifiers, so `option_left+space` — which is exactly what
    // the handy-keys recorder persists for a left-option chord — reads as a
    // bare main key above and would pass. Asking the parser here is what
    // makes the implementation switch reset such a chord and report it,
    // rather than failing to register it and saying nothing.
    raw.parse::<Shortcut>()
        .map(|_| ())
        .map_err(|e| format!("Tauri global shortcuts cannot parse '{raw}': {e}"))
}

fn handle_registered_shortcut_event(
    registered: &Shortcut,
    event_shortcut: &Shortcut,
    state: ShortcutState,
    binding_id: &str,
    dispatch: impl FnOnce(&str, &str, bool) -> Option<ShortcutIntent>,
) -> Option<ShortcutIntent> {
    if event_shortcut != registered {
        return None;
    }

    let shortcut_label = event_shortcut.into_string();
    let is_pressed = state == ShortcutState::Pressed;
    debug!(
        "tauri global-shortcut event: binding={binding_id}, shortcut={shortcut_label}, state={state:?}"
    );
    dispatch(binding_id, &shortcut_label, is_pressed)
}

/// Register a shortcut using Tauri's global-shortcut plugin
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    // Validate for Tauri requirements
    if let Err(e) = validate_shortcut(&binding.current_binding) {
        warn!(
            "register_tauri_shortcut validation error for binding '{}': {}",
            binding.current_binding, e
        );
        return Err(e);
    }

    // Parse shortcut and return error if it fails
    let shortcut = match binding.current_binding.parse::<Shortcut>() {
        Ok(s) => s,
        Err(e) => {
            let error_msg = format!(
                "Failed to parse shortcut '{}': {}",
                binding.current_binding, e
            );
            error!("register_tauri_shortcut parse error: {}", error_msg);
            return Err(error_msg);
        }
    };

    // Prevent duplicate registrations that would silently shadow one another
    if app.global_shortcut().is_registered(shortcut) {
        let error_msg = format!("Shortcut '{}' is already in use", binding.current_binding);
        warn!("register_tauri_shortcut duplicate error: {}", error_msg);
        return Err(error_msg);
    }

    // Clone binding.id for use in the closure
    let binding_id_for_closure = binding.id.clone();

    app.global_shortcut()
        .on_shortcut(shortcut, move |app_handle, scut, event| {
            let _ = handle_registered_shortcut_event(
                &shortcut,
                scut,
                event.state,
                &binding_id_for_closure,
                |binding_id, shortcut, is_pressed| {
                    handle_shortcut_event(app_handle, binding_id, shortcut, is_pressed)
                },
            );
        })
        .map_err(|e| {
            let error_msg = format!(
                "Couldn't register shortcut '{}': {}",
                binding.current_binding, e
            );
            error!("register_tauri_shortcut registration error: {}", error_msg);
            error_msg
        })?;

    Ok(())
}

/// Unregister a shortcut from Tauri's global-shortcut plugin
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let shortcut = match binding.current_binding.parse::<Shortcut>() {
        Ok(s) => s,
        Err(e) => {
            let error_msg = format!(
                "Failed to parse shortcut '{}' for unregistration: {}",
                binding.current_binding, e
            );
            error!("unregister_tauri_shortcut parse error: {}", error_msg);
            return Err(error_msg);
        }
    };

    app.global_shortcut().unregister(shortcut).map_err(|e| {
        let error_msg = format!(
            "Failed to unregister shortcut '{}': {}",
            binding.current_binding, e
        );
        error!("unregister_tauri_shortcut error: {}", error_msg);
        error_msg
    })?;

    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::super::{
        bindings_for_registration,
        handler::{shortcut_intent, ShortcutIntent},
    };
    use super::{handle_registered_shortcut_event, Shortcut, ShortcutState};
    use crate::command_mode::COMMAND_BINDING_ID;
    use crate::modes::{ensure_mode_settings, TranscriptionIntent};
    use crate::settings::get_default_settings;

    #[test]
    fn registered_macos_start_chords_deliver_one_coordinator_intent_each() {
        let mut settings = get_default_settings();
        ensure_mode_settings(&mut settings);
        let bindings = bindings_for_registration(&settings);
        let cases = [
            (COMMAND_BINDING_ID, TranscriptionIntent::Command),
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
        ];

        for (binding_id, expected) in cases {
            let binding = bindings
                .iter()
                .find(|binding| binding.id.as_str() == binding_id)
                .expect("start chord is registered");
            let registered = binding
                .current_binding
                .parse::<Shortcut>()
                .expect("parse registered shortcut");
            let mut coordinator_intents = Vec::new();
            let returned = handle_registered_shortcut_event(
                &registered,
                &registered,
                ShortcutState::Pressed,
                binding_id,
                |binding_id, _, is_pressed| {
                    assert!(is_pressed);
                    let intent = shortcut_intent(binding_id);
                    if let Some(ShortcutIntent::StartStop(intent)) = &intent {
                        coordinator_intents.push(intent.clone());
                    }
                    intent
                },
            );

            assert_eq!(coordinator_intents, vec![expected.clone()]);
            assert_eq!(returned, Some(ShortcutIntent::StartStop(expected)));
        }
    }

    #[test]
    fn unrelated_shortcut_event_is_not_dispatched() {
        let registered = "option+shift+space"
            .parse::<Shortcut>()
            .expect("parse registered shortcut");
        let unrelated = "option+shift+2"
            .parse::<Shortcut>()
            .expect("parse unrelated shortcut");
        let mut dispatched = 0;
        let returned = handle_registered_shortcut_event(
            &registered,
            &unrelated,
            ShortcutState::Pressed,
            COMMAND_BINDING_ID,
            |_, _, _| {
                dispatched += 1;
                shortcut_intent(COMMAND_BINDING_ID)
            },
        );

        assert_eq!(returned, None);
        assert_eq!(dispatched, 0);
    }
}
