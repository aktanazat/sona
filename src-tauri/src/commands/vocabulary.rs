use crate::audio_toolkit::text::vocabulary_spoken_key;
use crate::settings::{self, AppSettings, EmojiReplacement, ReplacementRule, VocabularyEntry};
use csv::{ReaderBuilder, StringRecord, WriterBuilder};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VocabularyScope {
    Global,
    CurrentMode,
    Mode { mode_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct VocabularyCsvPreview {
    pub total_rows: usize,
    pub valid_rows: usize,
    pub invalid_rows: usize,
    pub duplicate_rows: usize,
    pub conflict_rows: usize,
    pub can_apply: bool,
    pub entries: Vec<VocabularyEntry>,
}

#[derive(Clone, Debug)]
struct ParsedCsv {
    total_rows: usize,
    invalid_rows: usize,
    entries: Vec<VocabularyEntry>,
}

fn normalize_entry(entry: VocabularyEntry) -> Result<VocabularyEntry, String> {
    let entry = entry.trim_outer_whitespace();
    if !entry.is_usable() {
        return Err("Vocabulary pairs need both a spoken phrase and written text".to_string());
    }
    if vocabulary_spoken_key(&entry.spoken).is_empty() {
        return Err("Vocabulary spoken phrases need at least one letter or number".to_string());
    }
    Ok(entry)
}

fn normalize_entries(entries: Vec<VocabularyEntry>) -> Result<Vec<VocabularyEntry>, String> {
    let mut seen_spoken = HashSet::with_capacity(entries.len());
    let mut normalized = Vec::with_capacity(entries.len());

    for entry in entries {
        let entry = normalize_entry(entry)?;
        let spoken_key = vocabulary_spoken_key(&entry.spoken);
        if !seen_spoken.insert(spoken_key) {
            return Err(
                "Vocabulary entries need unique spoken phrases after normalization".to_string(),
            );
        }
        normalized.push(entry);
    }

    Ok(normalized)
}

fn normalize_emoji_replacement(replacement: EmojiReplacement) -> Result<EmojiReplacement, String> {
    let replacement = replacement.trim_outer_whitespace();
    if !replacement.is_usable() {
        return Err("Emoji replacements need both a spoken phrase and emoji text".to_string());
    }
    Ok(replacement)
}

fn parse_csv_record(record: &StringRecord) -> Option<VocabularyEntry> {
    if record.len() != 2 {
        return None;
    }
    normalize_entry(VocabularyEntry {
        spoken: record.get(0)?.trim().to_string(),
        written: record.get(1)?.trim().to_string(),
    })
    .ok()
}

fn parse_vocabulary_csv(csv_text: &str) -> ParsedCsv {
    let csv_text = csv_text.strip_prefix('\u{feff}').unwrap_or(csv_text);
    let mut reader = ReaderBuilder::new()
        .has_headers(false)
        .flexible(false)
        .from_reader(csv_text.as_bytes());
    let mut parsed = ParsedCsv {
        total_rows: 0,
        invalid_rows: 0,
        entries: Vec::new(),
    };
    let mut first_data_record = true;

    for record in reader.records() {
        match record {
            Ok(record)
                if first_data_record
                    && record.len() == 2
                    && record.get(0).is_some_and(|value| value.trim() == "spoken")
                    && record.get(1).is_some_and(|value| value.trim() == "written") =>
            {
                first_data_record = false;
            }
            Ok(record) => {
                first_data_record = false;
                parsed.total_rows += 1;
                match parse_csv_record(&record) {
                    Some(entry) => parsed.entries.push(entry),
                    None => parsed.invalid_rows += 1,
                }
            }
            Err(_) => {
                first_data_record = false;
                parsed.total_rows += 1;
                parsed.invalid_rows += 1;
            }
        }
    }

    parsed
}

fn pair_key(entry: &VocabularyEntry) -> (String, String) {
    (vocabulary_spoken_key(&entry.spoken), entry.written.clone())
}

fn preview_vocabulary_csv_for_entries(
    csv_text: &str,
    existing: &[VocabularyEntry],
) -> VocabularyCsvPreview {
    let parsed = parse_vocabulary_csv(csv_text);
    let existing_pairs: HashSet<(String, String)> = existing.iter().map(pair_key).collect();
    let existing_writes_by_spoken: HashMap<String, HashSet<String>> =
        existing.iter().fold(HashMap::new(), |mut values, entry| {
            values
                .entry(vocabulary_spoken_key(&entry.spoken))
                .or_default()
                .insert(entry.written.clone());
            values
        });

    let mut duplicate_rows = 0;
    let mut seen_pairs = HashSet::new();
    let mut unique_entries = Vec::with_capacity(parsed.entries.len());
    for entry in parsed.entries {
        let key = pair_key(&entry);
        if existing_pairs.contains(&key) || !seen_pairs.insert(key) {
            duplicate_rows += 1;
        } else {
            unique_entries.push(entry);
        }
    }

    let incoming_writes_by_spoken: HashMap<String, HashSet<String>> =
        unique_entries
            .iter()
            .fold(HashMap::new(), |mut values, entry| {
                values
                    .entry(vocabulary_spoken_key(&entry.spoken))
                    .or_default()
                    .insert(entry.written.clone());
                values
            });

    let mut conflicts = HashSet::new();
    for entry in &unique_entries {
        let spoken_key = vocabulary_spoken_key(&entry.spoken);
        let conflicts_with_existing =
            existing_writes_by_spoken
                .get(&spoken_key)
                .is_some_and(|written_forms| {
                    written_forms
                        .iter()
                        .any(|written| written != &entry.written)
                });
        let conflicts_with_import = incoming_writes_by_spoken
            .get(&spoken_key)
            .is_some_and(|written_forms| written_forms.len() > 1);
        if conflicts_with_existing || conflicts_with_import {
            conflicts.insert(pair_key(entry));
        }
    }

    let entries: Vec<_> = unique_entries
        .into_iter()
        .filter(|entry| !conflicts.contains(&pair_key(entry)))
        .collect();
    let conflict_rows = conflicts.len();
    let can_apply = parsed.invalid_rows == 0 && duplicate_rows == 0 && conflict_rows == 0;

    VocabularyCsvPreview {
        total_rows: parsed.total_rows,
        valid_rows: entries.len(),
        invalid_rows: parsed.invalid_rows,
        duplicate_rows,
        conflict_rows,
        can_apply,
        entries,
    }
}

fn apply_vocabulary_csv_to_entries(
    entries: &mut Vec<VocabularyEntry>,
    csv_text: &str,
) -> Result<Vec<VocabularyEntry>, String> {
    let preview = preview_vocabulary_csv_for_entries(csv_text, entries);
    if !preview.can_apply {
        return Err(format!(
            "CSV cannot be applied: {} invalid, {} duplicate, {} conflicting rows",
            preview.invalid_rows, preview.duplicate_rows, preview.conflict_rows
        ));
    }

    entries.extend(preview.entries);
    Ok(entries.clone())
}

fn entries_for_scope<'a>(
    settings: &'a AppSettings,
    scope: &VocabularyScope,
) -> Result<&'a [VocabularyEntry], String> {
    match scope {
        VocabularyScope::Global => Ok(&settings.custom_words),
        VocabularyScope::CurrentMode => settings
            .modes
            .iter()
            .find(|mode| mode.id == settings.active_mode_id)
            .map(|mode| mode.asr.custom_words.as_slice())
            .ok_or_else(|| "The active mode no longer exists".to_string()),
        VocabularyScope::Mode { mode_id } => settings
            .modes
            .iter()
            .find(|mode| mode.id == *mode_id)
            .map(|mode| mode.asr.custom_words.as_slice())
            .ok_or_else(|| format!("Mode '{mode_id}' does not exist")),
    }
}

fn entries_for_scope_mut<'a>(
    settings: &'a mut AppSettings,
    scope: &VocabularyScope,
) -> Result<&'a mut Vec<VocabularyEntry>, String> {
    match scope {
        VocabularyScope::Global => Ok(&mut settings.custom_words),
        VocabularyScope::CurrentMode => settings
            .modes
            .iter_mut()
            .find(|mode| mode.id == settings.active_mode_id)
            .map(|mode| &mut mode.asr.custom_words)
            .ok_or_else(|| "The active mode no longer exists".to_string()),
        VocabularyScope::Mode { mode_id } => settings
            .modes
            .iter_mut()
            .find(|mode| mode.id == *mode_id)
            .map(|mode| &mut mode.asr.custom_words)
            .ok_or_else(|| format!("Mode '{mode_id}' does not exist")),
    }
}

fn touch_mode_revision(settings: &mut AppSettings, scope: &VocabularyScope) {
    if !matches!(scope, VocabularyScope::Global) {
        settings.modes_revision = settings.modes_revision.saturating_add(1);
    }
}

/// Upserts one spoken form. Shared with the learning feed's accept path so
/// "learned a word" means exactly one thing wherever it is triggered from.
pub(super) fn add_correction_to_entries(
    entries: &mut Vec<VocabularyEntry>,
    entry: VocabularyEntry,
) -> VocabularyEntry {
    let spoken_key = vocabulary_spoken_key(&entry.spoken);
    if let Some(existing) = entries
        .iter_mut()
        .find(|existing| vocabulary_spoken_key(&existing.spoken) == spoken_key)
    {
        *existing = entry.clone();
    } else {
        entries.push(entry.clone());
    }
    entry
}

fn export_vocabulary_entries(entries: &[VocabularyEntry]) -> Result<String, String> {
    let mut writer = WriterBuilder::new()
        .has_headers(false)
        .from_writer(Vec::new());
    writer
        .write_record(["spoken", "written"])
        .map_err(|error| format!("Failed to write CSV header: {error}"))?;
    for entry in entries {
        writer
            .write_record([&entry.spoken, &entry.written])
            .map_err(|error| format!("Failed to write CSV row: {error}"))?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| format!("Failed to finish CSV export: {error}"))?;
    String::from_utf8(bytes).map_err(|error| format!("CSV writer returned invalid UTF-8: {error}"))
}

#[tauri::command]
#[specta::specta]
pub fn list_vocabulary_entries(
    app: AppHandle,
    scope: VocabularyScope,
) -> Result<Vec<VocabularyEntry>, String> {
    let settings = settings::get_settings(&app);
    Ok(entries_for_scope(&settings, &scope)?.to_vec())
}

#[tauri::command]
#[specta::specta]
pub fn update_vocabulary_entries(
    app: AppHandle,
    scope: VocabularyScope,
    entries: Vec<VocabularyEntry>,
) -> Result<Vec<VocabularyEntry>, String> {
    let entries = normalize_entries(entries)?;
    settings::try_update_settings(&app, |settings| {
        *entries_for_scope_mut(settings, &scope)? = entries.clone();
        touch_mode_revision(settings, &scope);
        Ok::<_, String>(entries.clone())
    })
}

#[tauri::command]
#[specta::specta]
pub fn preview_vocabulary_csv(
    app: AppHandle,
    scope: VocabularyScope,
    csv_text: String,
) -> Result<VocabularyCsvPreview, String> {
    let settings = settings::get_settings(&app);
    Ok(preview_vocabulary_csv_for_entries(
        &csv_text,
        entries_for_scope(&settings, &scope)?,
    ))
}

#[tauri::command]
#[specta::specta]
pub fn apply_vocabulary_csv(
    app: AppHandle,
    scope: VocabularyScope,
    csv_text: String,
) -> Result<Vec<VocabularyEntry>, String> {
    settings::try_update_settings(&app, |settings| {
        let updated =
            apply_vocabulary_csv_to_entries(entries_for_scope_mut(settings, &scope)?, &csv_text)?;
        touch_mode_revision(settings, &scope);
        Ok::<_, String>(updated)
    })
}

#[tauri::command]
#[specta::specta]
pub fn export_vocabulary_csv(app: AppHandle, scope: VocabularyScope) -> Result<String, String> {
    let settings = settings::get_settings(&app);
    export_vocabulary_entries(entries_for_scope(&settings, &scope)?)
}

#[tauri::command]
#[specta::specta]
pub fn update_emoji_replacements(
    app: AppHandle,
    replacements: Vec<EmojiReplacement>,
) -> Result<Vec<EmojiReplacement>, String> {
    let replacements = replacements
        .into_iter()
        .map(normalize_emoji_replacement)
        .collect::<Result<Vec<_>, _>>()?;
    settings::update_settings(&app, |settings| {
        settings.emoji_replacements = replacements.clone();
    })?;
    Ok(replacements)
}

#[tauri::command]
#[specta::specta]
pub fn update_emoji_replacements_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.emoji_replacements_enabled = enabled;
    })?;
    Ok(())
}

/// The only correction-learning write path. It changes one user-chosen scope
/// after an explicit history action; no history read, keypress, or background
/// event calls this helper.
#[tauri::command]
#[specta::specta]
pub fn add_vocabulary_correction(
    app: AppHandle,
    spoken: String,
    written: String,
    scope: VocabularyScope,
) -> Result<VocabularyEntry, String> {
    let entry = normalize_entry(VocabularyEntry { spoken, written })?;
    let saved = settings::try_update_settings(&app, |settings| {
        let saved = add_correction_to_entries(entries_for_scope_mut(settings, &scope)?, entry);
        touch_mode_revision(settings, &scope);
        Ok::<_, String>(saved)
    })?;
    // A human rewrote a dictation. That is the only kind of delta the
    // correction loop learns from, and this is where one happens.
    crate::meeting::learning::notify_dictation_corrected(&app, &saved.spoken, &saved.written);
    Ok(saved)
}

/// Normalizes a submitted rule set: outer whitespace trimmed, rules that could
/// never fire dropped, and the whole list capped so a paste accident cannot
/// make every transcription pay for thousands of dead rules.
///
/// Order is preserved. The matcher already prefers the longest match at a given
/// position, so list order is presentation, not precedence.
fn normalize_replacement_rules(rules: Vec<ReplacementRule>) -> Vec<ReplacementRule> {
    rules
        .into_iter()
        .map(ReplacementRule::trim_outer_whitespace)
        .filter(ReplacementRule::is_usable)
        .take(MAX_REPLACEMENT_RULES)
        .collect()
}

/// A ceiling high enough that no real symbol library reaches it and low enough
/// that the per-character scan stays cheap.
const MAX_REPLACEMENT_RULES: usize = 500;

#[tauri::command]
#[specta::specta]
pub fn get_text_replacements(app: AppHandle) -> Vec<ReplacementRule> {
    settings::get_settings(&app).replacements_rules
}

#[tauri::command]
#[specta::specta]
pub fn save_text_replacements(
    app: AppHandle,
    rules: Vec<ReplacementRule>,
) -> Result<Vec<ReplacementRule>, String> {
    let normalized = normalize_replacement_rules(rules);
    settings::update_settings(&app, |settings| {
        settings.replacements_rules = normalized.clone();
    })?;
    Ok(normalized)
}

/// Restores the shipped starter library, discarding user edits. The frontend
/// confirms first; this command is the destructive half.
#[tauri::command]
#[specta::specta]
pub fn reset_text_replacements(app: AppHandle) -> Result<Vec<ReplacementRule>, String> {
    let defaults = crate::settings::default_replacement_rules();
    settings::update_settings(&app, |settings| {
        settings.replacements_rules = defaults.clone();
    })?;
    Ok(defaults)
}

#[tauri::command]
#[specta::specta]
pub fn update_text_replacements_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.replacements_enabled = enabled;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_spoken_edits_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.spoken_edits_enabled = enabled;
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::DEFAULT_MODE_ID;

    fn entry(spoken: &str, written: &str) -> VocabularyEntry {
        VocabularyEntry {
            spoken: spoken.to_string(),
            written: written.to_string(),
        }
    }

    #[test]
    fn csv_round_trip_preserves_quoted_unicode_pairs() {
        let entries = vec![
            entry("north star", "Northstar"),
            entry("hello, world", "你好, world"),
        ];
        let csv = export_vocabulary_entries(&entries).unwrap();
        let preview = preview_vocabulary_csv_for_entries(&csv, &[]);

        assert!(preview.can_apply);
        assert_eq!(preview.entries, entries);
        assert_eq!(preview.invalid_rows, 0);
        assert_eq!(preview.duplicate_rows, 0);
        assert_eq!(preview.conflict_rows, 0);
    }

    #[test]
    fn csv_preview_counts_invalid_duplicate_and_conflicting_rows() {
        let existing = vec![entry("open ai", "OpenAI")];
        let csv = "spoken,written\nopen ai,OpenAI\nopen ai,Open Ai Inc\nnew term,NewTerm\nnew term,NewTerm\nmissing,\n";
        let preview = preview_vocabulary_csv_for_entries(csv, &existing);

        assert!(!preview.can_apply);
        assert_eq!(preview.valid_rows, 1);
        assert_eq!(preview.invalid_rows, 1);
        assert_eq!(preview.duplicate_rows, 2);
        assert_eq!(preview.conflict_rows, 1);
        assert_eq!(preview.entries, vec![entry("new term", "NewTerm")]);
    }

    #[test]
    fn csv_preview_uses_normalized_spoken_conflicts() {
        let existing = vec![entry("open ai", "OpenAI")];
        let preview = preview_vocabulary_csv_for_entries(
            "spoken,written
open-ai,Open AI Inc.
",
            &existing,
        );

        assert!(!preview.can_apply);
        assert_eq!(preview.conflict_rows, 1);
        assert!(preview.entries.is_empty());
    }

    #[test]
    fn csv_preview_treats_normalized_equivalent_pairs_as_duplicates() {
        let existing = vec![entry("open ai", "OpenAI")];
        let preview = preview_vocabulary_csv_for_entries(
            "spoken,written
open-ai,OpenAI
",
            &existing,
        );

        assert!(!preview.can_apply);
        assert_eq!(preview.duplicate_rows, 1);
        assert_eq!(preview.conflict_rows, 0);
        assert!(preview.entries.is_empty());
    }

    #[test]
    fn normalized_entries_reject_ambiguous_or_unmatchable_spoken_forms() {
        let conflict = normalize_entries(vec![
            entry("Open AI", "OpenAI"),
            entry("open-ai", "Open AI Inc."),
        ])
        .unwrap_err();
        assert!(conflict.contains("unique spoken"));

        let invalid = normalize_entries(vec![entry("---", "Dash")]).unwrap_err();
        assert!(invalid.contains("letter or number"));
    }

    #[test]
    fn csv_apply_is_atomic_when_any_row_is_invalid() {
        let mut entries = vec![entry("existing", "Existing")];
        let before = entries.clone();

        let error = apply_vocabulary_csv_to_entries(
            &mut entries,
            "spoken,written\nnew phrase,NewPhrase\nmissing,\n",
        )
        .unwrap_err();

        assert!(error.contains("cannot be applied"));
        assert_eq!(entries, before);
    }

    #[test]
    fn correction_mutates_only_the_requested_scope() {
        let mut settings = settings::get_default_settings();
        let global = VocabularyScope::Global;
        let current_mode = VocabularyScope::CurrentMode;
        let global_entry = entry("glob al", "Global");
        let mode_entry = entry("mode only", "ModeOnly");

        add_correction_to_entries(
            entries_for_scope_mut(&mut settings, &global).unwrap(),
            global_entry.clone(),
        );
        add_correction_to_entries(
            entries_for_scope_mut(&mut settings, &current_mode).unwrap(),
            mode_entry.clone(),
        );

        assert!(settings.custom_words.contains(&global_entry));
        let active = settings
            .modes
            .iter()
            .find(|mode| mode.id == DEFAULT_MODE_ID)
            .unwrap();
        assert!(active.asr.custom_words.contains(&mode_entry));
        assert!(!active.asr.custom_words.contains(&global_entry));
    }

    #[test]
    fn correction_upserts_by_normalized_spoken_form() {
        let mut entries = vec![entry("north star", "Northstar")];
        let saved = add_correction_to_entries(&mut entries, entry("north-star", "North Star"));

        assert_eq!(saved, entry("north-star", "North Star"));
        assert_eq!(entries, vec![entry("north-star", "North Star")]);
    }
}
