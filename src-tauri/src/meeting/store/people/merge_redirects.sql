CREATE TABLE person_merge_redirects (
    source_person_id TEXT PRIMARY KEY NOT NULL,
    target_person_id TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE
);
CREATE INDEX person_merge_redirects_target ON person_merge_redirects(target_person_id);
