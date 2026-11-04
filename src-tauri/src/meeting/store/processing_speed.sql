CREATE TABLE meeting_processing_speeds (
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    stage TEXT NOT NULL CHECK (stage IN ('audio', 'speech', 'speakers', 'notes', 'people')),
    audio_ns INTEGER NOT NULL CHECK (audio_ns > 0),
    wall_ns INTEGER NOT NULL CHECK (wall_ns > 0),
    measured_at_utc_ms INTEGER NOT NULL,
    PRIMARY KEY (session_id, stage)
);
CREATE INDEX meeting_processing_speeds_stage_idx
    ON meeting_processing_speeds(stage, measured_at_utc_ms);
CREATE TABLE meeting_processing_receipts (
    artifact_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_artifact_revisions(artifact_id) ON DELETE CASCADE,
    wall_ms INTEGER NOT NULL CHECK (wall_ms > 0),
    audio_ns INTEGER NOT NULL CHECK (audio_ns > 0),
    route TEXT NOT NULL CHECK (route IN ('local', 'relay'))
);
