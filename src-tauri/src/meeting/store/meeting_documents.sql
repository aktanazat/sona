DROP INDEX document_person_links_person_idx;
ALTER TABLE document_person_links RENAME TO document_person_links_rebuilding;
ALTER TABLE documents RENAME TO documents_rebuilding;
CREATE TABLE documents (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    source_name TEXT NOT NULL CHECK (length(trim(source_name)) > 0),
    media_type TEXT NOT NULL
        CHECK (media_type IN ('text/plain', 'text/markdown', 'application/pdf')),
    content TEXT NOT NULL,
    created_at_utc_ms INTEGER NOT NULL,
    updated_at_utc_ms INTEGER NOT NULL,
    session_id TEXT REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    original BLOB,
    CHECK ((media_type = 'application/pdf') = (original IS NOT NULL))
);
INSERT INTO documents (
    id, title, source_name, media_type, content, created_at_utc_ms, updated_at_utc_ms
)
SELECT id, title, source_name, media_type, content, created_at_utc_ms, updated_at_utc_ms
  FROM documents_rebuilding
 ORDER BY rowid;
CREATE INDEX documents_session_idx
    ON documents(session_id, created_at_utc_ms) WHERE session_id IS NOT NULL;
CREATE TABLE document_person_links (
    document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    person_id TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    created_at_utc_ms INTEGER NOT NULL,
    PRIMARY KEY (document_id, person_id)
);
INSERT INTO document_person_links (document_id, person_id, created_at_utc_ms)
SELECT document_id, person_id, created_at_utc_ms
  FROM document_person_links_rebuilding
 ORDER BY rowid;
CREATE INDEX document_person_links_person_idx
    ON document_person_links(person_id, created_at_utc_ms DESC);
DROP TABLE document_person_links_rebuilding;
DROP TABLE documents_rebuilding;
