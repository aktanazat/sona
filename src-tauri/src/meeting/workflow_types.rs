use super::document_types::DocumentId;
use super::types::MeetingSessionId;
use crate::analytics::DashboardTrendRange;
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowId {
    PersonLinking,
    PreMeetingBriefing,
    Continuity,
    VocabularyMining,
    DocumentLinking,
    /// Loop 1. Mines the dictation corpus for spoken symbol phrases no
    /// replacement rule covers yet.
    SpokenPunctuation,
    /// Loop 2. Turns repeated human-authored rewrites into vocabulary
    /// suggestions.
    CorrectionLearning,
    /// Loop 5. Notices a mode the user keeps reaching for by shortcut.
    ModeHabits,
    /// Loop 6. Reports capture-quality statistics worth acting on.
    CaptureAdvisor,
    /// Internal projection that narrates meeting recording decisions. It is
    /// permanently enabled and never appears in the Settings workflow list.
    MeetingActivity,
    /// Loop 4. Assembles the session-scoped priming blob for a meeting in a
    /// series with standing consent. Infrastructure, not a choice: it only ever
    /// runs for a series the user has already said yes to, and its output dies
    /// with the session.
    SeriesPriming,
    /// D20. Shapes one evening sentence out of the day's receipts. It is
    /// permanently enabled here and gated by `meeting_digest_enabled` at the
    /// scheduler instead: the switch a person looks for lives on Settings >
    /// Meetings beside the hour it fires at, and a second copy of it in the
    /// workflow list would be a second thing to keep true.
    DailyDigest,
}

impl WorkflowId {
    pub const CONFIGURABLE: [Self; 9] = [
        Self::PersonLinking,
        Self::PreMeetingBriefing,
        Self::Continuity,
        Self::VocabularyMining,
        Self::DocumentLinking,
        Self::SpokenPunctuation,
        Self::CorrectionLearning,
        Self::ModeHabits,
        Self::CaptureAdvisor,
    ];
    pub const ALL: [Self; 12] = [
        Self::PersonLinking,
        Self::PreMeetingBriefing,
        Self::Continuity,
        Self::VocabularyMining,
        Self::DocumentLinking,
        Self::SpokenPunctuation,
        Self::CorrectionLearning,
        Self::ModeHabits,
        Self::CaptureAdvisor,
        Self::MeetingActivity,
        Self::SeriesPriming,
        Self::DailyDigest,
    ];

    /// Workflows the Settings list never shows and `set_workflow_enabled`
    /// refuses to touch.
    pub const PERMANENT: [Self; 3] = [
        Self::MeetingActivity,
        Self::SeriesPriming,
        Self::DailyDigest,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersonLinking => "person_linking",
            Self::PreMeetingBriefing => "pre_meeting_briefing",
            Self::Continuity => "continuity",
            Self::VocabularyMining => "vocabulary_mining",
            Self::DocumentLinking => "document_linking",
            Self::SpokenPunctuation => "spoken_punctuation",
            Self::CorrectionLearning => "correction_learning",
            Self::ModeHabits => "mode_habits",
            Self::CaptureAdvisor => "capture_advisor",
            Self::MeetingActivity => "meeting_activity",
            Self::SeriesPriming => "series_priming",
            Self::DailyDigest => "daily_digest",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|workflow| workflow.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct WorkflowEventId(pub Uuid);

impl WorkflowEventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(transparent)]
pub struct WorkflowRunId(pub Uuid);

impl WorkflowRunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Ok,
    Failed,
    Skipped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOutcomeCode {
    PersonLinks,
    Briefing,
    Continuity,
    VocabularyCandidates,
    DocumentLinks,
    /// A learning loop finished a mining pass. `suggestions` counts what it
    /// added to the pending queue this run, which is the only number a reader
    /// can act on — a pass that mined a thousand runs and suggested nothing is
    /// a quiet success, not an event.
    LearningSuggestions,
    /// Loop 4 primed one session. `terms` counts what the session's own
    /// transcription will see; nothing was written to shared vocabulary.
    SeriesPrimed,
    /// D20 shaped one evening's sentence. The three counts below are the
    /// sentence: they are what the day held, not what this run changed —
    /// the digest writes nothing.
    DigestRaised,
    PromptRecorded,
    PromptIgnored,
    AutoRecordStarted,
    AutoRecordStopped,
    PrepPresented,
    PrepRecordArmed,
    PrepBriefOpened,
    PrepDismissed,
    WrapPresented,
    WrapNotesOpened,
    WrapFollowUpCopied,
    WrapDone,
    AlreadyProcessed,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowOutcomeCounts {
    pub changes: u64,
    pub persons: u64,
    pub series: u64,
    pub carried: u64,
    pub candidates: u64,
    pub suggestions: u64,
    pub terms: u64,
    /// D20: meetings captured on the digest's local day.
    pub meetings: u64,
    /// D20: open loops closed on the digest's local day.
    pub loops_closed: u64,
    /// D20: learning suggestions still waiting for an answer at digest time.
    /// Distinct from `suggestions`, which counts what a mining pass just added.
    pub suggestions_waiting: u64,
    /// D27: open rows somebody else has owed for longer than a working week,
    /// at digest time. A backlog like `suggestions_waiting`, not a day count.
    pub waiting_on_stale: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowJumpTarget {
    Meeting { session_id: MeetingSessionId },
    Document { document_id: DocumentId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowRunReceipt {
    pub id: WorkflowRunId,
    pub workflow_id: WorkflowId,
    pub event_kind: WorkflowEventKind,
    pub jump_target: Option<WorkflowJumpTarget>,
    pub status: WorkflowRunStatus,
    pub started_at_utc_ms: i64,
    pub finished_at_utc_ms: i64,
    pub outcome_summary: String,
    pub outcome_code: WorkflowOutcomeCode,
    pub outcome_counts: WorkflowOutcomeCounts,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowSummary {
    pub id: WorkflowId,
    pub enabled: bool,
    pub last_run: Option<WorkflowRunReceipt>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowsListResult {
    pub schema_version: u32,
    pub revision: u64,
    pub entries: Vec<WorkflowSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowSetEnabledRequest {
    pub workflow_id: WorkflowId,
    pub enabled: bool,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowRunCursor {
    pub started_at_utc_ms: i64,
    pub run_id: WorkflowRunId,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowRunsRequest {
    pub workflow_id: Option<WorkflowId>,
    pub cursor: Option<WorkflowRunCursor>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PaginatedWorkflowRuns {
    pub schema_version: u32,
    pub revision: u64,
    pub entries: Vec<WorkflowRunReceipt>,
    pub next_cursor: Option<WorkflowRunCursor>,
}

/// Runs started on one local calendar day.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowRunTrendPoint {
    pub local_date: String,
    pub runs: u64,
}

/// Runs per local calendar day over one trend window, every day present and
/// today last. Counted over every run the store holds, so the number is the
/// window's and not the run log's scroll position.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct WorkflowRunTrend {
    pub range: DashboardTrendRange,
    pub range_start_local_date: String,
    pub range_end_local_date: String,
    pub total: u64,
    pub points: Vec<WorkflowRunTrendPoint>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEventKind {
    MeetingFinalized,
    MeetingStarted,
    SpeakerRenamed,
    AudioImported,
    DocumentIngested,
    CalendarMeetingDetected,
    AgentHookEvent,
    MeetingPromptRecorded,
    MeetingPromptIgnored,
    MeetingAutoRecordStarted,
    MeetingAutoRecordStopped,
    MeetingPrepPresented,
    MeetingPrepRecordArmed,
    MeetingPrepBriefOpened,
    MeetingPrepDismissed,
    MeetingWrapPresented,
    MeetingWrapNotesOpened,
    MeetingWrapFollowUpCopied,
    MeetingWrapDone,
    /// The dictation history has runs the learning loops have not read yet.
    ///
    /// This is a wake-up, not data: it carries no transcript and its dedupe key
    /// is one local day, so a heavy dictation day produces one event and one
    /// bounded mining pass per loop rather than thousands.
    DictationCorpusSwept,
    /// A human corrected a dictation. Payload is the rewrite they performed;
    /// the dedupe key is that rewrite on that local day.
    DictationCorrectionRecorded,
    /// The configured digest hour has passed on a local day the digest has not
    /// summarized yet. Payload is that local day; the dedupe key is that local
    /// day, so a restart at 19:00 cannot fire a second evening notification.
    DailyDigestDue,
}

impl WorkflowEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MeetingFinalized => "meeting_finalized",
            Self::MeetingStarted => "meeting_started",
            Self::SpeakerRenamed => "speaker_renamed",
            Self::AudioImported => "audio_imported",
            Self::DocumentIngested => "doc_ingested",
            Self::CalendarMeetingDetected => "calendar_meeting_detected",
            Self::AgentHookEvent => "agent_hook_event",
            Self::MeetingPromptRecorded => "meeting_prompt_recorded",
            Self::MeetingPromptIgnored => "meeting_prompt_ignored",
            Self::MeetingAutoRecordStarted => "meeting_auto_record_started",
            Self::MeetingAutoRecordStopped => "meeting_auto_record_stopped",
            Self::MeetingPrepPresented => "meeting_prep_presented",
            Self::MeetingPrepRecordArmed => "meeting_prep_record_armed",
            Self::MeetingPrepBriefOpened => "meeting_prep_brief_opened",
            Self::MeetingPrepDismissed => "meeting_prep_dismissed",
            Self::MeetingWrapPresented => "meeting_wrap_presented",
            Self::MeetingWrapNotesOpened => "meeting_wrap_notes_opened",
            Self::MeetingWrapFollowUpCopied => "meeting_wrap_follow_up_copied",
            Self::MeetingWrapDone => "meeting_wrap_done",
            Self::DictationCorpusSwept => "dictation_corpus_swept",
            Self::DictationCorrectionRecorded => "dictation_correction_recorded",
            Self::DailyDigestDue => "daily_digest_due",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "meeting_finalized" => Some(Self::MeetingFinalized),
            "meeting_started" => Some(Self::MeetingStarted),
            "speaker_renamed" => Some(Self::SpeakerRenamed),
            "audio_imported" => Some(Self::AudioImported),
            "doc_ingested" => Some(Self::DocumentIngested),
            "calendar_meeting_detected" => Some(Self::CalendarMeetingDetected),
            "agent_hook_event" => Some(Self::AgentHookEvent),
            "meeting_prompt_recorded" => Some(Self::MeetingPromptRecorded),
            "meeting_prompt_ignored" => Some(Self::MeetingPromptIgnored),
            "meeting_auto_record_started" => Some(Self::MeetingAutoRecordStarted),
            "meeting_auto_record_stopped" => Some(Self::MeetingAutoRecordStopped),
            "meeting_prep_presented" => Some(Self::MeetingPrepPresented),
            "meeting_prep_record_armed" => Some(Self::MeetingPrepRecordArmed),
            "meeting_prep_brief_opened" => Some(Self::MeetingPrepBriefOpened),
            "meeting_prep_dismissed" => Some(Self::MeetingPrepDismissed),
            "meeting_wrap_presented" => Some(Self::MeetingWrapPresented),
            "meeting_wrap_notes_opened" => Some(Self::MeetingWrapNotesOpened),
            "meeting_wrap_follow_up_copied" => Some(Self::MeetingWrapFollowUpCopied),
            "meeting_wrap_done" => Some(Self::MeetingWrapDone),
            "dictation_corpus_swept" => Some(Self::DictationCorpusSwept),
            "dictation_correction_recorded" => Some(Self::DictationCorrectionRecorded),
            "daily_digest_due" => Some(Self::DailyDigestDue),
            _ => None,
        }
    }

    /// Whether a failed run of this event is worth another attempt.
    ///
    /// Most kinds are raised again by their own next occurrence, so a failure
    /// costs one signal and the next one recovers. Two kinds have no next
    /// occurrence, because everything later on the same local day collapses
    /// into the same dedupe key: the daily corpus sweep, where a single
    /// failure would otherwise silence all three of its loops until tomorrow,
    /// and the evening digest, where it would cost the day's only
    /// notification. For those two only a *successful* run is terminal.
    ///
    /// Who tries again differs. The sweep is a debt, and the startup
    /// reconciliation scan pays it whenever the app next runs. The digest is
    /// a moment: its own clock tries again on the next tick of the day it is
    /// about, and [`Self::reconciled_at_launch`] keeps the scan off it, since
    /// an evening summary of yesterday written over breakfast is worse than
    /// none at all.
    pub const fn retries_after_failure(self) -> bool {
        matches!(self, Self::DictationCorpusSwept | Self::DailyDigestDue)
    }

    /// Whether the startup reconciliation scan may run what this event still
    /// owes. False only for the digest, whose scheduler retries it inside its
    /// own day; see [`Self::retries_after_failure`].
    pub const fn reconciled_at_launch(self) -> bool {
        !matches!(self, Self::DailyDigestDue)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NewWorkflowEvent {
    pub kind: WorkflowEventKind,
    pub payload: serde_json::Value,
    pub occurred_at_utc_ms: i64,
    pub source: &'static str,
    pub dedupe_key: String,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkflowDispatchResult {
    pub inserted: bool,
    pub event_id: WorkflowEventId,
    #[cfg(test)]
    pub receipts: Vec<WorkflowRunReceipt>,
}
