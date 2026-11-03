use super::{phrase_at, StoreError};
use crate::meeting::transcript_correction_types::{
    TranscriptCorrection, TranscriptCorrectionContext, TranscriptCorrectionKind,
};
use crate::meeting::types::MeetingSessionId;

pub(super) fn validate(
    context: &TranscriptCorrectionContext,
    kind: TranscriptCorrectionKind,
    source: &str,
    original: &str,
) -> Result<(), StoreError> {
    match context {
        TranscriptCorrectionContext::SurroundingPhrase { phrase } => {
            if phrase.is_empty()
                || phrase.trim() != phrase
                || phrase.chars().count() > 512
                || phrase.chars().any(char::is_control)
                || phrase == original
                || !contains_phrase(source, phrase)
            {
                return Err(StoreError::Invalid);
            }
        }
        TranscriptCorrectionContext::Person => {
            if matches!(
                kind,
                TranscriptCorrectionKind::Dialect | TranscriptCorrectionKind::CodeSwitching
            ) {
                return Err(StoreError::Invalid);
            }
        }
        TranscriptCorrectionContext::Conversation => {}
    }
    Ok(())
}

pub(super) fn matches(
    rule: &TranscriptCorrection,
    session_id: MeetingSessionId,
    text: &str,
) -> bool {
    match &rule.context {
        TranscriptCorrectionContext::Person => true,
        TranscriptCorrectionContext::Conversation => rule.source_session_id == session_id,
        TranscriptCorrectionContext::SurroundingPhrase { phrase } => contains_phrase(text, phrase),
    }
}

pub(super) fn contains_phrase(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase)
        .any(|(offset, _)| phrase_at(text, offset, phrase))
}

pub(super) fn overlaps(
    left: &TranscriptCorrectionContext,
    left_session: MeetingSessionId,
    right: &TranscriptCorrectionContext,
    right_session: MeetingSessionId,
) -> bool {
    !matches!((left, right),
        (TranscriptCorrectionContext::Conversation, TranscriptCorrectionContext::Conversation)
            if left_session != right_session)
}
