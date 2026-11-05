ALTER TABLE meeting_sessions ADD COLUMN audio_purged_at_utc_ms INTEGER;
ALTER TABLE meeting_sessions ADD COLUMN audio_purge_pending INTEGER NOT NULL DEFAULT 0
    CHECK (audio_purge_pending IN (0, 1));
CREATE INDEX meeting_audio_retention_due_idx
    ON meeting_sessions(ended_at_utc_ms, id)
    WHERE audio_purged_at_utc_ms IS NULL;
CREATE TABLE meeting_audio_retention_policy (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    policy_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    changed_at_utc_ms INTEGER NOT NULL
);

-- A whole-meeting policy still at its first default was never chosen: keep
-- meetings, and delete their audio after 90 days, seven days from now at the
-- earliest. A policy someone set stays, with all the audio it kept.
INSERT INTO meeting_audio_retention_policy
    (singleton, policy_json, revision, changed_at_utc_ms)
SELECT 1,
       CASE WHEN revision = 0 AND policy_json = '{"kind":"delete_after_days","days":30}'
            THEN '{"kind":"delete_after_days","days":90}'
            ELSE '{"kind":"forever"}' END,
       0,
       CAST(strftime('%s', 'now') AS INTEGER) * 1000
FROM meeting_retention_policy WHERE singleton = 1;
-- A store still at its first default gave every meeting it made that
-- default, so a meeting with any other date came from another Mac.
UPDATE meeting_sessions
SET retention_policy_json = '{"kind":"forever"}', delete_after_utc_ms = NULL
WHERE EXISTS (
    SELECT 1 FROM meeting_retention_policy
    WHERE singleton = 1 AND revision = 0
      AND policy_json = '{"kind":"delete_after_days","days":30}'
);
UPDATE meeting_retention_policy SET policy_json = '{"kind":"forever"}'
WHERE singleton = 1 AND revision = 0
  AND policy_json = '{"kind":"delete_after_days","days":30}';
