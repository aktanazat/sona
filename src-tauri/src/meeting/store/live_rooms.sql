-- The room a meeting's link opened on the companion, and what the Mac needs
-- to empty it. A row outlives its meeting's delete until the room is told,
-- so a delete after a restart, or one the room missed, still empties it.
CREATE TABLE meeting_live_rooms (
    session_id TEXT PRIMARY KEY NOT NULL,
    room_id TEXT NOT NULL,
    socket TEXT NOT NULL,
    frame_key BLOB NOT NULL CHECK (length(frame_key) = 32),
    expires_at_utc_ms INTEGER NOT NULL
);
