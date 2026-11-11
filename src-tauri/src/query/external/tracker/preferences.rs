//! `sona work` verbs for Sona's own settings: read them, change one, modes,
//! dictation prompts, vocabulary, shortcuts, and the background workflows.
//!
//! Settings live in one file the running app holds in memory. A write here
//! saves the file and leaves the settings stamp, and the app reads the file
//! again and follows the new values as it does a change made in its window.
//! Consent, privacy, secrets and the agent gates themselves are not here.

use super::{now_ms, refusal, Tracker, WorkAnswer, WriteArgs};
use crate::commands::vocabulary::{self, VocabularyScope};
use crate::integrations::types::{Connection, ConnectionPreferences, SendScope, SendState};
use crate::meeting::analytics::{KeywordTracker, MeetingNotesTemplate};
use crate::meeting::evening::{FilingTarget, UnfiledRecording};
use crate::meeting::prep::PrepPreferences;
use crate::meeting::prompt_types::{
    normalized_prompt, PromptOutput, PromptTarget, SavedPromptDeleteRequest,
    SavedPromptSaveRequest,
};
use crate::meeting::series_types::{
    MeetingSeriesDigestSetRequest, MeetingSeriesPreferences, MeetingSeriesTemplateSetRequest,
};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::{
    MeetingCommandError, MeetingOperationId, MeetingTemplateId, SavedPromptId,
};
use crate::meeting::workflow_types::{
    WorkflowId, WorkflowOutcomeCode, WorkflowOutcomeCounts, WorkflowRunReceipt, WorkflowRunStatus,
    WorkflowRunsRequest, WorkflowSetEnabledRequest, WorkflowsListResult,
};
use crate::modes::{self, ModeDefinition, RequestedEngine};
use crate::planning;
use crate::query::external::{
    beacon, command_error, kept_local, meeting_refusal, session_id, store, visible_meeting,
    ExternalError,
    ExternalErrorCode, ExternalScope, READ_SCOPES, WRITE_SCOPES,
};
use crate::query::{KeptLocal, QueryError, QUERY_SCHEMA_VERSION};
use crate::settings::{
    self, AppSettings, DigestDay, LLMPrompt, LogLevel, OverlayStyle, PasteMethod, SoundTheme,
    Theme, VocabularyEntry, LOCAL_CLEANUP_PROVIDER_ID,
};
use clap::{Args, Subcommand, ValueEnum};
use rusqlite::OptionalExtension as _;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::path::PathBuf;
use uuid::Uuid;

const LIVE: &str = "A running Sona reads the change within a second and follows it.";

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum SettingsVerb {
    /// Sona's settings an agent may read: dictation, sounds, appearance,
    /// cleanup, modes, prompts, vocabulary, shortcuts, trackers, prep.
    Settings,
    /// Change one setting, as its switch or picker in Settings does.
    Setting(SettingArgs),
    /// Put a shortcut back to its default key, or the text replacements back
    /// to the starter library.
    SettingReset(SettingResetArgs),
    /// Make or change a mode from its full definition, as JSON.
    ModeSave(ModeSaveArgs),
    /// Delete a mode. The default mode stays.
    ModeDelete(ModeDeleteArgs),
    /// Put the modes in this order.
    ModeReorder(ModeReorderArgs),
    /// Add a dictation prompt, or change one with --id.
    PromptSave(PromptSaveArgs),
    /// Delete a dictation prompt. The last one stays.
    PromptDelete(PromptDeleteArgs),
    /// Add a vocabulary word: what is heard, and how to write it.
    VocabularyAdd(VocabularyAddArgs),
    /// Apply a vocabulary CSV (spoken,written per row), as Import does.
    VocabularyCsv(VocabularyCsvArgs),
    /// The background workflows, on or off, with their last run; --runs
    /// lists recent runs.
    Workflows(WorkflowsArgs),
    /// Turn a background workflow on, or off with --off.
    WorkflowEnable(WorkflowEnableArgs),
    /// The saved prompts: questions kept for a meeting, a person or a series.
    SavedPrompts,
    /// Save a saved prompt, or change one with --id.
    SavedPromptSave(SavedPromptSaveArgs),
    /// Delete a saved prompt.
    SavedPromptDelete(SavedPromptDeleteArgs),
    /// A meeting's series: its notes template and whether the evening digest
    /// covers it.
    Series(SeriesArgs),
    /// Choose the notes template for a meeting's series, or clear it.
    SeriesTemplate(SeriesTemplateArgs),
    /// Keep a meeting's series in the evening digest, or take it out with
    /// --off.
    SeriesDigest(SeriesDigestArgs),
    /// Today's evening card: its lines, and recordings waiting to be filed
    /// with where each may belong.
    Evening,
    /// File a recording where one evening suggestion says, or turn the
    /// suggestion down with --decline.
    Filing(FilingArgs),
    /// The connections, their automatic-send rules, and recent sends. Never
    /// a token.
    Connections,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingKey {
    /// true: hold the shortcut to talk; false: press to start and stop.
    PushToTalk,
    /// Sounds when dictation starts and stops: true or false.
    AudioFeedback,
    /// Their volume, 0 to 1.
    AudioFeedbackVolume,
    /// marimba, pop or custom.
    SoundTheme,
    /// Mute other audio while dictating: true or false.
    MuteWhileRecording,
    /// Pick up quiet speech: true or false.
    QuietSpeech,
    /// The command mode shortcut: true or false.
    CommandMode,
    /// The dictation overlay: none, minimal or live.
    Overlay,
    /// Dictation languages, comma-separated codes, primary first; "" for
    /// automatic.
    DictationLanguages,
    /// The app's own language, a code such as en.
    AppLanguage,
    /// system, light or dark.
    Theme,
    /// trace, debug, info, warn or error.
    LogLevel,
    /// Debug mode: true or false.
    DebugMode,
    /// Turn cleanup of dictated text on or off: true or false.
    Cleanup,
    /// The cleanup provider's id, from `sona work settings`.
    CleanupProvider,
    /// The cleanup model for --provider, or the selected provider.
    CleanupModel,
    /// The custom provider's base URL.
    CleanupBaseUrl,
    /// The evening digest: true or false.
    EveningDigest,
    /// Close a task as done once its sub-tasks are all closed and at least
    /// one is done: true or false.
    AutoCloseParents,
    /// The weekly project digest in the inbox: true or false.
    ProjectDigest,
    /// The day it arrives: monday to sunday.
    ProjectDigestDay,
    /// Snippet expansion: true or false.
    Snippets,
    /// Text replacements: true or false.
    TextReplacements,
    /// The active mode's id: the capture mode chip.
    Mode,
    /// Use a dictation prompt by id: the active mode takes its text.
    DictationPrompt,
    /// Keyword trackers, the whole list as JSON: [{"name":…,"patterns":[…]}].
    KeywordTrackers,
    /// Meeting prep preferences as JSON, from `sona work settings`. The mail
    /// and web switches stay the user's.
    MeetingPrep,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct Change {
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SettingArgs {
    #[arg(value_enum, value_name = "KEY")]
    pub key: SettingKey,
    #[arg(value_name = "VALUE", allow_hyphen_values = true)]
    pub value: String,
    /// The cleanup provider a model or base URL is for. Default: the
    /// selected one.
    #[arg(long)]
    pub provider: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResetTarget {
    /// One shortcut, by its id from `sona work settings`.
    Shortcut,
    /// The text replacements, back to the starter library.
    Replacements,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SettingResetArgs {
    #[arg(value_enum)]
    pub target: ResetTarget,
    /// The shortcut's id.
    #[arg(value_name = "ID")]
    pub id: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ModeSaveArgs {
    /// The whole mode as JSON, as `sona work settings --json` prints one
    /// under modes.modes (without shortcuts). A new id makes a new mode.
    #[arg(long, value_name = "JSON")]
    pub mode_json: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ModeDeleteArgs {
    #[arg(value_name = "MODE_ID")]
    pub mode: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ModeReorderArgs {
    /// Every mode's id, in the new order.
    #[arg(required = true, num_args = 1.., value_name = "MODE_ID")]
    pub modes: Vec<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PromptSaveArgs {
    #[arg(value_name = "NAME")]
    pub name: String,
    /// The prompt's text.
    #[arg(long)]
    pub prompt: String,
    /// The prompt to change. Left out: a new prompt.
    #[arg(long)]
    pub id: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PromptDeleteArgs {
    #[arg(value_name = "PROMPT_ID")]
    pub id: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct VocabularyAddArgs {
    /// What dictation hears.
    #[arg(value_name = "SPOKEN")]
    pub spoken: String,
    /// How to write it.
    #[arg(value_name = "WRITTEN")]
    pub written: String,
    /// A mode's own vocabulary. Default: every mode's.
    #[arg(long, value_name = "MODE_ID")]
    pub mode: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct VocabularyCsvArgs {
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// A mode's own vocabulary. Default: every mode's.
    #[arg(long, value_name = "MODE_ID")]
    pub mode: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct WorkflowsArgs {
    /// List recent runs instead.
    #[arg(long)]
    pub runs: bool,
    /// Only this workflow's runs, by id.
    #[arg(long)]
    pub workflow: Option<String>,
    /// How many runs.
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct WorkflowEnableArgs {
    #[arg(value_name = "WORKFLOW_ID")]
    pub workflow: String,
    /// Turn it off.
    #[arg(long)]
    pub off: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum PromptAbout {
    Meeting,
    Person,
    Series,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SavedPromptSaveArgs {
    pub name: String,
    /// What to ask.
    #[arg(long)]
    pub body: String,
    /// What the prompt is asked about.
    #[arg(long, value_enum)]
    pub about: PromptAbout,
    /// Answer as one JSON object checked against this JSON schema, instead
    /// of prose.
    #[arg(long)]
    pub schema: Option<String>,
    /// The saved prompt to change.
    #[arg(long)]
    pub id: Option<Uuid>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SavedPromptDeleteArgs {
    #[arg(value_name = "PROMPT_ID")]
    pub id: Uuid,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SeriesArgs {
    /// A meeting in the series.
    #[arg(value_name = "MEETING_ID")]
    pub meeting: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SeriesTemplateChoice {
    General,
    OneOnOne,
    Interview,
    SalesCall,
    Standup,
    /// Clear the series' choice.
    None,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SeriesTemplateArgs {
    /// A meeting in the series.
    #[arg(value_name = "MEETING_ID")]
    pub meeting: String,
    #[arg(value_enum, required_unless_present = "custom", conflicts_with = "custom")]
    pub template: Option<SeriesTemplateChoice>,
    /// One of your own templates, by id.
    #[arg(long)]
    pub custom: Option<Uuid>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SeriesDigestArgs {
    /// A meeting in the series.
    #[arg(value_name = "MEETING_ID")]
    pub meeting: String,
    /// Take the series out of the evening digest.
    #[arg(long)]
    pub off: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FilingArgs {
    #[arg(value_name = "SUGGESTION_ID")]
    pub suggestion: String,
    /// Turn the suggestion down for good.
    #[arg(long)]
    pub decline: bool,
    #[command(flatten)]
    pub change: Change,
}

impl SettingsVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self.change() {
            None
            | Some(Change {
                dry_run: true,
                ..
            }) => READ_SCOPES,
            Some(_) => WRITE_SCOPES,
        }
    }

    const fn change(&self) -> Option<&Change> {
        match self {
            Self::Settings
            | Self::Workflows(_)
            | Self::SavedPrompts
            | Self::Series(_)
            | Self::Evening
            | Self::Connections => None,
            Self::Setting(SettingArgs { change, .. })
            | Self::SettingReset(SettingResetArgs { change, .. })
            | Self::ModeSave(ModeSaveArgs { change, .. })
            | Self::ModeDelete(ModeDeleteArgs { change, .. })
            | Self::ModeReorder(ModeReorderArgs { change, .. })
            | Self::PromptSave(PromptSaveArgs { change, .. })
            | Self::PromptDelete(PromptDeleteArgs { change, .. })
            | Self::VocabularyAdd(VocabularyAddArgs { change, .. })
            | Self::VocabularyCsv(VocabularyCsvArgs { change, .. })
            | Self::WorkflowEnable(WorkflowEnableArgs { change, .. })
            | Self::SavedPromptSave(SavedPromptSaveArgs { change, .. })
            | Self::SavedPromptDelete(SavedPromptDeleteArgs { change, .. })
            | Self::SeriesTemplate(SeriesTemplateArgs { change, .. })
            | Self::SeriesDigest(SeriesDigestArgs { change, .. })
            | Self::Filing(FilingArgs { change, .. }) => Some(change),
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Setting(_) => "setting",
            Self::SettingReset(_) => "setting-reset",
            Self::ModeSave(_) => "mode-save",
            Self::ModeDelete(_) => "mode-delete",
            Self::ModeReorder(_) => "mode-reorder",
            Self::PromptSave(_) => "prompt-save",
            Self::PromptDelete(_) => "prompt-delete",
            Self::VocabularyAdd(_) => "vocabulary-add",
            Self::VocabularyCsv(_) => "vocabulary-csv",
            Self::Workflows(_) => "workflows",
            Self::WorkflowEnable(_) => "workflow-enable",
            Self::SavedPrompts => "saved-prompts",
            Self::SavedPromptSave(_) => "saved-prompt-save",
            Self::SavedPromptDelete(_) => "saved-prompt-delete",
            Self::Series(_) => "series",
            Self::SeriesTemplate(_) => "series-template",
            Self::SeriesDigest(_) => "series-digest",
            Self::Evening => "evening",
            Self::Filing(_) => "filing",
            Self::Connections => "connections",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSettings {
    pub schema_version: u32,
    pub what: String,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub replayed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<&'static str>,
}

/// A change to the settings document: what it shows before and after, and
/// the mutation itself, which a dry run makes on a copy.
struct SettingsChange<'a> {
    what: String,
    request: Value,
    show: Show<'a>,
    apply: Apply<'a>,
}

type Show<'a> = Box<dyn Fn(&AppSettings) -> Value + 'a>;
type Apply<'a> = Box<dyn Fn(&mut AppSettings) -> Result<(), String> + 'a>;

fn show<'a>(read: impl Fn(&AppSettings) -> Value + 'a) -> Show<'a> {
    Box::new(read)
}

fn apply<'a>(change: impl Fn(&mut AppSettings) -> Result<(), String> + 'a) -> Apply<'a> {
    Box::new(change)
}

fn flag(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        other => Err(format!("{other} is not true or false.")),
    }
}

fn parsed<T: serde::de::DeserializeOwned>(what: &str, value: &str) -> Result<T, String> {
    serde_json::from_str(value).map_err(|error| format!("{what} is not valid JSON for it: {error}"))
}

fn named<T: serde::de::DeserializeOwned>(value: &str, choices: &str) -> Result<T, String> {
    serde_json::from_value(json!(value.trim()))
        .map_err(|_| format!("{} is not one of: {choices}.", value.trim()))
}

fn log_level(value: &str) -> Result<LogLevel, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "trace" => Ok(LogLevel::Trace),
        "debug" => Ok(LogLevel::Debug),
        "info" => Ok(LogLevel::Info),
        "warn" => Ok(LogLevel::Warn),
        "error" => Ok(LogLevel::Error),
        other => Err(format!("{other} is not one of: trace, debug, info, warn, error.")),
    }
}

fn provider_for(settings: &AppSettings, asked: Option<&str>) -> Result<String, String> {
    let id = asked.map_or_else(|| settings.post_process_provider_id.clone(), str::to_owned);
    if settings.post_process_provider(&id).is_none() {
        return Err(format!("No cleanup provider {id}."));
    }
    Ok(id)
}

fn cleaned_trackers(trackers: Vec<KeywordTracker>) -> Vec<KeywordTracker> {
    trackers
        .into_iter()
        .filter_map(|tracker| {
            let name = tracker.name.trim().to_owned();
            let patterns: Vec<String> = tracker
                .patterns
                .into_iter()
                .map(|pattern| pattern.trim().to_owned())
                .filter(|pattern| !pattern.is_empty())
                .collect();
            (!name.is_empty()).then_some(KeywordTracker { name, patterns })
        })
        .collect()
}

/// The prep switches that let Sona read mail or the web stay the user's.
fn prep_consent_kept(current: &PrepPreferences, asked: &PrepPreferences) -> bool {
    current.email_context_enabled == asked.email_context_enabled
        && current.mail_compose_enabled == asked.mail_compose_enabled
        && current.web_research_enabled == asked.web_research_enabled
}

fn setting_change<'a>(args: &'a SettingArgs) -> SettingsChange<'a> {
    let value = args.value.as_str();
    let provider = args.provider.as_deref();
    let (show, apply) = match args.key {
        SettingKey::PushToTalk => (
            show(|s| json!(s.push_to_talk)),
            apply(move |s| Ok(s.push_to_talk = flag(value)?)),
        ),
        SettingKey::AudioFeedback => (
            show(|s| json!(s.audio_feedback)),
            apply(move |s| Ok(s.audio_feedback = flag(value)?)),
        ),
        SettingKey::AudioFeedbackVolume => (
            show(|s| json!(s.audio_feedback_volume)),
            apply(move |s| {
                let volume: f32 = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("{value} is not a number."))?;
                if !(0.0..=1.0).contains(&volume) {
                    return Err("The volume is from 0 to 1.".to_owned());
                }
                s.audio_feedback_volume = volume;
                Ok(())
            }),
        ),
        SettingKey::SoundTheme => (
            show(|s| json!(s.sound_theme)),
            apply(move |s| {
                s.sound_theme = named::<SoundTheme>(value, "marimba, pop, custom")?;
                Ok(())
            }),
        ),
        SettingKey::MuteWhileRecording => (
            show(|s| json!(s.mute_while_recording)),
            apply(move |s| Ok(s.mute_while_recording = flag(value)?)),
        ),
        SettingKey::QuietSpeech => (
            show(|s| json!(s.quiet_speech_enabled)),
            apply(move |s| Ok(s.quiet_speech_enabled = flag(value)?)),
        ),
        SettingKey::CommandMode => (
            show(|s| json!(s.command_mode_enabled)),
            apply(move |s| Ok(s.command_mode_enabled = flag(value)?)),
        ),
        SettingKey::Overlay => (
            show(|s| json!(s.overlay_style)),
            apply(move |s| {
                s.overlay_style = named::<OverlayStyle>(value, "none, minimal, live")?;
                Ok(())
            }),
        ),
        SettingKey::DictationLanguages => (
            show(|s| json!({ "languages": s.dictation_languages, "selected": s.selected_language })),
            apply(move |s| {
                let languages: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|code| !code.is_empty())
                    .map(str::to_owned)
                    .collect();
                settings::apply_dictation_languages(s, languages);
                Ok(())
            }),
        ),
        SettingKey::AppLanguage => (
            show(|s| json!(s.app_language)),
            apply(move |s| {
                let code = value.trim();
                if code.is_empty() {
                    return Err("The app language needs a code, such as en.".to_owned());
                }
                s.app_language = code.to_owned();
                Ok(())
            }),
        ),
        SettingKey::Theme => (
            show(|s| json!(s.theme)),
            apply(move |s| {
                s.theme = named::<Theme>(value, "system, light, dark")?;
                Ok(())
            }),
        ),
        SettingKey::LogLevel => (
            show(|s| json!(s.log_level)),
            apply(move |s| Ok(s.log_level = log_level(value)?)),
        ),
        SettingKey::DebugMode => (
            show(|s| json!(s.debug_mode)),
            apply(move |s| Ok(s.debug_mode = flag(value)?)),
        ),
        SettingKey::Cleanup => (
            show(|s| json!(s.post_process_enabled)),
            apply(move |s| Ok(s.post_process_enabled = flag(value)?)),
        ),
        SettingKey::CleanupProvider => (
            show(|s| json!(s.post_process_provider_id)),
            apply(move |s| {
                let id = value.trim();
                if id == LOCAL_CLEANUP_PROVIDER_ID {
                    return Err("Offline cleanup is chosen in Sona, which checks its model is downloaded first.".to_owned());
                }
                s.post_process_provider_id = provider_for(s, Some(id))?;
                Ok(())
            }),
        ),
        SettingKey::CleanupModel => (
            show(move |s| {
                let id = provider_for(s, provider).unwrap_or_default();
                json!({ "provider": id, "model": s.post_process_models.get(&id) })
            }),
            apply(move |s| {
                let id = provider_for(s, provider)?;
                s.post_process_models.insert(id, value.trim().to_owned());
                Ok(())
            }),
        ),
        SettingKey::CleanupBaseUrl => (
            show(move |s| {
                let id = provider_for(s, provider).unwrap_or_default();
                json!({ "provider": id, "base_url": s.post_process_provider(&id).map(|p| p.base_url.clone()) })
            }),
            apply(move |s| {
                let id = provider_for(s, provider)?;
                crate::shortcut::apply_post_process_base_url(s, &id, value.trim().to_owned())
            }),
        ),
        SettingKey::EveningDigest => (
            show(|s| json!(s.meeting_digest_enabled)),
            apply(move |s| Ok(s.meeting_digest_enabled = flag(value)?)),
        ),
        SettingKey::AutoCloseParents => (
            show(|s| json!(s.work_close_parents)),
            apply(move |s| Ok(s.work_close_parents = flag(value)?)),
        ),
        SettingKey::ProjectDigest => (
            show(|s| json!(s.work_digest_enabled)),
            apply(move |s| Ok(s.work_digest_enabled = flag(value)?)),
        ),
        SettingKey::ProjectDigestDay => (
            show(|s| json!(s.work_digest_day)),
            apply(move |s| {
                s.work_digest_day = named::<DigestDay>(
                    &value.trim().to_ascii_lowercase(),
                    "monday, tuesday, wednesday, thursday, friday, saturday, sunday",
                )?;
                Ok(())
            }),
        ),
        SettingKey::Snippets => (
            show(|s| json!(s.snippets_enabled)),
            apply(move |s| Ok(s.snippets_enabled = flag(value)?)),
        ),
        SettingKey::TextReplacements => (
            show(|s| json!(s.replacements_enabled)),
            apply(move |s| Ok(s.replacements_enabled = flag(value)?)),
        ),
        SettingKey::Mode => (
            show(|s| json!(s.active_mode_id)),
            apply(move |s| modes::apply_set_active_mode(s, value.trim())),
        ),
        SettingKey::DictationPrompt => (
            show(|s| json!(s.post_process_selected_prompt_id)),
            apply(move |s| modes::apply_use_post_process_prompt(s, value.trim())),
        ),
        SettingKey::KeywordTrackers => (
            show(|s| json!(s.trackers_list)),
            apply(move |s| {
                s.trackers_list = cleaned_trackers(parsed("The tracker list", value)?);
                Ok(())
            }),
        ),
        SettingKey::MeetingPrep => (
            show(|s| json!(s.meeting_prep)),
            apply(move |s| {
                let asked: PrepPreferences = parsed("The prep preferences", value)?;
                asked.validate()?;
                if !prep_consent_kept(&s.meeting_prep, &asked) {
                    return Err("Reading mail, drafting mail and web research are turned on or off by the user in Sona.".to_owned());
                }
                s.meeting_prep = asked;
                Ok(())
            }),
        ),
    };
    SettingsChange {
        what: serde_json::to_value(args.key)
            .ok()
            .and_then(|key| key.as_str().map(str::to_owned))
            .unwrap_or_default(),
        request: json!({ "key": args.key, "value": args.value, "provider": args.provider }),
        show,
        apply,
    }
}

fn mode_error(error: modes::ModeMutationError) -> String {
    serde_json::to_string(&error).unwrap_or_else(|_| "The mode could not be changed.".to_owned())
}

/// Saves the mode an agent sent, except where its audio goes and what runs to
/// paste: an agent cannot move a mode to a cloud engine or a paste script, so
/// those stay as the user left them, and a new mode starts local.
fn save_mode(asked: &ModeDefinition, settings: &mut AppSettings) -> Result<(), String> {
    let saved = settings.modes.iter().find(|mode| mode.id == asked.id);
    let mut mode = asked.clone();
    mode.asr.requested_engine =
        saved.map_or(RequestedEngine::Local, |saved| saved.asr.requested_engine);
    mode.delivery.external_script_path =
        saved.and_then(|saved| saved.delivery.external_script_path.clone());
    if mode.delivery.paste_method == PasteMethod::ExternalScript {
        mode.delivery.paste_method =
            saved.map_or_else(PasteMethod::default, |saved| saved.delivery.paste_method);
    }
    let revision = settings.modes_revision;
    modes::apply_upsert_mode(settings, mode, revision).map_err(mode_error)
}

#[derive(Serialize)]
struct RunView<'a> {
    workflow_id: &'a WorkflowId,
    status: &'a WorkflowRunStatus,
    started_at_utc_ms: i64,
    finished_at_utc_ms: i64,
    outcome_code: &'a WorkflowOutcomeCode,
    outcome_counts: &'a WorkflowOutcomeCounts,
    error: Option<&'a str>,
}

fn run_view(run: &WorkflowRunReceipt) -> RunView<'_> {
    RunView {
        workflow_id: &run.workflow_id,
        status: &run.status,
        started_at_utc_ms: run.started_at_utc_ms,
        finished_at_utc_ms: run.finished_at_utc_ms,
        outcome_code: &run.outcome_code,
        outcome_counts: &run.outcome_counts,
        error: run.error.as_deref(),
    }
}

fn workflow_id(value: &str) -> Result<WorkflowId, ExternalError> {
    serde_json::from_value(json!(value.trim()))
        .map_err(|_| ExternalError::invalid(format!("No workflow {value}. See: sona work workflows.")))
}

#[derive(Serialize)]
struct DictationView<'a> {
    push_to_talk: bool,
    audio_feedback: bool,
    audio_feedback_volume: f32,
    sound_theme: SoundTheme,
    mute_while_recording: bool,
    quiet_speech: bool,
    command_mode: bool,
    overlay: OverlayStyle,
    dictation_languages: &'a [String],
    microphone: Option<&'a str>,
    microphones: Vec<String>,
}

#[derive(Serialize)]
struct AppearanceView<'a> {
    theme: Theme,
    app_language: &'a str,
    log_level: LogLevel,
    debug_mode: bool,
}

#[derive(Serialize)]
struct ProviderView<'a> {
    id: &'a str,
    label: &'a str,
    base_url: &'a str,
    model: Option<&'a String>,
}

#[derive(Serialize)]
struct CleanupView<'a> {
    enabled: bool,
    provider: &'a str,
    providers: Vec<ProviderView<'a>>,
    dictation_prompts: &'a [LLMPrompt],
    selected_prompt: Option<&'a str>,
}

#[derive(Serialize)]
struct ShortcutView<'a> {
    id: &'a str,
    name: &'a str,
    key: &'a str,
    default: &'a str,
}

#[derive(Serialize)]
struct SettingsView<'a> {
    dictation: DictationView<'a>,
    appearance: AppearanceView<'a>,
    cleanup: CleanupView<'a>,
    modes: modes::ModeSettingsSnapshot,
    vocabulary: &'a [VocabularyEntry],
    snippets: bool,
    text_replacements: bool,
    shortcuts: Vec<ShortcutView<'a>>,
    evening_digest: bool,
    auto_close_parents: bool,
    project_digest: bool,
    project_digest_day: DigestDay,
    keyword_trackers: &'a [KeywordTracker],
    meeting_prep: &'a PrepPreferences,
}

fn settings_view(settings: &AppSettings, microphones: Vec<String>) -> SettingsView<'_> {
    let mut shortcuts: Vec<ShortcutView<'_>> = settings
        .bindings
        .values()
        .map(|binding| ShortcutView {
            id: &binding.id,
            name: &binding.name,
            key: &binding.current_binding,
            default: &binding.default_binding,
        })
        .collect();
    shortcuts.sort_by_key(|shortcut| shortcut.id);
    SettingsView {
        dictation: DictationView {
            push_to_talk: settings.push_to_talk,
            audio_feedback: settings.audio_feedback,
            audio_feedback_volume: settings.audio_feedback_volume,
            sound_theme: settings.sound_theme,
            mute_while_recording: settings.mute_while_recording,
            quiet_speech: settings.quiet_speech_enabled,
            command_mode: settings.command_mode_enabled,
            overlay: settings.overlay_style,
            dictation_languages: &settings.dictation_languages,
            microphone: settings.selected_microphone.as_deref(),
            microphones,
        },
        appearance: AppearanceView {
            theme: settings.theme,
            app_language: &settings.app_language,
            log_level: settings.log_level,
            debug_mode: settings.debug_mode,
        },
        cleanup: CleanupView {
            enabled: settings.post_process_enabled,
            provider: &settings.post_process_provider_id,
            providers: settings
                .post_process_providers
                .iter()
                .map(|provider| ProviderView {
                    id: &provider.id,
                    label: &provider.label,
                    base_url: &provider.base_url,
                    model: settings.post_process_models.get(&provider.id),
                })
                .collect(),
            dictation_prompts: &settings.post_process_prompts,
            selected_prompt: settings.post_process_selected_prompt_id.as_deref(),
        },
        modes: modes::mode_settings_snapshot(settings),
        vocabulary: &settings.custom_words,
        snippets: settings.snippets_enabled,
        text_replacements: settings.replacements_enabled,
        shortcuts,
        evening_digest: settings.meeting_digest_enabled,
        auto_close_parents: settings.work_close_parents,
        project_digest: settings.work_digest_enabled,
        project_digest_day: settings.work_digest_day,
        keyword_trackers: &settings.trackers_list,
        meeting_prep: &settings.meeting_prep,
    }
}

fn input_devices() -> Vec<String> {
    crate::audio_toolkit::audio::list_input_devices()
        .map(|devices| devices.into_iter().map(|device| device.name).collect())
        .unwrap_or_default()
}

impl Tracker<'_> {
    pub(super) async fn settings_verb(
        &self,
        verb: &SettingsVerb,
    ) -> Result<WorkAnswer, ExternalError> {
        match verb {
            SettingsVerb::Settings => {
                let microphones = tauri::async_runtime::spawn_blocking(input_devices)
                    .await
                    .unwrap_or_default();
                let settings = settings::get_settings(self.app);
                Ok(read(verb.name(), &settings_view(&settings, microphones)))
            }
            SettingsVerb::Setting(args) => {
                let change = setting_change(args);
                let cleared_briefs = args.key == SettingKey::MeetingPrep;
                self.change_settings(&args.change, change, cleared_briefs).await
            }
            SettingsVerb::SettingReset(args) => {
                let change = reset_change(args)?;
                self.change_settings(&args.change, change, false).await
            }
            SettingsVerb::ModeSave(args) => {
                let mode: ModeDefinition = parsed("--mode-json", &args.mode_json)
                    .map_err(ExternalError::invalid)?;
                let change = SettingsChange {
                    what: verb.name().to_owned(),
                    request: json!({ "mode": mode }),
                    show: show(|s| json!(modes::mode_settings_snapshot(s))),
                    apply: apply(move |s| save_mode(&mode, s)),
                };
                self.change_settings(&args.change, change, false).await
            }
            SettingsVerb::ModeDelete(args) => {
                let change = SettingsChange {
                    what: verb.name().to_owned(),
                    request: json!({ "mode": args.mode }),
                    show: show(|s| json!(modes::mode_settings_snapshot(s))),
                    apply: apply(|s| {
                        let revision = s.modes_revision;
                        modes::apply_delete_mode(s, &args.mode, revision).map_err(mode_error)
                    }),
                };
                self.change_settings(&args.change, change, false).await
            }
            SettingsVerb::ModeReorder(args) => {
                let change = SettingsChange {
                    what: verb.name().to_owned(),
                    request: json!({ "modes": args.modes }),
                    show: show(|s| json!(modes::mode_settings_snapshot(s))),
                    apply: apply(|s| {
                        let revision = s.modes_revision;
                        modes::apply_reorder_modes(s, &args.modes, revision).map_err(mode_error)
                    }),
                };
                self.change_settings(&args.change, change, false).await
            }
            SettingsVerb::PromptSave(args) => {
                let new_id = format!("prompt_{}", now_ms());
                let change = SettingsChange {
                    what: verb.name().to_owned(),
                    request: json!({ "id": args.id, "name": args.name, "prompt": args.prompt }),
                    show: show(|s| json!(s.post_process_prompts)),
                    apply: apply(move |s| match &args.id {
                        Some(id) => crate::shortcut::update_prompt_in(
                            s,
                            id,
                            args.name.clone(),
                            args.prompt.clone(),
                        ),
                        None => {
                            s.post_process_prompts.push(LLMPrompt {
                                id: new_id.clone(),
                                name: args.name.clone(),
                                prompt: args.prompt.clone(),
                            });
                            Ok(())
                        }
                    }),
                };
                self.change_settings(&args.change, change, false).await
            }
            SettingsVerb::PromptDelete(args) => {
                let change = SettingsChange {
                    what: verb.name().to_owned(),
                    request: json!({ "id": args.id }),
                    show: show(|s| json!(s.post_process_prompts)),
                    apply: apply(|s| crate::shortcut::delete_prompt_in(s, &args.id)),
                };
                self.change_settings(&args.change, change, false).await
            }
            SettingsVerb::VocabularyAdd(args) => {
                let scope = scope(args.mode.as_deref());
                let change = SettingsChange {
                    what: verb.name().to_owned(),
                    request: json!({ "spoken": args.spoken, "written": args.written, "mode": args.mode }),
                    show: vocabulary_shown(scope.clone()),
                    apply: apply(move |s| {
                        vocabulary::add_correction_in(
                            s,
                            &scope,
                            args.spoken.clone(),
                            args.written.clone(),
                        )
                        .map(|_| ())
                    }),
                };
                self.change_settings(&args.change, change, false).await
            }
            SettingsVerb::VocabularyCsv(args) => {
                let csv = std::fs::read_to_string(&args.file).map_err(|error| {
                    ExternalError::invalid(format!(
                        "{} could not be read: {error}",
                        args.file.display()
                    ))
                })?;
                let scope = scope(args.mode.as_deref());
                let change = SettingsChange {
                    what: verb.name().to_owned(),
                    request: json!({ "csv": csv, "mode": args.mode }),
                    show: vocabulary_shown(scope.clone()),
                    apply: apply(move |s| vocabulary::apply_csv_in(s, &scope, &csv).map(|_| ())),
                };
                self.change_settings(&args.change, change, false).await
            }
            SettingsVerb::Workflows(args) => self.workflows(args).await,
            SettingsVerb::WorkflowEnable(args) => self.workflow_enable(args).await,
            SettingsVerb::SavedPrompts => {
                let list = self.meetings.saved_prompts().await.map_err(meeting_error)?;
                Ok(read(verb.name(), &list))
            }
            SettingsVerb::SavedPromptSave(args) => self.saved_prompt_save(args).await,
            SettingsVerb::SavedPromptDelete(args) => self.saved_prompt_delete(args).await,
            SettingsVerb::Series(args) => {
                let (_, preferences) = self.meeting_series(&args.meeting).await?;
                Ok(read(verb.name(), &preferences))
            }
            SettingsVerb::SeriesTemplate(args) => self.series_template(args).await,
            SettingsVerb::SeriesDigest(args) => self.series_digest(args).await,
            SettingsVerb::Evening => Ok(read(verb.name(), &self.evening_card().await?)),
            SettingsVerb::Filing(args) => self.filing_decide(args).await,
            SettingsVerb::Connections => Ok(read(verb.name(), &connections(&*store(self.meetings).await?)?)),
        }
    }

    /// Runs `change` once per operation id, on the settings file, and leaves
    /// the stamp a running Sona follows.
    async fn change_settings(
        &self,
        args: &Change,
        change: SettingsChange<'_>,
        clears_briefs: bool,
    ) -> Result<WorkAnswer, ExternalError> {
        let current = settings::get_settings(self.app);
        let before = (change.show)(&current);
        let mut preview = current;
        (change.apply)(&mut preview).map_err(ExternalError::invalid)?;
        let after = (change.show)(&preview);
        if args.dry_run {
            return Ok(WorkAnswer::Settings(Box::new(ExternalSettings {
                schema_version: QUERY_SCHEMA_VERSION,
                what: change.what,
                dry_run: true,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                replayed: false,
                agent: args.write.agent.as_ref().map(|_| args.write.agent()),
                before: Some(before),
                value: after,
                live: None,
            })));
        }
        let operation_id = args.write.fresh_operation_id();
        let command = format!("cli_setting_{}", change.what);
        let request = change.request.to_string();
        if let Some(answer) = self.settings_replay(&operation_id, &command, &request)? {
            return Ok(WorkAnswer::Settings(Box::new(answer)));
        }
        self.applies("changing Sona's settings")?;
        if clears_briefs {
            self.meetings
                .store()
                .await
                .map_err(|_| refusal(self.history, planning::CONVERSATIONS_CLOSED.to_owned()))?
                .clear_briefs()
                .map_err(|_| {
                    ExternalError::new(
                        ExternalErrorCode::Failed,
                        "The saved meeting briefs could not be cleared.",
                    )
                })?;
        }
        let after = settings::try_update_settings(self.app, |settings| {
            (change.apply)(settings)?;
            Ok::<_, String>((change.show)(settings))
        })
        .map_err(|message| ExternalError::new(ExternalErrorCode::Failed, message))?;
        beacon::leave_settings(self.app);
        let answer = ExternalSettings {
            schema_version: QUERY_SCHEMA_VERSION,
            what: change.what,
            dry_run: false,
            operation_id: Some(operation_id.clone()),
            replayed: false,
            agent: Some(args.write.agent()),
            before: Some(before),
            value: after,
            live: Some(LIVE),
        };
        self.settings_record(&operation_id, &command, &request, &answer)?;
        Ok(WorkAnswer::Settings(Box::new(answer)))
    }

    fn settings_record(
        &self,
        operation_id: &str,
        command: &str,
        request: &str,
        answer: &ExternalSettings,
    ) -> Result<(), ExternalError> {
        let response = serde_json::to_string(answer).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The change could not be named.")
        })?;
        planning::history(self.history, |conn| {
            conn.execute(
                "INSERT INTO planning_operations (operation_id, command, request_json, response_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![operation_id, command, request, response, now_ms()],
            )?;
            Ok(())
        })
        .map_err(|message| refusal(self.history, message))
    }

    /// The earlier answer to a store change retried with its operation id.
    /// Checked before the change is looked up, since a filed suggestion or a
    /// deleted prompt is gone by the retry.
    fn settings_replayed(
        &self,
        what: &str,
        change: &Change,
        request: &Value,
    ) -> Result<Option<WorkAnswer>, ExternalError> {
        let Some(operation_id) = change.write.operation_id.filter(|_| !change.dry_run) else {
            return Ok(None);
        };
        Ok(self
            .settings_replay(&operation_id.to_string(), &ledger(what), &request.to_string())?
            .map(|answer| WorkAnswer::Settings(Box::new(answer))))
    }

    /// Runs `write` against the meeting library once, as the change `plan`
    /// shows, and keeps its answer for a retry with the same operation id.
    async fn store_change<T, Fut>(
        &self,
        plan: StoreChange<'_>,
        write: impl FnOnce(MeetingOperationId) -> Fut,
    ) -> Result<WorkAnswer, ExternalError>
    where
        T: Serialize,
        Fut: Future<Output = Result<T, ExternalError>>,
    {
        let args = &plan.change.write;
        if plan.change.dry_run {
            return Ok(WorkAnswer::Settings(Box::new(ExternalSettings {
                schema_version: QUERY_SCHEMA_VERSION,
                what: plan.what.to_owned(),
                dry_run: true,
                operation_id: args.operation_id.map(|id| id.to_string()),
                replayed: false,
                agent: args.agent.as_ref().map(|_| args.agent()),
                before: Some(plan.before),
                value: plan.after,
                live: None,
            })));
        }
        self.applies(plan.doing)?;
        let operation = args.operation_id.unwrap_or_else(Uuid::new_v4);
        let value = write(MeetingOperationId::from_uuid(operation)).await?;
        beacon::leave_settings(self.app);
        let answer = ExternalSettings {
            schema_version: QUERY_SCHEMA_VERSION,
            what: plan.what.to_owned(),
            dry_run: false,
            operation_id: Some(operation.to_string()),
            replayed: false,
            agent: Some(args.agent()),
            before: Some(plan.before),
            value: json!(value),
            live: Some(LIVE),
        };
        self.settings_record(
            &operation.to_string(),
            &ledger(plan.what),
            &plan.request.to_string(),
            &answer,
        )?;
        Ok(WorkAnswer::Settings(Box::new(answer)))
    }

    /// The answer an earlier run with this operation id gave, if the request
    /// is the same; a refusal if it is not.
    fn settings_replay(
        &self,
        operation_id: &str,
        command: &str,
        request: &str,
    ) -> Result<Option<ExternalSettings>, ExternalError> {
        let found: Option<(String, String, String)> = planning::history(self.history, |conn| {
            Ok(conn
                .query_row(
                    "SELECT command, request_json, response_json FROM planning_operations WHERE operation_id = ?1",
                    [operation_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?)
        })
        .map_err(|message| refusal(self.history, message))?;
        let Some((stored, asked, response)) = found else {
            return Ok(None);
        };
        if stored != command || asked != request {
            return Err(ExternalError::invalid(format!(
                "Operation id {operation_id} was already used for a different change. Pass a new --operation-id, or leave it out."
            )));
        }
        let earlier: Value = serde_json::from_str(&response).unwrap_or(Value::Null);
        Ok(Some(ExternalSettings {
            schema_version: QUERY_SCHEMA_VERSION,
            what: earlier["what"].as_str().unwrap_or_default().to_owned(),
            dry_run: false,
            operation_id: Some(operation_id.to_owned()),
            replayed: true,
            agent: earlier["agent"].as_str().map(str::to_owned),
            before: Some(earlier["before"].clone()),
            value: earlier["value"].clone(),
            live: earlier["live"].as_str().map(|_| LIVE),
        }))
    }

    async fn workflows(&self, args: &WorkflowsArgs) -> Result<WorkAnswer, ExternalError> {
        let failed = |_| refusal(self.history, planning::CONVERSATIONS_CLOSED.to_owned());
        let value = if args.runs {
            let workflow_id = args.workflow.as_deref().map(workflow_id).transpose()?;
            let runs = self
                .meetings
                .workflow_runs(WorkflowRunsRequest {
                    workflow_id,
                    cursor: None,
                    limit: args.limit,
                })
                .await
                .map_err(failed)?;
            json!({ "runs": runs.entries.iter().map(run_view).collect::<Vec<_>>() })
        } else {
            let list = self.meetings.workflows_list().await.map_err(failed)?;
            json!(workflows_view(&list))
        };
        Ok(read("workflows", &value))
    }

    async fn workflow_enable(&self, args: &WorkflowEnableArgs) -> Result<WorkAnswer, ExternalError> {
        let id = workflow_id(&args.workflow)?;
        let enabled = !args.off;
        let request = json!({ "workflow_id": id, "enabled": enabled });
        if let Some(done) = self.settings_replayed("workflow-enable", &args.change, &request)? {
            return Ok(done);
        }
        let failed = |_| refusal(self.history, planning::CONVERSATIONS_CLOSED.to_owned());
        let list = self.meetings.workflows_list().await.map_err(failed)?;
        let before = list
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.enabled)
            .ok_or_else(|| ExternalError::invalid(format!("No workflow {}.", args.workflow)))?;
        let plan = StoreChange {
            what: "workflow-enable",
            doing: "turning Sona's workflows on or off",
            change: &args.change,
            before: json!({ "workflow_id": id, "enabled": before }),
            after: request.clone(),
            request,
        };
        let revision = list.revision;
        self.store_change(plan, |_| async move {
            let list = self
                .meetings
                .workflow_set_enabled(WorkflowSetEnabledRequest {
                    workflow_id: id,
                    enabled,
                    expected_revision: revision,
                })
                .await
                .map_err(|error| {
                    ExternalError::new(
                        ExternalErrorCode::Failed,
                        format!("The workflow could not be changed: {error:?}"),
                    )
                })?;
            Ok(json!(workflows_view(&list)))
        })
        .await
    }

    async fn saved_prompt_save(&self, args: &SavedPromptSaveArgs) -> Result<WorkAnswer, ExternalError> {
        let request = json!({
            "id": args.id,
            "name": args.name,
            "body": args.body,
            "about": format!("{:?}", args.about),
            "schema": args.schema,
        });
        if let Some(done) = self.settings_replayed("saved-prompt-save", &args.change, &request)? {
            return Ok(done);
        }
        let list = self.meetings.saved_prompts().await.map_err(meeting_error)?;
        let prompt_id = args.id.map(SavedPromptId::from_uuid);
        let before = match prompt_id {
            Some(id) => Some(
                list.prompts
                    .iter()
                    .find(|prompt| prompt.prompt_id == id)
                    .ok_or_else(|| missing_prompt(id))?,
            ),
            None => None,
        };
        let mut save = SavedPromptSaveRequest {
            operation_id: MeetingOperationId::new(),
            prompt_id,
            name: args.name.clone(),
            body: args.body.clone(),
            output: args
                .schema
                .clone()
                .map_or(PromptOutput::Text, |json_schema| PromptOutput::Schema { json_schema }),
            target: match args.about {
                PromptAbout::Meeting => PromptTarget::Meeting,
                PromptAbout::Person => PromptTarget::Person,
                PromptAbout::Series => PromptTarget::Series,
            },
            expected_revision: list.revision,
        };
        let (name, body, output) = normalized_prompt(&save).map_err(|_| {
            ExternalError::invalid(
                "A saved prompt needs a name up to 120 bytes, a body up to 4 KB, and a schema that is one JSON object.",
            )
        })?;
        let plan = StoreChange {
            what: "saved-prompt-save",
            doing: "saving prompts",
            change: &args.change,
            before: json!(before),
            after: json!({ "prompt_id": prompt_id, "name": name, "body": body, "output": output, "target": save.target }),
            request,
        };
        self.store_change(plan, |operation_id| async move {
            save.operation_id = operation_id;
            let saved = self.meetings.save_saved_prompt(save).await.map_err(meeting_error)?;
            Ok(saved.prompts)
        })
        .await
    }

    async fn saved_prompt_delete(&self, args: &SavedPromptDeleteArgs) -> Result<WorkAnswer, ExternalError> {
        let request = json!({ "id": args.id });
        if let Some(done) = self.settings_replayed("saved-prompt-delete", &args.change, &request)? {
            return Ok(done);
        }
        let list = self.meetings.saved_prompts().await.map_err(meeting_error)?;
        let prompt_id = SavedPromptId::from_uuid(args.id);
        let before = list
            .prompts
            .iter()
            .find(|prompt| prompt.prompt_id == prompt_id)
            .ok_or_else(|| missing_prompt(prompt_id))?;
        let plan = StoreChange {
            what: "saved-prompt-delete",
            doing: "deleting prompts",
            change: &args.change,
            before: json!(before),
            after: Value::Null,
            request,
        };
        let revision = list.revision;
        self.store_change(plan, |operation_id| async move {
            let deleted = self
                .meetings
                .delete_saved_prompt(SavedPromptDeleteRequest {
                    operation_id,
                    prompt_id,
                    expected_revision: revision,
                })
                .await
                .map_err(meeting_error)?;
            Ok(deleted.prompts)
        })
        .await
    }

    /// The series a meeting an agent may see belongs to.
    async fn meeting_series(
        &self,
        meeting: &str,
    ) -> Result<(String, MeetingSeriesPreferences), ExternalError> {
        let session = session_id(meeting)?;
        let store = store(self.meetings).await?;
        visible_meeting(&store, session)?;
        store
            .session_snapshot(session)
            .map_err(|error| meeting_refusal(session, error))?;
        let preferences = self
            .meetings
            .series_preferences_for_session(session)
            .await
            .map_err(command_error(session.uuid()))?;
        let key = preferences.series_key.clone().ok_or_else(|| {
            ExternalError::invalid("This meeting is not part of a calendar series.")
        })?;
        Ok((key, preferences))
    }

    async fn series_template(&self, args: &SeriesTemplateArgs) -> Result<WorkAnswer, ExternalError> {
        let template = args.template.and_then(|choice| match choice {
            SeriesTemplateChoice::General => Some(MeetingNotesTemplate::General),
            SeriesTemplateChoice::OneOnOne => Some(MeetingNotesTemplate::OneOnOne),
            SeriesTemplateChoice::Interview => Some(MeetingNotesTemplate::Interview),
            SeriesTemplateChoice::SalesCall => Some(MeetingNotesTemplate::SalesCall),
            SeriesTemplateChoice::Standup => Some(MeetingNotesTemplate::Standup),
            SeriesTemplateChoice::None => None,
        });
        let custom = args.custom.map(MeetingTemplateId::from_uuid);
        let request = json!({ "meeting": args.meeting, "template": template, "custom": custom });
        if let Some(done) = self.settings_replayed("series-template", &args.change, &request)? {
            return Ok(done);
        }
        let (series_key, preferences) = self.meeting_series(&args.meeting).await?;
        let plan = StoreChange {
            what: "series-template",
            doing: "choosing a series' notes template",
            change: &args.change,
            before: json!({ "template": preferences.template, "custom_template_id": preferences.custom_template_id }),
            after: json!({ "template": template, "custom_template_id": custom }),
            request,
        };
        self.store_change(plan, |operation_id| async move {
            let changed = self
                .meetings
                .set_series_template(MeetingSeriesTemplateSetRequest {
                    operation_id,
                    series_key,
                    template,
                    custom_template_id: custom,
                    expected_revision: preferences.revision,
                })
                .await
                .map_err(meeting_error)?;
            Ok(changed.preferences)
        })
        .await
    }

    async fn series_digest(&self, args: &SeriesDigestArgs) -> Result<WorkAnswer, ExternalError> {
        let digest_included = !args.off;
        let request = json!({ "meeting": args.meeting, "digest_included": digest_included });
        if let Some(done) = self.settings_replayed("series-digest", &args.change, &request)? {
            return Ok(done);
        }
        let (series_key, preferences) = self.meeting_series(&args.meeting).await?;
        let plan = StoreChange {
            what: "series-digest",
            doing: "changing what the evening digest covers",
            change: &args.change,
            before: json!({ "digest_included": preferences.digest_included }),
            after: json!({ "digest_included": digest_included }),
            request,
        };
        self.store_change(plan, |operation_id| async move {
            let changed = self
                .meetings
                .set_series_digest(MeetingSeriesDigestSetRequest {
                    operation_id,
                    series_key,
                    digest_included,
                    expected_revision: preferences.revision,
                })
                .await
                .map_err(meeting_error)?;
            Ok(changed.preferences)
        })
        .await
    }

    async fn evening_card(&self) -> Result<EveningView, ExternalError> {
        let store = store(self.meetings).await?;
        let hidden = kept_local(&store)?;
        let card = self
            .meetings
            .home_evening(self.history)
            .await
            .map_err(|message| ExternalError::new(ExternalErrorCode::Failed, message))?;
        evening_view(&store, &hidden, card.local_day, card.lines.map(|lines| lines.lines), card.has_activity, card.unfiled)
    }

    async fn filing_decide(&self, args: &FilingArgs) -> Result<WorkAnswer, ExternalError> {
        let accept = !args.decline;
        let request = json!({ "suggestion": args.suggestion, "accept": accept });
        if let Some(done) = self.settings_replayed("filing", &args.change, &request)? {
            return Ok(done);
        }
        let card = self.evening_card().await?;
        let (recording, suggestion) = card
            .unfiled
            .iter()
            .find_map(|recording| {
                recording
                    .suggestions
                    .iter()
                    .find(|suggestion| suggestion.id == args.suggestion)
                    .map(|suggestion| (recording, suggestion))
            })
            .ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    format!("No filing suggestion {} is waiting.", args.suggestion),
                )
            })?;
        let plan = StoreChange {
            what: "filing",
            doing: "filing recordings",
            change: &args.change,
            before: json!({ "meeting_id": recording.session_id, "title": recording.title, "suggestion": suggestion }),
            after: json!({ "suggestion": args.suggestion, "filed": accept }),
            request,
        };
        self.store_change(plan, |_| async move {
            self.meetings
                .decide_filing(self.app, &args.suggestion, accept, None)
                .await
                .map_err(ExternalError::invalid)?;
            self.evening_card().await
        })
        .await
    }
}

struct StoreChange<'a> {
    what: &'static str,
    doing: &'static str,
    change: &'a Change,
    request: Value,
    before: Value,
    after: Value,
}

fn ledger(what: &str) -> String {
    format!("cli_{}", what.replace('-', "_"))
}

fn meeting_error(error: MeetingCommandError) -> ExternalError {
    QueryError::from(error).into()
}

fn store_failed(error: StoreError) -> ExternalError {
    QueryError::from(error).into()
}

fn missing_prompt(id: SavedPromptId) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("No saved prompt {}.", id.uuid()),
    )
}

#[derive(Serialize)]
struct EveningView {
    local_day: String,
    lines: Option<Vec<String>>,
    /// Today's lines draw on a meeting kept on this Mac, so they are left out.
    lines_left_out: bool,
    has_activity: bool,
    unfiled: Vec<UnfiledRecording>,
}

/// The evening card without what a meeting kept on this Mac said: lines
/// drawn from one, its recordings, and suggestions to make a recording the
/// plan heard in one.
fn evening_view(
    store: &MeetingStore,
    hidden: &KeptLocal,
    local_day: String,
    lines: Option<Vec<String>>,
    has_activity: bool,
    unfiled: Vec<UnfiledRecording>,
) -> Result<EveningView, ExternalError> {
    let lines_left_out = store
        .evening_lines(&local_day)
        .map_err(store_failed)?
        .is_some_and(|stored| stored.meeting_ids.iter().any(|id| hidden.hides(*id)));
    let mut shown = Vec::new();
    for mut recording in unfiled {
        if hidden.hides(recording.session_id) {
            continue;
        }
        let mut suggestions = Vec::new();
        for suggestion in recording.suggestions {
            let from_hidden = match &suggestion.target {
                FilingTarget::Meeting { plan_id, .. } => {
                    hidden.hides(store.heard_plan(*plan_id).map_err(store_failed)?.session_id)
                }
                _ => false,
            };
            if !from_hidden {
                suggestions.push(suggestion);
            }
        }
        recording.suggestions = suggestions;
        shown.push(recording);
    }
    Ok(EveningView {
        local_day,
        lines: lines.filter(|_| !lines_left_out),
        lines_left_out,
        has_activity,
        unfiled: shown,
    })
}

#[derive(Serialize)]
struct RuleView {
    id: String,
    connection_id: String,
    scope: SendScope,
    scope_name: Option<String>,
    enabled: bool,
}

#[derive(Serialize)]
struct ReceiptView {
    id: String,
    connection_name: String,
    meeting_id: Option<String>,
    state: SendState,
    detail: String,
    created_at_utc_ms: i64,
}

#[derive(Serialize)]
struct ConnectionsView {
    connections: Vec<Connection>,
    rules: Vec<RuleView>,
    /// A rule sends one series kept on this Mac, so it is not listed.
    rules_left_out: bool,
    preferences: ConnectionPreferences,
    receipts: Vec<ReceiptView>,
}

/// The connections as Settings lists them, without tokens, without the rules
/// for a series kept on this Mac, and without the sends of such a meeting.
fn connections(store: &MeetingStore) -> Result<ConnectionsView, ExternalError> {
    let hidden = kept_local(store)?;
    let series: HashMap<String, String> = store
        .automation_roster()
        .map_err(store_failed)?
        .series
        .into_iter()
        .map(|series| (series.series_key, series.title))
        .collect();
    let folders: HashMap<String, String> = store
        .meeting_folders()
        .map_err(store_failed)?
        .into_iter()
        .map(|folder| (folder.folder_id.uuid().to_string(), folder.name))
        .collect();
    let all_rules = store.connection_rules().map_err(store_failed)?;
    let rule_series: Vec<String> = all_rules
        .iter()
        .filter_map(|rule| match &rule.scope {
            SendScope::Series(key) => Some(key.clone()),
            SendScope::All | SendScope::Folder(_) => None,
        })
        .collect();
    let series_preferences = store
        .series_preferences_many(&rule_series)
        .map_err(store_failed)?;
    let (rules, kept): (Vec<_>, Vec<_>) = all_rules.into_iter().partition(|rule| match &rule.scope {
        SendScope::Series(key) => !series_preferences
            .get(key.trim())
            .is_some_and(|preferences| preferences.remote_intelligence_opt_out),
        SendScope::All | SendScope::Folder(_) => true,
    });
    let rules = rules
        .into_iter()
        .map(|rule| RuleView {
            scope_name: match &rule.scope {
                SendScope::All => Some("All future meetings".to_owned()),
                SendScope::Series(key) => series.get(key).cloned(),
                SendScope::Folder(id) => folders.get(id).cloned(),
            },
            id: rule.id,
            connection_id: rule.connection_id,
            scope: rule.scope,
            enabled: rule.enabled,
        })
        .collect();
    let receipts = store
        .connection_receipts()
        .map_err(store_failed)?
        .into_iter()
        .filter(|receipt| {
            !receipt
                .session_id
                .as_deref()
                .is_some_and(|meeting| hidden.hides_meeting(meeting))
        })
        .map(|receipt| ReceiptView {
            id: receipt.id,
            connection_name: receipt.connection_name,
            meeting_id: receipt.session_id,
            state: receipt.state,
            detail: receipt.detail,
            created_at_utc_ms: receipt.created_at_utc_ms,
        })
        .collect();
    Ok(ConnectionsView {
        connections: store.connections().map_err(store_failed)?,
        rules,
        rules_left_out: !kept.is_empty(),
        preferences: store.connection_preferences().map_err(store_failed)?,
        receipts,
    })
}

#[derive(Serialize)]
struct WorkflowView<'a> {
    id: &'a WorkflowId,
    enabled: bool,
    last_run: Option<RunView<'a>>,
}

#[derive(Serialize)]
struct WorkflowsView<'a> {
    revision: u64,
    workflows: Vec<WorkflowView<'a>>,
}

fn workflows_view(list: &WorkflowsListResult) -> WorkflowsView<'_> {
    WorkflowsView {
        revision: list.revision,
        workflows: list
            .entries
            .iter()
            .map(|entry| WorkflowView {
                id: &entry.id,
                enabled: entry.enabled,
                last_run: entry.last_run.as_ref().map(run_view),
            })
            .collect(),
    }
}

fn scope(mode: Option<&str>) -> VocabularyScope {
    mode.map_or(VocabularyScope::Global, |mode_id| VocabularyScope::Mode {
        mode_id: mode_id.to_owned(),
    })
}

fn vocabulary_shown<'a>(scope: VocabularyScope) -> Show<'a> {
    show(move |s| json!(vocabulary::entries_in(s, &scope).unwrap_or_default()))
}

fn reset_change(args: &SettingResetArgs) -> Result<SettingsChange<'_>, ExternalError> {
    Ok(match args.target {
        ResetTarget::Replacements => SettingsChange {
            what: "replacements-reset".to_owned(),
            request: json!({ "reset": "replacements" }),
            show: show(|s| json!(s.replacements_rules.len())),
            apply: apply(|s| {
                s.replacements_rules = settings::default_replacement_rules();
                Ok(())
            }),
        },
        ResetTarget::Shortcut => {
            let id = args
                .id
                .clone()
                .ok_or_else(|| ExternalError::invalid("Name the shortcut's id."))?;
            let shown = id.clone();
            SettingsChange {
                what: "shortcut-reset".to_owned(),
                request: json!({ "reset": "shortcut", "id": id }),
                show: show(move |s| {
                    json!(s.bindings.get(&shown).map(|binding| binding.current_binding.clone()))
                }),
                apply: apply(move |s| {
                    let default = s
                        .bindings
                        .get(&id)
                        .map(|binding| binding.default_binding.clone())
                        .ok_or_else(|| format!("No shortcut {id}."))?;
                    if let Some(other) = s.bindings.values().find(|binding| {
                        binding.id != id && !default.is_empty() && binding.current_binding == default
                    }) {
                        return Err(format!(
                            "{default} is {}'s shortcut now. Change that one first.",
                            other.name
                        ));
                    }
                    if let Some(binding) = s.bindings.get_mut(&id) {
                        binding.current_binding = default;
                    }
                    Ok(())
                }),
            }
        }
    })
}

fn read(what: &str, value: &impl Serialize) -> WorkAnswer {
    WorkAnswer::Settings(Box::new(ExternalSettings {
        schema_version: QUERY_SCHEMA_VERSION,
        what: what.to_owned(),
        dry_run: false,
        operation_id: None,
        replayed: false,
        agent: None,
        before: None,
        value: json!(value),
        live: None,
    }))
}

fn write_value(text: &mut String, indent: usize, key: &str, value: &Value) {
    let pad = "  ".repeat(indent);
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            let _ = writeln!(text, "{pad}{key}:");
            for (name, field) in fields {
                write_value(text, indent + 1, name, field);
            }
        }
        Value::Array(items) if items.iter().any(|item| item.is_object()) => {
            let _ = writeln!(text, "{pad}{key}:");
            for item in items {
                write_value(text, indent + 1, "-", item);
            }
        }
        Value::String(scalar) => {
            let _ = writeln!(text, "{pad}{key}: {scalar}");
        }
        other => {
            let _ = writeln!(text, "{pad}{key}: {other}");
        }
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let WorkAnswer::Settings(answer) = answer else {
        return;
    };
    if answer.replayed {
        text.push_str("Already done by an earlier run with this operation id.\n");
    }
    match &answer.before {
        Some(before) => {
            let verb = if answer.dry_run { "Would change" } else { "Changed" };
            let _ = writeln!(text, "{verb} {}.", answer.what);
            write_value(text, 0, "before", before);
            write_value(text, 0, "after", &answer.value);
        }
        None => write_value(text, 0, &answer.what, &answer.value),
    }
    if let Some(live) = answer.live {
        let _ = writeln!(text, "{live}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::evening::EveningLines;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::evening::StoredEveningLines;
    use crate::meeting::store::workflow_core_tests::reviewable_meeting;
    use crate::meeting::types::MeetingSessionId;
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;

    const NOW: i64 = 1_767_000_000_000;
    const DAY: i64 = 24 * 60 * 60 * 1_000;

    /// A library with one open meeting and one whose series is kept on this
    /// Mac.
    async fn library() -> (TempDir, Arc<MeetingStore>, MeetingSessionId, MeetingSessionId) {
        let directory = TempDir::new().unwrap();
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let meetings = MeetingSessionManager::with_parts(
            None,
            Some(directory.path().join("meetings")),
            secrets,
            Arc::new(NoCaptureSources),
        )
        .reading_only();
        let store = meetings.store().await.unwrap();
        let open = reviewable_meeting(&store, "Pricing sync", NOW);
        let kept = reviewable_meeting(&store, "Board prep", NOW - DAY);
        store
            .remember_calendar_facts(
                kept,
                &CalendarEventSummary {
                    event_key: format!("board#{}", NOW - DAY),
                    series_key: "board".to_owned(),
                    title: "Board prep".to_owned(),
                    attendee_count: 2,
                    start_utc_ms: NOW - DAY,
                    end_utc_ms: NOW - DAY + 30 * 60 * 1_000,
                    attendees: Vec::new(),
                    notes: None,
                    calendar_name: None,
                    location: None,
                    url: None,
                    place: None,
                },
            )
            .unwrap();
        store
            .set_series_remote_opt_out(
                &MeetingSeriesRemoteOptOutSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: "board".to_owned(),
                    remote_intelligence_opt_out: true,
                    expected_revision: 0,
                },
                NOW,
            )
            .unwrap();
        (directory, store, open, kept)
    }

    #[test]
    fn the_evening_card_leaves_out_what_a_meeting_kept_on_this_mac_said() {
        tauri::async_runtime::block_on(async {
            let (_directory, store, open, kept) = library().await;
            let day = "2025-12-29";
            let lines = vec!["The board prep ran long.".to_owned()];
            store
                .save_evening_lines(
                    day,
                    &StoredEveningLines {
                        lines: EveningLines {
                            lines: lines.clone(),
                            written_at_utc_ms: NOW,
                            model_id: "local".to_owned(),
                        },
                        meeting_ids: vec![open, kept],
                        dictation_ids: Vec::new(),
                    },
                )
                .unwrap();
            let recording = |session_id| UnfiledRecording {
                session_id,
                title: "New recording".to_owned(),
                started_at_utc_ms: NOW,
                ended_at_utc_ms: None,
                suggestions: Vec::new(),
            };
            let hidden = kept_local(&store).unwrap();
            let card = evening_view(
                &store,
                &hidden,
                day.to_owned(),
                Some(lines),
                true,
                vec![recording(open), recording(kept)],
            )
            .unwrap();
            assert_eq!(card.lines, None);
            assert!(card.lines_left_out);
            let shown: Vec<_> = card.unfiled.iter().map(|recording| recording.session_id).collect();
            assert_eq!(shown, [open]);
        });
    }

    #[test]
    fn the_connections_leave_out_a_series_kept_on_this_mac() {
        tauri::async_runtime::block_on(async {
            let (_directory, store, open, kept) = library().await;
            store
                .save_connection(&Connection {
                    id: "slack".to_owned(),
                    name: "Team Slack".to_owned(),
                    kind: crate::integrations::types::ConnectionKind::SlackWebhook,
                    target: "#team".to_owned(),
                    enabled: true,
                })
                .unwrap();
            for (id, scope) in [
                ("rule-all", SendScope::All),
                ("rule-board", SendScope::Series("board".to_owned())),
                ("rule-pricing", SendScope::Series("pricing".to_owned())),
            ] {
                store
                    .save_connection_rule(&mut crate::integrations::types::AutoSendRule {
                        id: id.to_owned(),
                        connection_id: "slack".to_owned(),
                        scope,
                        enabled: true,
                        created_at_utc_ms: NOW,
                    })
                    .unwrap();
            }
            for (id, session) in [("sent-open", open), ("sent-kept", kept)] {
                store
                    .claim_connection_send(
                        &crate::integrations::types::SendReceipt {
                            id: id.to_owned(),
                            connection_id: "slack".to_owned(),
                            connection_name: "Team Slack".to_owned(),
                            session_id: Some(session.uuid().to_string()),
                            state: SendState::Sent,
                            detail: String::new(),
                            created_at_utc_ms: NOW,
                            undo: None,
                        },
                        None,
                    )
                    .unwrap();
            }
            let listed = connections(&store).unwrap();
            let sent: Vec<_> = listed.receipts.iter().map(|receipt| receipt.id.as_str()).collect();
            assert_eq!(sent, ["sent-open"]);
            let rules: Vec<_> = listed.rules.iter().map(|rule| rule.id.as_str()).collect();
            assert_eq!(rules, ["rule-all", "rule-pricing"]);
            assert!(listed.rules_left_out);
            assert!(!json!(listed).to_string().to_lowercase().contains("board"));
        });
    }

    fn set(key: SettingKey, value: &str, settings: &mut AppSettings) -> Result<(), String> {
        let args = SettingArgs {
            key,
            value: value.to_owned(),
            provider: None,
            change: Change {
                dry_run: true,
                write: WriteArgs {
                    operation_id: None,
                    agent: None,
                },
            },
        };
        let change = setting_change(&args);
        (change.apply)(settings)
    }

    #[test]
    fn an_agent_cannot_turn_on_mail_or_web_reading_through_meeting_prep() {
        let mut settings = settings::get_default_settings();
        let mut asked = serde_json::to_value(&settings.meeting_prep).unwrap();
        asked["alert_minutes"] = json!(10);
        set(SettingKey::MeetingPrep, &asked.to_string(), &mut settings).unwrap();
        assert_eq!(settings.meeting_prep.alert_minutes, 10);

        let mut asked = serde_json::to_value(&settings.meeting_prep).unwrap();
        asked["web_research_enabled"] = json!(!settings.meeting_prep.web_research_enabled);
        let refused = set(SettingKey::MeetingPrep, &asked.to_string(), &mut settings);
        assert!(refused.is_err());
        assert_eq!(
            serde_json::to_value(&settings.meeting_prep).unwrap()["web_research_enabled"],
            json!(!asked["web_research_enabled"].as_bool().unwrap())
        );
    }

    #[test]
    fn a_shortcut_reset_never_takes_a_key_another_shortcut_holds() {
        let mut settings = settings::get_default_settings();
        let (id, other) = {
            let mut ids = settings
                .bindings
                .values()
                .filter(|binding| !binding.default_binding.is_empty())
                .map(|binding| binding.id.clone());
            (ids.next().unwrap(), ids.next().unwrap())
        };
        let default = settings.bindings[&id].default_binding.clone();
        settings.bindings.get_mut(&id).unwrap().current_binding = "f19".to_owned();
        settings.bindings.get_mut(&other).unwrap().current_binding = default;
        let args = SettingResetArgs {
            target: ResetTarget::Shortcut,
            id: Some(id.clone()),
            change: Change {
                dry_run: true,
                write: WriteArgs {
                    operation_id: None,
                    agent: None,
                },
            },
        };
        let change = reset_change(&args).unwrap();
        assert!((change.apply)(&mut settings).is_err());
        assert_eq!(settings.bindings[&id].current_binding, "f19");
    }

    #[test]
    fn an_agent_cannot_send_a_mode_to_the_cloud_or_have_it_run_a_script() {
        let mut settings = settings::get_default_settings();
        modes::ensure_mode_settings(&mut settings);
        let mut asked = settings.modes[0].clone();
        asked.name = "Dictation, renamed".to_owned();
        asked.asr.requested_engine = RequestedEngine::DeepgramNova3;
        asked.delivery.paste_method = PasteMethod::ExternalScript;
        asked.delivery.external_script_path = Some("/private/var/tmp/paste.sh".to_owned());
        save_mode(&asked, &mut settings).unwrap();

        let saved = &settings.modes[0];
        assert_eq!(saved.name, "Dictation, renamed");
        assert_eq!(saved.asr.requested_engine, RequestedEngine::Local);
        assert_eq!(saved.delivery.paste_method, PasteMethod::default());
        assert_eq!(saved.delivery.external_script_path, None);

        asked.id = "agent-made".to_owned();
        save_mode(&asked, &mut settings).unwrap();

        let made = settings.modes.iter().find(|mode| mode.id == "agent-made").unwrap();
        assert_eq!(made.asr.requested_engine, RequestedEngine::Local);
        assert_eq!(made.delivery.paste_method, PasteMethod::default());
        assert_eq!(made.delivery.external_script_path, None);
    }
}
