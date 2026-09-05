//! The two commands the learning feed uses.
//!
//! Accepting a suggestion is the only place a loop touches real settings, and it
//! happens here rather than in the store: the store owns evidence and decision
//! memory, settings own rules, vocabulary and modes, and neither reaches into
//! the other.
//!
//! Order matters. The settings write happens first; the decision is recorded
//! after it succeeded. A failure between the two leaves the rule written and the
//! candidate unanswered, and every miner excludes what settings already covers,
//! so the candidate is offered at most once more and then answers itself. That
//! is why there is no compensation machinery here.

use crate::meeting::learning_types::{
    LearningDecisionRequest, LearningDecisionStatus, LearningSuggestion, LearningSuggestionsResult,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingCommandError;
use crate::settings::{self, ReplacementRule, VocabularyEntry};
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
#[specta::specta]
pub async fn learning_suggestions(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<LearningSuggestionsResult, MeetingCommandError> {
    manager.learning_suggestions().await
}

/// Answers one suggestion: accept it into real settings, or dismiss it forever.
#[tauri::command]
#[specta::specta]
pub async fn learning_decide(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: LearningDecisionRequest,
) -> Result<LearningSuggestionsResult, MeetingCommandError> {
    if request.status == LearningDecisionStatus::Accepted {
        // A vocabulary term has no stored suggestion: its candidates are
        // computed live from meeting transcripts, and the vocabulary editor the
        // reader is looking at owns the write. Everything else carries the
        // payload that says what to write.
        if let Some(suggestion) = manager.learning_suggestion(&request).await? {
            accept(&app, &suggestion)?;
        }
    }
    manager.decide_learning_suggestion(&request).await
}

/// Performs the settings write one acceptance implies.
fn accept(app: &AppHandle, suggestion: &LearningSuggestion) -> Result<(), MeetingCommandError> {
    match suggestion {
        LearningSuggestion::SpokenPunctuation { spoken, written } => {
            let rule = ReplacementRule {
                spoken: spoken.clone(),
                written: written.clone(),
                enabled: true,
            }
            .trim_outer_whitespace();
            if !rule.is_usable() {
                return Err(MeetingCommandError::InvalidRequest);
            }
            settings::update_settings(app, |settings| {
                let already = settings
                    .replacements_rules
                    .iter()
                    .any(|existing| existing.spoken.eq_ignore_ascii_case(&rule.spoken));
                if !already {
                    settings.replacements_rules.push(rule.clone());
                }
            })
            .map_err(|_| MeetingCommandError::StorageUnavailable)?;
            Ok(())
        }
        LearningSuggestion::VocabularyCorrection { spoken, written } => {
            let entry = VocabularyEntry {
                spoken: spoken.clone(),
                written: written.clone(),
            }
            .trim_outer_whitespace();
            if !entry.is_usable() {
                return Err(MeetingCommandError::InvalidRequest);
            }
            settings::update_settings(app, |settings| {
                super::vocabulary::add_correction_to_entries(
                    &mut settings.custom_words,
                    entry.clone(),
                );
            })
            .map_err(|_| MeetingCommandError::StorageUnavailable)?;
            Ok(())
        }
        LearningSuggestion::ModeHabit { mode_id, .. } => {
            crate::modes::set_active_mode(app.clone(), mode_id.clone())
                .map(|_| ())
                .map_err(|_| MeetingCommandError::InvalidRequest)
        }
        // Advice is an observation. It is dismissible, never acceptable, and the
        // feed offers no accept button for it.
        LearningSuggestion::CaptureAdvice { .. } => Err(MeetingCommandError::InvalidRequest),
    }
}
