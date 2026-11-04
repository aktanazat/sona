use super::transcript_corrections::{audio_retained_in, confirmed_person_for_speaker_in};
use super::voice_identity::is_generated_speaker_label;
use super::{
    canonical_speaker_ids, from_i64, id, meeting_fts_match_query, parse_uuid, search_projection,
    speaker_assignment_from_db, speakers_for_session, to_sql_error, MeetingStore, StoreError,
};
use crate::meeting::types::{
    MeetingSessionId, SaidMoment, SaidPerson, SpeakerAssignmentKind, SpeakerId, TranscriptSegmentId,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::hash_map::Entry;
use std::collections::HashMap;

const SAID_MOMENTS_MAX: usize = 8;

struct MatchedLine {
    session_id: MeetingSessionId,
    segment_id: TranscriptSegmentId,
    text: String,
    meeting_title: String,
    meeting_started_at_utc_ms: i64,
    start_offset_ns: u64,
    end_offset_ns: u64,
    speaker_id: SpeakerId,
    assignment: SpeakerAssignmentKind,
}

#[derive(Clone, Default)]
struct Voice {
    label: Option<String>,
    person: Option<SaidPerson>,
}

struct SessionVoices {
    canonical: HashMap<SpeakerId, SpeakerId>,
    labels: HashMap<SpeakerId, String>,
    voices: HashMap<SpeakerId, Voice>,
}

impl MeetingStore {
    /// The transcript lines a search matches, best match first, each with
    /// who said it and whether its audio is still on this Mac.
    ///
    /// A person is named only from a confirmed identification. Any other
    /// label is shown only when the store generated it ("Speaker 2"), because
    /// automatic voice matching writes names into labels and nothing records
    /// that once the match is gone.
    pub(crate) fn said_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SaidMoment>, StoreError> {
        if query.trim().chars().count() < 2 {
            return Ok(Vec::new());
        }
        let Some(match_query) = meeting_fts_match_query(query) else {
            return Ok(Vec::new());
        };
        let mut connection = self.connection()?;
        search_projection::refresh_in(&mut connection)?;
        let lines = matched_lines_in(&connection, &match_query, limit.clamp(1, SAID_MOMENTS_MAX))?;
        let mut sessions = HashMap::new();
        let mut moments = Vec::with_capacity(lines.len());
        for line in lines {
            let voices = match sessions.entry(line.session_id) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    entry.insert(SessionVoices::read_in(&connection, line.session_id)?)
                }
            };
            let voice = voices.voice_in(
                &connection,
                line.session_id,
                line.speaker_id,
                line.assignment,
            )?;
            let audio_retained = audio_retained_in(&connection, line.session_id, line.segment_id)?;
            moments.push(SaidMoment {
                session_id: line.session_id,
                segment_id: line.segment_id,
                meeting_title: line.meeting_title,
                meeting_started_at_utc_ms: line.meeting_started_at_utc_ms,
                start_offset_ns: line.start_offset_ns,
                end_offset_ns: line.end_offset_ns,
                text: line.text,
                speaker_label: voice.label,
                person: voice.person,
                audio_retained,
            });
        }
        Ok(moments)
    }
}

fn matched_lines_in(
    connection: &Connection,
    match_query: &str,
    limit: usize,
) -> Result<Vec<MatchedLine>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT d.session_id, d.entity_id, snippet(meeting_search_fts, 0, '', '', '…', 24),
                m.title, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms),
                s.start_offset_ns, s.end_offset_ns,
                COALESCE(a.speaker_id, s.speaker_id),
                COALESCE(a.assignment_kind,
                    CASE t.source_kind WHEN 'microphone' THEN 'local_speaker' ELSE 'unknown' END)
           FROM meeting_search_fts
           JOIN meeting_search_documents d ON d.id = meeting_search_fts.rowid
           JOIN meeting_sessions m ON m.id = d.session_id
           JOIN meeting_transcript_segments s
             ON s.segment_id = d.entity_id
            AND s.transcript_revision_id = m.current_transcript_revision_id
           JOIN meeting_source_tracks t ON t.track_id = s.track_id
           LEFT JOIN meeting_diarization_assignments a
             ON a.generation_id = m.current_diarization_generation_id
            AND a.segment_id = s.segment_id
          WHERE meeting_search_fts MATCH ?1
            AND d.entity_kind = 'segment'
            AND m.phase != 'deleting'
          ORDER BY bm25(meeting_search_fts), m.created_at_utc_ms DESC
          LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![
            match_query,
            i64::try_from(limit).map_err(|_| StoreError::Invalid)?
        ],
        |row| {
            Ok(MatchedLine {
                session_id: MeetingSessionId::from_uuid(
                    parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
                ),
                segment_id: TranscriptSegmentId::from_uuid(
                    parse_uuid(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
                ),
                text: row.get(2)?,
                meeting_title: row.get(3)?,
                meeting_started_at_utc_ms: row.get(4)?,
                start_offset_ns: from_i64(row.get(5)?).map_err(to_sql_error)?,
                end_offset_ns: from_i64(row.get(6)?).map_err(to_sql_error)?,
                speaker_id: SpeakerId::from_uuid(
                    parse_uuid(&row.get::<_, String>(7)?).map_err(to_sql_error)?,
                ),
                assignment: speaker_assignment_from_db(&row.get::<_, String>(8)?)
                    .map_err(to_sql_error)?,
            })
        },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

impl SessionVoices {
    fn read_in(connection: &Connection, session_id: MeetingSessionId) -> Result<Self, StoreError> {
        Ok(Self {
            canonical: canonical_speaker_ids(connection, session_id)?,
            labels: speakers_for_session(connection, session_id)?
                .into_iter()
                .map(|speaker| (speaker.speaker_id, speaker.display_name))
                .collect(),
            voices: HashMap::new(),
        })
    }

    fn voice_in(
        &mut self,
        connection: &Connection,
        session_id: MeetingSessionId,
        speaker_id: SpeakerId,
        assignment: SpeakerAssignmentKind,
    ) -> Result<Voice, StoreError> {
        if matches!(
            assignment,
            SpeakerAssignmentKind::Unknown | SpeakerAssignmentKind::Overlap
        ) {
            return Ok(Voice::default());
        }
        let speaker_id = self
            .canonical
            .get(&speaker_id)
            .copied()
            .unwrap_or(speaker_id);
        if let Some(voice) = self.voices.get(&speaker_id) {
            return Ok(voice.clone());
        }
        let person = match confirmed_person_for_speaker_in(connection, session_id, speaker_id)? {
            Some(person_id) => connection
                .query_row(
                    "SELECT display_name FROM persons WHERE id = ?1",
                    params![id(person_id)],
                    |row| row.get(0),
                )
                .optional()?
                .map(|display_name| SaidPerson {
                    person_id,
                    display_name,
                }),
            None => None,
        };
        let label = self
            .labels
            .get(&speaker_id)
            .filter(|label| person.is_some() || is_generated_speaker_label(label))
            .cloned();
        let voice = Voice { label, person };
        self.voices.insert(speaker_id, voice.clone());
        Ok(voice)
    }
}

#[cfg(test)]
mod tests {
    use super::super::workflow_core_tests::{person, store};
    use super::*;
    use uuid::Uuid;

    /// A finished meeting with one microphone track holding `lines`, each
    /// said by its own speaker under the given label.
    fn meeting_with_lines(
        store: &MeetingStore,
        lines: &[(SpeakerId, &str, &str)],
    ) -> Result<MeetingSessionId, StoreError> {
        let session_id = MeetingSessionId::new();
        let plan_id = Uuid::new_v4().to_string();
        let track_id = Uuid::new_v4().to_string();
        let revision_id = Uuid::new_v4().to_string();
        let connection = store.connection()?;
        connection.execute(
            "INSERT INTO meeting_sessions (
                id, phase, revision, title, origin_kind, preflight_json, created_at_utc_ms,
                started_at_utc_ms, processing_status, retention_policy_json
             ) VALUES (?1, 'review_ready', 0, 'Budget sync', '\"manual\"', '{}', 1, 1,
                       '{\"kind\":\"succeeded\"}', '{\"kind\":\"keep_forever\"}')",
            params![id(session_id)],
        )?;
        connection.execute(
            "INSERT INTO meeting_run_plans (
                plan_id, session_id, attempt_number, schema_version, consent_id,
                canonical_plan_json, created_at_utc_ms
             ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
            params![plan_id, id(session_id), Uuid::new_v4().to_string()],
        )?;
        connection.execute(
            "INSERT INTO meeting_source_tracks (
                track_id, session_id, plan_id, source_kind, required, requested,
                descriptor_json, timestamp_bridge_json, health
             ) VALUES (?1, ?2, ?3, 'microphone', 1, 1, '{}', '{}', '\"healthy\"')",
            params![track_id, id(session_id), plan_id],
        )?;
        connection.execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
            params![revision_id, id(session_id)],
        )?;
        for (ordinal, (speaker_id, label, text)) in lines.iter().enumerate() {
            let ordinal = i64::try_from(ordinal).map_err(|_| StoreError::Invalid)?;
            connection.execute(
                "INSERT INTO meeting_speakers (speaker_id, session_id, source_kind, display_name, revision)
                 VALUES (?1, ?2, 'microphone', ?3, 0)",
                params![id(*speaker_id), id(session_id), label],
            )?;
            connection.execute(
                "INSERT INTO meeting_transcript_segments (
                    segment_id, transcript_revision_id, track_id, ordinal,
                    start_offset_ns, end_offset_ns, speaker_id, base_text
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    revision_id,
                    track_id,
                    ordinal,
                    ordinal * 2_000_000_000,
                    ordinal * 2_000_000_000 + 1_500_000_000,
                    id(*speaker_id),
                    text
                ],
            )?;
        }
        connection.execute(
            "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
            params![revision_id, id(session_id)],
        )?;
        Ok(session_id)
    }

    #[test]
    fn said_lines_name_only_confirmed_speakers_and_generated_labels() -> Result<(), StoreError> {
        let (_directory, store) = store();
        let confirmed = SpeakerId::new();
        let guessed = SpeakerId::new();
        let unnamed = SpeakerId::new();
        let noah = person(&store, "Noah Reyes", &[], &[]);
        let session_id = meeting_with_lines(
            &store,
            &[
                (confirmed, "Noah", "I will confirm the budget by Friday."),
                (guessed, "Ada Lovelace", "The budget stays on this Mac."),
                (unnamed, "Speaker 3", "The budget needs one more pass."),
            ],
        )?;
        store.connection()?.execute(
            "INSERT INTO meeting_voice_speaker_resolutions (
                operation_id, speaker_id, resolution_kind, resolved_person_id, is_current
             ) VALUES (?1, ?2, 'identified', ?3, 1)",
            params![Uuid::new_v4().to_string(), id(confirmed), id(noah)],
        )?;

        let moments = store.said_search("budget", 4)?;

        assert_eq!(moments.len(), 3);
        let said_by_noah = moments
            .iter()
            .find(|moment| moment.text.contains("Friday"))
            .ok_or(StoreError::Corrupt)?;
        assert_eq!(said_by_noah.session_id, session_id);
        assert_eq!(said_by_noah.meeting_title, "Budget sync");
        assert_eq!(
            said_by_noah.person,
            Some(SaidPerson {
                person_id: noah,
                display_name: "Noah Reyes".to_owned()
            })
        );
        assert_eq!(said_by_noah.speaker_label.as_deref(), Some("Noah"));
        let said_by_guess = moments
            .iter()
            .find(|moment| moment.text.contains("Mac"))
            .ok_or(StoreError::Corrupt)?;
        assert_eq!(said_by_guess.person, None);
        assert_eq!(said_by_guess.speaker_label, None);
        assert!(!said_by_guess.audio_retained);
        let said_by_unnamed = moments
            .iter()
            .find(|moment| moment.text.contains("pass"))
            .ok_or(StoreError::Corrupt)?;
        assert_eq!(said_by_unnamed.speaker_label.as_deref(), Some("Speaker 3"));
        Ok(())
    }
}
