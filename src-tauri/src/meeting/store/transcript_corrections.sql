CREATE TABLE transcript_correction_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0)
);
INSERT INTO transcript_correction_state(singleton, revision) VALUES (1, 0);

CREATE TABLE person_transcript_corrections (
    correction_id TEXT PRIMARY KEY NOT NULL,
    person_id TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    source_session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    source_segment_id TEXT NOT NULL REFERENCES meeting_transcript_segments(segment_id) ON DELETE CASCADE,
    source_speaker_id TEXT NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE CASCADE,
    original_text TEXT NOT NULL,
    corrected_text TEXT NOT NULL,
    original_phrase TEXT NOT NULL CHECK (length(trim(original_phrase)) > 0),
    replacement_phrase TEXT NOT NULL CHECK (length(trim(replacement_phrase)) > 0),
    created_at_utc_ms INTEGER NOT NULL,
    updated_at_utc_ms INTEGER NOT NULL,
    UNIQUE (person_id, original_phrase)
);
CREATE INDEX person_transcript_corrections_source_session_idx
    ON person_transcript_corrections(source_session_id);
CREATE INDEX person_transcript_corrections_source_segment_idx
    ON person_transcript_corrections(source_segment_id);
CREATE INDEX person_transcript_corrections_source_speaker_idx
    ON person_transcript_corrections(source_speaker_id);

CREATE TRIGGER person_transcript_corrections_source_immutable
BEFORE UPDATE OF person_id, source_session_id, source_segment_id, source_speaker_id,
                 original_text, created_at_utc_ms ON person_transcript_corrections
BEGIN SELECT RAISE(ABORT, 'correction source is immutable'); END;

CREATE TRIGGER person_transcript_corrections_source_owner
BEFORE INSERT ON person_transcript_corrections
WHEN NOT EXISTS (
    SELECT 1 FROM meeting_transcript_segments segment
    JOIN meeting_transcript_revisions revision
      ON revision.transcript_revision_id = segment.transcript_revision_id
    JOIN meeting_speakers speaker ON speaker.speaker_id = NEW.source_speaker_id
    WHERE segment.segment_id = NEW.source_segment_id
      AND revision.session_id = NEW.source_session_id
      AND speaker.session_id = NEW.source_session_id
      AND segment.base_text = NEW.original_text
)
BEGIN SELECT RAISE(ABORT, 'correction source does not match transcript'); END;

CREATE TRIGGER person_transcript_corrections_insert_revision
AFTER INSERT ON person_transcript_corrections
BEGIN UPDATE transcript_correction_state SET revision = revision + 1 WHERE singleton = 1; END;
CREATE TRIGGER person_transcript_corrections_update_revision
AFTER UPDATE ON person_transcript_corrections
BEGIN UPDATE transcript_correction_state SET revision = revision + 1 WHERE singleton = 1; END;
CREATE TRIGGER person_transcript_corrections_delete_revision
AFTER DELETE ON person_transcript_corrections
BEGIN UPDATE transcript_correction_state SET revision = revision + 1 WHERE singleton = 1; END;
