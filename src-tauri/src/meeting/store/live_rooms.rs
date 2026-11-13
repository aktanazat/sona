//! The rooms meetings' links opened on the companion. A room is kept past
//! its meeting's delete until it has been told, so the delete empties it
//! even when it comes in a later launch than the one that made the room.

use super::{id, parse_uuid, MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use rusqlite::params;
use zeroize::Zeroizing;

/// A meeting's room on the companion.
pub(crate) struct LiveRoom {
    pub(crate) id: String,
    /// Where the Mac feeds the room, with the token only the Mac holds.
    pub(crate) socket: String,
    /// The key the room's frames are sealed with.
    pub(crate) frame_key: Zeroizing<[u8; 32]>,
    pub(crate) expires_at_utc_ms: i64,
}

/// A room the store keeps, and whether its meeting was deleted since.
pub(crate) struct KeptLiveRoom {
    pub(crate) session_id: MeetingSessionId,
    pub(crate) room: LiveRoom,
    pub(crate) deleted: bool,
}

impl MeetingStore {
    /// Keep `room` as the meeting's room until it is told the meeting was
    /// deleted or its time is up.
    pub(crate) fn keep_live_room(
        &self,
        session_id: MeetingSessionId,
        room: &LiveRoom,
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO meeting_live_rooms
                (session_id, room_id, socket, frame_key, expires_at_utc_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE
                SET room_id = excluded.room_id,
                    socket = excluded.socket,
                    frame_key = excluded.frame_key,
                    expires_at_utc_ms = excluded.expires_at_utc_ms",
            params![
                id(session_id),
                room.id,
                room.socket,
                room.frame_key.as_slice(),
                room.expires_at_utc_ms
            ],
        )?;
        Ok(())
    }

    /// The rooms kept for meetings, once those whose time was up at
    /// `now_utc_ms` are let go.
    pub(crate) fn kept_live_rooms(&self, now_utc_ms: i64) -> Result<Vec<KeptLiveRoom>, StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM meeting_live_rooms WHERE expires_at_utc_ms <= ?1",
            params![now_utc_ms],
        )?;
        let mut statement = connection.prepare(
            "SELECT room.session_id, room.room_id, room.socket, room.frame_key,
                    room.expires_at_utc_ms,
                    NOT EXISTS (SELECT 1 FROM meeting_sessions session
                                 WHERE session.id = room.session_id
                                   AND session.phase != 'deleting')
             FROM meeting_live_rooms room",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    Zeroizing::new(row.get::<_, Vec<u8>>(3)?),
                    row.get::<_, i64>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(session_id, room_id, socket, frame_key, expires_at_utc_ms, deleted)| {
                Ok(KeptLiveRoom {
                    session_id: MeetingSessionId::from_uuid(parse_uuid(&session_id)?),
                    room: LiveRoom {
                        id: room_id,
                        socket,
                        frame_key: Zeroizing::new(
                            <[u8; 32]>::try_from(frame_key.as_slice())
                                .map_err(|_| StoreError::Corrupt)?,
                        ),
                        expires_at_utc_ms,
                    },
                    deleted,
                })
            })
            .collect()
    }

    /// Let go of a room that knows its meeting was deleted.
    pub(crate) fn forget_live_room(&self, room_id: &str) -> Result<(), StoreError> {
        self.connection()?.execute(
            "DELETE FROM meeting_live_rooms WHERE room_id = ?1",
            params![room_id],
        )?;
        Ok(())
    }
}
