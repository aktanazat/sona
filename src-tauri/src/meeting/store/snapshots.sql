CREATE TABLE meeting_snapshots (
    snapshot_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    offset_ns INTEGER NOT NULL CHECK (offset_ns >= 0),
    captured_at_utc_ms INTEGER NOT NULL,
    width INTEGER NOT NULL CHECK (width > 0),
    height INTEGER NOT NULL CHECK (height > 0),
    capture_trigger TEXT NOT NULL CHECK (capture_trigger IN ('manual', 'automatic')),
    app_bundle_id TEXT,
    byte_length INTEGER NOT NULL CHECK (byte_length > 0)
);
CREATE INDEX meeting_snapshots_session_idx ON meeting_snapshots(session_id, offset_ns);
