CREATE TABLE conversation_bookmarks (
    bookmark_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    start_offset_ns INTEGER NOT NULL CHECK (start_offset_ns >= 0),
    label TEXT,
    created_at_utc_ms INTEGER NOT NULL
);
CREATE INDEX conversation_bookmarks_session_offset
    ON conversation_bookmarks(session_id, start_offset_ns);
