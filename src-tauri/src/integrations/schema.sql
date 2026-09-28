CREATE TABLE integration_connections (
    id TEXT PRIMARY KEY NOT NULL,
    record_json TEXT NOT NULL
);
CREATE TABLE integration_preferences (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    record_json TEXT NOT NULL
);
CREATE TABLE integration_rules (
    id TEXT PRIMARY KEY NOT NULL,
    connection_id TEXT NOT NULL REFERENCES integration_connections(id) ON DELETE CASCADE,
    record_json TEXT NOT NULL
);
CREATE TABLE integration_receipts (
    id TEXT PRIMARY KEY NOT NULL,
    connection_id TEXT NOT NULL,
    dedup_key TEXT UNIQUE,
    created_at_utc_ms INTEGER NOT NULL,
    record_json TEXT NOT NULL
);
CREATE INDEX integration_receipts_recent ON integration_receipts(created_at_utc_ms DESC);
