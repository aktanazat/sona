//! Loops that close: the mutable half of a ledger row.
//!
//! A ledger is re-read out of the transcript on every artifact regeneration,
//! so the words of a loop live in the artifact revision and only there. What a
//! person *did* about a loop — resolved it, dropped it, gave it an owner, let
//! it run into the next session of a series — cannot live there: it would be
//! thrown away the next time the model read the meeting again.
//!
//! So identity is derived from content rather than minted. Two extractions of
//! the same commitment in the same meeting produce the same
//! [`MeetingLoopId`], which is what lets a resolution survive a regeneration,
//! and what lets an agent name a loop in a `sona://` link without the store
//! having handed it a number first.

use super::ledger::LedgerFirmness;
use super::people_types::PersonId;
use super::types::{ArtifactCitation, MeetingSessionId, OperationActor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use uuid::Uuid;

/// How many hex characters of the content digest an id carries. Eight bytes:
/// wide enough that two different rows in one meeting colliding is not a thing
/// that happens, short enough that the whole id fits in a link.
const DIGEST_HEX_CHARS: usize = 16;

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Which register of the ledger a row came out of. The two are separate id
/// namespaces because the same sentence can legitimately be both a commitment
/// and the question nobody answered.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingLoopKind {
    /// An open loop: a question asked out loud that never got an answer, or a
    /// thread that did not land.
    Loop,
    /// A commitment: a named person said they would do something.
    Commitment,
}

impl MeetingLoopKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loop => "loop",
            Self::Commitment => "commitment",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "loop" => Some(Self::Loop),
            "commitment" => Some(Self::Commitment),
            _ => None,
        }
    }
}

/// Where a loop stands. `Carried` is not a resolution and not an absence: the
/// same subject came up again in the next session of the series, so this
/// occurrence is closed and its successor is the live one.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingLoopStatus {
    Open,
    Done,
    Dropped,
    Carried,
}

impl MeetingLoopStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Dropped => "dropped",
            Self::Carried => "carried",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "done" => Some(Self::Done),
            "dropped" => Some(Self::Dropped),
            "carried" => Some(Self::Carried),
            _ => None,
        }
    }

    /// Whether this row still needs somebody. The one predicate every brief,
    /// inbox and count reads, so "open" means the same thing in all of them.
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// Which way an actionable row points: something the user owes, or something
/// the user is waiting on somebody else for.
///
/// Sona models no `Person` for its own user. The people store is built from
/// calendar attendees with `is_self` filtered out and from speaker labels on
/// the other side of the conversation, so a `PersonId` is always somebody
/// else. "Mine" therefore cannot be a link; it is what came in on this
/// machine's microphone.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingLoopDirection {
    /// The user owes this.
    Mine,
    /// Somebody else owes it. Which somebody is the row's own owner name.
    WaitingOn,
    /// Nothing in the ledger says whose it is. A question nobody answered,
    /// cited but unquoted, has no speaker and lands here.
    Unattributed,
}

/// What a resolve mutation is allowed to write. Narrower than
/// [`MeetingLoopStatus`] on purpose: reopening is its own mutation, and only
/// the ledger pass may carry a loop forward.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingLoopResolution {
    Done,
    Dropped,
}

impl MeetingLoopResolution {
    pub const fn status(self) -> MeetingLoopStatus {
        match self {
            Self::Done => MeetingLoopStatus::Done,
            Self::Dropped => MeetingLoopStatus::Dropped,
        }
    }
}

/// A loop's stable address: `<session uuid>:<kind>:<16 hex of the text>`.
///
/// The session uuid leads so a router can find the meeting without a lookup
/// table, and the digest trails so the id is stable across regenerations of the
/// same words.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Type)]
#[serde(transparent)]
pub struct MeetingLoopId(pub String);

impl MeetingLoopId {
    /// The id these words have in this meeting. Deterministic: the same
    /// session, register and text always produce the same id, which is the
    /// whole reason a resolution survives the next artifact pass.
    pub fn derive(session_id: MeetingSessionId, kind: MeetingLoopKind, text: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(normalized_loop_text(text).as_bytes());
        let digest = hasher.finalize();
        let mut value = String::with_capacity(36 + 1 + 10 + 1 + DIGEST_HEX_CHARS);
        value.push_str(&session_id.uuid().to_string());
        value.push(':');
        value.push_str(kind.as_str());
        value.push(':');
        for byte in digest.iter().take(DIGEST_HEX_CHARS / 2) {
            value.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            value.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
        }
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The meeting this loop belongs to, read straight out of the id. `None`
    /// when the string did not come from [`Self::derive`].
    pub fn session_id(&self) -> Option<MeetingSessionId> {
        let uuid = self.0.split(':').next()?;
        Uuid::parse_str(uuid).ok().map(MeetingSessionId::from_uuid)
    }

    /// The half of the id that is not the meeting: `<kind>:<digest>`. Two
    /// occurrences of the same loop in different sessions of a series share
    /// this and nothing else, which is how one is matched to the next.
    pub fn content_key(&self) -> Option<&str> {
        self.0.split_once(':').map(|(_, rest)| rest)
    }
}

/// The text an id is derived from: whitespace collapsed, case folded. Matches
/// the store's own `normalized`, so a loop carried into the next session
/// matches the same way people do.
fn normalized_loop_text(value: &str) -> String {
    value
        .split_whitespace()
        .flat_map(str::chars)
        .flat_map(char::to_lowercase)
        .collect()
}

/// Ledger owner names that name the speaker rather than a person. The model
/// fills `who` from the labelled transcript and usually writes the speaker's
/// label, but a first-person promise sometimes comes back in the first person.
const FIRST_PERSON_OWNERS: [&str; 3] = ["i", "me", "myself"];

/// How long a row somebody else owes may stay open before it is worth
/// mentioning: one working week, so a thing promised on Monday is not stale
/// on Friday.
pub const WAITING_ON_STALE_AFTER_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// Which way one ledger row points.
///
/// Precedence runs strongest claim first: the person the user picked, then the
/// owner name the ledger read, then the speaker who said it. Names are matched
/// against `my_speaker_labels` — the display names of this session's
/// microphone speakers — because the microphone track is the only thing that
/// says which voice is the user's without a voiceprint.
pub fn loop_direction(
    owner_person_id: Option<PersonId>,
    owner_text: Option<&str>,
    speaker: Option<&str>,
    my_speaker_labels: &[String],
) -> MeetingLoopDirection {
    // A `Person` is always somebody else, so an explicit assignment settles it
    // and the ledger's reading never overrides the user's pick.
    if owner_person_id.is_some() {
        return MeetingLoopDirection::WaitingOn;
    }
    for name in [owner_text, speaker].into_iter().flatten() {
        let normalized = normalized_loop_text(name);
        if normalized.is_empty() {
            continue;
        }
        let mine = FIRST_PERSON_OWNERS.contains(&normalized.as_str())
            || my_speaker_labels
                .iter()
                .any(|label| normalized_loop_text(label) == normalized);
        return if mine {
            MeetingLoopDirection::Mine
        } else {
            MeetingLoopDirection::WaitingOn
        };
    }
    MeetingLoopDirection::Unattributed
}

/// Whether a row somebody else owes has been open long enough to say so.
///
/// Only `WaitingOn` rows go stale. A thing the user owes is theirs to do, and
/// counting their own backlog at them every evening is not what this number is
/// for.
pub const fn waiting_on_is_stale(
    direction: MeetingLoopDirection,
    status: MeetingLoopStatus,
    at_utc_ms: i64,
    now_utc_ms: i64,
) -> bool {
    matches!(direction, MeetingLoopDirection::WaitingOn)
        && status.is_open()
        && now_utc_ms.saturating_sub(at_utc_ms) > WAITING_ON_STALE_AFTER_MS
}

/// One actionable ledger row: the words from the artifact, the state from the
/// store.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLoopRow {
    pub loop_id: MeetingLoopId,
    pub session_id: MeetingSessionId,
    pub kind: MeetingLoopKind,
    /// The question, or what was promised.
    pub text: String,
    /// Who the ledger read the row as belonging to, verbatim from the
    /// transcript. Kept beside `owner_person_id` because a name the model read
    /// and a person the user picked are different claims.
    pub owner_text: Option<String>,
    pub owner_person_id: Option<PersonId>,
    pub owner_display_name: Option<String>,
    /// Whose side of the conversation this row is on. Derived by
    /// [`loop_direction`] where the row is built, so every surface that groups
    /// "I owe" against "waiting on them" reads one answer.
    pub direction: MeetingLoopDirection,
    pub status: MeetingLoopStatus,
    pub resolved_at_utc_ms: Option<i64>,
    /// The operation that put this row in its current state, so a caller can
    /// look the receipt back up.
    pub resolving_operation_id: Option<String>,
    /// Who put the row in the state it is in, read off the receipt
    /// `resolving_operation_id` names: the person at the keyboard, this app
    /// acting on its own, or `sona --loop-resolve` from outside it. `None`
    /// while the row is open, and for a resolution whose receipt is gone.
    pub resolved_by: Option<OperationActor>,
    /// The successor this loop ran into, when it was carried forward.
    pub carried_into_loop_id: Option<MeetingLoopId>,
    /// The occurrence this loop was first raised at, when it is itself a
    /// successor.
    pub carried_since_at_utc_ms: Option<i64>,
    /// Offset into the meeting the row was read at, for the transcript jump.
    pub at_ms: u64,
    pub revision: u64,
    /// What happened instead of an answer, for a question that never got one.
    pub instead: Option<String>,
    /// How firmly a commitment was made. Absent for loops.
    pub firmness: Option<LedgerFirmness>,
    /// The transcript quote the row was read from. Questions that were never
    /// answered are cited but not quoted, so this is absent for them.
    pub quote: Option<String>,
    pub speaker: Option<String>,
    pub citations: Vec<ArtifactCitation>,
}

impl MeetingLoopRow {
    pub const fn is_open(&self) -> bool {
        self.status.is_open()
    }

    /// The user owes this one.
    pub const fn is_mine(&self) -> bool {
        matches!(self.direction, MeetingLoopDirection::Mine)
    }

    /// Somebody else owes this one.
    pub const fn is_waiting_on(&self) -> bool {
        matches!(self.direction, MeetingLoopDirection::WaitingOn)
    }

    /// When this row started being outstanding. That is its own meeting,
    /// except for a row carried forward — which has been outstanding since the
    /// session it was first raised in, and calling it a day old every time a
    /// series meets again would be the opposite of the truth.
    pub const fn outstanding_since(&self, meeting_at_utc_ms: i64) -> i64 {
        match self.carried_since_at_utc_ms {
            Some(first_raised) => first_raised,
            None => meeting_at_utc_ms,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLoopsResult {
    pub schema_version: u32,
    /// The session revision the rows were read at.
    pub revision: u64,
    pub rows: Vec<MeetingLoopRow>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLoopResolveRequest {
    pub operation_id: super::types::MeetingOperationId,
    pub loop_id: MeetingLoopId,
    pub expected_revision: u64,
    pub resolution: MeetingLoopResolution,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLoopReopenRequest {
    pub operation_id: super::types::MeetingOperationId,
    pub loop_id: MeetingLoopId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLoopAssignRequest {
    pub operation_id: super::types::MeetingOperationId,
    pub loop_id: MeetingLoopId,
    pub expected_revision: u64,
    /// `None` clears the owner.
    pub owner_person_id: Option<PersonId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLoopMutationResult {
    pub receipt: super::types::OperationReceipt,
    /// Every loop in the meeting, re-read after the write, so a caller never
    /// has to guess what its own mutation did to the rest of the list.
    pub loops: MeetingLoopsResult,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_words_in_the_same_meeting_get_the_same_id() {
        let session_id = MeetingSessionId::new();
        let first = MeetingLoopId::derive(
            session_id,
            MeetingLoopKind::Commitment,
            "Send the tier comparison",
        );
        let regenerated = MeetingLoopId::derive(
            session_id,
            MeetingLoopKind::Commitment,
            "  send   the Tier   comparison\n",
        );

        assert_eq!(first, regenerated, "a regeneration must not move an id");
    }

    #[test]
    fn kind_and_session_are_separate_namespaces() {
        let session_id = MeetingSessionId::new();
        let text = "Which tier does the trial convert into?";
        let as_loop = MeetingLoopId::derive(session_id, MeetingLoopKind::Loop, text);
        let as_commitment = MeetingLoopId::derive(session_id, MeetingLoopKind::Commitment, text);
        let elsewhere = MeetingLoopId::derive(MeetingSessionId::new(), MeetingLoopKind::Loop, text);

        assert_ne!(as_loop, as_commitment);
        assert_ne!(as_loop, elsewhere);
    }

    #[test]
    fn an_id_routes_to_its_meeting_without_a_lookup() {
        let session_id = MeetingSessionId::new();
        let loop_id = MeetingLoopId::derive(session_id, MeetingLoopKind::Loop, "Pricing");

        assert_eq!(loop_id.session_id(), Some(session_id));
        assert_eq!(MeetingLoopId("not-an-id".to_string()).session_id(), None);
    }

    /// The whole precedence table in one place: a picked person beats the
    /// ledger's owner name, which beats the speaker who said it, and every
    /// name is decided against the microphone speakers of that session.
    #[test]
    fn direction_reads_the_pick_then_the_owner_then_the_speaker() {
        let mine = ["Local speaker".to_string(), "Ada".to_string()];
        let cases: [(
            Option<PersonId>,
            Option<&str>,
            Option<&str>,
            MeetingLoopDirection,
        ); 9] = [
            // A picked person is always somebody else, even when the user
            // spoke the words and the ledger agreed they were the user's.
            (
                Some(PersonId::new()),
                Some("Ada"),
                Some("Ada"),
                MeetingLoopDirection::WaitingOn,
            ),
            // The ledger's owner name outranks the speaker: Ada can promise
            // something on Amir's behalf.
            (
                None,
                Some("Amir"),
                Some("Ada"),
                MeetingLoopDirection::WaitingOn,
            ),
            (None, Some("Ada"), Some("Amir"), MeetingLoopDirection::Mine),
            // Whitespace and case are the id's own normalization, so a label
            // the user renamed still matches.
            (
                None,
                Some("  local   SPEAKER "),
                None,
                MeetingLoopDirection::Mine,
            ),
            // A first-person promise the model did not resolve to a label.
            (None, Some("I"), Some("Amir"), MeetingLoopDirection::Mine),
            // No owner name: the voice that said it decides.
            (None, None, Some("Ada"), MeetingLoopDirection::Mine),
            (None, None, Some("Amir"), MeetingLoopDirection::WaitingOn),
            // A blank owner name is not an attribution; fall through to the
            // speaker rather than reading it as an unknown other.
            (None, Some("   "), Some("Ada"), MeetingLoopDirection::Mine),
            // An unanswered question is cited but never quoted, so it has
            // neither owner nor speaker.
            (None, None, None, MeetingLoopDirection::Unattributed),
        ];

        for (owner_person_id, owner_text, speaker, expected) in cases {
            assert_eq!(
                loop_direction(owner_person_id, owner_text, speaker, &mine),
                expected,
                "owner {owner_text:?} speaker {speaker:?}"
            );
        }
    }

    #[test]
    fn only_an_open_waiting_on_row_goes_stale_and_only_after_a_week() {
        let now = 1_000 * WAITING_ON_STALE_AFTER_MS;
        let a_week_ago = now - WAITING_ON_STALE_AFTER_MS;
        let stale = |direction, status, at| waiting_on_is_stale(direction, status, at, now);

        assert!(stale(
            MeetingLoopDirection::WaitingOn,
            MeetingLoopStatus::Open,
            a_week_ago - 1
        ));
        // Exactly a week is not yet overdue.
        assert!(!stale(
            MeetingLoopDirection::WaitingOn,
            MeetingLoopStatus::Open,
            a_week_ago
        ));
        // The user's own backlog is never nagged about.
        assert!(!stale(
            MeetingLoopDirection::Mine,
            MeetingLoopStatus::Open,
            a_week_ago - 1
        ));
        assert!(!stale(
            MeetingLoopDirection::Unattributed,
            MeetingLoopStatus::Open,
            a_week_ago - 1
        ));
        // A row that closed cannot be overdue, however old it is.
        for status in [
            MeetingLoopStatus::Done,
            MeetingLoopStatus::Dropped,
            MeetingLoopStatus::Carried,
        ] {
            assert!(!stale(MeetingLoopDirection::WaitingOn, status, 0));
        }
    }
}
