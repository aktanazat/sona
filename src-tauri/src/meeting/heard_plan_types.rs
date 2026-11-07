//! A plan to meet, heard in a recording: what, when, who and where as they
//! were said, with the words that said it.

use super::types::{ArtifactCitation, HeardPlanId, MeetingSessionId};
use serde::{Deserialize, Serialize};
use specta::Type;

/// Where a heard plan stands, worked out each time it is read: `happened`
/// when a later recording overlaps its time with someone confirmed there who
/// answers to one of its names, else `no_record` once its time has passed,
/// else `on_calendar` once it was added, else `heard`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HeardPlanStatus {
    Heard,
    OnCalendar,
    Happened,
    NoRecord,
}

/// When a heard plan is. A day said with no time is that whole local day.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct HeardPlanWindow {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub all_day: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct HeardPlan {
    pub plan_id: HeardPlanId,
    pub session_id: MeetingSessionId,
    pub title: String,
    /// The names as said, or as the operator corrected them.
    pub people: Vec<String>,
    /// The words that said when, as said.
    pub when_text: String,
    /// `None` when those words name no day this Mac can place. Such a plan is
    /// given a time before it can go on the calendar.
    pub window: Option<HeardPlanWindow>,
    pub place: Option<String>,
    /// The words that made the plan, and where they were said. `None` once
    /// the meeting's transcript is deleted.
    pub quote: Option<String>,
    pub citation: Option<ArtifactCitation>,
    pub status: HeardPlanStatus,
    pub on_calendar: bool,
    /// The operator answered "Not now": the plan is kept and stops asking.
    pub declined: bool,
    pub edited: bool,
    /// The later recording that matched the plan, when it happened.
    pub happened_session_id: Option<MeetingSessionId>,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
}

/// The operator's correction of a heard plan, before it goes on the
/// calendar.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct HeardPlanEdit {
    pub plan_id: HeardPlanId,
    pub title: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub people: Vec<String>,
    pub place: Option<String>,
}

/// One plan a reading found and checked against the transcript, before it
/// is kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HeardPlanDraft {
    pub(crate) title: String,
    pub(crate) when_text: String,
    pub(crate) window: Option<HeardPlanWindow>,
    pub(crate) people: Vec<String>,
    pub(crate) place: Option<String>,
    pub(crate) quote: String,
    pub(crate) citation: ArtifactCitation,
}
