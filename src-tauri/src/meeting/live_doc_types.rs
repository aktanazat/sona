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
