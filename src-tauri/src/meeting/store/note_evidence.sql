CREATE TABLE meeting_note_evidence (
    note_id TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    input_revision INTEGER NOT NULL CHECK (input_revision >= 0),
    title TEXT NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (note_id, session_id)
);
CREATE INDEX meeting_note_evidence_session ON meeting_note_evidence(session_id);
CREATE TRIGGER meeting_note_evidence_source_deleted
AFTER UPDATE OF phase, transcript_purged_at_utc_ms ON meeting_sessions
WHEN NEW.phase = 'deleting' OR NEW.transcript_purged_at_utc_ms IS NOT NULL
BEGIN
    DELETE FROM meeting_note_evidence WHERE session_id = NEW.id;
END;
CREATE TRIGGER meeting_note_evidence_require_retained_source_insert
BEFORE INSERT ON meeting_note_evidence
WHEN NOT EXISTS (
    SELECT 1 FROM meeting_sessions
    WHERE id = NEW.session_id AND phase != 'deleting'
      AND transcript_purged_at_utc_ms IS NULL AND revision = NEW.input_revision
)
BEGIN SELECT RAISE(ABORT, 'conversation evidence unavailable'); END;
CREATE TRIGGER meeting_note_evidence_require_retained_source_update
BEFORE UPDATE ON meeting_note_evidence
WHEN NOT EXISTS (
    SELECT 1 FROM meeting_sessions
    WHERE id = NEW.session_id AND phase != 'deleting'
      AND transcript_purged_at_utc_ms IS NULL AND revision = NEW.input_revision
)
BEGIN SELECT RAISE(ABORT, 'conversation evidence unavailable'); END;
