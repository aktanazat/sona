use super::people::{bump_people_revision_in, people_revision_in, person_by_id_in, upsert_link_in};
use super::{
    append_event, committed_receipt, decode_json, id, insert_operation_receipt,
    mark_artifacts_out_of_date, operation_receipt_in, session_row, to_i64, utc_now_ms,
    DurableTrackRecord, MeetingStore, StoreError, StoreMutation,
};
use crate::meeting::diarization::{
    match_speaker_profile, wespeaker_embedding_model_key, SpeakerEmbedding,
    SpeakerEmbeddingModelKey, SPEAKER_EMBEDDING_DIMENSIONS,
};
use crate::meeting::people_types::{
    PersonId, PersonLinkConfidence, PersonLinkSource, VoiceProfileMergeResolution,
};
use crate::meeting::types::{
    MeetingCommandKind, MeetingDiarizationGenerationId, MeetingOperationId, MeetingOrigin,
    MeetingPhase, MeetingSessionId, OperationActor, OperationReceipt, SourceTrackId, SpeakerId,
};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::HashSet;
use std::num::NonZeroU32;
const EMBEDDING_BYTES: usize = SPEAKER_EMBEDDING_DIMENSIONS * size_of::<f32>();
// The voice migration spells these two numbers as literals it cannot compute:
// `embedding_dimensions = 256` and `length(centroid) = 1024`. Sample rate and
// feature bins come from the model manifest at runtime and are checked there.
const _: () = assert!(SPEAKER_EMBEDDING_DIMENSIONS == 256 && EMBEDDING_BYTES == 1024);
const MIN_ENROLLMENT_EVIDENCE_SPANS: usize = 2;
/// Only caller-supplied evidence can exceed the minimum. `local_voice_enrollment_evidence`
/// returns the moment a run reaches `MIN_ENROLLMENT_EVIDENCE_SPANS`, so this cap
/// bounds a `VoiceEnrollmentEvidence` the store did not assemble itself.
const MAX_ENROLLMENT_EVIDENCE_SPANS: usize = 3;
const MAX_ENROLLMENT_EVIDENCE_DURATION_NS: u64 = 12_000_000_000;

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
        let mut run: Option<(String, String, u64, u64, usize)> = None;
        for (generation_id, track_id, start_offset_ns, end_offset_ns) in rows {
            let start_offset_ns = from_i64(start_offset_ns)?;
            let end_offset_ns = from_i64(end_offset_ns)?;
            if end_offset_ns <= start_offset_ns {
                return Err(StoreError::VoiceInvariant);
            }
            let Some((run_generation, run_track, run_start, run_end, count)) = &mut run else {
                run = Some((generation_id, track_id, start_offset_ns, end_offset_ns, 1));
                continue;
            };
            let same_run = *run_generation == generation_id
                && *run_track == track_id
                && *run_end == start_offset_ns
                && end_offset_ns
                    .checked_sub(*run_start)
                    .ok_or(StoreError::VoiceInvariant)?
                    <= MAX_ENROLLMENT_EVIDENCE_DURATION_NS;
            if same_run {
                *run_end = end_offset_ns;
                *count += 1;
                if *count >= MIN_ENROLLMENT_EVIDENCE_SPANS {
                    return VoiceEnrollmentEvidence::new(
                        session_id,
                        MeetingDiarizationGenerationId::from_uuid(parse_uuid(run_generation)?),
                        speaker_id,
                        SourceTrackId::from_uuid(parse_uuid(run_track)?),
                        *run_start,
                        *run_end,
                    )
                    .map_err(|_| StoreError::LocalEvidenceUnavailable);
                }
            } else {
                run = Some((generation_id, track_id, start_offset_ns, end_offset_ns, 1));
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
                        AND start_offset_ns >= ?4 AND end_offset_ns <= ?5
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
                        start_offset_ns: from_i64(start_offset_ns)?,
                        end_offset_ns: from_i64(end_offset_ns)?,
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
        let change = remove_voice_person_evidence_in(&transaction, person_id)?;
        if change.people_changed() {
            bump_people_revision_in(&transaction)?;
        }
        transaction.commit()?;
        Ok(VoiceProfileStatus::Unenrolled)
    }

    pub(crate) fn has_compatible_local_voice_profiles(
        &self,
        model: SpeakerEmbeddingModelKey,
    ) -> Result<bool, StoreError> {
        require_expected_model(model)?;
        let connection = self.connection()?;
        let exists: i64 = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM voice_profiles
                 WHERE model_id = ?1 AND model_revision = ?2 AND model_sha256 = ?3
                   AND embedding_dimensions = ?4 AND sample_rate_hz = ?5 AND feature_bins = ?6
                   AND feature_pipeline_revision = ?7 AND normalization = ?8
                   AND sample_count > 0
             )",
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
            |row| row.get(0),
        )?;
        Ok(exists != 0)
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
    pub(crate) fn unresolved_active_voice_speaker_ids(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<SpeakerId>, StoreError> {
        let connection = self.connection()?;
        let model = wespeaker_embedding_model_key();
        let mut statement = connection.prepare(
            "SELECT speaker.speaker_id FROM meeting_speakers speaker
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
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| {
            row.map_err(Into::into)
                .and_then(|speaker_id| Ok(SpeakerId::from_uuid(parse_uuid(&speaker_id)?)))
        })
        .collect()
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
        let speaker_revision = active_speaker_revision_in(&transaction, session_id, speaker_id)?;
        if speaker_revision != expected_speaker_revision {
            return Err(StoreError::StaleRevision);
        }
        let profile =
            profile_row_in(&transaction, matched.person_id)?.ok_or(StoreError::StaleRevision)?;
        require_profile_model(&profile, wespeaker_embedding_model_key())?;
        if profile.profile_revision != matched.profile_revision || profile.sample_count == 0 {
            return Err(StoreError::StaleRevision);
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
    pub(crate) fn identify_speaker(
        &self,
        request: MeetingSpeakerIdentifyRequest,
    ) -> Result<VoiceSpeakerIdentityResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = operation_receipt_in(&transaction, request.operation_id)? {
            let resolved_person_id =
                resolved_person_id_for_operation_in(&transaction, request.operation_id)?;
            transaction.commit()?;
            return Ok(VoiceSpeakerIdentityResult {
                receipt,
                resolved_person_id,
            });
        }
        let session = session_row(&transaction, request.session_id)?;
        if session.revision != request.expected_meeting_revision {
            return Err(StoreError::StaleRevision);
        }
        if !matches!(
            session.phase,
            MeetingPhase::CapturingRecording
                | MeetingPhase::CapturingPaused
                | MeetingPhase::Processing
                | MeetingPhase::ReviewReady
                | MeetingPhase::RecoveryRequired
        ) {
            return Err(StoreError::Conflict);
        }
        require_voice_people_revision_in(&transaction, request.expected_people_revision)?;
        active_speaker_revision_in(&transaction, request.session_id, request.speaker_id)?;

        // Label and CorrectTo resolve and link a target identically; they differ
        // only in whether the speaker's existing samples survive. MarkUnknown is
        // the same shape with no target.
        let (target, remove_samples) = match request.disposition {
            MeetingSpeakerIdentifyDisposition::Label { target } => (Some(target), false),
            MeetingSpeakerIdentifyDisposition::CorrectTo { target } => (Some(target), true),
            MeetingSpeakerIdentifyDisposition::MarkUnknown => (None, true),
        };
        let (display_name, person_changed, link_changed, manual_resolution) = match target {
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
        let profiles_changed = if remove_samples {
            remove_profile_samples_for_speaker_in(&transaction, request.speaker_id)?
        } else {
            false
        };
        transaction.execute(
            "UPDATE meeting_speakers
                SET display_name = ?1, revision = revision + 1
              WHERE speaker_id = ?2 AND session_id = ?3 AND merged_into_speaker_id IS NULL",
            params![display_name, id(request.speaker_id), id(request.session_id)],
        )?;
        mark_artifacts_out_of_date(&transaction, request.session_id)?;
        let next_revision = session
            .revision
            .checked_add(1)
            .ok_or(StoreError::VoiceInvariant)?;
        transaction.execute(
            "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
            params![to_i64(next_revision)?, id(request.session_id)],
        )?;
        append_event(
            &transaction,
            request.session_id,
            next_revision,
            session.phase,
            session.phase,
            "speaker_identified",
            None,
        )?;
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
        persist_manual_voice_speaker_resolution_in(
            &transaction,
            request.operation_id,
            request.speaker_id,
            manual_resolution,
        )?;
        transaction.commit()?;
        Ok(VoiceSpeakerIdentityResult {
            receipt,
            resolved_person_id,
        })
    }
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
    transaction.execute(
        "UPDATE meeting_voice_speaker_resolutions
            SET is_current = 0
          WHERE speaker_id = ?1 AND is_current = 1",
        params![id(speaker_id)],
    )?;
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
            clear_person_matches_in(transaction, source_person_id)?;
            let next_profile_revision = source
                .profile_revision
                .checked_add(1)
                .ok_or(StoreError::VoiceInvariant)?;
            // The copied centroid is a placeholder; `recompute_profile_in` below
            // rebuilds it from the samples now pointing at the target.
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
            )?;
            change.profiles_changed = true;
            Ok(change)
        }
        VoiceProfileMergeResolution::CombineCompatible => {
            let (Some(source), Some(target)) = (source, target) else {
                return Err(StoreError::ProfileModelIncompatible);
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
    let origin_json: String = transaction.query_row(
        "SELECT origin_kind FROM meeting_sessions WHERE id = ?1",
        params![id(evidence.session_id)],
        |row| row.get(0),
    )?;
    if matches!(decode_json(&origin_json)?, MeetingOrigin::Import) {
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
                AND start_offset_ns >= ?4 AND end_offset_ns <= ?5
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
    if !(MIN_ENROLLMENT_EVIDENCE_SPANS..=MAX_ENROLLMENT_EVIDENCE_SPANS).contains(&spans.len()) {
        return Err(StoreError::InsufficientEnrollmentEvidence);
    }
    let mut end_offset_ns = evidence.start_offset_ns;
    for (start_offset_ns, span_end_offset_ns) in spans {
        let start_offset_ns = from_i64(start_offset_ns)?;
        let span_end_offset_ns = from_i64(span_end_offset_ns)?;
        if start_offset_ns != end_offset_ns || span_end_offset_ns <= start_offset_ns {
            return Err(StoreError::InsufficientEnrollmentEvidence);
        }
        end_offset_ns = span_end_offset_ns;
    }
    if end_offset_ns != evidence.end_offset_ns
        || evidence
            .end_offset_ns
            .checked_sub(evidence.start_offset_ns)
            .ok_or(StoreError::VoiceInvariant)?
            > MAX_ENROLLMENT_EVIDENCE_DURATION_NS
    {
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

/// One model key, four readers: this conversion plus `require_profile_model`,
/// the SQL predicate in `has_compatible_local_voice_profiles`, and the SQL
/// predicate inside `unresolved_active_voice_speaker_ids`. Adding a field to
/// `SpeakerEmbeddingModelKey` means changing all four and the migration.
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
    let mut sums = [0.0_f32; SPEAKER_EMBEDDING_DIMENSIONS];
    for sample in &samples {
        let embedding = embedding_from_blob(sample)?;
        for (sum, value) in sums.iter_mut().zip(embedding.as_slice()) {
            *sum += *value;
        }
    }
    let norm = sums.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(StoreError::VoiceInvariant);
    }
    let centroid: [f32; SPEAKER_EMBEDDING_DIMENSIONS] =
        std::array::from_fn(|index| sums[index] / norm);
    let centroid =
        SpeakerEmbedding::from_normalized_slice(&centroid).ok_or(StoreError::VoiceInvariant)?;
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

fn embedding_from_blob(bytes: &[u8]) -> Result<SpeakerEmbedding, StoreError> {
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
