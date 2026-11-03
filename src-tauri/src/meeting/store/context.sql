CREATE TABLE conversation_context (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    kind_json TEXT NOT NULL,
    location_json TEXT,
    calendar_name TEXT,
    calendar_location TEXT
);
