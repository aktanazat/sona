-- Who changed a meeting's live doc and board, when, and how far into the
-- call: Sona's readings, the user's edits and teammates' edits from the
-- meeting's link. Kept so the marks outlive the call.
CREATE TABLE meeting_live_changes (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    changes_json TEXT NOT NULL DEFAULT '[]',
    updated_at_utc_ms INTEGER NOT NULL
);
