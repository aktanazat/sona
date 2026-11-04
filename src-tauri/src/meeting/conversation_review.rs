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

/// What a bookmark marks, as the person pressing it said. A decision mark is
/// their note, not a ledger decision, and a to-do mark makes no task.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConversationBookmarkKind {
    #[default]
    Mark,
    Decision,
    Question,
    Todo,
}

impl ConversationBookmarkKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mark => "mark",
            Self::Decision => "decision",
            Self::Question => "question",
            Self::Todo => "todo",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "mark" => Some(Self::Mark),
            "decision" => Some(Self::Decision),
            "question" => Some(Self::Question),
            "todo" => Some(Self::Todo),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ConversationBookmark {
    pub bookmark_id: String,
    pub session_id: MeetingSessionId,
    pub start_offset_ns: u64,
    pub label: Option<String>,
    #[serde(default)]
    pub kind: ConversationBookmarkKind,
    pub created_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConversationBookmarkRequest {
    pub bookmark_id: String,
    pub session_id: MeetingSessionId,
    pub start_offset_ns: u64,
    pub label: Option<String>,
    #[serde(default)]
    pub kind: ConversationBookmarkKind,
}
