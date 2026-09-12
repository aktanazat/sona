//! System tray icon and menu.
//! The tray is driven by a single *desired state* snapshot ([`TrayDesired`])
//! that callers update through [`set_tray_state`], [`refresh_tray_icon`] and
//! [`update_tray_menu`]. Every such call just records intent and schedules a
//! single applier on the main thread, which diffs the desired snapshot against
//! what is currently displayed and touches the native tray only for the parts
//! that actually changed. Requests that arrive while an apply is pending are
//! coalesced into it, so bursts of state changes never queue up native work.
//!
//! Why: native tray updates are the lever we control for the macOS tray
//! disappearance bug (tauri-apps/tauri#12060, Sona #1948). Before this, every
//! recording cycle rebuilt the full menu 3-6 times from several threads, and
//! concurrent rebuilds could interleave and leave a stale menu behind.
//!
//! Exception: [`set_tray_visibility`] and [`recreate_tray_icon`] call the tray
//! directly. Visibility is a separate attribute that never participates in the
//! icon/menu diff, both are rare and user-initiated, and Tauri marshals them
//! onto the main thread so they serialize with the applier anyway. Re-showing
//! a hidden tray relies on tray-icon recreating it from the last applied
//! icon/menu/tooltip, so those must only ever be set through the applier.

use crate::managers::history::{HistoryEntry, HistoryManager};
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::meeting::types::{AllowedMeetingAction, MeetingPhase, MeetingSessionSnapshot};
use crate::settings;
use crate::tray_i18n::{get_tray_translations, TrayStrings};
use log::{debug, error, info, trace, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Emitter, Manager, Theme};
use tauri_plugin_clipboard_manager::ClipboardExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayIconState {
    Idle,
    Recording,
    Transcribing,
}

pub(crate) const TRAY_STATUS_MENU_ID: &str = "tray_status";
pub(crate) const OPEN_SONA_MENU_ID: &str = "open_sona";
pub(crate) const START_DICTATION_MENU_ID: &str = "start_dictation";
pub(crate) const STOP_DICTATION_MENU_ID: &str = "stop_dictation";
pub(crate) const CANCEL_TRANSCRIPTION_MENU_ID: &str = "cancel_transcription";
pub(crate) const START_MEETING_NOTES_MENU_ID: &str = "start_meeting_notes";
pub(crate) const OPEN_MEETING_NOTES_MENU_ID: &str = "open_meeting_notes";
pub(crate) const STOP_MEETING_NOTES_MENU_ID: &str = "stop_meeting_notes";
pub(crate) const CANCEL_MEETING_NOTES_MENU_ID: &str = "cancel_meeting_notes";
pub(crate) const COPY_LAST_TRANSCRIPT_MENU_ID: &str = "copy_last_transcript";

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct TraySettingsRequestedEvent;

impl tauri_specta::Event for TraySettingsRequestedEvent {
    const NAME: &'static str = "tray:settings-requested";
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct TrayCopyFailedEvent;

impl tauri_specta::Event for TrayCopyFailedEvent {
    const NAME: &'static str = "tray:copy-failed";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MeetingMenuState {
    phase: MeetingPhase,
    can_stop: bool,
    can_cancel: bool,
}

impl MeetingMenuState {
    fn from_snapshot(snapshot: &MeetingSessionSnapshot) -> Self {
        Self::from_phase_and_actions(snapshot.phase, &snapshot.allowed_actions)
    }

    fn from_phase_and_actions(
        phase: MeetingPhase,
        allowed_actions: &[AllowedMeetingAction],
    ) -> Self {
        Self {
            phase,
            can_stop: allowed_actions.contains(&AllowedMeetingAction::Stop),
            can_cancel: allowed_actions.iter().any(|action| {
                matches!(
                    action,
                    AllowedMeetingAction::CancelPreflight | AllowedMeetingAction::Discard
                )
            }),
        }
    }

    const fn status(self) -> MenuStatus {
        match self.phase {
            MeetingPhase::Preflight => MenuStatus::MeetingPreflight,
            MeetingPhase::Starting => MenuStatus::MeetingStarting,
            MeetingPhase::CapturingRecording
            | MeetingPhase::CapturingPausing
            | MeetingPhase::CapturingResuming => MenuStatus::MeetingRecording,
            MeetingPhase::CapturingPaused => MenuStatus::MeetingPaused,
            MeetingPhase::Stopping | MeetingPhase::Processing | MeetingPhase::Deleting => {
                MenuStatus::MeetingProcessing
            }
            MeetingPhase::ReviewReady => MenuStatus::MeetingReady,
            MeetingPhase::RecoveryRequired => MenuStatus::MeetingRecovery,
        }
    }

    const fn icon_state(self) -> TrayIconState {
        match self.phase {
            MeetingPhase::Starting
            | MeetingPhase::CapturingRecording
            | MeetingPhase::CapturingPausing
            | MeetingPhase::CapturingPaused
            | MeetingPhase::CapturingResuming => TrayIconState::Recording,
            MeetingPhase::Stopping | MeetingPhase::Processing | MeetingPhase::Deleting => {
                TrayIconState::Transcribing
            }
            MeetingPhase::Preflight
            | MeetingPhase::ReviewReady
            | MeetingPhase::RecoveryRequired => TrayIconState::Idle,
        }
    }

    const fn primary_action(self) -> MenuAction {
        if self.can_stop {
            MenuAction::StopMeetingNotes
        } else {
            MenuAction::OpenMeetingNotes
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuStatus {
    Ready,
    Recording,
    Transcribing,
    MeetingPreflight,
    MeetingStarting,
    MeetingRecording,
    MeetingPaused,
    MeetingProcessing,
    MeetingReady,
    MeetingRecovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuAction {
    OpenSona,
    StartDictation,
    StopDictation,
    CancelTranscription,
    StartMeetingNotes,
    OpenMeetingNotes,
    StopMeetingNotes,
    CancelMeetingNotes,
    CopyLastTranscript,
    Settings,
    Quit,
}

impl MenuAction {
    const fn id(self) -> &'static str {
        match self {
            Self::OpenSona => OPEN_SONA_MENU_ID,
            Self::StartDictation => START_DICTATION_MENU_ID,
            Self::StopDictation => STOP_DICTATION_MENU_ID,
            Self::CancelTranscription => CANCEL_TRANSCRIPTION_MENU_ID,
            Self::StartMeetingNotes => START_MEETING_NOTES_MENU_ID,
            Self::OpenMeetingNotes => OPEN_MEETING_NOTES_MENU_ID,
            Self::StopMeetingNotes => STOP_MEETING_NOTES_MENU_ID,
            Self::CancelMeetingNotes => CANCEL_MEETING_NOTES_MENU_ID,
            Self::CopyLastTranscript => COPY_LAST_TRANSCRIPT_MENU_ID,
            Self::Settings => "settings",
            Self::Quit => "quit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuRow {
    Status(MenuStatus),
    SecureInputWarning,
    Separator,
    Action { action: MenuAction, enabled: bool },
    ModelSubmenu { enabled: bool },
}

fn menu_status(icon_state: TrayIconState, meeting: Option<MeetingMenuState>) -> MenuStatus {
    meeting
        .map(MeetingMenuState::status)
        .unwrap_or(match icon_state {
            TrayIconState::Idle => MenuStatus::Ready,
            TrayIconState::Recording => MenuStatus::Recording,
            TrayIconState::Transcribing => MenuStatus::Transcribing,
        })
}

fn menu_rows(
    icon_state: TrayIconState,
    warning: bool,
    meeting: Option<MeetingMenuState>,
) -> Vec<MenuRow> {
    let idle = icon_state == TrayIconState::Idle && meeting.is_none();
    let (dictation_action, dictation_enabled) = if meeting.is_some() {
        (MenuAction::StartDictation, false)
    } else {
        match icon_state {
            TrayIconState::Idle => (MenuAction::StartDictation, true),
            TrayIconState::Recording => (MenuAction::StopDictation, true),
            TrayIconState::Transcribing => (MenuAction::CancelTranscription, true),
        }
    };
    let (meeting_action, meeting_enabled) = meeting
        .map(|state| (state.primary_action(), true))
        .unwrap_or((MenuAction::StartMeetingNotes, idle));

    let mut rows = Vec::with_capacity(if warning { 16 } else { 14 });
    rows.push(MenuRow::Status(menu_status(icon_state, meeting)));
    if warning {
        rows.push(MenuRow::SecureInputWarning);
    }
    rows.extend([
        MenuRow::Separator,
        MenuRow::Action {
            action: MenuAction::OpenSona,
            enabled: true,
        },
        MenuRow::Separator,
        MenuRow::Action {
            action: dictation_action,
            enabled: dictation_enabled,
        },
        MenuRow::Action {
            action: meeting_action,
            enabled: meeting_enabled,
        },
    ]);
    if meeting.is_some_and(|state| state.can_cancel) {
        rows.push(MenuRow::Action {
            action: MenuAction::CancelMeetingNotes,
            enabled: true,
        });
    }
    rows.extend([
        MenuRow::Separator,
        MenuRow::Action {
            action: MenuAction::CopyLastTranscript,
            enabled: true,
        },
        MenuRow::ModelSubmenu { enabled: idle },
        MenuRow::Separator,
        MenuRow::Action {
            action: MenuAction::Settings,
            enabled: true,
        },
        MenuRow::Action {
            action: MenuAction::Quit,
            enabled: true,
        },
    ]);
    rows
}

/// Everything the tray *menu* (and tooltip) depends on. When two snapshots
/// compare equal the menu is not rebuilt.
///
/// The key is the *rendered* row description, not the activity and meeting
/// state that produced it. Several distinct states render an identical menu —
/// a dictation start, stop or cancel while a meeting owns the tray, or a
/// meeting phase that shares its status line and allowed actions with the
/// phase before it — and keying on the raw state rebuilt the whole native menu
/// for each of them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MenuInputs {
    rows: Vec<MenuRow>,
    model_loaded: bool,
    selected_model: String,
    /// `(id, name)` of downloaded models, sorted by name.
    downloaded_models: Vec<(String, String)>,
    locale: String,
}

/// Complete description of what the tray should look like.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TrayDesired {
    icon_path: &'static str,
    /// Dictation activity, for the apply log only. What the menu shows for it
    /// is already in `menu.rows`.
    activity: TrayIconState,
    menu: MenuInputs,
}

struct TrayInner {
    /// Dictation activity is updated by [`set_tray_state`].
    icon_state: TrayIconState,
    /// Meeting presentation derived from the latest authoritative snapshot.
    meeting: Option<MeetingMenuState>,
    /// Latest computed snapshot, waiting to be (or just) applied.
    desired: Option<TrayDesired>,
    /// Icon the native tray currently shows. Only updated when `set_icon`
    /// succeeds, so a failed update is retried on the next sync.
    applied_icon: Option<&'static str>,
    /// Inputs the native menu was last successfully built from. The tooltip
    /// is derived from the same inputs and set best-effort alongside the menu;
    /// it is not tracked separately.
    applied_menu: Option<MenuInputs>,
    /// An apply is scheduled on the main thread.
    pending: bool,
    /// Decoded icons by resource path so the main thread never touches disk.
    icons: HashMap<&'static str, Image<'static>>,
    /// Handed out to each sync request in trigger order, so a slow request
    /// can't overwrite the snapshot of one that was triggered after it.
    next_seq: u64,
    /// Sequence number of the request that produced `desired`.
    desired_seq: u64,
}

impl TrayInner {
    /// Whether the native menu has to be rebuilt for `desired` to be on
    /// screen. The single apply-time gate on menu work: everything the menu
    /// renders lives in [`MenuInputs`], so an equal snapshot means the menu
    /// already displayed is the one wanted.
    fn menu_needs_rebuild(&self, desired: &MenuInputs) -> bool {
        self.applied_menu.as_ref() != Some(desired)
    }
}

/// Tauri managed state owning the tray's desired/applied snapshots.
pub struct TrayState(Mutex<TrayInner>);

impl TrayState {
    pub fn new() -> Self {
        Self(Mutex::new(TrayInner {
            icon_state: TrayIconState::Idle,
            meeting: None,
            desired: None,
            applied_icon: None,
            applied_menu: None,
            pending: false,
            icons: HashMap::new(),
            next_seq: 0,
            desired_seq: 0,
        }))
    }

    fn lock(&self) -> MutexGuard<'_, TrayInner> {
        self.0.lock().unwrap_or_else(|poisoned| {
            warn!("Tray state mutex was poisoned, recovering");
            poisoned.into_inner()
        })
    }
}

impl Default for TrayState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AppTheme {
    Dark,
    Light,
    Colored, // Pink/colored theme for Linux
}

/// Gets the current app theme, with Linux defaulting to Colored theme
pub fn get_current_theme(app: &AppHandle) -> AppTheme {
    if cfg!(target_os = "linux") {
        // On Linux, always use the colored theme
        AppTheme::Colored
    } else {
        // On Windows the tray icon sits on the taskbar, which follows the
        // *system* theme (SystemUsesLightTheme), not the app theme. With the
        // "Custom" personalization mode the two can differ (e.g. dark taskbar
        // + light apps), and the window theme would pick an icon that is
        // invisible against the taskbar.
        #[cfg(target_os = "windows")]
        if let Some(theme) = windows_taskbar_theme() {
            return theme;
        }

        // On other platforms, map system theme to our app theme
        if let Some(main_window) = app.get_webview_window("main") {
            match main_window.theme().unwrap_or(Theme::Dark) {
                Theme::Light => AppTheme::Light,
                Theme::Dark => AppTheme::Dark,
                _ => AppTheme::Dark, // Default fallback
            }
        } else {
            AppTheme::Dark
        }
    }
}

/// Reads the Windows taskbar theme from the registry.
///
/// Returns None if the value is missing (older Windows 10 builds default to a
/// dark taskbar there, but falling back to the window theme is safer than
/// guessing).
#[cfg(target_os = "windows")]
fn windows_taskbar_theme() -> Option<AppTheme> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let personalize = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
        .ok()?;
    let system_uses_light: u32 = personalize.get_value("SystemUsesLightTheme").ok()?;
    Some(if system_uses_light == 1 {
        AppTheme::Light
    } else {
        AppTheme::Dark
    })
}

/// Gets the tray icon path for the current platform and activity.
///
/// macOS uses one black-alpha template family. AppKit applies the correct tint
/// for the menu bar appearance. Windows and Linux retain their existing assets
/// and theme selection.
pub fn get_icon_path(theme: AppTheme, state: TrayIconState, warning: bool) -> &'static str {
    #[cfg(target_os = "macos")]
    {
        let _ = theme;
        if warning && state == TrayIconState::Idle {
            return "resources/tray_template_idle_warning.png";
        }
        match state {
            TrayIconState::Idle => "resources/tray_template_idle.png",
            TrayIconState::Recording => "resources/tray_template_recording.png",
            TrayIconState::Transcribing => "resources/tray_template_transcribing.png",
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if warning && state == TrayIconState::Idle {
            return match theme {
                AppTheme::Dark => "resources/tray_idle_warning.png",
                AppTheme::Light => "resources/tray_idle_warning_dark.png",
                AppTheme::Colored => "resources/sona.png",
            };
        }
        match (theme, state) {
            (AppTheme::Dark, TrayIconState::Idle) => "resources/tray_idle.png",
            (AppTheme::Dark, TrayIconState::Recording) => "resources/tray_recording.png",
            (AppTheme::Dark, TrayIconState::Transcribing) => "resources/tray_transcribing.png",
            (AppTheme::Light, TrayIconState::Idle) => "resources/tray_idle_dark.png",
            (AppTheme::Light, TrayIconState::Recording) => "resources/tray_recording_dark.png",
            (AppTheme::Light, TrayIconState::Transcribing) => {
                "resources/tray_transcribing_dark.png"
            }
            (AppTheme::Colored, TrayIconState::Idle) => "resources/sona.png",
            (AppTheme::Colored, TrayIconState::Recording) => "resources/recording.png",
            (AppTheme::Colored, TrayIconState::Transcribing) => "resources/transcribing.png",
        }
    }
}

/// Emitted whenever the dictation activity changes. The native shell has no
/// tray to read, and the overlay states depend on the overlay style, so this
/// is the one style-independent record of what the pipeline is doing.
pub const DICTATION_ACTIVITY_EVENT: &str = "dictation-activity";

#[derive(Clone, Serialize)]
struct DictationActivity {
    state: &'static str,
}

/// Sets the current dictation activity shown by the tray.
pub fn set_tray_state(app: &AppHandle, state: TrayIconState) {
    let activity = DictationActivity {
        state: match state {
            TrayIconState::Idle => "idle",
            TrayIconState::Recording => "recording",
            TrayIconState::Transcribing => "transcribing",
        },
    };
    if let Err(error) = app.emit(DICTATION_ACTIVITY_EVENT, activity) {
        error!("Failed to emit {DICTATION_ACTIVITY_EVENT}: {error}");
    }
    sync_tray_with(app, |inner| inner.icon_state = state);
}

/// Applies the meeting presentation derived from the latest session snapshot.
/// Pass `None` after the current preflight or session is removed.
pub fn set_meeting_tray_snapshot(app: &AppHandle, snapshot: Option<&MeetingSessionSnapshot>) {
    let meeting = snapshot.map(MeetingMenuState::from_snapshot);
    sync_tray_with(app, move |inner| inner.meeting = meeting);
}

/// Re-syncs the tray after something other than the recording state changed
/// (theme, Secure Input warning). The recording state itself is preserved.
pub fn refresh_tray_icon(app: &AppHandle) {
    sync_tray(app);
}

/// Re-syncs the tray after something the menu depends on changed (model
/// list/selection/loaded state, language, settings).
pub fn update_tray_menu(app: &AppHandle) {
    sync_tray(app);
}

/// Records the current desired tray state and schedules one apply on the main
/// thread (or lets an already-pending apply pick it up). Never blocks on the
/// main thread.
///
/// The snapshot (settings, model list, loaded state) is computed on the
/// *calling* thread on purpose: the main-thread applier must not take manager
/// locks that a worker may hold across slow work (see #1716).
pub fn sync_tray(app: &AppHandle) {
    sync_tray_with(app, |_| {});
}

fn sync_tray_with(app: &AppHandle, update: impl FnOnce(&mut TrayInner)) {
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };

    // Record intent and claim a sequence number in one critical section, so
    // sequence order == the order in which state changes were requested.
    let (seq, icon_state, meeting) = {
        let mut inner = state.lock();
        update(&mut inner);
        inner.next_seq += 1;
        (inner.next_seq, inner.icon_state, inner.meeting)
    };

    // Tray not built yet (early secure-input monitor callbacks). The intent
    // is kept and picked up by the first sync after the tray exists.
    if app.try_state::<TrayIcon>().is_none() {
        return;
    }

    let desired = compute_desired(app, icon_state, meeting);

    // Decode the icon off the main thread, once per path, outside the lock.
    let needs_icon = !state.lock().icons.contains_key(desired.icon_path);
    let loaded_icon = if needs_icon {
        match load_tray_icon(app, desired.icon_path) {
            Ok(image) => Some(image),
            Err(err) => {
                error!("Failed to load tray icon '{}': {err}", desired.icon_path);
                None
            }
        }
    } else {
        None
    };

    let schedule = {
        let mut inner = state.lock();
        if let Some(image) = loaded_icon {
            inner.icons.insert(desired.icon_path, image);
        }
        if seq < inner.desired_seq {
            trace!(
                "tray sync: request {seq} superseded by {}",
                inner.desired_seq
            );
            return;
        }
        inner.desired = Some(desired);
        inner.desired_seq = seq;
        !std::mem::replace(&mut inner.pending, true)
    };

    if schedule {
        post_apply(app);
    } else {
        trace!("tray sync: apply already pending");
    }
}

fn compute_desired(
    app: &AppHandle,
    icon_state: TrayIconState,
    meeting: Option<MeetingMenuState>,
) -> TrayDesired {
    let settings = settings::get_settings(app);
    let theme = get_current_theme(app);
    let warning = crate::secure_input::tray_warning_active(app);
    let model_loaded = app.state::<Arc<TranscriptionManager>>().is_model_loaded();

    let downloaded_models = app.state::<Arc<ModelManager>>().downloaded_model_labels();
    let visible_icon_state = meeting
        .map(MeetingMenuState::icon_state)
        .unwrap_or(icon_state);

    TrayDesired {
        icon_path: get_icon_path(theme, visible_icon_state, warning),
        activity: icon_state,
        menu: MenuInputs {
            rows: menu_rows(icon_state, warning, meeting),
            model_loaded,
            selected_model: settings.selected_model,
            downloaded_models,
            locale: settings.app_language,
        },
    }
}

fn post_apply(app: &AppHandle) {
    let handle = app.clone();
    if let Err(err) = app.run_on_main_thread(move || apply_on_main(&handle)) {
        // Event loop is gone (shutdown). Clear `pending` so a later call, if
        // any, doesn't wait forever for an apply that will never run.
        error!("Failed to dispatch tray update to the main thread: {err}");
        if let Some(state) = app.try_state::<TrayState>() {
            state.lock().pending = false;
        }
    }
}

/// The single writer to the native tray. Runs on the main thread.
fn apply_on_main(app: &AppHandle) {
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };
    let Some(tray) = app.try_state::<TrayIcon>() else {
        return;
    };

    let started = Instant::now();
    let (desired, icon, icon_changed, menu_changed) = {
        let mut inner = state.lock();
        inner.pending = false;
        let Some(desired) = inner.desired.clone() else {
            return;
        };
        let icon_changed = inner.applied_icon != Some(desired.icon_path);
        let menu_changed = inner.menu_needs_rebuild(&desired.menu);
        if !icon_changed && !menu_changed {
            trace!("tray apply: nothing changed");
            return;
        }
        let icon = inner.icons.get(desired.icon_path).cloned();
        (desired, icon, icon_changed, menu_changed)
    };

    // Each part is recorded as applied only if its native call succeeded, so a
    // transient failure is retried on the next sync instead of being
    // remembered as displayed.
    let mut icon_ok = false;
    if icon_changed {
        match icon {
            Some(image) => match tray.set_icon_with_as_template(Some(image), true) {
                Ok(()) => icon_ok = true,
                Err(err) => error!("Failed to update tray icon '{}': {err}", desired.icon_path),
            },
            None => error!("Tray icon '{}' is not loaded", desired.icon_path),
        }
    }

    let mut menu_ok = false;
    if menu_changed {
        match build_menu(app, &desired.menu) {
            Ok((menu, tooltip)) => match tray.set_menu(Some(menu)) {
                Ok(()) => {
                    menu_ok = true;
                    // Best-effort: logged, not retried. The tooltip is cosmetic
                    // and can only fail on Windows, where a failing
                    // Shell_NotifyIcon call means the icon is failing too.
                    // Gating `menu_ok` on it would re-run the full menu
                    // rebuild on every sync for the cheapest mutation.
                    if let Err(err) = tray.set_tooltip(Some(tooltip)) {
                        error!("Failed to set tray tooltip: {err}");
                    }
                }
                Err(err) => error!("Failed to set tray menu: {err}"),
            },
            Err(err) => error!("Failed to build tray menu: {err}"),
        }
    }

    {
        let mut inner = state.lock();
        if icon_ok {
            inner.applied_icon = Some(desired.icon_path);
        }
        if menu_ok {
            inner.applied_menu = Some(desired.menu.clone());
        }
    }

    debug!(
        "tray apply: icon={} menu={} activity={:?} took={:?}",
        if icon_changed {
            desired.icon_path
        } else {
            "unchanged"
        },
        if menu_changed { "rebuilt" } else { "unchanged" },
        desired.activity,
        started.elapsed()
    );
}

/// Loads a tray icon after resource resolution. The resolver wrapper and
/// startup fallback both use this path-selection behavior.
pub(crate) fn load_tray_icon_from_path(
    _resource_path: &str,
    resolved: Option<PathBuf>,
) -> tauri::Result<Image<'static>> {
    let resolved = resolved.filter(|path| path.is_file());

    #[cfg(debug_assertions)]
    let resolved = resolved.or_else(|| {
        let development_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(_resource_path);
        development_path.is_file().then_some(development_path)
    });

    Image::from_path(resolved.ok_or(tauri::Error::UnknownPath)?).map(Image::to_owned)
}

pub(crate) fn load_tray_icon(
    app: &AppHandle,
    resource_path: &str,
) -> tauri::Result<Image<'static>> {
    let resolved = app
        .path()
        .resolve(resource_path, tauri::path::BaseDirectory::Resource)
        .ok();
    load_tray_icon_from_path(resource_path, resolved)
}

fn bundled_idle_tray_icon() -> Image<'static> {
    // PANIC: This compiled-in PNG is validated by the tray fallback test.
    Image::from_bytes(include_bytes!("../resources/tray_idle.png"))
        .expect("bundled tray fallback icon is valid")
}

pub(crate) fn load_tray_icon_or_fallback(
    resource_path: &str,
    load: impl FnOnce(&str) -> tauri::Result<Image<'static>>,
) -> Image<'static> {
    load(resource_path).unwrap_or_else(|error| {
        error!(
            "Could not load initial tray icon '{resource_path}': {error}; using bundled idle fallback"
        );
        bundled_idle_tray_icon()
    })
}

pub(crate) fn load_initial_tray_icon(app: &AppHandle, resource_path: &str) -> Image<'static> {
    load_tray_icon_or_fallback(resource_path, |resource_path| {
        load_tray_icon(app, resource_path)
    })
}

pub fn tray_tooltip() -> String {
    version_label()
}

fn version_label() -> String {
    if cfg!(debug_assertions) {
        format!("Sona v{} (Dev)", env!("CARGO_PKG_VERSION"))
    } else {
        format!("Sona v{}", env!("CARGO_PKG_VERSION"))
    }
}

fn menu_status_text(status: MenuStatus, strings: &TrayStrings) -> &str {
    match status {
        MenuStatus::Ready => &strings.status_ready,
        MenuStatus::Recording => &strings.status_recording,
        MenuStatus::Transcribing => &strings.status_transcribing,
        MenuStatus::MeetingPreflight => &strings.status_meeting_preflight,
        MenuStatus::MeetingStarting => &strings.status_meeting_starting,
        MenuStatus::MeetingRecording => &strings.status_meeting_recording,
        MenuStatus::MeetingPaused => &strings.status_meeting_paused,
        MenuStatus::MeetingProcessing => &strings.status_meeting_processing,
        MenuStatus::MeetingReady => &strings.status_meeting_ready,
        MenuStatus::MeetingRecovery => &strings.status_meeting_recovery,
    }
}

fn menu_action_text(action: MenuAction, strings: &TrayStrings) -> &str {
    match action {
        MenuAction::OpenSona => &strings.open_sona,
        MenuAction::StartDictation => &strings.start_dictation,
        MenuAction::StopDictation => &strings.stop_dictation,
        MenuAction::CancelTranscription => &strings.cancel_transcription,
        MenuAction::StartMeetingNotes => &strings.start_meeting_notes,
        MenuAction::OpenMeetingNotes => &strings.open_meeting_notes,
        MenuAction::StopMeetingNotes => &strings.stop_meeting_notes,
        MenuAction::CancelMeetingNotes => &strings.cancel_meeting_notes,
        MenuAction::CopyLastTranscript => &strings.copy_last_transcript,
        MenuAction::Settings => &strings.settings,
        MenuAction::Quit => &strings.quit,
    }
}

fn menu_action_accelerator(action: MenuAction) -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    let (settings, quit) = ("Cmd+,", "Cmd+Q");
    #[cfg(not(target_os = "macos"))]
    let (settings, quit) = ("Ctrl+,", "Ctrl+Q");

    match action {
        MenuAction::Settings => Some(settings),
        MenuAction::Quit => Some(quit),
        _ => None,
    }
}

fn build_model_submenu(
    app: &AppHandle,
    inputs: &MenuInputs,
    strings: &TrayStrings,
    enabled: bool,
) -> tauri::Result<Submenu<tauri::Wry>> {
    let active_model = inputs
        .downloaded_models
        .iter()
        .find(|(id, _)| *id == inputs.selected_model)
        .map(|(_, name)| name.as_str());
    let label = active_model
        .map(|name| format!("{}: {name}", strings.model))
        .unwrap_or_else(|| strings.model.clone());
    let submenu = Submenu::with_id(app, "model_submenu", label, enabled)?;

    for (id, name) in &inputs.downloaded_models {
        let item = CheckMenuItem::with_id(
            app,
            format!("model_select:{id}"),
            name,
            true,
            *id == inputs.selected_model,
            None::<&str>,
        )?;
        submenu.append(&item)?;
    }
    if !inputs.downloaded_models.is_empty() {
        submenu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    submenu.append(&MenuItem::with_id(
        app,
        "unload_model",
        &strings.unload_model,
        inputs.model_loaded,
        None::<&str>,
    )?)?;

    Ok(submenu)
}

/// Builds the native tray menu from the pure row description.
fn build_menu(app: &AppHandle, inputs: &MenuInputs) -> tauri::Result<(Menu<tauri::Wry>, String)> {
    let strings = get_tray_translations(Some(inputs.locale.clone()));
    let menu = Menu::new(app)?;
    // The tooltip says the same two things the first rows do, read off those
    // rows so there is one description of the menu, not two.
    let mut tooltip_status = None;
    let mut tooltip_warning = false;

    for row in &inputs.rows {
        match *row {
            MenuRow::Status(status) => {
                tooltip_status = Some(status);
                menu.append(&MenuItem::with_id(
                    app,
                    TRAY_STATUS_MENU_ID,
                    menu_status_text(status, &strings),
                    false,
                    None::<&str>,
                )?)?;
            }
            MenuRow::SecureInputWarning => {
                tooltip_warning = true;
                menu.append(&MenuItem::with_id(
                    app,
                    "secure_input_warning",
                    &strings.secure_input_warning,
                    true,
                    None::<&str>,
                )?)?;
            }
            MenuRow::Separator => menu.append(&PredefinedMenuItem::separator(app)?)?,
            MenuRow::Action { action, enabled } => menu.append(&MenuItem::with_id(
                app,
                action.id(),
                menu_action_text(action, &strings),
                enabled,
                menu_action_accelerator(action),
            )?)?,
            MenuRow::ModelSubmenu { enabled } => {
                menu.append(&build_model_submenu(app, inputs, &strings, enabled)?)?
            }
        }
    }

    let mut tooltip = match tooltip_status {
        Some(status) => format!(
            "{}: {}",
            version_label(),
            menu_status_text(status, &strings)
        ),
        None => version_label(),
    };
    if tooltip_warning {
        tooltip.push_str(": ");
        tooltip.push_str(&strings.secure_input_warning);
    }
    Ok((menu, tooltip))
}

fn last_transcript_text(entry: &HistoryEntry) -> &str {
    entry
        .post_processed_text
        .as_deref()
        .unwrap_or(&entry.transcription_text)
}

pub(crate) fn set_tray_visibility_with(
    visible: bool,
    set_visible: impl FnOnce(bool) -> tauri::Result<()>,
) -> tauri::Result<()> {
    set_visible(visible)
}

pub fn set_tray_visibility(app: &AppHandle, visible: bool) {
    let tray = app.state::<TrayIcon>();
    if let Err(error) = set_tray_visibility_with(visible, |visible| tray.set_visible(visible)) {
        error!("Failed to set tray visibility: {error}");
    } else {
        info!("Tray visibility set to: {visible}");
    }
}

/// Recovery for the macOS tray-disappearance bug (#1948, tauri-apps/tauri#12060):
/// the `NSStatusItem` can silently vanish with no error surfaced to the app.
/// Hiding and re-showing the tray recreates it with its current icon, menu and
/// tooltip. Called when the user "relaunches" Sona while it is already running
/// (`RunEvent::Reopen` for Spotlight/Finder/Dock, the single-instance callback
/// for a second process) — the natural "where did my icon go?" moment — so a
/// relaunch brings the icon back without a full quit.
#[cfg(target_os = "macos")]
pub(crate) fn recreate_tray_visibility_with(
    mut set_visible: impl FnMut(bool) -> tauri::Result<()>,
) -> tauri::Result<()> {
    set_visible(false)?;
    set_visible(true)
}

#[cfg(target_os = "macos")]
pub fn recreate_tray_icon(app: &AppHandle) {
    let no_tray = app
        .try_state::<crate::cli::CliArgs>()
        .map(|args| args.no_tray)
        .unwrap_or(false);
    if no_tray || !settings::get_settings(app).show_tray_icon {
        return;
    }
    let Some(tray) = app.try_state::<TrayIcon>() else {
        return;
    };
    info!("Recreating tray icon on relaunch");
    if let Err(error) = recreate_tray_visibility_with(|visible| tray.set_visible(visible)) {
        error!("Failed to recreate tray icon: {error}");
    }
}

pub(crate) fn open_settings<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    crate::show_main_window(app);
    <TraySettingsRequestedEvent as tauri_specta::Event>::emit(&TraySettingsRequestedEvent, app)
        .unwrap_or_else(|error| error!("Failed to request Settings from tray: {error}"));
}

fn show_tray_copy_failure<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    crate::show_main_window(app);
    <TrayCopyFailedEvent as tauri_specta::Event>::emit(&TrayCopyFailedEvent, app)
        .unwrap_or_else(|error| error!("Failed to report failed tray copy: {error}"));
}

pub fn copy_last_transcript(app: &AppHandle) {
    let history_manager = app.state::<Arc<HistoryManager>>();
    let entry = match history_manager.get_latest_completed_entry() {
        Ok(Some(entry)) => entry,
        Ok(None) => {
            warn!("No completed transcription history entries available for tray copy.");
            show_tray_copy_failure(app);
            return;
        }
        Err(err) => {
            error!(
                "Failed to fetch last completed transcription entry: {}",
                err
            );
            show_tray_copy_failure(app);
            return;
        }
    };

    let text = last_transcript_text(&entry);
    if text.trim().is_empty() {
        warn!("Last completed transcription is empty; skipping tray copy.");
        show_tray_copy_failure(app);
        return;
    }

    if let Err(err) = app.clipboard().write_text(text) {
        error!("Failed to copy last transcript to clipboard: {}", err);
        show_tray_copy_failure(app);
        return;
    }

    info!("Copied last transcript to clipboard via tray.");
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::recreate_tray_visibility_with;
    use super::{
        bundled_idle_tray_icon, last_transcript_text, load_tray_icon_from_path,
        load_tray_icon_or_fallback, menu_rows, open_settings, set_tray_visibility_with,
        show_tray_copy_failure, MeetingMenuState, MenuAction, MenuInputs, MenuRow, MenuStatus,
        TrayIconState, TrayState,
    };
    use crate::managers::history::HistoryEntry;
    use crate::meeting::types::{AllowedMeetingAction, MeetingPhase};
    use std::sync::{Arc, Mutex};
    use tauri::{image::Image, Listener};

    fn build_entry(transcription: &str, post_processed: Option<&str>) -> HistoryEntry {
        HistoryEntry {
            id: 1,
            file_name: "sona-1.wav".to_string(),
            timestamp: 0,
            saved: false,
            title: "Recording".to_string(),
            transcription_text: transcription.to_string(),
            post_processed_text: post_processed.map(|text| text.to_string()),
            post_process_requested: false,
            parent_id: None,
            match_kind: None,
        }
    }

    fn inputs(icon_state: TrayIconState) -> MenuInputs {
        menu_inputs(icon_state, None)
    }

    fn menu_inputs(icon_state: TrayIconState, meeting: Option<MeetingMenuState>) -> MenuInputs {
        MenuInputs {
            rows: menu_rows(icon_state, false, meeting),
            model_loaded: true,
            selected_model: "small".to_string(),
            downloaded_models: vec![("small".to_string(), "Small".to_string())],
            locale: "en".to_string(),
        }
    }

    /// Runs a scripted sequence of desired menu snapshots through the real
    /// apply-time gate and returns how many steps rebuilt the native menu.
    fn rebuilds(script: &[MenuInputs]) -> usize {
        let state = TrayState::new();
        let mut rebuilt = 0;
        for desired in script {
            let mut inner = state.lock();
            if inner.menu_needs_rebuild(desired) {
                rebuilt += 1;
                inner.applied_menu = Some(desired.clone());
            }
        }
        rebuilt
    }

    fn actions(icon_state: TrayIconState) -> Vec<(MenuAction, bool)> {
        menu_rows(icon_state, false, None)
            .into_iter()
            .filter_map(|row| match row {
                MenuRow::Action { action, enabled } => Some((action, enabled)),
                _ => None,
            })
            .collect()
    }

    fn meeting_state(phase: MeetingPhase, can_stop: bool, can_cancel: bool) -> MeetingMenuState {
        MeetingMenuState {
            phase,
            can_stop,
            can_cancel,
        }
    }

    fn meeting_actions(meeting: MeetingMenuState) -> Vec<(MenuAction, bool)> {
        menu_rows(TrayIconState::Idle, false, Some(meeting))
            .into_iter()
            .filter_map(|row| match row {
                MenuRow::Action { action, enabled } => Some((action, enabled)),
                _ => None,
            })
            .collect()
    }

    fn meeting_model_enabled(meeting: MeetingMenuState) -> bool {
        menu_rows(TrayIconState::Idle, false, Some(meeting))
            .into_iter()
            .find_map(|row| match row {
                MenuRow::ModelSubmenu { enabled } => Some(enabled),
                _ => None,
            })
            .expect("menu must contain the model submenu")
    }

    fn model_enabled(icon_state: TrayIconState) -> bool {
        menu_rows(icon_state, false, None)
            .into_iter()
            .find_map(|row| match row {
                MenuRow::ModelSubmenu { enabled } => Some(enabled),
                _ => None,
            })
            .expect("menu must contain the model submenu")
    }

    fn template_alpha(bytes: &[u8]) -> Vec<u8> {
        let image = Image::from_bytes(bytes).expect("template icon must decode");
        assert_eq!((image.width(), image.height()), (36, 36));
        assert_eq!(image.rgba().len(), 36 * 36 * 4);

        let mut min_x = 36usize;
        let mut min_y = 36usize;
        let mut max_x = 0usize;
        let mut max_y = 0usize;
        let mut transparent = false;
        let mut opaque = false;
        let mut alpha = Vec::with_capacity(36 * 36);
        for (index, pixel) in image.rgba().chunks_exact(4).enumerate() {
            let value = pixel[3];
            alpha.push(value);
            transparent |= value == 0;
            opaque |= value == 255;
            if value == 0 {
                continue;
            }
            assert_eq!(&pixel[..3], &[0, 0, 0]);
            let x = index % 36;
            let y = index / 36;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }

        assert!(transparent && opaque);
        assert!(min_x >= 1 && min_y >= 3);
        assert!(max_x <= 34 && max_y <= 32);
        alpha
    }

    #[test]
    fn tray_actions_emit_their_observable_frontend_events() {
        use super::{TrayCopyFailedEvent, TraySettingsRequestedEvent};

        let event_builder = tauri_specta::Builder::<tauri::test::MockRuntime>::new().events(
            tauri_specta::collect_events![TraySettingsRequestedEvent, TrayCopyFailedEvent],
        );
        let app = tauri::test::mock_app();
        event_builder.mount_events(&app);
        let handle = app.handle();
        let observed = Arc::new(Mutex::new(Vec::new()));

        let settings_observed = Arc::clone(&observed);
        handle.listen("tray:settings-requested", move |_| {
            settings_observed.lock().unwrap().push("settings");
        });
        let copy_observed = Arc::clone(&observed);
        handle.listen("tray:copy-failed", move |_| {
            copy_observed.lock().unwrap().push("copy-failed");
        });

        open_settings(handle);
        show_tray_copy_failure(handle);

        assert_eq!(
            observed.lock().unwrap().as_slice(),
            ["settings", "copy-failed"],
            "a tray action must reach the frontend event that makes its result visible"
        );
    }

    #[test]
    fn uses_post_processed_text_when_available() {
        let entry = build_entry("raw", Some("processed"));
        assert_eq!(last_transcript_text(&entry), "processed");
    }

    #[test]
    fn falls_back_to_raw_transcription() {
        let entry = build_entry("raw", None);
        assert_eq!(last_transcript_text(&entry), "raw");
    }

    #[test]
    fn tray_icon_resolution_failure_returns_an_error_for_the_requested_resource() {
        let resource_path = "resources/missing.png";
        let result = load_tray_icon_from_path(resource_path, None);

        assert!(result.is_err());
    }

    #[test]
    fn missing_tray_icon_uses_the_bundled_idle_fallback() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let missing = dir.path().join("does_not_exist.png");
        let resource_path = "resources/missing.png";
        let fallback = load_tray_icon_or_fallback(resource_path, |requested_path| {
            assert_eq!(requested_path, resource_path);
            load_tray_icon_from_path(requested_path, Some(missing))
        });
        let expected = bundled_idle_tray_icon();

        assert_eq!(fallback.rgba(), expected.rgba());
    }

    #[test]
    fn tray_visibility_supports_hide_and_show() {
        let mut calls = Vec::new();
        set_tray_visibility_with(false, |visible| {
            calls.push(visible);
            Ok(())
        })
        .expect("hide tray");
        set_tray_visibility_with(true, |visible| {
            calls.push(visible);
            Ok(())
        })
        .expect("show tray");

        assert_eq!(calls, [false, true]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reopening_recreates_the_tray_by_hiding_then_showing_it() {
        let mut calls = Vec::new();
        recreate_tray_visibility_with(|visible| {
            calls.push(visible);
            Ok(())
        })
        .expect("recreate tray");

        assert_eq!(calls, [false, true]);
    }

    #[test]
    fn idle_menu_has_the_requested_action_order() {
        assert_eq!(
            actions(TrayIconState::Idle),
            vec![
                (MenuAction::OpenSona, true),
                (MenuAction::StartDictation, true),
                (MenuAction::StartMeetingNotes, true),
                (MenuAction::CopyLastTranscript, true),
                (MenuAction::Settings, true),
                (MenuAction::Quit, true),
            ]
        );
        assert!(model_enabled(TrayIconState::Idle));
    }

    #[test]
    fn recording_menu_offers_stop_and_blocks_competing_starts() {
        assert_eq!(
            actions(TrayIconState::Recording),
            vec![
                (MenuAction::OpenSona, true),
                (MenuAction::StopDictation, true),
                (MenuAction::StartMeetingNotes, false),
                (MenuAction::CopyLastTranscript, true),
                (MenuAction::Settings, true),
                (MenuAction::Quit, true),
            ]
        );
        assert!(!model_enabled(TrayIconState::Recording));
    }

    #[test]
    fn transcribing_menu_offers_cancel_instead_of_stop() {
        let rows = menu_rows(TrayIconState::Transcribing, false, None);
        assert!(rows.contains(&MenuRow::Status(MenuStatus::Transcribing)));
        assert!(
            actions(TrayIconState::Transcribing).contains(&(MenuAction::CancelTranscription, true))
        );
        assert!(!actions(TrayIconState::Transcribing).contains(&(MenuAction::StopDictation, true)));
    }

    #[test]
    fn meeting_snapshot_actions_control_stop_and_cancel() {
        let preflight = MeetingMenuState::from_phase_and_actions(
            MeetingPhase::Preflight,
            &[AllowedMeetingAction::CancelPreflight],
        );
        assert!(!preflight.can_stop);
        assert!(preflight.can_cancel);

        let capture = MeetingMenuState::from_phase_and_actions(
            MeetingPhase::CapturingRecording,
            &[AllowedMeetingAction::Stop, AllowedMeetingAction::Discard],
        );
        assert!(capture.can_stop);
        assert!(capture.can_cancel);

        let processing = MeetingMenuState::from_phase_and_actions(
            MeetingPhase::Processing,
            &[AllowedMeetingAction::CancelRemote],
        );
        assert!(!processing.can_stop);
        assert!(!processing.can_cancel);
    }

    #[test]
    fn meeting_preflight_opens_setup_and_exposes_cancel() {
        let meeting = meeting_state(MeetingPhase::Preflight, false, true);
        let rows = menu_rows(TrayIconState::Idle, false, Some(meeting));
        assert_eq!(rows[0], MenuRow::Status(MenuStatus::MeetingPreflight));
        assert_eq!(
            meeting_actions(meeting),
            vec![
                (MenuAction::OpenSona, true),
                (MenuAction::StartDictation, false),
                (MenuAction::OpenMeetingNotes, true),
                (MenuAction::CancelMeetingNotes, true),
                (MenuAction::CopyLastTranscript, true),
                (MenuAction::Settings, true),
                (MenuAction::Quit, true),
            ]
        );
        assert!(!meeting_model_enabled(meeting));
    }

    #[test]
    fn active_meeting_exposes_only_allowed_stop_and_cancel_actions() {
        let meeting = meeting_state(MeetingPhase::CapturingRecording, true, true);
        assert_eq!(meeting.icon_state(), TrayIconState::Recording);
        assert_eq!(
            meeting_actions(meeting),
            vec![
                (MenuAction::OpenSona, true),
                (MenuAction::StartDictation, false),
                (MenuAction::StopMeetingNotes, true),
                (MenuAction::CancelMeetingNotes, true),
                (MenuAction::CopyLastTranscript, true),
                (MenuAction::Settings, true),
                (MenuAction::Quit, true),
            ]
        );
    }

    #[test]
    fn processing_meeting_uses_progress_status_without_fake_cancel() {
        let meeting = meeting_state(MeetingPhase::Processing, false, false);
        let rows = menu_rows(TrayIconState::Idle, false, Some(meeting));
        assert_eq!(meeting.icon_state(), TrayIconState::Transcribing);
        assert_eq!(rows[0], MenuRow::Status(MenuStatus::MeetingProcessing));
        let actions = meeting_actions(meeting);
        assert!(actions.contains(&(MenuAction::OpenMeetingNotes, true)));
        assert!(!actions.iter().any(|(action, _)| matches!(
            action,
            MenuAction::StopMeetingNotes | MenuAction::CancelMeetingNotes
        )));
    }

    #[test]
    fn secure_input_warning_follows_the_status_line() {
        let rows = menu_rows(TrayIconState::Idle, true, None);
        assert_eq!(rows[0], MenuRow::Status(MenuStatus::Ready));
        assert_eq!(rows[1], MenuRow::SecureInputWarning);
        assert_eq!(rows[2], MenuRow::Separator);
    }

    #[test]
    fn recording_to_transcribing_rebuilds_the_honest_menu() {
        assert_ne!(
            inputs(TrayIconState::Recording),
            inputs(TrayIconState::Transcribing)
        );
    }

    #[test]
    fn dictation_cycle_rebuilds_once_per_visible_menu_change() {
        let script = [
            inputs(TrayIconState::Idle),
            inputs(TrayIconState::Recording),
            inputs(TrayIconState::Recording),
            inputs(TrayIconState::Transcribing),
            inputs(TrayIconState::Idle),
        ];

        assert_eq!(rebuilds(&script), 4);
    }

    #[test]
    fn dictation_activity_flips_during_a_meeting_do_not_rebuild_the_menu() {
        let meeting = Some(meeting_state(MeetingPhase::CapturingRecording, true, true));
        let script = [
            menu_inputs(TrayIconState::Idle, meeting),
            menu_inputs(TrayIconState::Recording, meeting),
            menu_inputs(TrayIconState::Transcribing, meeting),
            menu_inputs(TrayIconState::Idle, meeting),
        ];

        assert_eq!(rebuilds(&script), 1);
    }

    #[test]
    fn meeting_phases_that_share_every_row_do_not_rebuild_the_menu() {
        let script = [
            MeetingPhase::CapturingRecording,
            MeetingPhase::CapturingPausing,
            MeetingPhase::CapturingResuming,
            MeetingPhase::CapturingRecording,
        ]
        .map(|phase| menu_inputs(TrayIconState::Idle, Some(meeting_state(phase, true, true))));

        assert_eq!(rebuilds(&script), 1);
    }

    #[test]
    fn macos_template_icons_are_black_alpha_at_two_x() {
        let idle = template_alpha(include_bytes!("../resources/tray_template_idle.png"));
        let recording = template_alpha(include_bytes!("../resources/tray_template_recording.png"));
        let transcribing = template_alpha(include_bytes!(
            "../resources/tray_template_transcribing.png"
        ));
        let warning = template_alpha(include_bytes!(
            "../resources/tray_template_idle_warning.png"
        ));

        assert_ne!(idle, recording);
        assert_ne!(idle, transcribing);
        assert_ne!(idle, warning);
        for state in [&recording, &transcribing, &warning] {
            for y in 0..24 {
                let row = y * 36;
                assert_eq!(&idle[row..row + 36], &state[row..row + 36]);
            }
        }
    }

    #[test]
    fn sona_mark_keeps_two_components_at_one_x() {
        let alpha = template_alpha(include_bytes!("../resources/tray_template_idle.png"));
        let one_x: Vec<u8> = (0..18)
            .flat_map(|y| {
                let alpha = &alpha;
                (0..18).map(move |x| {
                    let top = (y * 2) * 36 + x * 2;
                    let sum = u16::from(alpha[top])
                        + u16::from(alpha[top + 1])
                        + u16::from(alpha[top + 36])
                        + u16::from(alpha[top + 37]);
                    u8::try_from(sum / 4).expect("four alpha values average within u8")
                })
            })
            .collect();
        let mut seen = [false; 18 * 18];
        let mut component_sizes = Vec::new();
        for start in 0..one_x.len() {
            if seen[start] || one_x[start] < 64 {
                continue;
            }
            seen[start] = true;
            let mut stack = vec![start];
            let mut size = 0;
            while let Some(index) = stack.pop() {
                size += 1;
                let x = index % 18;
                let y = index / 18;
                for neighbor in [
                    (x > 0).then_some(index - 1),
                    (x < 17).then_some(index + 1),
                    (y > 0).then_some(index - 18),
                    (y < 17).then_some(index + 18),
                ]
                .into_iter()
                .flatten()
                {
                    if !seen[neighbor] && one_x[neighbor] >= 64 {
                        seen[neighbor] = true;
                        stack.push(neighbor);
                    }
                }
            }
            component_sizes.push(size);
        }
        component_sizes.sort_unstable();
        assert_eq!(component_sizes, vec![6, 59]);
    }
}
