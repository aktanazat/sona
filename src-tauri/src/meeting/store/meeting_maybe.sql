CREATE TABLE filing_suggestions_rebuilt (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    target_kind TEXT NOT NULL
        CHECK (target_kind IN ('event', 'person', 'project', 'kind', 'place', 'introducer', 'meeting')),
    target_key TEXT NOT NULL,
    target_json TEXT NOT NULL,
    reasons_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'declined')),
    created_at_utc_ms INTEGER NOT NULL,
    decided_at_utc_ms INTEGER,
    UNIQUE (session_id, target_kind, target_key)
);
INSERT INTO filing_suggestions_rebuilt (
    id, session_id, target_kind, target_key, target_json, reasons_json, state,
    created_at_utc_ms, decided_at_utc_ms
)
SELECT id, session_id, target_kind, target_key, target_json, reasons_json, state,
       created_at_utc_ms, decided_at_utc_ms
  FROM filing_suggestions
 ORDER BY rowid;
DROP TABLE filing_suggestions;
ALTER TABLE filing_suggestions_rebuilt RENAME TO filing_suggestions;
CREATE INDEX filing_suggestions_pending ON filing_suggestions(state, session_id);
