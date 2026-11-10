use super::people::{
    bump_people_revision_in, normalized, people_revision_in, person_by_id_in,
    person_for_named_speaker_in, upsert_link_in,
};
use super::{
    append_event, committed_receipt, decode_json, id, insert_operation_receipt,
    mark_artifacts_out_of_date, operation_receipt_in, phase_from_db, rebuild_search_documents_in,
    session_row, to_i64, utc_now_ms, DurableTrackRecord, MeetingStore, SessionRow, StoreError,
    StoreMutation, LOCAL_SPEAKER_LABEL,
};
use crate::meeting::diarization::{
    cosine_similarity, match_speaker_profile, pair_same_voices, wespeaker_embedding_model_key,
    SpeakerEmbedding, SpeakerEmbeddingModelKey, SPEAKER_EMBEDDING_DIMENSIONS,
};
use crate::meeting::people_types::{
    PersonId, PersonLinkConfidence, PersonLinkSource, VoiceProfileMergeResolution,
};
use crate::meeting::types::{
    MeetingCommandKind, MeetingDiarizationGenerationId, MeetingOperationId, MeetingOrigin,
    MeetingPhase, MeetingSessionId, OperationActor, OperationReceipt, SourceKind, SourceTrackId,
    SpeakerId,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use std::num::NonZeroU32;
const EMBEDDING_BYTES: usize = SPEAKER_EMBEDDING_DIMENSIONS * size_of::<f32>();
// The voice migration spells these two numbers as literals it cannot compute:
// `embedding_dimensions = 256` and `length(centroid) = 1024`. Sample rate and
// feature bins come from the model manifest at runtime and are checked there.
const _: () = assert!(SPEAKER_EMBEDDING_DIMENSIONS == 256 && EMBEDDING_BYTES == 1024);
const MIN_ENROLLMENT_EVIDENCE_DURATION_NS: u64 = 4_000_000_000;
const MAX_ENROLLMENT_EVIDENCE_DURATION_NS: u64 = 12_000_000_000;
/// How long a voice nobody named is kept after the run that heard it.
const UNNAMED_VOICE_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// A match must be at least 30% closer than a competing identity in each supporting meeting.
const CONFIRMED_VOICE_DISTANCE_RATIO: f32 = 0.7;
/// Reject unrelated voices even when no competing identity has a retained fingerprint.
const MAX_CONFIRMED_VOICE_DISTANCE: f32 = 0.4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiarizationEvidenceKind {
    SortformerExclusive,
    WeSpeakerUnambiguousWindow,
}

impl DiarizationEvidenceKind {
    const fn as_db(self) -> &'static str {
        match self {
            Self::SortformerExclusive => "sortformer_exclusive",
            Self::WeSpeakerUnambiguousWindow => "wespeaker_unambiguous_window",
        }
    }
}
/// One non-biometric diarization fact that can justify local enrollment later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DiarizationEvidenceSpanInput {
    pub generation_id: MeetingDiarizationGenerationId,
    pub speaker_id: SpeakerId,
    pub track_id: SourceTrackId,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub kind: DiarizationEvidenceKind,
}

/// The one source range retained for a human-approved meeting sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VoiceEnrollmentEvidence {
    session_id: MeetingSessionId,
    generation_id: MeetingDiarizationGenerationId,
    speaker_id: SpeakerId,
    track_id: SourceTrackId,
    start_offset_ns: u64,
    end_offset_ns: u64,
}

impl VoiceEnrollmentEvidence {
    pub(crate) fn new(
        session_id: MeetingSessionId,
        generation_id: MeetingDiarizationGenerationId,
        speaker_id: SpeakerId,
        track_id: SourceTrackId,
        start_offset_ns: u64,
        end_offset_ns: u64,
    ) -> Result<Self, StoreError> {
        if end_offset_ns <= start_offset_ns {
            return Err(StoreError::Invalid);
        }
        Ok(Self {
            session_id,
            generation_id,
            speaker_id,
            track_id,
            start_offset_ns,
            end_offset_ns,
        })
    }

    pub(crate) const fn session_id(self) -> MeetingSessionId {
        self.session_id
    }

    pub(crate) const fn track_id(self) -> SourceTrackId {
        self.track_id
    }

    pub(crate) const fn start_offset_ns(self) -> u64 {
        self.start_offset_ns
    }

    pub(crate) const fn end_offset_ns(self) -> u64 {
        self.end_offset_ns
    }
}
pub(crate) struct VoiceEnrollmentRecord {
    pub record: DurableTrackRecord,
    pub approved_spans: Vec<VoiceEnrollmentAudioSpan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VoiceEnrollmentAudioSpan {
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
}

/// Construction is the explicit-consent boundary; an absent or zero version
/// cannot become an enrollment request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExplicitVoiceConsent(NonZeroU32);

impl ExplicitVoiceConsent {
    pub(crate) fn granted(version: u32) -> Result<Self, StoreError> {
        NonZeroU32::new(version)
            .map(Self)
            .ok_or(StoreError::ExplicitConsentRequired)
    }

    const fn version(self) -> u32 {
        self.0.get()
    }
}

/// An already-normalized, backend-derived meeting sample. The store never
/// accepts a path, PCM, bytes, or an unvalidated vector.
pub(crate) struct VoiceProfileEnrollmentRequest {
    pub person_id: PersonId,
    pub expected_meeting_revision: u64,
    pub expected_people_revision: u64,
    pub expected_speaker_revision: u64,
    pub consent: ExplicitVoiceConsent,
    pub evidence: VoiceEnrollmentEvidence,
    pub model: SpeakerEmbeddingModelKey,
    pub embedding: SpeakerEmbedding,
    pub committed_at_utc_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VoiceProfileModelStatus {
    pub model_id: String,
    pub model_revision: String,
    pub model_sha256: String,
    pub embedding_dimensions: usize,
    pub sample_rate_hz: u32,
    pub feature_bins: usize,
    pub feature_pipeline_revision: String,
    pub normalization: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VoiceProfileStatus {
    Unenrolled,
    Enrolled {
        model: VoiceProfileModelStatus,
        sample_count: u64,
        profile_revision: u64,
        consent_version: u32,
    },
}

/// A matching decision that cannot be fabricated outside this module. It carries
/// its target, selected profile revision, and profile-set people fence, never a
/// centroid or sample identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SuccessfulVoiceProfileMatch {
    person_id: PersonId,
    profile_revision: u64,
    people_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VoiceSpeakerMatchOverlay {
    pub person_id: PersonId,
    pub profile_revision: u64,
    pub speaker_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MeetingSpeakerIdentifyTarget {
    Existing(PersonId),
    Create { display_name: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MeetingSpeakerIdentifyDisposition {
    Label {
        target: MeetingSpeakerIdentifyTarget,
        /// Speakers of other meetings with the same voice, named too.
        same_voice: Vec<SameVoiceSpeaker>,
    },
    CorrectTo {
        target: MeetingSpeakerIdentifyTarget,
    },
    MarkUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MeetingSpeakerIdentifyRequest {
    pub operation_id: MeetingOperationId,
    pub requested_at_utc_ms: i64,
    pub session_id: MeetingSessionId,
    pub expected_meeting_revision: u64,
    pub expected_people_revision: u64,
    pub speaker_id: SpeakerId,
    pub disposition: MeetingSpeakerIdentifyDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VoiceSpeakerIdentityResult {
    pub receipt: OperationReceipt,
    pub resolved_person_id: Option<PersonId>,
    /// Each other meeting the label named the same voice in, at its new
    /// revision.
    pub same_voice_revisions: Vec<(MeetingSessionId, u64)>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceSpeakerSuggestion {
    pub speaker_id: SpeakerId,
    /// None when the voice is the user's own, named `display_name` in About me.
    pub person_id: Option<PersonId>,
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum VoiceNameResolution {
    NamedByVoice,
    VoiceMatch,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceNamedSpeaker {
    pub speaker_id: SpeakerId,
    pub original_display_name: String,
    pub resolution_kind: VoiceNameResolution,
}

/// A speaker of another meeting that a label names too, at the revision the
/// user saw.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SameVoiceSpeaker {
    pub session_id: MeetingSessionId,
    pub speaker_id: SpeakerId,
    pub expected_speaker_revision: u64,
}

/// A speaker still waiting for a name, and the other meetings where the same
/// voice was heard and nobody named it either, oldest first.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SameVoice {
    pub speaker_id: SpeakerId,
    pub meetings: Vec<SameVoiceMeeting>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SameVoiceMeeting {
    pub session_id: MeetingSessionId,
    pub speaker_id: SpeakerId,
    pub speaker_revision: u64,
    pub title: String,
    pub started_at_utc_ms: i64,
    /// Where only this voice talks, to hear it by; none once the recording
    /// is gone or when no stretch is long enough.
    pub clip: Option<VoiceClip>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceClip {
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
}

/// The user's own voice as Sona learned it from their dictations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct UserVoiceprint {
    pub dictation_count: u32,
    pub speech_ms: u64,
}

/// A print of the user's own voice, made from their dictations once they
/// asked for it.
pub(crate) struct UserVoiceprintRequest {
    pub consent: ExplicitVoiceConsent,
    pub model: SpeakerEmbeddingModelKey,
    pub embedding: SpeakerEmbedding,
    pub voiceprint: UserVoiceprint,
    pub created_at_utc_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManualVoiceSpeakerResolution {
    Identified(PersonId),
    Unknown,
}

impl ManualVoiceSpeakerResolution {
    const fn kind(self) -> &'static str {
        match self {
            Self::Identified(_) => "identified",
            Self::Unknown => "unknown",
        }
    }

    const fn resolved_person_id(self) -> Option<PersonId> {
        match self {
            Self::Identified(person_id) => Some(person_id),
            Self::Unknown => None,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct VoiceProfileRow {
    person_id: PersonId,
    model: VoiceProfileModelStatus,
    sample_count: u64,
    profile_revision: u64,
    consent_version: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::meeting::store) struct VoiceEvidenceChange {
    pub profiles_changed: bool,
}

impl VoiceEvidenceChange {
    pub(in crate::meeting::store) const fn people_changed(self) -> bool {
        self.profiles_changed
    }
}

impl MeetingStore {
    /// Replace all evidence for one still-pending generation in one transaction.
    pub(crate) fn replace_diarization_evidence_spans(
        &self,
        generation_id: MeetingDiarizationGenerationId,
        spans: &[DiarizationEvidenceSpanInput],
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owner = pending_generation_session_in(&transaction, generation_id)?;
        let mut keys = HashSet::with_capacity(spans.len());
        for span in spans {
            if span.generation_id != generation_id || span.end_offset_ns <= span.start_offset_ns {
                return Err(StoreError::Invalid);
            }
            if !keys.insert((span.speaker_id, span.start_offset_ns, span.end_offset_ns)) {
                return Err(StoreError::Invalid);
            }
            require_evidence_span_owner_in(&transaction, owner, span)?;
        }
        transaction.execute(
            "DELETE FROM meeting_diarization_evidence_spans WHERE generation_id = ?1",
            params![id(generation_id)],
        )?;
        for span in spans {
            transaction.execute(
                "INSERT INTO meeting_diarization_evidence_spans (
                    generation_id, speaker_id, track_id, start_offset_ns, end_offset_ns, evidence_kind
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id(generation_id),
                    id(span.speaker_id),
                    id(span.track_id),
                    to_i64(span.start_offset_ns)?,
                    to_i64(span.end_offset_ns)?,
                    span.kind.as_db(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn local_voice_enrollment_evidence(
        &self,
        session_id: MeetingSessionId,
        speaker_id: SpeakerId,
    ) -> Result<VoiceEnrollmentEvidence, StoreError> {
        let connection = self.connection()?;
        super::transcript_retention::require_retained_transcript_in(&connection, session_id)?;
        let mut statement = connection.prepare(
            "SELECT evidence.generation_id, evidence.track_id,
                    evidence.start_offset_ns, evidence.end_offset_ns
               FROM meeting_diarization_evidence_spans evidence
               JOIN meeting_diarization_generations generation
                 ON generation.generation_id = evidence.generation_id
               JOIN meeting_speakers speaker ON speaker.speaker_id = evidence.speaker_id
              WHERE generation.session_id = ?1
                AND speaker.session_id = ?1
                AND evidence.speaker_id = ?2
                AND speaker.merged_into_speaker_id IS NULL
                AND generation.state = 'completed'
              ORDER BY generation.completed_at_utc_ms DESC, evidence.track_id,
                       evidence.start_offset_ns, evidence.end_offset_ns
              LIMIT 128",
        )?;
        let rows = statement
            .query_map(params![id(session_id), id(speaker_id)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut run: Option<(String, String, u64, u64)> = None;
        for (generation_id, track_id, start_offset_ns, end_offset_ns) in rows {
            let start_offset_ns = from_i64(start_offset_ns)?;
            let end_offset_ns = from_i64(end_offset_ns)?;
            if end_offset_ns <= start_offset_ns {
                return Err(StoreError::VoiceInvariant);
            }
            match &mut run {
                Some((run_generation, run_track, _, run_end))
                    if *run_generation == generation_id
                        && *run_track == track_id
                        && *run_end == start_offset_ns =>
                {
                    *run_end = end_offset_ns;
                }
                _ => run = Some((generation_id, track_id, start_offset_ns, end_offset_ns)),
            }
            if let Some((generation_id, track_id, run_start, run_end)) = &run {
                let duration = run_end
                    .checked_sub(*run_start)
                    .ok_or(StoreError::VoiceInvariant)?;
                if duration >= MIN_ENROLLMENT_EVIDENCE_DURATION_NS {
                    return VoiceEnrollmentEvidence::new(
                        session_id,
                        MeetingDiarizationGenerationId::from_uuid(parse_uuid(generation_id)?),
                        speaker_id,
                        SourceTrackId::from_uuid(parse_uuid(track_id)?),
                        *run_start,
                        run_start
                            .checked_add(duration.min(MAX_ENROLLMENT_EVIDENCE_DURATION_NS))
                            .ok_or(StoreError::VoiceInvariant)?,
                    );
                }
            }
        }
        Err(StoreError::InsufficientEnrollmentEvidence)
    }
    pub(crate) fn visit_voice_enrollment_evidence_records<F>(
        &self,
        evidence: VoiceEnrollmentEvidence,
        mut visitor: F,
    ) -> Result<(), StoreError>
    where
        F: FnMut(VoiceEnrollmentRecord) -> Result<(), StoreError>,
    {
        let evidence_spans = {
            let mut connection = self.connection()?;
            let transaction = connection.transaction()?;
            require_enrollment_evidence_in(&transaction, evidence)?;
            let rows = {
                let mut statement = transaction.prepare(
                    "SELECT start_offset_ns, end_offset_ns
                       FROM meeting_diarization_evidence_spans
                      WHERE generation_id = ?1 AND speaker_id = ?2 AND track_id = ?3
                        AND start_offset_ns < ?5 AND end_offset_ns > ?4
                      ORDER BY start_offset_ns, end_offset_ns",
                )?;
                let queried_rows = statement
                    .query_map(
                        params![
                            id(evidence.generation_id),
                            id(evidence.speaker_id),
                            id(evidence.track_id),
                            to_i64(evidence.start_offset_ns)?,
                            to_i64(evidence.end_offset_ns)?,
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                queried_rows
            };
            transaction.commit()?;
            rows.into_iter()
                .map(|(start_offset_ns, end_offset_ns)| {
                    Ok(VoiceEnrollmentAudioSpan {
                        start_offset_ns: from_i64(start_offset_ns)?.max(evidence.start_offset_ns),
                        end_offset_ns: from_i64(end_offset_ns)?.min(evidence.end_offset_ns),
                    })
                })
                .collect::<Result<Vec<_>, StoreError>>()?
        };
        self.visit_durable_track_records(
            evidence.session_id(),
            evidence.track_id(),
            None,
            |record| {
                let record_end = record
                    .start_offset_ns
                    .checked_add(record.duration_ns)
                    .ok_or(StoreError::VoiceInvariant)?;
                let mut approved_spans: Vec<VoiceEnrollmentAudioSpan> =
                    Vec::with_capacity(evidence_spans.len());
                for span in &evidence_spans {
                    let start_offset_ns = record.start_offset_ns.max(span.start_offset_ns);
                    let end_offset_ns = record_end.min(span.end_offset_ns);
                    if start_offset_ns >= end_offset_ns {
                        continue;
                    }
                    if let Some(previous) = approved_spans.last_mut() {
                        if previous.end_offset_ns == start_offset_ns {
                            previous.end_offset_ns = end_offset_ns;
                            continue;
                        }
                    }
                    approved_spans.push(VoiceEnrollmentAudioSpan {
                        start_offset_ns,
                        end_offset_ns,
                    });
                }
                if approved_spans.is_empty() {
                    Ok(())
                } else {
                    visitor(VoiceEnrollmentRecord {
                        record,
                        approved_spans,
                    })
                }
            },
        )
    }

    /// Persist exactly one normalized sample for one explicitly approved meeting.
    pub(crate) fn commit_voice_profile_enrollment(
        &self,
        request: VoiceProfileEnrollmentRequest,
    ) -> Result<VoiceProfileStatus, StoreError> {
        require_expected_model(request.model)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_voice_people_revision_in(&transaction, request.expected_people_revision)?;
        require_voice_person_in(&transaction, request.person_id)?;
        require_enrollment_evidence_in(&transaction, request.evidence)?;
        require_enrollment_authorization_in(
            &transaction,
            request.evidence,
            request.person_id,
            request.expected_meeting_revision,
            request.expected_speaker_revision,
        )?;

        if let Some(owner) = voice_profile_sample_owner_in(&transaction, request.evidence)? {
            if owner != request.person_id {
                return Err(StoreError::Conflict);
            }
            let profile = profile_row_in(&transaction, request.person_id)?
                .ok_or(StoreError::VoiceInvariant)?;
            require_profile_model(&profile, request.model)?;
            transaction.commit()?;
            return Ok(status_from_row(profile));
        }
        let existing = profile_row_in(&transaction, request.person_id)?;
        let next_profile_revision = match existing.as_ref() {
            Some(profile) => {
                require_profile_model(profile, request.model)?;
                profile
                    .profile_revision
                    .checked_add(1)
                    .ok_or(StoreError::VoiceInvariant)?
            }
            None => people_revision_in(&transaction)?
                .checked_add(1)
                .ok_or(StoreError::VoiceInvariant)?,
        };
        if existing.is_none() {
            insert_profile_in(
                &transaction,
                request.person_id,
                request.model,
                request.embedding.as_slice(),
                1,
                next_profile_revision,
                request.consent.version(),
                request.committed_at_utc_ms,
            )?;
        }
        transaction.execute(
            "INSERT INTO voice_profile_samples (
                person_id, source_session_id, source_generation_id, source_speaker_id, source_track_id,
                start_offset_ns, end_offset_ns, embedding, created_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id(request.person_id),
                id(request.evidence.session_id),
                id(request.evidence.generation_id),
                id(request.evidence.speaker_id),
                id(request.evidence.track_id),
                to_i64(request.evidence.start_offset_ns)?,
                to_i64(request.evidence.end_offset_ns)?,
                embedding_blob(&request.embedding),
                request.committed_at_utc_ms,
            ],
        )?;
        recompute_profile_in(
            &transaction,
            request.person_id,
            next_profile_revision,
            request.committed_at_utc_ms,
        )?;
        bump_people_revision_in(&transaction)?;
        let status = status_from_row(
            profile_row_in(&transaction, request.person_id)?.ok_or(StoreError::VoiceInvariant)?,
        );
        transaction.commit()?;
        Ok(status)
    }

    pub(crate) fn remove_voice_profile(
        &self,
        person_id: PersonId,
        expected_people_revision: u64,
    ) -> Result<VoiceProfileStatus, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_voice_people_revision_in(&transaction, expected_people_revision)?;
        require_voice_person_in(&transaction, person_id)?;
        forget_person_voiceprints_in(&transaction, person_id)?;
        let change = remove_voice_person_evidence_in(&transaction, person_id)?;
        if change.people_changed() {
            bump_people_revision_in(&transaction)?;
        }
        transaction.commit()?;
        Ok(VoiceProfileStatus::Unenrolled)
    }

    pub(crate) fn active_voice_speaker_revision(
        &self,
        session_id: MeetingSessionId,
        speaker_id: SpeakerId,
    ) -> Result<u64, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let revision = active_speaker_revision_in(&transaction, session_id, speaker_id)?;
        transaction.commit()?;
        Ok(revision)
    }
    /// The meeting's speakers with no manual answer, automatic name or enrolled voice match.
    pub(crate) fn unresolved_active_voice_speaker_ids(
        &self,
        session_id: MeetingSessionId,
        user_name: Option<&str>,
    ) -> Result<Vec<SpeakerId>, StoreError> {
        let connection = self.connection()?;
        let model = wespeaker_embedding_model_key();
        let mut statement = connection.prepare(
            "SELECT speaker.speaker_id, speaker.display_name FROM meeting_speakers speaker
              WHERE speaker.session_id = ?1
                AND speaker.merged_into_speaker_id IS NULL
                AND NOT EXISTS (
                    SELECT 1
                      FROM meeting_voice_speaker_resolutions manual
                     WHERE manual.speaker_id = speaker.speaker_id
                       AND manual.is_current = 1
                       AND manual.resolution_kind IN ('identified', 'unknown')
                )
                AND NOT EXISTS (
                    SELECT 1 FROM meeting_voice_names named
                     WHERE named.speaker_id = speaker.speaker_id
                       AND named.is_current = 1
                )
                AND NOT EXISTS (
                    SELECT 1
                      FROM voice_speaker_matches matched
                      JOIN voice_profiles profile ON profile.person_id = matched.person_id
                     WHERE matched.speaker_id = speaker.speaker_id
                       AND matched.session_id = speaker.session_id
                       AND matched.speaker_revision = speaker.revision
                       AND matched.profile_revision = profile.profile_revision
                       AND profile.model_id = ?2 AND profile.model_revision = ?3
                       AND profile.model_sha256 = ?4 AND profile.embedding_dimensions = ?5
                       AND profile.sample_rate_hz = ?6 AND profile.feature_bins = ?7
                       AND profile.feature_pipeline_revision = ?8 AND profile.normalization = ?9
                       AND profile.sample_count > 0
                )
              ORDER BY speaker.speaker_id",
        )?;
        let rows = statement.query_map(
            params![
                id(session_id),
                model.model_id,
                model.model_revision,
                model.model_sha256,
                i64::try_from(model.embedding_dimensions)
                    .map_err(|_| StoreError::VoiceInvariant)?,
                i64::from(model.sample_rate_hz),
                i64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?,
                model.feature_pipeline_revision,
                model.normalization,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut unresolved = Vec::new();
        for row in rows {
            let (speaker_id, display_name) = row?;
            if !names_user(&display_name, user_name) {
                unresolved.push(SpeakerId::from_uuid(parse_uuid(&speaker_id)?));
            }
        }
        Ok(unresolved)
    }

    /// Remember this run's voice for each diarized speaker, replacing what an
    /// earlier run kept for the same speaker. A speaker merged away or from
    /// another meeting is skipped.
    pub(crate) fn store_speaker_voiceprints(
        &self,
        session_id: MeetingSessionId,
        model: SpeakerEmbeddingModelKey,
        voiceprints: &[(SpeakerId, SpeakerEmbedding)],
        created_at_utc_ms: i64,
    ) -> Result<(), StoreError> {
        require_expected_model(model)?;
        let embedding_dimensions =
            i64::try_from(model.embedding_dimensions).map_err(|_| StoreError::VoiceInvariant)?;
        let feature_bins =
            i64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (speaker_id, embedding) in voiceprints {
            transaction.execute(
                "INSERT INTO meeting_speaker_voiceprints (
                    speaker_id, session_id, model_id, model_revision, model_sha256,
                    embedding_dimensions, sample_rate_hz, feature_bins,
                    feature_pipeline_revision, normalization, embedding, created_at_utc_ms
                 )
                 SELECT speaker_id, session_id, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
                   FROM meeting_speakers
                  WHERE speaker_id = ?1 AND session_id = ?2 AND merged_into_speaker_id IS NULL
                 ON CONFLICT(speaker_id) DO UPDATE SET
                    model_id = excluded.model_id,
                    model_revision = excluded.model_revision,
                    model_sha256 = excluded.model_sha256,
                    embedding_dimensions = excluded.embedding_dimensions,
                    sample_rate_hz = excluded.sample_rate_hz,
                    feature_bins = excluded.feature_bins,
                    feature_pipeline_revision = excluded.feature_pipeline_revision,
                    normalization = excluded.normalization,
                    embedding = excluded.embedding,
                    created_at_utc_ms = excluded.created_at_utc_ms",
                params![
                    id(*speaker_id),
                    id(session_id),
                    model.model_id,
                    model.model_revision,
                    model.model_sha256,
                    embedding_dimensions,
                    i64::from(model.sample_rate_hz),
                    feature_bins,
                    model.feature_pipeline_revision,
                    model.normalization,
                    embedding_blob(embedding),
                    created_at_utc_ms,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn local_user_voice(
        &self,
        user_name: Option<&str>,
    ) -> Result<Option<SpeakerEmbedding>, StoreError> {
        let known = known_voices_in(
            &*self.connection()?,
            None,
            Some(user_name.unwrap_or("")),
            &[],
        )?;
        Ok(known
            .people
            .into_iter()
            .find_map(|(person, _, voice)| person.is_none().then_some(voice)))
    }

    pub(crate) fn voice_named_speakers(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<VoiceNamedSpeaker>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT named.speaker_id, named.original_display_name, named.resolution_kind
               FROM meeting_voice_names named
               JOIN meeting_speakers speaker ON speaker.speaker_id = named.speaker_id
              WHERE named.session_id = ?1 AND speaker.session_id = ?1
                AND named.is_current = 1 AND named.speaker_revision = speaker.revision
                AND speaker.merged_into_speaker_id IS NULL
              ORDER BY named.speaker_id",
        )?;
        let rows = statement.query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (speaker_id, original_display_name, kind) = row?;
            Ok(VoiceNamedSpeaker {
                speaker_id: SpeakerId::from_uuid(parse_uuid(&speaker_id)?),
                original_display_name,
                resolution_kind: match kind.as_str() {
                    "named_by_voice" => VoiceNameResolution::NamedByVoice,
                    "voice_match" => VoiceNameResolution::VoiceMatch,
                    _ => return Err(StoreError::Corrupt),
                },
            })
        })
        .collect()
    }

    /// Names only generated speakers with independent confirmation in two earlier meetings.
    pub(crate) fn apply_confirmed_voice_names(
        &self,
        session_id: MeetingSessionId,
        user_name: Option<&str>,
    ) -> Result<(), StoreError> {
        let unresolved = self.unresolved_active_voice_speaker_ids(session_id, user_name)?;
        if unresolved.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let people = confirmed_people_before_in(&transaction, session_id)?;
        let known = known_voices_in(&transaction, Some(session_id), user_name, &unresolved)?;
        let mut people_changed = false;
        let mut names_changed = false;
        for (speaker_id, voice) in &known.waiting {
            let (original_name, revision): (String, i64) = transaction.query_row(
                "SELECT display_name, revision FROM meeting_speakers
                  WHERE speaker_id = ?1 AND session_id = ?2 AND merged_into_speaker_id IS NULL",
                params![id(*speaker_id), id(session_id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if revision != 0
                || !is_generated_speaker_label(&original_name)
                || automatic_name_blocked_in(&transaction, *speaker_id)?
            {
                continue;
            }
            let Some(person) = confirmed_voice_owner(voice, &people, &known, user_name) else {
                continue;
            };
            let speaker_revision = from_i64(revision)?
                .checked_add(1)
                .ok_or(StoreError::VoiceInvariant)?;
            transaction.execute(
                "UPDATE meeting_speakers SET display_name = ?1, revision = ?2
                  WHERE speaker_id = ?3 AND session_id = ?4",
                params![
                    person.display_name,
                    to_i64(speaker_revision)?,
                    id(*speaker_id),
                    id(session_id)
                ],
            )?;
            remember_voice_name_in(
                &transaction,
                session_id,
                *speaker_id,
                person.person_id,
                &original_name,
                speaker_revision,
                "named_by_voice",
            )?;
            names_changed = true;
            people_changed |= upsert_link_in(
                &transaction,
                session_id,
                person.person_id,
                PersonLinkSource::Speaker,
                PersonLinkConfidence::Suggested,
                utc_now_ms(),
            )?;
            mark_artifacts_out_of_date(&transaction, session_id)?;
            let session = session_row(&transaction, session_id)?;
            let next_revision = session
                .revision
                .checked_add(1)
                .ok_or(StoreError::VoiceInvariant)?;
            transaction.execute(
                "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
                params![to_i64(next_revision)?, id(session_id)],
            )?;
            append_event(
                &transaction,
                session_id,
                next_revision,
                session.phase,
                session.phase,
                "speaker_named_by_voice",
                None,
            )?;
        }
        if people_changed {
            bump_people_revision_in(&transaction)?;
        }
        if names_changed {
            rebuild_search_documents_in(&transaction, session_id)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Keep the user's explicitly opted-in dictation voice for local voice comparisons.
    pub(crate) fn save_user_voiceprint(
        &self,
        request: UserVoiceprintRequest,
    ) -> Result<UserVoiceprint, StoreError> {
        let UserVoiceprintRequest {
            consent,
            model,
            embedding,
            voiceprint,
            created_at_utc_ms,
        } = request;
        require_expected_model(model)?;
        self.connection()?.execute(
            "INSERT OR REPLACE INTO user_voiceprint (
                singleton, model_id, model_revision, model_sha256, embedding_dimensions,
                sample_rate_hz, feature_bins, feature_pipeline_revision, normalization,
                embedding, dictation_count, speech_ms, consent_version, created_at_utc_ms
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                model.model_id,
                model.model_revision,
                model.model_sha256,
                i64::try_from(model.embedding_dimensions)
                    .map_err(|_| StoreError::VoiceInvariant)?,
                i64::from(model.sample_rate_hz),
                i64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?,
                model.feature_pipeline_revision,
                model.normalization,
                embedding_blob(&embedding),
                i64::from(voiceprint.dictation_count),
                to_i64(voiceprint.speech_ms)?,
                i64::from(consent.version()),
                created_at_utc_ms,
            ],
        )?;
        Ok(voiceprint)
    }

    pub(crate) fn forget_user_voiceprint(&self) -> Result<(), StoreError> {
        let user_name = self.user_name();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let user_speakers = user_named_speakers_in(&transaction, user_name.as_deref())?;
        transaction.execute(
            "DELETE FROM meeting_speaker_voiceprints
              WHERE speaker_id IN (SELECT value FROM json_each(?1))",
            params![user_speakers],
        )?;
        let people = {
            let mut statement = transaction.prepare("SELECT id, display_name FROM persons")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut profiles_changed = false;
        for (person_id, name) in people {
            if names_user(&name, user_name.as_deref()) {
                let person_id = PersonId::from_uuid(parse_uuid(&person_id)?);
                forget_person_voiceprints_in(&transaction, person_id)?;
                profiles_changed |=
                    remove_voice_person_evidence_in(&transaction, person_id)?.people_changed();
            }
        }
        transaction.execute("DELETE FROM user_voiceprint", [])?;
        if profiles_changed {
            bump_people_revision_in(&transaction)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn user_voiceprint(&self) -> Result<Option<UserVoiceprint>, StoreError> {
        let row = self
            .connection()?
            .query_row(
                "SELECT dictation_count, speech_ms FROM user_voiceprint",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        row.map(|(dictation_count, speech_ms)| {
            Ok(UserVoiceprint {
                dictation_count: u32::try_from(dictation_count).map_err(|_| StoreError::Corrupt)?,
                speech_ms: from_i64(speech_ms)?,
            })
        })
        .transpose()
    }

    /// For each speaker still waiting for a name, the one person whose
    /// confirmed voices from any meeting it matches, or the user.
    /// Nothing is written: the speaker is named only when the user confirms,
    /// through `identify_speaker` for a person and a rename to `user_name`
    /// for the user.
    pub(crate) fn voice_speaker_suggestions(
        &self,
        session_id: MeetingSessionId,
        user_name: Option<&str>,
    ) -> Result<Vec<VoiceSpeakerSuggestion>, StoreError> {
        let unresolved = self.unresolved_active_voice_speaker_ids(session_id, user_name)?;
        if unresolved.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let known = known_voices_in(&connection, Some(session_id), user_name, &unresolved)?;
        Ok(known
            .waiting
            .iter()
            .filter_map(|(speaker_id, embedding)| {
                let (person_id, display_name) = known.owner(embedding)?;
                Some(VoiceSpeakerSuggestion {
                    speaker_id: *speaker_id,
                    person_id,
                    display_name: display_name.to_owned(),
                })
            })
            .collect())
    }

    /// For each speaker of this meeting still waiting for a name, the other
    /// meetings where `pair_same_voices` hears its voice, waiting there too.
    /// A voice that matches somebody already known is left to
    /// `voice_speaker_suggestions`. Nothing is written: a label names the
    /// others only when the user keeps them in it.
    pub(crate) fn same_voice_meetings(
        &self,
        session_id: MeetingSessionId,
        user_name: Option<&str>,
    ) -> Result<Vec<SameVoice>, StoreError> {
        let groups = same_voices_in(&*self.connection()?, session_id, user_name)?;
        groups
            .into_iter()
            .map(|(speaker_id, mut voices)| {
                voices.sort_by_key(|voice| voice.started_at_utc_ms);
                let meetings = voices
                    .into_iter()
                    .map(|voice| {
                        let clip = if voice.audio_kept {
                            match self
                                .local_voice_enrollment_evidence(voice.session_id, voice.speaker_id)
                            {
                                Ok(evidence) => Some(VoiceClip {
                                    start_offset_ns: evidence.start_offset_ns(),
                                    end_offset_ns: evidence.end_offset_ns(),
                                }),
                                Err(
                                    StoreError::InsufficientEnrollmentEvidence
                                    | StoreError::TranscriptDeleted,
                                ) => None,
                                Err(error) => return Err(error),
                            }
                        } else {
                            None
                        };
                        Ok(SameVoiceMeeting {
                            session_id: voice.session_id,
                            speaker_id: voice.speaker_id,
                            speaker_revision: voice.speaker_revision,
                            title: voice.title,
                            started_at_utc_ms: voice.started_at_utc_ms,
                            clip,
                        })
                    })
                    .collect::<Result<_, StoreError>>()?;
                Ok(SameVoice {
                    speaker_id,
                    meetings,
                })
            })
            .collect()
    }

    /// Forgets every voice still waiting for a name that a run heard more than
    /// `UNNAMED_VOICE_RETENTION_MS` before `now_utc_ms`, unless it matches
    /// somebody known. Returns how many it forgot.
    pub(crate) fn expire_unnamed_voiceprints(&self, now_utc_ms: i64) -> Result<usize, StoreError> {
        let user_name = self.user_name();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired = unnamed_voices_in(
            &transaction,
            now_utc_ms.saturating_sub(UNNAMED_VOICE_RETENTION_MS),
        )?;
        if expired.is_empty() {
            return Ok(0);
        }
        let known = known_voices_in(&transaction, None, user_name.as_deref(), &[])?;
        let mut forgotten = 0;
        for voice in expired {
            if voice
                .voice
                .as_ref()
                .is_some_and(|voice| known.owner(voice).is_some())
            {
                continue;
            }
            forget_speaker_voiceprint_in(&transaction, voice.speaker_id)?;
            forgotten += 1;
        }
        transaction.commit()?;
        Ok(forgotten)
    }

    /// Match entirely inside SQLCipher and return only a revision-fenced target.
    pub(crate) fn match_local_voice_profile(
        &self,
        embedding: &SpeakerEmbedding,
        model: SpeakerEmbeddingModelKey,
    ) -> Result<Option<SuccessfulVoiceProfileMatch>, StoreError> {
        require_expected_model(model)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        // Profile rows and the people revision must share one SQLCipher snapshot.
        let profiles = {
            let mut statement = transaction.prepare(
                "SELECT person_id, model_id, model_revision, model_sha256, embedding_dimensions,
                        sample_rate_hz, feature_bins, feature_pipeline_revision, normalization,
                        sample_count, profile_revision, consent_version, centroid
                   FROM voice_profiles ORDER BY person_id",
            )?;
            let rows = statement.query_map([], profile_row_with_centroid)?;
            let compatible_model = VoiceProfileModelStatus::from(model);
            let mut profiles = Vec::new();
            for row in rows {
                let (profile, centroid) = row?;
                if profile.model == compatible_model && profile.sample_count > 0 {
                    profiles.push((profile, embedding_from_blob(&centroid)?));
                }
            }
            profiles
        };
        let people_revision = people_revision_in(&transaction)?;
        transaction.commit()?;
        // The matcher returns the winning id rather than an index into this
        // Vec, and compatibility filtering above is the only such filter.
        let matched = match_speaker_profile(
            embedding,
            profiles.iter().map(|(profile, centroid)| {
                ((profile.person_id, profile.profile_revision), centroid)
            }),
        );
        Ok(matched.map(
            |(person_id, profile_revision)| SuccessfulVoiceProfileMatch {
                person_id,
                profile_revision,
                people_revision,
            },
        ))
    }

    /// Commit a successful matcher decision only if neither side has changed.
    ///
    /// A speaker still carrying the label diarization gave it takes the
    /// matched person's name and a suggested link to them, in the same
    /// transaction as the match, so the transcript says who spoke and the
    /// person's page lists the meeting. A name somebody typed is theirs: the
    /// match is kept and the name is left alone.
    pub(crate) fn commit_successful_voice_match(
        &self,
        session_id: MeetingSessionId,
        speaker_id: SpeakerId,
        expected_speaker_revision: u64,
        matched: SuccessfulVoiceProfileMatch,
        matched_at_utc_ms: i64,
    ) -> Result<VoiceSpeakerMatchOverlay, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_voice_people_revision_in(&transaction, matched.people_revision)?;
        let mut speaker_revision =
            active_speaker_revision_in(&transaction, session_id, speaker_id)?;
        if speaker_revision != expected_speaker_revision {
            return Err(StoreError::StaleRevision);
        }
        let profile =
            profile_row_in(&transaction, matched.person_id)?.ok_or(StoreError::StaleRevision)?;
        require_profile_model(&profile, wespeaker_embedding_model_key())?;
        if profile.profile_revision != matched.profile_revision || profile.sample_count == 0 {
            return Err(StoreError::StaleRevision);
        }
        let current_name: String = transaction.query_row(
            "SELECT display_name FROM meeting_speakers
              WHERE speaker_id = ?1 AND session_id = ?2 AND merged_into_speaker_id IS NULL",
            params![id(speaker_id), id(session_id)],
            |row| row.get(0),
        )?;
        if automatic_name_blocked_in(&transaction, speaker_id)? {
            return Err(StoreError::Conflict);
        }
        if is_generated_speaker_label(&current_name) && expected_speaker_revision == 0 {
            let person = person_by_id_in(&transaction, matched.person_id)?;
            speaker_revision = speaker_revision
                .checked_add(1)
                .ok_or(StoreError::VoiceInvariant)?;
            transaction.execute(
                "UPDATE meeting_speakers SET display_name = ?1, revision = ?2
                  WHERE speaker_id = ?3 AND session_id = ?4 AND merged_into_speaker_id IS NULL",
                params![
                    person.display_name,
                    to_i64(speaker_revision)?,
                    id(speaker_id),
                    id(session_id)
                ],
            )?;
            remember_voice_name_in(
                &transaction,
                session_id,
                speaker_id,
                matched.person_id,
                &current_name,
                speaker_revision,
                "voice_match",
            )?;
            let linked = upsert_link_in(
                &transaction,
                session_id,
                matched.person_id,
                PersonLinkSource::Speaker,
                PersonLinkConfidence::Suggested,
                matched_at_utc_ms,
            )?;
            mark_artifacts_out_of_date(&transaction, session_id)?;
            let session = session_row(&transaction, session_id)?;
            let next_revision = session
                .revision
                .checked_add(1)
                .ok_or(StoreError::VoiceInvariant)?;
            transaction.execute(
                "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
                params![to_i64(next_revision)?, id(session_id)],
            )?;
            append_event(
                &transaction,
                session_id,
                next_revision,
                session.phase,
                session.phase,
                "speaker_matched_by_voice",
                None,
            )?;
            if linked {
                bump_people_revision_in(&transaction)?;
            }
        }
        transaction.execute(
            "INSERT INTO voice_speaker_matches (
                speaker_id, session_id, person_id, profile_revision, speaker_revision, matched_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(speaker_id) DO UPDATE SET
                session_id = excluded.session_id,
                person_id = excluded.person_id,
                profile_revision = excluded.profile_revision,
                speaker_revision = excluded.speaker_revision,
                matched_at_utc_ms = excluded.matched_at_utc_ms",
            params![
                id(speaker_id),
                id(session_id),
                id(matched.person_id),
                to_i64(matched.profile_revision)?,
                to_i64(speaker_revision)?,
                matched_at_utc_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(VoiceSpeakerMatchOverlay {
            person_id: matched.person_id,
            profile_revision: matched.profile_revision,
            speaker_revision,
        })
    }

    /// A human identification is deliberately separate from automatic matching.
    /// A label names the same voice in other meetings too when the request
    /// lists them, each only while it still waits for a name at the revision
    /// the user saw and still sounds like this speaker.
    pub(crate) fn identify_speaker(
        &self,
        request: MeetingSpeakerIdentifyRequest,
    ) -> Result<VoiceSpeakerIdentityResult, StoreError> {
        let user_name = match &request.disposition {
            MeetingSpeakerIdentifyDisposition::Label { same_voice, .. }
                if !same_voice.is_empty() =>
            {
                self.user_name()
            }
            _ => None,
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = operation_receipt_in(&transaction, request.operation_id)? {
            let resolved_person_id =
                resolved_person_id_for_operation_in(&transaction, request.operation_id)?;
            transaction.commit()?;
            return Ok(VoiceSpeakerIdentityResult {
                receipt,
                resolved_person_id,
                same_voice_revisions: Vec::new(),
            });
        }
        let session = session_row(&transaction, request.session_id)?;
        if session.revision != request.expected_meeting_revision {
            return Err(StoreError::StaleRevision);
        }
        if !identifiable(session.phase) {
            return Err(StoreError::Conflict);
        }
        require_voice_people_revision_in(&transaction, request.expected_people_revision)?;
        active_speaker_revision_in(&transaction, request.session_id, request.speaker_id)?;

        // Label and CorrectTo resolve and link a target identically; they differ
        // only in whether the speaker's existing samples survive. MarkUnknown is
        // the same shape with no target.
        let (target, remove_samples, same_voice) = match request.disposition {
            MeetingSpeakerIdentifyDisposition::Label { target, same_voice } => {
                (Some(target), false, same_voice)
            }
            MeetingSpeakerIdentifyDisposition::CorrectTo { target } => {
                (Some(target), true, Vec::new())
            }
            MeetingSpeakerIdentifyDisposition::MarkUnknown => (None, true, Vec::new()),
        };
        require_same_voice_in(
            &transaction,
            request.session_id,
            request.speaker_id,
            &same_voice,
            user_name.as_deref(),
        )?;
        let (display_name, person_changed, mut link_changed, manual_resolution) = match target {
            Some(target) => {
                let (person_id, display_name, created) = resolve_identification_target_in(
                    &transaction,
                    target,
                    request.requested_at_utc_ms,
                )?;
                let linked = upsert_link_in(
                    &transaction,
                    request.session_id,
                    person_id,
                    PersonLinkSource::Speaker,
                    PersonLinkConfidence::Confirmed,
                    request.requested_at_utc_ms,
                )?;
                (
                    display_name,
                    created,
                    linked,
                    ManualVoiceSpeakerResolution::Identified(person_id),
                )
            }
            None => (
                "Unknown".to_owned(),
                false,
                false,
                ManualVoiceSpeakerResolution::Unknown,
            ),
        };
        clear_automatic_match_for_speaker_in(&transaction, request.speaker_id)?;
        link_changed |= withdraw_automatic_name_in(&transaction, request.speaker_id)?;
        let profiles_changed = if remove_samples {
            forget_speaker_voiceprint_in(&transaction, request.speaker_id)?;
            remove_profile_samples_for_speaker_in(&transaction, request.speaker_id)?
        } else {
            false
        };
        let next_revision = name_speaker_in(
            &transaction,
            request.session_id,
            &session,
            request.speaker_id,
            &display_name,
            request.operation_id,
            manual_resolution,
        )?;
        let mut same_voice_revisions = Vec::with_capacity(same_voice.len());
        if let ManualVoiceSpeakerResolution::Identified(person_id) = manual_resolution {
            for speaker in same_voice {
                link_changed |= upsert_link_in(
                    &transaction,
                    speaker.session_id,
                    person_id,
                    PersonLinkSource::Speaker,
                    PersonLinkConfidence::Confirmed,
                    request.requested_at_utc_ms,
                )?;
                let revision = name_speaker_in(
                    &transaction,
                    speaker.session_id,
                    &session_row(&transaction, speaker.session_id)?,
                    speaker.speaker_id,
                    &display_name,
                    MeetingOperationId::new(),
                    manual_resolution,
                )?;
                same_voice_revisions.push((speaker.session_id, revision));
            }
        }
        if person_changed || link_changed || profiles_changed {
            bump_people_revision_in(&transaction)?;
        }
        let resolved_person_id = manual_resolution.resolved_person_id();
        let receipt = committed_receipt(
            StoreMutation {
                operation_id: request.operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms: request.requested_at_utc_ms,
                session_id: request.session_id,
                expected_revision: request.expected_meeting_revision,
                command: MeetingCommandKind::SpeakerIdentify,
            },
            session.phase,
            session.phase,
            request.requested_at_utc_ms,
            next_revision,
            Vec::new(),
        );
        insert_operation_receipt(&transaction, &receipt, request.requested_at_utc_ms)?;
        transaction.commit()?;
        Ok(VoiceSpeakerIdentityResult {
            receipt,
            resolved_person_id,
            same_voice_revisions,
        })
    }
}

/// The phases in which a person can still answer for a meeting's speakers.
fn identifiable(phase: MeetingPhase) -> bool {
    matches!(
        phase,
        MeetingPhase::CapturingRecording
            | MeetingPhase::CapturingPaused
            | MeetingPhase::Processing
            | MeetingPhase::ReviewReady
            | MeetingPhase::RecoveryRequired
    )
}

/// Writes one person's answer for one speaker: the name it now carries, its
/// meeting's next revision and the answer, kept under `operation_id`.
/// Returns that revision.
fn name_speaker_in(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    session: &SessionRow,
    speaker_id: SpeakerId,
    display_name: &str,
    operation_id: MeetingOperationId,
    resolution: ManualVoiceSpeakerResolution,
) -> Result<u64, StoreError> {
    transaction.execute(
        "UPDATE meeting_speakers
            SET display_name = ?1, revision = revision + 1
          WHERE speaker_id = ?2 AND session_id = ?3 AND merged_into_speaker_id IS NULL",
        params![display_name, id(speaker_id), id(session_id)],
    )?;
    mark_artifacts_out_of_date(transaction, session_id)?;
    let next_revision = session
        .revision
        .checked_add(1)
        .ok_or(StoreError::VoiceInvariant)?;
    transaction.execute(
        "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
        params![to_i64(next_revision)?, id(session_id)],
    )?;
    append_event(
        transaction,
        session_id,
        next_revision,
        session.phase,
        session.phase,
        "speaker_identified",
        None,
    )?;
    persist_manual_voice_speaker_resolution_in(transaction, operation_id, speaker_id, resolution)?;
    Ok(next_revision)
}

/// A kept voice nobody has named: its speaker still carries the label
/// diarization gave it, and no answer or voice match covers it. `voice` is
/// none for a print another model made, which matches nobody.
struct UnnamedVoice {
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
    speaker_revision: u64,
    phase: MeetingPhase,
    title: String,
    started_at_utc_ms: i64,
    audio_kept: bool,
    voice: Option<SpeakerEmbedding>,
}

/// Every unnamed voice a run kept before `kept_before_utc_ms`, grouped by
/// meeting.
fn unnamed_voices_in(
    connection: &Connection,
    kept_before_utc_ms: i64,
) -> Result<Vec<UnnamedVoice>, StoreError> {
    let model = wespeaker_embedding_model_key();
    let mut statement = connection.prepare(
        "SELECT voiceprint.session_id, voiceprint.speaker_id, speaker.revision,
                speaker.display_name, session.phase, session.title,
                COALESCE(session.started_at_utc_ms, session.created_at_utc_ms),
                session.audio_purged_at_utc_ms IS NULL,
                voiceprint.model_id = ?1 AND voiceprint.model_revision = ?2
                    AND voiceprint.model_sha256 = ?3 AND voiceprint.embedding_dimensions = ?4
                    AND voiceprint.sample_rate_hz = ?5 AND voiceprint.feature_bins = ?6
                    AND voiceprint.feature_pipeline_revision = ?7
                    AND voiceprint.normalization = ?8,
                voiceprint.embedding
           FROM meeting_speaker_voiceprints voiceprint
           JOIN meeting_speakers speaker
             ON speaker.speaker_id = voiceprint.speaker_id
            AND speaker.merged_into_speaker_id IS NULL
           JOIN meeting_sessions session ON session.id = voiceprint.session_id
          WHERE voiceprint.created_at_utc_ms < ?9
            AND NOT EXISTS (
                SELECT 1 FROM meeting_voice_speaker_resolutions resolution
                 WHERE resolution.speaker_id = speaker.speaker_id AND resolution.is_current = 1
            )
            AND NOT EXISTS (
                SELECT 1 FROM voice_speaker_matches matched
                 WHERE matched.speaker_id = speaker.speaker_id
            )
          ORDER BY voiceprint.session_id, voiceprint.speaker_id",
    )?;
    let rows = statement.query_map(
        params![
            model.model_id,
            model.model_revision,
            model.model_sha256,
            i64::try_from(model.embedding_dimensions).map_err(|_| StoreError::VoiceInvariant)?,
            i64::from(model.sample_rate_hz),
            i64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?,
            model.feature_pipeline_revision,
            model.normalization,
            kept_before_utc_ms,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, bool>(8)?,
                row.get::<_, Vec<u8>>(9)?,
            ))
        },
    )?;
    let mut voices = Vec::new();
    for row in rows {
        let (
            session_id,
            speaker_id,
            revision,
            name,
            phase,
            title,
            started,
            audio_kept,
            current,
            embedding,
        ) = row?;
        if !is_generated_speaker_label(&name) {
            continue;
        }
        voices.push(UnnamedVoice {
            session_id: MeetingSessionId::from_uuid(parse_uuid(&session_id)?),
            speaker_id: SpeakerId::from_uuid(parse_uuid(&speaker_id)?),
            speaker_revision: from_i64(revision)?,
            phase: phase_from_db(&phase)?,
            title,
            started_at_utc_ms: started,
            audio_kept,
            voice: current
                .then(|| embedding_from_blob(&embedding))
                .transpose()?,
        });
    }
    Ok(voices)
}

/// Each unnamed speaker of `session_id` with the unnamed voices of other
/// meetings that `pair_same_voices` takes for its own, at most one from each
/// meeting. A voice that matches somebody known stays out on both sides, and
/// the other meetings are ones a label can still name in.
fn same_voices_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    user_name: Option<&str>,
) -> Result<Vec<(SpeakerId, Vec<UnnamedVoice>)>, StoreError> {
    let known = known_voices_in(connection, None, user_name, &[])?;
    let (here, there): (Vec<_>, Vec<_>) = unnamed_voices_in(connection, i64::MAX)?
        .into_iter()
        .filter(|voice| {
            identifiable(voice.phase)
                && voice
                    .voice
                    .as_ref()
                    .is_some_and(|voice| known.owner(voice).is_none())
        })
        .partition(|voice| voice.session_id == session_id);
    let here_voices = here
        .iter()
        .enumerate()
        .filter_map(|(index, voice)| Some((index, voice.voice.as_ref()?)))
        .collect::<Vec<_>>();
    let mut groups = here
        .iter()
        .map(|voice| (voice.speaker_id, Vec::new()))
        .collect::<Vec<_>>();
    let mut there = there.into_iter().peekable();
    while let Some(first) = there.next() {
        let other_session = first.session_id;
        let mut meeting = vec![first];
        while let Some(voice) = there.next_if(|voice| voice.session_id == other_session) {
            meeting.push(voice);
        }
        let there_voices = meeting
            .iter()
            .enumerate()
            .filter_map(|(index, voice)| Some((index, voice.voice.as_ref()?)))
            .collect::<Vec<_>>();
        let pairs = pair_same_voices(&here_voices, &there_voices);
        for (index, voice) in meeting.into_iter().enumerate() {
            if let Some(&(here_index, _)) = pairs.iter().find(|&&(_, paired)| paired == index) {
                groups[here_index].1.push(voice);
            }
        }
    }
    groups.retain(|(_, voices)| !voices.is_empty());
    Ok(groups)
}

/// Refuses a label that would also name a speaker of another meeting unless
/// each one still groups with this speaker's voice at the revision the user
/// saw, one speaker per meeting.
fn require_same_voice_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
    same_voice: &[SameVoiceSpeaker],
    user_name: Option<&str>,
) -> Result<(), StoreError> {
    if same_voice.is_empty() {
        return Ok(());
    }
    let meetings = same_voice
        .iter()
        .map(|speaker| speaker.session_id)
        .collect::<HashSet<_>>();
    if meetings.len() != same_voice.len() {
        return Err(StoreError::Invalid);
    }
    let grouped = same_voices_in(connection, session_id, user_name)?
        .into_iter()
        .find(|(waiting, _)| *waiting == speaker_id)
        .map(|(_, voices)| voices)
        .unwrap_or_default();
    same_voice
        .iter()
        .all(|speaker| {
            grouped.iter().any(|voice| {
                voice.session_id == speaker.session_id
                    && voice.speaker_id == speaker.speaker_id
                    && voice.speaker_revision == speaker.expected_speaker_revision
            })
        })
        .then_some(())
        .ok_or(StoreError::StaleRevision)
}

/// The voices Sona knows, and the voices of one meeting's speakers still
/// waiting for a name.
pub(super) struct KnownVoices {
    /// Each person's voice, the mean of every confirmed one, and the user's
    /// with no person.
    people: Vec<(Option<PersonId>, String, SpeakerEmbedding)>,
    waiting: Vec<(SpeakerId, SpeakerEmbedding)>,
}

impl KnownVoices {
    /// Whose voice `voice` is, when it matches one clearly; no person is the
    /// user.
    pub(super) fn owner(&self, voice: &SpeakerEmbedding) -> Option<(Option<PersonId>, &str)> {
        let index = match_speaker_profile(
            voice,
            self.people
                .iter()
                .enumerate()
                .map(|(index, (_, _, centroid))| (index, centroid)),
        )?;
        let (person_id, display_name, _) = &self.people[index];
        Some((*person_id, display_name))
    }
}

struct ConfirmedPersonVoice {
    person_id: PersonId,
    display_name: String,
    meetings: Vec<ConfirmedMeetingVoice>,
}

struct ConfirmedMeetingVoice {
    voice: SpeakerEmbedding,
    variation: f32,
}

fn confirmed_people_before_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<ConfirmedPersonVoice>, StoreError> {
    let model = wespeaker_embedding_model_key();
    let mut statement = connection.prepare(
        "SELECT person.id, person.display_name, source.id, voiceprint.embedding
           FROM meeting_speaker_voiceprints voiceprint
           JOIN meeting_speakers speaker ON speaker.speaker_id = voiceprint.speaker_id
           JOIN meeting_sessions source ON source.id = speaker.session_id
           JOIN meeting_voice_speaker_resolutions resolution
             ON resolution.speaker_id = speaker.speaker_id AND resolution.is_current = 1
            AND resolution.resolution_kind = 'identified'
           JOIN persons person ON person.id = resolution.resolved_person_id
           JOIN meeting_sessions target ON target.id = ?1
          WHERE source.id != target.id
            AND COALESCE(source.started_at_utc_ms, source.created_at_utc_ms)
                < COALESCE(target.started_at_utc_ms, target.created_at_utc_ms)
            AND source.phase != 'deleting' AND source.transcript_purged_at_utc_ms IS NULL
            AND speaker.merged_into_speaker_id IS NULL
            AND NOT EXISTS (SELECT 1 FROM voice_speaker_matches matched
                             WHERE matched.speaker_id = speaker.speaker_id)
            AND NOT EXISTS (SELECT 1 FROM meeting_voice_names named
                             WHERE named.speaker_id = speaker.speaker_id AND named.is_current = 1)
            AND voiceprint.model_id = ?2 AND voiceprint.model_revision = ?3
            AND voiceprint.model_sha256 = ?4 AND voiceprint.embedding_dimensions = ?5
            AND voiceprint.sample_rate_hz = ?6 AND voiceprint.feature_bins = ?7
            AND voiceprint.feature_pipeline_revision = ?8 AND voiceprint.normalization = ?9
          ORDER BY person.id, source.id, speaker.speaker_id",
    )?;
    let rows = statement.query_map(
        params![
            id(session_id),
            model.model_id,
            model.model_revision,
            model.model_sha256,
            i64::try_from(model.embedding_dimensions).map_err(|_| StoreError::VoiceInvariant)?,
            i64::from(model.sample_rate_hz),
            i64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?,
            model.feature_pipeline_revision,
            model.normalization,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        },
    )?;
    let mut grouped: Vec<(PersonId, String, Vec<(String, Vec<SpeakerEmbedding>)>)> = Vec::new();
    for row in rows {
        let (person_id, display_name, meeting, blob) = row?;
        let person_id = PersonId::from_uuid(parse_uuid(&person_id)?);
        let voice = embedding_from_blob(&blob)?;
        if grouped.last().is_none_or(|(last, _, _)| *last != person_id) {
            grouped.push((person_id, display_name, Vec::new()));
        }
        let Some((_, _, meetings)) = grouped.last_mut() else {
            return Err(StoreError::VoiceInvariant);
        };
        match meetings.last_mut() {
            Some((last, voices)) if *last == meeting => voices.push(voice),
            _ => meetings.push((meeting, vec![voice])),
        }
    }
    Ok(grouped
        .into_iter()
        .map(|(person_id, display_name, meetings)| {
            let mut meetings: Vec<_> = meetings
                .into_iter()
                .filter_map(|(_, voices)| mean_embedding(&voices))
                .map(|voice| ConfirmedMeetingVoice {
                    voice,
                    variation: 0.0,
                })
                .collect();
            for index in 0..meetings.len() {
                let variation = meetings
                    .iter()
                    .map(|other| voice_distance(&meetings[index].voice, &other.voice))
                    .reduce(f32::max)
                    .unwrap_or(0.0);
                meetings[index].variation = variation;
            }
            ConfirmedPersonVoice {
                person_id,
                display_name,
                meetings,
            }
        })
        .collect())
}

fn voice_distance(left: &SpeakerEmbedding, right: &SpeakerEmbedding) -> f32 {
    (1.0 - cosine_similarity(left, right)).max(0.0)
}

fn confirmed_voice_owner<'a>(
    voice: &SpeakerEmbedding,
    people: &'a [ConfirmedPersonVoice],
    known: &KnownVoices,
    user_name: Option<&str>,
) -> Option<&'a ConfirmedPersonVoice> {
    let mut winner = None;
    for person in people {
        if person.meetings.len() < 2 || names_user(&person.display_name, user_name) {
            continue;
        }
        let rival_distance = people
            .iter()
            .filter(|other| other.person_id != person.person_id)
            .flat_map(|other| other.meetings.iter().map(|meeting| &meeting.voice))
            .chain(
                known
                    .people
                    .iter()
                    .filter(|(person_id, _, _)| person_id.is_none())
                    .map(|(_, _, voice)| voice),
            )
            .map(|other| voice_distance(voice, other))
            .reduce(f32::min);
        let limit = rival_distance
            .map(|distance| distance * CONFIRMED_VOICE_DISTANCE_RATIO)
            .unwrap_or(MAX_CONFIRMED_VOICE_DISTANCE)
            .min(MAX_CONFIRMED_VOICE_DISTANCE);
        let support = person
            .meetings
            .iter()
            .filter(|confirmed| {
                let distance = voice_distance(voice, &confirmed.voice);
                distance < limit && distance <= confirmed.variation
            })
            .take(2)
            .count();
        if support == 2 {
            if winner.is_some() {
                return None;
            }
            winner = Some(person);
        }
    }
    winner
}

fn automatic_name_blocked_in(
    connection: &Connection,
    speaker_id: SpeakerId,
) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_voice_names
                        WHERE speaker_id = ?1 AND (blocked = 1 OR is_current = 1))
             OR EXISTS(SELECT 1 FROM meeting_voice_speaker_resolutions
                        WHERE speaker_id = ?1 AND is_current = 1)",
            params![id(speaker_id)],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn remember_voice_name_in(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
    person_id: PersonId,
    original_display_name: &str,
    speaker_revision: u64,
    resolution_kind: &str,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO meeting_voice_names (
            speaker_id, session_id, person_id, original_display_name,
            resolution_kind, speaker_revision, is_current, blocked
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 0)",
        params![
            id(speaker_id),
            id(session_id),
            id(person_id),
            original_display_name,
            resolution_kind,
            to_i64(speaker_revision)?
        ],
    )?;
    Ok(())
}

fn withdraw_automatic_name_in(
    transaction: &Transaction<'_>,
    speaker_id: SpeakerId,
) -> Result<bool, StoreError> {
    transaction.execute(
        "UPDATE meeting_voice_names SET is_current = 0, blocked = 1 WHERE speaker_id = ?1",
        params![id(speaker_id)],
    )?;
    let removed = transaction.execute(
        "DELETE FROM meeting_person_links
          WHERE source = 'speaker' AND confidence = 'suggested'
            AND (meeting_id, person_id) IN (
                SELECT session_id, person_id FROM meeting_voice_names WHERE speaker_id = ?1
            )
            AND NOT EXISTS (
                SELECT 1 FROM meeting_voice_names other
                 WHERE other.session_id = meeting_id AND other.person_id = meeting_person_links.person_id
                   AND other.is_current = 1
            )
            AND NOT EXISTS (
                SELECT 1 FROM meeting_voice_speaker_resolutions resolution
                JOIN meeting_speakers speaker ON speaker.speaker_id = resolution.speaker_id
                 WHERE speaker.session_id = meeting_id AND resolution.is_current = 1
                   AND resolution.resolution_kind = 'identified'
                   AND resolution.resolved_person_id = meeting_person_links.person_id
            )",
        params![id(speaker_id)],
    )?;
    Ok(removed != 0)
}

/// Manually confirmed voices, excluding automatic names and, when scoped, later meetings.
pub(super) fn known_voices_in(
    connection: &Connection,
    session_id: Option<MeetingSessionId>,
    user_name: Option<&str>,
    waiting: &[SpeakerId],
) -> Result<KnownVoices, StoreError> {
    let model = wespeaker_embedding_model_key();
    let user_speakers = user_named_speakers_in(connection, user_name)?;
    let user_voiceprint = user_voiceprint_in(connection, model)?;
    let mut statement = connection.prepare(
        "SELECT voiceprint.speaker_id, person.id, person.display_name, voiceprint.embedding,
                speaker.display_name,
                voiceprint.speaker_id IN (SELECT value FROM json_each(?10))
           FROM meeting_speaker_voiceprints voiceprint
           JOIN meeting_speakers speaker
             ON speaker.speaker_id = voiceprint.speaker_id
            AND speaker.merged_into_speaker_id IS NULL
           LEFT JOIN meeting_voice_speaker_resolutions resolution
             ON resolution.speaker_id = voiceprint.speaker_id
            AND resolution.is_current = 1
            AND resolution.resolution_kind = 'identified'
           LEFT JOIN persons person ON person.id = resolution.resolved_person_id
           JOIN meeting_sessions source_session ON source_session.id = voiceprint.session_id
          WHERE voiceprint.model_id = ?2 AND voiceprint.model_revision = ?3
            AND voiceprint.model_sha256 = ?4 AND voiceprint.embedding_dimensions = ?5
            AND voiceprint.sample_rate_hz = ?6 AND voiceprint.feature_bins = ?7
            AND voiceprint.feature_pipeline_revision = ?8 AND voiceprint.normalization = ?9
            AND source_session.phase != 'deleting'
            AND source_session.transcript_purged_at_utc_ms IS NULL
            AND NOT EXISTS (SELECT 1 FROM voice_speaker_matches matched
                             WHERE matched.speaker_id = speaker.speaker_id)
            AND NOT EXISTS (SELECT 1 FROM meeting_voice_names named
                             WHERE named.speaker_id = speaker.speaker_id AND named.is_current = 1)
            AND ((person.id IS NULL AND voiceprint.speaker_id NOT IN (SELECT value FROM json_each(?10)))
                 OR ?1 IS NULL
                 OR (source_session.id != ?1
                     AND COALESCE(source_session.started_at_utc_ms, source_session.created_at_utc_ms)
                         < (SELECT COALESCE(started_at_utc_ms, created_at_utc_ms)
                              FROM meeting_sessions WHERE id = ?1)))
            AND (person.id IS NOT NULL OR voiceprint.session_id = ?1
                 OR voiceprint.speaker_id IN (SELECT value FROM json_each(?10)))
          ORDER BY person.id, voiceprint.speaker_id",
    )?;
    let rows = statement.query_map(
        params![
            session_id.map(id),
            model.model_id,
            model.model_revision,
            model.model_sha256,
            i64::try_from(model.embedding_dimensions).map_err(|_| StoreError::VoiceInvariant)?,
            i64::from(model.sample_rate_hz),
            i64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?,
            model.feature_pipeline_revision,
            model.normalization,
            user_speakers,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
            ))
        },
    )?;
    let mut people: Vec<(Option<PersonId>, String, Vec<SpeakerEmbedding>)> = Vec::new();
    let mut user_voices = Vec::new();
    let mut unnamed = Vec::new();
    for row in rows {
        let (speaker_id, person_id, display_name, embedding, speaker_name, confirmed_user_name) =
            row?;
        let speaker_id = SpeakerId::from_uuid(parse_uuid(&speaker_id)?);
        let embedding = embedding_from_blob(&embedding)?;
        match person_id.zip(display_name) {
            Some((_, display_name)) if names_user(&display_name, user_name) => {
                user_voices.push(embedding);
            }
            Some((person_id, display_name)) => {
                let person_id = PersonId::from_uuid(parse_uuid(&person_id)?);
                match people.last_mut() {
                    Some((Some(last), _, voices)) if *last == person_id => {
                        voices.push(embedding);
                    }
                    _ => people.push((Some(person_id), display_name, vec![embedding])),
                }
            }
            None if confirmed_user_name && names_user(&speaker_name, user_name) => {
                user_voices.push(embedding);
            }
            None if waiting.contains(&speaker_id) => unnamed.push((speaker_id, embedding)),
            None => {}
        }
    }
    user_voices.extend(user_voiceprint);
    if let Some(user_name) = user_name.filter(|_| !user_voices.is_empty()) {
        people.push((None, user_name.to_owned(), user_voices));
    }
    Ok(KnownVoices {
        people: people
            .into_iter()
            .filter_map(|(person_id, display_name, voices)| {
                mean_embedding(&voices).map(|centroid| (person_id, display_name, centroid))
            })
            .collect(),
        waiting: unnamed,
    })
}

/// The labels the store writes itself: `Speaker N` for a diarized voice and
/// `Unknown speaker` for system audio nobody separated. Anything else was
/// typed by a person or chosen by them, and automatic matching never
/// overwrites it.
pub(in crate::meeting::store) fn is_generated_speaker_label(name: &str) -> bool {
    name == "Unknown speaker"
        || name.strip_prefix("Speaker ").is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Whether `name` is the user's own: their name from About me, in full or by
/// its first word.
pub(crate) fn names_user(name: &str, user_name: Option<&str>) -> bool {
    let Some(user_name) = user_name else {
        return false;
    };
    let name = normalized(name);
    !name.is_empty()
        && (normalized(user_name) == name
            || user_name
                .split_whitespace()
                .next()
                .is_some_and(|first| normalized(first) == name))
}

/// A local label or a matching name identifies the user, never the microphone lane alone.
/// Imports cannot use the local-label shortcut, and the system lane requires a full name.
pub(crate) fn speaker_is_user(
    display_name: &str,
    lane: SourceKind,
    imported: bool,
    named_by_voice_match: bool,
    user_name: Option<&str>,
) -> bool {
    if named_by_voice_match || is_generated_speaker_label(display_name) {
        return false;
    }
    if lane == SourceKind::Microphone {
        return (!imported && display_name == LOCAL_SPEAKER_LABEL)
            || names_user(display_name, user_name);
    }
    let name = normalized(display_name);
    !name.is_empty() && user_name.is_some_and(|user_name| normalized(user_name) == name)
}

/// Every kept voice's speaker, from any meeting, that no person claims and
/// whose name is the user's, as the JSON array `json_each` reads. Names are
/// compared here because SQLite's `lower` folds ASCII only.
fn user_named_speakers_in(
    connection: &Connection,
    user_name: Option<&str>,
) -> Result<String, StoreError> {
    if user_name.is_none() {
        return Ok("[]".to_owned());
    }
    let mut statement = connection.prepare(
        "SELECT speaker.speaker_id, speaker.display_name
           FROM meeting_speakers speaker
          WHERE speaker.merged_into_speaker_id IS NULL
            AND speaker.revision > 0
            AND EXISTS (
                SELECT 1 FROM meeting_speaker_voiceprints voiceprint
                 WHERE voiceprint.speaker_id = speaker.speaker_id
            )
            AND NOT EXISTS (
                SELECT 1 FROM meeting_voice_speaker_resolutions resolution
                 WHERE resolution.speaker_id = speaker.speaker_id AND resolution.is_current = 1
            )
            AND NOT EXISTS (
                SELECT 1 FROM voice_speaker_matches matched
                 WHERE matched.speaker_id = speaker.speaker_id
            )
            AND NOT EXISTS (
                SELECT 1 FROM meeting_voice_names named
                 WHERE named.speaker_id = speaker.speaker_id
                   AND (named.is_current = 1 OR named.speaker_revision = speaker.revision)
            )",
    )?;
    let mut speaker_ids = Vec::new();
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (speaker_id, display_name) = row?;
        if names_user(&display_name, user_name) {
            speaker_ids.push(speaker_id);
        }
    }
    Ok(serde_json::Value::from(speaker_ids).to_string())
}

/// The voice the user taught Sona, when the model in use is the one that
/// made it.
fn user_voiceprint_in(
    connection: &Connection,
    model: SpeakerEmbeddingModelKey,
) -> Result<Option<SpeakerEmbedding>, StoreError> {
    connection
        .query_row(
            "SELECT embedding FROM user_voiceprint
              WHERE model_id = ?1 AND model_revision = ?2 AND model_sha256 = ?3
                AND embedding_dimensions = ?4 AND sample_rate_hz = ?5 AND feature_bins = ?6
                AND feature_pipeline_revision = ?7 AND normalization = ?8",
            params![
                model.model_id,
                model.model_revision,
                model.model_sha256,
                i64::try_from(model.embedding_dimensions)
                    .map_err(|_| StoreError::VoiceInvariant)?,
                i64::from(model.sample_rate_hz),
                i64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?,
                model.feature_pipeline_revision,
                model.normalization,
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .map(|blob| embedding_from_blob(&blob))
        .transpose()
}

/// A name the user gives a speaker settles whose voice it is, the way
/// confirming it on the speaker sheet does: the kept voice counts toward that
/// person's suggestions in later meetings, which name nobody until the user
/// confirms one. A name that is nobody's, `user_name` among them, takes the
/// voice back off whoever it was given to. Returns whether people changed.
pub(in crate::meeting::store) fn resolve_renamed_speaker_in(
    transaction: &Transaction<'_>,
    operation_id: MeetingOperationId,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
    display_name: &str,
    user_name: Option<&str>,
    now_utc_ms: i64,
) -> Result<bool, StoreError> {
    let automatic_link_changed = withdraw_automatic_name_in(transaction, speaker_id)?;
    let current = identified_person_in(transaction, speaker_id)?;
    let named = if is_generated_speaker_label(display_name.trim()) {
        None
    } else {
        person_for_named_speaker_in(
            transaction,
            session_id,
            display_name,
            user_name,
            current,
            now_utc_ms,
        )?
    };
    let mut people_changed = automatic_link_changed;
    if let Some((person_id, created)) = named {
        let linked = upsert_link_in(
            transaction,
            session_id,
            person_id,
            PersonLinkSource::Speaker,
            PersonLinkConfidence::Confirmed,
            now_utc_ms,
        )?;
        people_changed |= created || linked;
    }
    let target = named.map(|(person_id, _)| person_id);
    if target == current {
        return Ok(people_changed);
    }
    people_changed |= remove_profile_samples_for_speaker_in(transaction, speaker_id)?;
    match target {
        Some(person_id) => persist_manual_voice_speaker_resolution_in(
            transaction,
            operation_id,
            speaker_id,
            ManualVoiceSpeakerResolution::Identified(person_id),
        )?,
        None => withdraw_manual_voice_speaker_resolution_in(transaction, speaker_id)?,
    }
    Ok(people_changed)
}

fn identified_person_in(
    transaction: &Transaction<'_>,
    speaker_id: SpeakerId,
) -> Result<Option<PersonId>, StoreError> {
    let person_id: Option<String> = transaction
        .query_row(
            "SELECT resolved_person_id FROM meeting_voice_speaker_resolutions
              WHERE speaker_id = ?1 AND is_current = 1 AND resolution_kind = 'identified'",
            params![id(speaker_id)],
            |row| row.get(0),
        )
        .optional()?;
    person_id
        .map(|person_id| Ok(PersonId::from_uuid(parse_uuid(&person_id)?)))
        .transpose()
}

fn resolved_person_id_for_operation_in(
    transaction: &Transaction<'_>,
    operation_id: MeetingOperationId,
) -> Result<Option<PersonId>, StoreError> {
    let (resolution_kind, resolved_person_id): (String, Option<String>) = transaction
        .query_row(
            "SELECT resolution_kind, resolved_person_id
               FROM meeting_voice_speaker_resolutions
              WHERE operation_id = ?1",
            params![id(operation_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(StoreError::Corrupt)?;
    match (resolution_kind.as_str(), resolved_person_id) {
        ("identified", Some(person_id)) => Ok(Some(PersonId::from_uuid(parse_uuid(&person_id)?))),
        ("unknown", None) => Ok(None),
        _ => Err(StoreError::Corrupt),
    }
}

fn persist_manual_voice_speaker_resolution_in(
    transaction: &Transaction<'_>,
    operation_id: MeetingOperationId,
    speaker_id: SpeakerId,
    resolution: ManualVoiceSpeakerResolution,
) -> Result<(), StoreError> {
    withdraw_manual_voice_speaker_resolution_in(transaction, speaker_id)?;
    transaction.execute(
        "INSERT INTO meeting_voice_speaker_resolutions (
            operation_id, speaker_id, resolution_kind, resolved_person_id, is_current
         ) VALUES (?1, ?2, ?3, ?4, 1)",
        params![
            id(operation_id),
            id(speaker_id),
            resolution.kind(),
            resolution.resolved_person_id().map(id),
        ],
    )?;
    Ok(())
}

fn withdraw_manual_voice_speaker_resolution_in(
    transaction: &Transaction<'_>,
    speaker_id: SpeakerId,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE meeting_voice_speaker_resolutions
            SET is_current = 0
          WHERE speaker_id = ?1 AND is_current = 1",
        params![id(speaker_id)],
    )?;
    Ok(())
}

pub(in crate::meeting::store) fn clear_automatic_match_for_speaker_in(
    transaction: &Transaction<'_>,
    speaker_id: SpeakerId,
) -> Result<bool, StoreError> {
    Ok(transaction.execute(
        "DELETE FROM voice_speaker_matches WHERE speaker_id = ?1",
        params![id(speaker_id)],
    )? != 0)
}

pub(in crate::meeting::store) fn clear_automatic_matches_for_speakers_in(
    transaction: &Transaction<'_>,
    source_speaker_id: SpeakerId,
    target_speaker_id: SpeakerId,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "DELETE FROM voice_speaker_matches WHERE speaker_id IN (?1, ?2)",
        params![id(source_speaker_id), id(target_speaker_id)],
    )?;
    Ok(changed != 0)
}

/// The merged speaker keeps its own voice, or takes the source's when it had
/// none.
pub(in crate::meeting::store) fn merge_speaker_voiceprint_in(
    transaction: &Transaction<'_>,
    source_speaker_id: SpeakerId,
    target_speaker_id: SpeakerId,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE OR IGNORE meeting_speaker_voiceprints SET speaker_id = ?2 WHERE speaker_id = ?1",
        params![id(source_speaker_id), id(target_speaker_id)],
    )?;
    forget_speaker_voiceprint_in(transaction, source_speaker_id)
}

fn forget_speaker_voiceprint_in(
    transaction: &Transaction<'_>,
    speaker_id: SpeakerId,
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM meeting_speaker_voiceprints WHERE speaker_id = ?1",
        params![id(speaker_id)],
    )?;
    Ok(())
}

/// Every remembered voice attributed to the person: speakers the user named
/// as them and speakers their enrolled profile matched. Runs before the
/// matches themselves are removed.
pub(in crate::meeting::store) fn forget_person_voiceprints_in(
    transaction: &Transaction<'_>,
    person_id: PersonId,
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM meeting_speaker_voiceprints
          WHERE speaker_id IN (
                SELECT speaker_id FROM meeting_voice_speaker_resolutions
                 WHERE is_current = 1 AND resolution_kind = 'identified'
                   AND resolved_person_id = ?1
                UNION
                SELECT speaker_id FROM voice_speaker_matches WHERE person_id = ?1
                UNION
                SELECT speaker_id FROM meeting_voice_names WHERE person_id = ?1
          )",
        params![id(person_id)],
    )?;
    Ok(())
}

pub(in crate::meeting::store) fn remove_profile_samples_for_speaker_in(
    transaction: &Transaction<'_>,
    speaker_id: SpeakerId,
) -> Result<bool, StoreError> {
    let people = sample_profile_people_in(transaction, "source_speaker_id = ?1", [id(speaker_id)])?;
    if people.is_empty() {
        return Ok(false);
    }
    transaction.execute(
        "DELETE FROM voice_profile_samples WHERE source_speaker_id = ?1",
        params![id(speaker_id)],
    )?;
    recompute_profiles_in(transaction, &people, utc_now_ms())?;
    Ok(true)
}

pub(in crate::meeting::store) fn purge_session_voice_evidence_in(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
) -> Result<VoiceEvidenceChange, StoreError> {
    let people = sample_profile_people_in(transaction, "source_session_id = ?1", [id(session_id)])?;
    let profiles_changed = if people.is_empty() {
        false
    } else {
        transaction.execute(
            "DELETE FROM voice_profile_samples WHERE source_session_id = ?1",
            params![id(session_id)],
        )?;
        recompute_profiles_in(transaction, &people, utc_now_ms())?;
        true
    };
    transaction.execute(
        "DELETE FROM voice_speaker_matches WHERE session_id = ?1",
        params![id(session_id)],
    )?;
    transaction.execute(
        "DELETE FROM meeting_speaker_voiceprints WHERE session_id = ?1",
        params![id(session_id)],
    )?;
    Ok(VoiceEvidenceChange { profiles_changed })
}

pub(in crate::meeting::store) fn remove_voice_person_evidence_in(
    transaction: &Transaction<'_>,
    person_id: PersonId,
) -> Result<VoiceEvidenceChange, StoreError> {
    transaction.execute(
        "DELETE FROM voice_speaker_matches WHERE person_id = ?1",
        params![id(person_id)],
    )?;
    transaction.execute(
        "UPDATE meeting_voice_names SET is_current = 0, blocked = 1 WHERE person_id = ?1",
        params![id(person_id)],
    )?;
    let profiles_changed = transaction.execute(
        "DELETE FROM voice_profiles WHERE person_id = ?1",
        params![id(person_id)],
    )? != 0;
    Ok(VoiceEvidenceChange { profiles_changed })
}

pub(in crate::meeting::store) fn remove_source_person_voice_evidence_for_sessions_in(
    transaction: &Transaction<'_>,
    source_person_id: PersonId,
    session_ids: &HashSet<MeetingSessionId>,
) -> Result<VoiceEvidenceChange, StoreError> {
    let mut change = VoiceEvidenceChange::default();
    for session_id in session_ids {
        let deleted_samples = transaction.execute(
            "DELETE FROM voice_profile_samples
              WHERE person_id = ?1 AND source_session_id = ?2",
            params![id(source_person_id), id(*session_id)],
        )?;
        change.profiles_changed |= deleted_samples != 0;
        transaction.execute(
            "DELETE FROM voice_speaker_matches
              WHERE person_id = ?1 AND session_id = ?2",
            params![id(source_person_id), id(*session_id)],
        )?;
    }
    if change.profiles_changed {
        let profile = profile_row_in(transaction, source_person_id)?;
        if let Some(profile) = profile {
            recompute_profile_in(
                transaction,
                source_person_id,
                profile
                    .profile_revision
                    .checked_add(1)
                    .ok_or(StoreError::VoiceInvariant)?,
                utc_now_ms(),
            )?;
        }
    }
    Ok(change)
}

pub(in crate::meeting::store) fn merge_voice_profiles_in(
    transaction: &Transaction<'_>,
    source_person_id: PersonId,
    target_person_id: PersonId,
    resolution: Option<VoiceProfileMergeResolution>,
    now_utc_ms: i64,
) -> Result<VoiceEvidenceChange, StoreError> {
    let source = profile_row_in(transaction, source_person_id)?;
    let target = profile_row_in(transaction, target_person_id)?;
    if source.is_none() && target.is_none() {
        clear_person_matches_in(transaction, source_person_id)?;
        return Ok(VoiceEvidenceChange::default());
    }
    let resolution = resolution.ok_or(StoreError::ProfileMergeResolutionRequired)?;
    match resolution {
        VoiceProfileMergeResolution::DiscardSource => Ok(remove_voice_person_evidence_in(
            transaction,
            source_person_id,
        )?),
        VoiceProfileMergeResolution::ReplaceTargetWithSource => {
            // With no source profile there is nothing to replace the target
            // with, so the target keeps its own and only the source side is
            // retired. Deleting the target first would destroy an enrolled
            // profile and every sample behind it.
            let Some(source) = source else {
                return remove_voice_person_evidence_in(transaction, source_person_id);
            };
            let mut change = remove_voice_person_evidence_in(transaction, target_person_id)?;
            move_profile_in(
                transaction,
                &source,
                source_person_id,
                target_person_id,
                now_utc_ms,
            )?;
            change.profiles_changed = true;
            Ok(change)
        }
        VoiceProfileMergeResolution::CombineCompatible => {
            // One profile between the two people is nothing to combine and
            // nothing to be incompatible with: the merged person keeps it.
            let (source, target) = match (source, target) {
                (None, Some(_)) => {
                    clear_person_matches_in(transaction, source_person_id)?;
                    return Ok(VoiceEvidenceChange::default());
                }
                (Some(source), None) => {
                    move_profile_in(
                        transaction,
                        &source,
                        source_person_id,
                        target_person_id,
                        now_utc_ms,
                    )?;
                    return Ok(VoiceEvidenceChange {
                        profiles_changed: true,
                    });
                }
                (Some(source), Some(target)) => (source, target),
                (None, None) => unreachable!("handled before a resolution is required"),
            };
            if source.model != target.model {
                return Err(StoreError::ProfileModelIncompatible);
            }
            clear_person_matches_in(transaction, source_person_id)?;
            clear_person_matches_in(transaction, target_person_id)?;
            let mut change = VoiceEvidenceChange::default();
            // Repoint, never copy. The uniqueness trigger aborts any insert
            // whose (generation, speaker) twin is still in the table, and
            // `OR IGNORE` does not suppress `RAISE(ABORT)`. That same trigger is
            // why repointing cannot collide with the target's own samples.
            transaction.execute(
                "UPDATE voice_profile_samples SET person_id = ?1 WHERE person_id = ?2",
                params![id(target_person_id), id(source_person_id)],
            )?;
            transaction.execute(
                "DELETE FROM voice_profiles WHERE person_id = ?1",
                params![id(source_person_id)],
            )?;
            recompute_profile_in(
                transaction,
                target_person_id,
                target
                    .profile_revision
                    .checked_add(1)
                    .ok_or(StoreError::VoiceInvariant)?,
                now_utc_ms,
            )?;
            change.profiles_changed = true;
            Ok(change)
        }
    }
}

/// Re-homes the source person's enrolled profile and every sample behind it
/// on the target, who must have none. The copied centroid is a placeholder;
/// `recompute_profile_in` rebuilds it from the samples now pointing at the
/// target.
fn move_profile_in(
    transaction: &Transaction<'_>,
    source: &VoiceProfileRow,
    source_person_id: PersonId,
    target_person_id: PersonId,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    clear_person_matches_in(transaction, source_person_id)?;
    let next_profile_revision = source
        .profile_revision
        .checked_add(1)
        .ok_or(StoreError::VoiceInvariant)?;
    transaction.execute(
        "INSERT INTO voice_profiles (
            person_id, model_id, model_revision, model_sha256, embedding_dimensions,
            sample_rate_hz, feature_bins, feature_pipeline_revision, normalization,
            centroid, sample_count, profile_revision, consent_version,
            created_at_utc_ms, updated_at_utc_ms
         )
         SELECT ?1, model_id, model_revision, model_sha256, embedding_dimensions,
                sample_rate_hz, feature_bins, feature_pipeline_revision, normalization,
                centroid, sample_count, ?2, consent_version, created_at_utc_ms, ?3
           FROM voice_profiles WHERE person_id = ?4",
        params![
            id(target_person_id),
            to_i64(next_profile_revision)?,
            now_utc_ms,
            id(source_person_id),
        ],
    )?;
    transaction.execute(
        "UPDATE voice_profile_samples SET person_id = ?1 WHERE person_id = ?2",
        params![id(target_person_id), id(source_person_id)],
    )?;
    transaction.execute(
        "DELETE FROM voice_profiles WHERE person_id = ?1",
        params![id(source_person_id)],
    )?;
    recompute_profile_in(
        transaction,
        target_person_id,
        next_profile_revision,
        now_utc_ms,
    )
}

fn clear_person_matches_in(
    transaction: &Transaction<'_>,
    person_id: PersonId,
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM voice_speaker_matches WHERE person_id = ?1",
        params![id(person_id)],
    )?;
    Ok(())
}

fn require_expected_model(model: SpeakerEmbeddingModelKey) -> Result<(), StoreError> {
    (model == wespeaker_embedding_model_key())
        .then_some(())
        .ok_or(StoreError::LocalModelUnavailable)
}

fn require_voice_people_revision_in(
    transaction: &Transaction<'_>,
    expected_revision: u64,
) -> Result<(), StoreError> {
    (people_revision_in(transaction)? == expected_revision)
        .then_some(())
        .ok_or(StoreError::StaleRevision)
}

fn require_voice_person_in(
    transaction: &Transaction<'_>,
    person_id: PersonId,
) -> Result<(), StoreError> {
    person_by_id_in(transaction, person_id)
        .map(|_| ())
        .map_err(|error| match error {
            StoreError::NotFound => StoreError::PersonNotFound,
            other => other,
        })
}

fn require_enrollment_authorization_in(
    transaction: &Transaction<'_>,
    evidence: VoiceEnrollmentEvidence,
    person_id: PersonId,
    expected_meeting_revision: u64,
    expected_speaker_revision: u64,
) -> Result<(), StoreError> {
    let session = session_row(transaction, evidence.session_id)?;
    if session.transcript_purged_at_utc_ms.is_some() {
        return Err(StoreError::TranscriptDeleted);
    }
    if session.revision != expected_meeting_revision {
        return Err(StoreError::StaleRevision);
    }
    if active_speaker_revision_in(transaction, evidence.session_id, evidence.speaker_id)?
        != expected_speaker_revision
    {
        return Err(StoreError::StaleRevision);
    }
    let authorized: i64 = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM meeting_voice_speaker_resolutions
             WHERE speaker_id = ?1 AND is_current = 1
               AND resolution_kind = 'identified' AND resolved_person_id = ?2
        )",
        params![id(evidence.speaker_id), id(person_id)],
        |row| row.get(0),
    )?;
    (authorized != 0).then_some(()).ok_or(StoreError::Conflict)
}

fn voice_profile_sample_owner_in(
    transaction: &Transaction<'_>,
    evidence: VoiceEnrollmentEvidence,
) -> Result<Option<PersonId>, StoreError> {
    let owner: Option<String> = transaction
        .query_row(
            "SELECT person_id FROM voice_profile_samples
              WHERE source_generation_id = ?1 AND source_speaker_id = ?2",
            params![id(evidence.generation_id), id(evidence.speaker_id)],
            |row| row.get(0),
        )
        .optional()?;
    owner
        .map(|person_id| Ok(PersonId::from_uuid(parse_uuid(&person_id)?)))
        .transpose()
}

fn pending_generation_session_in(
    transaction: &Transaction<'_>,
    generation_id: MeetingDiarizationGenerationId,
) -> Result<MeetingSessionId, StoreError> {
    let owner: Option<(String, String)> = transaction
        .query_row(
            "SELECT session_id, state FROM meeting_diarization_generations WHERE generation_id = ?1",
            params![id(generation_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((owner, state)) = owner else {
        return Err(StoreError::LocalEvidenceUnavailable);
    };
    if state != "running" {
        return Err(StoreError::Conflict);
    }
    Ok(MeetingSessionId::from_uuid(parse_uuid(&owner)?))
}

fn require_evidence_span_owner_in(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    span: &DiarizationEvidenceSpanInput,
) -> Result<(), StoreError> {
    let owner: Option<(String, String)> = transaction
        .query_row(
            "SELECT speaker.session_id, track.session_id
               FROM meeting_speakers speaker
               JOIN meeting_source_tracks track ON track.track_id = ?2
              WHERE speaker.speaker_id = ?1 AND speaker.merged_into_speaker_id IS NULL",
            params![id(span.speaker_id), id(span.track_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((speaker_session, track_session)) = owner else {
        return Err(StoreError::LocalEvidenceUnavailable);
    };
    if speaker_session != id(session_id) || track_session != id(session_id) {
        return Err(StoreError::LocalEvidenceUnavailable);
    }
    Ok(())
}

fn require_enrollment_evidence_in(
    transaction: &Transaction<'_>,
    evidence: VoiceEnrollmentEvidence,
) -> Result<(), StoreError> {
    let generation: Option<(String, String)> = transaction
        .query_row(
            "SELECT session_id, state FROM meeting_diarization_generations WHERE generation_id = ?1",
            params![id(evidence.generation_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((generation_session, generation_state)) = generation else {
        return Err(StoreError::LocalEvidenceUnavailable);
    };
    if generation_session != id(evidence.session_id) || generation_state != "completed" {
        return Err(StoreError::LocalEvidenceUnavailable);
    }
    let span_owner = DiarizationEvidenceSpanInput {
        generation_id: evidence.generation_id,
        speaker_id: evidence.speaker_id,
        track_id: evidence.track_id,
        start_offset_ns: evidence.start_offset_ns,
        end_offset_ns: evidence.end_offset_ns,
        kind: DiarizationEvidenceKind::SortformerExclusive,
    };
    require_evidence_span_owner_in(transaction, evidence.session_id, &span_owner)?;
    // The column holds `encode_json(&origin)`, so it must be decoded rather
    // than compared to a bare word.
    let (origin_json, audio_purged): (String, bool) = transaction.query_row(
        "SELECT origin_kind, audio_purged_at_utc_ms IS NOT NULL FROM meeting_sessions WHERE id = ?1",
        params![id(evidence.session_id)],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if audio_purged || matches!(decode_json(&origin_json)?, MeetingOrigin::Import) {
        return Err(StoreError::LocalEvidenceUnavailable);
    }
    let has_local_records: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM meeting_track_records WHERE track_id = ?1)",
        params![id(evidence.track_id)],
        |row| row.get(0),
    )?;
    if !has_local_records {
        return Err(StoreError::LocalEvidenceUnavailable);
    }
    let spans = {
        let mut statement = transaction.prepare(
            "SELECT start_offset_ns, end_offset_ns
               FROM meeting_diarization_evidence_spans
              WHERE generation_id = ?1 AND speaker_id = ?2 AND track_id = ?3
                AND start_offset_ns < ?5 AND end_offset_ns > ?4
              ORDER BY start_offset_ns, end_offset_ns",
        )?;
        let selected_spans = statement
            .query_map(
                params![
                    id(evidence.generation_id),
                    id(evidence.speaker_id),
                    id(evidence.track_id),
                    to_i64(evidence.start_offset_ns)?,
                    to_i64(evidence.end_offset_ns)?,
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        selected_spans
    };
    let duration = evidence
        .end_offset_ns
        .checked_sub(evidence.start_offset_ns)
        .ok_or(StoreError::VoiceInvariant)?;
    if !(MIN_ENROLLMENT_EVIDENCE_DURATION_NS..=MAX_ENROLLMENT_EVIDENCE_DURATION_NS)
        .contains(&duration)
        || spans.is_empty()
    {
        return Err(StoreError::InsufficientEnrollmentEvidence);
    }
    let mut end_offset_ns = evidence.start_offset_ns;
    for (start_offset_ns, span_end_offset_ns) in spans {
        let start_offset_ns = from_i64(start_offset_ns)?.max(evidence.start_offset_ns);
        let span_end_offset_ns = from_i64(span_end_offset_ns)?.min(evidence.end_offset_ns);
        if start_offset_ns != end_offset_ns || span_end_offset_ns <= start_offset_ns {
            return Err(StoreError::InsufficientEnrollmentEvidence);
        }
        end_offset_ns = span_end_offset_ns;
    }
    if end_offset_ns != evidence.end_offset_ns {
        return Err(StoreError::InsufficientEnrollmentEvidence);
    }
    Ok(())
}

fn active_speaker_revision_in(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
) -> Result<u64, StoreError> {
    let revision: Option<i64> = transaction
        .query_row(
            "SELECT revision FROM meeting_speakers
              WHERE speaker_id = ?1 AND session_id = ?2 AND merged_into_speaker_id IS NULL",
            params![id(speaker_id), id(session_id)],
            |row| row.get(0),
        )
        .optional()?;
    revision
        .map(from_i64)
        .transpose()?
        .ok_or(StoreError::SpeakerNotFound)
}

fn resolve_identification_target_in(
    transaction: &Transaction<'_>,
    target: MeetingSpeakerIdentifyTarget,
    now_utc_ms: i64,
) -> Result<(PersonId, String, bool), StoreError> {
    match target {
        MeetingSpeakerIdentifyTarget::Existing(person_id) => {
            let person = person_by_id_in(transaction, person_id).map_err(|error| match error {
                StoreError::NotFound => StoreError::PersonNotFound,
                other => other,
            })?;
            Ok((person.id, person.display_name, false))
        }
        MeetingSpeakerIdentifyTarget::Create { display_name } => {
            let display_name = display_name.trim().to_owned();
            if display_name.is_empty() {
                return Err(StoreError::Invalid);
            }
            let person_id = PersonId::new();
            transaction.execute(
                "INSERT INTO persons (
                    id, display_name, aliases_json, calendar_emails_json,
                    created_at_utc_ms, updated_at_utc_ms
                 ) VALUES (?1, ?2, '[]', '[]', ?3, ?3)",
                params![id(person_id), display_name, now_utc_ms],
            )?;
            Ok((person_id, display_name, true))
        }
    }
}

fn profile_row_in(
    transaction: &Transaction<'_>,
    person_id: PersonId,
) -> Result<Option<VoiceProfileRow>, StoreError> {
    transaction
        .query_row(
            "SELECT person_id, model_id, model_revision, model_sha256, embedding_dimensions,
                    sample_rate_hz, feature_bins, feature_pipeline_revision, normalization,
                    sample_count, profile_revision, consent_version
               FROM voice_profiles WHERE person_id = ?1",
            params![id(person_id)],
            profile_row,
        )
        .optional()
        .map_err(Into::into)
}

fn profile_row_with_centroid(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(VoiceProfileRow, Vec<u8>)> {
    Ok((profile_row(row)?, row.get(12)?))
}

fn profile_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VoiceProfileRow> {
    let person_id = row.get::<_, String>(0)?;
    let embedding_dimensions: i64 = row.get(4)?;
    let sample_rate_hz: i64 = row.get(5)?;
    let feature_bins: i64 = row.get(6)?;
    let sample_count: i64 = row.get(9)?;
    let profile_revision: i64 = row.get(10)?;
    let consent_version: i64 = row.get(11)?;
    Ok(VoiceProfileRow {
        person_id: PersonId::from_uuid(uuid::Uuid::parse_str(&person_id).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?),
        model: VoiceProfileModelStatus {
            model_id: row.get(1)?,
            model_revision: row.get(2)?,
            model_sha256: row.get(3)?,
            embedding_dimensions: usize::try_from(embedding_dimensions)
                .map_err(integer_conversion_error)?,
            sample_rate_hz: u32::try_from(sample_rate_hz).map_err(integer_conversion_error)?,
            feature_bins: usize::try_from(feature_bins).map_err(integer_conversion_error)?,
            feature_pipeline_revision: row.get(7)?,
            normalization: row.get(8)?,
        },
        sample_count: u64::try_from(sample_count).map_err(integer_conversion_error)?,
        profile_revision: u64::try_from(profile_revision).map_err(integer_conversion_error)?,
        consent_version: u32::try_from(consent_version).map_err(integer_conversion_error)?,
    })
}

fn integer_conversion_error(
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, Box::new(error))
}

fn status_from_row(row: VoiceProfileRow) -> VoiceProfileStatus {
    VoiceProfileStatus::Enrolled {
        model: row.model,
        sample_count: row.sample_count,
        profile_revision: row.profile_revision,
        consent_version: row.consent_version,
    }
}

fn require_profile_model(
    profile: &VoiceProfileRow,
    model: SpeakerEmbeddingModelKey,
) -> Result<(), StoreError> {
    let expected = VoiceProfileModelStatus::from(model);
    (profile.model == expected)
        .then_some(())
        .ok_or(StoreError::ProfileModelIncompatible)
}

/// One model key, five readers: this conversion plus `require_profile_model`,
/// and the SQL predicates inside `unresolved_active_voice_speaker_ids`,
/// `voice_speaker_suggestions` and `user_voiceprint_in`. Adding a field to
/// `SpeakerEmbeddingModelKey` means changing all five, the three writers
/// (`insert_profile_in`, `store_speaker_voiceprints` and
/// `save_user_voiceprint`) and the three tables' migrations.
impl From<SpeakerEmbeddingModelKey> for VoiceProfileModelStatus {
    fn from(value: SpeakerEmbeddingModelKey) -> Self {
        Self {
            model_id: value.model_id.to_owned(),
            model_revision: value.model_revision.to_owned(),
            model_sha256: value.model_sha256.to_owned(),
            embedding_dimensions: value.embedding_dimensions,
            sample_rate_hz: value.sample_rate_hz,
            feature_bins: value.feature_bins,
            feature_pipeline_revision: value.feature_pipeline_revision.to_owned(),
            normalization: value.normalization.to_owned(),
        }
    }
}

fn insert_profile_in(
    transaction: &Transaction<'_>,
    person_id: PersonId,
    model: SpeakerEmbeddingModelKey,
    centroid: &[f32],
    sample_count: u64,
    profile_revision: u64,
    consent_version: u32,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    require_expected_model(model)?;
    let centroid =
        SpeakerEmbedding::from_normalized_slice(centroid).ok_or(StoreError::VoiceInvariant)?;
    transaction.execute(
        "INSERT INTO voice_profiles (
            person_id, model_id, model_revision, model_sha256, embedding_dimensions,
            sample_rate_hz, feature_bins, feature_pipeline_revision, normalization,
            centroid, sample_count, profile_revision, consent_version, created_at_utc_ms, updated_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
        params![
            id(person_id),
            model.model_id,
            model.model_revision,
            model.model_sha256,
            to_i64(u64::try_from(model.embedding_dimensions).map_err(|_| StoreError::VoiceInvariant)?)?,
            i64::from(model.sample_rate_hz),
            to_i64(u64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?)?,
            model.feature_pipeline_revision,
            model.normalization,
            embedding_blob(&centroid),
            to_i64(sample_count)?,
            to_i64(profile_revision)?,
            i64::from(consent_version),
            now_utc_ms,
        ],
    )?;
    Ok(())
}

fn recompute_profiles_in(
    transaction: &Transaction<'_>,
    people: &[PersonId],
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    for person_id in people {
        let Some(profile) = profile_row_in(transaction, *person_id)? else {
            continue;
        };
        recompute_profile_in(
            transaction,
            *person_id,
            profile
                .profile_revision
                .checked_add(1)
                .ok_or(StoreError::VoiceInvariant)?,
            now_utc_ms,
        )?;
    }
    Ok(())
}

fn recompute_profile_in(
    transaction: &Transaction<'_>,
    person_id: PersonId,
    profile_revision: u64,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    // Deliberately no model check. Cleanup paths (speaker correction, speaker
    // merge, session deletion, person split) run long after the running model
    // may have moved on, and the centroid is rebuilt only from this profile's
    // own samples, so the row stays internally consistent with its own model
    // columns. Requiring compatibility here would make a meeting that
    // contributed a sample undeletable after a model revision.
    if profile_row_in(transaction, person_id)?.is_none() {
        return Err(StoreError::VoiceInvariant);
    }
    let mut statement = transaction.prepare(
        "SELECT embedding FROM voice_profile_samples WHERE person_id = ?1
          ORDER BY source_session_id, source_generation_id, source_speaker_id",
    )?;
    let samples = statement
        .query_map(params![id(person_id)], |row| row.get::<_, Vec<u8>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if samples.is_empty() {
        transaction.execute(
            "DELETE FROM voice_profiles WHERE person_id = ?1",
            params![id(person_id)],
        )?;
        return Ok(());
    }
    let embeddings = samples
        .iter()
        .map(|sample| embedding_from_blob(sample))
        .collect::<Result<Vec<_>, _>>()?;
    let centroid = mean_embedding(&embeddings).ok_or(StoreError::VoiceInvariant)?;
    transaction.execute(
        "UPDATE voice_profiles
            SET centroid = ?1, sample_count = ?2, profile_revision = ?3, updated_at_utc_ms = ?4
          WHERE person_id = ?5",
        params![
            embedding_blob(&centroid),
            to_i64(u64::try_from(samples.len()).map_err(|_| StoreError::VoiceInvariant)?)?,
            to_i64(profile_revision)?,
            now_utc_ms,
            id(person_id),
        ],
    )?;
    Ok(())
}

fn mean_embedding<'a>(
    embeddings: impl IntoIterator<Item = &'a SpeakerEmbedding>,
) -> Option<SpeakerEmbedding> {
    let mut sums = [0.0_f32; SPEAKER_EMBEDDING_DIMENSIONS];
    for embedding in embeddings {
        for (sum, value) in sums.iter_mut().zip(embedding.as_slice()) {
            *sum += *value;
        }
    }
    let norm = sums.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return None;
    }
    let centroid: [f32; SPEAKER_EMBEDDING_DIMENSIONS] =
        std::array::from_fn(|index| sums[index] / norm);
    SpeakerEmbedding::from_normalized_slice(&centroid)
}

fn sample_profile_people_in<const N: usize>(
    transaction: &Transaction<'_>,
    predicate: &str,
    params_values: [String; N],
) -> Result<Vec<PersonId>, StoreError> {
    let query = format!(
        "SELECT DISTINCT person_id FROM voice_profile_samples WHERE {predicate} ORDER BY person_id"
    );
    let mut statement = transaction.prepare(&query)?;
    let rows = statement.query_map(rusqlite::params_from_iter(params_values), |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| {
        row.map_err(Into::into)
            .and_then(|person_id| Ok(PersonId::from_uuid(parse_uuid(&person_id)?)))
    })
    .collect()
}

fn embedding_blob(embedding: &SpeakerEmbedding) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(EMBEDDING_BYTES);
    for value in embedding.as_slice() {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

pub(in crate::meeting::store) fn embedding_from_blob(
    bytes: &[u8],
) -> Result<SpeakerEmbedding, StoreError> {
    if bytes.len() != EMBEDDING_BYTES {
        return Err(StoreError::VoiceInvariant);
    }
    let mut values = [0.0; SPEAKER_EMBEDDING_DIMENSIONS];
    for (index, value) in values.iter_mut().enumerate() {
        let offset = index * size_of::<f32>();
        let value_bytes: [u8; size_of::<f32>()] = bytes[offset..offset + size_of::<f32>()]
            .try_into()
            .map_err(|_| StoreError::VoiceInvariant)?;
        *value = f32::from_le_bytes(value_bytes);
    }
    SpeakerEmbedding::from_normalized_slice(&values).ok_or(StoreError::VoiceInvariant)
}

fn from_i64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::VoiceInvariant)
}

fn parse_uuid(value: &str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::VoiceInvariant)
}

#[cfg(test)]
mod tests {
    use super::speaker_is_user;
    use crate::meeting::types::SourceKind;

    #[test]
    fn a_call_voice_with_only_the_users_first_name_is_someone_else() {
        let user = Some("Alex Chen");

        assert!(!speaker_is_user(
            "Alex",
            SourceKind::SystemAudio,
            false,
            false,
            user
        ));
        assert!(speaker_is_user(
            "alex  chen",
            SourceKind::SystemAudio,
            false,
            false,
            user
        ));
        assert!(speaker_is_user(
            "Alex",
            SourceKind::Microphone,
            true,
            false,
            user
        ));
    }
}
