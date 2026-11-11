//! The bin: a deleted task stays 30 days, archived and out of every list,
//! and can be restored as it was until it is purged for good.

use super::items::{self, Changed};
use super::types::{WorkActivityKind, WorkActor, WorkItem};
use super::{now_ms, run_history, store, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::catalog::BIN_MS;
use crate::planning::store::Refusal;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// What the history keeps for a move into or out of the bin.
pub(crate) const BIN: &str = "bin";

/// A task in the bin and when it went there.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Binned {
    pub(crate) item: WorkItem,
    pub(crate) trashed_at_ms: i64,
    pub(crate) purge_at_ms: i64,
}

/// When the task went to the bin, if it is there.
pub(crate) fn trashed_at(conn: &Connection, id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT trashed_at_ms FROM planning_tasks WHERE id = ?1",
            [id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Moves a task to the bin. It is archived with it, unless it already was,
/// so restoring it brings back whichever it was.
pub(crate) fn trash(conn: &Connection, id: &str, actor: WorkActor, now_ms: i64) -> Result<Changed> {
    let before = items::item(conn, id)?;
    if trashed_at(conn, id)?.is_some() {
        return Ok(Changed {
            after: before.clone(),
            before,
        });
    }
    conn.execute(
        "UPDATE planning_tasks SET trashed_at_ms = ?2, archived_at_ms = coalesce(archived_at_ms, ?2),
            revision = revision + 1, updated_at_ms = ?2
         WHERE id = ?1",
        params![id, now_ms],
    )?;
    items::record(
        conn,
        id,
        now_ms,
        actor,
        WorkActivityKind::Archived,
        None,
        Some(BIN),
    )?;
    Ok(Changed {
        before,
        after: items::item(conn, id)?,
    })
}

/// Takes a task out of the bin as it was before. One past its 30 days is
/// gone.
pub(crate) fn untrash(
    conn: &Connection,
    id: &str,
    actor: WorkActor,
    now_ms: i64,
) -> Result<Changed> {
    let before = items::item(conn, id)?;
    let Some(trashed_at_ms) = trashed_at(conn, id)? else {
        return Ok(Changed {
            after: before.clone(),
            before,
        });
    };
    if trashed_at_ms <= now_ms - BIN_MS {
        return Err(Refusal::NotFound.into());
    }
    conn.execute(
        "UPDATE planning_tasks SET archived_at_ms = CASE WHEN archived_at_ms = trashed_at_ms
                THEN NULL ELSE archived_at_ms END,
            trashed_at_ms = NULL, revision = revision + 1, updated_at_ms = ?2
         WHERE id = ?1",
        params![id, now_ms],
    )?;
    items::record(
        conn,
        id,
        now_ms,
        actor,
        WorkActivityKind::Restored,
        Some(BIN),
        None,
    )?;
    Ok(Changed {
        before,
        after: items::item(conn, id)?,
    })
}

/// Deletes for good every task in the bin for 30 days or more. Returns
/// the tasks purged.
pub(crate) fn purge(conn: &Connection, now_ms: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "DELETE FROM planning_tasks WHERE trashed_at_ms IS NOT NULL AND trashed_at_ms <= ?1
         RETURNING id",
    )?;
    let gone = statement
        .query_map([now_ms - BIN_MS], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(gone)
}

/// The tasks in the bin, most recently deleted first; none past 30 days.
pub(crate) fn binned(conn: &Connection, now_ms: i64) -> Result<Vec<Binned>> {
    let mut statement = conn.prepare(
        "SELECT id, trashed_at_ms FROM planning_tasks
         WHERE trashed_at_ms IS NOT NULL AND trashed_at_ms > ?1
         ORDER BY trashed_at_ms DESC, number DESC",
    )?;
    let found = statement
        .query_map([now_ms - BIN_MS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    found
        .into_iter()
        .map(|(id, trashed_at_ms)| {
            Ok(Binned {
                item: items::item(conn, &id)?,
                trashed_at_ms,
                purge_at_ms: trashed_at_ms + BIN_MS,
            })
        })
        .collect()
}

/// Archived tasks that are not in the bin, most recently archived first.
pub(crate) fn archived(conn: &Connection) -> Result<Vec<WorkItem>> {
    let mut statement = conn.prepare(
        "SELECT id FROM planning_tasks
         WHERE archived_at_ms IS NOT NULL AND trashed_at_ms IS NULL
         ORDER BY archived_at_ms DESC, number DESC",
    )?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter().map(|id| items::item(conn, id)).collect()
}

fn set_trashed(
    history: &mut Connection,
    change: Change<'_>,
    ids: &[String],
    trashed: bool,
) -> Result<Done<Vec<WorkItem>>> {
    let command = if trashed {
        "work_delete"
    } else {
        "work_undelete"
    };
    store::mutate(
        history,
        change.operation_id,
        command,
        &ids,
        change.at_ms,
        |tx| {
            purge(tx, change.at_ms)?;
            let mut effects = Effects::default();
            let mut value = Vec::new();
            for id in super::unique(ids)? {
                let changed = if trashed {
                    trash(tx, &id, change.actor, change.at_ms)?
                } else {
                    untrash(tx, &id, change.actor, change.at_ms)?
                };
                effects.changed(&changed);
                value.push(changed.after);
            }
            Ok(Done { value, effects })
        },
    )
}

/// Moves tasks to the bin, or out of it when `trashed` is false, made by
/// `actor`: every task moves, or none does.
pub(crate) fn set_trashed_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    ids: &[String],
    trashed: bool,
) -> Result<Vec<WorkItem>, String> {
    let change = Change::now(operation_id, actor);
    run_history(app, |history| set_trashed(history, change, ids, trashed))
}

/// The archive page: archived tasks, and the deleted ones still in the bin.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ArchiveList {
    archived: Vec<WorkItem>,
    deleted: Vec<Binned>,
}

fn read<T>(app: &AppHandle, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| work(conn))
}

pub(crate) fn work_delete(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
) -> Result<Vec<WorkItem>, String> {
    set_trashed_as(&app, WorkActor::Me, &operation_id, &ids, true)
}

pub(crate) fn work_undelete(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
) -> Result<Vec<WorkItem>, String> {
    set_trashed_as(&app, WorkActor::Me, &operation_id, &ids, false)
}

pub(crate) fn work_bin(app: AppHandle) -> Result<Vec<Binned>, String> {
    read(&app, |conn| binned(conn, now_ms()))
}

pub(crate) fn work_archive_list(app: AppHandle) -> Result<ArchiveList, String> {
    read(&app, |conn| {
        Ok(ArchiveList {
            archived: archived(conn)?,
            deleted: binned(conn, now_ms())?,
        })
    })
}
