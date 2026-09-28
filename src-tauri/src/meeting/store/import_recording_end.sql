-- Imports used a synthetic stop at import time. Their closed capture windows
-- retain the measured duration, so recover the recording's real end without
-- changing the separate whole-meeting deletion deadline.
WITH recorded_ends AS (
    SELECT m.id, m.started_at_utc_ms AS started_at,
           MAX(w.end_offset_ns) / 1000000 AS duration_ms
    FROM meeting_sessions m
    JOIN meeting_capture_windows w ON w.session_id = m.id
    WHERE m.origin_kind = '"import"'
      AND m.phase IN ('processing', 'review_ready', 'recovery_required')
      AND m.started_at_utc_ms IS NOT NULL
      AND w.end_offset_ns IS NOT NULL
    GROUP BY m.id
)
UPDATE meeting_sessions
SET ended_at_utc_ms = (
    SELECT started_at + duration_ms FROM recorded_ends WHERE id = meeting_sessions.id
)
WHERE id IN (
    SELECT id FROM recorded_ends WHERE started_at <= 9223372036854775807 - duration_ms
);
