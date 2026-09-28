//! D28: the week ahead, as the Meetings home reads it.
//!
//! These are view rows, not stored facts. Every field is either something
//! EventKit reported this second or something the series store already owns;
//! nothing here is persisted, and nothing here is a second copy of a calendar
//! event. The one thing the shape adds is the join: a row arrives with its
//! series' three decisions already on it, so the pane never has to fan out one
//! preference read per row to know whether to draw a switch on.

use super::analytics::MeetingNotesTemplate;
use super::detection::calendar::CalendarAccess;
use super::detection::machine::ParticipationStatus;
use super::people_types::PersonId;
use super::template_types::MeetingTemplateId;
use serde::{Deserialize, Serialize};
use specta::Type;

/// One named participant on an upcoming event, and whether they are already
/// somebody Sona has a page for.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingUpcomingAttendee {
    pub name: String,
    pub status: ParticipationStatus,
    /// True for the calendar account's own entry, which a chip row renders as
    /// "you" rather than as a person to link to.
    pub is_self: bool,
    /// The person page behind this address, when the address book knows one.
    /// `None` is the ordinary case for a first-time attendee.
    pub person_id: Option<PersonId>,
}

/// What one series has decided, joined onto every row that belongs to it.
///
/// Present only on a recurring row: a one-off has no series to remember
/// anything, and offering "always record this" for an event that happens once
/// would be a control with nothing behind it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingUpcomingSeries {
    pub series_key: String,
    /// A live standing grant covers this series, so its occurrences record
    /// themselves.
    pub always_record: bool,
    /// The notes template this series is remembered by, or `None` for the app
    /// default.
    pub template: Option<MeetingNotesTemplate>,
    #[serde(default)]
    pub custom_template_id: Option<MeetingTemplateId>,
    pub digest_included: bool,
}

/// One event in the window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingUpcomingRow {
    /// The occurrence's own key, which is what `meeting_preflight_create`
    /// accepts to start this specific event.
    pub event_key: String,
    pub title: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    /// Participants EventKit named, in the order it named them.
    pub attendees: Vec<MeetingUpcomingAttendee>,
    /// Participants including the ones EventKit refused to name, so it can
    /// exceed `attendees.len()`.
    pub attendee_count: u32,
    /// Title of the calendar this event sits on — "Work", "Personal", the
    /// Google or Outlook account name — or `None` when EventKit reports none.
    pub calendar_name: Option<String>,
    /// The event's own URL, which for a scheduled call is the join link.
    pub join_url: Option<String>,
    /// `Some` exactly when the event repeats. Carries the series' decisions.
    pub series: Option<MeetingUpcomingSeries>,
}

/// The whole answer: the window that was read, what is in it, and why it might
/// be empty.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingUpcomingEvents {
    /// Whether events are readable at all. An empty list under `authorized` is
    /// a free week; an empty list under anything else is a missing grant, and
    /// the section says something different for each.
    pub access: CalendarAccess,
    pub window_start_utc_ms: i64,
    pub window_end_utc_ms: i64,
    pub rows: Vec<MeetingUpcomingRow>,
    /// The series-preferences fence every control on these rows writes with.
    /// One number for the whole pane, because one counter fences all three
    /// decisions.
    pub series_revision: u64,
}
