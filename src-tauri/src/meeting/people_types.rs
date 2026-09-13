use super::document_types::{DocumentId, DocumentSummary};
use super::loop_types::{MeetingLoopDirection, MeetingLoopId, MeetingLoopStatus};
use super::types::MeetingSessionId;
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(transparent)]
pub struct PersonId(pub Uuid);

impl PersonId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl From<PersonId> for Uuid {
    fn from(value: PersonId) -> Self {
        value.uuid()
    }
}

impl Default for PersonId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonLinkSource {
    Calendar,
    Speaker,
    Title,
    Manual,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonLinkConfidence {
    Confirmed,
    Suggested,
}

/// The relationship paragraph on a person's page: three sentences generated
/// from that person's evidence pack, and the two facts that make it readable.
///
/// One struct rather than three optional columns on [`Person`], because a
/// paragraph with no engine behind it and an engine with no paragraph are both
/// states the store cannot produce and no reader should have to handle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonSummary {
    pub text: String,
    pub generated_at_utc_ms: i64,
    /// The engine that wrote it, as [`crate::meeting::processing::MeetingTextGenerator::model_id`]
    /// reports it.
    pub model_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct Person {
    pub id: PersonId,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub calendar_emails: Vec<String>,
    pub organization: Option<String>,
    /// Absent until an artifact pass has had an engine to write it with.
    pub summary: Option<PersonSummary>,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
}

/// The address of an organization page: the display name, lower-cased, with
/// every run of other characters collapsed to one hyphen.
///
/// Written once and applied to both sides of a lookup, so `organization_detail`
/// answers to the slug a `sona://organization/<slug>` link carries *and* to the
/// label a person's header shows. `Person::organization` is derived from an
/// email domain, so in practice it is already one ASCII word — the collapsing
/// is what keeps a hand-written link and a two-word label meeting in the middle.
pub fn organization_slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.extend(character.to_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMeetingSummary {
    pub id: MeetingSessionId,
    pub title: String,
    pub at_utc_ms: i64,
    pub headline: Option<String>,
    pub series_number: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMeetingLink {
    pub meeting: PersonMeetingSummary,
    pub source: PersonLinkSource,
    pub confidence: PersonLinkConfidence,
}

/// A loop raised in a meeting with this person, as the people surfaces read it.
///
/// The words come from the meeting's ledger; `loop_id` and `status` come from
/// the loop state row that ledger row is keyed to, so a resolution made on the
/// review screen shows up here without a second copy of the state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonOpenLoop {
    pub loop_id: MeetingLoopId,
    pub meeting_id: MeetingSessionId,
    pub title: String,
    pub at_utc_ms: i64,
    pub text: String,
    pub owner_person_id: Option<PersonId>,
    pub status: MeetingLoopStatus,
    /// Which side of the conversation this row is on: the user owes it, or
    /// this person does. What lets a page show two lists instead of one.
    pub direction: MeetingLoopDirection,
    /// This person has owed it for longer than a working week. Computed
    /// against the store's clock at read time, so it is never a cached
    /// yesterday.
    pub waiting_on_stale: bool,
    /// When this loop was first raised, if it reached this meeting by being
    /// carried forward from an earlier session in the series.
    pub carried_since_at_utc_ms: Option<i64>,
    /// The meeting this loop was carried into, if it has already moved on.
    pub carried_into_meeting_id: Option<MeetingSessionId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonCommitment {
    pub loop_id: MeetingLoopId,
    pub meeting_id: MeetingSessionId,
    pub title: String,
    pub at_utc_ms: i64,
    pub text: String,
    pub status: MeetingLoopStatus,
    /// See [`PersonOpenLoop::direction`].
    pub direction: MeetingLoopDirection,
    /// See [`PersonOpenLoop::waiting_on_stale`].
    pub waiting_on_stale: bool,
    pub resolved_at_utc_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PersonMeetingHeadline {
    Ledger { text: String },
    Summary { text: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonListLastMeeting {
    pub session_id: MeetingSessionId,
    pub title: String,
    pub at_ms: i64,
    pub headline: Option<PersonMeetingHeadline>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonListEntry {
    pub person: Person,
    pub meetings_count: u64,
    pub last_meeting_at_utc_ms: Option<i64>,
    pub suggested_count: u64,
    pub evidence_sources: Vec<PersonLinkSource>,
    pub confirmed_count: u64,
    pub last_meeting: Option<PersonListLastMeeting>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PeopleListResult {
    pub schema_version: u32,
    pub revision: u64,
    pub entries: Vec<PersonListEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonDetail {
    pub person: Person,
    pub links: Vec<PersonMeetingLink>,
    pub open_loops: Vec<PersonOpenLoop>,
    pub commitments: Vec<PersonCommitment>,
    pub talk_share_avg_permille: Option<u32>,
    pub documents: Vec<DocumentSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonDetailResult {
    pub schema_version: u32,
    pub revision: u64,
    pub detail: PersonDetail,
}

/// What pressing "Regenerate" on a person's paragraph did. Every arm except
/// `Written` leaves the earlier paragraph in place, so the page needs the
/// word to say why nothing new appeared rather than reporting a success it
/// cannot show.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonSummaryOutcome {
    /// A new paragraph was written and is in `detail`.
    Written,
    /// No confirmed meeting and no loop: nothing to write a relationship
    /// out of, and a paragraph from an empty pack would be invention.
    NoEvidence,
    /// The meeting's engine could not be resolved: none on this Mac, or the
    /// chosen one is not ready.
    EngineUnavailable,
    /// The engine ran and answered nothing usable.
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonSummaryRegenerateResult {
    pub outcome: PersonSummaryOutcome,
    /// The page after the attempt, whichever way it went.
    pub page: PersonDetailResult,
}

/// One organization, read across the people who carry it.
///
/// Every field is a union of what its people already answer, in the same
/// shapes: an organization is not a stored noun in this corpus — no row, no
/// identity, no mutation — it is the set of people whose calendar addresses
/// landed on one domain. So this page reuses the person row, the person's
/// meeting summary and the person's loop, and adds nothing of its own beyond
/// the union.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct OrganizationDetail {
    /// The label as its people carry it, not the slug it was looked up by.
    pub name: String,
    pub people: Vec<PersonListEntry>,
    /// Meetings with anybody here, newest first, deduplicated across people.
    pub recent_meetings: Vec<PersonMeetingSummary>,
    /// What is still open with anybody here, newest first.
    pub open_loops: Vec<PersonOpenLoop>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct OrganizationDetailResult {
    pub schema_version: u32,
    pub revision: u64,
    pub detail: OrganizationDetail,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonBriefingLastMeeting {
    pub id: MeetingSessionId,
    pub title: String,
    pub at_utc_ms: i64,
    pub headline: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonBriefingRow {
    pub person_id: PersonId,
    pub display_name: String,
    pub meetings_count: u64,
    pub last: Option<PersonBriefingLastMeeting>,
    pub open_loops: Vec<PersonOpenLoop>,
    pub commitments: Vec<PersonCommitment>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonContextResult {
    pub schema_version: u32,
    pub revision: u64,
    pub rows: Vec<PersonBriefingRow>,
}

/// One person on a meeting's "previously together" band. Everything here is
/// relative to that meeting: the count, the last meeting, and the loop all
/// come from meetings that happened before it, never after.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingPersonContextRow {
    pub person_id: PersonId,
    pub display_name: String,
    pub evidence_source: PersonLinkSource,
    /// How many confirmed meetings with this person came before this one.
    pub prior_meetings: u64,
    pub last_prior_meeting: Option<PersonBriefingLastMeeting>,
    pub top_open_loop: Option<PersonOpenLoop>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingPeopleContextResult {
    pub schema_version: u32,
    pub revision: u64,
    pub rows: Vec<MeetingPersonContextRow>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct OpenLoopsInboxResult {
    pub schema_version: u32,
    pub revision: u64,
    pub entries: Vec<PersonOpenLoop>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VocabularyCandidate {
    pub text: String,
    pub occurrences: u64,
    pub meetings_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VocabularyCandidatesResult {
    pub schema_version: u32,
    pub revision: u64,
    pub entries: Vec<VocabularyCandidate>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonRenameRequest {
    pub person_id: PersonId,
    pub display_name: String,
    pub expected_revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum VoiceProfileMergeResolution {
    DiscardSource,
    ReplaceTargetWithSource,
    CombineCompatible,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMergeRequest {
    pub source_person_id: PersonId,
    pub target_person_id: PersonId,
    pub expected_revision: u64,
    #[serde(default)]
    pub voice_profile_resolution: Option<VoiceProfileMergeResolution>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonDeleteRequest {
    pub person_id: PersonId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonLinkRequest {
    pub meeting_id: MeetingSessionId,
    pub person_id: PersonId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PersonSplitTarget {
    Create { display_name: String },
    Existing { person_id: PersonId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonSplitRequest {
    pub source_person_id: PersonId,
    pub target: PersonSplitTarget,
    pub meeting_ids: Vec<MeetingSessionId>,
    pub aliases: Vec<String>,
    pub calendar_emails: Vec<String>,
    pub document_ids: Vec<DocumentId>,
    pub expected_revision: u64,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PeopleMutationResult {
    pub schema_version: u32,
    pub revision: u64,
    pub person: Option<Person>,
    pub removed: bool,
}

#[cfg(test)]
mod tests {
    use super::organization_slug;

    /// The rule both sides of an organization lookup run: a label slugifies to
    /// the slug a link carries, and a slug slugifies to itself.
    #[test]
    fn a_label_and_its_slug_agree() {
        for (label, slug) in [
            ("Acme", "acme"),
            ("acme", "acme"),
            ("  ACME  ", "acme"),
            ("Northstar Labs", "northstar-labs"),
            ("northstar-labs", "northstar-labs"),
            ("Acme (EU) / Ltd.", "acme-eu-ltd"),
            ("", ""),
            ("···", ""),
        ] {
            assert_eq!(organization_slug(label), slug, "{label:?}");
            assert_eq!(organization_slug(slug), slug, "{slug:?} is stable");
        }
    }
}
