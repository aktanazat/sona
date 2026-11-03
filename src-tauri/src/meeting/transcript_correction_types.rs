use super::people_types::PersonId;
use super::types::{MeetingSessionId, SpeakerId, TranscriptSegmentId};
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptCorrectionKind {
    Phrase,
    Name,
    Term,
    Dialect,
    CodeSwitching,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscriptCorrectionContext {
    Person,
    Conversation,
    SurroundingPhrase { phrase: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptCorrectionStatus {
    Active,
    Paused,
    SourceChanged,
    Conflicting,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TranscriptCorrection {
    pub correction_id: Uuid,
    pub person_id: PersonId,
    pub source_session_id: MeetingSessionId,
    pub source_segment_id: TranscriptSegmentId,
    pub source_speaker_id: SpeakerId,
    pub source_start_offset_ns: u64,
    pub original_text: String,
    pub corrected_text: String,
    pub original_phrase: String,
    pub replacement_phrase: String,
    pub source_audio_retained: bool,
    pub kind: TranscriptCorrectionKind,
    pub context: TranscriptCorrectionContext,
    pub enabled: bool,
    pub status: TranscriptCorrectionStatus,
    pub active: bool,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TranscriptCorrectionList {
    pub revision: u64,
    pub rules: Vec<TranscriptCorrection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TranscriptCorrectionSegment {
    pub segment_id: TranscriptSegmentId,
    pub speaker_id: SpeakerId,
    pub start_offset_ns: u64,
    pub person_id: Option<PersonId>,
    pub person_name: Option<String>,
    pub identity_confirmed: bool,
    pub raw_text: String,
    pub corrected_text: String,
    pub explicitly_edited: bool,
    pub audio_retained: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TranscriptCorrectionReview {
    pub session_id: MeetingSessionId,
    pub meeting_revision: u64,
    pub correction_revision: u64,
    pub can_edit: bool,
    pub segments: Vec<TranscriptCorrectionSegment>,
    pub rules: Vec<TranscriptCorrection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct TranscriptCorrectionTeachRequest {
    pub person_id: PersonId,
    pub session_id: MeetingSessionId,
    pub segment_id: TranscriptSegmentId,
    pub expected_meeting_revision: u64,
    pub expected_correction_revision: u64,
    pub original_phrase: String,
    pub replacement_phrase: String,
    pub kind: TranscriptCorrectionKind,
    pub context: TranscriptCorrectionContext,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct TranscriptCorrectionUpdateRequest {
    pub correction_id: Uuid,
    pub person_id: PersonId,
    pub expected_correction_revision: u64,
    pub original_phrase: String,
    pub replacement_phrase: String,
    pub kind: TranscriptCorrectionKind,
    pub context: TranscriptCorrectionContext,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct TranscriptCorrectionRemoveRequest {
    pub correction_id: Uuid,
    pub person_id: PersonId,
    pub expected_correction_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TranscriptCorrectionChangedSession {
    pub session_id: MeetingSessionId,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TranscriptCorrectionMutationResult {
    pub corrections: TranscriptCorrectionList,
    pub changed_sessions: Vec<TranscriptCorrectionChangedSession>,
}
