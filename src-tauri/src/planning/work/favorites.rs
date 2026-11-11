//! Favorite tasks, projects and saved views, in the order they were added.

use super::types::WorkActor;
use super::{run_history, store, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::store::Refusal;
use anyhow::Result;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FavoriteKind {
    Task,
    Project,
    View,
}

crate::planning::types::sql_enum!(FavoriteKind {
    Task => "task",
    Project => "project",
    View => "view",
});

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Favorite {
    pub(crate) kind: FavoriteKind,
    pub(crate) target_id: String,
    pub(crate) created_at_ms: i64,
}

fn exists(conn: &Connection, kind: FavoriteKind, id: &str) -> Result<bool> {
    let sql = match kind {
        FavoriteKind::Task => {
            "SELECT EXISTS(SELECT 1 FROM planning_tasks WHERE id = ?1 AND trashed_at_ms IS NULL)"
        }
        FavoriteKind::Project => {
            "SELECT EXISTS(SELECT 1 FROM planning_projects WHERE id = ?1 AND trashed_at_ms IS NULL)"
        }
        FavoriteKind::View => "SELECT EXISTS(SELECT 1 FROM planning_views WHERE id = ?1)",
    };
    Ok(conn.query_row(sql, [id], |row| row.get(0))?)
}

/// Adds or removes one favorite. True when it changed.
pub(crate) fn set(
    conn: &Connection,
    kind: FavoriteKind,
    id: &str,
    on: bool,
    now_ms: i64,
) -> Result<bool> {
    let id = store::uuid(id)?;
    if !on {
        return Ok(conn.execute(
            "DELETE FROM planning_favorites WHERE kind = ?1 AND target_id = ?2",
            params![kind.as_str(), id],
        )? > 0);
    }
    if !exists(conn, kind, &id)? {
        return Err(Refusal::NotFound.into());
    }
    Ok(conn.execute(
        "INSERT INTO planning_favorites (kind, target_id, created_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(kind, target_id) DO NOTHING",
        params![kind.as_str(), id, now_ms],
    )? > 0)
}

pub(crate) fn favorites(conn: &Connection) -> Result<Vec<Favorite>> {
    let mut statement = conn.prepare(
        "SELECT kind, target_id, created_at_ms FROM planning_favorites
         ORDER BY created_at_ms, rowid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Favorite {
            kind: row.get(0)?,
            target_id: row.get(1)?,
            created_at_ms: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn setting(
    history: &mut Connection,
    change: Change<'_>,
    kind: FavoriteKind,
    id: &str,
    on: bool,
) -> Result<Done<bool>> {
    store::mutate(
        history,
        change.operation_id,
        "work_favorite",
        &(kind, id, on),
        change.at_ms,
        |tx| {
            Ok(Done {
                value: set(tx, kind, id, on, change.at_ms)?,
                effects: Effects::default(),
            })
        },
    )
}

/// Adds or removes a favorite. True when it changed.
pub(crate) fn set_as(
    app: &AppHandle,
    operation_id: &str,
    kind: FavoriteKind,
    id: &str,
    on: bool,
) -> Result<bool, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_history(app, |history| setting(history, change, kind, id, on))
}

pub(crate) fn work_favorite_set(
    app: AppHandle,
    operation_id: String,
    kind: FavoriteKind,
    id: String,
    on: bool,
) -> Result<bool, String> {
    set_as(&app, &operation_id, kind, &id, on)
}

pub(crate) fn work_favorites(app: AppHandle) -> Result<Vec<Favorite>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| favorites(conn))
}
