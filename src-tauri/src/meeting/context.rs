use super::types::{MeetingOrigin, MeetingSessionId};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    #[default]
    Unspecified,
    Huddle,
    OnlineCall,
    InPerson,
    NoteSession,
    Imported,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct ConversationLocation {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// Coordinates are always finite: JSON has no NaN, and a pin from the
/// calendar is kept only once `is_valid` passes.
impl Eq for ConversationLocation {}

impl ConversationLocation {
    pub(crate) fn is_valid(&self) -> bool {
        !self.name.trim().is_empty()
            && self.name.len() <= 1_000
            && self.latitude.is_finite()
            && self.longitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct ConversationContext {
    pub session_id: MeetingSessionId,
    pub revision: u64,
    pub kind: ConversationKind,
    pub origin: MeetingOrigin,
    pub application_bundle_ids: Vec<String>,
    pub captured_microphone: bool,
    pub captured_system_audio: bool,
    pub calendar_name: Option<String>,
    pub calendar_location: Option<String>,
    pub location: Option<ConversationLocation>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct ConversationContextUpdate {
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub kind: ConversationKind,
    pub location: Option<ConversationLocation>,
}
