-- Notes templates a person writes for themselves. See store/custom_templates.rs.
CREATE TABLE meeting_custom_template_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0)
);
INSERT INTO meeting_custom_template_state(singleton, revision) VALUES (1, 0);
CREATE TABLE meeting_custom_templates (
    template_id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    purpose TEXT NOT NULL,
    -- The ordered sections, as `[{"title":..,"instructions":..}]`.
    sections_json TEXT NOT NULL,
    created_at_utc_ms INTEGER NOT NULL,
    updated_at_utc_ms INTEGER NOT NULL
);
-- A meeting's or a series' own custom choice. No foreign key: a deleted
-- template is skipped on read and the next rung decides, and a meeting keeps
-- saying it chose a custom template rather than silently adopting the stale
-- built-in its row also carries.
ALTER TABLE meeting_user_notes ADD COLUMN custom_template_id TEXT;
ALTER TABLE meeting_series_preferences ADD COLUMN custom_template_id TEXT;
