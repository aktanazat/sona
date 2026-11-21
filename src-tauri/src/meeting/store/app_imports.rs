//! The meetings imported from other apps, by each app's own id for them.

use super::{id, MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use rusqlite::params;

impl MeetingStore {
    /// Whether an import from `source` already made a meeting from the
    /// export's `source_key` that is still kept.
    pub(crate) fn app_import_exists(
        &self,
        source: &str,
        source_key: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection()?.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM meeting_app_imports imported
                JOIN meeting_sessions session ON session.id = imported.session_id
                WHERE imported.source = ?1 AND imported.source_key = ?2
                  AND session.phase != 'deleting'
             )",
            params![source, source_key],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn remember_app_import(
        &self,
        source: &str,
        source_key: &str,
        session_id: MeetingSessionId,
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO meeting_app_imports (source, source_key, session_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source, source_key) DO UPDATE SET session_id = excluded.session_id",
            params![source, source_key, id(session_id)],
        )?;
        Ok(())
    }
}
