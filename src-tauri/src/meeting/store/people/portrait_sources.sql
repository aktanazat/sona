ALTER TABLE person_avatars ADD COLUMN provenance_json TEXT;
ALTER TABLE person_avatars ADD COLUMN original BLOB CHECK (original IS NULL OR (length(original) > 0 AND length(original) <= 10485760));
