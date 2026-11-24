-- A link can now come from what was said in the meeting or from the names its
-- call listed. SQLite cannot widen a CHECK in place, so the table is rebuilt.
CREATE TABLE meeting_person_links_next (
    meeting_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    person_id TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('calendar', 'speaker', 'title', 'transcript', 'manual')),
    confidence TEXT NOT NULL CHECK (confidence IN ('confirmed', 'suggested')),
    created_at_utc_ms INTEGER NOT NULL,
    PRIMARY KEY (meeting_id, person_id)
);
INSERT INTO meeting_person_links_next
    SELECT meeting_id, person_id, source, confidence, created_at_utc_ms FROM meeting_person_links;
DROP TABLE meeting_person_links;
ALTER TABLE meeting_person_links_next RENAME TO meeting_person_links;
CREATE INDEX meeting_person_links_person_idx
    ON meeting_person_links(person_id, confidence, created_at_utc_ms DESC);
