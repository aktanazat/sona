-- Folders: a person's own grouping of meetings. See store/folders.rs.
CREATE TABLE meeting_folder_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0)
);
INSERT INTO meeting_folder_state(singleton, revision) VALUES (1, 0);
CREATE TABLE meeting_folders (
    folder_id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    template_id TEXT CHECK (template_id IS NULL OR length(trim(template_id)) > 0),
    created_at_utc_ms INTEGER NOT NULL,
    updated_at_utc_ms INTEGER NOT NULL
);
CREATE TABLE meeting_folder_members (
    folder_id TEXT NOT NULL REFERENCES meeting_folders(folder_id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    added_at_utc_ms INTEGER NOT NULL,
    PRIMARY KEY (folder_id, session_id)
);
CREATE INDEX meeting_folder_members_session_idx ON meeting_folder_members(session_id);
CREATE TABLE meeting_folder_prompts (
    folder_id TEXT NOT NULL REFERENCES meeting_folders(folder_id) ON DELETE CASCADE,
    prompt_id TEXT NOT NULL REFERENCES saved_prompts(prompt_id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (folder_id, prompt_id)
);
-- Where a trashed meeting was filed, keyed by its deletion job. No key onto
-- the job or its receipt: the job row is gone once the deletion finishes, and
-- the receipt is written only then.
CREATE TABLE meeting_folder_trash (
    job_id TEXT NOT NULL,
    folder_id TEXT NOT NULL REFERENCES meeting_folders(folder_id) ON DELETE CASCADE,
    added_at_utc_ms INTEGER NOT NULL,
    PRIMARY KEY (job_id, folder_id)
);
