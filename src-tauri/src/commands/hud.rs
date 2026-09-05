//! Commands for the always-visible idle pill.
//!
//! The pill lives in the existing recording-overlay window (see
//! [`crate::overlay`]); these commands are only the settings toggle and the two
//! interactions the pill itself dispatches.

use crate::modes;
use crate::overlay;
use crate::settings::{self, OverlayPosition};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager};

/// Everything the idle pill needs to render itself.
///
/// The pill runs in the overlay webview, which has no settings store of its own,
/// so it asks for this once on mount and again whenever the backend tells it the
/// mode changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct HudPillState {
    pub enabled: bool,
    pub position: OverlayPosition,
    /// Name of the mode a click would record under. `None` when no mode
    /// resolves, which is also the state in which a click does nothing.
    pub mode_name: Option<String>,
    pub mode_id: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn hud_pill_state(app: AppHandle) -> HudPillState {
    let settings = settings::get_settings(&app);
    let mode = modes::active_mode(&settings);
    HudPillState {
        enabled: settings.hud_pill_enabled,
        position: settings.hud_pill_position,
        mode_name: mode.map(|mode| mode.name.clone()),
        mode_id: mode.map(|mode| mode.id.clone()),
    }
}

#[tauri::command]
#[specta::specta]
pub fn set_hud_pill_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.hud_pill_enabled = enabled;
    })?;
    overlay::sync_hud_pill(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn set_hud_pill_position(app: AppHandle, position: OverlayPosition) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.hud_pill_position = position;
    })?;
    overlay::sync_hud_pill(&app);
    Ok(())
}

/// A click on the pill. Routed through the same intent channel as the tray, the
/// CLI, and the global shortcut, so the pill cannot start a recording by a path
/// the rest of the app does not already have.
#[tauri::command]
#[specta::specta]
pub fn hud_toggle_recording(app: AppHandle) {
    crate::signal_handle::send_transcription_intent(
        &app,
        modes::TranscriptionIntent::ActiveMode,
        "hud-pill",
    );
}

/// A right-click on the pill: pick the active mode from a native menu.
///
/// Building the menu here rather than in the webview keeps it a real OS menu,
/// which is what a persistent desktop affordance should have, and avoids giving
/// the non-activating overlay panel a focusable popup it cannot host.
#[tauri::command]
#[specta::specta]
pub fn hud_open_mode_menu(app: AppHandle) -> Result<(), String> {
    use tauri::menu::{Menu, MenuItem};

    let settings = settings::get_settings(&app);
    let active_id = modes::active_mode(&settings).map(|mode| mode.id.clone());
    let menu = Menu::new(&app).map_err(|error| error.to_string())?;
    for mode in &settings.modes {
        let checked = active_id.as_deref() == Some(mode.id.as_str());
        let label = if checked {
            format!("✓ {}", mode.name)
        } else {
            mode.name.clone()
        };
        let item = MenuItem::with_id(
            &app,
            format!("{HUD_MODE_MENU_PREFIX}{}", mode.id),
            label,
            !checked,
            None::<&str>,
        )
        .map_err(|error| error.to_string())?;
        menu.append(&item).map_err(|error| error.to_string())?;
    }

    let window = app
        .get_webview_window("recording_overlay")
        .ok_or_else(|| "recording overlay window is not available".to_string())?;
    window
        .popup_menu(&menu)
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Menu-id prefix for the pill's mode entries. Dispatch lives with the other
/// menu handlers in `lib.rs`.
pub const HUD_MODE_MENU_PREFIX: &str = "hud_mode:";
