use super::types::MeetingSessionId;
use serde::{Deserialize, Serialize};
use specta::Type;

/// The doc a meeting builds on request: the points said so far, kept
/// current while it records, and what the user asked Sona out loud.
#[derive(Clone, Debug, Serialize, Type)]
pub struct MeetingLiveDoc {
    pub session_id: MeetingSessionId,
    /// Whether the doc was asked for. A doc that was never started has no
    /// points and is not read.
    pub started: bool,
    pub points: Vec<MeetingLiveDocPoint>,
    /// Spoken asks, oldest first. Only a recording has them.
    pub asks: Vec<MeetingVoiceAsk>,
    /// Model requests this capture has made for the doc.
    pub model_requests: u32,
    /// Whether the meeting is still recording, so later readings may add.
    pub live: bool,
    /// Who changed the doc and the board, oldest first.
    pub changes: Vec<MeetingLiveChange>,
}

/// One change to a meeting's doc or board: who made it, when, and how far
/// into the call.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLiveChange {
    pub id: String,
    pub by: MeetingLiveAuthor,
    /// The name a teammate gave the link. Only a teammate's change has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub at_ms: i64,
    /// How far into the call, by the newest line heard; `None` for a
    /// change made after the stop.
    pub call_ns: Option<u64>,
    pub target: MeetingLiveTarget,
    pub target_id: String,
    pub kind: MeetingLiveChangeKind,
    /// The words after the change, or the words removed.
    pub text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingLiveAuthor {
    Sona,
    /// The person recording.
    Owner,
    /// Someone with the meeting's link.
    Teammate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingLiveTarget {
    /// A point in the doc.
    Point,
    /// A promise on the board.
    Card,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingLiveChangeKind {
    Added,
    Rewrote,
    Edited,
    Removed,
}

/// One point in the doc.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLiveDocPoint {
    pub id: String,
    pub text: String,
    /// Written or changed by the user. Later readings leave it alone.
    pub edited: bool,
    /// When it was first said, from the start of the meeting. `None` for a
    /// point the user wrote.
    pub start_offset_ns: Option<u64>,
}

/// Something the user said to Sona on their own microphone.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Type)]
pub struct MeetingVoiceAsk {
    /// Counts up from 1 in the order the asks were heard.
    pub sequence: u32,
    pub intent: MeetingVoiceIntent,
    /// The words after "Sona", as heard.
    pub words: String,
    pub start_offset_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingVoiceIntent {
    /// "Sona, start a doc for this plan."
    StartDoc,
    /// "Sona, put it on a board."
    Board,
    /// "Sona, give me a recap."
    Recap,
    /// Any other question about the call.
    Ask,
}
