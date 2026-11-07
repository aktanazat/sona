CREATE TABLE evening_lines (
    local_day TEXT PRIMARY KEY NOT NULL,
    lines_json TEXT NOT NULL,
    meeting_ids_json TEXT NOT NULL,
    dictation_ids_json TEXT NOT NULL,
    model_id TEXT NOT NULL,
    written_at_utc_ms INTEGER NOT NULL
);
CREATE TABLE filing_reviews (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    reviewed_at_utc_ms INTEGER NOT NULL
);
CREATE TABLE filing_suggestions (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('event', 'person', 'project')),
    target_key TEXT NOT NULL,
    target_json TEXT NOT NULL,
    reasons_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'declined')),
    created_at_utc_ms INTEGER NOT NULL,
    decided_at_utc_ms INTEGER,
    UNIQUE (session_id, target_kind, target_key)
);
CREATE INDEX filing_suggestions_pending ON filing_suggestions(state, session_id);
