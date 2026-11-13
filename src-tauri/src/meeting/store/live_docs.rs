//! The doc a meeting built while it recorded. One row per meeting, written
//! whenever the doc changes, so the meeting's page and chat have it after
//! the stop and an app that quits mid-call keeps what it had.

use super::{decode_json, encode_json, id, utc_now_ms, MeetingStore, StoreError};
use crate::meeting::live_doc_types::MeetingLiveDocPoint;
use crate::meeting::types::MeetingSessionId;
use rusqlite::{params, OptionalExtension};

impl MeetingStore {
    /// The saved points of a meeting's doc; empty when it has none.
    pub(crate) fn live_doc_points(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingLiveDocPoint>, StoreError> {
        let saved: Option<String> = self
            .connection()?
            .query_row(
                "SELECT points_json FROM meeting_live_docs WHERE session_id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()?;
        saved.map_or_else(|| Ok(Vec::new()), |json| decode_json(&json))
    }

    /// Keep `points` as the meeting's doc; no points leaves no row.
    pub(crate) fn save_live_doc_points(
        &self,
        session_id: MeetingSessionId,
        points: &[MeetingLiveDocPoint],
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        if points.is_empty() {
            connection.execute(
                "DELETE FROM meeting_live_docs WHERE session_id = ?1",
                params![id(session_id)],
            )?;
            return Ok(());
        }
        connection.execute(
            "INSERT INTO meeting_live_docs (session_id, points_json, updated_at_utc_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE
                SET points_json = excluded.points_json,
                    updated_at_utc_ms = excluded.updated_at_utc_ms",
            params![id(session_id), encode_json(points)?, utc_now_ms()],
        )?;
        Ok(())
    }
}
