-- The doc a meeting built while it recorded, asked for during the call.
-- Its points are summaries in the meeting's own words, not quotes, so they
-- stay when the transcript goes, like the meeting's notes.
CREATE TABLE meeting_live_docs (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    points_json TEXT NOT NULL DEFAULT '[]',
    updated_at_utc_ms INTEGER NOT NULL
);
