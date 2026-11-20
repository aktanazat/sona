//! What Sona closes or archives on its own, in batches: housekeeping's daily
//! run and the sub-tasks closed with their parent. Each batch is one inbox
//! notice keyed by the batch, whose summary counts every move still
//! standing, and which the user can undo as a whole. The tracker changes a
//! batch writes are marked quiet, so automations do not run on them.

use super::inbox;
use super::items;
use super::loops::LoopRefusal;
use super::types::{WorkActor, WorkPatch, WorkStatus};
use super::{edit, run_with_meetings, Change, Done, Effects};
use crate::planning::store;
use anyhow::Result;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub(crate) const BATCHES_MIGRATION: &str = "CREATE TABLE planning_sona_moves (
    batch_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('closed_stale', 'closed_subtask', 'archived_task',
        'archived_project', 'archived_cycle')),
    target_id TEXT NOT NULL,
    from_status TEXT,
    at_ms INTEGER NOT NULL,
    undone_at_ms INTEGER,
    CHECK((kind IN ('closed_stale', 'closed_subtask')) = (from_status IS NOT NULL))
);
CREATE INDEX planning_sona_moves_batch ON planning_sona_moves(batch_key);
CREATE INDEX planning_sona_moves_target ON planning_sona_moves(target_id, kind);
CREATE TABLE planning_quiet_changes (
    first_seq INTEGER NOT NULL,
    last_seq INTEGER NOT NULL,
    CHECK(first_seq <= last_seq)
);";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MoveKind {
    ClosedStale,
    ClosedSubtask,
    ArchivedTask,
    ArchivedProject,
    ArchivedCycle,
}

crate::planning::types::sql_enum!(MoveKind {
    ClosedStale => "closed_stale",
    ClosedSubtask => "closed_subtask",
    ArchivedTask => "archived_task",
    ArchivedProject => "archived_project",
    ArchivedCycle => "archived_cycle",
});

/// What undoing a batch did.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUndo {
    /// Moves put back.
    pub restored: u32,
    /// Moves left alone because the user changed that work since.
    pub skipped: u32,
}

/// Records that Sona moved `target_id` as part of `batch_key`.
pub(crate) fn record(
    conn: &Connection,
    batch_key: &str,
    kind: MoveKind,
    target_id: &str,
    from_status: Option<WorkStatus>,
    at_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_sona_moves (batch_key, kind, target_id, from_status, at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            batch_key,
            kind.as_str(),
            target_id,
            from_status.map(|status| status.as_str()),
            at_ms
        ],
    )?;
    Ok(())
}

/// The newest tracker change, to mark what follows quiet with [`quiet_since`].
pub(crate) fn last_change(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM planning_changes",
        [],
        |row| row.get(0),
    )?)
}

/// Marks every tracker change after `seq` quiet: Sona's own batch wrote it,
/// so automations pass over it.
pub(crate) fn quiet_since(conn: &Connection, seq: i64) -> Result<()> {
    let last = last_change(conn)?;
    if last > seq {
        conn.execute(
            "DELETE FROM planning_quiet_changes
             WHERE last_seq < (SELECT COALESCE(MIN(seq), 0) FROM planning_changes)",
            [],
        )?;
        conn.execute(
            "INSERT INTO planning_quiet_changes (first_seq, last_seq) VALUES (?1, ?2)",
            params![seq + 1, last],
        )?;
    }
    Ok(())
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What the moves of `batch_key` still standing add up to, as what Sona
/// did: "closed 2 stale tasks; archived 1 project".
fn summary(conn: &Connection, batch_key: &str) -> Result<String> {
    let mut statement = conn.prepare(
        "SELECT m.kind, t.title FROM planning_sona_moves m
         LEFT JOIN planning_tasks t ON t.id = m.target_id
         WHERE m.batch_key = ?1 AND m.undone_at_ms IS NULL
         ORDER BY m.rowid",
    )?;
    let moves = statement
        .query_map([batch_key], |row| {
            Ok((row.get::<_, MoveKind>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let count = |kind: MoveKind| moves.iter().filter(|(k, _)| *k == kind).count();
    let mut parts = Vec::new();
    let stale = count(MoveKind::ClosedStale);
    if stale > 0 {
        parts.push(format!("closed {}", plural(stale, "stale task", "stale tasks")));
    }
    let sub_tasks: Vec<&str> = moves
        .iter()
        .filter(|(kind, _)| *kind == MoveKind::ClosedSubtask)
        .map(|(_, title)| title.as_deref().unwrap_or("a deleted task"))
        .collect();
    if !sub_tasks.is_empty() {
        parts.push(format!(
            "canceled {}: {}",
            plural(sub_tasks.len(), "open sub-task", "open sub-tasks"),
            sub_tasks.join(", ")
        ));
    }
    let mut archived = Vec::new();
    for (kind, one, many) in [
        (MoveKind::ArchivedTask, "task", "tasks"),
        (MoveKind::ArchivedProject, "project", "projects"),
        (MoveKind::ArchivedCycle, "cycle", "cycles"),
    ] {
        let n = count(kind);
        if n > 0 {
            archived.push(plural(n, one, many));
        }
    }
    if !archived.is_empty() {
        parts.push(format!("archived {}", archived.join(", ")));
    }
    Ok(parts.join("; "))
}

/// Posts the notice for `batch_key`, or brings the one already there up
/// to date and unread again, so a day's runs add up in one notice.
pub(crate) fn post(
    conn: &Connection,
    task_id: Option<&str>,
    batch_key: &str,
    now_ms: i64,
) -> Result<()> {
    let text = summary(conn, batch_key)?;
    inbox::batch_posted(conn, task_id, batch_key, &text, now_ms)
}

/// Puts back what the batch behind `notice_id` closed or archived, where
/// the work still stands as Sona left it, in the name of `change`'s actor.
pub(super) fn undo(
    history: &Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    notice_id: &str,
    effects: &mut Effects,
) -> Result<BatchUndo> {
    let Some(batch_key) = inbox::batch_key(history, notice_id)? else {
        return Err(LoopRefusal("This notice has nothing to undo.".to_owned()).into());
    };
    let mut statement = history.prepare(
        "SELECT rowid, kind, target_id, from_status FROM planning_sona_moves
         WHERE batch_key = ?1 AND undone_at_ms IS NULL ORDER BY rowid",
    )?;
    let moves = statement
        .query_map([&batch_key], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, MoveKind>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<WorkStatus>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if moves.is_empty() {
        return Err(LoopRefusal("Everything here was already undone.".to_owned()).into());
    }
    let mut result = BatchUndo::default();
    for (rowid, kind, target_id, from_status) in moves {
        let restored = match (kind, from_status) {
            (MoveKind::ClosedStale | MoveKind::ClosedSubtask, Some(from_status)) => {
                let canceled = task_holds(history, &target_id, "status = 'canceled'")?;
                if canceled {
                    let patch = WorkPatch {
                        status: Some(from_status),
                        ..WorkPatch::default()
                    };
                    edit(history, meetings, change, &target_id, &patch, effects)?;
                }
                canceled
            }
            (MoveKind::ArchivedTask, _) => {
                let archived = task_holds(history, &target_id, "archived_at_ms IS NOT NULL")?;
                if archived {
                    let changed = items::set_archived(
                        history,
                        &target_id,
                        false,
                        change.actor,
                        change.at_ms,
                    )?;
                    effects.changed(&changed);
                }
                archived
            }
            (MoveKind::ArchivedProject, _) => {
                let restored = history.execute(
                    "UPDATE planning_projects SET archived_at_ms = NULL, updated_at_ms = ?2
                     WHERE id = ?1 AND archived_at_ms IS NOT NULL",
                    params![target_id, change.at_ms],
                )? > 0;
                if restored && !effects.projects.contains(&target_id) {
                    effects.projects.push(target_id.clone());
                }
                restored
            }
            (MoveKind::ArchivedCycle, _) => {
                history.execute(
                    "UPDATE planning_cycles SET archived_at_ms = NULL
                     WHERE CAST(number AS TEXT) = ?1 AND archived_at_ms IS NOT NULL",
                    [&target_id],
                )? > 0
            }
            (MoveKind::ClosedStale | MoveKind::ClosedSubtask, None) => false,
        };
        if restored {
            result.restored += 1;
        } else {
            result.skipped += 1;
        }
        history.execute(
            "UPDATE planning_sona_moves SET undone_at_ms = ?2 WHERE rowid = ?1",
            params![rowid, change.at_ms],
        )?;
    }
    inbox::batch_handled(history, &batch_key, change.at_ms)?;
    Ok(result)
}

/// Whether the live task `id` meets `condition`, a test on its own columns.
fn task_holds(conn: &Connection, id: &str, condition: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {condition} FROM planning_tasks WHERE id = ?1 AND trashed_at_ms IS NULL"
            ),
            [id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// [`undo`] without the conversations, for a dry run the caller rolls back.
pub(crate) fn rehearse_undo(conn: &Connection, notice_id: &str, now_ms: i64) -> Result<BatchUndo> {
    let change = Change {
        operation_id: "",
        actor: WorkActor::Me,
        at_ms: now_ms,
    };
    undo(conn, None, change, notice_id, &mut Effects::default())
}

/// [`undo`] for the app, the command line and agents, as the user's change.
pub(crate) async fn undo_as(
    app: &tauri::AppHandle,
    operation_id: &str,
    notice_id: &str,
) -> Result<BatchUndo, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_with_meetings(app, |history, meetings| {
        store::mutate(
            history,
            operation_id,
            "work_housekeeping_undo",
            &notice_id,
            change.at_ms,
            |tx| {
                let mut effects = Effects::default();
                let value = undo(tx, meetings, change, notice_id, &mut effects)?;
                Ok(Done { value, effects })
            },
        )
    })
    .await
}
