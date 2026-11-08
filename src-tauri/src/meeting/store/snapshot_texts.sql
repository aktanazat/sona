CREATE TABLE meeting_snapshot_texts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id TEXT NOT NULL UNIQUE REFERENCES meeting_snapshots(snapshot_id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    text TEXT NOT NULL
);
CREATE VIRTUAL TABLE meeting_snapshot_text_fts USING fts5(
    text,
    content='meeting_snapshot_texts',
    content_rowid='id'
);
CREATE TRIGGER meeting_snapshot_texts_insert AFTER INSERT ON meeting_snapshot_texts BEGIN
    INSERT INTO meeting_snapshot_text_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER meeting_snapshot_texts_delete AFTER DELETE ON meeting_snapshot_texts BEGIN
    INSERT INTO meeting_snapshot_text_fts(meeting_snapshot_text_fts, rowid, text)
    VALUES ('delete', old.id, old.text);
END;
