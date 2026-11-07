//! The wrap after a meeting: who was there, where, the plans heard and the
//! promises made, each with the line that said it.

use super::context::ConversationKind;
use super::heard_plan_types::HeardPlan;
use super::loop_types::MeetingLoopRow;
use super::people_types::PersonId;
use super::types::ArtifactCitation;
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingWrap {
    /// The people confirmed as there. A suggested link names nobody.
    pub people: Vec<MeetingWrapPerson>,
    pub kind: ConversationKind,
    pub place: Option<MeetingWrapPlace>,
    pub plans: Vec<HeardPlan>,
    /// Open promises the user made.
    pub mine: Vec<MeetingLoopRow>,
    /// Open promises made to the user.
    pub owed: Vec<MeetingLoopRow>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingWrapPerson {
    pub person_id: PersonId,
    pub display_name: String,
    /// Their first line in the meeting, once their voice is confirmed as
    /// theirs.
    pub first_heard: Option<ArtifactCitation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingWrapPlace {
    pub name: String,
    /// The invite's location as written, not a place the user chose.
    pub from_invite: bool,
}
