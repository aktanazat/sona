//! What one recurring meeting remembers about itself.
//!
//! A series is identified by the same `series_key` standing consent and loop 4's
//! priming already use — EventKit's calendar-item identifier — so "this series"
//! means exactly what it means everywhere else in the app. A meeting with no
//! calendar event behind it has no series and therefore no preference; that is
//! the `None` in every shape below, not an error.
//!
//! Four decisions live here, and they are read as one record because the
//! surfaces that show them show them together: D21's notes template, D28's
//! digest inclusion, D14's remote exclusion, and whether the series announces
//! itself in the meeting's chat when it starts recording. Always-record is not
//! stored here — it is the live standing-consent grant in
//! `meeting_series_consents`, joined in on read — because permission to record
//! is consent, not a preference, and it must stay in the table the consent
//! receipts revalidate against.

use super::analytics::MeetingNotesTemplate;
use super::types::{MeetingOperationId, MeetingTemplateId, OperationReceipt};
use serde::{Deserialize, Serialize};
use specta::Type;

/// Everything one series has decided, and the fence a write against it carries.
///
/// `series_key` is `None` when the surface asking has no series at all — a
/// manual recording, or a meeting whose calendar event was never captured. In
/// that case every other field is its default, which is what lets a caller
/// render "not part of a series" without a second shape.
///
/// `revision` counts every series write on this machine rather than only this
/// series'. Two windows editing two different series is not a real collision,
/// but a shared counter costs one retry in that case and removes a per-row
/// revision column that nothing else would read. All three setters below fence
/// on it and bump it, so the three controls a surface shows side by side are
/// fenced by the one number that surface already holds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSeriesPreferences {
    pub series_key: Option<String>,
    /// `None` when a custom template is chosen or this series has no choice.
    pub template: Option<MeetingNotesTemplate>,
    #[serde(default)]
    pub custom_template_id: Option<MeetingTemplateId>,
    /// D28. True unless the operator has taken this series out of the evening
    /// digest; a series with no row at all is included.
    pub digest_included: bool,
    /// True while a live, unrevoked standing grant covers this series, which is
    /// exactly the condition that lets detection auto-start an occurrence.
    pub always_record: bool,
    /// D14. True when this series' text is written on this Mac even while
    /// meeting intelligence is routed to the operator's own server.
    ///
    /// The default is false — a series inherits the global setting — because a
    /// switch that quietly excluded series would make the global one a lie.
    /// What is stored here is only the departure from that.
    pub remote_intelligence_opt_out: bool,
    /// True when this series posts one disclosure line into the meeting's own
    /// chat as it starts recording.
    ///
    /// The default is false: a recording announces itself only because somebody
    /// asked it to, and a line typed into a shared chat by an app nobody invited
    /// would be exactly the ambient authority this design refuses.
    pub announce_in_chat: bool,
    pub revision: u64,
}

/// Choose, or unchoose, the template for one series.
///
/// Both template fields `None` clears the preference. A custom id takes
/// precedence when both are supplied.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSeriesTemplateSetRequest {
    pub operation_id: MeetingOperationId,
    pub series_key: String,
    pub template: Option<MeetingNotesTemplate>,
    #[serde(default)]
    pub custom_template_id: Option<MeetingTemplateId>,
    pub expected_revision: u64,
}

/// D28. Keep this series in the evening digest, or take it out of it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSeriesDigestSetRequest {
    pub operation_id: MeetingOperationId,
    pub series_key: String,
    pub digest_included: bool,
    pub expected_revision: u64,
}

/// D28. Grant or revoke the standing consent that lets this series record
/// itself.
///
/// `acknowledged_sources` is the operator's acknowledgement, not a hint: it is
/// the set of capture sources selected on the surface they pressed this on, and
/// it is what the grant stores and what a later auto-started attempt cites in
/// its own consent receipt. Turning always-record *off* ignores it, because
/// revoking permission needs none.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSeriesAlwaysRecordSetRequest {
    pub operation_id: MeetingOperationId,
    pub series_key: String,
    pub always_record: bool,
    pub policy_version: u32,
    pub acknowledged_sources: Vec<super::types::SourceKind>,
    pub expected_revision: u64,
}

/// D14. Keep this series' text on this Mac, or hand it back to the global
/// meeting-intelligence setting.
///
/// A separate mutation from the global switch on purpose: excluding one series
/// is a statement about that series, it survives the switch being turned off
/// and on again, and it earns its own receipt naming the series it touched.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSeriesRemoteOptOutSetRequest {
    pub operation_id: MeetingOperationId,
    pub series_key: String,
    pub remote_intelligence_opt_out: bool,
    pub expected_revision: u64,
}

/// D14. One series the operator can keep on this Mac, as the settings surface
/// shows it.
///
/// `series_key`, `title`, `last_met_at_utc_ms` and `meetings` are
/// `store::series::series_roster_in`'s answer, unchanged — the same four the
/// automations roster carries, spelled `meeting_count` there for the same
/// count. Only `remote_intelligence_opt_out` belongs to this surface.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSeriesRemoteRow {
    pub series_key: String,
    pub title: String,
    pub last_met_at_utc_ms: i64,
    pub meetings: u32,
    pub remote_intelligence_opt_out: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSeriesRemoteRoster {
    pub rows: Vec<MeetingSeriesRemoteRow>,
    /// The one fence every switch on these rows writes with.
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSeriesMutationResult {
    pub receipt: OperationReceipt,
    pub preferences: MeetingSeriesPreferences,
}
