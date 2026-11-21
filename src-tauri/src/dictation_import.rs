//! Dictation history and vocabulary from other dictation apps.
//!
//! None of these apps documents an export, so each reader follows the files
//! the app keeps on this Mac, as the open-source readers cited beside it read
//! them, and one sample of each lives in `dictation_import/fixtures/`. A
//! reader opens the vendor's files read-only and never writes beside them.
//!
//! Every dictation carries the vendor's own id (or, where it has none, its
//! time), which is what makes a second import of the same app add only what
//! is new.

use crate::audio_toolkit::text::vocabulary_spoken_key;
use crate::managers::history::{AppDictationImport, HistoryManager};
use crate::meeting::import_formats::wispr_timestamp_ms;
use crate::settings::{self, VocabularyEntry};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, State};

/// The largest JSON file a reader holds at once. Aqua Voice keeps about a
/// hundred dictations in its settings file and Willow one per file, so a file
/// past this is not one of theirs.
const MAX_JSON_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DictationApp {
    WisprFlow,
    AquaVoice,
    Typeless,
    WillowVoice,
}

impl DictationApp {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WisprFlow => "wispr_flow",
            Self::AquaVoice => "aqua_voice",
            Self::Typeless => "typeless",
            Self::WillowVoice => "willow_voice",
        }
    }

    pub(crate) fn from_stored(value: &str) -> Option<Self> {
        match value {
            "wispr_flow" => Some(Self::WisprFlow),
            "aqua_voice" => Some(Self::AquaVoice),
            "typeless" => Some(Self::Typeless),
            "willow_voice" => Some(Self::WillowVoice),
            _ => None,
        }
    }
}

/// One dictation as the other app kept it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppDictation {
    /// The app's id for the dictation, stable across imports.
    pub source_key: String,
    /// Seconds since 1970, as history keeps them.
    pub timestamp: i64,
    /// What the app heard.
    pub heard: String,
    /// What the app wrote, when it differs from what it heard.
    pub written: Option<String>,
    pub duration_ms: Option<u64>,
    pub word_count: Option<u64>,
}

/// Everything one app's files hold that Sona can take.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct AppExport {
    pub dictations: Vec<AppDictation>,
    pub vocabulary: Vec<VocabularyEntry>,
    /// Vocabulary rows Sona has no place for: snippets, and replacements
    /// whose written side the app's files do not name.
    pub vocabulary_unsupported: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct DictationImportResult {
    pub dictations_imported: u64,
    /// Already imported, from an earlier run of the same app.
    pub dictations_skipped: u64,
    pub words_added: u64,
    /// Already in the vocabulary, or a kind of entry Sona does not keep.
    pub words_skipped: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DictationImportError {
    /// The path is not that app's data, or its files could not be read.
    Unreadable,
    /// "Dictations to keep" is zero: history is off, so there is nowhere to
    /// put the dictations.
    HistoryOff,
    /// The new rows would take history past "Dictations to keep" by more
    /// rows than the person agreed to lose.
    HistoryWouldRemoveOlder {
        removals: u64,
    },
    Internal,
}

impl std::fmt::Display for DictationImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for DictationImportError {}

impl From<settings::SettingsPersistError> for DictationImportError {
    fn from(_: settings::SettingsPersistError) -> Self {
        Self::Internal
    }
}

/// Bring one app's dictations into history and its words into the global
/// vocabulary. `path` is the app's data folder or the one file in it that
/// holds the data; see [`read_app_export`].
#[tauri::command]
#[specta::specta]
pub async fn import_dictation_app(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    source: DictationApp,
    path: PathBuf,
    removals_allowed: Option<u64>,
) -> Result<DictationImportResult, DictationImportError> {
    let _activity = crate::planning::quiet::foreground_activity();
    let history = history.inner().clone();
    let removals_allowed = removals_allowed.unwrap_or(0);
    let (export, imported) = tokio::task::spawn_blocking(move || {
        let export = read_app_export(source, &path)?;
        let imported = history
            .import_app_dictations(source, &export.dictations, removals_allowed)
            .map_err(|error| {
                log::error!("Dictation import from {source:?} failed: {error}");
                DictationImportError::Internal
            })?;
        Ok::<_, DictationImportError>((export, imported))
    })
    .await
    .map_err(|_| DictationImportError::Internal)??;
    let (dictations_imported, dictations_skipped) = match imported {
        AppDictationImport::Imported { imported, skipped } => (imported, skipped),
        AppDictationImport::HistoryOff => return Err(DictationImportError::HistoryOff),
        AppDictationImport::WouldRemoveOlder { removals } => {
            return Err(DictationImportError::HistoryWouldRemoveOlder { removals })
        }
    };
    let vocabulary = export.vocabulary;
    let unsupported = export.vocabulary_unsupported;
    let (words_added, words_skipped) = settings::try_update_settings(&app, move |settings| {
        let (added, skipped) = merge_vocabulary(&mut settings.custom_words, vocabulary);
        Ok::<_, DictationImportError>((added, skipped.saturating_add(unsupported)))
    })?;
    Ok(DictationImportResult {
        dictations_imported,
        dictations_skipped,
        words_added,
        words_skipped,
    })
}

/// Add the words whose spoken form the vocabulary does not have yet. A word
/// already there keeps what the person wrote for it.
pub(crate) fn merge_vocabulary(
    entries: &mut Vec<VocabularyEntry>,
    incoming: Vec<VocabularyEntry>,
) -> (u64, u64) {
    let mut known: HashSet<String> = entries
        .iter()
        .map(|entry| vocabulary_spoken_key(&entry.spoken))
        .collect();
    let (mut added, mut skipped) = (0_u64, 0_u64);
    for entry in incoming {
        let entry = entry.trim_outer_whitespace();
        let key = vocabulary_spoken_key(&entry.spoken);
        if !entry.is_usable() || key.is_empty() || !known.insert(key) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        entries.push(entry);
        added = added.saturating_add(1);
    }
    (added, skipped)
}

/// Read one app's files. Each app takes its data folder, or the file in it
/// that holds the data: Wispr Flow `flow.sqlite`, Aqua Voice `settings.json`,
/// Typeless `typeless.db`. Willow keeps one file per dictation, so it takes
/// the folder.
pub(crate) fn read_app_export(
    app: DictationApp,
    path: &Path,
) -> Result<AppExport, DictationImportError> {
    let result = match app {
        DictationApp::WisprFlow => read_wispr_flow(&data_file(path, "flow.sqlite")),
        DictationApp::AquaVoice => read_aqua_voice(&data_file(path, "settings.json")),
        DictationApp::Typeless => read_typeless(&data_file(path, "typeless.db")),
        DictationApp::WillowVoice => read_willow_voice(path),
    };
    result.map_err(|reason| {
        log::warn!(
            "Dictation import from {app:?} refused {}: {reason}",
            path.display()
        );
        DictationImportError::Unreadable
    })
}

fn data_file(path: &Path, name: &str) -> PathBuf {
    if path.is_dir() {
        path.join(name)
    } else {
        path.to_path_buf()
    }
}

fn open_read_only(path: &Path) -> Result<Connection, String> {
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_JSON_BYTES {
        return Err(format!("{} is not a readable JSON file", path.display()));
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// The columns of `table` that exist, so a reader can ask an older version's
/// database for a column it never had and get NULL instead of an error.
fn columns(connection: &Connection, table: &str) -> Result<HashSet<String>, String> {
    let mut statement = connection
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .map_err(|error| error.to_string())?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(names)
}

fn column_or_null(present: &HashSet<String>, name: &str) -> String {
    if present.contains(name) {
        format!("\"{name}\"")
    } else {
        "NULL".to_string()
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// What the app heard and what it wrote, folded into history's two columns:
/// the written words only when they differ from the heard ones.
fn heard_and_written(
    heard: Option<String>,
    written: Option<String>,
) -> Option<(String, Option<String>)> {
    match (non_empty(heard), non_empty(written)) {
        (Some(heard), Some(written)) if heard != written => Some((heard, Some(written))),
        (Some(heard), _) => Some((heard, None)),
        (None, Some(written)) => Some((written, None)),
        (None, None) => None,
    }
}

fn seconds_to_ms(seconds: Option<f64>) -> Option<u64> {
    seconds
        .and_then(|seconds| std::time::Duration::try_from_secs_f64(seconds).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn iso_seconds(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|stamp| stamp.timestamp())
}

// ---------------------------------------------------------------------------
// Wispr Flow
// ---------------------------------------------------------------------------

/// Wispr Flow's Sequelize database, `~/Library/Application Support/Wispr
/// Flow/flow.sqlite`: the `History` and `Dictionary` tables.
/// <https://github.com/pedramamini/WisprMCP/blob/main/wispr_flow_database_schema.md>
/// <https://github.com/iamnikolie/wispr-cli> (`internal/store`)
///
/// Only rows the app delivered are dictations (`formatted`, `raw_transcript`),
/// and an archived row is one the person removed. The written words are the
/// person's own edit when there is one, else what Wispr pasted.
fn read_wispr_flow(path: &Path) -> Result<AppExport, String> {
    let connection = open_read_only(path)?;
    let history = columns(&connection, "History")?;
    if !history.contains("transcriptEntityId") || !history.contains("timestamp") {
        return Err("no Wispr Flow History table".to_string());
    }
    let select = format!(
        "SELECT transcriptEntityId, timestamp, {asr}, {formatted}, {edited}, {duration}, {words}
         FROM History
         WHERE COALESCE({archived}, 0) = 0
           AND COALESCE({status}, 'formatted') IN ('formatted', 'raw_transcript')
         ORDER BY timestamp ASC",
        asr = column_or_null(&history, "asrText"),
        formatted = column_or_null(&history, "formattedText"),
        edited = column_or_null(&history, "editedText"),
        duration = column_or_null(&history, "duration"),
        words = column_or_null(&history, "numWords"),
        archived = column_or_null(&history, "isArchived"),
        status = column_or_null(&history, "status"),
    );
    let mut statement = connection
        .prepare(&select)
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut dictations = Vec::new();
    for row in rows {
        let (id, stamp, asr, formatted, edited, duration, words) =
            row.map_err(|error| error.to_string())?;
        let Some(timestamp) = stamp
            .as_deref()
            .and_then(wispr_timestamp_ms)
            .map(|ms| ms.div_euclid(1_000))
        else {
            continue;
        };
        let Some((heard, written)) = heard_and_written(asr, non_empty(edited).or(formatted)) else {
            continue;
        };
        dictations.push(AppDictation {
            source_key: id,
            timestamp,
            heard,
            written,
            duration_ms: seconds_to_ms(duration),
            word_count: words.and_then(|words| u64::try_from(words).ok()),
        });
    }

    let mut export = AppExport {
        dictations,
        ..AppExport::default()
    };
    let dictionary = columns(&connection, "Dictionary")?;
    if dictionary.contains("phrase") {
        let select = format!(
            "SELECT phrase, {replacement}, COALESCE({snippet}, 0)
             FROM Dictionary
             WHERE COALESCE({deleted}, 0) = 0",
            replacement = column_or_null(&dictionary, "replacement"),
            snippet = column_or_null(&dictionary, "isSnippet"),
            deleted = column_or_null(&dictionary, "isDeleted"),
        );
        let mut statement = connection
            .prepare(&select)
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (phrase, replacement, snippet) = row.map_err(|error| error.to_string())?;
            let Some(phrase) = non_empty(phrase) else {
                continue;
            };
            if snippet != 0 {
                export.vocabulary_unsupported = export.vocabulary_unsupported.saturating_add(1);
                continue;
            }
            let written = non_empty(replacement).unwrap_or_else(|| phrase.clone());
            export.vocabulary.push(VocabularyEntry {
                spoken: phrase,
                written,
            });
        }
    }
    Ok(export)
}

// ---------------------------------------------------------------------------
// Aqua Voice
// ---------------------------------------------------------------------------

/// Aqua Voice's `~/Library/Application Support/Aqua Voice/settings.json`.
/// <https://github.com/Infatoshi/phonon> (`macos/Sources/DictationImport.swift`)
/// The dictionary and replacement shapes match Aqua's own API:
/// <https://aquavoice.com/docs/api>
#[derive(Debug, Deserialize)]
struct AquaSettings {
    #[serde(default)]
    dictionary: Vec<String>,
    #[serde(default)]
    replacements: Vec<AquaReplacement>,
    #[serde(default)]
    history: Vec<AquaHistoryItem>,
}

#[derive(Debug, Deserialize)]
struct AquaReplacement {
    from: String,
    to: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AquaHistoryItem {
    kind: Option<String>,
    raw_text: Option<String>,
    content: Option<String>,
    timestamp: Option<String>,
    session_id: Option<serde_json::Value>,
}

fn read_aqua_voice(path: &Path) -> Result<AppExport, String> {
    let settings: AquaSettings = read_json(path)?;
    let dictations = settings
        .history
        .into_iter()
        .filter(|item| {
            item.kind
                .as_deref()
                .is_none_or(|kind| kind == "transcription")
        })
        .filter_map(|item| {
            let stamp = item.timestamp?;
            let timestamp = iso_seconds(&stamp)?;
            let (heard, written) = heard_and_written(item.raw_text, item.content)?;
            // Aqua's entries carry a session number that restarts, so the
            // millisecond stamp is what tells two of them apart.
            let source_key = match item.session_id {
                Some(session) if !session.is_null() => format!("{stamp}#{session}"),
                _ => stamp,
            };
            Some(AppDictation {
                source_key,
                timestamp,
                heard,
                written,
                duration_ms: None,
                word_count: None,
            })
        })
        .collect();
    let vocabulary = settings
        .dictionary
        .into_iter()
        .map(|word| VocabularyEntry {
            spoken: word.clone(),
            written: word,
        })
        .chain(
            settings
                .replacements
                .into_iter()
                .map(|replacement| VocabularyEntry {
                    spoken: replacement.from,
                    written: replacement.to,
                }),
        )
        .collect();
    Ok(AppExport {
        dictations,
        vocabulary,
        vocabulary_unsupported: 0,
    })
}

// ---------------------------------------------------------------------------
// Typeless
// ---------------------------------------------------------------------------

/// Typeless's `~/Library/Application Support/Typeless/typeless.db`: `history`
/// and, from v1.8.0, `history_v2`, merged on `id` with `history_v2` winning.
/// <https://github.com/IcyBlue17/Hoshiro> (`src-tauri/src/importer.rs`)
/// <https://github.com/UllrAI/SayKuku> (`Scripts/export-typeless.py`)
///
/// Typeless keeps its dictionary on its servers only
/// (<https://github.com/ConstantineLiu/typeless-switcher>), so this reader
/// brings history and no words.
fn read_typeless(path: &Path) -> Result<AppExport, String> {
    let connection = open_read_only(path)?;
    let mut merged: BTreeMap<String, AppDictation> = BTreeMap::new();
    let mut found = false;
    for table in ["history", "history_v2"] {
        let present = columns(&connection, table)?;
        if !present.contains("id") || !present.contains("created_at") {
            continue;
        }
        found = true;
        let select = format!(
            "SELECT id, created_at, {refined}, {meta}, {duration}
             FROM {table}
             WHERE COALESCE({status}, 'completed') = 'completed'",
            refined = column_or_null(&present, "refined_text"),
            meta = column_or_null(&present, "mode_meta"),
            duration = column_or_null(&present, "duration"),
            status = column_or_null(&present, "status"),
        );
        let mut statement = connection
            .prepare(&select)
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3).or_else(|_| {
                        row.get::<_, Option<String>>(3)
                            .map(|text| text.map(String::into_bytes))
                    })?,
                    row.get::<_, Option<f64>>(4)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (id, created, refined, meta, duration) = row.map_err(|error| error.to_string())?;
            let Some(timestamp) = created.as_deref().and_then(iso_seconds) else {
                continue;
            };
            let meta = meta
                .and_then(|bytes| serde_json::from_slice::<TypelessModeMeta>(&bytes).ok())
                .and_then(|meta| meta.ai_result);
            let (prompt, meta_refined) = meta
                .map(|result| (result.user_prompt, result.refined_text))
                .unwrap_or_default();
            let written = non_empty(refined).or(non_empty(meta_refined));
            let Some((heard, written)) = heard_and_written(prompt.or(written.clone()), written)
            else {
                continue;
            };
            merged.insert(
                id.clone(),
                AppDictation {
                    source_key: id,
                    timestamp,
                    heard,
                    written,
                    duration_ms: seconds_to_ms(duration),
                    word_count: None,
                },
            );
        }
    }
    if !found {
        return Err("no Typeless history table".to_string());
    }
    let mut dictations: Vec<AppDictation> = merged.into_values().collect();
    dictations.sort_by_key(|dictation| dictation.timestamp);
    Ok(AppExport {
        dictations,
        ..AppExport::default()
    })
}

#[derive(Debug, Deserialize)]
struct TypelessModeMeta {
    ai_result: Option<TypelessAiResult>,
}

#[derive(Debug, Default, Deserialize)]
struct TypelessAiResult {
    user_prompt: Option<String>,
    refined_text: Option<String>,
}

// ---------------------------------------------------------------------------
// Willow Voice
// ---------------------------------------------------------------------------

/// Willow's `~/Library/Application Support/com.seewillow.WillowMac/`: one
/// `Transcripts/<UUID>.json` per dictation, and `Preferences/glossary.json`.
/// <https://github.com/chigbojk/soro> (`docs/DESIGN.md` §6)
///
/// `date` counts seconds from 2001-01-01, Apple's reference date. A shortcut
/// in the glossary (`isReplacement`) is left out: the files' field for what it
/// expands to is not known, and a guessed one would write the wrong words.
const COCOA_EPOCH_UNIX_SECONDS: f64 = 978_307_200.0;
const WILLOW_FAILED_TRANSCRIPT: &str = "ERROR_TRANSCRIBING";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WillowTranscript {
    id: String,
    text: Option<String>,
    recording_duration: Option<f64>,
    date: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WillowGlossaryItem {
    term: String,
    #[serde(default = "enabled")]
    is_enabled: bool,
    #[serde(default)]
    is_replacement: bool,
}

const fn enabled() -> bool {
    true
}

fn read_willow_voice(path: &Path) -> Result<AppExport, String> {
    let transcripts = path.join("Transcripts");
    let glossary = path.join("Preferences").join("glossary.json");
    if !transcripts.is_dir() && !glossary.is_file() {
        return Err("no Willow Transcripts folder or glossary".to_string());
    }
    let mut dictations = Vec::new();
    if transcripts.is_dir() {
        let entries = fs::read_dir(&transcripts).map_err(|error| error.to_string())?;
        for entry in entries {
            let file = entry.map_err(|error| error.to_string())?.path();
            if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            // One unreadable file is one dictation lost, not the import.
            let Ok(transcript) = read_json::<WillowTranscript>(&file) else {
                log::warn!("Skipped an unreadable Willow transcript");
                continue;
            };
            let Some(text) =
                non_empty(transcript.text).filter(|text| text != WILLOW_FAILED_TRANSCRIPT)
            else {
                continue;
            };
            let unix = transcript.date + COCOA_EPOCH_UNIX_SECONDS;
            let Some(timestamp) = unix
                .is_finite()
                .then(|| format!("{:.0}", unix.floor()))
                .and_then(|digits| digits.parse::<i64>().ok())
            else {
                continue;
            };
            dictations.push(AppDictation {
                source_key: transcript.id,
                timestamp,
                heard: text,
                written: None,
                duration_ms: seconds_to_ms(transcript.recording_duration),
                word_count: None,
            });
        }
    }
    dictations.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.source_key.cmp(&b.source_key))
    });
    let mut export = AppExport {
        dictations,
        ..AppExport::default()
    };
    if glossary.is_file() {
        let items: Vec<WillowGlossaryItem> = read_json(&glossary)?;
        for item in items.into_iter().filter(|item| item.is_enabled) {
            if item.is_replacement {
                export.vocabulary_unsupported = export.vocabulary_unsupported.saturating_add(1);
                continue;
            }
            export.vocabulary.push(VocabularyEntry {
                spoken: item.term.clone(),
                written: item.term,
            });
        }
    }
    Ok(export)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/dictation_import/fixtures")
            .join(name)
    }

    /// The SQL fixtures are the vendors' DDL with synthetic rows; each test
    /// builds the database the app would have left on disk.
    fn database_from(sql: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        let connection = Connection::open(dir.path().join("data.db")).expect("open db");
        connection
            .execute_batch(&fs::read_to_string(fixture(sql)).expect("read fixture"))
            .expect("build fixture db");
        dir
    }

    #[test]
    fn wispr_flow_keeps_delivered_rows_and_plain_dictionary_words() {
        let dir = database_from("wispr_flow.sql");
        let export = read_app_export(DictationApp::WisprFlow, &dir.path().join("data.db"))
            .expect("read Wispr Flow");

        assert_eq!(
            export.dictations,
            vec![
                AppDictation {
                    source_key: "a1b2c3d4-0000-4000-8000-000000000001".to_string(),
                    timestamp: 1_794_387_600,
                    heard: "ship it friday".to_string(),
                    written: Some("Ship it Friday.".to_string()),
                    duration_ms: Some(3_500),
                    word_count: Some(3),
                },
                AppDictation {
                    source_key: "a1b2c3d4-0000-4000-8000-000000000004".to_string(),
                    timestamp: 1_794_398_400,
                    heard: "lets meet at noon".to_string(),
                    written: Some("Let's meet at 12:30.".to_string()),
                    duration_ms: Some(2_000),
                    word_count: Some(4),
                },
            ]
        );
        assert_eq!(
            export.vocabulary,
            vec![
                VocabularyEntry {
                    spoken: "Sona".to_string(),
                    written: "Sona".to_string(),
                },
                VocabularyEntry {
                    spoken: "jason".to_string(),
                    written: "JSON".to_string(),
                },
            ]
        );
        assert_eq!(export.vocabulary_unsupported, 1);
    }

    #[test]
    fn aqua_voice_reads_history_dictionary_and_replacements() {
        let export = read_app_export(DictationApp::AquaVoice, &fixture("aqua_voice"))
            .expect("read Aqua Voice");

        assert_eq!(
            export.dictations,
            vec![AppDictation {
                source_key: "2026-07-27T15:54:07.373Z#42".to_string(),
                timestamp: 1_785_167_647,
                heard: "run the triton kernel".to_string(),
                written: Some("Run the Triton kernel.".to_string()),
                duration_ms: None,
                word_count: None,
            }]
        );
        assert_eq!(
            export.vocabulary,
            vec![
                VocabularyEntry {
                    spoken: "Triton".to_string(),
                    written: "Triton".to_string(),
                },
                VocabularyEntry {
                    spoken: " ".to_string(),
                    written: " ".to_string(),
                },
                VocabularyEntry {
                    spoken: "PyTorch".to_string(),
                    written: "PyTorch".to_string(),
                },
                VocabularyEntry {
                    spoken: "jason".to_string(),
                    written: "JSON".to_string(),
                },
            ]
        );
    }

    #[test]
    fn typeless_merges_both_history_tables_and_reads_command_results() {
        let dir = database_from("typeless.sql");
        let export = read_app_export(DictationApp::Typeless, dir.path().join("data.db").as_path())
            .expect("read Typeless");

        assert_eq!(
            export.dictations,
            vec![
                AppDictation {
                    source_key: "old".to_string(),
                    timestamp: 1_767_225_600,
                    heard: "Hi".to_string(),
                    written: None,
                    duration_ms: Some(1_000),
                    word_count: None,
                },
                AppDictation {
                    source_key: "a".to_string(),
                    timestamp: 1_791_572_251,
                    heard: "Hello world.".to_string(),
                    written: None,
                    duration_ms: Some(2_500),
                    word_count: None,
                },
                AppDictation {
                    source_key: "b".to_string(),
                    timestamp: 1_791_572_400,
                    heard: "translate hello to spanish".to_string(),
                    written: Some("Hola".to_string()),
                    duration_ms: Some(1_000),
                    word_count: None,
                },
            ]
        );
        assert!(export.vocabulary.is_empty());
    }

    #[test]
    fn willow_voice_reads_transcripts_and_enabled_terms() {
        let export = read_app_export(DictationApp::WillowVoice, &fixture("willow_voice"))
            .expect("read Willow");

        assert_eq!(
            export.dictations,
            vec![AppDictation {
                source_key: "91286E6C-244A-4028-A947-36FA3E0FDA1B".to_string(),
                timestamp: 1_756_446_630,
                heard: "Send the deck to Ana by Friday.".to_string(),
                written: None,
                duration_ms: Some(12_400),
                word_count: None,
            }]
        );
        assert_eq!(
            export.vocabulary,
            vec![
                VocabularyEntry {
                    spoken: "Sona".to_string(),
                    written: "Sona".to_string(),
                },
                VocabularyEntry {
                    spoken: "Supabase".to_string(),
                    written: "Supabase".to_string(),
                },
            ]
        );
        assert_eq!(export.vocabulary_unsupported, 1);
    }

    #[test]
    fn a_folder_that_is_not_the_app_is_refused() {
        let empty = tempfile::tempdir().expect("temp dir");
        for app in [
            DictationApp::WisprFlow,
            DictationApp::AquaVoice,
            DictationApp::Typeless,
            DictationApp::WillowVoice,
        ] {
            assert_eq!(
                read_app_export(app, empty.path()),
                Err(DictationImportError::Unreadable),
                "{app:?}"
            );
        }
    }

    fn history() -> Connection {
        let mut connection = Connection::open_in_memory().expect("open history");
        rusqlite_migration::Migrations::new(crate::managers::history::MIGRATIONS.to_vec())
            .to_latest(&mut connection)
            .expect("migrate history");
        connection
    }

    fn wispr_dictations() -> Vec<AppDictation> {
        let dir = database_from("wispr_flow.sql");
        read_app_export(DictationApp::WisprFlow, &dir.path().join("data.db"))
            .expect("read Wispr Flow")
            .dictations
    }

    #[test]
    fn a_second_import_adds_nothing_and_rows_keep_their_time_and_source() {
        let mut connection = history();
        let dictations = wispr_dictations();

        let (first, inserted) = HistoryManager::import_app_dictations_with_connection(
            &mut connection,
            DictationApp::WisprFlow,
            &dictations,
            100,
            0,
        )
        .expect("first import");
        assert_eq!(
            first,
            AppDictationImport::Imported {
                imported: 2,
                skipped: 0
            }
        );
        assert_eq!(inserted.len(), 2);

        let (second, inserted) = HistoryManager::import_app_dictations_with_connection(
            &mut connection,
            DictationApp::WisprFlow,
            &dictations,
            100,
            0,
        )
        .expect("second import");
        assert_eq!(
            second,
            AppDictationImport::Imported {
                imported: 0,
                skipped: 2
            }
        );
        assert!(inserted.is_empty());

        let mut statement = connection
            .prepare(
                "SELECT h.timestamp, h.transcription_text, h.post_processed_text, d.source
                 FROM transcription_history h
                 JOIN dictation_import_history d ON d.history_id = h.id
                 ORDER BY h.timestamp",
            )
            .expect("prepare");
        let rows: Vec<(i64, String, Option<String>, String)> = statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(
            rows,
            vec![
                (
                    1_794_387_600,
                    "ship it friday".to_string(),
                    Some("Ship it Friday.".to_string()),
                    "wispr_flow".to_string(),
                ),
                (
                    1_794_398_400,
                    "lets meet at noon".to_string(),
                    Some("Let's meet at 12:30.".to_string()),
                    "wispr_flow".to_string(),
                ),
            ]
        );
    }

    #[test]
    fn an_import_past_the_dictation_limit_waits_for_its_removals_to_be_agreed() {
        let mut connection = history();
        let dictations = wispr_dictations();

        let refused = HistoryManager::import_app_dictations_with_connection(
            &mut connection,
            DictationApp::WisprFlow,
            &dictations,
            1,
            0,
        )
        .expect("refused import");
        assert_eq!(
            refused.0,
            AppDictationImport::WouldRemoveOlder { removals: 1 }
        );
        let off = HistoryManager::import_app_dictations_with_connection(
            &mut connection,
            DictationApp::WisprFlow,
            &dictations,
            0,
            u64::MAX,
        )
        .expect("history off");
        assert_eq!(off.0, AppDictationImport::HistoryOff);
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM transcription_history", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(count, 0);

        let agreed = HistoryManager::import_app_dictations_with_connection(
            &mut connection,
            DictationApp::WisprFlow,
            &dictations,
            1,
            1,
        )
        .expect("agreed import");
        assert_eq!(
            agreed.0,
            AppDictationImport::Imported {
                imported: 2,
                skipped: 0
            }
        );
    }

    #[test]
    fn merging_keeps_the_words_already_there() {
        let mut entries = vec![VocabularyEntry {
            spoken: "jason".to_string(),
            written: "Jason".to_string(),
        }];
        let (added, skipped) = merge_vocabulary(
            &mut entries,
            vec![
                VocabularyEntry {
                    spoken: "Jason".to_string(),
                    written: "JSON".to_string(),
                },
                VocabularyEntry {
                    spoken: "Triton".to_string(),
                    written: "Triton".to_string(),
                },
                VocabularyEntry {
                    spoken: " ".to_string(),
                    written: " ".to_string(),
                },
                VocabularyEntry {
                    spoken: "triton".to_string(),
                    written: "Triton".to_string(),
                },
            ],
        );

        assert_eq!((added, skipped), (1, 3));
        assert_eq!(
            entries,
            vec![
                VocabularyEntry {
                    spoken: "jason".to_string(),
                    written: "Jason".to_string(),
                },
                VocabularyEntry {
                    spoken: "Triton".to_string(),
                    written: "Triton".to_string(),
                },
            ]
        );
    }
}
