ALTER TABLE conversation_bookmarks
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'mark'
    CHECK (kind IN ('mark', 'decision', 'question', 'todo'));
