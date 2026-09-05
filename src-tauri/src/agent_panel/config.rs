use super::protocol::{
    DeviceNames, ProposalValidationError, SonaAllowedValuesV1, SonaAppearanceSnapshotV1,
    SonaAudioSnapshotV1, SonaCloudProvidersSnapshotV1, SonaConfigSnapshotV1,
    SonaInstalledModelsSnapshotV1, SonaLanguageSnapshotV1, SonaMicrophoneSnapshotV1,
    SonaModesSnapshotV1, SonaOverlaySnapshotV1, SonaPlatformSnapshotV1, SonaPrivacySnapshotV1,
    SonaRetentionSnapshotV1, SonaSettingChangeV1, SonaStartupSnapshotV1,
    SONA_CONFIG_SNAPSHOT_VERSION,
};
use crate::audio_toolkit::audio::{list_input_devices, list_output_devices};
use crate::managers::model::ModelManager;
use crate::settings::{self, AppSettings, RecordingRetentionPeriod};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigError {
    SnapshotUnavailable,
    StaleRevision,
    InvalidProposal,
    InvalidSetting,
    /// The settings store took the change in memory and would not write it
    /// out. An agent that hears its proposal applied would be wrong.
    NotPersisted,
}

impl From<ProposalValidationError> for ConfigError {
    fn from(_: ProposalValidationError) -> Self {
        Self::InvalidProposal
    }
}

impl From<crate::settings::SettingsPersistError> for ConfigError {
    fn from(_: crate::settings::SettingsPersistError) -> Self {
        Self::NotPersisted
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotContext {
    pub(crate) snapshot: SonaConfigSnapshotV1,
    pub(crate) allowed: SonaAllowedValuesV1,
}

#[derive(Clone, Debug)]
pub(crate) struct AppliedSettings {
    pub(crate) revision: u64,
    undo: Vec<SettingUndo>,
}

impl AppliedSettings {
    pub(crate) fn undo(&self) -> &[SettingUndo] {
        &self.undo
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingKind {
    Theme,
    OverlayStyle,
    OverlayPosition,
    AudioFeedback,
    AudioVolume,
    MuteWhileRecording,
    OutputDevice,
    Microphone,
    DefaultModel,
    Language,
    Spelling,
    ModeSelection,
    ModeToggles,
    Retention,
    StartHidden,
    TrayVisibility,
    UpdateNoteVisibility,
}

#[derive(Clone, Debug, PartialEq)]
enum ResolvedChange {
    Theme(crate::settings::Theme),
    OverlayStyle(crate::settings::OverlayStyle),
    OverlayPosition(crate::settings::OverlayPosition),
    AudioFeedback(bool),
    AudioVolume(f32),
    MuteWhileRecording(bool),
    OutputDevice(Option<String>),
    Microphone(Option<String>),
    DefaultModel(String),
    Language(String),
    Spelling(crate::settings::EnglishSpelling),
    ModeSelection(String),
    ModeToggles(BTreeMap<String, bool>),
    Retention(RecordingRetentionPeriod),
    StartHidden(bool),
    TrayVisibility(bool),
    UpdateNoteVisibility(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SettingUndo {
    Theme(crate::settings::Theme),
    OverlayStyle(crate::settings::OverlayStyle),
    OverlayPosition(crate::settings::OverlayPosition),
    AudioFeedback(bool),
    AudioVolume(f32),
    MuteWhileRecording(bool),
    OutputDevice(Option<String>),
    Microphone(Option<String>),
    DefaultModel {
        model_id: String,
        onboarding_completed: bool,
        active_mode_model_id: Option<String>,
    },
    Language {
        language: String,
        active_mode_language: Option<String>,
    },
    Spelling(crate::settings::EnglishSpelling),
    ModeSelection(String),
    ModeToggles {
        global_translate_to_english: bool,
        active_mode_translate_to_english: Option<bool>,
    },
    Retention(RecordingRetentionPeriod),
    StartHidden(bool),
    TrayVisibility(bool),
    UpdateNoteVisibility(bool),
}

impl SettingUndo {
    fn kind(&self) -> SettingKind {
        match self {
            Self::Theme(_) => SettingKind::Theme,
            Self::OverlayStyle(_) => SettingKind::OverlayStyle,
            Self::OverlayPosition(_) => SettingKind::OverlayPosition,
            Self::AudioFeedback(_) => SettingKind::AudioFeedback,
            Self::AudioVolume(_) => SettingKind::AudioVolume,
            Self::MuteWhileRecording(_) => SettingKind::MuteWhileRecording,
            Self::OutputDevice(_) => SettingKind::OutputDevice,
            Self::Microphone(_) => SettingKind::Microphone,
            Self::DefaultModel { .. } => SettingKind::DefaultModel,
            Self::Language { .. } => SettingKind::Language,
            Self::Spelling(_) => SettingKind::Spelling,
            Self::ModeSelection(_) => SettingKind::ModeSelection,
            Self::ModeToggles { .. } => SettingKind::ModeToggles,
            Self::Retention(_) => SettingKind::Retention,
            Self::StartHidden(_) => SettingKind::StartHidden,
            Self::TrayVisibility(_) => SettingKind::TrayVisibility,
            Self::UpdateNoteVisibility(_) => SettingKind::UpdateNoteVisibility,
        }
    }
}

#[derive(Clone, Serialize)]
struct SettingsChangedEvent {
    setting: &'static str,
}

pub(crate) async fn build_snapshot(app: &AppHandle) -> Result<SnapshotContext, ConfigError> {
    let settings = settings::get_settings(app);
    let models = app
        .try_state::<Arc<ModelManager>>()
        .map(|manager| manager.get_available_models())
        .unwrap_or_default();
    let device_names = tauri::async_runtime::spawn_blocking(|| {
        let mut names = DeviceNames::default();
        names.input.insert("default".to_string(), String::new());
        names.output.insert("default".to_string(), String::new());
        if let Ok(devices) = list_input_devices() {
            for device in devices {
                if is_identifier(&device.index) {
                    names.input.insert(device.index, device.name);
                }
            }
        }
        if let Ok(devices) = list_output_devices() {
            for device in devices {
                if is_identifier(&device.index) {
                    names.output.insert(device.index, device.name);
                }
            }
        }
        names
    })
    .await
    .map_err(|_| ConfigError::SnapshotUnavailable)?;

    let snapshot = snapshot_from_parts(&settings, &models, &device_names);
    let allowed = snapshot.allowed_values(&device_names);
    Ok(SnapshotContext { snapshot, allowed })
}

pub(super) fn snapshot_from_parts(
    settings: &AppSettings,
    models: &[crate::managers::model::ModelInfo],
    device_names: &DeviceNames,
) -> SonaConfigSnapshotV1 {
    let available_model_ids = models
        .iter()
        .filter(|model| model.is_downloaded && is_identifier(&model.id))
        .map(|model| model.id.clone())
        .take(128)
        .collect::<Vec<_>>();
    let default_model = if available_model_ids.contains(&settings.selected_model) {
        settings.selected_model.clone()
    } else {
        available_model_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "none".to_string())
    };
    let available_languages = available_languages(models);
    let language = if available_languages.contains(&settings.selected_language) {
        settings.selected_language.clone()
    } else {
        "auto".to_string()
    };
    let available_mode_ids = settings
        .modes
        .iter()
        .filter(|mode| is_identifier(&mode.id))
        .map(|mode| mode.id.clone())
        .take(128)
        .collect::<Vec<_>>();
    let selected_mode = available_mode_ids
        .contains(&settings.active_mode_id)
        .then(|| settings.active_mode_id.clone())
        .or_else(|| available_mode_ids.first().cloned())
        .unwrap_or_else(|| "default".to_string());
    let active_mode = settings.modes.iter().find(|mode| mode.id == selected_mode);
    let mut mode_toggles = BTreeMap::new();
    mode_toggles.insert(
        "translate_to_english".to_string(),
        active_mode
            .map(|mode| mode.asr.translate_to_english)
            .unwrap_or(settings.translate_to_english),
    );

    SonaConfigSnapshotV1 {
        version: SONA_CONFIG_SNAPSHOT_VERSION.to_string(),
        settings_revision: settings.settings_revision,
        appearance: SonaAppearanceSnapshotV1 {
            theme: theme_name(settings.theme).to_string(),
            available_themes: vec![
                "system".to_string(),
                "light".to_string(),
                "dark".to_string(),
            ],
        },
        overlay: SonaOverlaySnapshotV1 {
            style: overlay_style_name(settings.overlay_style).to_string(),
            position: overlay_position_name(settings.overlay_position).to_string(),
            available_styles: vec![
                "none".to_string(),
                "minimal".to_string(),
                "live".to_string(),
            ],
            available_positions: vec!["top".to_string(), "bottom".to_string()],
        },
        audio: SonaAudioSnapshotV1 {
            feedback_enabled: settings.audio_feedback,
            volume: settings.audio_feedback_volume,
            mute_while_recording: settings.mute_while_recording,
            output_device_id: selected_device_id(
                &settings.selected_output_device,
                &device_names.output,
            ),
            available_output_device_ids: device_names.output.keys().cloned().collect(),
        },
        microphone: SonaMicrophoneSnapshotV1 {
            selected_device_id: selected_device_id(
                &settings.selected_microphone,
                &device_names.input,
            ),
            available_device_ids: device_names.input.keys().cloned().collect(),
        },
        installed_models: SonaInstalledModelsSnapshotV1 {
            default_transcription_model: default_model,
            available_ids: available_model_ids,
        },
        language: SonaLanguageSnapshotV1 {
            language,
            spelling_behavior: spelling_name(settings.english_spelling).to_string(),
            available_languages,
            available_spelling_behaviors: vec!["as_spoken".to_string(), "british".to_string()],
        },
        modes: SonaModesSnapshotV1 {
            selected: selected_mode,
            available: available_mode_ids,
            toggles: mode_toggles,
        },
        privacy: SonaPrivacySnapshotV1 {
            clipboard_enabled: settings.context_policy_ceiling
                != crate::context::ContextPolicy::None,
            url_context_enabled: settings.context_url_capture_enabled,
            selected_text_enabled: matches!(
                settings.context_policy_ceiling,
                crate::context::ContextPolicy::TargetAndSelection
                    | crate::context::ContextPolicy::Full
            ),
            application_context_enabled: settings.context_policy_ceiling
                != crate::context::ContextPolicy::None,
            context_ceiling: context_ceiling_rank(settings.context_policy_ceiling),
        },
        retention: SonaRetentionSnapshotV1 {
            local_retention_days: retention_days(settings.recording_retention_period),
        },
        cloud_providers: SonaCloudProvidersSnapshotV1 {
            states: cloud_provider_states(settings),
        },
        platform: platform_snapshot(),
        startup: SonaStartupSnapshotV1 {
            start_hidden: settings.start_hidden,
            tray_visibility: settings.show_tray_icon,
            update_note_visibility: settings.show_whats_new_on_update,
        },
    }
}

fn available_languages(models: &[crate::managers::model::ModelInfo]) -> Vec<String> {
    let mut languages = BTreeSet::from(["auto".to_string()]);
    for model in models.iter().filter(|model| model.is_downloaded) {
        for language in &model.supported_languages {
            if is_identifier(language) {
                languages.insert(language.clone());
            }
        }
    }
    languages.into_iter().collect()
}

fn selected_device_id(selected_name: &Option<String>, names: &BTreeMap<String, String>) -> String {
    match selected_name {
        None => "default".to_string(),
        Some(name) => names
            .iter()
            .find(|(_, candidate)| *candidate == name)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| "default".to_string()),
    }
}

fn cloud_provider_states(settings: &AppSettings) -> BTreeMap<String, String> {
    let mut states = BTreeMap::new();
    for provider in &settings.cloud_stt_providers {
        states.insert(
            provider.provider.id().to_string(),
            if provider.secret_state.configured {
                "configured".to_string()
            } else {
                "not_configured".to_string()
            },
        );
    }
    for (provider, state) in &settings.post_process_secret_states {
        if is_identifier(provider) {
            states.insert(
                provider.clone(),
                if state.configured {
                    "configured".to_string()
                } else {
                    "not_configured".to_string()
                },
            );
        }
    }
    states
}

fn platform_snapshot() -> SonaPlatformSnapshotV1 {
    SonaPlatformSnapshotV1 {
        capabilities: BTreeMap::from([
            ("agent_panel".to_string(), true),
            ("native_windows".to_string(), true),
        ]),
        permissions: BTreeMap::from([
            ("microphone".to_string(), "unavailable".to_string()),
            ("accessibility".to_string(), "unavailable".to_string()),
            ("screen_recording".to_string(), "unavailable".to_string()),
        ]),
    }
}

fn theme_name(theme: crate::settings::Theme) -> &'static str {
    match theme {
        crate::settings::Theme::System => "system",
        crate::settings::Theme::Light => "light",
        crate::settings::Theme::Dark => "dark",
    }
}

fn overlay_style_name(style: crate::settings::OverlayStyle) -> &'static str {
    match style {
        crate::settings::OverlayStyle::None => "none",
        crate::settings::OverlayStyle::Minimal => "minimal",
        crate::settings::OverlayStyle::Live => "live",
    }
}

fn overlay_position_name(position: crate::settings::OverlayPosition) -> &'static str {
    match position {
        crate::settings::OverlayPosition::Top => "top",
        crate::settings::OverlayPosition::Bottom => "bottom",
    }
}

fn spelling_name(spelling: crate::settings::EnglishSpelling) -> &'static str {
    match spelling {
        crate::settings::EnglishSpelling::AsSpoken => "as_spoken",
        crate::settings::EnglishSpelling::British => "british",
    }
}

fn context_ceiling_rank(ceiling: crate::context::ContextPolicy) -> u32 {
    match ceiling {
        crate::context::ContextPolicy::None => 0,
        crate::context::ContextPolicy::Target => 1,
        crate::context::ContextPolicy::TargetAndSelection => 2,
        crate::context::ContextPolicy::Full => 3,
    }
}

fn retention_days(retention: RecordingRetentionPeriod) -> u32 {
    match retention {
        RecordingRetentionPeriod::Never | RecordingRetentionPeriod::PreserveLimit => 0,
        RecordingRetentionPeriod::Days3 => 3,
        RecordingRetentionPeriod::Weeks2 => 14,
        RecordingRetentionPeriod::Months3 => 90,
    }
}

fn retention_from_days(days: u32) -> Option<RecordingRetentionPeriod> {
    match days {
        0 => Some(RecordingRetentionPeriod::Never),
        3 => Some(RecordingRetentionPeriod::Days3),
        14 => Some(RecordingRetentionPeriod::Weeks2),
        90 => Some(RecordingRetentionPeriod::Months3),
        _ => None,
    }
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

pub(crate) fn apply_changes(
    app: &AppHandle,
    expected_revision: u64,
    changes: &[SonaSettingChangeV1],
    allowed: &SonaAllowedValuesV1,
) -> Result<AppliedSettings, ConfigError> {
    let (undo, revision) = settings::try_update_settings_with_revision(app, |settings| {
        apply_changes_to_settings(settings, expected_revision, changes, allowed)
    })?;
    let kinds = undo.iter().map(SettingUndo::kind).collect::<Vec<_>>();
    apply_runtime_effects(app, &kinds);
    Ok(AppliedSettings { revision, undo })
}

pub(super) fn apply_changes_to_settings(
    settings: &mut AppSettings,
    expected_revision: u64,
    changes: &[SonaSettingChangeV1],
    allowed: &SonaAllowedValuesV1,
) -> Result<Vec<SettingUndo>, ConfigError> {
    let resolved = resolve_changes(changes, allowed)?;
    mutate_settings(settings, expected_revision, &resolved)
}

pub(crate) fn undo_changes(
    app: &AppHandle,
    expected_revision: u64,
    undo: &[SettingUndo],
) -> Result<u64, ConfigError> {
    let (_, revision) = settings::try_update_settings_with_revision(app, |settings| {
        undo_changes_to_settings(settings, expected_revision, undo)
    })?;
    let kinds = undo.iter().map(SettingUndo::kind).collect::<Vec<_>>();
    apply_runtime_effects(app, &kinds);
    Ok(revision)
}

pub(super) fn undo_changes_to_settings(
    settings: &mut AppSettings,
    expected_revision: u64,
    undo: &[SettingUndo],
) -> Result<(), ConfigError> {
    if settings.settings_revision != expected_revision {
        return Err(ConfigError::StaleRevision);
    }
    for change in undo.iter().rev() {
        restore_setting(settings, change)?;
    }
    Ok(())
}

fn resolve_changes(
    changes: &[SonaSettingChangeV1],
    allowed: &SonaAllowedValuesV1,
) -> Result<Vec<ResolvedChange>, ConfigError> {
    let mut keys = BTreeSet::new();
    let mut resolved = Vec::with_capacity(changes.len());
    for change in changes {
        if !keys.insert(change.key_name()) {
            return Err(ConfigError::InvalidProposal);
        }
        let value = match change {
            SonaSettingChangeV1::Theme(value) => ResolvedChange::Theme(*value),
            SonaSettingChangeV1::OverlayStyle(value) => ResolvedChange::OverlayStyle(*value),
            SonaSettingChangeV1::OverlayPosition(value) => ResolvedChange::OverlayPosition(*value),
            SonaSettingChangeV1::AudioFeedback(value) => ResolvedChange::AudioFeedback(*value),
            SonaSettingChangeV1::AudioVolume(value)
                if value.is_finite() && (0.0..=1.0).contains(value) =>
            {
                ResolvedChange::AudioVolume(*value)
            }
            SonaSettingChangeV1::MuteWhileRecording(value) => {
                ResolvedChange::MuteWhileRecording(*value)
            }
            SonaSettingChangeV1::AudioOutputDeviceId(id) => {
                ResolvedChange::OutputDevice(resolve_device(id, &allowed.output_device_names)?)
            }
            SonaSettingChangeV1::MicrophoneId(id) => {
                ResolvedChange::Microphone(resolve_device(id, &allowed.input_device_names)?)
            }
            SonaSettingChangeV1::DefaultTranscriptionModel(id)
                if allowed.model_ids.contains(id) =>
            {
                ResolvedChange::DefaultModel(id.clone())
            }
            SonaSettingChangeV1::Language(value) if allowed.language_codes.contains(value) => {
                ResolvedChange::Language(value.clone())
            }
            SonaSettingChangeV1::SpellingBehavior(value) => ResolvedChange::Spelling(*value),
            SonaSettingChangeV1::ModeSelection(value) if allowed.mode_ids.contains(value) => {
                ResolvedChange::ModeSelection(value.clone())
            }
            SonaSettingChangeV1::ModeToggles(values)
                if !values.is_empty()
                    && values
                        .keys()
                        .all(|name| allowed.mode_toggle_names.contains(name)) =>
            {
                ResolvedChange::ModeToggles(values.clone())
            }
            SonaSettingChangeV1::LocalRetentionPeriod(days) => ResolvedChange::Retention(
                retention_from_days(*days).ok_or(ConfigError::InvalidSetting)?,
            ),
            SonaSettingChangeV1::StartHidden(value) => ResolvedChange::StartHidden(*value),
            SonaSettingChangeV1::TrayVisibility(value) => ResolvedChange::TrayVisibility(*value),
            SonaSettingChangeV1::UpdateNoteVisibility(value) => {
                ResolvedChange::UpdateNoteVisibility(*value)
            }
            _ => return Err(ConfigError::InvalidSetting),
        };
        resolved.push(value);
    }
    Ok(resolved)
}

fn resolve_device(
    id: &str,
    available: &BTreeMap<String, String>,
) -> Result<Option<String>, ConfigError> {
    if id == "default" {
        return Ok(None);
    }
    available
        .get(id)
        .cloned()
        .map(Some)
        .ok_or(ConfigError::InvalidSetting)
}

fn mutate_settings(
    settings: &mut AppSettings,
    expected_revision: u64,
    changes: &[ResolvedChange],
) -> Result<Vec<SettingUndo>, ConfigError> {
    if settings.settings_revision != expected_revision {
        return Err(ConfigError::StaleRevision);
    }
    let mut undo = Vec::with_capacity(changes.len());
    for change in changes {
        let prior = match change {
            ResolvedChange::Theme(value) => {
                let prior = SettingUndo::Theme(settings.theme);
                settings.theme = *value;
                prior
            }
            ResolvedChange::OverlayStyle(value) => {
                let prior = SettingUndo::OverlayStyle(settings.overlay_style);
                settings.overlay_style = *value;
                prior
            }
            ResolvedChange::OverlayPosition(value) => {
                let prior = SettingUndo::OverlayPosition(settings.overlay_position);
                settings.overlay_position = *value;
                prior
            }
            ResolvedChange::AudioFeedback(value) => {
                let prior = SettingUndo::AudioFeedback(settings.audio_feedback);
                settings.audio_feedback = *value;
                prior
            }
            ResolvedChange::AudioVolume(value) => {
                let prior = SettingUndo::AudioVolume(settings.audio_feedback_volume);
                settings.audio_feedback_volume = *value;
                prior
            }
            ResolvedChange::MuteWhileRecording(value) => {
                let prior = SettingUndo::MuteWhileRecording(settings.mute_while_recording);
                settings.mute_while_recording = *value;
                prior
            }
            ResolvedChange::OutputDevice(value) => {
                let prior = SettingUndo::OutputDevice(settings.selected_output_device.clone());
                settings.selected_output_device = value.clone();
                prior
            }
            ResolvedChange::Microphone(value) => {
                let prior = SettingUndo::Microphone(settings.selected_microphone.clone());
                settings.selected_microphone = value.clone();
                prior
            }
            ResolvedChange::DefaultModel(value) => {
                let active_mode_model_id = settings
                    .modes
                    .iter()
                    .find(|mode| mode.id == settings.active_mode_id)
                    .map(|mode| mode.asr.model_id.clone());
                let prior = SettingUndo::DefaultModel {
                    model_id: settings.selected_model.clone(),
                    onboarding_completed: settings.onboarding_completed,
                    active_mode_model_id,
                };
                settings.selected_model = value.clone();
                settings.onboarding_completed = true;
                if let Some(mode) = settings
                    .modes
                    .iter_mut()
                    .find(|mode| mode.id == settings.active_mode_id)
                {
                    mode.asr.model_id = value.clone();
                    settings.modes_revision = settings.modes_revision.saturating_add(1);
                }
                prior
            }
            ResolvedChange::Language(value) => {
                let active_mode_language = settings
                    .modes
                    .iter()
                    .find(|mode| mode.id == settings.active_mode_id)
                    .map(|mode| mode.asr.language.clone());
                let prior = SettingUndo::Language {
                    language: settings.selected_language.clone(),
                    active_mode_language,
                };
                settings.selected_language = value.clone();
                if let Some(mode) = settings
                    .modes
                    .iter_mut()
                    .find(|mode| mode.id == settings.active_mode_id)
                {
                    mode.asr.language = value.clone();
                    settings.modes_revision = settings.modes_revision.saturating_add(1);
                }
                prior
            }
            ResolvedChange::Spelling(value) => {
                let prior = SettingUndo::Spelling(settings.english_spelling);
                settings.english_spelling = *value;
                prior
            }
            ResolvedChange::ModeSelection(value) => {
                if !settings.modes.iter().any(|mode| mode.id == *value) {
                    return Err(ConfigError::InvalidSetting);
                }
                let prior = SettingUndo::ModeSelection(settings.active_mode_id.clone());
                settings.active_mode_id = value.clone();
                prior
            }
            ResolvedChange::ModeToggles(values) => {
                let active_mode_translate_to_english = settings
                    .modes
                    .iter()
                    .find(|mode| mode.id == settings.active_mode_id)
                    .map(|mode| mode.asr.translate_to_english);
                let prior = SettingUndo::ModeToggles {
                    global_translate_to_english: settings.translate_to_english,
                    active_mode_translate_to_english,
                };
                if let Some(enabled) = values.get("translate_to_english") {
                    settings.translate_to_english = *enabled;
                    if let Some(mode) = settings
                        .modes
                        .iter_mut()
                        .find(|mode| mode.id == settings.active_mode_id)
                    {
                        mode.asr.translate_to_english = *enabled;
                        settings.modes_revision = settings.modes_revision.saturating_add(1);
                    }
                }
                prior
            }
            ResolvedChange::Retention(value) => {
                let prior = SettingUndo::Retention(settings.recording_retention_period);
                settings.recording_retention_period = *value;
                prior
            }
            ResolvedChange::StartHidden(value) => {
                let prior = SettingUndo::StartHidden(settings.start_hidden);
                settings.start_hidden = *value;
                prior
            }
            ResolvedChange::TrayVisibility(value) => {
                let prior = SettingUndo::TrayVisibility(settings.show_tray_icon);
                settings.show_tray_icon = *value;
                prior
            }
            ResolvedChange::UpdateNoteVisibility(value) => {
                let prior = SettingUndo::UpdateNoteVisibility(settings.show_whats_new_on_update);
                settings.show_whats_new_on_update = *value;
                prior
            }
        };
        undo.push(prior);
    }
    Ok(undo)
}

fn restore_setting(settings: &mut AppSettings, undo: &SettingUndo) -> Result<(), ConfigError> {
    match undo {
        SettingUndo::Theme(value) => settings.theme = *value,
        SettingUndo::OverlayStyle(value) => settings.overlay_style = *value,
        SettingUndo::OverlayPosition(value) => settings.overlay_position = *value,
        SettingUndo::AudioFeedback(value) => settings.audio_feedback = *value,
        SettingUndo::AudioVolume(value) => settings.audio_feedback_volume = *value,
        SettingUndo::MuteWhileRecording(value) => settings.mute_while_recording = *value,
        SettingUndo::OutputDevice(value) => settings.selected_output_device = value.clone(),
        SettingUndo::Microphone(value) => settings.selected_microphone = value.clone(),
        SettingUndo::DefaultModel {
            model_id,
            onboarding_completed,
            active_mode_model_id,
        } => {
            settings.selected_model = model_id.clone();
            settings.onboarding_completed = *onboarding_completed;
            if let Some(model_id) = active_mode_model_id {
                let mode = settings
                    .modes
                    .iter_mut()
                    .find(|mode| mode.id == settings.active_mode_id)
                    .ok_or(ConfigError::InvalidSetting)?;
                mode.asr.model_id = model_id.clone();
                settings.modes_revision = settings.modes_revision.saturating_add(1);
            }
        }
        SettingUndo::Language {
            language,
            active_mode_language,
        } => {
            settings.selected_language = language.clone();
            if let Some(language) = active_mode_language {
                let mode = settings
                    .modes
                    .iter_mut()
                    .find(|mode| mode.id == settings.active_mode_id)
                    .ok_or(ConfigError::InvalidSetting)?;
                mode.asr.language = language.clone();
                settings.modes_revision = settings.modes_revision.saturating_add(1);
            }
        }
        SettingUndo::Spelling(value) => settings.english_spelling = *value,
        SettingUndo::ModeSelection(value) => settings.active_mode_id = value.clone(),
        SettingUndo::ModeToggles {
            global_translate_to_english,
            active_mode_translate_to_english,
        } => {
            settings.translate_to_english = *global_translate_to_english;
            if let Some(enabled) = active_mode_translate_to_english {
                let mode = settings
                    .modes
                    .iter_mut()
                    .find(|mode| mode.id == settings.active_mode_id)
                    .ok_or(ConfigError::InvalidSetting)?;
                mode.asr.translate_to_english = *enabled;
                settings.modes_revision = settings.modes_revision.saturating_add(1);
            }
        }
        SettingUndo::Retention(value) => settings.recording_retention_period = *value,
        SettingUndo::StartHidden(value) => settings.start_hidden = *value,
        SettingUndo::TrayVisibility(value) => settings.show_tray_icon = *value,
        SettingUndo::UpdateNoteVisibility(value) => settings.show_whats_new_on_update = *value,
    }
    Ok(())
}

fn apply_runtime_effects(app: &AppHandle, kinds: &[SettingKind]) {
    let mut unique = BTreeSet::new();
    unique.extend(kinds.iter().copied().map(setting_kind_index));
    let settings = settings::get_settings(app);
    if unique.contains(&setting_kind_index(SettingKind::Theme)) {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        crate::shortcut::apply_window_theme(app, settings.theme);
        let _ = app.emit("theme-changed", settings.theme);
    }
    if unique.contains(&setting_kind_index(SettingKind::OverlayStyle)) {
        crate::overlay::update_overlay_enabled_cache(
            settings.overlay_style != crate::settings::OverlayStyle::None,
        );
    }
    if unique.contains(&setting_kind_index(SettingKind::OverlayStyle))
        || unique.contains(&setting_kind_index(SettingKind::OverlayPosition))
    {
        crate::utils::update_overlay_position(app);
    }
    if unique.contains(&setting_kind_index(SettingKind::TrayVisibility)) {
        crate::tray::set_tray_visibility(app, settings.show_tray_icon);
    }
    if unique.contains(&setting_kind_index(SettingKind::DefaultModel))
        || unique.contains(&setting_kind_index(SettingKind::Language))
        || unique.contains(&setting_kind_index(SettingKind::ModeSelection))
        || unique.contains(&setting_kind_index(SettingKind::ModeToggles))
    {
        crate::modes::emit_modes_changed(app, &crate::modes::mode_settings_snapshot(&settings));
    }
    let _ = app.emit(
        "settings-changed",
        SettingsChangedEvent {
            setting: "agent_panel",
        },
    );
}

const fn setting_kind_index(kind: SettingKind) -> u8 {
    match kind {
        SettingKind::Theme => 1,
        SettingKind::OverlayStyle => 2,
        SettingKind::OverlayPosition => 3,
        SettingKind::AudioFeedback => 4,
        SettingKind::AudioVolume => 5,
        SettingKind::MuteWhileRecording => 6,
        SettingKind::OutputDevice => 7,
        SettingKind::Microphone => 8,
        SettingKind::DefaultModel => 9,
        SettingKind::Language => 10,
        SettingKind::Spelling => 11,
        SettingKind::ModeSelection => 12,
        SettingKind::ModeToggles => 13,
        SettingKind::Retention => 14,
        SettingKind::StartHidden => 15,
        SettingKind::TrayVisibility => 16,
        SettingKind::UpdateNoteVisibility => 17,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::model::{EngineType, ModelInfo, ModelSource};
    use crate::settings::{get_default_settings, OverlayStyle, Theme};

    fn model() -> ModelInfo {
        ModelInfo {
            id: "small".to_string(),
            name: "Small".to_string(),
            description: "Installed model".to_string(),
            filename: "small.bin".to_string(),
            source: ModelSource::Local,
            size_mb: 1,
            is_downloaded: true,
            is_downloading: false,
            partial_size: 0,
            is_directory: false,
            engine_type: EngineType::TranscribeCpp,
            accuracy_score: 0.0,
            speed_score: 0.0,
            supports_translation: true,
            is_recommended: false,
            supported_languages: vec!["en".to_string()],
            supports_language_selection: true,
            is_custom: false,
            supports_streaming: false,
            supports_language_detection: true,
        }
    }

    fn device_names() -> DeviceNames {
        DeviceNames {
            input: BTreeMap::from([
                ("default".to_string(), String::new()),
                ("input-1".to_string(), "Microphone".to_string()),
            ]),
            output: BTreeMap::from([
                ("default".to_string(), String::new()),
                ("output-1".to_string(), "Speakers".to_string()),
            ]),
        }
    }

    #[test]
    fn snapshot_excludes_secret_text_paths_and_provider_endpoints() {
        let mut settings = get_default_settings();
        settings.external_script_path = Some("/private/secret-command".to_string());
        let snapshot = snapshot_from_parts(&settings, &[model()], &device_names());
        let serialized = serde_json::to_string(&snapshot).expect("snapshot serializes");
        assert!(!serialized.contains("secret-command"));
        assert!(!serialized.contains("external_script_path"));
        assert!(!serialized.contains("post_process_providers"));
        assert!(!serialized.contains("filename"));
    }

    #[test]
    fn stale_mutation_changes_nothing() {
        let mut settings = get_default_settings();
        let before = settings.clone();
        let expected_revision = settings.settings_revision.saturating_add(1);
        let result = mutate_settings(
            &mut settings,
            expected_revision,
            &[ResolvedChange::Theme(Theme::Dark)],
        );
        assert_eq!(result, Err(ConfigError::StaleRevision));
        assert_eq!(settings.theme, before.theme);
        assert_eq!(settings.settings_revision, before.settings_revision);
    }

    #[test]
    fn safe_appearance_defaults_off_and_undo_restores_prior_values() {
        let mut settings = get_default_settings();
        assert!(!settings.agent_panel_safe_appearance_auto_apply);
        let expected = settings.settings_revision;
        let undo = mutate_settings(
            &mut settings,
            expected,
            &[
                ResolvedChange::Theme(Theme::Dark),
                ResolvedChange::OverlayStyle(OverlayStyle::Minimal),
            ],
        )
        .expect("settings mutate");
        settings.settings_revision = settings.settings_revision.saturating_add(1);
        for change in undo.iter().rev() {
            restore_setting(&mut settings, change).expect("undo applies");
        }
        assert_eq!(settings.theme, Theme::System);
        assert_eq!(settings.overlay_style, OverlayStyle::Live);
    }

    #[test]
    fn device_ids_resolve_only_from_snapshot_values() {
        let settings = get_default_settings();
        let names = device_names();
        let snapshot = snapshot_from_parts(&settings, &[model()], &names);
        let allowed = snapshot.allowed_values(&names);
        assert!(resolve_changes(
            &[SonaSettingChangeV1::AudioOutputDeviceId(
                "output-1".to_string()
            )],
            &allowed,
        )
        .is_ok());
        assert_eq!(
            resolve_changes(
                &[SonaSettingChangeV1::AudioOutputDeviceId(
                    "not-present".to_string()
                )],
                &allowed,
            ),
            Err(ConfigError::InvalidSetting)
        );
    }
}
