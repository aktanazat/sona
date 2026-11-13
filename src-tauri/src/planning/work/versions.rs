//! A task description's versions: each edit keeps the text it left, and
//! the first edit keeps the text the task had before it.

use super::types::{WorkActor, WorkItem, WorkPatch};
use crate::managers::history::HistoryManager;
use crate::planning::store::Refusal;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// One version of a task's description.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DescriptionVersion {
    pub(crate) id: String,
    pub(crate) item_id: String,
    /// None for an empty description.
    pub(crate) body: Option<String>,
    /// Who wrote it; none for the text a task had before its first edit.
    pub(crate) actor: Option<WorkActor>,
    /// The agent, for an agent's edit that named one.
    pub(crate) agent: Option<String>,
    pub(crate) created_at_ms: i64,
}

const COLUMNS: &str = "v.id, v.task_id, v.body, v.actor, n.agent, v.created_at_ms";

fn read(row: &Row<'_>) -> rusqlite::Result<DescriptionVersion> {
    Ok(DescriptionVersion {
        id: row.get(0)?,
        item_id: row.get(1)?,
        body: row.get(2)?,
        actor: row.get(3)?,
        agent: row.get(4)?,
        created_at_ms: row.get(5)?,
    })
}

fn insert(
    conn: &Connection,
    task_id: &str,
    body: Option<&str>,
    actor: Option<WorkActor>,
    activity_id: Option<&str>,
    at_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_task_versions (id, task_id, body, actor, activity_id, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            Uuid::new_v4().to_string(),
            task_id,
            body,
            actor.map(WorkActor::as_str),
            activity_id,
            at_ms
        ],
    )?;
    Ok(())
}

/// Keeps the description an edit wrote, after the one it replaced when the
/// task has no versions yet.
pub(crate) fn saved(
    conn: &Connection,
    before: &WorkItem,
    after: Option<&str>,
    activity_id: &str,
    actor: WorkActor,
    now_ms: i64,
) -> Result<()> {
    let any: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_task_versions WHERE task_id = ?1)",
        [&before.id],
        |row| row.get(0),
    )?;
    if !any && before.details.is_some() {
        insert(
            conn,
            &before.id,
            before.details.as_deref(),
            None,
            None,
            before.updated_at_ms,
        )?;
    }
    insert(
        conn,
        &before.id,
        after,
        Some(actor),
        Some(activity_id),
        now_ms,
    )
}

/// A task's versions, newest first.
pub(crate) fn list(conn: &Connection, task_id: &str) -> Result<Vec<DescriptionVersion>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM planning_task_versions v
         LEFT JOIN planning_activity_agents n ON n.activity_id = v.activity_id
         WHERE v.task_id = ?1 ORDER BY v.created_at_ms DESC, v.rowid DESC"
    ))?;
    let rows = statement.query_map([task_id], read)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn version(conn: &Connection, id: &str) -> Result<DescriptionVersion> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM planning_task_versions v
             LEFT JOIN planning_activity_agents n ON n.activity_id = v.activity_id
             WHERE v.id = ?1"
        ),
        [id],
        read,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

pub(crate) fn work_description_versions(
    app: AppHandle,
    id: String,
) -> Result<Vec<DescriptionVersion>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| list(conn, &id))
}

/// Puts a version back as the task's description, through the same update
/// every description edit takes.
pub(crate) async fn work_description_restore(
    app: AppHandle,
    operation_id: String,
    id: String,
    expected_revision: i64,
    version_id: String,
) -> Result<WorkItem, String> {
    let found = crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        let found = version(conn, &version_id)?;
        if found.item_id != id {
            return Err(Refusal::NotFound.into());
        }
        Ok(found)
    })?;
    let patch = WorkPatch {
        details: Some(found.body),
        ..WorkPatch::default()
    };
    super::work_update(app, operation_id, id, expected_revision, patch).await
}
