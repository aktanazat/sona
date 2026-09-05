use crate::audio_toolkit::text::vocabulary_spoken_key;
use crate::context::{self, CaptureOptions, ContextPolicy, ContextSnapshot, PendingContext};
use crate::settings::{
    self, AppSettings, AutoSubmitKey, ClipboardHandling, EmojiReplacement, EnglishSpelling,
    OrtAcceleratorSetting, PasteMethod, PersonaSample, PostProcessEndpoint, PostProcessProvider,
    ReplacementRule, ShortcutBinding, TranscribeAcceleratorSetting, TypingTool, VocabularyEntry,
};
use crate::snippets::Snippet;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;
#[cfg(target_os = "macos")]
use tauri::Manager;
use tauri_specta::Event;

pub const DEFAULT_MODE_ID: &str = "message";
pub const LEGACY_POST_PROCESS_BINDING_ID: &str = "transcribe_with_post_process";

static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Casual,
    SemiCasual,
    #[default]
    Balanced,
    SemiFormal,
    Formal,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PromptPreset {
    #[default]
    MinimalistCleanup,
    ApplicationContext,
    Email,
    Meeting,
    Notes,
    Generic,
}

/// The two direct, user-owned speech providers Sona can use. This remains
/// separate from [`RequestedEngine`] so settings cannot accidentally create a
/// remote route by naming an arbitrary provider.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CloudSttProvider {
    #[serde(rename = "deepgram_nova_3", alias = "deepgram_nova3")]
    DeepgramNova3,
    ElevenLabsScribeV2,
}

impl CloudSttProvider {
    pub const fn id(self) -> &'static str {
        match self {
            Self::DeepgramNova3 => "deepgram_nova3",
            Self::ElevenLabsScribeV2 => "elevenlabs_scribe_v2",
        }
    }
}

fn default_local_fallback_enabled() -> bool {
    true
}

fn default_cloud_timestamps() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ModeAsrSettings {
    /// Empty inherits the globally selected model at plan-build time.
    pub model_id: String,
    pub language: String,
    pub translate_to_english: bool,
    pub custom_words: Vec<VocabularyEntry>,
    pub filler_word_removal_enabled: bool,
    pub custom_filler_words: Option<Vec<String>>,
    /// Convert supported spoken punctuation terms before vocabulary correction.
    /// Existing modes deserialize with this disabled.
    #[serde(default)]
    pub literal_punctuation: bool,
    pub vad_enabled: bool,
    /// A closed engine choice. Existing modes deserialize as local.
    #[serde(default)]
    pub requested_engine: RequestedEngine,
    /// Keep complete local PCM useful when the remote session cannot provide a
    /// trustworthy final. New modes opt in to this safety path by default.
    #[serde(default = "default_local_fallback_enabled")]
    pub local_fallback_enabled: bool,
    /// Empty means the mode's local model. This permits a deliberately smaller
    /// fallback without creating a second global model setting.
    #[serde(default)]
    pub local_fallback_model_id: Option<String>,
    /// Provider vocabulary is sent only in the frozen cloud request.
    #[serde(default)]
    pub cloud_keyterms: Vec<String>,
    /// Cloud transports always request word timestamps; turning this off is
    /// rejected before a remote run because an un-timestamped final is not
    /// trustworthy enough to deliver.
    #[serde(default = "default_cloud_timestamps")]
    pub cloud_timestamps: bool,
}

impl ModeAsrSettings {
    fn from_legacy(settings: &AppSettings) -> Self {
        Self {
            model_id: settings.selected_model.clone(),
            language: settings.selected_language.clone(),
            translate_to_english: settings.translate_to_english,
            custom_words: settings.custom_words.clone(),
            filler_word_removal_enabled: settings.filler_word_removal_enabled,
            custom_filler_words: settings.custom_filler_words.clone(),
            vad_enabled: settings.vad_enabled,
            literal_punctuation: false,
            requested_engine: RequestedEngine::Local,
            local_fallback_enabled: default_local_fallback_enabled(),
            local_fallback_model_id: None,
            cloud_keyterms: Vec::new(),
            cloud_timestamps: default_cloud_timestamps(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ModeLlmSettings {
    pub enabled: bool,
    /// The provider this mode overrides to, or `None` to inherit
    /// [`AppSettings::post_process_provider_id`].
    ///
    /// Inherit is the default and the seeded value, so the provider chosen
    /// once in Settings reaches every mode that never named one of its own.
    /// A plain `String` here had no way to say "nobody chose": a new mode
    /// baked a copy of the global at creation and a later global change never
    /// reached it, which is how four modes came to name a provider with no
    /// credential while the one that actually ran was named by nothing.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// The model for an overridden `provider_id`. Inert while the provider is
    /// inherited: the destination is then the global pair, whose model already
    /// lives in `AppSettings::post_process_models` and is owned there.
    pub model_id: String,
    /// Whether a trailing `Sona, …` sentence is handed to this mode's rewrite
    /// provider as an edit instruction instead of being typed. Off by default,
    /// and inert while `enabled` is false — without a rewrite provider the
    /// instruction has nowhere to go. Existing modes deserialize with it off.
    /// See [`crate::audio_toolkit::split_spoken_instruction`].
    #[serde(default)]
    pub spoken_instructions: bool,
}

/// The rewrite destination a mode will actually use.
///
/// This deliberately has no serde or Specta implementation, for the same
/// reason [`DeliveryPlan`] has none: it is not another settings owner and
/// never crosses IPC. It is a view over a mode's own choice joined with the
/// global one, so [`ModeLlmSettings::destination`] can be the only answer to
/// "which provider does this mode use" without becoming a fifth place a
/// provider is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeLlmDestination {
    pub provider_id: String,
    pub model_id: String,
    /// True when the mode named no provider of its own and took the global.
    pub inherited: bool,
}

impl ModeLlmSettings {
    /// Resolve the mode's rewrite destination.
    ///
    /// The rule is deliberately a single fallback with no predicate in it: an
    /// override is honoured exactly as written, whether or not it can run
    /// right now. Falling back on *failure* instead would send a mode's text
    /// to a destination it never named — and the settings row could only
    /// display that by re-deriving consent, credential and endpoint state, in
    /// the other language, from the same store.
    pub fn destination(&self, settings: &AppSettings) -> ModeLlmDestination {
        match &self.provider_id {
            Some(provider_id) => ModeLlmDestination {
                provider_id: provider_id.clone(),
                model_id: self.model_id.clone(),
                inherited: false,
            },
            None => {
                let provider_id = settings.post_process_provider_id.clone();
                let model_id = settings
                    .post_process_models
                    .get(&provider_id)
                    .cloned()
                    .unwrap_or_default();
                ModeLlmDestination {
                    provider_id,
                    model_id,
                    inherited: true,
                }
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ModePromptSettings {
    pub preset: PromptPreset,
    pub source_prompt_id: Option<String>,
    pub custom_prompt: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ModeDeliverySettings {
    pub paste_method: PasteMethod,
    pub clipboard_handling: ClipboardHandling,
    pub auto_submit: bool,
    pub auto_submit_key: AutoSubmitKey,
    pub append_trailing_space: bool,
    pub paste_delay_ms: u64,
    pub paste_delay_after_ms: u64,
    pub reliable_paste: bool,
    pub typing_tool: TypingTool,
    pub external_script_path: Option<String>,
}

impl ModeDeliverySettings {
    fn from_legacy(settings: &AppSettings) -> Self {
        Self {
            paste_method: settings.paste_method,
            clipboard_handling: settings.clipboard_handling,
            auto_submit: settings.auto_submit,
            auto_submit_key: settings.auto_submit_key,
            append_trailing_space: settings.append_trailing_space,
            paste_delay_ms: settings.paste_delay_ms,
            paste_delay_after_ms: settings.paste_delay_after_ms,
            reliable_paste: settings.reliable_paste,
            typing_tool: settings.typing_tool,
            external_script_path: settings.external_script_path.clone(),
        }
    }
}

/// Delivery mechanics resolved from persisted settings at recording start.
///
/// This deliberately has no serde or Specta implementation: it is not another
/// settings owner and never crosses IPC. A run owns one immutable value, and
/// delivery accepts only this value rather than reading mutable settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryPlan {
    pub paste_method: PasteMethod,
    pub clipboard_handling: ClipboardHandling,
    pub auto_submit: bool,
    pub auto_submit_key: AutoSubmitKey,
    pub append_trailing_space: bool,
    pub paste_delay_ms: u64,
    pub paste_delay_after_ms: u64,
    pub reliable_paste: bool,
    pub typing_tool: TypingTool,
    pub external_script_path: Option<String>,
}

impl From<&ModeDeliverySettings> for DeliveryPlan {
    fn from(settings: &ModeDeliverySettings) -> Self {
        Self {
            paste_method: settings.paste_method,
            clipboard_handling: settings.clipboard_handling,
            auto_submit: settings.auto_submit,
            auto_submit_key: settings.auto_submit_key,
            append_trailing_space: settings.append_trailing_space,
            paste_delay_ms: settings.paste_delay_ms,
            paste_delay_after_ms: settings.paste_delay_after_ms,
            reliable_paste: settings.reliable_paste,
            typing_tool: settings.typing_tool,
            external_script_path: settings.external_script_path.clone(),
        }
    }
}

/// Chords are an IPC-only view over `AppSettings.bindings`; they are never
/// serialized inside a `ModeDefinition`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct ModeShortcuts {
    pub transcribe: ShortcutBinding,
    pub switch: ShortcutBinding,
}

/// One exact frontmost-application identity mapped to one mode. Application
/// bundle identities are the only match keys; URLs and sites never enter this
/// setting.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ModeActivationRule {
    pub app_id: String,
    pub mode_id: String,
}

/// The scope of one website activation rule. Exact rules match only the
/// normalized host; suffix rules also match its subdomains.
#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WebsiteHostMatch {
    Exact,
    Suffix,
}

/// One normalized website host mapped to one mode. This stores no URL, path,
/// query, fragment, or page content.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ModeWebsiteActivationRule {
    pub host: String,
    pub match_kind: WebsiteHostMatch,
    pub mode_id: String,
}

/// Persisted mode behavior. Chords intentionally do not live here: a mode's
/// binding IDs are derived from its ID and `AppSettings.bindings` owns the keys.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ModeDefinition {
    pub id: String,
    pub name: String,
    pub tone: Tone,
    pub context_policy: ContextPolicy,
    pub asr: ModeAsrSettings,
    pub llm: ModeLlmSettings,
    pub prompt: ModePromptSettings,
    pub delivery: ModeDeliverySettings,
}

impl ModeDefinition {
    fn from_legacy(settings: &AppSettings, id: &str, name: &str) -> Self {
        let selected_prompt = settings
            .post_process_selected_prompt_id
            .as_ref()
            .and_then(|id| {
                settings
                    .post_process_prompts
                    .iter()
                    .find(|prompt| &prompt.id == id)
            });

        Self {
            id: id.to_string(),
            name: name.to_string(),
            tone: Tone::Balanced,
            context_policy: ContextPolicy::Target,
            asr: ModeAsrSettings::from_legacy(settings),
            // A new mode inherits rather than copying: a baked copy is what
            // let the global choice and the mode's row disagree forever.
            llm: ModeLlmSettings {
                enabled: settings.post_process_enabled,
                provider_id: None,
                model_id: String::new(),
                spoken_instructions: false,
            },
            prompt: ModePromptSettings {
                preset: PromptPreset::MinimalistCleanup,
                source_prompt_id: selected_prompt.map(|prompt| prompt.id.clone()),
                custom_prompt: selected_prompt.map(|prompt| prompt.prompt.clone()),
            },
            delivery: ModeDeliverySettings::from_legacy(settings),
        }
    }

    fn specialist(settings: &AppSettings, id: &str, name: &str, preset: PromptPreset) -> Self {
        let mut mode = Self::from_legacy(settings, id, name);
        mode.llm.enabled = true;
        mode.prompt = ModePromptSettings {
            preset,
            source_prompt_id: None,
            custom_prompt: None,
        };
        mode
    }
}

/// A frontend-facing mode joined with its current persisted chords.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct ModeView {
    pub id: String,
    pub name: String,
    pub tone: Tone,
    pub context_policy: ContextPolicy,
    pub asr: ModeAsrSettings,
    pub llm: ModeLlmSettings,
    pub prompt: ModePromptSettings,
    pub delivery: ModeDeliverySettings,
    pub shortcuts: ModeShortcuts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptionIntent {
    /// Start or stop the active mode through the stable `transcribe` binding.
    ActiveMode,
    /// Preserve the old SIGUSR1 / CLI force-post-process behavior without a
    /// second persisted binding ID.
    ActiveModeWithPostProcess,
    /// Start or stop one explicitly addressed mode.
    Mode { mode_id: String },
    /// Start or stop a voice command run: the transcript is an instruction, and
    /// the text selected when the chord was pressed is what it edits.
    Command,
}

impl TranscriptionIntent {
    pub fn from_binding(binding_id: &str) -> Option<Self> {
        if binding_id == "transcribe" {
            return Some(Self::ActiveMode);
        }
        if binding_id == crate::command_mode::COMMAND_BINDING_ID {
            return Some(Self::Command);
        }

        match parse_mode_shortcut_id(binding_id) {
            Some((mode_id, ModeShortcutKind::Transcribe)) => Some(Self::Mode { mode_id }),
            _ => None,
        }
    }

    pub fn recording_id(&self) -> String {
        match self {
            Self::ActiveMode => "transcribe".to_string(),
            Self::ActiveModeWithPostProcess => "intent/active-mode/post-process".to_string(),
            Self::Mode { mode_id } => transcribe_binding_id(mode_id),
            Self::Command => crate::command_mode::COMMAND_BINDING_ID.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeShortcutKind {
    Transcribe,
    Switch,
}

pub fn transcribe_binding_id(mode_id: &str) -> String {
    if mode_id == DEFAULT_MODE_ID {
        "transcribe".to_string()
    } else {
        format!("mode/{mode_id}/transcribe")
    }
}

pub fn switch_binding_id(mode_id: &str) -> String {
    format!("mode/{mode_id}/switch")
}

pub fn parse_mode_shortcut_id(binding_id: &str) -> Option<(String, ModeShortcutKind)> {
    if binding_id == "transcribe" {
        return Some((DEFAULT_MODE_ID.to_string(), ModeShortcutKind::Transcribe));
    }

    let mut segments = binding_id.split('/');
    match (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) {
        (Some("mode"), Some(mode_id), Some("transcribe"), None) if !mode_id.is_empty() => {
            Some((mode_id.to_string(), ModeShortcutKind::Transcribe))
        }
        (Some("mode"), Some(mode_id), Some("switch"), None) if !mode_id.is_empty() => {
            Some((mode_id.to_string(), ModeShortcutKind::Switch))
        }
        _ => None,
    }
}

fn default_shortcut(index: usize, role: ModeShortcutKind) -> String {
    let number = index + 1;
    #[cfg(target_os = "macos")]
    let modifiers = match role {
        ModeShortcutKind::Transcribe => "option+shift",
        ModeShortcutKind::Switch => "option",
    };
    #[cfg(not(target_os = "macos"))]
    let modifiers = match role {
        ModeShortcutKind::Transcribe => "ctrl+alt+shift",
        ModeShortcutKind::Switch => "ctrl+alt",
    };
    format!("{modifiers}+{number}")
}

#[cfg(target_os = "windows")]
fn default_primary_transcribe_shortcut() -> &'static str {
    "ctrl+space"
}

#[cfg(target_os = "macos")]
fn default_primary_transcribe_shortcut() -> &'static str {
    "option+space"
}

#[cfg(target_os = "linux")]
fn default_primary_transcribe_shortcut() -> &'static str {
    "ctrl+space"
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn default_primary_transcribe_shortcut() -> &'static str {
    "alt+space"
}

fn mode_binding_template(
    mode: &ModeDefinition,
    index: usize,
    role: ModeShortcutKind,
) -> ShortcutBinding {
    let (id, name, description, default_binding) = match role {
        ModeShortcutKind::Transcribe if mode.id == DEFAULT_MODE_ID => (
            transcribe_binding_id(&mode.id),
            "Transcribe".to_string(),
            "Converts your speech using the active mode.".to_string(),
            default_primary_transcribe_shortcut().to_string(),
        ),
        ModeShortcutKind::Transcribe => {
            let binding = default_shortcut(index, role);
            (
                transcribe_binding_id(&mode.id),
                format!("Transcribe: {}", mode.name),
                format!("Transcribes using the {} mode.", mode.name),
                binding,
            )
        }
        ModeShortcutKind::Switch => {
            let binding = default_shortcut(index, role);
            (
                switch_binding_id(&mode.id),
                format!("Switch to {}", mode.name),
                format!("Makes {} the active transcription mode.", mode.name),
                binding,
            )
        }
    };

    ShortcutBinding {
        id,
        name,
        description,
        current_binding: default_binding.clone(),
        default_binding,
    }
}

fn mode_shortcuts(settings: &AppSettings, mode: &ModeDefinition, index: usize) -> ModeShortcuts {
    let transcribe_id = transcribe_binding_id(&mode.id);
    let switch_id = switch_binding_id(&mode.id);
    ModeShortcuts {
        transcribe: settings
            .bindings
            .get(&transcribe_id)
            .cloned()
            .unwrap_or_else(|| mode_binding_template(mode, index, ModeShortcutKind::Transcribe)),
        switch: settings
            .bindings
            .get(&switch_id)
            .cloned()
            .unwrap_or_else(|| mode_binding_template(mode, index, ModeShortcutKind::Switch)),
    }
}

pub fn default_modes(settings: &AppSettings) -> Vec<ModeDefinition> {
    vec![
        ModeDefinition::from_legacy(settings, DEFAULT_MODE_ID, "Message"),
        ModeDefinition::specialist(settings, "email", "Email", PromptPreset::Email),
        ModeDefinition::specialist(settings, "meeting", "Meeting", PromptPreset::Meeting),
        ModeDefinition::specialist(settings, "notes", "Notes", PromptPreset::Notes),
    ]
}

/// Converges persisted mode metadata and derives only missing binding records.
/// Existing chord values always win; no reconciliation path writes them back.
pub fn ensure_mode_settings(settings: &mut AppSettings) -> bool {
    let mut changed = false;
    if settings.modes.is_empty() {
        settings.modes = default_modes(settings);
        settings.active_mode_id = DEFAULT_MODE_ID.to_string();
        settings.modes_revision = settings.modes_revision.max(1);
        changed = true;
    }

    if !settings
        .modes
        .iter()
        .any(|mode| mode.id == settings.active_mode_id)
    {
        settings.active_mode_id = settings.modes[0].id.clone();
        changed = true;
    }

    let valid_mode_ids: HashSet<String> =
        settings.modes.iter().map(|mode| mode.id.clone()).collect();
    let mut seen_app_ids = HashSet::new();
    let rules_before = settings.mode_activation_rules.len();
    settings.mode_activation_rules.retain(|rule| {
        !rule.app_id.is_empty()
            && valid_mode_ids.contains(&rule.mode_id)
            && seen_app_ids.insert(rule.app_id.clone())
    });
    if settings.mode_activation_rules.len() != rules_before {
        changed = true;
    }

    let mut seen_website_rules = HashSet::new();
    let mut website_rules_changed = false;
    settings.mode_website_activation_rules.retain_mut(|rule| {
        let Some(host) = context::normalize_website_host(&rule.host) else {
            website_rules_changed = true;
            return false;
        };
        if rule.host != host {
            rule.host = host;
            website_rules_changed = true;
        }
        let valid = valid_mode_ids.contains(&rule.mode_id)
            && seen_website_rules.insert((rule.host.clone(), rule.match_kind));
        if !valid {
            website_rules_changed = true;
        }
        valid
    });
    if website_rules_changed {
        changed = true;
    }

    // Schema migration transfers this record before `ensure_mode_settings` runs.
    // Any later appearance is obsolete and must never become a live shortcut.
    if settings
        .bindings
        .remove(LEGACY_POST_PROCESS_BINDING_ID)
        .is_some()
    {
        changed = true;
    }

    let derived_bindings: Vec<_> = settings
        .modes
        .iter()
        .enumerate()
        .flat_map(|(index, mode)| {
            [
                mode_binding_template(mode, index, ModeShortcutKind::Transcribe),
                mode_binding_template(mode, index, ModeShortcutKind::Switch),
            ]
        })
        .collect();
    for binding in derived_bindings {
        if let std::collections::hash_map::Entry::Vacant(entry) =
            settings.bindings.entry(binding.id.clone())
        {
            entry.insert(binding);
            changed = true;
        }
    }

    changed
}

/// Whether a generation-only clipboard observer may run. Full mode context and
/// the global ceiling are both required; this avoids even polling clipboard
/// metadata until the user has enabled the only policy that can use it.
pub(crate) fn should_watch_recent_clipboard(settings: &AppSettings) -> bool {
    settings.context_policy_ceiling == ContextPolicy::Full
        && settings
            .modes
            .iter()
            .any(|mode| mode.context_policy == ContextPolicy::Full)
}

pub(crate) fn refresh_clipboard_context_watcher(settings: &AppSettings) {
    context::set_clipboard_watch_enabled(should_watch_recent_clipboard(settings));
}

pub fn active_mode(settings: &AppSettings) -> Option<&ModeDefinition> {
    settings
        .modes
        .iter()
        .find(|mode| mode.id == settings.active_mode_id)
        .or_else(|| settings.modes.first())
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ModeSettingsSnapshot {
    pub modes: Vec<ModeView>,
    pub active_mode_id: String,
    pub revision: u64,
    pub mode_activation_rules: Vec<ModeActivationRule>,
    pub mode_website_activation_rules: Vec<ModeWebsiteActivationRule>,
}

fn mode_view(settings: &AppSettings, mode: &ModeDefinition, index: usize) -> ModeView {
    ModeView {
        id: mode.id.clone(),
        name: mode.name.clone(),
        tone: mode.tone,
        context_policy: mode.context_policy,
        asr: mode.asr.clone(),
        llm: mode.llm.clone(),
        prompt: mode.prompt.clone(),
        delivery: mode.delivery.clone(),
        shortcuts: mode_shortcuts(settings, mode, index),
    }
}

pub(crate) fn mode_settings_snapshot(settings: &AppSettings) -> ModeSettingsSnapshot {
    ModeSettingsSnapshot {
        modes: settings
            .modes
            .iter()
            .enumerate()
            .map(|(index, mode)| mode_view(settings, mode, index))
            .collect(),
        active_mode_id: settings.active_mode_id.clone(),
        mode_website_activation_rules: settings.mode_website_activation_rules.clone(),
        revision: settings.modes_revision,
        mode_activation_rules: settings.mode_activation_rules.clone(),
    }
}

/// Full mode state after one committed mutation. Consumers can replace their
/// cached mode snapshot without reconstructing a delta.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct ModesChangedEvent(pub ModeSettingsSnapshot);

pub(crate) fn emit_modes_changed(app: &AppHandle, settings: &ModeSettingsSnapshot) {
    let _ = ModesChangedEvent(settings.clone()).emit(app);
}

#[derive(Clone, Debug, Serialize, Type, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModeMutationError {
    StaleRevision {
        expected_revision: u64,
        actual_revision: u64,
    },
    InvalidModeId,
    EmptyName,
    CannotDeleteDefault,
    UnknownMode {
        mode_id: String,
    },
    DuplicateModeId {
        mode_id: String,
    },
    InvalidReorder,
    InvalidAppIdentity,
    FrontmostApplicationUnavailable,
    InvalidWebsiteHost,
    WebsiteActivationConsentRequired,
    FrontmostWebsiteUnavailable,
    WebsiteActivationSecureField,
    /// The mode change is in the running process and not on disk. Reported
    /// rather than swallowed because the next launch reads the old modes.
    NotPersisted,
    #[cfg(not(target_os = "macos"))]
    ModeActivationUnsupported,
}

impl From<crate::settings::SettingsPersistError> for ModeMutationError {
    fn from(_: crate::settings::SettingsPersistError) -> Self {
        Self::NotPersisted
    }
}

fn check_expected_revision(
    settings: &AppSettings,
    expected_revision: u64,
) -> Result<(), ModeMutationError> {
    if settings.modes_revision != expected_revision {
        return Err(ModeMutationError::StaleRevision {
            expected_revision,
            actual_revision: settings.modes_revision,
        });
    }
    Ok(())
}

fn apply_upsert_mode(
    settings: &mut AppSettings,
    mode: ModeDefinition,
    expected_revision: u64,
) -> Result<(), ModeMutationError> {
    check_expected_revision(settings, expected_revision)?;
    if !is_valid_mode_id(&mode.id) {
        return Err(ModeMutationError::InvalidModeId);
    }
    if mode.name.trim().is_empty() {
        return Err(ModeMutationError::EmptyName);
    }

    match settings
        .modes
        .iter()
        .position(|existing| existing.id == mode.id)
    {
        Some(position) => settings.modes[position] = mode,
        None => settings.modes.push(mode),
    }
    ensure_mode_settings(settings);
    settings.modes_revision = settings.modes_revision.saturating_add(1);
    Ok(())
}

fn apply_delete_mode(
    settings: &mut AppSettings,
    mode_id: &str,
    expected_revision: u64,
) -> Result<(), ModeMutationError> {
    check_expected_revision(settings, expected_revision)?;
    if mode_id == DEFAULT_MODE_ID {
        return Err(ModeMutationError::CannotDeleteDefault);
    }

    let position = settings
        .modes
        .iter()
        .position(|mode| mode.id == mode_id)
        .ok_or_else(|| ModeMutationError::UnknownMode {
            mode_id: mode_id.to_string(),
        })?;
    settings.modes.remove(position);
    settings.bindings.remove(&transcribe_binding_id(mode_id));
    settings.bindings.remove(&switch_binding_id(mode_id));
    if settings.active_mode_id == mode_id {
        settings.active_mode_id = DEFAULT_MODE_ID.to_string();
    }
    ensure_mode_settings(settings);
    settings.modes_revision = settings.modes_revision.saturating_add(1);
    Ok(())
}

fn apply_reorder_modes(
    settings: &mut AppSettings,
    ordered_ids: &[String],
    expected_revision: u64,
) -> Result<(), ModeMutationError> {
    check_expected_revision(settings, expected_revision)?;

    let current_ids: HashSet<_> = settings.modes.iter().map(|mode| mode.id.as_str()).collect();
    let mut seen = HashSet::with_capacity(ordered_ids.len());
    for mode_id in ordered_ids {
        if !seen.insert(mode_id.as_str()) {
            return Err(ModeMutationError::DuplicateModeId {
                mode_id: mode_id.clone(),
            });
        }
        if !current_ids.contains(mode_id.as_str()) {
            return Err(ModeMutationError::UnknownMode {
                mode_id: mode_id.clone(),
            });
        }
    }
    if ordered_ids.len() != current_ids.len() || seen.len() != current_ids.len() {
        return Err(ModeMutationError::InvalidReorder);
    }

    let mut reordered = Vec::with_capacity(ordered_ids.len());
    for mode_id in ordered_ids {
        let mode = settings
            .modes
            .iter()
            .find(|mode| mode.id == *mode_id)
            .cloned()
            .ok_or_else(|| ModeMutationError::UnknownMode {
                mode_id: mode_id.clone(),
            })?;
        reordered.push(mode);
    }
    settings.modes = reordered;
    settings.modes_revision = settings.modes_revision.saturating_add(1);
    Ok(())
}

fn apply_set_active_mode(settings: &mut AppSettings, mode_id: &str) -> Result<(), String> {
    if !settings.modes.iter().any(|mode| mode.id == mode_id) {
        return Err(format!("Mode '{mode_id}' does not exist"));
    }
    settings.active_mode_id = mode_id.to_string();
    Ok(())
}

fn apply_capture_mode_activation_rule(
    settings: &mut AppSettings,
    app_id: String,
    mode_id: &str,
    expected_revision: u64,
) -> Result<(), ModeMutationError> {
    check_expected_revision(settings, expected_revision)?;
    if app_id.is_empty() {
        return Err(ModeMutationError::InvalidAppIdentity);
    }
    if !settings.modes.iter().any(|mode| mode.id == mode_id) {
        return Err(ModeMutationError::UnknownMode {
            mode_id: mode_id.to_string(),
        });
    }

    settings
        .mode_activation_rules
        .retain(|rule| rule.app_id != app_id);
    settings.mode_activation_rules.push(ModeActivationRule {
        app_id,
        mode_id: mode_id.to_string(),
    });
    settings.modes_revision = settings.modes_revision.saturating_add(1);
    Ok(())
}

fn apply_remove_mode_activation_rule(
    settings: &mut AppSettings,
    app_id: &str,
    expected_revision: u64,
) -> Result<(), ModeMutationError> {
    check_expected_revision(settings, expected_revision)?;
    if app_id.is_empty() {
        return Err(ModeMutationError::InvalidAppIdentity);
    }
    settings
        .mode_activation_rules
        .retain(|rule| rule.app_id != app_id);
    settings.modes_revision = settings.modes_revision.saturating_add(1);
    Ok(())
}

fn apply_capture_mode_website_activation_rule(
    settings: &mut AppSettings,
    host: String,
    match_kind: WebsiteHostMatch,
    mode_id: &str,
    expected_revision: u64,
) -> Result<(), ModeMutationError> {
    check_expected_revision(settings, expected_revision)?;
    if !settings.context_url_capture_enabled {
        return Err(ModeMutationError::WebsiteActivationConsentRequired);
    }
    let host =
        context::normalize_website_host(&host).ok_or(ModeMutationError::InvalidWebsiteHost)?;
    if !settings.modes.iter().any(|mode| mode.id == mode_id) {
        return Err(ModeMutationError::UnknownMode {
            mode_id: mode_id.to_string(),
        });
    }

    settings
        .mode_website_activation_rules
        .retain(|rule| rule.host != host || rule.match_kind != match_kind);
    settings
        .mode_website_activation_rules
        .push(ModeWebsiteActivationRule {
            host,
            match_kind,
            mode_id: mode_id.to_string(),
        });
    settings.modes_revision = settings.modes_revision.saturating_add(1);
    Ok(())
}

fn apply_remove_mode_website_activation_rule(
    settings: &mut AppSettings,
    host: &str,
    match_kind: WebsiteHostMatch,
    expected_revision: u64,
) -> Result<(), ModeMutationError> {
    check_expected_revision(settings, expected_revision)?;
    let host =
        context::normalize_website_host(host).ok_or(ModeMutationError::InvalidWebsiteHost)?;
    settings
        .mode_website_activation_rules
        .retain(|rule| rule.host != host || rule.match_kind != match_kind);
    settings.modes_revision = settings.modes_revision.saturating_add(1);
    Ok(())
}

fn commit_mode_mutation<E>(
    app: &AppHandle,
    apply: impl FnOnce(&mut AppSettings) -> Result<(), E>,
) -> Result<ModeSettingsSnapshot, E>
where
    E: From<crate::settings::SettingsPersistError>,
{
    let (result, old_bindings, new_bindings) = settings::try_update_settings(app, |settings| {
        let old_bindings = settings.bindings.clone();
        apply(settings)?;
        let result = mode_settings_snapshot(settings);
        Ok::<_, E>((result, old_bindings, settings.bindings.clone()))
    })?;

    crate::shortcut::reconcile_mode_shortcuts(app, &old_bindings, &new_bindings);
    emit_modes_changed(app, &result);
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub fn get_modes(app: AppHandle) -> ModeSettingsSnapshot {
    mode_settings_snapshot(&settings::get_settings(&app))
}

#[tauri::command]
#[specta::specta]
pub fn set_active_mode(app: AppHandle, mode_id: String) -> Result<ModeSettingsSnapshot, String> {
    commit_mode_mutation(&app, |settings| apply_set_active_mode(settings, &mode_id))
}

#[tauri::command]
#[specta::specta]
pub fn upsert_mode(
    app: AppHandle,
    mode: ModeDefinition,
    expected_revision: u64,
) -> Result<ModeSettingsSnapshot, ModeMutationError> {
    commit_mode_mutation(&app, |settings| {
        apply_upsert_mode(settings, mode, expected_revision)
    })
}

#[tauri::command]
#[specta::specta]
pub fn delete_mode(
    app: AppHandle,
    mode_id: String,
    expected_revision: u64,
) -> Result<ModeSettingsSnapshot, ModeMutationError> {
    commit_mode_mutation(&app, |settings| {
        apply_delete_mode(settings, &mode_id, expected_revision)
    })
}

#[tauri::command]
#[specta::specta]
pub fn reorder_modes(
    app: AppHandle,
    ordered_ids: Vec<String>,
    expected_revision: u64,
) -> Result<ModeSettingsSnapshot, ModeMutationError> {
    commit_mode_mutation(&app, |settings| {
        apply_reorder_modes(settings, &ordered_ids, expected_revision)
    })
}

/// Captures the application that was active immediately before the mode editor
/// became visible. Hiding the window briefly lets macOS return focus to that
/// application without reading Accessibility data or a browser URL.
#[tauri::command]
#[specta::specta]
pub async fn capture_mode_activation_rule(
    app: AppHandle,
    mode_id: String,
    expected_revision: u64,
) -> Result<ModeSettingsSnapshot, ModeMutationError> {
    #[cfg(target_os = "macos")]
    {
        let main_window = app
            .get_webview_window("main")
            .ok_or(ModeMutationError::FrontmostApplicationUnavailable)?;
        main_window
            .hide()
            .map_err(|_| ModeMutationError::FrontmostApplicationUnavailable)?;
        std::thread::sleep(Duration::from_millis(120));
        let app_id = context::frontmost_application_identifier();
        crate::show_main_window(&app);
        let app_id = app_id.ok_or(ModeMutationError::FrontmostApplicationUnavailable)?;
        commit_mode_mutation(&app, |settings| {
            apply_capture_mode_activation_rule(settings, app_id, &mode_id, expected_revision)
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, mode_id, expected_revision);
        Err(ModeMutationError::ModeActivationUnsupported)
    }
}

#[tauri::command]
#[specta::specta]
pub fn remove_mode_activation_rule(
    app: AppHandle,
    app_id: String,
    expected_revision: u64,
) -> Result<ModeSettingsSnapshot, ModeMutationError> {
    commit_mode_mutation(&app, |settings| {
        apply_remove_mode_activation_rule(settings, &app_id, expected_revision)
    })
}

/// Captures the browser host visible immediately before the editor opens. This
/// command is gated by the separate browser-URL consent and persists only the
/// normalized host, never the captured URL.
#[tauri::command]
#[specta::specta]
pub async fn capture_mode_website_activation_rule(
    app: AppHandle,
    mode_id: String,
    match_kind: WebsiteHostMatch,
    expected_revision: u64,
) -> Result<ModeSettingsSnapshot, ModeMutationError> {
    #[cfg(target_os = "macos")]
    {
        let settings = settings::get_settings(&app);
        check_expected_revision(&settings, expected_revision)?;
        if !settings.context_url_capture_enabled {
            return Err(ModeMutationError::WebsiteActivationConsentRequired);
        }

        let main_window = app
            .get_webview_window("main")
            .ok_or(ModeMutationError::FrontmostWebsiteUnavailable)?;
        main_window
            .hide()
            .map_err(|_| ModeMutationError::FrontmostWebsiteUnavailable)?;
        std::thread::sleep(Duration::from_millis(120));
        let capture = context::capture_frontmost_website_host();
        crate::show_main_window(&app);
        let host = match capture {
            context::WebsiteHostCapture::Captured(host) => host,
            context::WebsiteHostCapture::SecureField => {
                return Err(ModeMutationError::WebsiteActivationSecureField);
            }
            context::WebsiteHostCapture::Unavailable => {
                return Err(ModeMutationError::FrontmostWebsiteUnavailable);
            }
        };
        commit_mode_mutation(&app, |settings| {
            apply_capture_mode_website_activation_rule(
                settings,
                host,
                match_kind,
                &mode_id,
                expected_revision,
            )
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, mode_id, match_kind, expected_revision);
        Err(ModeMutationError::ModeActivationUnsupported)
    }
}

#[tauri::command]
#[specta::specta]
pub fn remove_mode_website_activation_rule(
    app: AppHandle,
    host: String,
    match_kind: WebsiteHostMatch,
    expected_revision: u64,
) -> Result<ModeSettingsSnapshot, ModeMutationError> {
    commit_mode_mutation(&app, |settings| {
        apply_remove_mode_website_activation_rule(settings, &host, match_kind, expected_revision)
    })
}

fn is_valid_mode_id(id: &str) -> bool {
    !id.is_empty()
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

fn effective_vocabulary(
    global: &[VocabularyEntry],
    overrides: &[VocabularyEntry],
) -> Vec<VocabularyEntry> {
    let overridden_spoken: HashSet<String> = overrides
        .iter()
        .map(|entry| vocabulary_spoken_key(&entry.spoken))
        .collect();
    let mut entries = Vec::with_capacity(global.len() + overrides.len());
    entries.extend(
        global
            .iter()
            .filter(|entry| !overridden_spoken.contains(&vocabulary_spoken_key(&entry.spoken)))
            .cloned(),
    );
    entries.extend(overrides.iter().cloned());
    entries
}

#[derive(Clone, Debug)]
pub struct AsrPlan {
    pub model_id: String,
    pub language: String,
    pub translate_to_english: bool,
    pub custom_words: Vec<VocabularyEntry>,
    pub emoji_replacements: Vec<EmojiReplacement>,
    pub emoji_replacements_enabled: bool,
    pub snippets: Vec<Snippet>,
    pub snippets_enabled: bool,
    pub correction_threshold: f64,
    pub filler_word_removal_enabled: bool,
    pub custom_filler_words: Option<Vec<String>>,
    pub vad_enabled: bool,
    pub literal_punctuation: bool,
    pub english_spelling: EnglishSpelling,
    pub transcribe_accelerator: TranscribeAcceleratorSetting,
    pub transcribe_gpu_device: Option<String>,
    pub ort_accelerator: OrtAcceleratorSetting,
    pub replacements_rules: Vec<ReplacementRule>,
    pub replacements_enabled: bool,
    pub spoken_edits_enabled: bool,
}

impl AsrPlan {
    pub(crate) fn from_mode(settings: &AppSettings, mode: &ModeAsrSettings) -> Self {
        // An empty per-mode model means "inherit the globally selected model".
        // The default modes are created before onboarding has picked a model,
        // so they persist an empty id; resolving the inheritance here keeps
        // settings.selected_model the single source of truth for it.
        let model_id = if mode.model_id.trim().is_empty() {
            settings.selected_model.clone()
        } else {
            mode.model_id.clone()
        };
        Self {
            model_id,
            language: mode.language.clone(),
            translate_to_english: mode.translate_to_english,
            custom_words: effective_vocabulary(&settings.custom_words, &mode.custom_words),
            emoji_replacements: settings.emoji_replacements.clone(),
            emoji_replacements_enabled: settings.emoji_replacements_enabled,
            snippets: settings.snippets.clone(),
            snippets_enabled: settings.snippets_enabled,
            correction_threshold: settings.word_correction_threshold,
            filler_word_removal_enabled: mode.filler_word_removal_enabled,
            custom_filler_words: mode.custom_filler_words.clone(),
            literal_punctuation: mode.literal_punctuation,
            english_spelling: settings.english_spelling,
            vad_enabled: mode.vad_enabled,
            transcribe_accelerator: settings.transcribe_accelerator,
            transcribe_gpu_device: settings.transcribe_gpu_device.clone(),
            ort_accelerator: settings.ort_accelerator,
            replacements_rules: settings.replacements_rules.clone(),
            replacements_enabled: settings.replacements_enabled,
            spoken_edits_enabled: settings.spoken_edits_enabled,
        }
    }

    pub fn from_settings(settings: &AppSettings) -> Self {
        Self {
            model_id: settings.selected_model.clone(),
            language: settings.selected_language.clone(),
            translate_to_english: settings.translate_to_english,
            custom_words: settings.custom_words.clone(),
            emoji_replacements: settings.emoji_replacements.clone(),
            emoji_replacements_enabled: settings.emoji_replacements_enabled,
            snippets: settings.snippets.clone(),
            snippets_enabled: settings.snippets_enabled,
            correction_threshold: settings.word_correction_threshold,
            filler_word_removal_enabled: settings.filler_word_removal_enabled,
            custom_filler_words: settings.custom_filler_words.clone(),
            vad_enabled: settings.vad_enabled,
            literal_punctuation: false,
            english_spelling: settings.english_spelling,
            transcribe_accelerator: settings.transcribe_accelerator,
            transcribe_gpu_device: settings.transcribe_gpu_device.clone(),
            ort_accelerator: settings.ort_accelerator,
            replacements_rules: settings.replacements_rules.clone(),
            replacements_enabled: settings.replacements_enabled,
            spoken_edits_enabled: settings.spoken_edits_enabled,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedLlmSettings {
    pub provider: PostProcessProvider,
    pub model_id: String,
    pub(crate) endpoint: PostProcessEndpoint,
}

#[derive(Clone, Debug)]
pub struct PromptPlan {
    pub tone: Tone,
    pub preset: PromptPreset,
    pub custom_prompt: Option<String>,
    pub llm: Option<ResolvedLlmSettings>,
    pub post_process_requested: bool,
    /// Samples of the user's own writing, frozen at run start like every other
    /// plan field so a mid-run settings edit cannot change the prompt.
    pub persona_samples: Vec<PersonaSample>,
    /// Whether this run reads a trailing `Sona, …` sentence as an edit
    /// instruction. False whenever the run has no rewrite, whatever the mode
    /// says: without a rewrite provider the instruction has nowhere to go.
    /// Frozen with the rest of the plan, so turning the toggle off
    /// mid-dictation cannot leave the cue text in the delivery.
    pub spoken_instructions: bool,
}

#[derive(Clone, Debug)]
pub struct ContextPlan {
    requested_policy: ContextPolicy,
    ceiling: ContextPolicy,
    effective_policy: ContextPolicy,
    pending: Arc<PendingContext>,
}

impl ContextPlan {
    pub fn requested_policy(&self) -> ContextPolicy {
        self.requested_policy
    }

    pub fn ceiling(&self) -> ContextPolicy {
        self.ceiling
    }

    pub fn effective_policy(&self) -> ContextPolicy {
        self.effective_policy
    }

    pub fn snapshot(&self) -> &ContextSnapshot {
        self.pending.snapshot()
    }

    fn without_live_capture(&mut self) {
        self.pending = Arc::new(PendingContext::resolved(ContextSnapshot::unavailable(
            self.requested_policy,
            self.ceiling,
            crate::context::ContextSourceStatus::NotRequested,
        )));
    }
}

/// The frozen operand of a voice command run: the text that was selected when
/// the user pressed the command chord.
///
/// The selection is captured before the microphone opens, so the rewrite edits
/// what the user was looking at when they started speaking even if the screen
/// changes while they speak. Its presence on a [`RunPlan`] is what makes the run
/// a command; there is no second flag to keep in step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPlan {
    selection: String,
}

impl CommandPlan {
    pub(crate) fn new(selection: String) -> Self {
        Self { selection }
    }

    pub fn selection(&self) -> &str {
        &self.selection
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RequestedEngine {
    #[default]
    Local,
    #[serde(rename = "deepgram_nova_3", alias = "deepgram_nova3")]
    DeepgramNova3,
    ElevenLabsScribeV2,
}

impl RequestedEngine {
    pub const fn cloud_provider(self) -> Option<CloudSttProvider> {
        match self {
            Self::Local => None,
            Self::DeepgramNova3 => Some(CloudSttProvider::DeepgramNova3),
            Self::ElevenLabsScribeV2 => Some(CloudSttProvider::ElevenLabsScribeV2),
        }
    }

    /// The route's durable name, which keys receipt rows and user-visible
    /// copy. It deliberately does not track the serialized wire value: serde
    /// writes Deepgram as `deepgram_nova_3` to match the settings UI, while
    /// stored rows keep the older `deepgram_nova3` spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::DeepgramNova3 => "deepgram_nova3",
            Self::ElevenLabsScribeV2 => "eleven_labs_scribe_v2",
        }
    }
}

/// Which rule selected the mode frozen into a run. The receipt deliberately
/// records the decision without copying a frontmost application's identity.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ModeSelectionSource {
    #[default]
    ActiveMode,
    AppActivationRule,
    WebsiteActivationRule,
    ExplicitModeShortcut,
}

/// Immutable cloud-only data derived before capture starts. Credentials never
/// belong here: the worker receives a one-use native secret separately.
/// Durable cloud outcome attached to the run receipt, never a provider body.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CloudReceiptStatus {
    #[default]
    NotRequested,
    Final,
    Fallback,
    HeldCloudUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudRunPlan {
    provider: CloudSttProvider,
    language: Option<String>,
    keyterms: Box<[String]>,
    timestamps: bool,
}

impl CloudRunPlan {
    fn new(
        provider: CloudSttProvider,
        language: String,
        keyterms: Vec<String>,
        timestamps: bool,
    ) -> Self {
        Self {
            provider,
            language: (!language.eq_ignore_ascii_case("auto")).then_some(language),
            keyterms: keyterms.into_boxed_slice(),
            timestamps,
        }
    }

    #[cfg(feature = "cloud-realtime")]
    pub const fn provider(&self) -> CloudSttProvider {
        self.provider
    }

    #[cfg(feature = "cloud-realtime")]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    #[cfg(feature = "cloud-realtime")]
    pub fn keyterms(&self) -> &[String] {
        &self.keyterms
    }

    #[cfg(feature = "cloud-realtime")]
    pub const fn timestamps(&self) -> bool {
        self.timestamps
    }
}

/// A rejected plan never starts capture. The UI receives a closed reason rather
/// than interpreting a remote failure as a local transcription error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RunPlanError {
    NoMatchingMode,
    MissingPostProcessProvider,
    InvalidPostProcessDestination,
    PostProcessConsentRequired,
    CloudConsentRequired {
        provider: CloudSttProvider,
    },
    CloudPrivacyConsentRequired {
        provider: CloudSttProvider,
    },
    CloudTimestampsRequired {
        provider: CloudSttProvider,
    },
    CloudFallbackModelRequired {
        provider: CloudSttProvider,
    },
    /// The command chord was pressed with nothing selected. Nothing was
    /// recorded: a command needs an operand before it needs audio.
    CommandWithoutSelection,
}

impl fmt::Display for RunPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMatchingMode => {
                formatter.write_str("No matching transcription mode is configured")
            }
            Self::MissingPostProcessProvider => {
                formatter.write_str("The selected post-processing provider is not configured")
            }
            Self::InvalidPostProcessDestination => {
                formatter.write_str("The selected post-processing destination is invalid")
            }
            Self::PostProcessConsentRequired => {
                formatter.write_str("Remote post-processing requires destination consent")
            }
            Self::CloudConsentRequired { .. } => {
                formatter.write_str("Cloud transcription requires provider consent")
            }
            Self::CloudPrivacyConsentRequired { .. } => formatter
                .write_str("Cloud transcription requires the provider privacy acknowledgement"),
            Self::CloudTimestampsRequired { .. } => {
                formatter.write_str("Cloud transcription requires word timestamps")
            }
            Self::CloudFallbackModelRequired { .. } => {
                formatter.write_str("Cloud transcription requires a local fallback model")
            }
            Self::CommandWithoutSelection => {
                formatter.write_str("Voice command mode needs text selected before you speak")
            }
        }
    }
}

impl std::error::Error for RunPlanError {}

/// All state that can change a run's transcript or delivery is frozen before
/// recording starts. Resource-retention timing stays outside this value.
#[derive(Clone, Debug)]
pub struct RunPlan {
    pub run_id: u64,
    pub run_started_at_ms: u64,
    pub settings_revision: u64,
    pub mode_id: String,
    asr: AsrPlan,
    prompt: PromptPlan,
    context: ContextPlan,
    delivery: DeliveryPlan,
    requested_engine: RequestedEngine,
    local_fallback: Option<AsrPlan>,
    cloud: Option<CloudRunPlan>,
    mode_selection_source: ModeSelectionSource,
    /// Present exactly for a voice command run, carrying the selection it
    /// rewrites. See [`CommandPlan`].
    command: Option<CommandPlan>,
}

fn matching_app_activation_mode<'a>(
    settings: &'a AppSettings,
    application_id: Option<&str>,
) -> Option<&'a ModeDefinition> {
    let application_id = application_id?;
    let rule = settings
        .mode_activation_rules
        .iter()
        .find(|rule| rule.app_id == application_id)?;
    settings.modes.iter().find(|mode| mode.id == rule.mode_id)
}

fn website_rule_matches(rule: &ModeWebsiteActivationRule, host: &str) -> bool {
    host == rule.host
        || matches!(rule.match_kind, WebsiteHostMatch::Suffix)
            && host
                .strip_suffix(&rule.host)
                .is_some_and(|prefix| prefix.ends_with('.'))
}

fn website_rule_precedes(
    candidate: &ModeWebsiteActivationRule,
    incumbent: &ModeWebsiteActivationRule,
) -> bool {
    (
        matches!(candidate.match_kind, WebsiteHostMatch::Exact),
        candidate.host.len(),
    ) > (
        matches!(incumbent.match_kind, WebsiteHostMatch::Exact),
        incumbent.host.len(),
    )
}

fn matching_website_activation_mode<'a>(
    settings: &'a AppSettings,
    website_host: Option<&str>,
) -> Option<&'a ModeDefinition> {
    let host = context::normalize_website_host(website_host?)?;
    let mut selected_rule: Option<&ModeWebsiteActivationRule> = None;
    for rule in &settings.mode_website_activation_rules {
        let should_select = website_rule_matches(rule, &host)
            && match selected_rule {
                Some(current) => website_rule_precedes(rule, current),
                None => true,
            };
        if should_select {
            selected_rule = Some(rule);
        }
    }
    selected_rule.and_then(|rule| settings.modes.iter().find(|mode| mode.id == rule.mode_id))
}

fn website_automation_enabled(settings: &AppSettings) -> bool {
    settings.context_url_capture_enabled
        && settings.context_policy_ceiling.wants_target()
        && !settings.mode_website_activation_rules.is_empty()
}

impl RunPlan {
    pub fn for_intent(
        settings: &AppSettings,
        intent: &TranscriptionIntent,
    ) -> Result<Self, RunPlanError> {
        // A command run resolves its own plan before anything is read from
        // the screen: the text it edits has to be captured before any
        // application, activation rule, or website is considered.
        if matches!(intent, TranscriptionIntent::Command) {
            return Self::for_command(settings);
        }
        let frontmost_application_id = context::frontmost_application_identifier();
        let website_rules_enabled = matches!(
            intent,
            TranscriptionIntent::ActiveMode | TranscriptionIntent::ActiveModeWithPostProcess
        ) && website_automation_enabled(settings);
        let website_host = if website_rules_enabled
            && matching_app_activation_mode(settings, frontmost_application_id.as_deref()).is_none()
        {
            context::frontmost_website_host().host().map(str::to_owned)
        } else {
            None
        };
        Self::for_intent_with_automation_target(
            settings,
            intent,
            frontmost_application_id.as_deref(),
            website_host.as_deref(),
        )
    }

    fn for_intent_with_automation_target(
        settings: &AppSettings,
        intent: &TranscriptionIntent,
        frontmost_application_id: Option<&str>,
        website_host: Option<&str>,
    ) -> Result<Self, RunPlanError> {
        let (mode, post_process_override, mode_selection_source) = match intent {
            // Exhaustiveness only. `for_intent` routes a command run before
            // this function is reached, and the ordering rationale lives on
            // that early return.
            TranscriptionIntent::Command => return Self::for_command(settings),
            TranscriptionIntent::Mode { mode_id } => (
                settings.modes.iter().find(|mode| mode.id == *mode_id),
                None,
                ModeSelectionSource::ExplicitModeShortcut,
            ),
            TranscriptionIntent::ActiveMode | TranscriptionIntent::ActiveModeWithPostProcess => {
                let post_process_override =
                    matches!(intent, TranscriptionIntent::ActiveModeWithPostProcess)
                        .then_some(true);
                let app_mode = matching_app_activation_mode(settings, frontmost_application_id);
                let website_mode = if app_mode.is_none() && website_automation_enabled(settings) {
                    matching_website_activation_mode(settings, website_host)
                } else {
                    None
                };
                let (mode, source) = if let Some(mode) = app_mode {
                    (Some(mode), ModeSelectionSource::AppActivationRule)
                } else if let Some(mode) = website_mode {
                    (Some(mode), ModeSelectionSource::WebsiteActivationRule)
                } else {
                    (active_mode(settings), ModeSelectionSource::ActiveMode)
                };
                (mode, post_process_override, source)
            }
        };
        let mode = mode.cloned().ok_or(RunPlanError::NoMatchingMode)?;
        Self::for_mode(
            settings,
            mode,
            post_process_override,
            mode_selection_source,
            false,
        )
    }

    /// Freeze the active mode for a file import without capturing application
    /// context or admitting a post-processing/delivery path. The mode's ASR
    /// choices and provenance still belong to the resulting history receipt.
    pub fn for_media_import(settings: &AppSettings) -> Result<Self, RunPlanError> {
        let mode = active_mode(settings)
            .cloned()
            .ok_or(RunPlanError::NoMatchingMode)?;
        let requested_policy = mode.context_policy;
        let ceiling = settings.context_policy_ceiling;
        let effective_policy = requested_policy.clamp_to(ceiling);

        Ok(Self {
            run_id: NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed),
            run_started_at_ms: run_now_ms(),
            settings_revision: settings.modes_revision,
            mode_id: mode.id.clone(),
            asr: AsrPlan::from_mode(settings, &mode.asr),
            prompt: PromptPlan {
                tone: mode.tone,
                preset: mode.prompt.preset,
                custom_prompt: None,
                llm: None,
                post_process_requested: false,
                persona_samples: Vec::new(),
                spoken_instructions: false,
            },
            context: ContextPlan {
                requested_policy,
                ceiling,
                effective_policy,
                pending: Arc::new(PendingContext::resolved(ContextSnapshot::unavailable(
                    requested_policy,
                    ceiling,
                    crate::context::ContextSourceStatus::NotRequested,
                ))),
            },
            delivery: DeliveryPlan::from(&mode.delivery),
            requested_engine: RequestedEngine::Local,
            local_fallback: None,
            mode_selection_source: ModeSelectionSource::ActiveMode,
            cloud: None,
            command: None,
        })
    }

    /// The mode's rewrite destination, or why it cannot be used. Consent is
    /// checked against the destination the provider resolves to right now, so
    /// an edited base URL invalidates a consent granted for the old one.
    ///
    /// Which provider that is comes from [`ModeLlmSettings::destination`] and
    /// nowhere else, so the run can never reach a provider the mode's own row
    /// does not name.
    fn resolve_rewrite(
        settings: &AppSettings,
        llm: &ModeLlmSettings,
    ) -> Result<ResolvedLlmSettings, RunPlanError> {
        let destination = llm.destination(settings);
        let provider = settings
            .post_process_provider(&destination.provider_id)
            .cloned()
            .ok_or(RunPlanError::MissingPostProcessProvider)?;
        let endpoint = provider
            .endpoint()
            .map_err(|_| RunPlanError::InvalidPostProcessDestination)?;
        if !settings.has_current_post_process_provider_consent(&provider, &endpoint) {
            return Err(RunPlanError::PostProcessConsentRequired);
        }
        Ok(ResolvedLlmSettings {
            provider,
            model_id: destination.model_id,
            endpoint,
        })
    }

    /// Freeze a run under one mode.
    ///
    /// `rewrite_required` says whether the rewrite is the run's output rather
    /// than a pass over it. Only a voice command is: it replaces a selection
    /// with what the provider returns, so an unusable provider leaves nothing
    /// to deliver and the run must refuse before the microphone opens.
    fn for_mode(
        settings: &AppSettings,
        mode: ModeDefinition,
        post_process_override: Option<bool>,
        mode_selection_source: ModeSelectionSource,
        rewrite_required: bool,
    ) -> Result<Self, RunPlanError> {
        let mut post_process_requested = post_process_override.unwrap_or(mode.llm.enabled);
        let llm = if post_process_requested {
            match Self::resolve_rewrite(settings, &mode.llm) {
                Ok(llm) => Some(llm),
                Err(error) if rewrite_required => return Err(error),
                // Cleanup is a pass over words that already exist, so an
                // unusable provider costs the cleanup and never the dictation.
                // Refusing here instead cost the user every word: the caller
                // returns before it opens the microphone
                // (`transcription_coordinator::start`) and reports nothing for
                // a dictation intent (`command_mode::report_refused_run`), so
                // the chord silently did nothing. Delivering the raw
                // transcript is what a rewrite that fails mid-run already
                // does, and it sends nothing to an unconsented destination.
                Err(error) => {
                    log::warn!(
                        "Mode '{}' requested a rewrite that is unavailable ({error}); \
                         delivering the raw transcript",
                        mode.id
                    );
                    post_process_requested = false;
                    None
                }
            }
        } else {
            None
        };
        let asr = AsrPlan::from_mode(settings, &mode.asr);
        let requested_engine = mode.asr.requested_engine;
        let (cloud, local_fallback) = match requested_engine.cloud_provider() {
            None => (None, None),
            Some(provider) => {
                let provider_settings = settings
                    .cloud_stt_provider(provider)
                    .ok_or(RunPlanError::CloudConsentRequired { provider })?;
                if provider_settings.consent_version != crate::settings::CLOUD_STT_CONSENT_VERSION
                    || !provider_settings.audio_transfer_consent
                {
                    return Err(RunPlanError::CloudConsentRequired { provider });
                }
                if !provider_settings.privacy_consent || !provider_settings.local_fallback_consent {
                    return Err(RunPlanError::CloudPrivacyConsentRequired { provider });
                }
                if !mode.asr.cloud_timestamps {
                    return Err(RunPlanError::CloudTimestampsRequired { provider });
                }

                let fallback = if mode.asr.local_fallback_enabled {
                    let mut fallback = asr.clone();
                    if let Some(model_id) = mode
                        .asr
                        .local_fallback_model_id
                        .as_deref()
                        .filter(|model_id| !model_id.trim().is_empty())
                    {
                        fallback.model_id = model_id.to_string();
                    }
                    if fallback.model_id.trim().is_empty() {
                        return Err(RunPlanError::CloudFallbackModelRequired { provider });
                    }
                    Some(fallback)
                } else {
                    None
                };

                let keyterms = mode
                    .asr
                    .cloud_keyterms
                    .iter()
                    .map(|keyterm| keyterm.trim())
                    .filter(|keyterm| !keyterm.is_empty())
                    .map(str::to_owned)
                    .collect();
                (
                    Some(CloudRunPlan::new(
                        provider,
                        mode.asr.language.clone(),
                        keyterms,
                        mode.asr.cloud_timestamps,
                    )),
                    fallback,
                )
            }
        };
        let requested_policy = mode.context_policy;
        let ceiling = settings.context_policy_ceiling;
        let effective_policy = requested_policy.clamp_to(ceiling);
        refresh_clipboard_context_watcher(settings);
        let context = ContextPlan {
            requested_policy,
            ceiling,
            effective_policy,
            pending: Arc::new(context::start_capture(
                requested_policy,
                ceiling,
                CaptureOptions {
                    url_capture_enabled: settings.context_url_capture_enabled,
                    clipboard_preroll_ms: settings.context_capture_clipboard_preroll_ms,
                },
            )),
        };

        Ok(Self {
            run_id: NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed),
            run_started_at_ms: run_now_ms(),
            settings_revision: settings.modes_revision,
            mode_id: mode.id,
            asr,
            prompt: PromptPlan {
                tone: mode.tone,
                preset: mode.prompt.preset,
                custom_prompt: mode.prompt.custom_prompt,
                llm,
                post_process_requested,
                persona_samples: settings.persona_samples.clone(),
                spoken_instructions: post_process_requested && mode.llm.spoken_instructions,
            },
            context,
            delivery: DeliveryPlan::from(&mode.delivery),
            requested_engine,
            local_fallback,
            cloud,
            mode_selection_source,
            command: None,
        })
    }

    /// Freeze a retry under the current active mode and its original
    /// post-processing decision. Retries do not capture Sona's own history
    /// window as target context.
    pub fn for_retry(
        settings: &AppSettings,
        post_process_requested: bool,
    ) -> Result<Self, RunPlanError> {
        let mut mode = active_mode(settings)
            .cloned()
            .ok_or(RunPlanError::NoMatchingMode)?;
        mode.asr.requested_engine = RequestedEngine::Local;
        let mut run = Self::for_mode(
            settings,
            mode,
            Some(post_process_requested),
            ModeSelectionSource::ActiveMode,
            false,
        )?;
        run.context.without_live_capture();
        Ok(run)
    }

    /// Freeze a voice command run: the text selected right now becomes the
    /// operand, and the active mode supplies the audio and rewrite settings.
    ///
    /// The selection is read before the microphone opens, so a chord pressed
    /// with nothing selected costs the user nothing. Command mode always
    /// rewrites, so the mode's own post-processing switch is overridden — the
    /// spoken words are an instruction, and pasting them over the selection is
    /// never what was asked for.
    pub fn for_command(settings: &AppSettings) -> Result<Self, RunPlanError> {
        Self::for_command_with_selection(settings, context::capture_selected_text)
    }

    fn for_command_with_selection(
        settings: &AppSettings,
        capture_selection: impl FnOnce() -> context::SelectionCapture,
    ) -> Result<Self, RunPlanError> {
        let selection = match capture_selection() {
            context::SelectionCapture::Captured(selection) => selection,
            context::SelectionCapture::Unavailable(reason) => {
                log::debug!("Refusing a command run: no usable selection ({reason:?})");
                return Err(RunPlanError::CommandWithoutSelection);
            }
        };
        let mode = active_mode(settings)
            .cloned()
            .ok_or(RunPlanError::NoMatchingMode)?;
        let mut run = Self::for_mode(
            settings,
            mode,
            Some(true),
            ModeSelectionSource::ActiveMode,
            true,
        )?;
        // A rewritten selection is a replacement, not an insertion: a trailing
        // space or a submit key would corrupt the text it replaced.
        run.delivery.append_trailing_space = false;
        run.delivery.auto_submit = false;
        run.command = Some(CommandPlan::new(selection));
        Ok(run)
    }

    /// Freeze a stored recording for reprocessing under an explicitly chosen
    /// mode. Unlike [`Self::for_retry`], which repeats the original run under
    /// whatever mode is active now, this exists to run the same audio through a
    /// *different* mode, so the target mode's own rewrite decision applies
    /// rather than the source entry's. Replay never reaches a cloud engine and
    /// never captures live selection or application context: the recording
    /// already happened, so anything captured now describes the wrong moment.
    pub fn for_reprocess(settings: &AppSettings, mode_id: &str) -> Result<Self, RunPlanError> {
        let mut mode = settings
            .modes
            .iter()
            .find(|mode| mode.id == mode_id)
            .cloned()
            .ok_or(RunPlanError::NoMatchingMode)?;
        mode.asr.requested_engine = RequestedEngine::Local;
        let mut run = Self::for_mode(
            settings,
            mode,
            None,
            ModeSelectionSource::ExplicitModeShortcut,
            false,
        )?;
        run.context.without_live_capture();
        Ok(run)
    }

    /// The mode's local model settings, retained for local runs and frozen
    /// fallback decoding. Call local_asr to learn whether this run is allowed
    /// to invoke the local engine.
    pub fn asr(&self) -> &AsrPlan {
        &self.asr
    }

    #[cfg(feature = "cloud-realtime")]
    pub const fn requested_engine(&self) -> RequestedEngine {
        self.requested_engine
    }

    pub fn cloud(&self) -> Option<&CloudRunPlan> {
        self.cloud.as_ref()
    }

    pub fn local_asr(&self) -> Option<&AsrPlan> {
        match self.requested_engine {
            RequestedEngine::Local => Some(&self.asr),
            RequestedEngine::DeepgramNova3 | RequestedEngine::ElevenLabsScribeV2 => {
                self.local_fallback.as_ref()
            }
        }
    }

    pub fn prompt(&self) -> &PromptPlan {
        &self.prompt
    }

    pub fn post_process_requested(&self) -> bool {
        self.prompt.post_process_requested
    }

    pub fn delivery(&self) -> &DeliveryPlan {
        &self.delivery
    }

    pub fn context(&self) -> &ContextSnapshot {
        self.context.snapshot()
    }

    pub fn context_plan(&self) -> &ContextPlan {
        &self.context
    }

    /// The selection this run rewrites, for a voice command run only. `None` is
    /// what makes every other run a dictation.
    pub fn command(&self) -> Option<&CommandPlan> {
        self.command.as_ref()
    }

    pub fn mode_receipt(&self) -> ModeReceipt {
        self.mode_receipt_with_engine(Some(self.requested_engine), false)
    }

    /// Build the immutable history receipt only after the final cloud/local
    /// choice is known. The requested route remains distinct from the engine
    /// that supplied the delivered text.
    pub fn mode_receipt_with_engine(
        &self,
        engine_used: Option<RequestedEngine>,
        cloud_fallback: bool,
    ) -> ModeReceipt {
        let cloud_status = if cloud_fallback {
            CloudReceiptStatus::Fallback
        } else if self.cloud.is_some() {
            CloudReceiptStatus::Final
        } else {
            CloudReceiptStatus::NotRequested
        };
        self.mode_receipt_with_cloud_status(engine_used, cloud_status)
    }

    pub fn mode_receipt_with_cloud_status(
        &self,
        engine_used: Option<RequestedEngine>,
        cloud_status: CloudReceiptStatus,
    ) -> ModeReceipt {
        ModeReceipt {
            run_id: self.run_id,
            settings_revision: self.settings_revision,
            mode_id: self.mode_id.clone(),
            mode_selection_source: self.mode_selection_source,
            tone: self.prompt.tone,
            requested_context_policy: self.context.requested_policy(),
            context_policy_ceiling: self.context.ceiling(),
            context_policy: self.context.effective_policy(),
            prompt_preset: self.prompt.preset,
            post_process_requested: self.post_process_requested(),
            provider_id: self.prompt.llm.as_ref().map(|llm| llm.provider.id.clone()),
            model_id: self.prompt.llm.as_ref().map(|llm| llm.model_id.clone()),
            engine_requested: self.requested_engine,
            engine_used,
            cloud_fallback: cloud_status == CloudReceiptStatus::Fallback,
            cloud_status,
            local_fallback_model_id: self
                .local_fallback
                .as_ref()
                .map(|fallback| fallback.model_id.clone()),
            // A plan is frozen before the microphone is read, so it cannot know
            // the capture's amplitude or how fast the engine will decode it.
            // The receipt-write seam attaches both.
            input_peak: None,
            input_rms: None,
            realtime_factor: None,
        }
    }
}

impl ModeReceipt {
    /// Attach the amplitude Sona measured for this capture's audio.
    ///
    /// Called at the receipt-write seam, which is the only place that has both
    /// the frozen plan and the measured samples. A receipt that never reaches
    /// this call keeps both fields absent, which is the honest claim for audio
    /// Sona never measured — an imported file, a reprocess of stored text, an
    /// overrun prefix — and is not the same claim as a measured zero.
    #[must_use]
    pub fn with_input_level(mut self, peak: f32, rms: f32) -> Self {
        self.input_peak = Some(peak);
        self.input_rms = Some(rms);
        self
    }

    /// Attach the realtime factor the local batch decode of this capture
    /// achieved.
    ///
    /// Absent means no timed local batch decode produced this receipt's text: a
    /// streamed transcript, a cloud final, a capture that never reached the
    /// engine, or any row written before the field existed. It is never a
    /// stand-in for a decode that was simply fast.
    #[must_use]
    pub fn with_realtime_factor(mut self, factor: Option<f32>) -> Self {
        self.realtime_factor = factor;
        self
    }
}
/// `Eq` is deliberately absent: the measured amplitudes below are floats, and a
/// measurement is compared for equality only in tests, never keyed on.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct ModeReceipt {
    pub run_id: u64,
    pub settings_revision: u64,
    #[serde(default)]
    pub mode_selection_source: ModeSelectionSource,
    pub mode_id: String,
    pub tone: Tone,
    pub requested_context_policy: ContextPolicy,
    pub context_policy_ceiling: ContextPolicy,
    pub context_policy: ContextPolicy,
    pub prompt_preset: PromptPreset,
    pub post_process_requested: bool,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    /// The route selected at capture start. The former requested_engine field
    /// is accepted as a legacy receipt alias from the earlier plan-only schema.
    #[serde(default, alias = "requested_engine")]
    pub engine_requested: RequestedEngine,
    /// None is a held remote recording: no provider final was trusted and no
    /// local fallback was available, so the app must never deliver text.
    #[serde(default)]
    pub engine_used: Option<RequestedEngine>,
    #[serde(default)]
    pub cloud_fallback: bool,
    #[serde(default)]
    pub cloud_status: CloudReceiptStatus,
    #[serde(default)]
    pub local_fallback_model_id: Option<String>,
    /// Peak and RMS amplitude of this capture's audio, normalized to full
    /// scale, exactly as `measure_input_level` reported them. Absent means the
    /// audio was never measured, which is the distinction that separates a dead
    /// input stream from a quiet but real utterance on a no-speech receipt.
    #[serde(default)]
    pub input_peak: Option<f32>,
    #[serde(default)]
    pub input_rms: Option<f32>,
    /// Audio seconds per decode second for the local batch decode that produced
    /// this receipt's text — 13.8 means 1.05 s of audio decoded in 76 ms. This
    /// is the engine's measured throughput on this machine, which is the only
    /// version of it worth showing. Absent means no timed local batch decode
    /// was involved.
    #[serde(default)]
    pub realtime_factor: Option<f32>,
}

fn run_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::get_default_settings;

    #[test]
    fn mode_vocabulary_overrides_use_the_matcher_key() {
        let global = vec![VocabularyEntry {
            spoken: "Open.AI".to_string(),
            written: "global".to_string(),
        }];
        let overrides = vec![VocabularyEntry {
            spoken: "open ai".to_string(),
            written: "mode".to_string(),
        }];

        let effective = effective_vocabulary(&global, &overrides);
        assert_eq!(effective, overrides);
    }
    fn configured_settings() -> AppSettings {
        let mut settings = get_default_settings();
        ensure_mode_settings(&mut settings);
        settings
    }
    #[test]
    fn command_chord_captures_only_its_explicit_operand_when_context_is_disabled() {
        let mut settings = configured_settings();
        global_local_provider(&mut settings);
        settings.context_policy_ceiling = ContextPolicy::None;
        settings.modes[0].context_policy = ContextPolicy::Full;
        let selection_reads = std::cell::Cell::new(0);

        let run = RunPlan::for_command_with_selection(&settings, || {
            selection_reads.set(selection_reads.get() + 1);
            context::SelectionCapture::Captured("selected by the command chord".to_string())
        })
        .expect("the explicit selection is a valid command operand");

        assert_eq!(selection_reads.get(), 1);
        assert_eq!(
            run.command().expect("command plan").selection(),
            "selected by the command chord"
        );
        let snapshot = run.context();
        assert_eq!(snapshot.packet(), &context::ContextPacket::default());
        assert_eq!(
            snapshot.receipt().sources,
            context::ContextSources {
                target: context::ContextSourceStatus::DisabledByCeiling,
                focused_field: context::ContextSourceStatus::DisabledByCeiling,
                selected_text: context::ContextSourceStatus::DisabledByCeiling,
                browser_url: context::ContextSourceStatus::DisabledByCeiling,
                clipboard: context::ContextSourceStatus::DisabledByCeiling,
            }
        );
    }
    #[test]
    fn command_chord_refuses_a_missing_explicit_operand_before_context_capture() {
        let settings = configured_settings();
        let selection_reads = std::cell::Cell::new(0);

        let result = RunPlan::for_command_with_selection(&settings, || {
            selection_reads.set(selection_reads.get() + 1);
            context::SelectionCapture::Unavailable(context::ContextSourceStatus::Empty)
        });

        assert!(matches!(result, Err(RunPlanError::CommandWithoutSelection)));
        assert_eq!(selection_reads.get(), 1);
    }

    #[test]
    fn empty_mode_list_seeds_delivery_from_legacy_roots() {
        let raw = serde_json::json!({
            "modes": [],
            "paste_method": "direct",
            "clipboard_handling": "copy_to_clipboard",
            "auto_submit": true,
            "auto_submit_key": "cmd_enter",
            "typing_tool": "xdotool",
        });
        let mut settings: AppSettings =
            serde_json::from_value(raw).expect("legacy delivery roots deserialize");

        assert!(settings.modes.is_empty());
        assert!(ensure_mode_settings(&mut settings));
        assert_eq!(settings.modes.len(), 4);
        for mode in &settings.modes {
            assert_eq!(mode.delivery.paste_method, PasteMethod::Direct);
            assert_eq!(
                mode.delivery.clipboard_handling,
                ClipboardHandling::CopyToClipboard
            );
            assert!(mode.delivery.auto_submit);
            assert_eq!(mode.delivery.auto_submit_key, AutoSubmitKey::CmdEnter);
            assert_eq!(mode.delivery.typing_tool, TypingTool::Xdotool);
        }
    }

    fn grant_remote_llm_consent(settings: &mut AppSettings, provider_id: &str) {
        let provider = settings
            .post_process_provider(provider_id)
            .expect("configured provider")
            .clone();
        let endpoint = provider.endpoint().expect("provider endpoint");
        assert!(endpoint.is_remote(), "test expects a remote provider");
        settings.post_process_provider_consents.insert(
            provider_id.to_string(),
            crate::settings::PostProcessProviderConsent::for_endpoint(&endpoint),
        );
    }

    /// Point the global at a keyless loopback provider, the way Aktan's store
    /// does. A loopback destination needs no consent, so this is a provider
    /// that genuinely runs.
    fn global_local_provider(settings: &mut AppSettings) {
        settings.post_process_provider_id = "custom".to_string();
        settings
            .post_process_models
            .insert("custom".to_string(), "gemma4:12b-mlx".to_string());
        assert!(
            settings.post_process_provider_consents.is_empty(),
            "the case under test has no destination consent at all"
        );
    }

    /// The live defect, both halves. A mode that baked a copy of the global at
    /// creation names `openai`, which has no credential and no consent, so the
    /// rewrite is dropped and the working local provider is named by nothing.
    /// A mode that never chose one inherits and reaches it.
    #[test]
    fn a_mode_that_never_chose_a_provider_runs_the_global_one() {
        let mut settings = configured_settings();
        global_local_provider(&mut settings);
        settings.modes[0].llm.enabled = true;

        settings.modes[0].llm.provider_id = Some("openai".to_string());
        let baked = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("an unusable rewrite never costs the dictation");
        assert!(!baked.post_process_requested());
        assert!(baked.prompt().llm.is_none());

        settings.modes[0].llm.provider_id = None;
        let inherited = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("active mode resolves");
        let llm = inherited
            .prompt()
            .llm
            .as_ref()
            .expect("the global provider is the mode's destination");
        assert_eq!(llm.provider.id, "custom");
        assert_eq!(llm.model_id, "gemma4:12b-mlx");
        assert!(inherited.post_process_requested());
    }

    /// The case the inherit default must not eat: a provider named in the
    /// mode's own row is a decision. It outranks a different global even when
    /// the global would run and this one needed a consent first, and the
    /// resolution never reads credential or consent state to decide.
    #[test]
    fn a_deliberate_provider_override_outranks_a_different_global() {
        let mut settings = configured_settings();
        global_local_provider(&mut settings);
        settings.modes[0].llm.enabled = true;
        settings.modes[0].llm.provider_id = Some("openai".to_string());
        settings.modes[0].llm.model_id = "gpt-4o-mini".to_string();
        grant_remote_llm_consent(&mut settings, "openai");

        let destination = settings.modes[0].llm.destination(&settings);
        assert_eq!(destination.provider_id, "openai");
        assert_eq!(destination.model_id, "gpt-4o-mini");
        assert!(!destination.inherited);

        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("active mode resolves");
        let llm = run.prompt().llm.as_ref().expect("the override runs");
        assert_eq!(llm.provider.id, "openai");
        assert_eq!(llm.model_id, "gpt-4o-mini");
    }

    /// The override survives the schema change, and a document that never
    /// recorded a provider deserializes as inherit rather than as a name.
    #[test]
    fn a_stored_mode_provider_survives_the_inherit_schema() {
        let settings = configured_settings();
        assert_eq!(settings.modes[0].llm.provider_id, None);

        let mut stored = serde_json::to_value(&settings.modes[0]).expect("mode serializes");
        stored["llm"]["provider_id"] = serde_json::json!("openai");
        let restored: ModeDefinition =
            serde_json::from_value(stored.clone()).expect("stored mode deserializes");
        assert_eq!(restored.llm.provider_id.as_deref(), Some("openai"));

        stored["llm"]
            .as_object_mut()
            .expect("llm object")
            .remove("provider_id");
        let inheriting: ModeDefinition =
            serde_json::from_value(stored).expect("mode without a provider deserializes");
        assert_eq!(inheriting.llm.provider_id, None);
    }

    /// Reprocessing exists to run stored audio through a *different* mode, so
    /// the plan must come from the named mode rather than the active one, and
    /// must carry that mode's own rewrite decision.
    #[test]
    fn for_reprocess_freezes_the_named_mode_not_the_active_one() {
        let mut settings = configured_settings();
        let mut second = settings.modes[0].clone();
        second.id = "mode_reprocess_target".to_string();
        second.name = "Target".to_string();
        second.tone = Tone::Formal;
        second.prompt.preset = PromptPreset::Email;
        settings.modes.push(second);
        let active_id = settings.active_mode_id.clone();
        assert_ne!(active_id, "mode_reprocess_target");

        let run = RunPlan::for_reprocess(&settings, "mode_reprocess_target")
            .expect("named mode resolves");

        assert_eq!(run.mode_id, "mode_reprocess_target");
        assert_eq!(run.prompt().preset, PromptPreset::Email);
        assert_eq!(run.prompt().tone, Tone::Formal);
        // Choosing a mode to reprocess with must not change what the next
        // dictation uses.
        assert_eq!(settings.active_mode_id, active_id);
    }

    #[test]
    fn for_reprocess_rejects_an_unknown_mode() {
        let settings = configured_settings();
        assert!(matches!(
            RunPlan::for_reprocess(&settings, "mode_does_not_exist"),
            Err(RunPlanError::NoMatchingMode)
        ));
    }

    #[test]
    fn an_empty_mode_model_inherits_the_global_selection() {
        let mut settings = configured_settings();
        settings.selected_model = "globally-chosen-model".to_string();
        settings.modes[0].asr.model_id.clear();
        settings.active_mode_id = settings.modes[0].id.clone();

        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("active mode resolves");
        assert_eq!(run.asr().model_id, "globally-chosen-model");

        // A mode that names its own model keeps it.
        settings.modes[0].asr.model_id = "mode-specific".to_string();
        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("active mode resolves");
        assert_eq!(run.asr().model_id, "mode-specific");
    }

    /// The recording already happened. Capturing the selection or frontmost app
    /// now would describe the wrong moment, and a stored WAV must never be
    /// re-uploaded to a cloud engine on the user's behalf.
    #[test]
    fn for_reprocess_replays_locally_without_live_context() {
        let settings = configured_settings();
        let mode_id = settings.active_mode_id.clone();

        let run = RunPlan::for_reprocess(&settings, &mode_id).expect("active mode resolves");

        assert_eq!(run.asr().model_id, settings.modes[0].asr.model_id);
        assert!(run.cloud().is_none());
        // No live selection, clipboard, or application context is read for a
        // replay: the receipt records a capture that was never requested rather
        // than an empty one that was.
        let receipt = run.context().receipt().clone();
        assert_eq!(receipt.policy, ContextPolicy::None);
        let sources = receipt.sources;
        for status in [
            sources.target,
            sources.focused_field,
            sources.selected_text,
            sources.browser_url,
            sources.clipboard,
        ] {
            assert_ne!(
                status,
                crate::context::ContextSourceStatus::Captured,
                "a replay must not claim to have read anything"
            );
        }
        assert_eq!(receipt.application_captured_at_ms, None);
    }

    #[test]
    fn every_run_plan_carries_the_users_writing_samples() {
        let mut settings = configured_settings();
        settings.persona_samples = vec![crate::settings::PersonaSample {
            id: "sample_1".to_string(),
            text: "Short, plain sentences. No throat clearing.".to_string(),
        }];
        let mode_id = settings.active_mode_id.clone();

        let run = RunPlan::for_reprocess(&settings, &mode_id).expect("active mode resolves");

        assert_eq!(run.prompt().persona_samples, settings.persona_samples);
    }

    /// The cue is read from the frozen plan, so a mode that never opted in
    /// cannot have a `Sona, …` sentence taken out of its delivery, and an
    /// existing mode deserializes opted out.
    #[test]
    fn spoken_instructions_are_off_until_the_mode_opts_in() {
        let mut settings = configured_settings();
        let mode_id = settings.active_mode_id.clone();
        assert!(!settings.modes[0].llm.spoken_instructions);
        let persisted = serde_json::to_value(&settings.modes[0]).unwrap();
        let restored: ModeDefinition = serde_json::from_value(persisted).unwrap();
        assert!(!restored.llm.spoken_instructions);

        // The cue rides on the mode's rewrite, so the test gives it one.
        grant_remote_llm_consent(&mut settings, "openai");
        settings.modes[0].llm.enabled = true;
        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("active mode resolves");
        assert!(run.post_process_requested());
        assert!(!run.prompt().spoken_instructions);

        settings.modes[0].llm.spoken_instructions = true;
        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("active mode resolves");
        assert!(run.prompt().spoken_instructions);
        // A replay runs under the chosen mode's own rewrite decision, so it
        // inherits the toggle rather than carrying a second answer.
        assert!(
            RunPlan::for_reprocess(&settings, &mode_id)
                .expect("active mode resolves")
                .prompt()
                .spoken_instructions
        );
        // A mode with the toggle on but AI cleanup off has no rewrite for the
        // instruction to reach, so the plan says so itself.
        settings.modes[0].llm.enabled = false;
        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("active mode resolves");
        assert!(!run.post_process_requested());
        assert!(!run.prompt().spoken_instructions);
        // A file import admits no rewrite path at all, so the cue cannot fire
        // there whatever the mode says.
        settings.modes[0].llm.enabled = true;
        let import = RunPlan::for_media_import(&settings).expect("active mode resolves");
        assert!(!import.prompt().spoken_instructions);
        assert!(!import.post_process_requested());
    }

    #[test]
    fn migration_defaults_keep_legacy_transcribe_binding() {
        let mut settings = get_default_settings();
        let legacy_binding = settings.bindings["transcribe"].clone();
        settings.modes.clear();
        settings.active_mode_id.clear();
        assert!(ensure_mode_settings(&mut settings));
        let message = active_mode(&settings).unwrap();
        assert_eq!(message.id, DEFAULT_MODE_ID);
        assert_eq!(settings.bindings["transcribe"], legacy_binding);
        assert!(settings.bindings.contains_key("mode/email/transcribe"));
        assert!(settings.bindings.contains_key("mode/notes/switch"));
        assert!(serde_json::to_value(message)
            .unwrap()
            .get("shortcuts")
            .is_none());
    }

    #[test]
    fn reconciliation_adds_missing_bindings_without_replacing_a_rebound_chord() {
        let mut settings = configured_settings();
        let id = "mode/email/transcribe";
        settings.bindings.get_mut(id).unwrap().current_binding = "f13".to_string();
        let persisted = serde_json::to_string(&settings).unwrap();
        let mut reloaded: AppSettings = serde_json::from_str(&persisted).unwrap();
        assert!(!ensure_mode_settings(&mut reloaded));
        assert_eq!(reloaded.bindings[id].current_binding, "f13");
    }

    #[test]
    fn creation_and_deletion_manage_only_derived_binding_ids() {
        let mut settings = configured_settings();
        let revision = settings.modes_revision;
        let mut mode = settings.modes[1].clone();
        mode.id = "research".to_string();
        mode.name = "Research".to_string();
        apply_upsert_mode(&mut settings, mode, revision).unwrap();
        assert!(settings.bindings.contains_key("mode/research/transcribe"));
        assert!(settings.bindings.contains_key("mode/research/switch"));
        let rebound = "f14".to_string();
        settings
            .bindings
            .get_mut("mode/research/transcribe")
            .unwrap()
            .current_binding = rebound.clone();
        let revision = settings.modes_revision;
        apply_delete_mode(&mut settings, "research", revision).unwrap();
        assert!(!settings.bindings.contains_key("mode/research/transcribe"));
        assert!(!settings.bindings.contains_key("mode/research/switch"));
        assert_eq!(rebound, "f14");
    }

    #[test]
    fn clipboard_generation_watcher_requires_full_mode_and_ceiling() {
        let mut settings = configured_settings();
        assert!(!should_watch_recent_clipboard(&settings));

        settings.context_policy_ceiling = ContextPolicy::Full;
        assert!(!should_watch_recent_clipboard(&settings));

        settings.modes[0].context_policy = ContextPolicy::Full;
        assert!(should_watch_recent_clipboard(&settings));
    }

    #[test]
    fn run_plan_freezes_the_mutation_matrix() {
        let mut settings = configured_settings();
        settings.modes[0].llm.enabled = true;
        settings.modes[0].llm.provider_id = Some("openai".to_string());
        settings.modes[0].llm.model_id = "gpt-4o-mini".to_string();
        grant_remote_llm_consent(&mut settings, "openai");
        let first = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode).unwrap();

        let mode = &mut settings.modes[0];
        mode.asr.model_id = "after-model".to_string();
        mode.asr.language = "fr".to_string();
        mode.asr.custom_words = vec![VocabularyEntry {
            spoken: "after word".to_string(),
            written: "AfterWord".to_string(),
        }];
        mode.asr.filler_word_removal_enabled = false;
        mode.llm.provider_id = Some("custom".to_string());
        mode.llm.model_id = "after-provider-model".to_string();
        mode.prompt.custom_prompt = Some("after-prompt".to_string());
        mode.delivery.append_trailing_space = true;
        mode.asr.literal_punctuation = true;
        settings.english_spelling = EnglishSpelling::British;
        settings.context_policy_ceiling = ContextPolicy::Full;
        settings.modes_revision += 1;
        let second = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode).unwrap();

        assert_ne!(first.run_id, second.run_id);
        assert_ne!(first.asr().model_id, second.asr().model_id);
        assert_eq!(first.asr().language, "auto");
        assert_ne!(first.asr().custom_words, second.asr().custom_words);
        assert!(first.asr().filler_word_removal_enabled);
        assert!(!first.asr().literal_punctuation);
        assert!(second.asr().literal_punctuation);
        assert_eq!(first.asr().english_spelling, EnglishSpelling::AsSpoken);
        assert_eq!(second.asr().english_spelling, EnglishSpelling::British);
        assert_ne!(
            first.prompt().llm.as_ref().unwrap().provider.id,
            second.prompt().llm.as_ref().unwrap().provider.id
        );
        assert_ne!(first.prompt().custom_prompt, second.prompt().custom_prompt);
        assert!(!first.delivery().append_trailing_space);
        assert!(second.delivery().append_trailing_space);
        assert_eq!(first.context_plan().ceiling(), ContextPolicy::None);
        assert_eq!(second.context_plan().ceiling(), ContextPolicy::Full);
        let receipt = first.mode_receipt();
        assert_eq!(receipt.engine_requested, RequestedEngine::Local);
        assert_eq!(receipt.local_fallback_model_id, None);
    }

    #[test]
    fn manual_mode_app_rule_website_rule_and_active_mode_have_stable_precedence() {
        let mut settings = configured_settings();
        grant_remote_llm_consent(&mut settings, "openai");
        settings.active_mode_id = DEFAULT_MODE_ID.to_string();
        settings.context_policy_ceiling = ContextPolicy::Target;
        settings.context_url_capture_enabled = true;
        settings.mode_activation_rules = vec![ModeActivationRule {
            app_id: "com.example.browser".to_string(),
            mode_id: "email".to_string(),
        }];
        settings.mode_website_activation_rules = vec![
            ModeWebsiteActivationRule {
                host: "example.com".to_string(),
                match_kind: WebsiteHostMatch::Suffix,
                mode_id: "notes".to_string(),
            },
            ModeWebsiteActivationRule {
                host: "docs.example.com".to_string(),
                match_kind: WebsiteHostMatch::Exact,
                mode_id: "meeting".to_string(),
            },
        ];

        let manual = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::Mode {
                mode_id: "notes".to_string(),
            },
            Some("com.example.browser"),
            Some("docs.example.com"),
        )
        .expect("explicit mode selects its mode");
        assert_eq!(manual.mode_id, "notes");
        assert_eq!(
            manual.mode_receipt().mode_selection_source,
            ModeSelectionSource::ExplicitModeShortcut
        );

        let app_selected = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::ActiveMode,
            Some("com.example.browser"),
            Some("docs.example.com"),
        )
        .expect("bundle-ID app rule is authoritative");
        assert_eq!(app_selected.mode_id, "email");
        assert_eq!(
            app_selected.mode_receipt().mode_selection_source,
            ModeSelectionSource::AppActivationRule
        );

        let exact_website = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::ActiveMode,
            Some("com.example.other-browser"),
            Some("DOCS.EXAMPLE.COM."),
        )
        .expect("exact website rule selects a mode");
        assert_eq!(exact_website.mode_id, "meeting");
        assert_eq!(
            exact_website.mode_receipt().mode_selection_source,
            ModeSelectionSource::WebsiteActivationRule
        );

        let suffix_website = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::ActiveMode,
            Some("com.example.other-browser"),
            Some("api.example.com"),
        )
        .expect("suffix website rule selects a mode");
        assert_eq!(suffix_website.mode_id, "notes");
        assert_eq!(
            suffix_website.mode_receipt().mode_selection_source,
            ModeSelectionSource::WebsiteActivationRule
        );

        let fallback = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::ActiveMode,
            Some("com.example.other-browser"),
            Some("unmatched.example"),
        )
        .expect("unmatched automation falls back to the active mode");
        assert_eq!(fallback.mode_id, DEFAULT_MODE_ID);
        assert_eq!(
            fallback.mode_receipt().mode_selection_source,
            ModeSelectionSource::ActiveMode
        );
    }

    #[test]
    fn website_automation_requires_url_consent_and_ignores_secure_fields() {
        let mut settings = configured_settings();
        grant_remote_llm_consent(&mut settings, "openai");
        settings.context_policy_ceiling = ContextPolicy::Target;
        settings.mode_website_activation_rules = vec![ModeWebsiteActivationRule {
            host: "example.com".to_string(),
            match_kind: WebsiteHostMatch::Suffix,
            mode_id: "email".to_string(),
        }];

        let consent_off = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::ActiveMode,
            Some("com.example.browser"),
            Some("mail.example.com"),
        )
        .expect("consent-off run");
        assert_eq!(consent_off.mode_id, DEFAULT_MODE_ID);
        assert_eq!(
            consent_off.mode_receipt().mode_selection_source,
            ModeSelectionSource::ActiveMode
        );

        settings.context_url_capture_enabled = true;
        let secure_field = context::WebsiteHostCapture::SecureField;
        let secure_fallback = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::ActiveMode,
            Some("com.example.browser"),
            secure_field.host(),
        )
        .expect("secure field run");
        assert_eq!(secure_fallback.mode_id, DEFAULT_MODE_ID);
        assert_eq!(
            secure_fallback.mode_receipt().mode_selection_source,
            ModeSelectionSource::ActiveMode
        );
    }

    #[test]
    fn malformed_website_rules_are_pruned_and_valid_hosts_are_normalized() {
        let mut settings = configured_settings();
        settings.mode_website_activation_rules = vec![
            ModeWebsiteActivationRule {
                host: " Example.COM. ".to_string(),
                match_kind: WebsiteHostMatch::Exact,
                mode_id: "email".to_string(),
            },
            ModeWebsiteActivationRule {
                host: "https://example.com".to_string(),
                match_kind: WebsiteHostMatch::Suffix,
                mode_id: "notes".to_string(),
            },
            ModeWebsiteActivationRule {
                host: "example.com".to_string(),
                match_kind: WebsiteHostMatch::Exact,
                mode_id: "notes".to_string(),
            },
            ModeWebsiteActivationRule {
                host: "docs.example.com".to_string(),
                match_kind: WebsiteHostMatch::Suffix,
                mode_id: "missing".to_string(),
            },
        ];

        assert!(ensure_mode_settings(&mut settings));
        assert_eq!(
            settings.mode_website_activation_rules,
            vec![ModeWebsiteActivationRule {
                host: "example.com".to_string(),
                match_kind: WebsiteHostMatch::Exact,
                mode_id: "email".to_string(),
            }]
        );
    }

    #[test]
    fn deleting_a_mode_prunes_application_and_website_activation_rules() {
        let mut settings = configured_settings();
        settings.mode_activation_rules = vec![ModeActivationRule {
            app_id: "com.example.mail".to_string(),
            mode_id: "email".to_string(),
        }];
        settings.mode_website_activation_rules = vec![ModeWebsiteActivationRule {
            host: "mail.example.com".to_string(),
            match_kind: WebsiteHostMatch::Exact,
            mode_id: "email".to_string(),
        }];
        let revision = settings.modes_revision;

        apply_delete_mode(&mut settings, "email", revision).expect("delete email mode");

        assert!(settings.mode_activation_rules.is_empty());
        assert!(settings.mode_website_activation_rules.is_empty());
    }

    #[test]
    fn app_rule_changes_apply_only_to_a_later_run() {
        let mut settings = configured_settings();
        grant_remote_llm_consent(&mut settings, "openai");
        settings.mode_activation_rules = vec![ModeActivationRule {
            app_id: "com.example.mail".to_string(),
            mode_id: "email".to_string(),
        }];
        let first = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::ActiveMode,
            Some("com.example.mail"),
            None,
        )
        .expect("first app-selected run");

        settings.mode_activation_rules[0].mode_id = "notes".to_string();
        let second = RunPlan::for_intent_with_automation_target(
            &settings,
            &TranscriptionIntent::ActiveMode,
            Some("com.example.mail"),
            None,
        )
        .expect("second app-selected run");

        assert_eq!(first.mode_id, "email");
        assert_eq!(second.mode_id, "notes");
    }

    #[test]
    fn legacy_post_process_intent_forces_llm_without_a_binding_id() {
        let mut settings = configured_settings();
        settings.modes[0].llm.enabled = false;
        grant_remote_llm_consent(&mut settings, "openai");
        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveModeWithPostProcess)
            .unwrap();
        assert!(run.post_process_requested());
        assert!(run.prompt().llm.is_some());
        assert!(TranscriptionIntent::from_binding(LEGACY_POST_PROCESS_BINDING_ID).is_none());
    }

    /// A command run has no output but the rewrite, so an unconsented
    /// destination still refuses it before capture. Consent is measured
    /// against the destination the provider resolves to now, so editing the
    /// base URL invalidates it.
    #[test]
    fn remote_llm_requires_current_destination_consent_without_local_fallback() {
        let mut settings = configured_settings();
        let mode = &mut settings.modes[0];
        mode.llm.enabled = true;
        mode.llm.provider_id = Some("openai".to_string());
        mode.llm.model_id = "gpt-4o-mini".to_string();
        let unconsented_command = |settings: &AppSettings| {
            RunPlan::for_mode(
                settings,
                settings.modes[0].clone(),
                Some(true),
                ModeSelectionSource::ActiveMode,
                true,
            )
        };

        assert!(matches!(
            unconsented_command(&settings),
            Err(RunPlanError::PostProcessConsentRequired)
        ));

        grant_remote_llm_consent(&mut settings, "openai");
        let plan = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("acknowledged remote destination");
        assert_eq!(
            plan.prompt()
                .llm
                .as_ref()
                .expect("LLM plan")
                .endpoint
                .base_url(),
            "https://api.openai.com/v1"
        );

        settings
            .post_process_provider_mut("openai")
            .expect("OpenAI provider")
            .base_url = "https://api.example.test/v1".to_string();
        assert!(matches!(
            unconsented_command(&settings),
            Err(RunPlanError::PostProcessConsentRequired)
        ));
    }

    /// The shape three of Aktan's four modes ship with: `llm.enabled` true,
    /// the default remote provider, no model and no consent. Refusing the plan
    /// made the chord do nothing at all — `transcription_coordinator::start`
    /// returns before it opens the microphone, and `report_refused_run` emits
    /// nothing for a dictation intent — so a mode configured for a rewrite it
    /// cannot perform has to dictate anyway.
    #[test]
    fn a_mode_whose_rewrite_is_unavailable_still_dictates() {
        let mut settings = configured_settings();
        let mode = &mut settings.modes[0];
        mode.llm.enabled = true;
        mode.llm.provider_id = Some("openai".to_string());
        mode.llm.model_id.clear();
        mode.asr.model_id = "mode-chosen-model".to_string();

        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("an unusable rewrite provider never costs the dictation");

        assert!(!run.post_process_requested());
        assert!(run.prompt().llm.is_none());
        assert!(!run.prompt().spoken_instructions);
        assert_eq!(run.asr().model_id, "mode-chosen-model");

        settings.modes[0].llm.provider_id = Some("does_not_exist".to_string());
        let unknown_provider = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("an unknown provider never costs the dictation either");
        assert!(!unknown_provider.post_process_requested());
    }

    #[test]
    fn dynamic_binding_ids_resolve_their_own_mode() {
        assert_eq!(
            parse_mode_shortcut_id("mode/email/transcribe"),
            Some(("email".to_string(), ModeShortcutKind::Transcribe))
        );
        assert_eq!(
            parse_mode_shortcut_id("mode/notes/switch"),
            Some(("notes".to_string(), ModeShortcutKind::Switch))
        );
        assert_eq!(
            TranscriptionIntent::from_binding("transcribe"),
            Some(TranscriptionIntent::ActiveMode)
        );
        assert_eq!(parse_mode_shortcut_id("mode/email/unknown"), None);
    }

    #[test]
    fn modes_changed_event_is_a_full_snapshot() {
        let settings = configured_settings();
        let payload =
            serde_json::to_value(ModesChangedEvent(mode_settings_snapshot(&settings))).unwrap();

        assert_eq!(ModesChangedEvent::NAME, "modes-changed-event");
        assert!(payload["modes"].is_array());
        assert_eq!(payload["active_mode_id"], settings.active_mode_id);
        assert_eq!(payload["revision"], settings.modes_revision);
    }

    #[test]
    fn stale_upsert_and_delete_leave_settings_unchanged() {
        let mut settings = configured_settings();
        let before = settings.clone();
        let mode = settings.modes[1].clone();
        assert_eq!(
            apply_upsert_mode(&mut settings, mode, before.modes_revision + 1),
            Err(ModeMutationError::StaleRevision {
                expected_revision: before.modes_revision + 1,
                actual_revision: before.modes_revision,
            })
        );
        assert_eq!(settings.modes, before.modes);
        assert_eq!(
            apply_delete_mode(&mut settings, "email", before.modes_revision + 1),
            Err(ModeMutationError::StaleRevision {
                expected_revision: before.modes_revision + 1,
                actual_revision: before.modes_revision,
            })
        );
        assert_eq!(settings.modes, before.modes);
    }

    #[test]
    fn concurrent_structural_mutations_have_one_revision_winner() {
        let initial = configured_settings();
        let expected_revision = initial.modes_revision;
        let email = initial.modes[1].clone();
        let notes = initial.modes[2].clone();
        let settings = Arc::new(std::sync::Mutex::new(initial));
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let first_settings = Arc::clone(&settings);
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            let mut mode = email;
            mode.name = "Email from first writer".to_string();
            first_barrier.wait();
            let mut settings = first_settings.lock().unwrap();
            apply_upsert_mode(&mut settings, mode, expected_revision)
        });
        let second_settings = Arc::clone(&settings);
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            let mut mode = notes;
            mode.name = "Notes from second writer".to_string();
            second_barrier.wait();
            let mut settings = second_settings.lock().unwrap();
            apply_upsert_mode(&mut settings, mode, expected_revision)
        });

        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Err(ModeMutationError::StaleRevision { .. })))
                .count(),
            1
        );
        assert_eq!(
            settings.lock().unwrap().modes_revision,
            expected_revision + 1
        );
    }

    #[test]
    fn active_mode_change_keeps_the_structural_revision() {
        let mut settings = configured_settings();
        let revision = settings.modes_revision;

        apply_set_active_mode(&mut settings, "email").unwrap();

        assert_eq!(settings.active_mode_id, "email");
        assert_eq!(settings.modes_revision, revision);
        let mode = settings.modes[1].clone();
        assert!(apply_upsert_mode(&mut settings, mode, revision).is_ok());
    }

    #[test]
    fn reorder_requires_an_exact_current_permutation() {
        let mut settings = configured_settings();
        let revision = settings.modes_revision;
        let expected = vec![
            "notes".to_string(),
            "message".to_string(),
            "email".to_string(),
            "meeting".to_string(),
        ];
        apply_reorder_modes(&mut settings, &expected, revision).unwrap();
        assert_eq!(
            settings
                .modes
                .iter()
                .map(|mode| mode.id.as_str())
                .collect::<Vec<_>>(),
            ["notes", "message", "email", "meeting"]
        );
        assert_eq!(settings.modes_revision, revision + 1);

        let revision = settings.modes_revision;
        assert_eq!(
            apply_reorder_modes(
                &mut settings,
                &["notes".to_string(), "notes".to_string()],
                revision,
            ),
            Err(ModeMutationError::DuplicateModeId {
                mode_id: "notes".to_string(),
            })
        );
        assert_eq!(
            apply_reorder_modes(
                &mut settings,
                &[
                    "notes".to_string(),
                    "message".to_string(),
                    "email".to_string(),
                    "unknown".to_string(),
                ],
                revision,
            ),
            Err(ModeMutationError::UnknownMode {
                mode_id: "unknown".to_string(),
            })
        );
        assert_eq!(
            apply_reorder_modes(&mut settings, &expected, revision - 1),
            Err(ModeMutationError::StaleRevision {
                expected_revision: revision - 1,
                actual_revision: revision,
            })
        );
    }

    #[test]
    fn mode_receipt_omits_custom_prompt_and_credentials() {
        let mut settings = configured_settings();
        settings.modes[0].llm.enabled = true;
        settings.modes[0].llm.model_id = "model-a".to_string();
        settings.modes[0].prompt.custom_prompt = Some("secret instruction".to_string());
        grant_remote_llm_consent(&mut settings, "openai");
        let receipt = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .unwrap()
            .mode_receipt();
        let serialized = serde_json::to_string(&receipt).unwrap();
        assert!(!serialized.contains("secret instruction"));
        assert!(!serialized.contains("api_key"));
    }
    #[cfg(feature = "cloud-realtime")]
    fn configured_cloud_settings() -> AppSettings {
        let mut settings = configured_settings();
        let mode = &mut settings.modes[0];
        mode.asr.requested_engine = RequestedEngine::DeepgramNova3;
        mode.asr.local_fallback_model_id = Some("frozen-fallback".to_string());
        mode.asr.cloud_keyterms = vec!["Sona".to_string(), "M4 Pro".to_string()];
        let provider = settings
            .cloud_stt_provider_mut(CloudSttProvider::DeepgramNova3)
            .expect("default Deepgram provider");
        provider.consent_version = crate::settings::CLOUD_STT_CONSENT_VERSION;
        provider.audio_transfer_consent = true;
        provider.privacy_consent = true;
        provider.local_fallback_consent = true;
        settings
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn retry_forces_a_local_route_and_truthful_receipt() {
        let settings = configured_cloud_settings();
        let run = RunPlan::for_retry(&settings, false).unwrap();
        let receipt = run.mode_receipt();

        assert_eq!(run.requested_engine(), RequestedEngine::Local);
        assert!(run.cloud().is_none());
        assert!(run.local_fallback.is_none());
        assert_eq!(receipt.engine_requested, RequestedEngine::Local);
        assert_eq!(receipt.engine_used, Some(RequestedEngine::Local));
        assert_eq!(receipt.cloud_status, CloudReceiptStatus::NotRequested);
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn cloud_mode_requires_versioned_provider_consent() {
        let mut settings = configured_settings();
        settings.modes[0].asr.requested_engine = RequestedEngine::DeepgramNova3;
        settings.modes[0].asr.local_fallback_model_id = Some("fallback".to_string());
        assert!(matches!(
            RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode),
            Err(RunPlanError::CloudConsentRequired {
                provider: CloudSttProvider::DeepgramNova3,
            })
        ));
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn cloud_plan_freezes_provider_data_without_credentials() {
        let mut settings = configured_cloud_settings();
        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode).unwrap();
        let cloud = run.cloud().expect("cloud plan");
        assert_eq!(cloud.provider(), CloudSttProvider::DeepgramNova3);
        assert_eq!(cloud.keyterms(), ["Sona", "M4 Pro"]);
        assert!(cloud.timestamps());
        assert_eq!(run.requested_engine(), RequestedEngine::DeepgramNova3);
        assert_eq!(run.local_asr().unwrap().model_id, "frozen-fallback");

        settings.modes[0].asr.language = "fr".to_string();
        settings.modes[0].asr.requested_engine = RequestedEngine::ElevenLabsScribeV2;
        settings.modes[0].asr.cloud_keyterms = vec!["later".to_string()];
        settings.modes[0].asr.cloud_timestamps = false;
        settings.modes[0].asr.local_fallback_enabled = false;
        settings.modes[0].asr.local_fallback_model_id = Some("later-model".to_string());
        let provider = settings
            .cloud_stt_provider_mut(CloudSttProvider::DeepgramNova3)
            .expect("default Deepgram provider");
        provider.audio_transfer_consent = false;

        assert_eq!(cloud.language(), None);
        assert_eq!(cloud.keyterms(), ["Sona", "M4 Pro"]);
        assert!(cloud.timestamps());
        assert_eq!(run.requested_engine(), RequestedEngine::DeepgramNova3);
        assert_eq!(run.local_asr().unwrap().model_id, "frozen-fallback");
        assert!(!format!("{run:?}").contains("api_key"));
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn cloud_plan_rejects_missing_privacy_timestamps_and_fallback_model() {
        let mut settings = configured_cloud_settings();
        let provider = settings
            .cloud_stt_provider_mut(CloudSttProvider::DeepgramNova3)
            .expect("default Deepgram provider");
        provider.privacy_consent = false;
        assert!(matches!(
            RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode),
            Err(RunPlanError::CloudPrivacyConsentRequired {
                provider: CloudSttProvider::DeepgramNova3,
            })
        ));

        let mut settings = configured_cloud_settings();
        settings.modes[0].asr.cloud_timestamps = false;
        assert!(matches!(
            RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode),
            Err(RunPlanError::CloudTimestampsRequired {
                provider: CloudSttProvider::DeepgramNova3,
            })
        ));

        let mut settings = configured_cloud_settings();
        settings.modes[0].asr.model_id.clear();
        settings.modes[0].asr.local_fallback_model_id = None;
        assert!(matches!(
            RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode),
            Err(RunPlanError::CloudFallbackModelRequired {
                provider: CloudSttProvider::DeepgramNova3,
            })
        ));
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn cloud_receipt_records_requested_and_used_engines_independently() {
        let settings = configured_cloud_settings();
        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode).unwrap();

        let fallback = run.mode_receipt_with_cloud_status(
            Some(RequestedEngine::Local),
            CloudReceiptStatus::Fallback,
        );
        assert_eq!(fallback.engine_requested, RequestedEngine::DeepgramNova3);
        assert_eq!(fallback.engine_used, Some(RequestedEngine::Local));
        assert!(fallback.cloud_fallback);
        assert_eq!(fallback.cloud_status, CloudReceiptStatus::Fallback);
        assert_eq!(
            fallback.local_fallback_model_id.as_deref(),
            Some("frozen-fallback")
        );

        let held =
            run.mode_receipt_with_cloud_status(None, CloudReceiptStatus::HeldCloudUnavailable);
        assert_eq!(held.engine_requested, RequestedEngine::DeepgramNova3);
        assert_eq!(held.engine_used, None);
        assert!(!held.cloud_fallback);
        assert_eq!(held.cloud_status, CloudReceiptStatus::HeldCloudUnavailable);
    }

    /// The generated bindings and the settings UI name Deepgram
    /// `deepgram_nova_3`, while serde's own `snake_case` writes
    /// `deepgram_nova3`. Both enums have to answer to the UI spelling and
    /// keep reading whatever was persisted under the older one, and the two
    /// durable identifiers have to stay put: `CloudSttProvider::id` names the
    /// stored secret account and `RequestedEngine::as_str` keys receipt rows.
    #[test]
    fn deepgram_wire_values_match_the_ui_and_still_read_the_legacy_spelling() {
        assert_eq!(
            serde_json::to_value(CloudSttProvider::DeepgramNova3).expect("provider serializes"),
            serde_json::json!("deepgram_nova_3")
        );
        assert_eq!(
            serde_json::to_value(RequestedEngine::DeepgramNova3).expect("engine serializes"),
            serde_json::json!("deepgram_nova_3")
        );

        for spelling in ["deepgram_nova_3", "deepgram_nova3"] {
            assert_eq!(
                serde_json::from_value::<CloudSttProvider>(serde_json::json!(spelling))
                    .unwrap_or_else(|error| panic!("provider reads {spelling}: {error}")),
                CloudSttProvider::DeepgramNova3
            );
            assert_eq!(
                serde_json::from_value::<RequestedEngine>(serde_json::json!(spelling))
                    .unwrap_or_else(|error| panic!("engine reads {spelling}: {error}")),
                RequestedEngine::DeepgramNova3
            );
        }

        assert_eq!(CloudSttProvider::DeepgramNova3.id(), "deepgram_nova3");
        assert_eq!(RequestedEngine::DeepgramNova3.as_str(), "deepgram_nova3");
    }
}
