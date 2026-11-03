CREATE TABLE transcript_correction_invalidations (
    person_id TEXT NOT NULL,
    source_session_id TEXT
);
CREATE VIEW transcript_correction_dependent_sessions AS
SELECT DISTINCT session.id AS session_id, COALESCE(resolution.resolved_person_id, matched.person_id) AS person_id
FROM meeting_sessions session
JOIN meeting_speakers speaker ON speaker.session_id = session.id
LEFT JOIN meeting_voice_speaker_resolutions resolution
  ON resolution.speaker_id = speaker.speaker_id AND resolution.is_current = 1
LEFT JOIN voice_speaker_matches matched ON matched.speaker_id = speaker.speaker_id
WHERE session.phase != 'deleting' AND session.current_transcript_revision_id IS NOT NULL;

CREATE TRIGGER transcript_correction_invalidate_dependents
AFTER INSERT ON transcript_correction_invalidations
BEGIN
    UPDATE transcript_correction_state SET revision = revision + 1 WHERE singleton = 1;
    UPDATE meeting_sessions SET revision = revision + 1
    WHERE id IS NOT NEW.source_session_id AND id IN (
        SELECT session_id FROM transcript_correction_dependent_sessions WHERE person_id = NEW.person_id
    );
    UPDATE meeting_artifacts SET state = 'out_of_date'
    WHERE state = 'current' AND session_id IS NOT NEW.source_session_id AND session_id IN (
        SELECT session_id FROM transcript_correction_dependent_sessions WHERE person_id = NEW.person_id
    );
    UPDATE meeting_artifact_revisions SET state = 'out_of_date'
    WHERE state = 'current' AND session_id IS NOT NEW.source_session_id AND session_id IN (
        SELECT session_id FROM transcript_correction_dependent_sessions WHERE person_id = NEW.person_id
    );
    UPDATE meeting_questions SET answer_state = 'out_of_date'
    WHERE answer_state != 'forgotten' AND session_id IS NOT NEW.source_session_id AND session_id IN (
        SELECT session_id FROM transcript_correction_dependent_sessions WHERE person_id = NEW.person_id
    );
    DELETE FROM transcript_correction_invalidations;
END;

CREATE TRIGGER transcript_correction_resolution_insert
AFTER INSERT ON meeting_voice_speaker_resolutions
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, (SELECT session_id FROM meeting_speakers WHERE speaker_id = NEW.speaker_id) FROM person_transcript_corrections WHERE source_speaker_id = NEW.speaker_id;
END;

CREATE TRIGGER transcript_correction_resolution_update
AFTER UPDATE ON meeting_voice_speaker_resolutions
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, (SELECT session_id FROM meeting_speakers WHERE speaker_id = NEW.speaker_id) FROM person_transcript_corrections WHERE source_speaker_id = NEW.speaker_id;
END;

CREATE TRIGGER transcript_correction_resolution_delete
BEFORE DELETE ON meeting_voice_speaker_resolutions
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, (SELECT session_id FROM meeting_speakers WHERE speaker_id = OLD.speaker_id) FROM person_transcript_corrections WHERE source_speaker_id = OLD.speaker_id;
END;

CREATE TRIGGER transcript_correction_match_insert
AFTER INSERT ON voice_speaker_matches
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, NEW.session_id FROM person_transcript_corrections WHERE source_speaker_id = NEW.speaker_id;
END;

CREATE TRIGGER transcript_correction_match_update
AFTER UPDATE ON voice_speaker_matches
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, NEW.session_id FROM person_transcript_corrections WHERE source_speaker_id = NEW.speaker_id;
END;

CREATE TRIGGER transcript_correction_match_delete
BEFORE DELETE ON voice_speaker_matches
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, OLD.session_id FROM person_transcript_corrections WHERE source_speaker_id = OLD.speaker_id;
END;

CREATE TRIGGER transcript_correction_speaker_update
AFTER UPDATE OF revision, merged_into_speaker_id ON meeting_speakers
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, NEW.session_id FROM person_transcript_corrections WHERE source_speaker_id = NEW.speaker_id;
END;

CREATE TRIGGER transcript_correction_speaker_delete
BEFORE DELETE ON meeting_speakers
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, OLD.session_id FROM person_transcript_corrections WHERE source_speaker_id = OLD.speaker_id;
END;

CREATE TRIGGER transcript_correction_session_update
AFTER UPDATE OF current_transcript_revision_id, current_diarization_generation_id, phase ON meeting_sessions
WHEN OLD.current_transcript_revision_id IS NOT NEW.current_transcript_revision_id OR OLD.current_diarization_generation_id IS NOT NEW.current_diarization_generation_id OR (NEW.phase = 'deleting' AND OLD.phase != 'deleting')
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, NEW.id FROM person_transcript_corrections WHERE source_session_id = NEW.id;
END;

CREATE TRIGGER transcript_correction_session_delete
BEFORE DELETE ON meeting_sessions
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, OLD.id FROM person_transcript_corrections WHERE source_session_id = OLD.id;
END;

CREATE TRIGGER transcript_correction_segment_delete
BEFORE DELETE ON meeting_transcript_segments
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, (SELECT session_id FROM meeting_transcript_revisions WHERE transcript_revision_id = OLD.transcript_revision_id) FROM person_transcript_corrections WHERE source_segment_id = OLD.segment_id;
END;

CREATE TRIGGER transcript_correction_segment_edit
AFTER INSERT ON meeting_segment_edits
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, (SELECT source_session_id FROM person_transcript_corrections WHERE source_segment_id = NEW.segment_id LIMIT 1) FROM person_transcript_corrections WHERE source_segment_id = NEW.segment_id;
END;

CREATE TRIGGER transcript_correction_profile_update
AFTER UPDATE ON voice_profiles
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, NULL FROM person_transcript_corrections WHERE person_id = NEW.person_id;
END;

CREATE TRIGGER transcript_correction_profile_delete
BEFORE DELETE ON voice_profiles
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, NULL FROM person_transcript_corrections WHERE person_id = OLD.person_id;
END;

CREATE TRIGGER transcript_correction_person_delete
BEFORE DELETE ON persons
BEGIN
    INSERT INTO transcript_correction_invalidations(person_id, source_session_id)
    SELECT DISTINCT person_id, NULL FROM person_transcript_corrections WHERE person_id = OLD.id;
END;
