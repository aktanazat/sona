CREATE TABLE transcript_relisten_decisions (
    segment_id TEXT NOT NULL REFERENCES meeting_transcript_segments(segment_id) ON DELETE CASCADE,
    original_phrase TEXT NOT NULL CHECK (length(trim(original_phrase)) > 0),
    replacement_phrase TEXT NOT NULL CHECK (length(trim(replacement_phrase)) > 0),
    accepted INTEGER NOT NULL CHECK (accepted IN (0, 1)),
    edit_sequence INTEGER,
    source_session_id TEXT NOT NULL,
    source_segment_id TEXT NOT NULL,
    fixed_at_utc_ms INTEGER NOT NULL,
    decided_at_utc_ms INTEGER NOT NULL,
    PRIMARY KEY (segment_id, original_phrase, replacement_phrase),
    FOREIGN KEY (segment_id, edit_sequence)
        REFERENCES meeting_segment_edits(segment_id, edit_sequence) ON DELETE CASCADE,
    CHECK ((accepted = 1) = (edit_sequence IS NOT NULL))
);
