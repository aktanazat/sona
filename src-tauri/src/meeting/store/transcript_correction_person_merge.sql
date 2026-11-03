DROP TRIGGER person_transcript_corrections_source_immutable;
CREATE TRIGGER person_transcript_corrections_source_immutable
BEFORE UPDATE OF correction_id, source_session_id, source_segment_id, source_speaker_id,
                 original_text, created_at_utc_ms ON person_transcript_corrections
BEGIN SELECT RAISE(ABORT, 'correction source is immutable'); END;
