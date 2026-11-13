//! Who changed a meeting's live doc and board. One row per meeting, so the
//! marks on points and cards and the timeline outlive the call.

use super::{decode_json, encode_json, id, utc_now_ms, MeetingStore, StoreError};
use crate::meeting::live_doc_types::MeetingLiveChange;
use crate::meeting::types::MeetingSessionId;
use rusqlite::{params, OptionalExtension};

impl MeetingStore {
    /// The saved changes of a meeting, oldest first; empty when it has none.
    pub(crate) fn live_changes(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingLiveChange>, StoreError> {
        let saved: Option<String> = self
            .connection()?
            .query_row(
                "SELECT changes_json FROM meeting_live_changes WHERE session_id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()?;
        saved.map_or_else(|| Ok(Vec::new()), |json| decode_json(&json))
    }

    /// Keep `changes` as the meeting's changes.
    pub(crate) fn save_live_changes(
        &self,
        session_id: MeetingSessionId,
        changes: &[MeetingLiveChange],
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO meeting_live_changes (session_id, changes_json, updated_at_utc_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE
                SET changes_json = excluded.changes_json,
                    updated_at_utc_ms = excluded.updated_at_utc_ms",
            params![id(session_id), encode_json(changes)?, utc_now_ms()],
        )?;
        Ok(())
    }
}
