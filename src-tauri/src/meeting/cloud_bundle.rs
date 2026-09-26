use super::store::{MeetingStore, StoreError};
use super::types::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub(crate) const CLOUD_MEETING_BUNDLE_VERSION: u32 = 1;
const MAX_CLOUD_MEETING_BUNDLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CLOUD_MEETING_BUNDLE_ROWS: usize = 100_000;
const MAX_CLOUD_MEETING_BUNDLE_TEXT_BYTES: usize = 256 * 1024;

/// A portable, non-audio normalized meeting snapshot suitable for encrypted sync.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudMeetingBundleV1 {
    pub format_version: u32,
    pub audio_included: bool,
    /// Notes can outlive their cited transcript. Absent in older bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_purged_at_utc_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_analytics: Option<super::analytics::MeetingAnalytics>,
    pub session: CloudBundleSession,
    pub run_plans: Vec<CloudBundleRunPlan>,
    pub consents: Vec<CloudBundleConsent>,
    pub source_tracks: Vec<CloudBundleSourceTrack>,
    pub source_clock_epochs: Vec<CloudBundleSourceClockEpoch>,
    pub capture_windows: Vec<CloudBundleCaptureWindow>,
    pub source_gaps: Vec<CloudBundleSourceGap>,
    pub speakers: Vec<CloudBundleSpeaker>,
    pub transcript_revisions: Vec<CloudBundleTranscriptRevision>,
    pub transcript_segments: Vec<CloudBundleTranscriptSegment>,
    pub segment_edits: Vec<CloudBundleSegmentEdit>,
    pub notes: Vec<ManualNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_notes: Option<super::analytics::MeetingUserNotes>,
    pub artifacts: Vec<CloudBundleArtifact>,
    pub artifact_revisions: Vec<MeetingArtifactRevision>,
    pub questions: Vec<MeetingAnswer>,
    pub diarization_generations: Vec<CloudBundleDiarizationGeneration>,
    pub diarization_assignments: Vec<CloudBundleDiarizationAssignment>,
}

/// The session row and its immutable review/recovery metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleSession {
    pub session_id: MeetingSessionId,
    pub phase: MeetingPhase,
    pub revision: u64,
    pub title: String,
    pub origin: MeetingOrigin,
    pub preflight: MeetingPreflightSnapshot,
    pub created_at_utc_ms: i64,
    pub started_at_utc_ms: Option<i64>,
    pub ended_at_utc_ms: Option<i64>,
    pub recovered_at_utc_ms: Option<i64>,
    pub successful_plan_id: Option<MeetingPlanId>,
    pub processing_status: ProcessingStatus,
    pub retention_policy: MeetingRetentionPolicy,
    pub delete_after_utc_ms: Option<i64>,
    pub current_transcript_revision_id: Option<TranscriptRevisionId>,
    pub current_diarization_generation_id: Option<MeetingDiarizationGenerationId>,
    pub diarization_status: DiarizationStatus,
    pub diarization_model_id: Option<String>,
    pub diarization_model_version: Option<String>,
}

/// One canonical capture plan retained to satisfy normalized track ownership.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleRunPlan {
    pub plan_id: MeetingPlanId,
    pub consent_id: ConsentId,
    pub attempt_number: u32,
    pub schema_version: u32,
    pub canonical_plan: MeetingRunPlan,
    pub created_at_utc_ms: i64,
}

/// One consent acknowledgement paired with a portable capture plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleConsent {
    pub consent_id: ConsentId,
    pub attempt_number: u32,
    pub preflight_revision: u64,
    pub policy_version: u32,
    pub acknowledgement: MeetingConsent,
    pub acknowledged_at_utc_ms: i64,
}

/// One source-track row without durable PCM record metadata or files.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleSourceTrack {
    pub track_id: SourceTrackId,
    pub plan_id: MeetingPlanId,
    pub source_kind: SourceKind,
    pub required: bool,
    pub requested: bool,
    pub descriptor_json: String,
    pub timestamp_bridge: TimestampBridge,
    pub format: Option<AudioFormat>,
    pub first_offset_ns: Option<u64>,
    pub last_offset_ns: Option<u64>,
    pub health: SourceHealth,
}

/// One source clock epoch required to reconstruct the recorded timeline metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleSourceClockEpoch {
    pub track_id: SourceTrackId,
    pub source_epoch: SourceEpoch,
    pub format_epoch: u64,
    pub bridge: TimestampBridge,
    pub observed_host_monotonic_ns: u64,
}

/// One capture interval retained without any audio payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleCaptureWindow {
    pub sequence: u64,
    pub start_offset_ns: u64,
    pub end_offset_ns: Option<u64>,
    pub close_reason: Option<String>,
}

/// One source-gap observation retained without callback payloads.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleSourceGap {
    pub track_id: SourceTrackId,
    pub source_epoch: SourceEpoch,
    pub start_offset_ns: Option<u64>,
    pub end_offset_ns: Option<u64>,
    pub reason: SourceGapReason,
    pub dropped_frames: Option<u64>,
    pub observed_at_utc_ms: i64,
}

/// One editable speaker identity in the imported meeting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleSpeaker {
    pub speaker_id: SpeakerId,
    pub source_kind: SourceKind,
    pub display_name: String,
    pub revision: u64,
    pub merged_into_speaker_id: Option<SpeakerId>,
}

/// The only transcript and diarization task states that a portable bundle may carry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CloudBundleTaskState {
    Running,
    Completed,
    Failed,
}

/// One transcript revision and its source/provenance metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleTranscriptRevision {
    pub transcript_revision_id: TranscriptRevisionId,
    pub engine_id: String,
    pub model_version: Option<String>,
    pub destination: ProcessingDestination,
    pub source_set: Vec<SourceKind>,
    pub language: String,
    pub state: CloudBundleTaskState,
    pub created_at_utc_ms: i64,
    pub completed_at_utc_ms: Option<i64>,
    pub error_code: Option<String>,
}

/// One immutable base transcript segment before editable replacements.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleTranscriptSegment {
    pub segment_id: TranscriptSegmentId,
    pub transcript_revision_id: TranscriptRevisionId,
    pub track_id: SourceTrackId,
    pub ordinal: u64,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub speaker_id: SpeakerId,
    pub base_text: String,
    pub confidence_milli: Option<u16>,
}

/// One append-only edit to an immutable transcript segment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleSegmentEdit {
    pub segment_id: TranscriptSegmentId,
    pub edit_sequence: u64,
    pub replacement_text: String,
    pub removed: bool,
    pub operator_at_utc_ms: i64,
}

/// One generated-artifact metadata row distinct from its revision content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleArtifact {
    pub artifact_id: MeetingArtifactId,
    pub kind: MeetingArtifactKind,
    pub transcript_revision_id: Option<TranscriptRevisionId>,
    pub input_revision: u64,
    pub state: MeetingArtifactState,
    pub created_at_utc_ms: i64,
}

/// One diarization generation retained with its assignment provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleDiarizationGeneration {
    pub generation_id: MeetingDiarizationGenerationId,
    pub transcript_revision_id: TranscriptRevisionId,
    pub input_revision: u64,
    pub model_id: String,
    pub model_version: String,
    pub state: CloudBundleTaskState,
    pub created_at_utc_ms: i64,
    pub completed_at_utc_ms: Option<i64>,
}

/// One diarization assignment referencing normalized transcript and speaker rows.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBundleDiarizationAssignment {
    pub generation_id: MeetingDiarizationGenerationId,
    pub segment_id: TranscriptSegmentId,
    pub speaker_id: SpeakerId,
    pub assignment_kind: SpeakerAssignmentKind,
}

impl CloudMeetingBundleV1 {
    pub(crate) fn export_from_store(
        store: &MeetingStore,
        session_id: MeetingSessionId,
    ) -> Result<Self, StoreError> {
        store.export_cloud_meeting_bundle(session_id)
    }

    pub(crate) fn import_into_store(
        self,
        store: &MeetingStore,
    ) -> Result<MeetingSessionId, StoreError> {
        self.validate()?;
        store.import_cloud_meeting_bundle(&self)
    }

    pub(crate) fn to_json_bytes(&self) -> Result<Vec<u8>, StoreError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| StoreError::Corrupt)?;
        if bytes.len() > MAX_CLOUD_MEETING_BUNDLE_BYTES {
            return Err(StoreError::Invalid);
        }
        Ok(bytes)
    }

    pub(crate) fn from_json_bytes(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.is_empty() || bytes.len() > MAX_CLOUD_MEETING_BUNDLE_BYTES {
            return Err(StoreError::Invalid);
        }
        let bundle: Self = serde_json::from_slice(bytes).map_err(|_| StoreError::Invalid)?;
        bundle.validate()?;
        Ok(bundle)
    }

    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        if self.format_version != CLOUD_MEETING_BUNDLE_VERSION || self.audio_included {
            return Err(StoreError::Invalid);
        }
        if !matches!(
            self.session.phase,
            MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
        ) {
            return Err(StoreError::Conflict);
        }
        if self.session.preflight.session_id != self.session.session_id
            || self.session.title.trim().is_empty()
        {
            return Err(StoreError::Invalid);
        }
        validate_text(&self.session.title)?;
        validate_optional_text(&self.session.diarization_model_id)?;
        validate_optional_text(&self.session.diarization_model_version)?;
        validate_row_budget(self)?;
        if let Some(notes) = &self.user_notes {
            if notes.session_id != self.session.session_id || i64::try_from(notes.revision).is_err() {
                return Err(StoreError::Invalid);
            }
            validate_text(&notes.body)?;
        }

        let plan_ids = unique_ids(self.run_plans.iter().map(|plan| plan.plan_id))?;
        let consent_ids = unique_ids(self.consents.iter().map(|consent| consent.consent_id))?;
        let mut plan_attempts = HashSet::new();
        for plan in &self.run_plans {
            if plan.attempt_number == 0
                || !plan_attempts.insert(plan.attempt_number)
                || plan.canonical_plan.plan_id != plan.plan_id
                || plan.canonical_plan.consent_id != plan.consent_id
                || plan.canonical_plan.session_id != self.session.session_id
                || !consent_ids.contains(&plan.consent_id)
            {
                return Err(StoreError::Invalid);
            }
            validate_text(&plan.canonical_plan.app_build)?;
            validate_text(&plan.canonical_plan.language)?;
        }
        if self
            .session
            .successful_plan_id
            .is_some_and(|plan_id| !plan_ids.contains(&plan_id))
        {
            return Err(StoreError::Invalid);
        }
        let mut consent_attempts = HashSet::new();
        for consent in &self.consents {
            if consent.attempt_number == 0
                || !consent_attempts.insert(consent.attempt_number)
                || consent.acknowledgement.consent_id != consent.consent_id
                || consent.acknowledgement.session_id != self.session.session_id
                || consent.acknowledgement.attempt_number != consent.attempt_number
            {
                return Err(StoreError::Invalid);
            }
        }

        let track_ids = unique_ids(self.source_tracks.iter().map(|track| track.track_id))?;
        let mut source_kinds = HashSet::new();
        for track in &self.source_tracks {
            if !plan_ids.contains(&track.plan_id)
                || !source_kinds.insert(track.source_kind)
                || track
                    .first_offset_ns
                    .zip(track.last_offset_ns)
                    .is_some_and(|(first, last)| first > last)
                || track.timestamp_bridge.native_timescale == 0
            {
                return Err(StoreError::Invalid);
            }
            if let Some(format) = track.format {
                if format.sample_rate_hz == 0 || format.channels == 0 {
                    return Err(StoreError::Invalid);
                }
            }
            validate_json_text(&track.descriptor_json)?;
        }
        let mut clock_keys = HashSet::new();
        for epoch in &self.source_clock_epochs {
            if !track_ids.contains(&epoch.track_id)
                || epoch.bridge.native_timescale == 0
                || !clock_keys.insert((
                    epoch.track_id,
                    epoch.source_epoch.get(),
                    epoch.format_epoch,
                ))
            {
                return Err(StoreError::Invalid);
            }
        }
        let mut window_sequences = HashSet::new();
        for window in &self.capture_windows {
            if !window_sequences.insert(window.sequence)
                || window
                    .end_offset_ns
                    .is_some_and(|end_offset_ns| end_offset_ns < window.start_offset_ns)
            {
                return Err(StoreError::Invalid);
            }
            validate_optional_text(&window.close_reason)?;
        }
        for gap in &self.source_gaps {
            if !track_ids.contains(&gap.track_id)
                || gap
                    .start_offset_ns
                    .zip(gap.end_offset_ns)
                    .is_some_and(|(start, end)| end < start)
            {
                return Err(StoreError::Invalid);
            }
        }

        let speaker_ids = unique_ids(self.speakers.iter().map(|speaker| speaker.speaker_id))?;
        let speaker_links: HashMap<_, _> = self
            .speakers
            .iter()
            .filter_map(|speaker| {
                speaker
                    .merged_into_speaker_id
                    .map(|merged_into| (speaker.speaker_id, merged_into))
            })
            .collect();
        for speaker in &self.speakers {
            if speaker.merged_into_speaker_id.is_some_and(|merged_into| {
                merged_into == speaker.speaker_id || !speaker_ids.contains(&merged_into)
            }) {
                return Err(StoreError::Invalid);
            }
            validate_text(&speaker.display_name)?;
        }
        for speaker_id in &speaker_ids {
            let mut seen = HashSet::new();
            let mut current = *speaker_id;
            while let Some(next) = speaker_links.get(&current) {
                if !seen.insert(current) {
                    return Err(StoreError::Invalid);
                }
                current = *next;
            }
        }

        let transcript_revision_ids = unique_ids(
            self.transcript_revisions
                .iter()
                .map(|revision| revision.transcript_revision_id),
        )?;
        for revision in &self.transcript_revisions {
            validate_text(&revision.engine_id)?;
            validate_optional_text(&revision.model_version)?;
            validate_text(&revision.language)?;
            validate_optional_text(&revision.error_code)?;
            if revision.source_set.is_empty()
                || revision
                    .completed_at_utc_ms
                    .is_some_and(|completed| completed < revision.created_at_utc_ms)
                || !task_timing_is_valid(
                    revision.state,
                    revision.completed_at_utc_ms,
                    revision.error_code.as_deref(),
                )
            {
                return Err(StoreError::Invalid);
            }
        }
        if self
            .session
            .current_transcript_revision_id
            .is_some_and(|revision_id| !transcript_revision_ids.contains(&revision_id))
        {
            return Err(StoreError::Invalid);
        }

        let segment_ids = unique_ids(
            self.transcript_segments
                .iter()
                .map(|segment| segment.segment_id),
        )?;
        if self.transcript_purged_at_utc_ms.is_some()
            && (!segment_ids.is_empty() || !self.segment_edits.is_empty() || !self.diarization_assignments.is_empty())
        {
            return Err(StoreError::Invalid);
        }
        let cited_segment_ids = self.transcript_purged_at_utc_ms.is_none().then_some(&segment_ids);
        let mut segment_ordinals = HashSet::new();
        for segment in &self.transcript_segments {
            if !transcript_revision_ids.contains(&segment.transcript_revision_id)
                || !track_ids.contains(&segment.track_id)
                || !speaker_ids.contains(&segment.speaker_id)
                || segment.start_offset_ns >= segment.end_offset_ns
                || segment.confidence_milli.is_some_and(|value| value > 1_000)
                || !segment_ordinals.insert((segment.transcript_revision_id, segment.ordinal))
            {
                return Err(StoreError::Invalid);
            }
            validate_text(&segment.base_text)?;
        }
        let mut edit_keys = HashSet::new();
        for edit in &self.segment_edits {
            if !segment_ids.contains(&edit.segment_id)
                || !edit_keys.insert((edit.segment_id, edit.edit_sequence))
            {
                return Err(StoreError::Invalid);
            }
            validate_text(&edit.replacement_text)?;
        }

        let note_ids = unique_ids(self.notes.iter().map(|note| note.note_id))?;
        for note in &self.notes {
            if note.session_id != self.session.session_id
                || note
                    .start_offset_ns
                    .zip(note.end_offset_ns)
                    .is_some_and(|(start, end)| end < start)
                || note.updated_at_utc_ms < note.created_at_utc_ms
            {
                return Err(StoreError::Invalid);
            }
            validate_text(&note.body)?;
        }

        let _artifact_ids = unique_ids(self.artifacts.iter().map(|artifact| artifact.artifact_id))?;
        for artifact in &self.artifacts {
            if artifact
                .transcript_revision_id
                .is_some_and(|revision_id| !transcript_revision_ids.contains(&revision_id))
            {
                return Err(StoreError::Invalid);
            }
        }
        let mut generation_keys = HashSet::new();
        for artifact in &self.artifact_revisions {
            if artifact.session_id != self.session.session_id
                || !transcript_revision_ids.contains(&artifact.transcript_revision_id)
                || !generation_keys.insert(artifact.generation_key.as_str())
            {
                return Err(StoreError::Invalid);
            }
            validate_text(&artifact.template_id)?;
            validate_text(&artifact.generation_key)?;
            validate_generated_artifacts(artifact.content.as_ref(), cited_segment_ids)?;
        }

        let _question_ids = unique_ids(self.questions.iter().map(|question| question.question_id))?;
        for question in &self.questions {
            if question.session_id != self.session.session_id
                || question.question.as_deref().is_none_or(str::is_empty)
                || !question_scope_is_local(&question.scope, self.session.session_id)
            {
                return Err(StoreError::Invalid);
            }
            validate_text(question.question.as_deref().ok_or(StoreError::Invalid)?)?;
            validate_optional_text(&question.answer)?;
            for citation in &question.citations {
                validate_citation(citation, self.session.session_id, cited_segment_ids, &note_ids)?;
            }
        }

        let generation_ids = unique_ids(
            self.diarization_generations
                .iter()
                .map(|generation| generation.generation_id),
        )?;
        for generation in &self.diarization_generations {
            if !transcript_revision_ids.contains(&generation.transcript_revision_id)
                || generation
                    .completed_at_utc_ms
                    .is_some_and(|completed| completed < generation.created_at_utc_ms)
                || !task_timing_is_valid(generation.state, generation.completed_at_utc_ms, None)
            {
                return Err(StoreError::Invalid);
            }
            validate_text(&generation.model_id)?;
            validate_text(&generation.model_version)?;
        }
        if self
            .session
            .current_diarization_generation_id
            .is_some_and(|generation_id| !generation_ids.contains(&generation_id))
        {
            return Err(StoreError::Invalid);
        }
        let mut assignment_keys = HashSet::new();
        for assignment in &self.diarization_assignments {
            if !generation_ids.contains(&assignment.generation_id)
                || !segment_ids.contains(&assignment.segment_id)
                || !speaker_ids.contains(&assignment.speaker_id)
                || !assignment_keys.insert((assignment.generation_id, assignment.segment_id))
            {
                return Err(StoreError::Invalid);
            }
        }
        Ok(())
    }
}

fn unique_ids<T>(values: impl Iterator<Item = T>) -> Result<HashSet<T>, StoreError>
where
    T: Eq + std::hash::Hash,
{
    let mut ids = HashSet::new();
    for value in values {
        if !ids.insert(value) {
            return Err(StoreError::Invalid);
        }
    }
    Ok(ids)
}

fn validate_row_budget(bundle: &CloudMeetingBundleV1) -> Result<(), StoreError> {
    let rows = [
        bundle.run_plans.len(),
        bundle.consents.len(),
        bundle.source_tracks.len(),
        bundle.source_clock_epochs.len(),
        bundle.capture_windows.len(),
        bundle.source_gaps.len(),
        bundle.speakers.len(),
        bundle.transcript_revisions.len(),
        bundle.transcript_segments.len(),
        bundle.segment_edits.len(),
        bundle.notes.len(),
        bundle.artifacts.len(),
        bundle.artifact_revisions.len(),
        bundle.questions.len(),
        bundle.diarization_generations.len(),
        bundle.diarization_assignments.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, count| total.checked_add(count))
    .ok_or(StoreError::Invalid)?;
    if rows > MAX_CLOUD_MEETING_BUNDLE_ROWS {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_text(value: &str) -> Result<(), StoreError> {
    if value.len() > MAX_CLOUD_MEETING_BUNDLE_TEXT_BYTES || value.contains('\0') {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_optional_text(value: &Option<String>) -> Result<(), StoreError> {
    value.as_deref().map(validate_text).transpose()?;
    Ok(())
}

fn validate_json_text(value: &str) -> Result<(), StoreError> {
    validate_text(value)?;
    serde_json::from_str::<serde_json::Value>(value).map_err(|_| StoreError::Invalid)?;
    Ok(())
}

fn task_timing_is_valid(
    state: CloudBundleTaskState,
    completed_at_utc_ms: Option<i64>,
    error_code: Option<&str>,
) -> bool {
    match state {
        CloudBundleTaskState::Running => completed_at_utc_ms.is_none() && error_code.is_none(),
        CloudBundleTaskState::Completed => completed_at_utc_ms.is_some() && error_code.is_none(),
        CloudBundleTaskState::Failed => error_code.is_some(),
    }
}

fn question_scope_is_local(scope: &MeetingQuestionScope, session_id: MeetingSessionId) -> bool {
    match scope {
        MeetingQuestionScope::ThisMeeting => true,
        MeetingQuestionScope::ExplicitSeries { session_ids } => {
            !session_ids.is_empty() && session_ids.iter().all(|candidate| *candidate == session_id)
        }
    }
}

fn validate_generated_artifacts(
    artifacts: Option<&GeneratedMeetingArtifacts>,
    segment_ids: Option<&HashSet<TranscriptSegmentId>>,
) -> Result<(), StoreError> {
    let Some(artifacts) = artifacts else {
        return Ok(());
    };
    validate_cited_artifact_text(&artifacts.summary, segment_ids)?;
    for entry in &artifacts.summary_trace {
        validate_artifact_citation(&entry.anchor, segment_ids)?;
    }
    for topic in &artifacts.outline {
        validate_cited_artifact_text(&topic.title, segment_ids)?;
        if let Some(detail) = &topic.detail {
            validate_cited_artifact_text(detail, segment_ids)?;
        }
    }
    for item in &artifacts.decisions {
        validate_cited_artifact_text(item, segment_ids)?;
    }
    for item in &artifacts.action_items {
        validate_cited_artifact_text(&item.text, segment_ids)?;
        validate_optional_text(&item.owner_text)?;
        validate_optional_text(&item.due_text)?;
    }
    for item in &artifacts.key_questions {
        validate_cited_artifact_text(item, segment_ids)?;
    }
    for item in &artifacts.risks {
        validate_cited_artifact_text(item, segment_ids)?;
    }
    validate_cited_artifact_text(&artifacts.follow_up_draft, segment_ids)
}

fn validate_cited_artifact_text(
    value: &CitedArtifactText,
    segment_ids: Option<&HashSet<TranscriptSegmentId>>,
) -> Result<(), StoreError> {
    validate_text(&value.text)?;
    for citation in &value.citations {
        validate_artifact_citation(citation, segment_ids)?;
    }
    Ok(())
}

/// A citation names a real span. Its segment must be present unless retention
/// deliberately removed all transcript text from this bundle.
fn validate_artifact_citation(
    citation: &ArtifactCitation,
    segment_ids: Option<&HashSet<TranscriptSegmentId>>,
) -> Result<(), StoreError> {
    if segment_ids.is_some_and(|ids| !ids.contains(&citation.segment_id))
        || citation.start_offset_ns >= citation.end_offset_ns
    {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_citation(
    citation: &MeetingCitation,
    session_id: MeetingSessionId,
    segment_ids: Option<&HashSet<TranscriptSegmentId>>,
    note_ids: &HashSet<ManualNoteId>,
) -> Result<(), StoreError> {
    if citation.session_id != session_id
        || citation
            .start_offset_ns
            .zip(citation.end_offset_ns)
            .is_some_and(|(start, end)| end < start)
    {
        return Err(StoreError::Invalid);
    }
    match citation.kind {
        CitationKind::Transcript => {
            let segment_id = TranscriptSegmentId::from_uuid(
                Uuid::parse_str(&citation.entity_id).map_err(|_| StoreError::Invalid)?,
            );
            if segment_ids.is_some_and(|ids| !ids.contains(&segment_id)) {
                return Err(StoreError::Invalid);
            }
        }
        CitationKind::ManualNote => {
            let note_id = ManualNoteId::from_uuid(
                Uuid::parse_str(&citation.entity_id).map_err(|_| StoreError::Invalid)?,
            );
            if !note_ids.contains(&note_id) {
                return Err(StoreError::Invalid);
            }
        }
        CitationKind::Title => {
            if citation.entity_id != session_id.uuid().to_string() {
                return Err(StoreError::Invalid);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest bundle the export path can legitimately produce: a review-ready
    /// session with no transcript yet. Every cross-reference set is empty, so the only
    /// invariants left are the ones this module owns at its byte boundary.
    fn review_ready_bundle() -> CloudMeetingBundleV1 {
        let session_id = MeetingSessionId::new();
        CloudMeetingBundleV1 {
            format_version: CLOUD_MEETING_BUNDLE_VERSION,
            audio_included: false,
            transcript_purged_at_utc_ms: None,
            retained_analytics: None,
            session: CloudBundleSession {
                session_id,
                phase: MeetingPhase::ReviewReady,
                revision: 3,
                title: "Design sync".to_owned(),
                origin: MeetingOrigin::Manual,
                preflight: MeetingPreflightSnapshot {
                    session_id,
                    revision: 0,
                    proposed_title: "Design sync".to_owned(),
                    origin: MeetingOrigin::Manual,
                    sources: Vec::new(),
                    storage: StorageAvailability::Available,
                    local_processing: SourceAvailability::Available,
                    destination: ProcessingDestination::Local,
                    microphone_device_uid: None,
                    frozen_system_audio_application_bundle_ids: Vec::new(),
                    accepted_known_missing_sources: Vec::new(),
                    degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                    required_acknowledgements: Vec::new(),
                    allowed_actions: Vec::new(),
                },
                created_at_utc_ms: 1,
                started_at_utc_ms: Some(2),
                ended_at_utc_ms: Some(3),
                recovered_at_utc_ms: None,
                successful_plan_id: None,
                processing_status: ProcessingStatus::Succeeded,
                retention_policy: MeetingRetentionPolicy::Forever,
                delete_after_utc_ms: None,
                current_transcript_revision_id: None,
                current_diarization_generation_id: None,
                diarization_status: DiarizationStatus::NotRequested,
                diarization_model_id: None,
                diarization_model_version: None,
            },
            run_plans: Vec::new(),
            consents: Vec::new(),
            source_tracks: Vec::new(),
            source_clock_epochs: Vec::new(),
            capture_windows: Vec::new(),
            source_gaps: Vec::new(),
            speakers: Vec::new(),
            transcript_revisions: Vec::new(),
            transcript_segments: Vec::new(),
            segment_edits: Vec::new(),
            notes: Vec::new(),
            user_notes: None,
            artifacts: Vec::new(),
            artifact_revisions: Vec::new(),
            questions: Vec::new(),
            diarization_generations: Vec::new(),
            diarization_assignments: Vec::new(),
        }
    }

    fn tampered(
        bundle: &CloudMeetingBundleV1,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> Vec<u8> {
        let mut document: serde_json::Value =
            serde_json::from_slice(&bundle.to_json_bytes().expect("valid bundle serializes"))
                .expect("bundle bytes are JSON");
        edit(&mut document);
        serde_json::to_vec(&document).expect("tampered document serializes")
    }

    #[test]
    fn a_bundle_survives_its_own_bytes() {
        let bundle = review_ready_bundle();
        let bytes = bundle.to_json_bytes().expect("valid bundle serializes");

        assert_eq!(
            CloudMeetingBundleV1::from_json_bytes(&bytes).expect("own bytes parse"),
            bundle
        );
    }

    #[test]
    fn purged_bundle_keeps_cited_notes_and_metrics_without_requiring_deleted_segments() {
        let mut bundle = review_ready_bundle();
        let session_id = bundle.session.session_id;
        let revision_id = TranscriptRevisionId::new();
        let segment_id = TranscriptSegmentId::new();
        bundle.transcript_revisions.push(CloudBundleTranscriptRevision {
            transcript_revision_id: revision_id, engine_id: "local".to_owned(), model_version: None,
            destination: ProcessingDestination::Local, source_set: vec![SourceKind::Microphone],
            language: "en".to_owned(), state: CloudBundleTaskState::Completed,
            created_at_utc_ms: 1, completed_at_utc_ms: Some(2), error_code: None,
        });
        bundle.session.current_transcript_revision_id = Some(revision_id);
        let content: GeneratedMeetingArtifacts = serde_json::from_value(serde_json::json!({
            "summary": {"text": "Keep the decision", "citations": [{
                "segment_id": segment_id, "start_offset_ns": 1, "end_offset_ns": 2
            }]},
            "outline": [], "decisions": [], "action_items": [], "key_questions": [], "risks": [],
            "follow_up_draft": {"text": "Keep the follow-up", "citations": []}
        })).expect("generated notes");
        bundle.artifact_revisions.push(MeetingArtifactRevision {
            artifact_id: MeetingArtifactId::new(), session_id, transcript_revision_id: revision_id,
            input_revision: 2, template_id: "meeting-review".to_owned(), template_version: 1,
            generation_key: "retained-notes".to_owned(), state: MeetingArtifactState::Current,
            generated_at_utc_ms: 3, content: Some(content),
        });
        bundle.questions.push(MeetingAnswer {
            question_id: MeetingQuestionId::new(), session_id, scope: MeetingQuestionScope::ThisMeeting,
            question: Some("What was decided?".to_owned()), state: MeetingAnswerState::Supported,
            answer: Some("Keep the draft.".to_owned()), citations: vec![MeetingCitation {
                kind: CitationKind::Transcript, session_id, entity_id: segment_id.uuid().to_string(),
                start_offset_ns: Some(1), end_offset_ns: Some(2),
            }], input_revision: 2, revision: 1, created_at_utc_ms: 3, through_offset_ns: None, provisional: false,
        });
        assert_eq!(bundle.validate(), Err(StoreError::Invalid));
        bundle.transcript_purged_at_utc_ms = Some(10);
        let mut metrics = super::super::analytics::MeetingAnalytics::default();
        metrics.talk.segment_count = 7;
        bundle.retained_analytics = Some(metrics.clone());
        let bytes = bundle.to_json_bytes().expect("purged notes serialize");
        let restored = CloudMeetingBundleV1::from_json_bytes(&bytes).expect("purged notes deserialize");
        assert_eq!(restored, bundle);
        let (_dir, store) = super::super::store::workflow_core_tests::store();
        restored.import_into_store(&store).expect("purged bundle imports");
        let exported = CloudMeetingBundleV1::export_from_store(&store, session_id).expect("purged bundle exports");
        assert_eq!(exported.artifact_revisions, bundle.artifact_revisions);
        assert_eq!(exported.questions, bundle.questions);
        assert_eq!(exported.retained_analytics, Some(metrics));
        assert_eq!(exported.transcript_purged_at_utc_ms, Some(10));
        assert!(exported.transcript_segments.is_empty());
        assert_eq!(store.require_retained_transcript(session_id), Err(StoreError::TranscriptDeleted));

        let invalid_span = tampered(&bundle, |value| {
            value["artifact_revisions"][0]["content"]["summary"]["citations"][0]["end_offset_ns"] = serde_json::json!(1);
        });
        assert_eq!(CloudMeetingBundleV1::from_json_bytes(&invalid_span), Err(StoreError::Invalid));
        bundle.segment_edits.push(CloudBundleSegmentEdit {
            segment_id, edit_sequence: 1, replacement_text: "Must not survive".to_owned(),
            removed: false, operator_at_utc_ms: 3,
        });
        assert_eq!(bundle.validate(), Err(StoreError::Invalid));
    }

    #[test]
    fn old_bundle_without_retention_marker_stays_a_retained_transcript() {
        let bundle = review_ready_bundle();
        let bytes = bundle.to_json_bytes().expect("old bundle shape");
        let decoded = CloudMeetingBundleV1::from_json_bytes(&bytes).expect("old bundle opens");
        assert_eq!(decoded.transcript_purged_at_utc_ms, None);
        let (_dir, store) = super::super::store::workflow_core_tests::store();
        decoded.import_into_store(&store).expect("old bundle imports");
        assert_eq!(store.require_retained_transcript(bundle.session.session_id), Ok(()));
    }

    #[test]
    fn own_notes_round_trip_and_old_bundles_still_open() {
        let mut bundle = review_ready_bundle();
        let legacy = bundle.to_json_bytes().expect("legacy bundle");
        assert!(CloudMeetingBundleV1::from_json_bytes(&legacy).expect("old bundle").user_notes.is_none());
        let mut notes = super::super::analytics::MeetingUserNotes::empty(
            bundle.session.session_id, super::super::analytics::MeetingNotesTemplate::General);
        notes.body = "Ask about the renewal date.".to_owned();
        notes.revision = 1;
        bundle.user_notes = Some(notes.clone());
        assert_eq!(CloudMeetingBundleV1::from_json_bytes(&bundle.to_json_bytes().expect("notes bundle"))
            .expect("notes round trip").user_notes, Some(notes));
        let foreign = tampered(&bundle, |value| {
            value["user_notes"]["session_id"] = serde_json::json!(MeetingSessionId::new());
        });
        assert_eq!(CloudMeetingBundleV1::from_json_bytes(&foreign), Err(StoreError::Invalid));
    }

    #[test]
    fn a_bundle_claiming_audio_is_refused_at_the_byte_boundary() {
        let bytes = tampered(&review_ready_bundle(), |document| {
            document["audio_included"] = serde_json::Value::Bool(true);
        });

        assert_eq!(
            CloudMeetingBundleV1::from_json_bytes(&bytes),
            Err(StoreError::Invalid)
        );
    }

    #[test]
    fn a_bundle_whose_preflight_names_another_session_is_refused() {
        let bytes = tampered(&review_ready_bundle(), |document| {
            document["session"]["preflight"]["session_id"] =
                serde_json::Value::String(Uuid::new_v4().to_string());
        });

        assert_eq!(
            CloudMeetingBundleV1::from_json_bytes(&bytes),
            Err(StoreError::Invalid)
        );
    }

    #[test]
    fn a_bundle_still_capturing_is_refused_as_a_conflict() {
        let bytes = tampered(&review_ready_bundle(), |document| {
            document["session"]["phase"] = serde_json::json!("capturing_recording");
        });

        assert_eq!(
            CloudMeetingBundleV1::from_json_bytes(&bytes),
            Err(StoreError::Conflict)
        );
    }

    #[test]
    fn a_truncated_or_empty_bundle_is_refused_at_every_length() {
        let bytes = review_ready_bundle()
            .to_json_bytes()
            .expect("valid bundle serializes");

        assert_eq!(
            CloudMeetingBundleV1::from_json_bytes(&[]),
            Err(StoreError::Invalid)
        );
        for length in 1..bytes.len() {
            assert_eq!(
                CloudMeetingBundleV1::from_json_bytes(&bytes[..length]),
                Err(StoreError::Invalid),
                "a {length}-byte prefix of a bundle must not parse"
            );
        }
    }

    #[test]
    fn a_bundle_carrying_an_unknown_field_is_refused() {
        let bytes = tampered(&review_ready_bundle(), |document| {
            document["audio_relative_path"] = serde_json::json!("audio/track.wav");
        });

        assert_eq!(
            CloudMeetingBundleV1::from_json_bytes(&bytes),
            Err(StoreError::Invalid)
        );
    }
}
