CREATE TABLE meeting_search_projection_state (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    input_revision INTEGER NOT NULL CHECK (input_revision >= 0)
);
