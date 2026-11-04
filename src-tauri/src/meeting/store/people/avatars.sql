CREATE TABLE person_avatars (
    person_id TEXT PRIMARY KEY NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    png BLOB NOT NULL CHECK (length(png) > 0 AND length(png) <= 1048576),
    updated_at_utc_ms INTEGER NOT NULL
);
