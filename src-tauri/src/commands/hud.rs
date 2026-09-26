//! Commands for the always-visible idle pill.
//!
//! The pill lives in the existing recording-overlay window (see
//! [`crate::overlay`]); these commands are only the settings toggles, the
//! one-hour hide from its menu, and the two interactions the pill itself
//! dispatches.

use crate::modes;
use crate::overlay;
use crate::settings::{self, HudPillEdge};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager};

/// How long "Hide for an hour" keeps the idle pill off screen.
const HUD_PILL_HIDE_MS: i64 = 60 * 60 * 1000;

/// Everything the idle pill needs to render itself.
///
/// The pill runs in the overlay webview, which has no settings store of its own,
/// so it asks for this once on mount and again whenever the backend tells it the
/// mode changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct HudPillState {
    pub enabled: bool,
    pub position: HudPillEdge,
    /// The wall-clock time (milliseconds since the Unix epoch) the pill's
    /// one-hour hide ends, while one is running.
    pub hidden_until_ms: Option<i64>,
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
        hidden_until_ms: settings.hud_pill_hidden_until_ms,
        mode_name: mode.map(|mode| mode.name.clone()),
        mode_id: mode.map(|mode| mode.id.clone()),
    }
}

/// Turning the pill off or on ends a running one-hour hide as well: a pill
/// switched on is a pill asked for now, and one switched off has nothing left
/// to hide.
#[tauri::command]
#[specta::specta]
pub fn set_hud_pill_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.hud_pill_enabled = enabled;
        settings.hud_pill_hidden_until_ms = None;
    })?;
    overlay::sync_hud_pill(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn set_hud_pill_position(app: AppHandle, position: HudPillEdge) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.hud_pill_position = position;
    })?;
    overlay::sync_hud_pill(&app);
    Ok(())
}

/// "Hide for an hour" from the idle pill's menu. Answers the time the pill
/// comes back, in milliseconds since the Unix epoch. The pill returns by
/// itself then (see [`overlay::sync_hud_pill`]); a recording still shows the
/// bar meanwhile, and the hour survives a restart.
#[tauri::command]
#[specta::specta]
pub fn hide_hud_pill_for_an_hour(app: AppHandle) -> Result<i64, String> {
    let until_ms = utc_now_ms().saturating_add(HUD_PILL_HIDE_MS);
    settings::update_settings(&app, |settings| {
        settings.hud_pill_hidden_until_ms = Some(until_ms);
    })?;
    overlay::sync_hud_pill(&app);
    Ok(until_ms)
}

/// "Show now" in Dictation settings: ends the one-hour hide early.
#[tauri::command]
#[specta::specta]
pub fn show_hud_pill_now(app: AppHandle) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.hud_pill_hidden_until_ms = None;
    })?;
    overlay::sync_hud_pill(&app);
    Ok(())
}

fn utc_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
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
