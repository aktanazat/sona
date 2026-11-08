CREATE TABLE meeting_open_windows (
    item_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    item_key TEXT NOT NULL,
    app_name TEXT,
    bundle_id TEXT,
    title TEXT,
    document_path TEXT,
    url TEXT,
    host TEXT,
    first_offset_ns INTEGER NOT NULL CHECK (first_offset_ns >= 0),
    last_offset_ns INTEGER NOT NULL CHECK (last_offset_ns >= first_offset_ns),
    frontmost_ns INTEGER NOT NULL CHECK (frontmost_ns >= 0),
    created_at_utc_ms INTEGER NOT NULL,
    attached_at_utc_ms INTEGER,
    UNIQUE (session_id, item_key)
);
CREATE INDEX meeting_open_windows_offered_idx
    ON meeting_open_windows(created_at_utc_ms) WHERE attached_at_utc_ms IS NULL;
