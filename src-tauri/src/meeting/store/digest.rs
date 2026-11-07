//! D20: what one local day held, counted from the store.
//!
//! Four numbers, each read from the thing that already owns it: meetings from
//! the sessions that actually began capturing, loops closed from the receipts
//! D18's resolve mutation wrote, suggestions waiting from the pending learning
//! queue, and overdue handoffs from D27's direction on the corpus ledger walk.
//! Nothing here writes, and nothing here decides whether a notification is
//! worth raising — that is `crate::meeting::digest`'s call.

use super::loops::ledger_rows_in;
#[cfg(test)]
use super::MeetingStore;
use super::StoreError;
use crate::meeting::loop_types::waiting_on_is_stale;
use crate::meeting::types::{MeetingCommandKind, OperationResult};
use rusqlite::{params, Connection};

/// A day's activity, in the four numbers the evening sentence is made of.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MeetingDigestCounts {
    pub meetings: u64,
    pub loops_closed: u64,
    pub suggestions_waiting: u64,
    pub waiting_on_stale: u64,
}

#[cfg(test)]
impl MeetingStore {
    pub(crate) fn digest_counts(
        &self,
        day_start_utc_ms: i64,
        day_end_utc_ms: i64,
    ) -> Result<MeetingDigestCounts, StoreError> {
        let user_name = self.user_name();
        let connection = self.connection()?;
        digest_counts_in(
            &connection,
            day_start_utc_ms,
            day_end_utc_ms,
            user_name.as_deref(),
        )
    }
}

pub(super) fn digest_counts_in(
    connection: &Connection,
    day_start_utc_ms: i64,
    day_end_utc_ms: i64,
    user_name: Option<&str>,
) -> Result<MeetingDigestCounts, StoreError> {
    Ok(MeetingDigestCounts {
        meetings: meetings_captured_in(connection, day_start_utc_ms, day_end_utc_ms)?,
        loops_closed: loops_closed_in(connection, day_start_utc_ms, day_end_utc_ms)?,
        suggestions_waiting: suggestions_waiting_in(connection)?,
        // The day's end is this run's "now", so replaying the event counts the
        // same evening rather than today's.
        waiting_on_stale: waiting_on_stale_in(connection, day_end_utc_ms, user_name)?,
    })
}

/// Rows somebody else has owed for longer than a working week.
///
/// Like the waiting suggestions, a backlog rather than a day count — and for
/// the same reason: a promise nobody kept last month is exactly the thing an
/// evening nudge is for. Both registers count, because "waiting on them" is
/// mostly commitments and a question left hanging on somebody is the same
/// debt.
fn waiting_on_stale_in(
    connection: &Connection,
    now_utc_ms: i64,
    user_name: Option<&str>,
) -> Result<u64, StoreError> {
    let mut count: u64 = 0;
    for meeting in ledger_rows_in(connection, user_name)? {
        for row in &meeting.rows {
            if waiting_on_is_stale(
                row.direction,
                row.status,
                row.outstanding_since(meeting.at_utc_ms),
                now_utc_ms,
            ) {
                count += 1;
            }
        }
    }
    Ok(count)
}

/// Meetings that actually began, bucketed by when capture started.
///
/// The dashboard trend counts by `created_at_utc_ms`, which includes a
/// preflight someone opened and abandoned. An evening sentence should not: "2
/// meetings" has to mean two meetings that happened, so the window is on
/// `started_at_utc_ms` and a session with none is not one.
///
/// D28. A meeting whose series has been taken out of the digest is not counted.
/// The preference is a `COALESCE` rather than a join so the three cases that
/// mean "count it" — no calendar event, no preference row, an included series —
/// stay one predicate instead of three.
fn meetings_captured_in(
    connection: &Connection,
    day_start_utc_ms: i64,
    day_end_utc_ms: i64,
) -> Result<u64, StoreError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM meeting_sessions s
          WHERE s.phase != 'deleting'
            AND s.started_at_utc_ms IS NOT NULL
            AND s.started_at_utc_ms >= ?1
            AND s.started_at_utc_ms < ?2
            AND COALESCE((
                  SELECT p.digest_included
                    FROM meeting_calendar_facts f
                    JOIN meeting_series_preferences p
                      ON p.series_key = json_extract(f.event_json, '$.seriesKey')
                   WHERE f.session_id = s.id
                ), 1) = 1",
        params![day_start_utc_ms, day_end_utc_ms],
        |row| row.get(0),
    )?;
    u64::try_from(count).map_err(|_| StoreError::Corrupt)
}

/// Loops D18's resolve mutation closed today.
///
/// One committed `loop_resolve` receipt is exactly one loop — the mutation
/// never batches — so counting receipts is counting loops. `loop_resolve`
/// covers both "done" and "dropped", and both leave the loop closed, which is
/// what the sentence says.
///
/// A rejected receipt is a write that was fenced out and changed nothing, so
/// the result is part of the predicate rather than an afterthought.
fn loops_closed_in(
    connection: &Connection,
    day_start_utc_ms: i64,
    day_end_utc_ms: i64,
) -> Result<u64, StoreError> {
    let command = serde_json::to_value(MeetingCommandKind::LoopResolve)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or(StoreError::Corrupt)?;
    let committed = serde_json::to_value(OperationResult::Committed)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or(StoreError::Corrupt)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM meeting_operation_receipts
          WHERE created_at_utc_ms >= ?1
            AND created_at_utc_ms < ?2
            AND json_extract(receipt_json, '$.command') = ?3
            AND json_extract(receipt_json, '$.result') = ?4",
        params![day_start_utc_ms, day_end_utc_ms, command, committed],
        |row| row.get(0),
    )?;
    u64::try_from(count).map_err(|_| StoreError::Corrupt)
}

/// Suggestions still waiting for an answer, at any age.
///
/// Not a day count: "1 suggestion waiting" is a backlog, and a backlog that
/// built up yesterday is exactly the thing an evening nudge is for. Deciding a
/// suggestion deletes its row, so the table is the queue.
fn suggestions_waiting_in(connection: &Connection) -> Result<u64, StoreError> {
    let count: i64 =
        connection.query_row("SELECT COUNT(*) FROM learning_suggestions", [], |row| {
            row.get(0)
        })?;
    u64::try_from(count).map_err(|_| StoreError::Corrupt)
}
