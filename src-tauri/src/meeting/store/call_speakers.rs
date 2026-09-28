use super::*;
use crate::meeting::call_speakers::{
    mapping::{match_names, may_name, ActiveSpan, NamedParticipant, SpeechSpan},
    CallNameStatus, CallNameSuggestion, CallNameTarget, CallObservation, CallParticipant,
    CallSample, MAX_CALL_SAMPLES,
};

impl MeetingStore {
    pub(crate) fn begin_call_names(
        &self,
        plan: &MeetingRunPlan,
        target: &CallNameTarget,
        automatically_use: bool,
    ) -> Result<(), StoreError> {
        self.require_retained_transcript(plan.session_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let previous: Option<String> = transaction
            .query_row(
                "SELECT plan_id FROM meeting_call_name_runs WHERE session_id = ?1",
                params![id(plan.session_id)],
                |row| row.get(0),
            )
            .optional()?;
        if previous
            .as_deref()
            .is_some_and(|previous| previous != id(plan.plan_id))
        {
            transaction.execute(
                "DELETE FROM meeting_call_name_runs WHERE session_id = ?1",
                params![id(plan.session_id)],
            )?;
        }
        transaction.execute(
            "INSERT INTO meeting_call_name_runs (session_id, plan_id, target_json, automatically_use, enabled, state, detail)
             VALUES (?1, ?2, ?3, ?4, 1, 'reading', 'Waiting for the selected call.')
             ON CONFLICT(session_id) DO UPDATE SET target_json = excluded.target_json,
             automatically_use = excluded.automatically_use, enabled = 1, state = excluded.state, detail = excluded.detail",
            params![id(plan.session_id), id(plan.plan_id), encode_json(target)?, bool_to_i64(automatically_use)],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn set_call_names_automatic(
        &self,
        session_id: MeetingSessionId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "UPDATE meeting_call_name_runs SET automatically_use = ?2 WHERE session_id = ?1",
            params![id(session_id), bool_to_i64(enabled)],
        )?;
        Ok(())
    }

    pub(crate) fn call_names_state(
        &self,
        session_id: MeetingSessionId,
        state: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "UPDATE meeting_call_name_runs SET state = ?2, detail = ?3 WHERE session_id = ?1",
            params![id(session_id), state, detail],
        )?;
        Ok(())
    }

    pub(crate) fn finish_call_names(
        &self,
        session_id: MeetingSessionId,
        state: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        self.connection()?.execute("UPDATE meeting_call_name_runs SET enabled = 0, state = ?2, detail = ?3 WHERE session_id = ?1",
            params![id(session_id), state, detail])?;
        Ok(())
    }

    /// Updates the end of the same observation rather than appending heartbeats.
    /// A delayed poll starts a fresh interval, leaving the missing time uncovered.
    pub(crate) fn record_call_observation(
        &self,
        session_id: MeetingSessionId,
        offset_ns: u64,
        observation: &CallObservation,
        continuous: bool,
        unchanged: bool,
    ) -> Result<bool, StoreError> {
        if !observation.valid() {
            return Err(StoreError::Invalid);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let row = session_row(&transaction, session_id)?;
        if row.phase != MeetingPhase::CapturingRecording
            || row.transcript_purged_at_utc_ms.is_some()
        {
            return Err(StoreError::Conflict);
        }
        let last: Option<(i64, i64)> = transaction.query_row(
            "SELECT sequence, end_ns FROM meeting_call_name_samples WHERE session_id = ?1 ORDER BY sequence DESC LIMIT 1",
            params![id(session_id)], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let offset = to_i64(offset_ns)?;
        let next = last.map_or(0, |(sequence, _)| sequence + 1);
        if !unchanged && usize::try_from(next).map_err(|_| StoreError::Invalid)? >= MAX_CALL_SAMPLES
        {
            return Ok(false);
        }
        if let Some((sequence, end)) = last {
            if offset <= end {
                return Err(StoreError::Invalid);
            }
            if continuous {
                transaction.execute("UPDATE meeting_call_name_samples SET end_ns = ?3 WHERE session_id = ?1 AND sequence = ?2",
                    params![id(session_id), sequence, offset])?;
            }
        }
        if !unchanged {
            transaction.execute(
                "INSERT INTO meeting_call_name_samples (session_id, sequence, start_ns, end_ns, observation_json) VALUES (?1, ?2, ?3, ?3, ?4)",
                params![id(session_id), next, offset, encode_json(observation)?],
            )?;
        }
        for person in &observation.participants {
            transaction.execute(
                "INSERT INTO meeting_call_participants (session_id, participant_id, display_name, is_local)
                 SELECT ?1, ?2, ?3, ?4 WHERE (SELECT COUNT(*) FROM meeting_call_participants WHERE session_id = ?1) < 512
                    OR EXISTS (SELECT 1 FROM meeting_call_participants WHERE session_id = ?1 AND participant_id = ?2)
                 ON CONFLICT(session_id, participant_id) DO UPDATE SET display_name = excluded.display_name, is_local = excluded.is_local",
                params![id(session_id), person.id, person.name, bool_to_i64(person.is_local)],
            )?;
        }
        transaction.execute(
            "UPDATE meeting_call_name_runs SET state = ?2, detail = ?3 WHERE session_id = ?1",
            params![id(session_id), observation.state, observation.detail],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn call_name_status(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<CallNameStatus, StoreError> {
        self.require_retained_transcript(session_id)?;
        let connection = self.connection()?;
        let mut status = call_status_in(&connection, session_id)?;
        status.roster = call_participants_in(&connection, session_id)?
            .into_iter()
            .filter(|person| !person.is_local)
            .map(|person| person.name)
            .collect();
        status.roster.sort();
        status.roster.dedup();
        let mut statement = connection.prepare(
            "SELECT n.speaker_id, n.display_name, n.overlap_ns, n.speech_ns, s.display_name, s.revision,
                    EXISTS (SELECT 1 FROM voice_speaker_matches v WHERE v.speaker_id = s.speaker_id)
             FROM meeting_call_name_suggestions n JOIN meeting_speakers s ON s.speaker_id = n.speaker_id
             JOIN meeting_sessions m ON m.id = n.session_id
             WHERE n.session_id = ?1 AND n.dismissed = 0 AND s.merged_into_speaker_id IS NULL
               AND n.generation_id = m.current_diarization_generation_id ORDER BY n.speaker_id",
        )?;
        let rows = statement.query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, bool>(6)?,
            ))
        })?;
        for row in rows {
            let (speaker, name, overlap, speech, current, revision, matched) = row?;
            if may_name(&current, from_i64(revision)?, matched) {
                status.suggestions.push(CallNameSuggestion {
                    speaker_id: SpeakerId::from_uuid(parse_uuid(&speaker)?),
                    display_name: name,
                    overlap_ns: from_i64(overlap)?,
                    speech_ns: from_i64(speech)?,
                });
            }
        }
        // An interrupted capture cannot truthfully still be 'reading'. Recovery
        // is owned by the session manager; this projection does not restart AX.
        let phase = session_row(&connection, session_id)?.phase;
        if status.enabled && phase.capture_mode().is_none() {
            status.enabled = false;
            status.state = "stopped".into();
            status.detail =
                "Reading ended with the recording. Earlier observed names are kept.".into();
        }
        Ok(status)
    }

    pub fn dismiss_call_name(
        &self,
        session_id: MeetingSessionId,
        speaker_id: SpeakerId,
    ) -> Result<(), StoreError> {
        self.require_retained_transcript(session_id)?;
        self.connection()?.execute("UPDATE meeting_call_name_suggestions SET dismissed = 1 WHERE session_id = ?1 AND speaker_id = ?2",
            params![id(session_id), id(speaker_id)])?;
        Ok(())
    }

    /// Runs after voice matching, inside one transaction. The same precedence
    /// gate protects both suggestions and automatic use; neither renames a
    /// local voice, a manually edited label, a merged speaker, or a voice match.
    pub(crate) fn apply_call_names(&self, session_id: MeetingSessionId) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = call_status_in(&transaction, session_id)?;
        if state.target.is_none() {
            return Ok(());
        }
        let generation: Option<String> = transaction.query_row(
            "SELECT current_diarization_generation_id FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )?;
        let Some(generation) = generation else {
            return Ok(());
        };
        let speakers = speakers_for_session(&transaction, session_id)?;
        let segments = effective_segments_for_session(&transaction, session_id)?;
        let samples = call_samples_in(&transaction, session_id)?;
        let roster = call_participants_in(&transaction, session_id)?;
        let mut voice_statement = transaction
            .prepare("SELECT speaker_id FROM voice_speaker_matches WHERE session_id = ?1")?;
        let matched: HashSet<String> = voice_statement
            .query_map(params![id(session_id)], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        drop(voice_statement);
        let eligible: Vec<_> = speakers
            .iter()
            .map(|speaker| {
                speaker.source_kind == SourceKind::SystemAudio
                    && may_name(
                        &speaker.display_name,
                        speaker.revision,
                        matched.contains(&id(speaker.speaker_id)),
                    )
            })
            .collect();
        let speaker_indices: HashMap<_, _> = speakers
            .iter()
            .enumerate()
            .map(|(index, speaker)| (speaker.speaker_id, index))
            .collect();
        let speech: Vec<_> = segments
            .iter()
            .filter(|segment| {
                !segment.removed
                    && segment.speaker_assignment == SpeakerAssignmentKind::SystemSpeaker
            })
            .filter_map(|segment| {
                Some(SpeechSpan {
                    speaker: *speaker_indices.get(&segment.assigned_speaker_id)?,
                    start_ns: segment.base.start_offset_ns,
                    end_ns: segment.base.end_offset_ns,
                })
            })
            .collect();
        let active: Vec<_> = samples
            .iter()
            .filter_map(|sample| {
                let observation = &sample.observation;
                if observation.state != "reading" || observation.active_ids.len() != 1 {
                    return None;
                }
                let participant = &observation.active_ids[0];
                if !observation
                    .participants
                    .iter()
                    .any(|person| person.id == *participant && !person.is_local)
                {
                    return None;
                }
                Some(ActiveSpan {
                    participant,
                    start_ns: sample.start_ns,
                    end_ns: sample.end_ns,
                })
            })
            .collect();
        let names: Vec<_> = roster
            .iter()
            .filter(|person| !person.is_local)
            .map(|person| NamedParticipant {
                id: &person.id,
                name: &person.name,
            })
            .collect();
        let suggestions = match_names(&speech, &active, &names, &eligible);
        // A new diarization replaces old suggestions. A dismissed name in the
        // same generation stays dismissed if processing revisits this stage.
        transaction.execute("DELETE FROM meeting_call_name_suggestions WHERE session_id = ?1 AND generation_id != ?2",
            params![id(session_id), generation])?;
        let mut renamed = false;
        let mut linked = false;
        for suggestion in suggestions {
            let speaker = &speakers[suggestion.speaker];
            transaction.execute(
                "INSERT INTO meeting_call_name_suggestions (session_id, speaker_id, generation_id, display_name, overlap_ns, speech_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(session_id, speaker_id) DO UPDATE SET
                 display_name = excluded.display_name, overlap_ns = excluded.overlap_ns, speech_ns = excluded.speech_ns",
                params![id(session_id), id(speaker.speaker_id), generation, suggestion.name, to_i64(suggestion.overlap_ns)?, to_i64(suggestion.speech_ns)?],
            )?;
            let dismissed: bool = transaction.query_row("SELECT dismissed FROM meeting_call_name_suggestions WHERE session_id = ?1 AND speaker_id = ?2",
                params![id(session_id), id(speaker.speaker_id)], |row| row.get(0))?;
            if state.automatically_use && !dismissed {
                let changed = transaction.execute(
                    "UPDATE meeting_speakers SET display_name = ?3, revision = revision + 1
                     WHERE session_id = ?1 AND speaker_id = ?2 AND revision = 0 AND merged_into_speaker_id IS NULL",
                    params![id(session_id), id(speaker.speaker_id), suggestion.name],
                )?;
                if changed == 1 {
                    renamed = true;
                    linked |= people::derive_speaker_link_in(
                        &transaction,
                        session_id,
                        suggestion.name,
                        utc_now_ms(),
                    )? > 0;
                }
            }
        }
        if renamed {
            mark_artifacts_out_of_date(&transaction, session_id)?;
            rebuild_search_documents_in(&transaction, session_id)?;
            let row = session_row(&transaction, session_id)?;
            let revision = row.revision.checked_add(1).ok_or(StoreError::Invalid)?;
            transaction.execute(
                "UPDATE meeting_sessions SET revision = ?2 WHERE id = ?1",
                params![id(session_id), to_i64(revision)?],
            )?;
            append_event(
                &transaction,
                session_id,
                revision,
                row.phase,
                row.phase,
                "speakers_named_from_call",
                None,
            )?;
        }
        if linked {
            people::recompute_organizations_in(&transaction)?;
            people::bump_people_revision_in(&transaction)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn call_status_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<CallNameStatus, StoreError> {
    let row: Option<(String, bool, bool, String, String)> = connection.query_row(
        "SELECT target_json, automatically_use, enabled, state, detail FROM meeting_call_name_runs WHERE session_id = ?1",
        params![id(session_id)], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).optional()?;
    let Some((target, automatically_use, enabled, state, detail)) = row else {
        return Ok(CallNameStatus::default());
    };
    Ok(CallNameStatus {
        target: Some(decode_json(&target)?),
        automatically_use,
        enabled,
        state,
        detail,
        roster: Vec::new(),
        suggestions: Vec::new(),
    })
}

fn call_participants_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<CallParticipant>, StoreError> {
    let mut statement = connection.prepare("SELECT participant_id, display_name, is_local FROM meeting_call_participants WHERE session_id = ?1 ORDER BY participant_id")?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        Ok(CallParticipant {
            id: row.get(0)?,
            name: row.get(1)?,
            is_local: row.get(2)?,
        })
    })?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

fn call_samples_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<CallSample>, StoreError> {
    let mut statement = connection.prepare("SELECT start_ns, end_ns, observation_json FROM meeting_call_name_samples WHERE session_id = ?1 ORDER BY sequence")?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (start, end, observation) = row?;
        Ok(CallSample {
            start_ns: from_i64(start)?,
            end_ns: from_i64(end)?,
            observation: decode_json(&observation)?,
        })
    })
    .collect()
}
