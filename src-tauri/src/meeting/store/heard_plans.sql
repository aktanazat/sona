-- Plans to meet heard in a recording. What, when, who and where are kept as
-- said; the window is the time those words name, read against the meeting's
-- own date. People are names, never linked to a person until someone confirms
-- that person in a later recording. Status is worked out on read, never
-- stored.
CREATE TABLE meeting_heard_plans (
    plan_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    people_json TEXT NOT NULL,
    when_text TEXT NOT NULL,
    start_utc_ms INTEGER,
    end_utc_ms INTEGER,
    all_day INTEGER NOT NULL DEFAULT 0 CHECK (all_day IN (0, 1)),
    place TEXT,
    quote TEXT,
    segment_id TEXT,
    start_offset_ns INTEGER,
    end_offset_ns INTEGER,
    calendar_event_id TEXT,
    declined_at_utc_ms INTEGER,
    edited_at_utc_ms INTEGER,
    created_at_utc_ms INTEGER NOT NULL,
    updated_at_utc_ms INTEGER NOT NULL,
    CHECK ((start_utc_ms IS NULL) = (end_utc_ms IS NULL)),
    CHECK (start_utc_ms IS NULL OR start_utc_ms < end_utc_ms),
    CHECK (start_utc_ms IS NOT NULL OR all_day = 0),
    CHECK ((segment_id IS NULL) = (start_offset_ns IS NULL)
        AND (segment_id IS NULL) = (end_offset_ns IS NULL))
);
CREATE INDEX meeting_heard_plans_session_idx ON meeting_heard_plans(session_id);
-- The transcript a meeting's last reading covered, so a meeting is read once
-- per transcript even when the reading found nothing.
CREATE TABLE meeting_heard_plan_readings (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    transcript_revision_id TEXT NOT NULL,
    read_at_utc_ms INTEGER NOT NULL
);

-- The quote is a copy of the transcript: it goes when the transcript goes.
CREATE TRIGGER meeting_heard_plans_transcript_purge
AFTER UPDATE OF transcript_purged_at_utc_ms ON meeting_sessions
WHEN OLD.transcript_purged_at_utc_ms IS NULL AND NEW.transcript_purged_at_utc_ms IS NOT NULL
BEGIN
    UPDATE meeting_heard_plans
    SET quote = NULL, segment_id = NULL, start_offset_ns = NULL, end_offset_ns = NULL
    WHERE session_id = NEW.id;
END;

-- A reading that started before the purge cannot put words back.
CREATE TRIGGER meeting_heard_plans_require_retained_transcript
BEFORE INSERT ON meeting_heard_plans
WHEN NEW.quote IS NOT NULL
  AND (SELECT transcript_purged_at_utc_ms FROM meeting_sessions WHERE id = NEW.session_id) IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'meeting transcript deleted'); END;
