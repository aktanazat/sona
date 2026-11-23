use super::{
    append_event, artifact_revision_from_row, encode_json, id, loops, session_row, to_i64,
    utc_now_ms, MeetingStore, ReceiptSpeakers, StoreError,
};
use crate::meeting::note_edits::{self, NoteEditApply, NoteEditSource};
use crate::meeting::types::{
    GeneratedMeetingArtifacts, MeetingArtifactId, MeetingArtifactRevision, MeetingPhase,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

fn edit_source(
    connection: &Connection,
    source: NoteEditSource,
) -> Result<(MeetingPhase, MeetingArtifactRevision), StoreError> {
    let session = session_row(connection, source.session_id)?;
    if session.revision != source.expected_revision {
        return Err(StoreError::StaleRevision);
    }
    if !matches!(
        session.phase,
        MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
    ) {
        return Err(StoreError::Conflict);
    }
    let artifact = connection
        .query_row(
            "SELECT a.artifact_id, a.transcript_revision_id, a.input_revision, a.template_id,
                a.template_version, a.generation_key, a.state, a.content_json,
                a.generated_at_utc_ms, r.wall_ms, r.audio_ns, r.route
         FROM meeting_artifact_revisions a
         LEFT JOIN meeting_processing_receipts r ON r.artifact_id = a.artifact_id
         WHERE a.session_id = ?1 AND a.state = 'current' AND a.content_json IS NOT NULL
         ORDER BY a.generated_at_utc_ms DESC, a.rowid DESC LIMIT 1",
            params![id(source.session_id)],
            |row| artifact_revision_from_row(row, source.session_id),
        )
        .optional()?;
    artifact
        .filter(|artifact| artifact.artifact_id == source.artifact_id)
        .map(|artifact| (session.phase, artifact))
        .ok_or(StoreError::StaleRevision)
}

impl MeetingStore {
    pub(crate) fn finished_note_for_edit(
        &self,
        source: NoteEditSource,
    ) -> Result<GeneratedMeetingArtifacts, StoreError> {
        let user_name = self.user_name();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut notes = edit_source(&transaction, source)?
            .1
            .content
            .ok_or(StoreError::Corrupt)?;
        if notes
            .action_items
            .iter()
            .any(|item| item.owner_text.is_some() && !item.owner_edited)
        {
            let labels = loops::user_speaker_labels_in(
                &transaction,
                source.session_id,
                user_name.as_deref(),
            )?;
            ReceiptSpeakers::read(&transaction, source.session_id)?.name_owners(
                &mut notes.action_items,
                &labels,
                user_name.as_deref().unwrap_or("Me"),
            );
        }
        Ok(notes)
    }

    pub(crate) fn save_finished_note_edit(&self, edit: NoteEditApply) -> Result<(), StoreError> {
        let source = edit.source;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (phase, artifact) = edit_source(&transaction, source)?;
        let mut content = artifact.content.ok_or(StoreError::Corrupt)?;
        note_edits::apply(&mut content, edit.changes).map_err(|_| StoreError::Invalid)?;
        let revision = source
            .expected_revision
            .checked_add(1)
            .ok_or(StoreError::Corrupt)?;
        let artifact_id = MeetingArtifactId::new();
        let newest = artifact
            .generated_at_utc_ms
            .checked_add(1)
            .ok_or(StoreError::Corrupt)?;
        let now = utc_now_ms();
        transaction.execute(
            "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
            params![to_i64(revision)?, id(source.session_id)],
        )?;
        transaction.execute(
            "INSERT INTO meeting_artifact_revisions (
                artifact_id, session_id, transcript_revision_id, input_revision, template_id,
                template_version, generation_key, state, content_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'current', ?8, ?9)",
            params![
                id(artifact_id),
                id(source.session_id),
                id(artifact.transcript_revision_id),
                to_i64(revision)?,
                artifact.template_id,
                i64::from(artifact.template_version),
                format!("note-edit:{}", artifact_id.uuid()),
                encode_json(&content)?,
                newest.max(now),
            ],
        )?;
        transaction.execute(
            "INSERT INTO meeting_action_item_states (artifact_id, action_index, session_id, done, updated_at_utc_ms)
             SELECT ?1, action_index, session_id, done, updated_at_utc_ms
               FROM meeting_action_item_states WHERE artifact_id = ?2 AND session_id = ?3",
            params![id(artifact_id), id(source.artifact_id), id(source.session_id)],
        )?;
        append_event(
            &transaction,
            source.session_id,
            revision,
            phase,
            phase,
            "finished_notes_edited",
            None,
        )?;
        transaction.commit()?;
        Ok(())
    }
}
