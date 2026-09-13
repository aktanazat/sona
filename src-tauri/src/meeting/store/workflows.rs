mod runner;

use super::learning::LearningInputs;
use super::{MeetingStore, StoreError};
use crate::analytics::{DashboardTrendRequest, LocalCalendarRange};
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::document_types::DocumentId;
use crate::meeting::types::MeetingSessionId;
use crate::meeting::workflow_types::{
    NewWorkflowEvent, PaginatedWorkflowRuns, WorkflowDispatchResult, WorkflowEventId,
    WorkflowEventKind, WorkflowId, WorkflowJumpTarget, WorkflowOutcomeCode, WorkflowOutcomeCounts,
    WorkflowRunCursor, WorkflowRunId, WorkflowRunReceipt, WorkflowRunStatus, WorkflowRunTrend,
    WorkflowRunTrendPoint, WorkflowRunsRequest, WorkflowSummary, WorkflowsListResult,
};
use chrono::{DateTime, Local, NaiveDate, TimeZone};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;

pub(super) struct StoredWorkflowEvent {
    pub id: WorkflowEventId,
    pub kind: WorkflowEventKind,
    pub payload: Value,
    pub occurred_at_utc_ms: i64,
}

impl MeetingStore {
    pub(crate) fn record_workflow_event(
        &self,
        event: NewWorkflowEvent,
    ) -> Result<WorkflowDispatchResult, StoreError> {
        let payload_json =
            serde_json::to_string(&event.payload).map_err(|_| StoreError::Invalid)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate_id = WorkflowEventId::new();
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO workflow_events
                (id, kind, payload_json, occurred_at_utc_ms, source, dedupe_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                candidate_id.uuid().to_string(),
                event.kind.as_str(),
                payload_json,
                event.occurred_at_utc_ms,
                event.source,
                &event.dedupe_key
            ],
        )? != 0;
        let event_id = if inserted {
            candidate_id
        } else {
            let id: String = transaction.query_row(
                "SELECT id FROM workflow_events WHERE dedupe_key = ?1",
                [&event.dedupe_key],
                |row| row.get(0),
            )?;
            WorkflowEventId(Uuid::parse_str(&id).map_err(|_| StoreError::Corrupt)?)
        };
        transaction.commit()?;
        Ok(WorkflowDispatchResult {
            inserted,
            event_id,
            #[cfg(test)]
            receipts: Vec::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn record_and_run_workflow_event(
        &self,
        event: NewWorkflowEvent,
        inputs: &dyn LearningInputs,
    ) -> Result<WorkflowDispatchResult, StoreError> {
        let mut dispatch = self.record_workflow_event(event)?;
        dispatch.receipts = runner::run_event(self, dispatch.event_id, !dispatch.inserted, inputs)?;
        Ok(dispatch)
    }

    pub(crate) fn run_workflow_event(
        &self,
        event_id: WorkflowEventId,
        record_skips: bool,
        inputs: &dyn LearningInputs,
    ) -> Result<Vec<WorkflowRunReceipt>, StoreError> {
        runner::run_event(self, event_id, record_skips, inputs)
    }

    /// Events that still have work to do, for the reconciliation scan at
    /// startup.
    ///
    /// An event whose kind this build does not know is skipped rather than
    /// treated as corruption. Such a row can only come from a newer build that
    /// wrote it before a downgrade, and failing the scan on it would stall every
    /// other pending event on the machine — one unreadable row taking the whole
    /// queue with it. A kind that retries on its own clock is skipped too; see
    /// [`WorkflowEventKind::reconciled_at_launch`].
    pub(crate) fn pending_workflow_event_ids(&self) -> Result<Vec<WorkflowEventId>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT id, kind FROM workflow_events ORDER BY occurred_at_utc_ms, id")?;
        let events = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut pending = Vec::new();
        for (event_id, kind) in events {
            let event_id =
                WorkflowEventId(Uuid::parse_str(&event_id).map_err(|_| StoreError::Corrupt)?);
            let Some(kind) = WorkflowEventKind::from_str(&kind) else {
                continue;
            };
            if !kind.reconciled_at_launch() {
                continue;
            }
            for workflow_id in matching_enabled_workflows_in(&connection, kind)? {
                if !terminal_receipt_exists_in(&connection, event_id, kind, workflow_id)? {
                    pending.push(event_id);
                    break;
                }
            }
        }
        Ok(pending)
    }

    #[cfg(test)]
    pub(crate) fn rerun_workflow_event(
        &self,
        event_id: WorkflowEventId,
        inputs: &dyn LearningInputs,
    ) -> Result<Vec<WorkflowRunReceipt>, StoreError> {
        runner::run_event(self, event_id, true, inputs)
    }

    pub(crate) fn workflows_list(&self) -> Result<WorkflowsListResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let result = workflows_list_in(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn set_workflow_enabled(
        &self,
        workflow_id: WorkflowId,
        enabled: bool,
        expected_revision: u64,
    ) -> Result<WorkflowsListResult, StoreError> {
        // A permanently-enabled workflow is infrastructure, not a choice: it
        // never appears in the Settings list, so a request to toggle one is a
        // caller bug rather than a preference.
        if WorkflowId::PERMANENT.contains(&workflow_id) {
            return Err(StoreError::Invalid);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if workflow_revision_in(&transaction)? != expected_revision {
            return Err(StoreError::Conflict);
        }
        let changed = transaction.execute(
            "UPDATE workflow_settings SET enabled = ?1
              WHERE workflow_id = ?2 AND enabled != ?1",
            params![i64::from(enabled), workflow_id.as_str()],
        )? != 0;
        if changed {
            bump_workflow_revision_in(&transaction)?;
        }
        let result = workflows_list_in(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn workflow_runs(
        &self,
        request: WorkflowRunsRequest,
    ) -> Result<PaginatedWorkflowRuns, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let revision = workflow_run_revision_in(&transaction)?;
        let limit = request.limit.unwrap_or(50).min(100);
        let mut sql = String::from(
            "SELECT r.id, r.workflow_id, r.status, r.started_at_utc_ms,
                    r.finished_at_utc_ms, r.outcome_summary, r.error,
                    e.kind, e.payload_json
               FROM workflow_runs r
               JOIN workflow_events e ON e.id = r.event_id
              WHERE 1 = 1",
        );
        let mut values = Vec::<rusqlite::types::Value>::new();
        if let Some(workflow_id) = request.workflow_id {
            sql.push_str(" AND r.workflow_id = ?");
            values.push(workflow_id.as_str().to_string().into());
        }
        if let Some(cursor) = request.cursor {
            sql.push_str(
                " AND (r.started_at_utc_ms < ? OR
                       (r.started_at_utc_ms = ? AND r.id < ?))",
            );
            values.push(cursor.started_at_utc_ms.into());
            values.push(cursor.started_at_utc_ms.into());
            values.push(cursor.run_id.uuid().to_string().into());
        }
        sql.push_str(" ORDER BY r.started_at_utc_ms DESC, r.id DESC LIMIT ?");
        values.push(
            i64::try_from(limit.saturating_add(1))
                .map_err(|_| StoreError::Invalid)?
                .into(),
        );
        let mut statement = transaction.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(values), receipt_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut entries = rows
            .into_iter()
            .map(receipt_from_columns)
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        let next_cursor =
            has_more
                .then(|| entries.last())
                .flatten()
                .map(|receipt| WorkflowRunCursor {
                    started_at_utc_ms: receipt.started_at_utc_ms,
                    run_id: receipt.id,
                });
        let result = PaginatedWorkflowRuns {
            schema_version: SCHEMA_VERSION,
            revision,
            entries,
            next_cursor,
        };
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn workflow_run_revision(&self) -> Result<u64, StoreError> {
        let connection = self.connection()?;
        workflow_run_revision_in(&connection)
    }

    /// Runs started on each local calendar day of the window, today last.
    /// The days are cut where every dashboard range cuts them, so a run
    /// counted here is on the same day the meeting trend would put it.
    pub(crate) fn workflow_run_trend(
        &self,
        request: DashboardTrendRequest,
    ) -> Result<WorkflowRunTrend, StoreError> {
        self.workflow_run_trend_at(Local::now(), request)
    }

    pub(super) fn workflow_run_trend_at(
        &self,
        now: DateTime<Local>,
        request: DashboardTrendRequest,
    ) -> Result<WorkflowRunTrend, StoreError> {
        let calendar =
            LocalCalendarRange::at(now, request.range).map_err(|_| StoreError::Invalid)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT started_at_utc_ms FROM workflow_runs
              WHERE started_at_utc_ms >= ?1 AND started_at_utc_ms < ?2",
        )?;
        let starts = statement
            .query_map(
                params![calendar.start_utc_ms(), calendar.end_exclusive_utc_ms()],
                |row| row.get::<_, i64>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut per_day = HashMap::<NaiveDate, u64>::new();
        for started in starts {
            // A UTC instant has exactly one local reading.
            let date = Local
                .timestamp_millis_opt(started)
                .single()
                .ok_or(StoreError::Corrupt)?
                .date_naive();
            *per_day.entry(date).or_default() += 1;
        }
        let mut total = 0;
        let mut points = Vec::with_capacity(request.range.days());
        for date in calendar.local_dates().map_err(|_| StoreError::Invalid)? {
            let runs = per_day.remove(&date).unwrap_or(0);
            total += runs;
            points.push(WorkflowRunTrendPoint {
                local_date: date.format("%F").to_string(),
                runs,
            });
        }
        if !per_day.is_empty() {
            return Err(StoreError::Corrupt);
        }
        Ok(WorkflowRunTrend {
            range: request.range,
            range_start_local_date: calendar.start_local_date(),
            range_end_local_date: calendar.end_local_date(),
            total,
            points,
        })
    }

    pub(crate) fn remember_calendar_facts(
        &self,
        session_id: MeetingSessionId,
        event: &CalendarEventSummary,
    ) -> Result<(), StoreError> {
        let event_json = serde_json::to_string(event).map_err(|_| StoreError::Invalid)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO meeting_calendar_facts(session_id, event_key, event_json)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET
                event_key = excluded.event_key, event_json = excluded.event_json",
            params![session_id.uuid().to_string(), event.event_key, event_json],
        )?;
        Ok(())
    }

    pub(crate) fn meeting_calendar_facts(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<CalendarEventSummary>, StoreError> {
        let connection = self.connection()?;
        calendar_facts_in(&connection, session_id)
    }
}

pub(super) fn stored_event_in(
    connection: &Connection,
    event_id: WorkflowEventId,
) -> Result<StoredWorkflowEvent, StoreError> {
    let row = connection
        .query_row(
            "SELECT kind, payload_json, occurred_at_utc_ms
               FROM workflow_events WHERE id = ?1",
            [event_id.uuid().to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    Ok(StoredWorkflowEvent {
        id: event_id,
        kind: WorkflowEventKind::from_str(&row.0).ok_or(StoreError::Corrupt)?,
        payload: serde_json::from_str(&row.1).map_err(|_| StoreError::Corrupt)?,
        occurred_at_utc_ms: row.2,
    })
}

pub(super) fn calendar_facts_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Option<CalendarEventSummary>, StoreError> {
    let event_json: Option<String> = connection
        .query_row(
            "SELECT event_json FROM meeting_calendar_facts WHERE session_id = ?1",
            [session_id.uuid().to_string()],
            |row| row.get(0),
        )
        .optional()?;
    event_json
        .map(|json| serde_json::from_str(&json).map_err(|_| StoreError::Corrupt))
        .transpose()
}

/// Which enabled workflows an event of this kind runs.
///
/// The `match` is exhaustive over the Rust enum, so an event kind this build
/// does not know can only reach here as a database row a newer build wrote —
/// which is why the enabled lookup below tolerates a workflow row that is
/// likewise absent instead of failing the whole dispatch.
pub(super) fn matching_enabled_workflows_in(
    connection: &Connection,
    kind: WorkflowEventKind,
) -> Result<Vec<WorkflowId>, StoreError> {
    let matching: &[WorkflowId] = match kind {
        WorkflowEventKind::MeetingFinalized => &[
            WorkflowId::PersonLinking,
            WorkflowId::Continuity,
            WorkflowId::VocabularyMining,
            WorkflowId::CorrectionLearning,
        ],
        WorkflowEventKind::MeetingStarted => {
            &[WorkflowId::PersonLinking, WorkflowId::SeriesPriming]
        }
        WorkflowEventKind::SpeakerRenamed => &[WorkflowId::PersonLinking],
        WorkflowEventKind::CalendarMeetingDetected => &[WorkflowId::PreMeetingBriefing],
        WorkflowEventKind::DocumentIngested => &[WorkflowId::DocumentLinking],
        WorkflowEventKind::MeetingPromptRecorded
        | WorkflowEventKind::MeetingPromptIgnored
        | WorkflowEventKind::MeetingAutoRecordStarted
        | WorkflowEventKind::MeetingAutoRecordStopped
        | WorkflowEventKind::MeetingPrepPresented
        | WorkflowEventKind::MeetingPrepRecordArmed
        | WorkflowEventKind::MeetingPrepBriefOpened
        | WorkflowEventKind::MeetingPrepDismissed
        | WorkflowEventKind::MeetingWrapPresented
        | WorkflowEventKind::MeetingWrapNotesOpened
        | WorkflowEventKind::MeetingWrapFollowUpCopied
        | WorkflowEventKind::MeetingWrapDone => &[WorkflowId::MeetingActivity],
        WorkflowEventKind::DictationCorpusSwept => &[
            WorkflowId::SpokenPunctuation,
            WorkflowId::ModeHabits,
            WorkflowId::CaptureAdvisor,
        ],
        WorkflowEventKind::DictationCorrectionRecorded => &[WorkflowId::CorrectionLearning],
        WorkflowEventKind::DailyDigestDue => &[WorkflowId::DailyDigest],
        WorkflowEventKind::AudioImported | WorkflowEventKind::AgentHookEvent => &[],
    };
    let mut enabled = Vec::new();
    for workflow in matching {
        let is_enabled: Option<bool> = connection
            .query_row(
                "SELECT enabled FROM workflow_settings WHERE workflow_id = ?1",
                [workflow.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if is_enabled == Some(true) {
            enabled.push(*workflow);
        }
    }
    Ok(enabled)
}

/// Whether this workflow is done with this event.
///
/// "Done" is not the same question for every event kind. A failed run of an
/// event some later signal will raise again needs no retry — the next
/// occurrence brings its own event. The daily corpus sweep has no next
/// occurrence: every later dictation on the same local day collapses into the
/// same dedupe key, so for that kind only a successful run is the last word,
/// and the startup reconciliation scan picks the failure back up.
pub(super) fn terminal_receipt_exists_in(
    connection: &Connection,
    event_id: WorkflowEventId,
    event_kind: WorkflowEventKind,
    workflow_id: WorkflowId,
) -> Result<bool, StoreError> {
    let statuses: &str = if event_kind.retries_after_failure() {
        "('ok')"
    } else {
        "('ok', 'failed')"
    };
    connection
        .query_row(
            &format!(
                "SELECT EXISTS(
                    SELECT 1 FROM workflow_runs
                     WHERE workflow_id = ?1 AND event_id = ?2
                       AND status IN {statuses}
                 )"
            ),
            params![workflow_id.as_str(), event_id.uuid().to_string()],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(in crate::meeting::store) fn workflow_enabled_in(
    connection: &Connection,
    workflow_id: WorkflowId,
) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT enabled FROM workflow_settings WHERE workflow_id = ?1",
            [workflow_id.as_str()],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(in crate::meeting::store) fn workflow_has_successful_run_in(
    connection: &Connection,
    workflow_id: WorkflowId,
) -> Result<bool, StoreError> {
    if !workflow_enabled_in(connection, workflow_id)? {
        return Ok(false);
    }
    connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM workflow_runs
                 WHERE workflow_id = ?1 AND status = 'ok'
             )",
            [workflow_id.as_str()],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(in crate::meeting::store) fn workflow_succeeded_for_session_in(
    connection: &Connection,
    workflow_id: WorkflowId,
    session_id: MeetingSessionId,
) -> Result<bool, StoreError> {
    if !workflow_enabled_in(connection, workflow_id)? {
        return Ok(false);
    }
    let mut statement = connection.prepare(
        "SELECT e.payload_json
           FROM workflow_runs r
           JOIN workflow_events e ON e.id = r.event_id
          WHERE r.workflow_id = ?1 AND r.status = 'ok'",
    )?;
    let payloads = statement
        .query_map([workflow_id.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(payloads.into_iter().any(|payload| {
        serde_json::from_str::<Value>(&payload)
            .ok()
            .and_then(|payload| {
                payload
                    .get("session_id")
                    .and_then(Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
            })
            .is_some_and(|value| value == session_id.uuid())
    }))
}

pub(in crate::meeting::store) fn workflow_succeeded_for_calendar_event_in(
    connection: &Connection,
    event_key: &str,
) -> Result<bool, StoreError> {
    if !workflow_enabled_in(connection, WorkflowId::PreMeetingBriefing)? {
        return Ok(false);
    }
    let mut statement = connection.prepare(
        "SELECT e.payload_json
           FROM workflow_runs r
           JOIN workflow_events e ON e.id = r.event_id
          WHERE r.workflow_id = 'pre_meeting_briefing' AND r.status = 'ok'",
    )?;
    let payloads = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(payloads.into_iter().any(|payload| {
        serde_json::from_str::<Value>(&payload)
            .ok()
            .and_then(|payload| {
                payload
                    .get("event")
                    .and_then(|event| event.get("eventKey"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .is_some_and(|value| value == event_key)
    }))
}

pub(super) fn workflow_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT revision FROM workflow_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::Corrupt)
}

pub(super) fn bump_workflow_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    connection.execute(
        "UPDATE workflow_state SET revision = revision + 1 WHERE singleton = 1",
        [],
    )?;
    workflow_revision_in(connection)
}

pub(super) fn workflow_run_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT run_revision FROM workflow_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::Corrupt)
}

pub(super) fn bump_workflow_run_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    connection.execute(
        "UPDATE workflow_state SET run_revision = run_revision + 1 WHERE singleton = 1",
        [],
    )?;
    workflow_run_revision_in(connection)
}

fn workflows_list_in(connection: &Connection) -> Result<WorkflowsListResult, StoreError> {
    let revision = workflow_revision_in(connection)?;
    let mut entries = Vec::with_capacity(WorkflowId::CONFIGURABLE.len());
    for workflow_id in WorkflowId::CONFIGURABLE {
        let enabled: bool = connection.query_row(
            "SELECT enabled FROM workflow_settings WHERE workflow_id = ?1",
            [workflow_id.as_str()],
            |row| row.get(0),
        )?;
        let columns = connection
            .query_row(
                "SELECT r.id, r.workflow_id, r.status, r.started_at_utc_ms,
                        r.finished_at_utc_ms, r.outcome_summary, r.error,
                        e.kind, e.payload_json
                   FROM workflow_runs r
                   JOIN workflow_events e ON e.id = r.event_id
                  WHERE r.workflow_id = ?1
                  ORDER BY r.started_at_utc_ms DESC, r.id DESC LIMIT 1",
                [workflow_id.as_str()],
                receipt_columns,
            )
            .optional()?;
        entries.push(WorkflowSummary {
            id: workflow_id,
            enabled,
            last_run: columns.map(receipt_from_columns).transpose()?,
        });
    }
    Ok(WorkflowsListResult {
        schema_version: SCHEMA_VERSION,
        revision,
        entries,
    })
}

pub(super) type ReceiptColumns = (
    String,
    String,
    String,
    i64,
    i64,
    String,
    Option<String>,
    String,
    String,
);

pub(super) fn receipt_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReceiptColumns> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

pub(super) fn receipt_from_columns(
    columns: ReceiptColumns,
) -> Result<WorkflowRunReceipt, StoreError> {
    let id = Uuid::parse_str(&columns.0).map_err(|_| StoreError::Corrupt)?;
    let workflow_id = WorkflowId::from_str(&columns.1).ok_or(StoreError::Corrupt)?;
    let status = match columns.2.as_str() {
        "ok" => WorkflowRunStatus::Ok,
        "failed" => WorkflowRunStatus::Failed,
        "skipped" => WorkflowRunStatus::Skipped,
        _ => return Err(StoreError::Corrupt),
    };
    let event_kind = WorkflowEventKind::from_str(&columns.7).ok_or(StoreError::Corrupt)?;
    let payload: Value = serde_json::from_str(&columns.8).map_err(|_| StoreError::Corrupt)?;
    let (outcome_code, outcome_counts) =
        outcome_projection(workflow_id, event_kind, status, &columns.5);
    Ok(WorkflowRunReceipt {
        id: WorkflowRunId(id),
        workflow_id,
        event_kind,
        jump_target: jump_target(event_kind, &payload),
        status,
        started_at_utc_ms: columns.3,
        finished_at_utc_ms: columns.4,
        outcome_summary: columns.5,
        outcome_code,
        outcome_counts,
        error: columns.6,
    })
}

/// What a stored run means, in the two forms a reader needs: one outcome code
/// and the counts that fill its sentence.
///
/// Summaries are one grammar — `name:field=value`, values numeric — and this is
/// the only thing that parses them. Nothing here string-matches a whole
/// summary: the meeting-activity codes come from the event kind, which is the
/// fact they were always a restatement of.
pub(super) fn outcome_projection(
    workflow_id: WorkflowId,
    event_kind: WorkflowEventKind,
    status: WorkflowRunStatus,
    summary: &str,
) -> (WorkflowOutcomeCode, WorkflowOutcomeCounts) {
    let code = match status {
        WorkflowRunStatus::Failed => WorkflowOutcomeCode::Failed,
        WorkflowRunStatus::Skipped if summary == "already_processed" => {
            WorkflowOutcomeCode::AlreadyProcessed
        }
        WorkflowRunStatus::Skipped => WorkflowOutcomeCode::Skipped,
        WorkflowRunStatus::Ok => match workflow_id {
            WorkflowId::PersonLinking => WorkflowOutcomeCode::PersonLinks,
            WorkflowId::PreMeetingBriefing => WorkflowOutcomeCode::Briefing,
            WorkflowId::Continuity => WorkflowOutcomeCode::Continuity,
            WorkflowId::VocabularyMining => WorkflowOutcomeCode::VocabularyCandidates,
            WorkflowId::DocumentLinking => WorkflowOutcomeCode::DocumentLinks,
            WorkflowId::SpokenPunctuation
            | WorkflowId::CorrectionLearning
            | WorkflowId::ModeHabits
            | WorkflowId::CaptureAdvisor => WorkflowOutcomeCode::LearningSuggestions,
            WorkflowId::SeriesPriming => WorkflowOutcomeCode::SeriesPrimed,
            WorkflowId::DailyDigest => WorkflowOutcomeCode::DigestRaised,
            WorkflowId::MeetingActivity => match event_kind {
                WorkflowEventKind::MeetingPromptRecorded => WorkflowOutcomeCode::PromptRecorded,
                WorkflowEventKind::MeetingPromptIgnored => WorkflowOutcomeCode::PromptIgnored,
                WorkflowEventKind::MeetingAutoRecordStarted => {
                    WorkflowOutcomeCode::AutoRecordStarted
                }
                WorkflowEventKind::MeetingAutoRecordStopped => {
                    WorkflowOutcomeCode::AutoRecordStopped
                }
                WorkflowEventKind::MeetingPrepPresented => WorkflowOutcomeCode::PrepPresented,
                WorkflowEventKind::MeetingPrepRecordArmed => WorkflowOutcomeCode::PrepRecordArmed,
                WorkflowEventKind::MeetingPrepBriefOpened => WorkflowOutcomeCode::PrepBriefOpened,
                WorkflowEventKind::MeetingPrepDismissed => WorkflowOutcomeCode::PrepDismissed,
                WorkflowEventKind::MeetingWrapPresented => WorkflowOutcomeCode::WrapPresented,
                WorkflowEventKind::MeetingWrapNotesOpened => WorkflowOutcomeCode::WrapNotesOpened,
                WorkflowEventKind::MeetingWrapFollowUpCopied => {
                    WorkflowOutcomeCode::WrapFollowUpCopied
                }
                WorkflowEventKind::MeetingWrapDone => WorkflowOutcomeCode::WrapDone,
                _ => WorkflowOutcomeCode::Skipped,
            },
        },
    };
    let mut counts = WorkflowOutcomeCounts::default();
    for field in summary.split([':', ',']) {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let Ok(value) = value.parse::<u64>() else {
            continue;
        };
        match key {
            "changes" => counts.changes = value,
            "persons" => counts.persons = value,
            "series" => counts.series = value,
            "carried" => counts.carried = value,
            "candidates" => counts.candidates = value,
            "suggestions" => counts.suggestions = value,
            "terms" => counts.terms = value,
            "meetings" => counts.meetings = value,
            "loops_closed" => counts.loops_closed = value,
            "suggestions_waiting" => counts.suggestions_waiting = value,
            "waiting_on_stale" => counts.waiting_on_stale = value,
            _ => {}
        }
    }
    (code, counts)
}

fn jump_target(kind: WorkflowEventKind, payload: &Value) -> Option<WorkflowJumpTarget> {
    let key = match kind {
        WorkflowEventKind::DocumentIngested => "document_id",
        WorkflowEventKind::MeetingFinalized
        | WorkflowEventKind::MeetingStarted
        | WorkflowEventKind::SpeakerRenamed
        | WorkflowEventKind::MeetingPromptRecorded
        | WorkflowEventKind::MeetingAutoRecordStarted
        | WorkflowEventKind::MeetingAutoRecordStopped
        | WorkflowEventKind::MeetingPrepPresented
        | WorkflowEventKind::MeetingPrepRecordArmed
        | WorkflowEventKind::MeetingPrepBriefOpened
        | WorkflowEventKind::MeetingPrepDismissed
        | WorkflowEventKind::MeetingWrapPresented
        | WorkflowEventKind::MeetingWrapNotesOpened
        | WorkflowEventKind::MeetingWrapFollowUpCopied
        | WorkflowEventKind::MeetingWrapDone => "session_id",
        _ => return None,
    };
    let id = Uuid::parse_str(payload.get(key)?.as_str()?).ok()?;
    match kind {
        WorkflowEventKind::DocumentIngested => Some(WorkflowJumpTarget::Document {
            document_id: DocumentId(id),
        }),
        _ => Some(WorkflowJumpTarget::Meeting {
            session_id: MeetingSessionId::from_uuid(id),
        }),
    }
}
