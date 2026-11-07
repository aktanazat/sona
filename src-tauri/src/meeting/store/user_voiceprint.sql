CREATE TABLE user_voiceprint (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    model_id TEXT NOT NULL CHECK (length(trim(model_id)) > 0),
    model_revision TEXT NOT NULL CHECK (length(trim(model_revision)) > 0),
    model_sha256 TEXT NOT NULL CHECK (length(model_sha256) = 64),
    embedding_dimensions INTEGER NOT NULL CHECK (embedding_dimensions = 256),
    sample_rate_hz INTEGER NOT NULL CHECK (sample_rate_hz = 16000),
    feature_bins INTEGER NOT NULL CHECK (feature_bins = 80),
    feature_pipeline_revision TEXT NOT NULL CHECK (length(trim(feature_pipeline_revision)) > 0),
    normalization TEXT NOT NULL CHECK (length(trim(normalization)) > 0),
    embedding BLOB NOT NULL CHECK (length(embedding) = 1024),
    dictation_count INTEGER NOT NULL CHECK (dictation_count > 0),
    speech_ms INTEGER NOT NULL CHECK (speech_ms > 0),
    consent_version INTEGER NOT NULL CHECK (consent_version > 0),
    created_at_utc_ms INTEGER NOT NULL
);
