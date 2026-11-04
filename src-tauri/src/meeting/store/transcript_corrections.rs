mod relisten;
mod scope;

use super::{
    canonical_speaker_ids, effective_segments_for_session, from_i64, id,
    mark_artifacts_out_of_date, parse_uuid, session_row, to_i64, utc_now_ms, MeetingStore,
    StoreError,
};
use crate::meeting::diarization::wespeaker_embedding_model_key;
use crate::meeting::people_types::PersonId;
use crate::meeting::transcript_correction_types::{
    TranscriptCorrection, TranscriptCorrectionChangedSession, TranscriptCorrectionContext,
    TranscriptCorrectionKind, TranscriptCorrectionList, TranscriptCorrectionMutationResult,
    TranscriptCorrectionRemoveRequest, TranscriptCorrectionReview, TranscriptCorrectionSegment,
    TranscriptCorrectionStatus, TranscriptCorrectionTeachRequest,
    TranscriptCorrectionUpdateRequest,
};
use crate::meeting::types::{
    EffectiveTranscriptSegment, MeetingPhase, MeetingSessionId, SpeakerAssignmentKind, SpeakerId,
    TranscriptSegmentId,
};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::HashMap;
use std::sync::LazyLock;
use uuid::Uuid;

static WORD_CHARACTER: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    Regex::new(r"[\p{L}\p{M}\p{N}\p{Pc}'’]").expect("constant word-boundary pattern")
});

pub(crate) struct TranscriptCorrectionSyncSnapshot {
    sessions: Vec<(MeetingSessionId, HashMap<TranscriptSegmentId, String>)>,
}

impl MeetingStore {
    pub(crate) fn transcript_correction_identity_snapshot(
        &self,
        session_id: MeetingSessionId,
        speaker_ids: &[SpeakerId],
    ) -> Result<TranscriptCorrectionSyncSnapshot, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT session.id FROM meeting_sessions session
             WHERE session.phase != 'deleting'
               AND session.transcript_purged_at_utc_ms IS NULL
               AND session.current_transcript_revision_id IS NOT NULL
               AND (session.id = ?1 OR session.id IN (
                   SELECT session_id FROM transcript_correction_dependent_sessions
                   WHERE person_id IN (
                       SELECT person_id FROM person_transcript_corrections
                       WHERE source_speaker_id = ?2
                       UNION
                       SELECT person_id FROM voice_profile_samples WHERE source_speaker_id = ?2
                   )
               ))
             ORDER BY session.id",
        )?;
        let mut sessions = Vec::new();
        for speaker_id in speaker_ids {
            sessions.extend(
                statement
                    .query_map(params![id(session_id), id(*speaker_id)], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        sessions.sort_unstable();
        sessions.dedup();
        projection_snapshot_in(&connection, sessions)
    }

    pub(crate) fn transcript_correction_source_snapshot(
        &self,
        source_session_id: MeetingSessionId,
    ) -> Result<TranscriptCorrectionSyncSnapshot, StoreError> {
        let source_id = id(source_session_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT person_id FROM person_transcript_corrections
             WHERE source_session_id = ?1",
        )?;
        let person_ids = statement
            .query_map(params![&source_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut sessions = Vec::new();
        for raw in person_ids {
            sessions.extend(person_sessions_in(
                &connection,
                PersonId(parse_uuid(&raw)?),
            )?);
        }
        sessions.retain(|session| session != &source_id);
        sessions.sort_unstable();
        sessions.dedup();
        projection_snapshot_in(&connection, sessions)
    }

    pub(crate) fn transcript_correction_people_snapshot(
        &self,
        person_ids: &[PersonId],
    ) -> Result<TranscriptCorrectionSyncSnapshot, StoreError> {
        let connection = self.connection()?;
        let mut sessions = Vec::new();
        for person_id in person_ids {
            sessions.extend(person_sessions_in(&connection, *person_id)?);
        }
        sessions.sort_unstable();
        sessions.dedup();
        projection_snapshot_in(&connection, sessions)
    }

    pub(crate) fn transcript_correction_projection_changes(
        &self,
        before: TranscriptCorrectionSyncSnapshot,
    ) -> Result<Vec<TranscriptCorrectionChangedSession>, StoreError> {
        let connection = self.connection()?;
        changed_projection_sessions_in(&connection, before)?
            .into_iter()
            .map(|raw| {
                let session_id = MeetingSessionId::from_uuid(parse_uuid(&raw)?);
                Ok(TranscriptCorrectionChangedSession {
                    session_id,
                    revision: session_row(&connection, session_id)?.revision,
                })
            })
            .collect()
    }

    pub(crate) fn transcript_corrections(
        &self,
        person_id: PersonId,
    ) -> Result<TranscriptCorrectionList, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        require_person_in(&transaction, person_id)?;
        let result = list_in(&transaction, person_id)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn transcript_correction_review(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<TranscriptCorrectionReview, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        super::transcript_retention::require_retained_transcript_in(&transaction, session_id)?;
        let session = session_row(&transaction, session_id)?;
        let canonical = canonical_speaker_ids(&transaction, session_id)?;
        let segments = effective_segments_for_session(&transaction, session_id)?;
        let mut people = HashMap::new();
        let mut identities = HashMap::new();
        let mut review_segments = Vec::with_capacity(segments.len());
        for segment in segments.into_iter().filter(|segment| !segment.removed) {
            let speaker_id = canonical
                .get(&segment.assigned_speaker_id)
                .copied()
                .unwrap_or(segment.assigned_speaker_id);
            let (person_id, confirmed_person_id) = if usable_assignment(segment.speaker_assignment)
            {
                match identities.get(&speaker_id) {
                    Some(identity) => *identity,
                    None => {
                        let identity = (
                            resolved_person_for_speaker_in(&transaction, session_id, speaker_id)?,
                            confirmed_person_for_speaker_in(&transaction, session_id, speaker_id)?,
                        );
                        identities.insert(speaker_id, identity);
                        identity
                    }
                }
            } else {
                (None, None)
            };
            let person_name = if let Some(person_id) = person_id {
                if !people.contains_key(&person_id) {
                    let name: String = transaction.query_row(
                        "SELECT display_name FROM persons WHERE id = ?1",
                        params![id(person_id)],
                        |row| row.get(0),
                    )?;
                    people.insert(person_id, name);
                }
                people.get(&person_id).cloned()
            } else {
                None
            };
            review_segments.push(TranscriptCorrectionSegment {
                segment_id: segment.base.segment_id,
                speaker_id,
                start_offset_ns: segment.base.start_offset_ns,
                person_id,
                person_name,
                identity_confirmed: person_id.is_some() && confirmed_person_id == person_id,
                audio_retained: audio_retained_in(
                    &transaction,
                    session_id,
                    segment.base.segment_id,
                )?,
                corrected_text: segment
                    .replacement_text
                    .unwrap_or_else(|| segment.base.text.clone()),
                raw_text: segment.base.text,
                explicitly_edited: segment.edit_revision.is_some(),
            });
        }
        let mut rules = Vec::new();
        for person_id in people.keys() {
            rules.extend(rules_in(&transaction, *person_id)?);
        }
        rules.sort_by(|left, right| {
            left.person_id
                .0
                .cmp(&right.person_id.0)
                .then_with(|| left.original_phrase.cmp(&right.original_phrase))
        });
        let result = TranscriptCorrectionReview {
            session_id,
            meeting_revision: session.revision,
            correction_revision: revision_in(&transaction)?,
            can_edit: editable_phase(session.phase),
            segments: review_segments,
            rules,
        };
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn teach_transcript_correction(
        &self,
        request: TranscriptCorrectionTeachRequest,
    ) -> Result<TranscriptCorrectionMutationResult, StoreError> {
        validate_phrases(&request.original_phrase, &request.replacement_phrase)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_revision_in(&transaction, request.expected_correction_revision)?;
        require_person_in(&transaction, request.person_id)?;
        super::transcript_retention::require_retained_transcript_in(
            &transaction,
            request.session_id,
        )?;
        let session = session_row(&transaction, request.session_id)?;
        if session.revision != request.expected_meeting_revision {
            return Err(StoreError::StaleRevision);
        }
        if !editable_phase(session.phase) {
            return Err(StoreError::Conflict);
        }
        let source = source_segment_in(&transaction, request.session_id, request.segment_id)?;
        if !source.is_current {
            return Err(StoreError::StaleRevision);
        }
        if source.person_id != Some(request.person_id) || !source.identity_confirmed {
            return Err(StoreError::ExplicitConsentRequired);
        }
        let corrected = replace_phrase(
            &source.raw_text,
            &request.original_phrase,
            &request.replacement_phrase,
        )
        .ok_or(StoreError::Invalid)?;
        scope::validate(
            &request.context,
            request.kind,
            &source.raw_text,
            &request.original_phrase,
        )?;
        let context_json =
            serde_json::to_string(&request.context).map_err(|_| StoreError::Invalid)?;
        let kind_json = serde_json::to_string(&request.kind).map_err(|_| StoreError::Invalid)?;
        let existing: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM person_transcript_corrections
             WHERE person_id = ?1 AND original_phrase = ?2 AND context_json = ?3
               AND (json_extract(context_json, '$.kind') != 'conversation' OR source_session_id = ?4))",
            params![id(request.person_id), request.original_phrase, context_json, id(request.session_id)],
            |row| row.get(0),
        )?;
        if existing {
            return Err(StoreError::Conflict);
        }
        let before = projection_snapshot_in(
            &transaction,
            person_sessions_in(&transaction, request.person_id)?,
        )?;
        let now = utc_now_ms();
        transaction.execute(
            "INSERT INTO person_transcript_corrections (
                correction_id, person_id, source_session_id, source_segment_id, source_speaker_id,
                original_text, corrected_text, original_phrase, replacement_phrase,
                created_at_utc_ms, updated_at_utc_ms, kind_json, context_json, enabled
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?12, 1)",
            params![
                Uuid::new_v4().to_string(),
                id(request.person_id),
                id(request.session_id),
                id(request.segment_id),
                id(source.speaker_id),
                source.raw_text,
                corrected,
                request.original_phrase,
                request.replacement_phrase,
                now,
                kind_json,
                context_json
            ],
        )?;
        let changed_sessions = invalidate_changed_projections_in(&transaction, before)?;
        let corrections = list_in(&transaction, request.person_id)?;
        transaction.commit()?;
        Ok(TranscriptCorrectionMutationResult {
            corrections,
            changed_sessions,
        })
    }

    pub(crate) fn update_transcript_correction(
        &self,
        request: TranscriptCorrectionUpdateRequest,
    ) -> Result<TranscriptCorrectionMutationResult, StoreError> {
        validate_phrases(&request.original_phrase, &request.replacement_phrase)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_revision_in(&transaction, request.expected_correction_revision)?;
        require_person_in(&transaction, request.person_id)?;
        let rules = rules_in(&transaction, request.person_id)?;
        let rule = rules
            .iter()
            .find(|rule| rule.correction_id == request.correction_id)
            .ok_or(StoreError::NotFound)?;
        if request.enabled && rule.status == TranscriptCorrectionStatus::SourceChanged {
            return Err(StoreError::ExplicitConsentRequired);
        }
        scope::validate(
            &request.context,
            request.kind,
            &rule.original_text,
            &request.original_phrase,
        )?;
        let corrected = replace_phrase(
            &rule.original_text,
            &request.original_phrase,
            &request.replacement_phrase,
        )
        .ok_or(StoreError::Invalid)?;
        if rules.iter().any(|other| {
            other.correction_id != rule.correction_id
                && other.original_phrase == request.original_phrase
                && other.enabled
                && request.enabled
                && other.replacement_phrase != request.replacement_phrase
                && scope::overlaps(
                    &other.context,
                    other.source_session_id,
                    &request.context,
                    rule.source_session_id,
                )
        }) {
            return Err(StoreError::Conflict);
        }
        let before = projection_snapshot_in(
            &transaction,
            person_sessions_in(&transaction, request.person_id)?,
        )?;
        transaction.execute(
            "UPDATE person_transcript_corrections SET original_phrase = ?1,
                replacement_phrase = ?2, corrected_text = ?3, updated_at_utc_ms = ?4,
                kind_json = ?7, context_json = ?8, enabled = ?9
             WHERE correction_id = ?5 AND person_id = ?6",
            params![
                request.original_phrase,
                request.replacement_phrase,
                corrected,
                utc_now_ms(),
                request.correction_id.to_string(),
                id(request.person_id),
                serde_json::to_string(&request.kind).map_err(|_| StoreError::Invalid)?,
                serde_json::to_string(&request.context).map_err(|_| StoreError::Invalid)?,
                request.enabled
            ],
        )?;
        let changed_sessions = invalidate_changed_projections_in(&transaction, before)?;
        let corrections = list_in(&transaction, request.person_id)?;
        transaction.commit()?;
        Ok(TranscriptCorrectionMutationResult {
            corrections,
            changed_sessions,
        })
    }

    pub(crate) fn remove_transcript_correction(
        &self,
        request: TranscriptCorrectionRemoveRequest,
    ) -> Result<TranscriptCorrectionMutationResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_revision_in(&transaction, request.expected_correction_revision)?;
        require_person_in(&transaction, request.person_id)?;
        let before = projection_snapshot_in(
            &transaction,
            person_sessions_in(&transaction, request.person_id)?,
        )?;
        let changed = transaction.execute(
            "DELETE FROM person_transcript_corrections WHERE correction_id = ?1 AND person_id = ?2",
            params![request.correction_id.to_string(), id(request.person_id)],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        let changed_sessions = invalidate_changed_projections_in(&transaction, before)?;
        let corrections = list_in(&transaction, request.person_id)?;
        transaction.commit()?;
        Ok(TranscriptCorrectionMutationResult {
            corrections,
            changed_sessions,
        })
    }
}

pub(super) fn prepare_person_merge_in(
    transaction: &Transaction<'_>,
    source_id: PersonId,
    target_id: PersonId,
) -> Result<(), StoreError> {
    let conflicting: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM person_transcript_corrections source
            JOIN person_transcript_corrections target
              ON target.original_phrase = source.original_phrase
            WHERE source.person_id = ?1 AND target.person_id = ?2
              AND source.replacement_phrase != target.replacement_phrase
        )",
        params![id(source_id), id(target_id)],
        |row| row.get(0),
    )?;
    if conflicting {
        return Err(StoreError::TranscriptCorrectionConflict);
    }
    let mut statement = transaction.prepare(
        "SELECT DISTINCT session_id FROM transcript_correction_dependent_sessions
         WHERE person_id IN (?1, ?2) AND EXISTS (
             SELECT 1 FROM person_transcript_corrections WHERE person_id IN (?1, ?2)
         )
         ORDER BY session_id",
    )?;
    let sessions = statement
        .query_map(params![id(source_id), id(target_id)], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    invalidate_sessions_in(transaction, sessions)?;
    Ok(())
}

pub(super) fn merge_person_corrections_in(
    transaction: &Transaction<'_>,
    source_id: PersonId,
    target_id: PersonId,
) -> Result<(), StoreError> {
    let duplicates = {
        let mut statement = transaction.prepare(
            "SELECT source.correction_id, target.correction_id,
                    source.source_session_id, source.source_segment_id,
                    target.source_session_id, target.source_segment_id,
                    (source.created_at_utc_ms, source.correction_id)
                        < (target.created_at_utc_ms, target.correction_id)
             FROM person_transcript_corrections source
             JOIN person_transcript_corrections target
               ON target.original_phrase = source.original_phrase
              AND target.context_json = source.context_json
              AND target.kind_json = source.kind_json
              AND target.enabled = source.enabled
              AND (json_extract(source.context_json, '$.kind') != 'conversation'
                   OR target.source_session_id = source.source_session_id)
             WHERE source.person_id = ?1 AND target.person_id = ?2
             ORDER BY source.original_phrase",
        )?;
        let rows = statement
            .query_map(params![id(source_id), id(target_id)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, bool>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (
        source_correction,
        target_correction,
        source_session,
        source_segment,
        target_session,
        target_segment,
        source_first,
    ) in duplicates
    {
        let source = source_segment_in(
            transaction,
            MeetingSessionId::from_uuid(parse_uuid(&source_session)?),
            TranscriptSegmentId::from_uuid(parse_uuid(&source_segment)?),
        )?;
        let target = source_segment_in(
            transaction,
            MeetingSessionId::from_uuid(parse_uuid(&target_session)?),
            TranscriptSegmentId::from_uuid(parse_uuid(&target_segment)?),
        )?;
        let source_active = source.is_current && source.person_id == Some(target_id);
        let target_active = target.is_current && target.person_id == Some(target_id);
        let keep_source =
            (source_active && !target_active) || (source_active == target_active && source_first);
        transaction.execute(
            "DELETE FROM person_transcript_corrections WHERE correction_id = ?1",
            params![if keep_source {
                target_correction
            } else {
                source_correction
            }],
        )?;
    }
    transaction.execute(
        "UPDATE person_transcript_corrections SET person_id = ?1 WHERE person_id = ?2",
        params![id(target_id), id(source_id)],
    )?;
    Ok(())
}

pub(crate) fn resolved_person_for_speaker_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
) -> Result<Option<PersonId>, StoreError> {
    let manual: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT resolution.resolution_kind, resolution.resolved_person_id
         FROM meeting_voice_speaker_resolutions resolution
         JOIN meeting_speakers speaker ON speaker.speaker_id = resolution.speaker_id
         WHERE resolution.speaker_id = ?1 AND resolution.is_current = 1
           AND speaker.session_id = ?2 AND speaker.merged_into_speaker_id IS NULL",
            params![id(speaker_id), id(session_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((kind, person_id)) = manual {
        return match (kind.as_str(), person_id) {
            ("identified", Some(person_id)) => Ok(Some(PersonId(parse_uuid(&person_id)?))),
            ("unknown", None) => Ok(None),
            _ => Err(StoreError::Corrupt),
        };
    }
    let model = wespeaker_embedding_model_key();
    let person_id: Option<String> = connection
        .query_row(
            "SELECT matched.person_id FROM voice_speaker_matches matched
         JOIN meeting_speakers speaker ON speaker.speaker_id = matched.speaker_id
         JOIN voice_profiles profile ON profile.person_id = matched.person_id
         WHERE matched.speaker_id = ?1 AND matched.session_id = ?2
           AND speaker.session_id = ?2 AND speaker.merged_into_speaker_id IS NULL
           AND matched.speaker_revision = speaker.revision
           AND matched.profile_revision = profile.profile_revision AND profile.sample_count > 0
           AND profile.model_id = ?3 AND profile.model_revision = ?4 AND profile.model_sha256 = ?5
           AND profile.embedding_dimensions = ?6 AND profile.sample_rate_hz = ?7
           AND profile.feature_bins = ?8 AND profile.feature_pipeline_revision = ?9
           AND profile.normalization = ?10",
            params![
                id(speaker_id),
                id(session_id),
                model.model_id,
                model.model_revision,
                model.model_sha256,
                i64::try_from(model.embedding_dimensions).map_err(|_| StoreError::Corrupt)?,
                i64::from(model.sample_rate_hz),
                i64::try_from(model.feature_bins).map_err(|_| StoreError::Corrupt)?,
                model.feature_pipeline_revision,
                model.normalization
            ],
            |row| row.get(0),
        )
        .optional()?;
    person_id
        .map(|raw| parse_uuid(&raw).map(PersonId))
        .transpose()
}

pub(crate) fn confirmed_person_for_speaker_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
) -> Result<Option<PersonId>, StoreError> {
    let person: Option<String> = connection
        .query_row(
            "SELECT resolution.resolved_person_id
         FROM meeting_voice_speaker_resolutions resolution
         JOIN meeting_speakers speaker ON speaker.speaker_id = resolution.speaker_id
         WHERE resolution.speaker_id = ?1 AND speaker.session_id = ?2
           AND speaker.merged_into_speaker_id IS NULL
           AND resolution.is_current = 1 AND resolution.resolution_kind = 'identified'",
            params![id(speaker_id), id(session_id)],
            |row| row.get(0),
        )
        .optional()?;
    person.map(|raw| parse_uuid(&raw).map(PersonId)).transpose()
}

pub(crate) fn apply_learned_corrections_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    segments: &mut [EffectiveTranscriptSegment],
) -> Result<(), StoreError> {
    let canonical = canonical_speaker_ids(connection, session_id)?;
    let mut identities = HashMap::new();
    let mut rules_by_person = HashMap::new();
    for segment in segments {
        if segment.removed
            || segment.replacement_text.is_some()
            || !usable_assignment(segment.speaker_assignment)
        {
            continue;
        }
        let speaker_id = canonical
            .get(&segment.assigned_speaker_id)
            .copied()
            .unwrap_or(segment.assigned_speaker_id);
        let person_id = match identities.get(&speaker_id) {
            Some(person_id) => *person_id,
            None => {
                let person_id = resolved_person_for_speaker_in(connection, session_id, speaker_id)?;
                identities.insert(speaker_id, person_id);
                person_id
            }
        };
        let Some(person_id) = person_id else { continue };
        if !rules_by_person.contains_key(&person_id) {
            rules_by_person.insert(person_id, rules_in(connection, person_id)?);
        }
        if let Some(rules) = rules_by_person.get(&person_id) {
            segment.replacement_text = replace_rules(&segment.base.text, session_id, rules);
        }
    }
    Ok(())
}

fn usable_assignment(assignment: SpeakerAssignmentKind) -> bool {
    matches!(
        assignment,
        SpeakerAssignmentKind::LocalSpeaker | SpeakerAssignmentKind::SystemSpeaker
    )
}

fn editable_phase(phase: MeetingPhase) -> bool {
    matches!(
        phase,
        MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
    )
}

struct CorrectionSource {
    speaker_id: SpeakerId,
    person_id: Option<PersonId>,
    raw_text: String,
    is_current: bool,
    identity_confirmed: bool,
    start_offset_ns: u64,
}

fn source_segment_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    segment_id: TranscriptSegmentId,
) -> Result<CorrectionSource, StoreError> {
    let row: Option<(String, String, String, bool, bool, i64)> = connection.query_row(
        "SELECT segment.base_text, COALESCE(assignment.speaker_id, segment.speaker_id),
                COALESCE(assignment.assignment_kind,
                    CASE track.source_kind WHEN 'microphone' THEN 'local_speaker' ELSE 'unknown' END),
                COALESCE(segment.transcript_revision_id = session.current_transcript_revision_id
                    AND session.phase != 'deleting' AND session.transcript_purged_at_utc_ms IS NULL, 0),
                COALESCE((SELECT removed FROM meeting_segment_edits edit
                          WHERE edit.segment_id = segment.segment_id
                          ORDER BY edit_sequence DESC LIMIT 1), 0),
                segment.start_offset_ns
         FROM meeting_transcript_segments segment
         JOIN meeting_transcript_revisions revision
           ON revision.transcript_revision_id = segment.transcript_revision_id
         JOIN meeting_sessions session ON session.id = revision.session_id
         JOIN meeting_source_tracks track ON track.track_id = segment.track_id
         LEFT JOIN meeting_diarization_assignments assignment
           ON assignment.generation_id = session.current_diarization_generation_id
          AND assignment.segment_id = segment.segment_id
         WHERE segment.segment_id = ?1 AND revision.session_id = ?2",
        params![id(segment_id), id(session_id)],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    ).optional()?;
    let (raw_text, speaker, assignment, is_current, removed, start_offset_ns) =
        row.ok_or(StoreError::NotFound)?;
    let speaker_id = SpeakerId::from_uuid(parse_uuid(&speaker)?);
    let canonical = canonical_speaker_ids(connection, session_id)?;
    let speaker_id = canonical.get(&speaker_id).copied().unwrap_or(speaker_id);
    let person_id = if !removed && matches!(assignment.as_str(), "local_speaker" | "system_speaker")
    {
        resolved_person_for_speaker_in(connection, session_id, speaker_id)?
    } else {
        None
    };
    Ok(CorrectionSource {
        speaker_id,
        person_id,
        raw_text,
        is_current,
        identity_confirmed: person_id.is_some()
            && confirmed_person_for_speaker_in(connection, session_id, speaker_id)? == person_id,
        start_offset_ns: from_i64(start_offset_ns)?,
    })
}

fn list_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<TranscriptCorrectionList, StoreError> {
    Ok(TranscriptCorrectionList {
        revision: revision_in(connection)?,
        rules: rules_in(connection, person_id)?,
    })
}

pub(crate) fn audio_retained_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    segment_id: TranscriptSegmentId,
) -> Result<bool, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT EXISTS(
            SELECT 1 FROM meeting_transcript_segments segment
            JOIN meeting_transcript_revisions revision
              ON revision.transcript_revision_id = segment.transcript_revision_id
            JOIN meeting_sessions session ON session.id = revision.session_id
            JOIN meeting_track_records record ON record.track_id = segment.track_id
            WHERE segment.segment_id = ?1 AND session.id = ?2
              AND session.transcript_purged_at_utc_ms IS NULL AND session.phase != 'deleting'
              AND session.audio_purged_at_utc_ms IS NULL
              AND record.start_offset_ns < segment.end_offset_ns
              AND record.start_offset_ns + record.duration_ns > segment.start_offset_ns
        )",
    )?;
    statement
        .query_row(params![id(segment_id), id(session_id)], |row| row.get(0))
        .map_err(Into::into)
}

fn rules_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<Vec<TranscriptCorrection>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT correction_id, source_session_id, source_segment_id, source_speaker_id,
                original_text, corrected_text, original_phrase, replacement_phrase,
                created_at_utc_ms, updated_at_utc_ms, kind_json, context_json, enabled
         FROM person_transcript_corrections WHERE person_id = ?1
         ORDER BY length(original_phrase) DESC, original_phrase, correction_id",
    )?;
    let rows = statement.query_map(params![id(person_id)], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, bool>(12)?,
        ))
    })?;
    let mut rules = Vec::new();
    for row in rows {
        let (
            correction,
            session,
            segment,
            speaker,
            original_text,
            corrected_text,
            original_phrase,
            replacement_phrase,
            created_at_utc_ms,
            updated_at_utc_ms,
            kind_json,
            context_json,
            enabled,
        ) = row?;
        let source_session_id = MeetingSessionId::from_uuid(parse_uuid(&session)?);
        let source_segment_id = TranscriptSegmentId::from_uuid(parse_uuid(&segment)?);
        let source = source_segment_in(connection, source_session_id, source_segment_id)?;
        rules.push(TranscriptCorrection {
            correction_id: parse_uuid(&correction)?,
            person_id,
            source_session_id,
            source_segment_id,
            source_speaker_id: SpeakerId::from_uuid(parse_uuid(&speaker)?),
            source_start_offset_ns: source.start_offset_ns,
            source_audio_retained: audio_retained_in(
                connection,
                source_session_id,
                source_segment_id,
            )?,
            original_text,
            corrected_text,
            original_phrase,
            replacement_phrase,
            kind: serde_json::from_str::<TranscriptCorrectionKind>(&kind_json)
                .map_err(|_| StoreError::Corrupt)?,
            context: serde_json::from_str::<TranscriptCorrectionContext>(&context_json)
                .map_err(|_| StoreError::Corrupt)?,
            enabled,
            status: if !source.is_current
                || source.person_id != Some(person_id)
                || !source.identity_confirmed
                || source.speaker_id != SpeakerId::from_uuid(parse_uuid(&speaker)?)
            {
                TranscriptCorrectionStatus::SourceChanged
            } else if !enabled {
                TranscriptCorrectionStatus::Paused
            } else {
                TranscriptCorrectionStatus::Active
            },
            active: enabled
                && source.is_current
                && source.person_id == Some(person_id)
                && source.identity_confirmed
                && source.speaker_id == SpeakerId::from_uuid(parse_uuid(&speaker)?),
            created_at_utc_ms,
            updated_at_utc_ms,
        });
    }
    let conflicting = rules
        .iter()
        .map(|rule| {
            rule.active
                && rules.iter().any(|other| {
                    other.active
                        && other.correction_id != rule.correction_id
                        && other.original_phrase == rule.original_phrase
                        && other.replacement_phrase != rule.replacement_phrase
                        && scope::overlaps(
                            &rule.context,
                            rule.source_session_id,
                            &other.context,
                            other.source_session_id,
                        )
                })
        })
        .collect::<Vec<_>>();
    for (rule, conflicting) in rules.iter_mut().zip(conflicting) {
        if conflicting {
            rule.active = false;
            rule.status = TranscriptCorrectionStatus::Conflicting;
        }
    }
    Ok(rules)
}

fn require_person_in(connection: &Connection, person_id: PersonId) -> Result<(), StoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM persons WHERE id = ?1)",
        params![id(person_id)],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::PersonNotFound)
    }
}

fn revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT revision FROM transcript_correction_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    from_i64(revision)
}

fn require_revision_in(connection: &Connection, expected: u64) -> Result<(), StoreError> {
    if revision_in(connection)? == expected {
        Ok(())
    } else {
        Err(StoreError::StaleRevision)
    }
}

fn person_sessions_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT session.id FROM meeting_sessions session
         JOIN meeting_speakers speaker ON speaker.session_id = session.id
         LEFT JOIN meeting_voice_speaker_resolutions resolution
           ON resolution.speaker_id = speaker.speaker_id AND resolution.is_current = 1
         LEFT JOIN voice_speaker_matches matched ON matched.speaker_id = speaker.speaker_id
         WHERE session.phase != 'deleting' AND session.current_transcript_revision_id IS NOT NULL
           AND session.transcript_purged_at_utc_ms IS NULL
           AND (resolution.resolved_person_id = ?1 OR matched.person_id = ?1)
         ORDER BY session.id",
    )?;
    let sessions = statement
        .query_map(params![id(person_id)], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sessions)
}

pub(super) fn learned_projection_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<HashMap<TranscriptSegmentId, String>, StoreError> {
    Ok(effective_segments_for_session(connection, session_id)?
        .into_iter()
        .filter(|segment| !segment.removed && segment.edit_revision.is_none())
        .filter_map(|segment| {
            segment
                .replacement_text
                .filter(|text| *text != segment.base.text)
                .map(|text| (segment.base.segment_id, text))
        })
        .collect())
}

fn projection_snapshot_in(
    connection: &Connection,
    sessions: Vec<String>,
) -> Result<TranscriptCorrectionSyncSnapshot, StoreError> {
    let sessions = sessions
        .into_iter()
        .map(|raw| {
            let session_id = MeetingSessionId::from_uuid(parse_uuid(&raw)?);
            Ok((session_id, learned_projection_in(connection, session_id)?))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(TranscriptCorrectionSyncSnapshot { sessions })
}

fn changed_projection_sessions_in(
    connection: &Connection,
    before: TranscriptCorrectionSyncSnapshot,
) -> Result<Vec<String>, StoreError> {
    let mut changed = Vec::new();
    for (session_id, projection) in before.sessions {
        if learned_projection_in(connection, session_id)? != projection {
            changed.push(id(session_id));
        }
    }
    Ok(changed)
}

fn invalidate_changed_projections_in(
    transaction: &Transaction<'_>,
    before: TranscriptCorrectionSyncSnapshot,
) -> Result<Vec<TranscriptCorrectionChangedSession>, StoreError> {
    invalidate_sessions_in(
        transaction,
        changed_projection_sessions_in(transaction, before)?,
    )
}

fn invalidate_sessions_in(
    connection: &Transaction<'_>,
    sessions: Vec<String>,
) -> Result<Vec<TranscriptCorrectionChangedSession>, StoreError> {
    let mut changed = Vec::with_capacity(sessions.len());
    for raw in sessions {
        let session_id = MeetingSessionId::from_uuid(parse_uuid(&raw)?);
        let session = session_row(connection, session_id)?;
        let revision = session.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        connection.execute(
            "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
            params![to_i64(revision)?, raw],
        )?;
        mark_artifacts_out_of_date(connection, session_id)?;
        changed.push(TranscriptCorrectionChangedSession {
            session_id,
            revision,
        });
    }
    Ok(changed)
}

fn validate_phrases(original: &str, replacement: &str) -> Result<(), StoreError> {
    if original.is_empty()
        || replacement.is_empty()
        || original == replacement
        || original.trim() != original
        || replacement.trim() != replacement
        || original.chars().count() > 512
        || replacement.chars().count() > 512
        || original.chars().any(char::is_control)
        || replacement.chars().any(char::is_control)
    {
        Err(StoreError::Invalid)
    } else {
        Ok(())
    }
}

fn word_character(character: char) -> bool {
    let mut bytes = [0; 4];
    WORD_CHARACTER.is_match(character.encode_utf8(&mut bytes))
}

fn phrase_at(text: &str, offset: usize, phrase: &str) -> bool {
    text[offset..].starts_with(phrase)
        && text[..offset]
            .chars()
            .next_back()
            .is_none_or(|character| !word_character(character))
        && text[offset + phrase.len()..]
            .chars()
            .next()
            .is_none_or(|character| !word_character(character))
}

fn replace_phrase(text: &str, original: &str, replacement: &str) -> Option<String> {
    replace_pairs(text, std::iter::once((original, replacement)))
}

fn replace_rules(
    text: &str,
    session_id: MeetingSessionId,
    rules: &[TranscriptCorrection],
) -> Option<String> {
    replace_pairs(
        text,
        rules
            .iter()
            .filter(|rule| rule.active && scope::matches(rule, session_id, text))
            .map(|rule| {
                (
                    rule.original_phrase.as_str(),
                    rule.replacement_phrase.as_str(),
                )
            }),
    )
}

fn replace_pairs<'a>(
    text: &str,
    pairs: impl Iterator<Item = (&'a str, &'a str)> + Clone,
) -> Option<String> {
    let mut output: Option<String> = None;
    let mut copied_through = 0;
    let mut cursor = 0;
    while cursor < text.len() {
        let matched = pairs
            .clone()
            .find(|(original, _)| phrase_at(text, cursor, original));
        if let Some((original, replacement)) = matched {
            let output = output.get_or_insert_with(|| String::with_capacity(text.len()));
            output.push_str(&text[copied_through..cursor]);
            output.push_str(replacement);
            cursor += original.len();
            copied_through = cursor;
        } else if let Some(character) = text[cursor..].chars().next() {
            cursor += character.len_utf8();
        }
    }
    if let Some(output) = &mut output {
        output.push_str(&text[copied_through..]);
    }
    output
}
