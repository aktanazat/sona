//! Saved prompts and their runs.
//!
//! Two tables with two different jobs, the same split
//! [`super::automations`] makes.
//!
//! `saved_prompts` is a preference: rows the operator writes, fenced on one
//! shared revision and written under an [`crate::meeting::types::OperationReceipt`]
//! like every other user mutation here. The three rows the migration seeds are
//! ordinary rows — the surface cannot tell them apart from one typed this
//! morning, which is the point.
//!
//! `saved_prompt_runs` is the run log, and it is *the* receipt for one
//! generation rather than a copy of one: nothing about a run is fenced or
//! requested, so it records its own outcome in its own row. Nothing retries. A
//! row reading `failed` is the answer, and it stays.
//!
//! Both deletions that can reach a run are foreign keys rather than sweeps: the
//! prompt's, because an answer is derived from the prompt, and the anchor
//! meeting's, because a run quotes words that die with that meeting.

use super::fence::{write_fenced, Fence, FencedWrite};
use super::{id, parse_uuid, MeetingStore, StoreError};
use crate::meeting::people_types::PersonId;
use crate::meeting::prompt_types::{
    normalized_prompt, PromptOutput, PromptRun, PromptRunFailure, PromptRunResult, PromptTarget,
    PromptTargetRef, SavedPrompt, SavedPromptDeleteRequest, SavedPromptList,
    SavedPromptMutationResult, SavedPromptSaveRequest,
};
use crate::meeting::types::{
    MeetingArtifactId, MeetingCommandKind, MeetingSessionId, SavedPromptId,
};
use rusqlite::{params, Connection, OptionalExtension};

/// How many meetings a person or series prompt reads from. A prompt about a
/// recurring meeting is about the recent ones; reading every meeting a person
/// ever attended would spend the whole evidence budget on the oldest of them.
pub(crate) const PROMPT_SESSION_LIMIT: usize = 12;

impl MeetingStore {
    /// Every prompt, oldest first, with the fence its writes carry.
    ///
    /// Creation order rather than name order: the seeded three keep the order
    /// they were written in, and a prompt the operator adds lands at the end
    /// where they left it rather than jumping into the middle of a list.
    pub(crate) fn saved_prompts(&self) -> Result<SavedPromptList, StoreError> {
        let connection = self.connection()?;
        prompts_in(&connection)
    }

    /// One prompt, or `None` when it has been deleted.
    pub(crate) fn saved_prompt(
        &self,
        prompt_id: SavedPromptId,
    ) -> Result<Option<SavedPrompt>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT prompt_id, name, body, output_kind, json_schema, target,
                        created_at_utc_ms, updated_at_utc_ms
                   FROM saved_prompts WHERE prompt_id = ?1",
                params![id(prompt_id)],
                prompt_columns,
            )
            .optional()?
            .map(prompt_from_columns)
            .transpose()
    }

    /// Create a prompt, or rewrite one.
    ///
    /// Idempotent on `operation_id` and fenced on `expected_revision`, like
    /// every other receipted mutation. An unusable prompt — no name, no body, a
    /// schema that is not JSON — is refused as invalid rather than stored and
    /// failed at generation time, so the field the operator is still looking at
    /// is where they find out.
    pub(crate) fn save_saved_prompt(
        &self,
        request: &SavedPromptSaveRequest,
        requested_at_utc_ms: i64,
    ) -> Result<SavedPromptMutationResult, StoreError> {
        let (name, body, output) = normalized_prompt(request).map_err(|_| StoreError::Invalid)?;
        let prompt_id = request.prompt_id.unwrap_or_default();
        let (output_kind, json_schema) = stored_output(&output);
        let (receipt, prompts) = write_fenced(
            self,
            FencedWrite {
                fence: PROMPTS_FENCE,
                command: MeetingCommandKind::SavedPromptSave,
                effect_ids: vec![id(prompt_id)],
                operation_id: request.operation_id,
                expected_revision: request.expected_revision,
                requested_at_utc_ms,
            },
            prompts_in,
            |connection, now| {
                connection.execute(
                    "INSERT INTO saved_prompts (
                        prompt_id, name, body, output_kind, json_schema, target,
                        created_at_utc_ms, updated_at_utc_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                     ON CONFLICT(prompt_id) DO UPDATE SET
                        name = excluded.name,
                        body = excluded.body,
                        output_kind = excluded.output_kind,
                        json_schema = excluded.json_schema,
                        target = excluded.target,
                        updated_at_utc_ms = excluded.updated_at_utc_ms",
                    params![
                        id(prompt_id),
                        name,
                        body,
                        output_kind,
                        json_schema,
                        request.target.as_str(),
                        now
                    ],
                )?;
                Ok(())
            },
        )?;
        Ok(SavedPromptMutationResult { receipt, prompts })
    }

    /// Forget a prompt, and with it every answer it produced.
    pub(crate) fn delete_saved_prompt(
        &self,
        request: &SavedPromptDeleteRequest,
        requested_at_utc_ms: i64,
    ) -> Result<SavedPromptMutationResult, StoreError> {
        let (receipt, prompts) = write_fenced(
            self,
            FencedWrite {
                fence: PROMPTS_FENCE,
                command: MeetingCommandKind::SavedPromptDelete,
                effect_ids: vec![id(request.prompt_id)],
                operation_id: request.operation_id,
                expected_revision: request.expected_revision,
                requested_at_utc_ms,
            },
            prompts_in,
            |connection, _now| {
                connection.execute(
                    "DELETE FROM saved_prompts WHERE prompt_id = ?1",
                    params![id(request.prompt_id)],
                )?;
                Ok(())
            },
        )?;
        Ok(SavedPromptMutationResult { receipt, prompts })
    }

    /// Write down what one run produced.
    ///
    /// `anchor_session_id` is the meeting whose deletion takes this row with
    /// it. For a meeting prompt that is the meeting itself; for a person or a
    /// series it is the newest meeting behind that noun, which is the same
    /// meeting whose series decided where the evidence was allowed to go.
    pub(crate) fn record_prompt_run(
        &self,
        run: &PromptRun,
        anchor_session_id: MeetingSessionId,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO saved_prompt_runs (
                run_id, prompt_id, target_kind, target_id, anchor_session_id,
                artifact_id, model_id, model_version, produced_at_utc_ms,
                result_kind, result
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id(run.run_id),
                id(run.prompt_id),
                run.target_kind.as_str(),
                run.target_id,
                id(anchor_session_id),
                run.artifact_id.map(id),
                run.model_id,
                run.model_version,
                run.produced_at_utc_ms,
                run.result.as_str(),
                stored_result(&run.result),
            ],
        )?;
        Ok(())
    }

    /// Every run for one noun, newest first.
    pub(crate) fn prompt_runs(
        &self,
        target: &PromptTargetRef,
    ) -> Result<Vec<PromptRun>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT run_id, prompt_id, target_kind, target_id, artifact_id,
                    model_id, model_version, produced_at_utc_ms, result_kind, result
               FROM saved_prompt_runs
              WHERE target_kind = ?1 AND target_id = ?2
              ORDER BY produced_at_utc_ms DESC, run_id DESC",
        )?;
        let rows = statement
            .query_map(params![target.target().as_str(), target.id()], run_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        rows.into_iter().map(run_from_columns).collect()
    }

    /// A completed or failed attempt both count: folder defaults must not
    /// silently spend another model call when finalization is delivered again.
    pub(crate) fn has_prompt_run_for_artifact(
        &self,
        prompt_id: SavedPromptId,
        session_id: MeetingSessionId,
        artifact_id: MeetingArtifactId,
    ) -> Result<bool, StoreError> {
        self.connection()?
            .query_row(
                "SELECT EXISTS(
                SELECT 1 FROM saved_prompt_runs
                 WHERE prompt_id = ?1 AND target_kind = 'meeting'
                   AND target_id = ?2 AND artifact_id = ?3)",
                params![id(prompt_id), id(session_id), id(artifact_id)],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// The notes revision a meeting currently shows, or `None` when nothing has
    /// been generated for it yet.
    pub(crate) fn current_artifact_id(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<MeetingArtifactId>, StoreError> {
        let connection = self.connection()?;
        let stored: Option<String> = connection
            .query_row(
                "SELECT artifact_id FROM meeting_artifact_revisions
                  WHERE session_id = ?1 AND state = 'current'
                  ORDER BY generated_at_utc_ms DESC LIMIT 1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()?;
        stored
            .map(|value| parse_uuid(&value).map(MeetingArtifactId::from_uuid))
            .transpose()
    }

    /// The meetings of one series, newest first.
    ///
    /// Joined through the calendar facts each session remembered when it
    /// started, the same way [`super::series::session_series_key_in`] reads the
    /// key back — a series is only ever known through the meetings that carry
    /// it.
    pub(crate) fn series_session_ids(
        &self,
        series_key: &str,
        limit: usize,
    ) -> Result<Vec<MeetingSessionId>, StoreError> {
        let connection = self.connection()?;
        session_ids(
            &connection,
            "SELECT f.session_id
               FROM meeting_calendar_facts f
               JOIN meeting_sessions m ON m.id = f.session_id
              WHERE json_extract(f.event_json, '$.seriesKey') = ?1
                AND m.phase != 'deleting'
              ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC
              LIMIT ?2",
            params![series_key.trim(), to_limit(limit)?],
        )
    }

    /// The meetings one person was confirmed in, newest first.
    pub(crate) fn person_session_ids(
        &self,
        person_id: PersonId,
        limit: usize,
    ) -> Result<Vec<MeetingSessionId>, StoreError> {
        let connection = self.connection()?;
        session_ids(
            &connection,
            "SELECT l.meeting_id
               FROM meeting_person_links l
               JOIN meeting_sessions m ON m.id = l.meeting_id
              WHERE l.person_id = ?1 AND l.confidence = 'confirmed'
                AND m.phase != 'deleting'
              ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC
              LIMIT ?2",
            params![person_id.uuid().to_string(), to_limit(limit)?],
        )
    }
}

fn to_limit(limit: usize) -> Result<i64, StoreError> {
    i64::try_from(limit).map_err(|_| StoreError::Invalid)
}

fn session_ids(
    connection: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<MeetingSessionId>, StoreError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map(parameters, |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    rows.iter()
        .map(|value| parse_uuid(value).map(MeetingSessionId::from_uuid))
        .collect()
}

fn prompts_in(connection: &Connection) -> Result<SavedPromptList, StoreError> {
    let revision = prompts_revision_in(connection)?;
    let mut statement = connection.prepare(
        "SELECT prompt_id, name, body, output_kind, json_schema, target,
                created_at_utc_ms, updated_at_utc_ms
           FROM saved_prompts
          ORDER BY created_at_utc_ms ASC, prompt_id ASC",
    )?;
    let rows = statement
        .query_map([], prompt_columns)?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    Ok(SavedPromptList {
        prompts: rows
            .into_iter()
            .map(prompt_from_columns)
            .collect::<Result<Vec<_>, _>>()?,
        revision,
    })
}

/// `(prompt_id, name, body, output_kind, json_schema, target, created, updated)`,
/// still as SQLite spelled them.
type PromptColumns = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
    i64,
);

fn prompt_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<PromptColumns> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

/// One row, or the corruption it is. A stored output kind or target this build
/// does not know is an error rather than a silently dropped prompt.
fn prompt_from_columns(columns: PromptColumns) -> Result<SavedPrompt, StoreError> {
    let (prompt_id, name, body, output_kind, json_schema, target, created, updated) = columns;
    let output = match (output_kind.as_str(), json_schema) {
        ("text", _) => PromptOutput::Text,
        ("schema", Some(json_schema)) => PromptOutput::Schema { json_schema },
        _ => return Err(StoreError::Corrupt),
    };
    Ok(SavedPrompt {
        prompt_id: SavedPromptId::from_uuid(parse_uuid(&prompt_id)?),
        name,
        body,
        output,
        target: PromptTarget::from_str(&target).ok_or(StoreError::Corrupt)?,
        created_at_utc_ms: created,
        updated_at_utc_ms: updated,
    })
}

fn stored_output(output: &PromptOutput) -> (&'static str, Option<&str>) {
    match output {
        PromptOutput::Text => ("text", None),
        PromptOutput::Schema { json_schema } => ("schema", Some(json_schema.as_str())),
    }
}

/// The one column a result's words live in. The kind is stored beside it, so a
/// failure keeps its reason where the other two keep their answer.
fn stored_result(result: &PromptRunResult) -> &str {
    match result {
        PromptRunResult::Text { text } => text,
        PromptRunResult::Json { json } => json,
        PromptRunResult::Failed { reason } => reason.as_str(),
    }
}

/// `(run_id, prompt_id, target_kind, target_id, artifact_id, model_id,
/// model_version, produced_at, result_kind, result)`.
type RunColumns = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    i64,
    String,
    String,
);

fn run_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunColumns> {
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
        row.get(9)?,
    ))
}

fn run_from_columns(columns: RunColumns) -> Result<PromptRun, StoreError> {
    let (
        run_id,
        prompt_id,
        target_kind,
        target_id,
        artifact_id,
        model_id,
        model_version,
        produced_at_utc_ms,
        result_kind,
        result,
    ) = columns;
    let result = match result_kind.as_str() {
        "text" => PromptRunResult::Text { text: result },
        "json" => PromptRunResult::Json { json: result },
        "failed" => PromptRunResult::Failed {
            reason: PromptRunFailure::from_str(&result).ok_or(StoreError::Corrupt)?,
        },
        _ => return Err(StoreError::Corrupt),
    };
    Ok(PromptRun {
        run_id: crate::meeting::types::PromptRunId::from_uuid(parse_uuid(&run_id)?),
        prompt_id: SavedPromptId::from_uuid(parse_uuid(&prompt_id)?),
        target_kind: PromptTarget::from_str(&target_kind).ok_or(StoreError::Corrupt)?,
        target_id,
        artifact_id: artifact_id
            .as_deref()
            .map(|value| parse_uuid(value).map(MeetingArtifactId::from_uuid))
            .transpose()?,
        model_id,
        model_version,
        produced_at_utc_ms,
        result,
    })
}

fn prompts_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT revision FROM saved_prompt_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::Corrupt)
}

fn bump_prompts_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    connection.execute(
        "UPDATE saved_prompt_state SET revision = revision + 1 WHERE singleton = 1",
        [],
    )?;
    prompts_revision_in(connection)
}

/// The counter every prompt write is fenced on: one for the whole table,
/// because the settings page and the palette both hold the whole list.
const PROMPTS_FENCE: Fence = Fence {
    read: prompts_revision_in,
    bump: bump_prompts_revision_in,
};
