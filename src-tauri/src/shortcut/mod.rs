//! Keyboard shortcut management module
//!
//! This module provides a unified interface for keyboard shortcuts with
//! multiple backend implementations:
//!
//! - `tauri`: Uses Tauri's built-in global-shortcut plugin
//! - `handy_keys`: Uses the handy-keys library for more control
//!
//! The active implementation is determined by the `keyboard_implementation`
//! setting and can be changed at runtime.

mod handler;
pub mod handy_keys;
pub mod tauri_impl;

use log::{debug, error, info, warn};
use serde::Serialize;
use specta::Type;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

use crate::secrets::{SecretAccount, SecretCommandError, SecretManager, SecretRead};
use crate::settings::{
    self, get_settings, AppSettings, AppearanceMaterial, EnglishSpelling, KeyboardImplementation,
    LLMPrompt, OverlayPosition, OverlayStyle, PostProcessCatalogSource, PostProcessModelCatalog,
    PostProcessModelDiscovery, PostProcessModelOption, ShortcutBinding, SoundTheme, Theme,
};
use crate::tray;

// Note: Commands are accessed via shortcut::handy_keys:: in lib.rs

/// Initialize shortcuts using the configured implementation
pub fn init_shortcuts(app: &AppHandle) {
    let user_settings = settings::load_or_create_app_settings(app);

    // Check which implementation to use
    match user_settings.keyboard_implementation {
        KeyboardImplementation::Tauri => {
            tauri_impl::init_shortcuts(app);
        }
        KeyboardImplementation::HandyKeys => {
            if let Err(e) = handy_keys::init_shortcuts(app) {
                error!("Failed to initialize handy-keys shortcuts: {}", e);
                // Fall back to Tauri implementation and persist this fallback
                warn!("Falling back to Tauri global shortcut implementation and saving fallback to settings");

                // Update settings to persist the fallback so we don't retry HandyKeys on next launch
                // Nothing in this function's shape can report a store failure; the settings seam logs it.
                let _ = settings::update_settings(app, |settings| {
                    settings.keyboard_implementation = KeyboardImplementation::Tauri;
                });

                tauri_impl::init_shortcuts(app);
            }
        }
    }
}

/// Register the cancel shortcut (called when recording starts)
pub fn register_cancel_shortcut(app: &AppHandle) {
    // Track recording lifecycle independently of the current implementation so
    // switching implementations mid-recording cannot leave stale fallback state.
    crate::secure_input::register_cancel_fallback(app);

    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_cancel_shortcut(app),
        KeyboardImplementation::HandyKeys => handy_keys::register_cancel_shortcut(app),
    }
}

/// Unregister the cancel shortcut (called when recording stops)
pub fn unregister_cancel_shortcut(app: &AppHandle) {
    crate::secure_input::unregister_cancel_fallback(app);

    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::unregister_cancel_shortcut(app),
        KeyboardImplementation::HandyKeys => handy_keys::unregister_cancel_shortcut(app),
    }
}

/// Register a shortcut using the appropriate implementation
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_shortcut(app, binding),
        KeyboardImplementation::HandyKeys => handy_keys::register_shortcut(app, binding),
    }
}

/// Unregister a shortcut using the appropriate implementation
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::unregister_shortcut(app, binding),
        KeyboardImplementation::HandyKeys => handy_keys::unregister_shortcut(app, binding),
    }
}

/// Return the full persisted registration set in a deterministic order. The
/// cancel key is registered only while a recording is active, and the command
/// chord only while command mode is on — a disabled feature must release its
/// chord to the rest of the system rather than swallow it.
pub(crate) fn bindings_for_registration(settings: &AppSettings) -> Vec<ShortcutBinding> {
    let mut bindings: Vec<_> = settings
        .bindings
        .iter()
        .filter(|(id, _)| id.as_str() != "cancel")
        .filter(|(id, _)| {
            settings.command_mode_enabled || id.as_str() != crate::command_mode::COMMAND_BINDING_ID
        })
        .map(|(_, binding)| binding.clone())
        .collect();
    bindings.sort_by(|left, right| left.id.cmp(&right.id));
    bindings
}

/// The cancel key's full registration set for a recording that is open.
///
/// The stored chord is a bare key, and both backends match a hotkey's
/// modifiers exactly — so an Escape struck while a push-to-talk chord is still
/// down arrives carrying that chord's modifiers and is not the cancel hotkey.
/// That left cancel deaf for exactly as long as a recording was held open,
/// which is when it is reached for. The key is therefore also registered under
/// each modifier prefix a recording can be held by: never a combination the
/// user has not already dedicated to Sona, and only for the length of a
/// recording.
///
/// Each variant carries its own id, because registrations are keyed by binding
/// id: reusing `cancel` would leak every variant but the last and leave its
/// chord swallowed for the life of the process.
pub(crate) fn cancel_bindings_for_registration(settings: &AppSettings) -> Vec<ShortcutBinding> {
    let Some(cancel) = settings.bindings.get("cancel") else {
        return Vec::new();
    };

    let mut prefixes: Vec<String> = settings
        .bindings
        .iter()
        .filter(|(id, _)| {
            settings.command_mode_enabled || id.as_str() != crate::command_mode::COMMAND_BINDING_ID
        })
        .filter(|(id, _)| crate::modes::TranscriptionIntent::from_binding(id).is_some())
        .filter_map(|(_, binding)| modifier_prefix(&binding.current_binding))
        .collect();
    prefixes.sort_unstable();
    prefixes.dedup();

    let mut bindings = Vec::with_capacity(prefixes.len() + 1);
    bindings.push(cancel.clone());
    bindings.extend(prefixes.into_iter().map(|prefix| ShortcutBinding {
        id: format!("cancel#{prefix}"),
        current_binding: format!("{prefix}+{}", cancel.current_binding),
        ..cancel.clone()
    }));
    bindings
}

/// The modifiers a key struck during `chord` arrives with, in the canonical
/// order the stored format uses. `None` when the chord holds no modifier,
/// which the bare cancel key already covers.
///
/// The stored format is handy-keys' own, so its parser is what decides which
/// parts are modifiers — the same authority `validate_shortcut` defers to,
/// rather than a second modifier-name table to keep in step with it.
fn modifier_prefix(chord: &str) -> Option<String> {
    let hotkey: ::handy_keys::Hotkey = chord.parse().ok()?;
    // A modifier-only hotkey renders as exactly the prefix, and an empty
    // modifier set cannot build one — so this rejects bare keys for free.
    ::handy_keys::Hotkey::new(hotkey.modifiers, None::<::handy_keys::Key>)
        .ok()
        .map(|modifiers| modifiers.to_handy_string())
}

// ============================================================================
// Binding Management Commands
// ============================================================================

#[derive(Serialize, Type)]
pub struct BindingResponse {
    success: bool,
    binding: Option<ShortcutBinding>,
    error: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn change_binding(
    app: AppHandle,
    id: String,
    binding: String,
) -> Result<BindingResponse, String> {
    // Reject empty bindings — every shortcut should have a value
    if binding.trim().is_empty() {
        return Err("Binding cannot be empty".to_string());
    }

    let settings = settings::get_settings(&app);

    // Get the binding to modify, or create it from defaults if it doesn't exist
    let binding_to_modify = match settings.bindings.get(&id) {
        Some(binding) => binding.clone(),
        None => {
            // Try to get the default binding for this id
            let default_settings = settings::get_default_settings();
            match default_settings.bindings.get(&id) {
                Some(default_binding) => {
                    warn!(
                        "Binding '{}' not found in settings, creating from defaults",
                        id
                    );
                    default_binding.clone()
                }
                None => {
                    let error_msg = format!("Binding with id '{}' not found in defaults", id);
                    warn!("change_binding error: {}", error_msg);
                    return Ok(BindingResponse {
                        success: false,
                        binding: None,
                        error: Some(error_msg),
                    });
                }
            }
        }
    };

    // If this is the cancel binding, just update the settings and return
    // It's managed dynamically, so we don't register/unregister here
    if id == "cancel" {
        let updated = settings::update_settings(&app, |settings| {
            settings
                .bindings
                .get(&id)
                .cloned()
                .map(|mut stored_binding| {
                    stored_binding.current_binding = binding.clone();
                    settings.bindings.insert(id.clone(), stored_binding.clone());
                    stored_binding
                })
        })?;
        if let Some(binding) = updated {
            crate::secure_input::reconcile_fallback(&app);
            return Ok(BindingResponse {
                success: true,
                binding: Some(binding),
                error: None,
            });
        }
    }

    // Unregister the existing binding
    if let Err(e) = unregister_shortcut(&app, binding_to_modify.clone()) {
        let error_msg = format!("Failed to unregister shortcut: {}", e);
        error!("change_binding error: {}", error_msg);
    }

    // Validate the new shortcut for the current keyboard implementation
    if let Err(e) = validate_shortcut_for_implementation(&binding, settings.keyboard_implementation)
    {
        warn!("change_binding validation error: {}", e);
        restore_registration(&app, &binding_to_modify);
        return Err(e);
    }

    // Create an updated binding
    let mut updated_binding = binding_to_modify.clone();
    updated_binding.current_binding = binding;

    // Register the new binding
    if let Err(e) = register_shortcut(&app, updated_binding.clone()) {
        let error_msg = format!("Failed to register shortcut: {}", e);
        error!("change_binding error: {}", error_msg);
        restore_registration(&app, &binding_to_modify);
        return Ok(BindingResponse {
            success: false,
            binding: None,
            error: Some(error_msg),
        });
    }

    // Save the settings and synchronize any active Secure Input shadows.
    settings::update_settings(&app, |settings| {
        settings.bindings.insert(id, updated_binding.clone());
    })?;
    crate::secure_input::reconcile_fallback(&app);

    // Return the updated binding
    Ok(BindingResponse {
        success: true,
        binding: Some(updated_binding),
        error: None,
    })
}

/// Best-effort re-register of the previous binding after a failed change,
/// so a failure leaves the user's shortcut working exactly as before.
fn restore_registration(app: &AppHandle, binding: &ShortcutBinding) {
    if let Err(e) = register_shortcut(app, binding.clone()) {
        error!(
            "Failed to restore previous binding '{}' ({}): {}",
            binding.id, binding.current_binding, e
        );
    }
}

#[tauri::command]
#[specta::specta]
pub fn reset_binding(app: AppHandle, id: String) -> Result<BindingResponse, String> {
    let binding = settings::get_stored_binding(&app, &id);
    change_binding(app, id, binding.default_binding)
}

/// Unregister every binding while the user is recording a new shortcut in
/// the UI, so no existing shortcut can fire — or swallow the keystrokes —
/// mid-capture. The "cancel" binding is untouched: it is managed dynamically
/// by the recording lifecycle.
pub fn suspend_all_shortcuts(app: &AppHandle) {
    for (id, binding) in settings::get_bindings(app) {
        if id == "cancel" {
            continue;
        }
        if let Err(e) = unregister_shortcut(app, binding) {
            debug!(
                "suspend_all_shortcuts: could not unregister '{}': {}",
                id, e
            );
        }
    }
}

/// Re-register every binding from settings after shortcut recording ends.
/// Registering an already-registered shortcut fails cleanly in both
/// implementations, so this is idempotent and safe on every exit path.
pub fn resume_all_shortcuts(app: &AppHandle) {
    let settings = get_settings(app);
    for binding in bindings_for_registration(&settings) {
        if let Err(e) = register_shortcut(app, binding.clone()) {
            debug!(
                "resume_all_shortcuts: could not register '{}': {}",
                binding.id, e
            );
        }
    }
}

/// Apply a mode edit to the live keyboard registration: unregister bindings
/// that disappeared or changed keys, then register the current ones. Both
/// backends reject duplicate registrations cleanly, so this is idempotent.
pub fn reconcile_mode_shortcuts(
    app: &AppHandle,
    previous: &std::collections::HashMap<String, ShortcutBinding>,
    current: &std::collections::HashMap<String, ShortcutBinding>,
) {
    for (id, binding) in previous {
        if id == "cancel" {
            continue;
        }
        if current.get(id) == Some(binding) {
            continue;
        }
        if let Err(e) = unregister_shortcut(app, binding.clone()) {
            debug!("reconcile_mode_shortcuts: could not unregister '{id}': {e}");
        }
    }

    for (id, binding) in current {
        if id == "cancel" {
            continue;
        }
        if previous.get(id) == Some(binding) {
            continue;
        }
        if let Err(e) = register_shortcut(app, binding.clone()) {
            debug!("reconcile_mode_shortcuts: could not register '{id}': {e}");
        }
    }
}

/// Temporarily unregister all bindings while the user is recording a
/// shortcut in the UI. This avoids firing actions while keys are recorded.
#[tauri::command]
#[specta::specta]
pub fn suspend_all_bindings(app: AppHandle) -> Result<(), String> {
    suspend_all_shortcuts(&app);
    Ok(())
}

/// Re-register all bindings after the user has finished recording.
#[tauri::command]
#[specta::specta]
pub fn resume_all_bindings(app: AppHandle) -> Result<(), String> {
    resume_all_shortcuts(&app);
    Ok(())
}

// ============================================================================
// Keyboard Implementation Switching
// ============================================================================

/// Result of changing keyboard implementation
#[derive(Serialize, Type)]
pub struct ImplementationChangeResult {
    pub success: bool,
    /// List of binding IDs that were reset to defaults due to incompatibility
    pub reset_bindings: Vec<String>,
}

/// Change the keyboard implementation with runtime switching.
/// This will unregister all shortcuts from the old implementation,
/// validate shortcuts for the new implementation (resetting invalid ones to defaults),
/// and register them with the new implementation.
#[tauri::command]
#[specta::specta]
pub fn change_keyboard_implementation_setting(
    app: AppHandle,
    implementation: String,
) -> Result<ImplementationChangeResult, String> {
    let current_settings = settings::get_settings(&app);
    let current_impl = current_settings.keyboard_implementation;
    let new_impl = parse_keyboard_implementation(&implementation);

    // If same implementation, nothing to do
    if current_impl == new_impl {
        return Ok(ImplementationChangeResult {
            success: true,
            reset_bindings: vec![],
        });
    }

    info!(
        "Switching keyboard implementation from {:?} to {:?}",
        current_impl, new_impl
    );

    // Unregister all shortcuts from the current implementation
    unregister_all_shortcuts(&app, current_impl);

    // Update the setting
    settings::update_settings(&app, |settings| {
        settings.keyboard_implementation = new_impl;
    })?;

    // Carbon fallback registrations use the Tauri plugin. Remove them before
    // registering the full Tauri implementation to avoid duplicate conflicts.
    if new_impl == KeyboardImplementation::Tauri {
        crate::secure_input::reconcile_fallback(&app);
    }

    // Initialize new implementation if needed (HandyKeys needs state)
    if new_impl == KeyboardImplementation::HandyKeys && initialize_handy_keys_with_rollback(&app)? {
        // Shortcuts already registered during init.
        crate::secure_input::reconcile_fallback(&app);
        return Ok(ImplementationChangeResult {
            success: true,
            reset_bindings: vec![],
        });
    }

    // Register all shortcuts with new implementation, resetting invalid ones
    let reset_bindings = register_all_shortcuts_for_implementation(&app, new_impl);
    crate::secure_input::reconcile_fallback(&app);

    // Emit event to notify frontend of the change
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "keyboard_implementation",
            "value": implementation,
            "reset_bindings": reset_bindings
        }),
    );

    info!("Keyboard implementation switched to {:?}", new_impl);

    Ok(ImplementationChangeResult {
        success: true,
        reset_bindings,
    })
}

/// Get the current keyboard implementation
#[tauri::command]
#[specta::specta]
pub fn get_keyboard_implementation(app: AppHandle) -> String {
    let settings = settings::get_settings(&app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => "tauri".to_string(),
        KeyboardImplementation::HandyKeys => "handy_keys".to_string(),
    }
}

// ============================================================================
// Validation Helpers
// ============================================================================

/// Validate a shortcut for a specific implementation
fn validate_shortcut_for_implementation(
    raw: &str,
    implementation: KeyboardImplementation,
) -> Result<(), String> {
    match implementation {
        KeyboardImplementation::Tauri => tauri_impl::validate_shortcut(raw),
        KeyboardImplementation::HandyKeys => handy_keys::validate_shortcut(raw),
    }
}

/// Parse a keyboard implementation string into the enum
fn parse_keyboard_implementation(s: &str) -> KeyboardImplementation {
    match s {
        "tauri" => KeyboardImplementation::Tauri,
        "handy_keys" => KeyboardImplementation::HandyKeys,
        other => {
            warn!(
                "Invalid keyboard implementation '{}', defaulting to tauri",
                other
            );
            KeyboardImplementation::Tauri
        }
    }
}

/// Unregister all shortcuts for the current implementation
fn unregister_all_shortcuts(app: &AppHandle, implementation: KeyboardImplementation) {
    let bindings = settings::get_bindings(app);

    for (id, binding) in bindings {
        // Skip cancel shortcut as it's dynamically registered
        if id == "cancel" {
            continue;
        }

        let result = match implementation {
            KeyboardImplementation::Tauri => tauri_impl::unregister_shortcut(app, binding),
            KeyboardImplementation::HandyKeys => handy_keys::unregister_shortcut(app, binding),
        };

        if let Err(e) = result {
            warn!(
                "Failed to unregister shortcut '{}' during switch: {}",
                id, e
            );
        }
    }
}

/// Register all persisted shortcuts for a specific implementation, resetting
/// only chords that the target backend cannot parse.
fn register_all_shortcuts_for_implementation(
    app: &AppHandle,
    implementation: KeyboardImplementation,
) -> Vec<String> {
    let mut reset_bindings = Vec::new();
    let mut current_settings = settings::get_settings(app);

    for mut binding in bindings_for_registration(&current_settings) {
        let id = binding.id.clone();
        if let Err(e) =
            validate_shortcut_for_implementation(&binding.current_binding, implementation)
        {
            info!(
                "Shortcut '{}' ({}) is invalid for {:?}: {}. Resetting to default.",
                id, binding.current_binding, implementation, e
            );
            binding.current_binding = binding.default_binding.clone();
            current_settings
                .bindings
                .insert(id.clone(), binding.clone());
            reset_bindings.push(id.clone());
        }

        let result = match implementation {
            KeyboardImplementation::Tauri => tauri_impl::register_shortcut(app, binding),
            KeyboardImplementation::HandyKeys => handy_keys::register_shortcut(app, binding),
        };

        if let Err(e) = result {
            error!(
                "Failed to register shortcut '{}' for {:?}: {}",
                id, implementation, e
            );
        }
    }

    if !reset_bindings.is_empty() {
        let reset_values = reset_bindings
            .iter()
            .filter_map(|id| current_settings.bindings.get(id).cloned())
            .collect::<Vec<_>>();
        // Nothing in this function's shape can report a store failure; the settings seam logs it.
        let _ = settings::update_settings(app, |settings| {
            for binding in reset_values {
                settings.bindings.insert(binding.id.clone(), binding);
            }
        });
    }

    reset_bindings
}

/// Initialize HandyKeys if not already initialized, with rollback on failure
fn initialize_handy_keys_with_rollback(app: &AppHandle) -> Result<bool, String> {
    if app.try_state::<handy_keys::HandyKeysState>().is_some() {
        return Ok(false); // Already initialized, caller should continue
    }

    if let Err(e) = handy_keys::init_shortcuts(app) {
        error!("Failed to initialize HandyKeys: {}", e);
        // Rollback to Tauri
        settings::update_settings(app, |settings| {
            settings.keyboard_implementation = KeyboardImplementation::Tauri;
        })?;
        crate::secure_input::reconcile_fallback(app);
        tauri_impl::init_shortcuts(app);
        return Err(format!(
            "Failed to initialize HandyKeys: {}. Reverted to Tauri.",
            e
        ));
    }

    // init_shortcuts already registered shortcuts
    Ok(true)
}

// ============================================================================
// General Settings Commands
// ============================================================================

#[tauri::command]
#[specta::specta]
pub fn change_ptt_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.push_to_talk = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_audio_feedback_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.audio_feedback = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_audio_feedback_volume_setting(app: AppHandle, volume: f32) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.audio_feedback_volume = volume;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_sound_theme_setting(app: AppHandle, theme: String) -> Result<(), String> {
    let parsed = match theme.as_str() {
        "marimba" => SoundTheme::Marimba,
        "pop" => SoundTheme::Pop,
        "custom" => SoundTheme::Custom,
        other => {
            warn!("Invalid sound theme '{}', defaulting to marimba", other);
            SoundTheme::Marimba
        }
    };
    settings::update_settings(&app, |settings| {
        settings.sound_theme = parsed;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_theme_setting(app: AppHandle, theme: String) -> Result<(), String> {
    let parsed = match theme.as_str() {
        "system" => Theme::System,
        "light" => Theme::Light,
        "dark" => Theme::Dark,
        other => {
            warn!("Invalid theme '{}', defaulting to system", other);
            Theme::System
        }
    };
    settings::update_settings(&app, |settings| {
        settings.theme = parsed;
    })?;
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    apply_window_theme(&app, parsed);
    // Notify other webviews (the recording overlay) so they re-apply the palette
    // live — they set `data-theme` on their own document and can't see this one.
    let _ = app.emit("theme-changed", parsed);
    Ok(())
}

/// Applies the appearance setting to the native window chrome (title bar), which
/// CSS `data-theme` cannot reach. `System` clears the override so the window
/// follows the OS. Call this on startup and whenever the setting changes to keep
/// the title bar in sync with the in-app palette.
///
/// On Windows this themes the title bar only. On macOS `set_theme` sets
/// `NSApp.appearance` app-wide, which is what we want here: it darkens the title
/// bar and keeps the overlay in step. Linux is left to `data-theme` alone, since
/// its window theming is backend-dependent and unreliable.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn apply_window_theme(app: &AppHandle, theme: Theme) {
    let window_theme = match theme {
        Theme::System => None,
        Theme::Light => Some(tauri::Theme::Light),
        Theme::Dark => Some(tauri::Theme::Dark),
    };
    if let Some(window) = app.get_webview_window("main") {
        if let Err(e) = window.set_theme(window_theme) {
            warn!("Failed to apply window theme: {}", e);
        }
    }
}

/// Windows whose document follows the Material setting: every window there is.
/// The consent panel is its own NSPanel and its own vibrancy view, so its
/// truth is tracked separately from the main window's.
const MATERIAL_WINDOWS: [&str; 3] = [
    "main",
    "recording_overlay",
    crate::meeting::consent_panel::CONSENT_PANEL_LABEL,
];

/// The material actually in force — intent AND a live vibrancy view. Cached so
/// a document that loads later (a reload, or the overlay window being created
/// after startup) can be told the truth without re-running the native apply.
/// Written only by `apply_window_material`.
static GLASS_IN_FORCE: AtomicBool = AtomicBool::new(false);

/// The same, for the consent panel: it is created after the material is first
/// applied, and its vibrancy can fail on its own.
static CONSENT_GLASS_IN_FORCE: AtomicBool = AtomicBool::new(false);

fn material_in_force() -> AppearanceMaterial {
    if GLASS_IN_FORCE.load(Ordering::Relaxed) {
        AppearanceMaterial::Glass
    } else {
        AppearanceMaterial::Solid
    }
}

fn consent_material_in_force() -> AppearanceMaterial {
    if CONSENT_GLASS_IN_FORCE.load(Ordering::Relaxed) {
        AppearanceMaterial::Glass
    } else {
        AppearanceMaterial::Solid
    }
}

fn material_for(label: &str) -> AppearanceMaterial {
    if label == crate::meeting::consent_panel::CONSENT_PANEL_LABEL {
        consent_material_in_force()
    } else {
        material_in_force()
    }
}

fn material_script(material: AppearanceMaterial) -> String {
    format!(
        "document.documentElement.dataset.material = '{}';",
        material.as_str()
    )
}

/// Re-applies the material to a document that has just loaded. Every window
/// starts from an initialization script that writes the conservative `solid`,
/// so without this a reload would silently drop Glass; and the attribute is
/// never allowed to claim Glass that the native layer is not backing, which is
/// why this reads the cached truth rather than the setting.
pub fn reassert_window_material(webview: &tauri::Webview) {
    if !MATERIAL_WINDOWS.contains(&webview.label()) {
        return;
    }
    if let Err(error) = webview.eval(material_script(material_for(webview.label()))) {
        warn!(
            "Could not restore the {} window material: {error}",
            webview.label()
        );
    }
}

/// Puts the Material setting into force and returns what is actually in force.
///
/// Glass needs a native vibrancy view behind the transparent webview; that is
/// macOS-only and can fail, and a webview left transparent over nothing is a
/// blank window. So intent and reality are resolved here, once, and the result
/// is written to every relevant webview's `data-material` — which makes this
/// the single owner of that attribute. The frontend never sets it.
///
/// Vibrancy is applied to the main window and, separately, to the meeting
/// consent panel, whose window is exactly its card. The recording overlay is a
/// transparent panel sized larger than the card it draws (256x46 around a
/// 184x40 pill, and the card animates its own width), so a window-scoped
/// NSVisualEffectView would paint a frosted rectangle around the pill; its card
/// takes the tint-only glass class instead.
pub fn apply_window_material(app: &AppHandle, material: AppearanceMaterial) -> AppearanceMaterial {
    let effective = resolve_window_material(app, material);
    GLASS_IN_FORCE.store(effective == AppearanceMaterial::Glass, Ordering::Relaxed);
    // The panel may not exist yet at startup; `apply_consent_panel_material`
    // runs again from `consent_panel::create` once it does.
    apply_consent_panel_material(app);
    for label in MATERIAL_WINDOWS {
        if let Some(window) = app.get_webview_window(label) {
            if let Err(error) = window.eval(material_script(material_for(label))) {
                warn!("Could not set the {label} window material: {error}");
            }
        }
    }
    effective
}

/// Puts the material in force on the consent panel, whose window is created
/// after the first apply. Its card fills the window, so the vibrancy view is
/// exactly the card and its radius is the card's.
pub fn apply_consent_panel_material(app: &AppHandle) {
    let label = crate::meeting::consent_panel::CONSENT_PANEL_LABEL;
    let effective = resolve_consent_panel_material(app, material_in_force());
    CONSENT_GLASS_IN_FORCE.store(effective == AppearanceMaterial::Glass, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window(label) {
        if let Err(error) = window.eval(material_script(effective)) {
            warn!("Could not set the {label} window material: {error}");
        }
    }
}

#[cfg(target_os = "macos")]
fn resolve_consent_panel_material(
    app: &AppHandle,
    material: AppearanceMaterial,
) -> AppearanceMaterial {
    use window_vibrancy::{
        apply_vibrancy, clear_vibrancy, NSVisualEffectMaterial, NSVisualEffectState,
    };

    let Some(window) = app.get_webview_window(crate::meeting::consent_panel::CONSENT_PANEL_LABEL)
    else {
        return AppearanceMaterial::Solid;
    };
    match material {
        AppearanceMaterial::Solid => {
            if let Err(error) = clear_vibrancy(&window) {
                warn!("Could not clear the consent panel's vibrancy: {error}");
            }
            AppearanceMaterial::Solid
        }
        // Popover rather than UnderWindowBackground: this is a floating panel
        // over other applications, and Active rather than
        // FollowsWindowActiveState because a prompt is answered while the app
        // behind it keeps focus.
        AppearanceMaterial::Glass => match apply_vibrancy(
            &window,
            NSVisualEffectMaterial::Popover,
            Some(NSVisualEffectState::Active),
            Some(crate::meeting::consent_panel::PANEL_CORNER_RADIUS),
        ) {
            Ok(()) => AppearanceMaterial::Glass,
            Err(error) => {
                warn!("Could not frost the consent panel; using the solid material: {error}");
                AppearanceMaterial::Solid
            }
        },
    }
}

#[cfg(not(target_os = "macos"))]
fn resolve_consent_panel_material(
    _app: &AppHandle,
    _material: AppearanceMaterial,
) -> AppearanceMaterial {
    AppearanceMaterial::Solid
}

/// True while macOS Reduce Transparency is on.
///
/// WebKit does not implement `prefers-reduced-transparency`, so the webview
/// cannot answer this and the CSS fallback in primitives.css never fires. The
/// setting is read here, where the vibrancy view is applied, and the answer is
/// Solid: the accessibility preference outranks the appearance intent.
#[cfg(target_os = "macos")]
fn reduce_transparency() -> bool {
    use objc2_app_kit::NSWorkspace;
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

#[cfg(target_os = "macos")]
fn resolve_window_material(app: &AppHandle, material: AppearanceMaterial) -> AppearanceMaterial {
    use window_vibrancy::{
        apply_vibrancy, clear_vibrancy, NSVisualEffectMaterial, NSVisualEffectState,
    };

    let Some(window) = app.get_webview_window("main") else {
        return AppearanceMaterial::Solid;
    };
    let material = if reduce_transparency() {
        AppearanceMaterial::Solid
    } else {
        material
    };
    match material {
        AppearanceMaterial::Solid => {
            if let Err(error) = clear_vibrancy(&window) {
                warn!("Could not clear Sona window vibrancy: {error}");
            }
            AppearanceMaterial::Solid
        }
        AppearanceMaterial::Glass => match apply_vibrancy(
            &window,
            NSVisualEffectMaterial::UnderWindowBackground,
            Some(NSVisualEffectState::FollowsWindowActiveState),
            None,
        ) {
            Ok(()) => AppearanceMaterial::Glass,
            Err(error) => {
                warn!("Could not apply Sona window vibrancy; using the solid material: {error}");
                AppearanceMaterial::Solid
            }
        },
    }
}

/// Glass is a macOS vibrancy effect and nothing else implements it, so every
/// other platform is Solid regardless of the stored intent.
#[cfg(not(target_os = "macos"))]
fn resolve_window_material(_app: &AppHandle, _material: AppearanceMaterial) -> AppearanceMaterial {
    AppearanceMaterial::Solid
}

#[tauri::command]
#[specta::specta]
pub fn change_appearance_material_setting(app: AppHandle, material: String) -> Result<(), String> {
    let parsed = AppearanceMaterial::from_str_or_solid(&material);
    if material != parsed.as_str() {
        warn!("Invalid appearance material '{material}', using solid");
    }
    settings::update_settings(&app, |settings| {
        settings.appearance_material = parsed;
    })?;
    let effective = apply_window_material(&app, parsed);
    // The overlay webview cannot see this window's store, and
    // the effective material is not always the stored one, so the event carries
    // what is actually in force rather than what was asked for.
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "appearance_material",
            "value": effective.as_str()
        }),
    );
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_translate_to_english_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.translate_to_english = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_selected_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.selected_language = language;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_english_spelling_setting(
    app: AppHandle,
    spelling: EnglishSpelling,
) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.english_spelling = spelling;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_overlay_position_setting(app: AppHandle, position: String) -> Result<(), String> {
    let parsed = match position.as_str() {
        // "none" is retired (visibility is overlay_style now); fold legacy callers
        // onto Bottom rather than warn.
        "none" | "bottom" => OverlayPosition::Bottom,
        "top" => OverlayPosition::Top,
        other => {
            warn!("Invalid overlay position '{}', defaulting to bottom", other);
            OverlayPosition::Bottom
        }
    };
    settings::update_settings(&app, |settings| {
        settings.overlay_position = parsed;
    })?;

    // Whether the overlay shows at all is owned by overlay_style now; position
    // only ever toggles Top/Bottom, so the enabled cache is untouched here.
    // Update overlay position without recreating window
    crate::utils::update_overlay_position(&app);

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_overlay_style_setting(app: AppHandle, style: String) -> Result<(), String> {
    let parsed = match style.as_str() {
        "none" => OverlayStyle::None,
        "minimal" => OverlayStyle::Minimal,
        "live" => OverlayStyle::Live,
        other => {
            warn!("Invalid overlay style '{}', defaulting to minimal", other);
            OverlayStyle::Minimal
        }
    };
    settings::update_settings(&app, |settings| {
        settings.overlay_style = parsed;
    })?;

    // Keep the cached overlay-enabled flag in sync so emit_levels stops (or
    // resumes) emitting on the next audio callback.
    crate::overlay::update_overlay_enabled_cache(parsed != OverlayStyle::None);

    // Reposition in case the window needs to re-center for the new style.
    crate::utils::update_overlay_position(&app);

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_debug_mode_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.debug_mode = enabled;
    })?;
    // Keep webview log streaming in sync: the live log viewer only exists in
    // debug mode, so logs are forwarded to the frontend only while it is on.
    crate::WEBVIEW_LOG_STREAMING.store(enabled, std::sync::atomic::Ordering::Relaxed);

    // Emit event to notify frontend of debug mode change
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "debug_mode",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_start_hidden_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.start_hidden = enabled;
    })?;
    // Notify frontend
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "start_hidden",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_autostart_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.autostart_enabled = enabled;
    })?;
    // Apply the autostart setting immediately
    crate::autostart::apply_autostart(&app, enabled);

    // Notify frontend
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "autostart_enabled",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_show_whats_new_on_update_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.show_whats_new_on_update = enabled;
    })?;
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "show_whats_new_on_update",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_whats_new_last_seen_version_setting(
    app: AppHandle,
    version: String,
) -> Result<(), String> {
    let version = version.trim().to_string();
    settings::update_settings(&app, |settings| {
        settings.whats_new_last_seen_version = version.clone();
    })?;
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "whats_new_last_seen_version",
            "value": version
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_word_correction_threshold_setting(
    app: AppHandle,
    threshold: f64,
) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.word_correction_threshold = threshold;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_extra_recording_buffer_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.extra_recording_buffer_ms = ms;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_available_typing_tools() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        crate::clipboard::get_available_typing_tools()
    }
    #[cfg(not(target_os = "linux"))]
    {
        vec!["auto".to_string()]
    }
}

#[tauri::command]
#[specta::specta]
pub fn change_external_script_path_setting(
    app: AppHandle,
    path: Option<String>,
) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.external_script_path = path;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.post_process_enabled = enabled;
    })?;
    crate::secure_input::reconcile_fallback(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_experimental_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.experimental_enabled = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_base_url_setting(
    app: AppHandle,
    provider_id: String,
    base_url: String,
) -> Result<(), String> {
    settings::try_update_settings(&app, |settings| {
        let provider = settings
            .post_process_provider(&provider_id)
            .cloned()
            .ok_or_else(|| format!("Provider '{}' not found", provider_id))?;

        if provider.id != "custom" {
            return Err(format!(
                "Provider '{}' does not allow editing the base URL",
                provider.label
            ));
        }

        let mut candidate = provider;
        candidate.base_url = base_url;
        candidate.endpoint().map_err(|_| {
            "Custom provider URLs must use HTTPS or loopback HTTP without credentials, queries, or fragments"
                .to_string()
        })?;

        let provider = settings
            .post_process_provider_mut(&provider_id)
            .ok_or_else(|| "Provider is no longer configured".to_string())?;
        provider.base_url = candidate.base_url;
        settings.post_process_provider_consents.remove(&provider_id);
        Ok(())
    })
}

/// Generic helper to validate provider exists
fn validate_provider_exists(
    settings: &settings::AppSettings,
    provider_id: &str,
) -> Result<(), String> {
    if !settings
        .post_process_providers
        .iter()
        .any(|provider| provider.id == provider_id)
    {
        return Err(format!("Provider '{}' not found", provider_id));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_model_setting(
    app: AppHandle,
    provider_id: String,
    model: String,
) -> Result<(), String> {
    settings::try_update_settings(&app, |settings| {
        validate_provider_exists(settings, &provider_id)?;
        settings.post_process_models.insert(provider_id, model);
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn set_post_process_provider(app: AppHandle, provider_id: String) -> Result<(), String> {
    settings::try_update_settings(&app, |settings| {
        validate_provider_exists(settings, &provider_id)?;
        settings.post_process_provider_id = provider_id;
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn add_post_process_prompt(
    app: AppHandle,
    name: String,
    prompt: String,
) -> Result<LLMPrompt, String> {
    // Generate unique ID using timestamp and random component
    let id = format!("prompt_{}", chrono::Utc::now().timestamp_millis());

    let new_prompt = LLMPrompt {
        id: id.clone(),
        name,
        prompt,
    };

    settings::update_settings(&app, |settings| {
        settings.post_process_prompts.push(new_prompt.clone());
    })?;

    Ok(new_prompt)
}

#[tauri::command]
#[specta::specta]
pub fn update_post_process_prompt(
    app: AppHandle,
    id: String,
    name: String,
    prompt: String,
) -> Result<(), String> {
    settings::try_update_settings(&app, |settings| {
        let existing_prompt = settings
            .post_process_prompts
            .iter_mut()
            .find(|prompt| prompt.id == id)
            .ok_or_else(|| format!("Prompt with id '{}' not found", id))?;
        existing_prompt.name = name;
        existing_prompt.prompt = prompt;
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn delete_post_process_prompt(app: AppHandle, id: String) -> Result<(), String> {
    settings::try_update_settings(&app, |settings| {
        if settings.post_process_prompts.len() <= 1 {
            return Err("Cannot delete the last prompt".to_string());
        }

        let original_len = settings.post_process_prompts.len();
        settings
            .post_process_prompts
            .retain(|prompt| prompt.id != id);
        if settings.post_process_prompts.len() == original_len {
            return Err(format!("Prompt with id '{}' not found", id));
        }

        if settings.post_process_selected_prompt_id.as_ref() == Some(&id) {
            settings.post_process_selected_prompt_id = settings
                .post_process_prompts
                .first()
                .map(|prompt| prompt.id.clone());
        }
        Ok(())
    })
}

fn custom_provider_can_fetch_without_secret(provider: &settings::PostProcessProvider) -> bool {
    provider.id == "custom"
        && provider
            .endpoint()
            .is_ok_and(|endpoint| !endpoint.is_remote())
}

/// Assemble the typed catalog the command returns. Every discovery state
/// carries only what the provider reported: a saved selection belongs to
/// settings, and the caller merges it with the id for its own scope, which is
/// the mode's model for a mode pane and the global one elsewhere.
fn catalog_with_discovery(
    provider_id: String,
    allows_manual_model_id: bool,
    discovery: PostProcessModelDiscovery,
    models: Vec<PostProcessModelOption>,
) -> PostProcessModelCatalog {
    PostProcessModelCatalog {
        provider_id,
        models,
        discovery,
        allows_manual_model_id,
    }
}

fn catalog_discovery_for_secret_error(error: SecretCommandError) -> PostProcessModelDiscovery {
    match error {
        SecretCommandError::Unavailable | SecretCommandError::Backend => {
            PostProcessModelDiscovery::CredentialUnavailable
        }
        SecretCommandError::Locked => PostProcessModelDiscovery::CredentialLocked,
        SecretCommandError::Corrupt => PostProcessModelDiscovery::CredentialCorrupt,
        SecretCommandError::Invalid => PostProcessModelDiscovery::InvalidDestination,
        SecretCommandError::Busy => PostProcessModelDiscovery::CredentialBusy,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn discover_post_process_model_catalog(
    app: AppHandle,
    provider_id: String,
) -> PostProcessModelCatalog {
    let settings = settings::get_settings(&app);
    let Some(provider) = settings.post_process_provider(&provider_id).cloned() else {
        return PostProcessModelCatalog {
            provider_id,
            models: Vec::new(),
            discovery: PostProcessModelDiscovery::InvalidDestination,
            allows_manual_model_id: false,
        };
    };
    let allows_manual_model_id = provider.allows_manual_model_id();
    let endpoint = match provider.endpoint() {
        Ok(endpoint) => endpoint,
        Err(_) => {
            return catalog_with_discovery(
                provider_id,
                allows_manual_model_id,
                PostProcessModelDiscovery::InvalidDestination,
                Vec::new(),
            );
        }
    };

    if provider.catalog_source() == PostProcessCatalogSource::Unsupported {
        return catalog_with_discovery(
            provider_id,
            allows_manual_model_id,
            PostProcessModelDiscovery::Unsupported,
            Vec::new(),
        );
    }
    if endpoint.is_remote()
        && !settings.has_current_post_process_provider_consent(&provider, &endpoint)
    {
        return catalog_with_discovery(
            provider_id,
            allows_manual_model_id,
            PostProcessModelDiscovery::RequiresConsent,
            Vec::new(),
        );
    }

    let secret = if custom_provider_can_fetch_without_secret(&provider) {
        None
    } else {
        let account = match SecretAccount::llm(&provider_id) {
            Ok(account) => account,
            Err(error) => {
                return catalog_with_discovery(
                    provider_id,
                    allows_manual_model_id,
                    catalog_discovery_for_secret_error(SecretCommandError::from(error)),
                    Vec::new(),
                );
            }
        };
        let secrets = app.state::<Arc<SecretManager>>();
        match secrets.resolve_optional(account).await {
            Ok(SecretRead::Found(secret)) => Some(secret),
            Ok(SecretRead::NotFound) => {
                return catalog_with_discovery(
                    provider_id,
                    allows_manual_model_id,
                    PostProcessModelDiscovery::MissingCredential,
                    Vec::new(),
                );
            }
            Err(error) => {
                return catalog_with_discovery(
                    provider_id,
                    allows_manual_model_id,
                    catalog_discovery_for_secret_error(error.into()),
                    Vec::new(),
                );
            }
        }
    };

    match crate::llm_client::discover_models(&provider, &endpoint, secret.as_ref()).await {
        Ok(models) => catalog_with_discovery(
            provider_id,
            allows_manual_model_id,
            PostProcessModelDiscovery::Ready,
            models,
        ),
        Err(discovery) => {
            catalog_with_discovery(provider_id, allows_manual_model_id, discovery, Vec::new())
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn set_post_process_selected_prompt(app: AppHandle, id: String) -> Result<(), String> {
    settings::try_update_settings(&app, |settings| {
        if !settings
            .post_process_prompts
            .iter()
            .any(|prompt| prompt.id == id)
        {
            return Err(format!("Prompt with id '{}' not found", id));
        }
        settings.post_process_selected_prompt_id = Some(id);
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn change_mute_while_recording_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.mute_while_recording = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_append_trailing_space_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.append_trailing_space = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_lazy_stream_close_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.lazy_stream_close = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_vad_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.vad_enabled = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_filler_word_removal_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.filler_word_removal_enabled = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_app_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.app_language = language.clone();
    })?;
    // Refresh the tray menu with the new language
    tray::update_tray_menu(&app);

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_show_tray_icon_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.show_tray_icon = enabled;
    })?;
    // Apply change immediately
    tray::set_tray_visibility(&app, enabled);

    Ok(())
}

/// Save accelerator settings and make the next model use reload with them.
/// The currently running transcription, if any, keeps its existing engine.
fn reload_model_on_next_use(app: &AppHandle) {
    let tm = app.state::<std::sync::Arc<crate::managers::transcription::TranscriptionManager>>();
    tm.reload_model_on_next_use();
}

#[tauri::command]
#[specta::specta]
pub fn change_transcribe_accelerator_setting(
    app: AppHandle,
    accelerator: settings::TranscribeAcceleratorSetting,
) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.transcribe_accelerator = accelerator;
    })?;
    reload_model_on_next_use(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_ort_accelerator_setting(
    app: AppHandle,
    accelerator: settings::OrtAcceleratorSetting,
) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.ort_accelerator = accelerator;
    })?;
    reload_model_on_next_use(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_transcribe_gpu_device(app: AppHandle, device: Option<String>) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.transcribe_gpu_device = device;
    })?;
    reload_model_on_next_use(&app);
    Ok(())
}

/// Return which accelerators and GPU devices are available for this build.
///
/// First-call cost is dominated by enumerating GPU devices through the
/// transcribe.cpp Metal/Vulkan backend, which loads dynamic libraries and
/// probes hardware. Run it on the blocking pool so the webview thread
/// stays responsive — see also the startup pre-warm in `lib.rs`.
#[tauri::command]
#[specta::specta]
pub async fn get_available_accelerators() -> crate::managers::transcription::AvailableAccelerators {
    match tauri::async_runtime::spawn_blocking(
        crate::managers::transcription::get_available_accelerators,
    )
    .await
    {
        Ok(accelerators) => accelerators,
        Err(error) => panic!("get_available_accelerators task failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::ensure_mode_settings;
    #[test]
    fn catalog_secret_errors_return_safe_discovery_states() {
        let cases = [
            (
                SecretCommandError::Unavailable,
                PostProcessModelDiscovery::CredentialUnavailable,
            ),
            (
                SecretCommandError::Backend,
                PostProcessModelDiscovery::CredentialUnavailable,
            ),
            (
                SecretCommandError::Locked,
                PostProcessModelDiscovery::CredentialLocked,
            ),
            (
                SecretCommandError::Corrupt,
                PostProcessModelDiscovery::CredentialCorrupt,
            ),
            (
                SecretCommandError::Invalid,
                PostProcessModelDiscovery::InvalidDestination,
            ),
            (
                SecretCommandError::Busy,
                PostProcessModelDiscovery::CredentialBusy,
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(catalog_discovery_for_secret_error(error), expected);
        }
    }
    #[test]
    fn rebound_mode_chord_survives_reload_for_both_shortcut_backends() -> Result<(), String> {
        let mut settings = settings::get_default_settings();
        ensure_mode_settings(&mut settings);
        let id = "mode/email/transcribe";
        let binding = settings
            .bindings
            .get_mut(id)
            .ok_or_else(|| format!("missing default binding '{id}'"))?;
        binding.current_binding = "option+shift+9".to_string();

        let serialized = serde_json::to_string(&settings)
            .map_err(|error| format!("failed to serialize default shortcut settings: {error}"))?;
        let mut reloaded: AppSettings = serde_json::from_str(&serialized)
            .map_err(|error| format!("failed to reload shortcut settings: {error}"))?;
        assert!(!ensure_mode_settings(&mut reloaded));

        let rebound = reloaded
            .bindings
            .get(id)
            .ok_or_else(|| format!("missing reloaded binding '{id}'"))?
            .current_binding
            .clone();
        let tauri_bindings = bindings_for_registration(&reloaded);
        let handy_keys_bindings = bindings_for_registration(&reloaded);
        assert!(tauri_bindings
            .iter()
            .any(|binding| binding.id == id && binding.current_binding == rebound));
        assert!(handy_keys_bindings
            .iter()
            .any(|binding| binding.id == id && binding.current_binding == rebound));
        assert!(tauri_impl::validate_shortcut(&rebound).is_ok());
        assert!(handy_keys::validate_shortcut(&rebound).is_ok());
        Ok(())
    }

    /// Why the cancel key needs more than its stored chord. handy-keys
    /// matches a hotkey's modifier groups exactly: a group the hotkey does not
    /// name must be absent from the event. So the bare `escape` this ships
    /// with — the live registration reads `Registered handy-keys shortcut:
    /// cancel -> Hotkey { modifiers: Modifiers(0x0), key: Some(Escape) }` — is
    /// not the hotkey an Escape struck mid-hold produces, and on its own it
    /// left hold-to-talk with no keyboard abort at all.
    /// `every_recording_chord_leaves_the_cancel_key_reachable` is the other
    /// half: what makes the key reachable anyway.
    #[test]
    fn the_stored_cancel_chord_alone_cannot_hear_a_mid_hold_escape() -> Result<(), String> {
        let mut settings = settings::get_default_settings();
        ensure_mode_settings(&mut settings);

        let chord = |id: &str| -> Result<::handy_keys::Hotkey, String> {
            settings
                .bindings
                .get(id)
                .ok_or_else(|| format!("missing default binding '{id}'"))?
                .current_binding
                .parse()
                .map_err(|error| format!("'{id}' is not a handy-keys chord: {error}"))
        };

        let cancel = chord("cancel")?;
        assert!(cancel.modifiers.is_empty(), "cancel ships as a bare key");

        // Every chord that can hold a recording open carries a modifier, and
        // while it is held those are exactly the modifiers Escape arrives with.
        for id in ["transcribe", "command", "mode/email/transcribe"] {
            let held = chord(id)?;
            assert!(!held.modifiers.is_empty(), "'{id}' holds no modifier");
            assert!(
                !cancel.modifiers.matches(held.modifiers),
                "cancel is deaf while '{id}' is held"
            );
        }
        Ok(())
    }

    /// The contract that answers it: whatever chord is holding a recording
    /// open, some member of the cancel set is the hotkey an Escape struck
    /// during that hold produces. Asserted against the modifier state the
    /// listener would stamp on that Escape, so it fails if the set stops
    /// covering a chord — and the exact set is pinned too, so it also fails
    /// if the set widens beyond the modifiers the user has already given
    /// Sona.
    #[test]
    fn every_recording_chord_leaves_the_cancel_key_reachable() -> Result<(), String> {
        let mut settings = settings::get_default_settings();
        ensure_mode_settings(&mut settings);

        let parse = |chord: &str| -> Result<::handy_keys::Hotkey, String> {
            chord
                .parse()
                .map_err(|error| format!("'{chord}' is not a handy-keys chord: {error}"))
        };

        let cancel_set = cancel_bindings_for_registration(&settings)
            .into_iter()
            .map(|binding| binding.current_binding)
            .collect::<Vec<_>>();
        assert_eq!(
            cancel_set,
            vec!["escape", "option+escape", "option+shift+escape"],
            "the cancel set is the bare key plus the prefixes already in use"
        );

        let cancel_hotkeys = cancel_set
            .iter()
            .map(|chord| parse(chord))
            .collect::<Result<Vec<_>, _>>()?;

        for (id, binding) in &settings.bindings {
            if crate::modes::TranscriptionIntent::from_binding(id).is_none() {
                continue;
            }
            let held = parse(&binding.current_binding)?.modifiers;
            assert!(
                cancel_hotkeys.iter().any(|cancel| {
                    cancel.modifiers.matches(held) && cancel.key == Some(::handy_keys::Key::Escape)
                }),
                "no cancel hotkey fires while '{id}' is held"
            );
        }
        Ok(())
    }

    /// The handy-keys recorder persists what the macOS listener saw, and that
    /// listener tracks modifiers by side: recording option+space on the left
    /// option key stores `option_left+space`. The Tauri accelerator grammar
    /// has no side-specific names, so this chord has to be reported invalid
    /// for that backend — otherwise switching to it neither registers the
    /// chord nor lists it in `reset_bindings`, and the user's shortcut
    /// disappears without a word.
    #[test]
    fn a_side_specific_chord_is_invalid_for_the_tauri_backend() -> Result<(), String> {
        let recorded =
            ::handy_keys::Hotkey::new(::handy_keys::Modifiers::OPT_LEFT, ::handy_keys::Key::Space)
                .map_err(|error| format!("could not build the recorded chord: {error}"))?
                .to_handy_string();
        assert_eq!(recorded, "option_left+space");

        handy_keys::validate_shortcut(&recorded)
            .map_err(|error| format!("handy-keys rejected its own recording: {error}"))?;
        assert!(
            tauri_impl::validate_shortcut(&recorded).is_err(),
            "the Tauri backend cannot register '{recorded}' and must say so"
        );
        Ok(())
    }
}
