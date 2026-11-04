use super::{parse_uuid, rebuild_search_documents_in, StoreError};
use crate::meeting::types::MeetingSessionId;
use rusqlite::Connection;

pub(super) fn refresh_in(connection: &mut Connection) -> Result<(), StoreError> {
    let sessions = {
        let mut statement = connection.prepare(
            "SELECT meeting.id FROM meeting_sessions meeting
             LEFT JOIN meeting_search_projection_state projection ON projection.session_id = meeting.id
             WHERE meeting.phase != 'deleting'
               AND meeting.current_transcript_revision_id IS NOT NULL
               AND (projection.session_id IS NULL OR projection.input_revision != meeting.revision)",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if sessions.is_empty() {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    for session in sessions {
        rebuild_search_documents_in(
            &transaction,
            MeetingSessionId::from_uuid(parse_uuid(&session)?),
        )?;
    }
    transaction.commit()?;
    Ok(())
}
