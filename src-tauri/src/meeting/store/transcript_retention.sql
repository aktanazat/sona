ALTER TABLE meeting_sessions ADD COLUMN transcript_purged_at_utc_ms INTEGER;
ALTER TABLE meeting_sessions ADD COLUMN transcript_audio_purge_pending INTEGER NOT NULL DEFAULT 0
    CHECK (transcript_audio_purge_pending IN (0, 1));
-- Earlier versions stamped the end when notes finished. Use the recorded stop
-- for existing finished meetings without changing whole-meeting deadlines.
UPDATE meeting_sessions
SET ended_at_utc_ms = (
    SELECT MIN(e.observed_at_utc_ms) FROM meeting_session_events e
    WHERE e.session_id = meeting_sessions.id AND e.next_phase = 'stopping'
)
WHERE phase IN ('review_ready', 'recovery_required')
  AND EXISTS (
    SELECT 1 FROM meeting_session_events e
    WHERE e.session_id = meeting_sessions.id AND e.next_phase = 'stopping'
  );
CREATE INDEX meeting_transcript_retention_due_idx
    ON meeting_sessions(ended_at_utc_ms, id)
    WHERE transcript_purged_at_utc_ms IS NULL;
CREATE TABLE meeting_transcript_retention_policy (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    policy_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    changed_at_utc_ms INTEGER NOT NULL
);
INSERT INTO meeting_transcript_retention_policy
    (singleton, policy_json, revision, changed_at_utc_ms)
    VALUES (1, '{"kind":"forever"}', 0, 0);
CREATE TABLE meeting_transcript_purge_paths (
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    directory INTEGER NOT NULL CHECK (directory IN (0, 1)),
    PRIMARY KEY (session_id, relative_path)
);

-- Keep transcript revision identities: generated notes reference them with CASCADE.
-- Marking the session and removing every stored transcript copy are one write.
CREATE TRIGGER meeting_transcript_retention_purge
AFTER UPDATE OF transcript_purged_at_utc_ms ON meeting_sessions
WHEN OLD.transcript_purged_at_utc_ms IS NULL AND NEW.transcript_purged_at_utc_ms IS NOT NULL
BEGIN
    DELETE FROM meeting_transcript_segments WHERE transcript_revision_id IN (
        SELECT transcript_revision_id FROM meeting_transcript_revisions WHERE session_id = NEW.id
    );
    DELETE FROM meeting_search_documents WHERE session_id = NEW.id AND entity_kind = 'segment';
    DELETE FROM meeting_semantic_chunks WHERE session_id = NEW.id;
    DELETE FROM meeting_semantic_index_state WHERE session_id = NEW.id;
    DELETE FROM meeting_diarization_evidence_spans WHERE generation_id IN (
        SELECT generation_id FROM meeting_diarization_generations WHERE session_id = NEW.id
    );
    -- Suggestions carry a second copy of examples, even when other days still
    -- contribute to the same suggestion. Drop those cards before their source rows.
    DELETE FROM learning_suggestions WHERE (loop_kind, candidate_key) IN (
        SELECT loop_kind, candidate_key FROM learning_observations WHERE source_session_id = NEW.id
    );
    DELETE FROM learning_observations WHERE source_session_id = NEW.id;
    INSERT OR IGNORE INTO meeting_transcript_purge_paths
        SELECT NEW.id, payload_relative_dir, 1 FROM meeting_cloud_outbox WHERE source_session_id = NEW.id;
    INSERT OR IGNORE INTO meeting_transcript_purge_paths
        SELECT NEW.id, remote_bundle_relative_path, 0 FROM meeting_cloud_conflicts WHERE source_session_id = NEW.id;
    INSERT OR IGNORE INTO meeting_transcript_purge_paths
        SELECT NEW.id, '.cloud-inbox/' || object_id || '.wav', 0 FROM meeting_cloud_heads WHERE source_session_id = NEW.id;
    INSERT OR IGNORE INTO meeting_transcript_purge_paths
        SELECT NEW.id, '.cloud-conflicts/' || object_id || '.bundle', 0 FROM meeting_cloud_heads WHERE source_session_id = NEW.id;
    UPDATE meeting_cloud_outbox SET state = 'cancelled', claim_token = NULL, claimed_at_utc_ms = NULL
        WHERE source_session_id = NEW.id AND state IN ('pending', 'claimed', 'terminal') AND kind != 'tombstone';
    DELETE FROM meeting_cloud_outbox_chunks WHERE outbox_id IN (
        SELECT outbox_id FROM meeting_cloud_outbox WHERE source_session_id = NEW.id
    );
    DELETE FROM meeting_cloud_conflicts WHERE source_session_id = NEW.id;
    -- Links that reached the server remain live. An unfinished share has no
    -- durable published contract and must not upload the old transcript later.
    UPDATE meeting_cloud_shares SET state = 'failed'
        WHERE source_session_id = NEW.id AND state = 'pending';
END;

-- A model or evidence job that started before the purge cannot put words back.
CREATE TRIGGER meeting_transcript_segments_require_retained_transcript
BEFORE INSERT ON meeting_transcript_segments
WHEN EXISTS (
    SELECT 1 FROM meeting_transcript_revisions r JOIN meeting_sessions m ON m.id = r.session_id
    WHERE r.transcript_revision_id = NEW.transcript_revision_id AND m.transcript_purged_at_utc_ms IS NOT NULL
)
BEGIN SELECT RAISE(ABORT, 'meeting transcript deleted'); END;
CREATE TRIGGER voice_profile_samples_require_retained_transcript
BEFORE INSERT ON voice_profile_samples
WHEN (SELECT transcript_purged_at_utc_ms FROM meeting_sessions WHERE id = NEW.source_session_id) IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'meeting transcript deleted'); END;
