CREATE TABLE transcript_correction_context_copy AS
SELECT * FROM person_transcript_corrections;
DROP TABLE person_transcript_corrections;

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
    kind_json TEXT NOT NULL CHECK (kind_json IN ('"phrase"', '"name"', '"term"', '"dialect"', '"code_switching"')),
    context_json TEXT NOT NULL CHECK (json_valid(context_json) AND json_extract(context_json, '$.kind') IN ('person', 'conversation', 'surrounding_phrase')),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
);
INSERT INTO person_transcript_corrections (
    correction_id, person_id, source_session_id, source_segment_id, source_speaker_id,
    original_text, corrected_text, original_phrase, replacement_phrase,
    created_at_utc_ms, updated_at_utc_ms, kind_json, context_json, enabled
)
SELECT correction_id, person_id, source_session_id, source_segment_id, source_speaker_id,
       original_text, corrected_text, original_phrase, replacement_phrase,
       created_at_utc_ms, updated_at_utc_ms, '"phrase"', '{"kind":"person"}', 1
FROM transcript_correction_context_copy;
DROP TABLE transcript_correction_context_copy;

CREATE INDEX person_transcript_corrections_person_phrase_idx
    ON person_transcript_corrections(person_id, original_phrase);
CREATE INDEX person_transcript_corrections_source_session_idx
    ON person_transcript_corrections(source_session_id);
CREATE INDEX person_transcript_corrections_source_segment_idx
    ON person_transcript_corrections(source_segment_id);
CREATE INDEX person_transcript_corrections_source_speaker_idx
    ON person_transcript_corrections(source_speaker_id);

CREATE TRIGGER person_transcript_corrections_source_immutable
BEFORE UPDATE OF correction_id, source_session_id, source_segment_id, source_speaker_id,
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

INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
SELECT DISTINCT person_id, NULL FROM person_transcript_corrections;
UPDATE transcript_correction_state SET revision = revision + 1 WHERE singleton = 1;
