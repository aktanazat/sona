CREATE TABLE meeting_call_name_runs (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    plan_id TEXT NOT NULL REFERENCES meeting_run_plans(plan_id) ON DELETE CASCADE,
    target_json TEXT NOT NULL,
    automatically_use INTEGER NOT NULL DEFAULT 0 CHECK (automatically_use IN (0, 1)),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    state TEXT NOT NULL,
    detail TEXT NOT NULL
);
CREATE TABLE meeting_call_name_samples (
    session_id TEXT NOT NULL REFERENCES meeting_call_name_runs(session_id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence >= 0 AND sequence < 4096),
    start_ns INTEGER NOT NULL CHECK (start_ns >= 0),
    end_ns INTEGER NOT NULL CHECK (end_ns >= start_ns),
    observation_json TEXT NOT NULL CHECK (length(observation_json) <= 100000),
    PRIMARY KEY (session_id, sequence)
);
CREATE TABLE meeting_call_participants (
    session_id TEXT NOT NULL REFERENCES meeting_call_name_runs(session_id) ON DELETE CASCADE,
    participant_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    is_local INTEGER NOT NULL CHECK (is_local IN (0, 1)),
    PRIMARY KEY (session_id, participant_id)
);
CREATE TABLE meeting_call_name_suggestions (
    session_id TEXT NOT NULL REFERENCES meeting_call_name_runs(session_id) ON DELETE CASCADE,
    speaker_id TEXT NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE CASCADE,
    generation_id TEXT NOT NULL REFERENCES meeting_diarization_generations(generation_id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    overlap_ns INTEGER NOT NULL,
    speech_ns INTEGER NOT NULL,
    dismissed INTEGER NOT NULL DEFAULT 0 CHECK (dismissed IN (0, 1)),
    PRIMARY KEY (session_id, speaker_id)
);
CREATE TRIGGER purge_call_names_with_transcript
AFTER UPDATE OF transcript_purged_at_utc_ms ON meeting_sessions
WHEN NEW.transcript_purged_at_utc_ms IS NOT NULL
BEGIN
    DELETE FROM meeting_call_name_runs WHERE session_id = NEW.id;
END;
