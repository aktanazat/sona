-- The meetings an import from another app made, by that app's own id for
-- each, so a second import of the same export adds only what is new.
-- Deleting the meeting forgets it: importing it again brings it back.
CREATE TABLE meeting_app_imports (
    source TEXT NOT NULL,
    source_key TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    PRIMARY KEY (source, source_key)
);
CREATE INDEX meeting_app_imports_session ON meeting_app_imports(session_id);
