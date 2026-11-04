CREATE TABLE meeting_speaker_voiceprints (
    speaker_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    model_id TEXT NOT NULL CHECK (length(trim(model_id)) > 0),
    model_revision TEXT NOT NULL CHECK (length(trim(model_revision)) > 0),
    model_sha256 TEXT NOT NULL CHECK (length(model_sha256) = 64),
    embedding_dimensions INTEGER NOT NULL CHECK (embedding_dimensions = 256),
    sample_rate_hz INTEGER NOT NULL CHECK (sample_rate_hz = 16000),
    feature_bins INTEGER NOT NULL CHECK (feature_bins = 80),
    feature_pipeline_revision TEXT NOT NULL CHECK (length(trim(feature_pipeline_revision)) > 0),
    normalization TEXT NOT NULL CHECK (length(trim(normalization)) > 0),
    embedding BLOB NOT NULL CHECK (length(embedding) = 1024),
    created_at_utc_ms INTEGER NOT NULL
);
CREATE INDEX meeting_speaker_voiceprints_session_idx
    ON meeting_speaker_voiceprints(session_id);
