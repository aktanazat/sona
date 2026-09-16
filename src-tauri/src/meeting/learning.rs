//! What the learning loops are allowed to see outside the meeting store.
//!
//! [`crate::meeting::store::learning::LearningInputs`] is the whole boundary: the
//! dictation corpus, read-only and bounded, plus the live settings a suggestion
//! has to be checked against. This module is its one production implementation,
//! and [`no_inputs`] is what callers use when the app is not there to ask —
//! a headless run, or a dispatch that happens before managers exist.

use super::learning_types::SeriesPrimingBlob;
use super::store::learning::LearningInputs;
use crate::audio_toolkit::text::vocabulary_spoken_key;
use crate::managers::history::{DictationRunRow, HistoryManager};
use crate::modes::AsrPlan;
use crate::settings::{get_settings, ReplacementRule, VocabularyEntry};
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// The seams that wake the learning loops from outside the meeting module.
///
/// Both are fire-and-forget: a loop that does not run now runs on the next
/// signal, and the dictation pipeline should not wait on a background miner.
pub(crate) fn notify_dictation_history_changed(app: &AppHandle) {
    let Some(manager) = app.try_state::<Arc<super::session::MeetingSessionManager>>() else {
        return;
    };
    let manager = Arc::clone(&manager);
    tauri::async_runtime::spawn(async move {
        manager.record_dictation_corpus_swept().await;
    });
}

/// Records the rewrite a human just performed on a dictation.
///
/// Not a filter: the store works out which words a passage changed. Its one
/// caller narrows the pair before calling for a different reason, so that a
/// passage it cannot bound never reaches an event payload at all.
///
/// macOS-only because its one caller is: the destination observer in
/// [`crate::context::macos`] is the only thing that produces a dictation
/// correction. Nothing on the other platforms has an editor to watch.
#[cfg(target_os = "macos")]
pub(crate) fn notify_dictation_corrected(app: &AppHandle, spoken: &str, written: &str) {
    let (spoken, written) = (spoken.to_owned(), written.to_owned());
    let Some(manager) = app.try_state::<Arc<super::session::MeetingSessionManager>>() else {
        return;
    };
    let manager = Arc::clone(&manager);
    tauri::async_runtime::spawn(async move {
        manager.record_dictation_correction(spoken, written).await;
    });
}

/// Biases one session's transcription with its priming blob.
///
/// The entries go into the run's own [`AsrPlan`], which is a per-run value built
/// from settings and thrown away afterwards. Nothing here reaches shared
/// vocabulary, and the transcript this produces is also what the notes and
/// ledger prompts later read, so priming transcription is what primes the
/// rewrite.
pub(crate) fn apply_series_priming(plan: &mut AsrPlan, blob: &SeriesPrimingBlob) {
    let mut known = plan
        .custom_words
        .iter()
        .map(|entry| vocabulary_spoken_key(&entry.spoken))
        .collect::<HashSet<_>>();
    for term in blob.terms.iter().chain(blob.participants.iter()) {
        let term = term.trim();
        let key = vocabulary_spoken_key(term);
        if key.is_empty() || !known.insert(key) {
            continue;
        }
        plan.custom_words.push(VocabularyEntry {
            spoken: term.to_string(),
            written: term.to_string(),
        });
    }
}

/// Reads the corpus through the history manager and settings through the app.
pub(crate) struct AppLearningInputs {
    history: Arc<HistoryManager>,
    app: AppHandle,
}

impl AppLearningInputs {
    /// `None` when there is no app handle or the history manager is not
    /// registered yet, which is a real startup state rather than a failure.
    pub(crate) fn resolve(app: Option<&AppHandle>) -> Option<Self> {
        let app = app?;
        let history = app.try_state::<Arc<HistoryManager>>()?;
        Some(Self {
            history: Arc::clone(&history),
            app: app.clone(),
        })
    }
}

impl LearningInputs for AppLearningInputs {
    fn dictation_runs_after(&self, after: i64, limit: usize) -> Vec<DictationRunRow> {
        match self.history.dictation_runs_after(after, limit) {
            Ok(rows) => rows,
            Err(error) => {
                // A locked history database is the ordinary startup state, not a
                // reason to fail a mining pass: the next sweep reads the same
                // slice, because the cursor only advances over rows it saw.
                log::debug!("learning corpus unavailable: {error}");
                Vec::new()
            }
        }
    }

    fn replacement_rules(&self) -> Vec<ReplacementRule> {
        get_settings(&self.app).replacements_rules
    }

    fn known_vocabulary(&self) -> Vec<String> {
        super::workflow_engine::known_vocabulary(Some(&self.app))
    }

    fn mode_display_name(&self, mode_id: &str) -> Option<String> {
        get_settings(&self.app)
            .modes
            .into_iter()
            .find(|mode| mode.id == mode_id)
            .map(|mode| mode.name)
    }

    fn active_mode_id(&self) -> Option<String> {
        let settings = get_settings(&self.app);
        crate::modes::active_mode(&settings).map(|mode| mode.id.clone())
    }
}

/// A corpus that is empty and settings that claim nothing.
///
/// Every loop treats an empty slice as "nothing new", so a pass with these
/// inputs advances no cursor and suggests nothing. That is the correct answer
/// when there is nobody to ask, and it is not a silent fallback for a failure:
/// the only callers are paths that genuinely have no app handle.
pub(crate) fn no_inputs() -> impl LearningInputs {
    NoInputs
}

struct NoInputs;

impl LearningInputs for NoInputs {
    fn dictation_runs_after(&self, _after: i64, _limit: usize) -> Vec<DictationRunRow> {
        Vec::new()
    }

    fn replacement_rules(&self) -> Vec<ReplacementRule> {
        Vec::new()
    }

    fn known_vocabulary(&self) -> Vec<String> {
        Vec::new()
    }

    fn mode_display_name(&self, _mode_id: &str) -> Option<String> {
        None
    }

    fn active_mode_id(&self) -> Option<String> {
        None
    }
}
