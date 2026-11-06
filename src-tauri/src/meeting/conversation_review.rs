use super::context::ConversationLocation;
use super::loop_types::{MeetingLoopDirection, MeetingLoopStatus};
use super::types::{MeetingSessionId, SaidMoment, TranscriptSegmentId};
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

/// What a reel moment is: a promise, a decision, a question, or one of the
/// person's own lines when their meetings held too little else.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ReelMomentKind {
    Promise,
    Decision,
    Question,
    Line,
}

/// One recorded moment of a reel. A promise or a question carries which way
/// it points and where it stands now. The play offsets bound what is heard:
/// from just before the line to just after it, never longer than a reel
/// gives one moment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ReelMoment {
    pub said: SaidMoment,
    pub kind: ReelMomentKind,
    pub direction: Option<MeetingLoopDirection>,
    pub status: Option<MeetingLoopStatus>,
    pub play_start_offset_ns: u64,
    pub play_end_offset_ns: u64,
}

/// The recorded moments behind a person or a promise, oldest first. A moment
/// whose audio is gone stays listed with `audio_retained` false.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ConversationReel {
    pub moments: Vec<ReelMoment>,
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

/// What a bookmark marks, as the person pressing it said. The notes list what
/// was said in the seconds up to a decision mark as a decision, and up to a
/// to-do mark as an action item. The other two only mark a place.
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
