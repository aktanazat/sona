use super::context::ConversationLocation;
use super::types::{MeetingSessionId, TranscriptSegmentId};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ConversationAudioClip {
    pub session_id: MeetingSessionId,
    pub segment_id: Option<TranscriptSegmentId>,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub contribution_offset_ns: u64,
    pub next_offset_ns: Option<u64>,
    pub wav_base64: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ConversationPlaceSession {
    pub session_id: MeetingSessionId,
    pub title: String,
    pub started_at_utc_ms: Option<i64>,
    pub location: ConversationLocation,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ConversationPlaceReview {
    pub session_id: MeetingSessionId,
    pub location: Option<ConversationLocation>,
    pub sessions: Vec<ConversationPlaceSession>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ConversationBookmark {
    pub bookmark_id: String,
    pub session_id: MeetingSessionId,
    pub start_offset_ns: u64,
    pub label: Option<String>,
    pub created_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConversationBookmarkRequest {
    pub bookmark_id: String,
    pub session_id: MeetingSessionId,
    pub start_offset_ns: u64,
    pub label: Option<String>,
}
