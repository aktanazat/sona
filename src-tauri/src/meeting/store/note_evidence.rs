use super::{id, parse_uuid, session_row, to_i64, to_sql_error, MeetingStore, StoreError};
use crate::meeting::types::{MeetingPhase, MeetingSessionId};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

pub(crate) struct NoteEvidenceInput<'a> {
    pub session_id: MeetingSessionId,
    pub input_revision: u64,
    pub title: &'a str,
    pub text: &'a str,
}

pub(crate) struct CapturedNoteSource {
    pub session_id: MeetingSessionId,
    pub input_revision: u64,
    pub title: String,
}

pub(crate) struct CapturedNoteEvidence {
    pub source: CapturedNoteSource,
    pub text: String,
}

pub(crate) enum NoteEvidenceRemoval {
    Note(Uuid),
    Source {
        note_id: Uuid,
        session_id: MeetingSessionId,
    },
}

#[derive(Debug)]
pub(crate) enum NoteEvidenceError<E> {
    Store(StoreError),
    Persist(E),
}

impl<E> From<StoreError> for NoteEvidenceError<E> {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl<E> From<rusqlite::Error> for NoteEvidenceError<E> {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

impl MeetingStore {
    /// Captures retained revisions before persisting a note under the history lock.
    /// A failed history write rolls back the captures; a failed final commit never
    /// places conversation evidence in history.
    pub(crate) fn capture_note_evidence<T, E>(
        &self,
        note_id: Uuid,
        evidence: &[NoteEvidenceInput<'_>],
        persist: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, NoteEvidenceError<E>> {
        if evidence.is_empty() {
            return persist().map_err(NoteEvidenceError::Persist);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note_id = note_id.to_string();
        for source in evidence {
            let session = session_row(&transaction, source.session_id)?;
            require_retained_source(session.phase, session.transcript_purged_at_utc_ms)?;
            if session.revision != source.input_revision {
                return Err(StoreError::StaleRevision.into());
            }
            transaction.execute(
                "INSERT INTO meeting_note_evidence
                 (note_id, session_id, input_revision, title, text) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    note_id,
                    id(source.session_id),
                    to_i64(source.input_revision)?,
                    source.title,
                    source.text,
                ],
            )?;
        }
        let result = persist().map_err(NoteEvidenceError::Persist)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn captured_note_sources(
        &self,
        note_id: Uuid,
    ) -> Result<Vec<CapturedNoteSource>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT evidence.session_id, evidence.input_revision, evidence.title
             FROM meeting_note_evidence evidence
             JOIN meeting_sessions session ON session.id = evidence.session_id
             WHERE evidence.note_id = ?1 AND session.phase != 'deleting'
               AND session.transcript_purged_at_utc_ms IS NULL",
        )?;
        let sources = statement.query_map(params![note_id.to_string()], |row| {
            let session_id = parse_uuid(&row.get::<_, String>(0)?)
                .map(MeetingSessionId::from_uuid)
                .map_err(to_sql_error)?;
            let input_revision = u64::try_from(row.get::<_, i64>(1)?)
                .map_err(|_| to_sql_error(StoreError::Corrupt))?;
            Ok(CapturedNoteSource {
                session_id,
                input_revision,
                title: row.get(2)?,
            })
        })?;
        sources.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn captured_note_evidence(
        &self,
        note_id: Uuid,
        session_id: MeetingSessionId,
    ) -> Result<Option<CapturedNoteEvidence>, StoreError> {
        let connection = self.connection()?;
        let session = session_row(&connection, session_id)?;
        require_retained_source(session.phase, session.transcript_purged_at_utc_ms)?;
        let captured = connection
            .query_row(
                "SELECT input_revision, title, text FROM meeting_note_evidence
                 WHERE note_id = ?1 AND session_id = ?2",
                params![note_id.to_string(), id(session_id)],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        captured
            .map(|(revision, title, text)| {
                Ok(CapturedNoteEvidence {
                    source: CapturedNoteSource {
                        session_id,
                        input_revision: u64::try_from(revision).map_err(|_| StoreError::Corrupt)?,
                        title,
                    },
                    text,
                })
            })
            .transpose()
    }

    /// Erases evidence durably before removing its history reference.
    /// A failed history write leaves no evidence behind; meeting stays locked
    /// until that write finishes, so every caller takes meeting before history.
    pub(crate) fn remove_note_evidence<T, E>(
        &self,
        target: NoteEvidenceRemoval,
        persist: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, NoteEvidenceError<E>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match target {
            NoteEvidenceRemoval::Note(note_id) => {
                transaction.execute(
                    "DELETE FROM meeting_note_evidence WHERE note_id = ?1",
                    params![note_id.to_string()],
                )?;
            }
            NoteEvidenceRemoval::Source {
                note_id,
                session_id,
            } => {
                transaction.execute(
                    "DELETE FROM meeting_note_evidence WHERE note_id = ?1 AND session_id = ?2",
                    params![note_id.to_string(), id(session_id)],
                )?;
            }
        }
        transaction.commit()?;
        persist().map_err(NoteEvidenceError::Persist)
    }
}

fn require_retained_source(
    phase: MeetingPhase,
    transcript_purged_at_utc_ms: Option<i64>,
) -> Result<(), StoreError> {
    if phase == MeetingPhase::Deleting {
        return Err(StoreError::NotFound);
    }
    if transcript_purged_at_utc_ms.is_some() {
        return Err(StoreError::TranscriptDeleted);
    }
    Ok(())
}
