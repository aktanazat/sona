//! Project updates: a health and a note on how a project is going, newest
//! first, and a plain draft of what moved since the last one.

use super::progress;
use super::store::{self, Refusal};
use super::types::{
    ProjectHealth, ProjectUpdate, ProjectUpdateAuthor, ProjectUpdatePage, TaskStatus,
};
use super::work::comments::Author;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::fmt::Write as _;
use uuid::Uuid;

pub(crate) const PAGE: usize = 20;
pub(crate) const MAX_PAGE: usize = 100;
/// The draft names progress only once it moved more than this many points.
const PROGRESS_STEP: u8 = 2;

const COLUMNS: &str = "id, project_id, health, body, progress, author_kind, author_name,
    created_at_ms, edited_at_ms, deleted_at_ms";

fn read(row: &Row<'_>) -> rusqlite::Result<ProjectUpdate> {
    let deleted = row.get::<_, Option<i64>>(9)?.is_some();
    Ok(ProjectUpdate {
        id: row.get(0)?,
        project_id: row.get(1)?,
        health: row.get(2)?,
        body: if deleted { None } else { Some(row.get(3)?) },
        progress: row.get(4)?,
        author: ProjectUpdateAuthor {
            kind: row.get(5)?,
            name: row.get(6)?,
        },
        created_at_ms: row.get(7)?,
        edited_at_ms: row.get(8)?,
        deleted,
    })
}

pub(crate) fn update(conn: &Connection, id: &str) -> Result<ProjectUpdate> {
    let id = store::uuid(id)?;
    let found = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM planning_project_updates WHERE id = ?1"),
            [&id],
            read,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    if found.deleted {
        return Err(Refusal::NotFound.into());
    }
    Ok(found)
}

fn wrote(author: Author<'_>, update: &ProjectUpdate) -> bool {
    update.author.kind == author.actor
        && match (author.name(), update.author.name.as_deref()) {
            (Some(mine), Some(theirs)) => mine.eq_ignore_ascii_case(theirs),
            (None, None) => true,
            _ => false,
        }
}

fn clean_body(body: &str) -> Result<String> {
    let body = body.trim();
    if body.is_empty() {
        return Err(Refusal::Invalid.into());
    }
    Ok(body.to_owned())
}

fn touch(conn: &Connection, project_id: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE planning_projects SET updated_at_ms = ?2 WHERE id = ?1",
        params![project_id, now_ms],
    )?;
    Ok(())
}

pub(crate) fn add(
    conn: &Connection,
    author: Author<'_>,
    project_id: &str,
    health: ProjectHealth,
    body: &str,
    now_ms: i64,
) -> Result<ProjectUpdate> {
    let project = store::project(conn, &store::uuid(project_id)?)?;
    if project.trashed_at_ms.is_some() {
        return Err(Refusal::NotFound.into());
    }
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_project_updates
            (id, project_id, health, body, progress, author_kind, author_name, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            project.id,
            health.as_str(),
            clean_body(body)?,
            progress::percent_now(conn, &project.id)?,
            author.actor.as_str(),
            author.name(),
            now_ms
        ],
    )?;
    touch(conn, &project.id, now_ms)?;
    update(conn, &id)
}

pub(crate) fn edit(
    conn: &Connection,
    author: Author<'_>,
    id: &str,
    health: Option<ProjectHealth>,
    body: Option<&str>,
    now_ms: i64,
) -> Result<ProjectUpdate> {
    let current = update(conn, id)?;
    if !wrote(author, &current) {
        return Err(Refusal::NotAuthor.into());
    }
    let body = match body {
        Some(body) => clean_body(body)?,
        None => current.body.clone().unwrap_or_default(),
    };
    let health = health.unwrap_or(current.health);
    if Some(&body) == current.body.as_ref() && health == current.health {
        return Ok(current);
    }
    conn.execute(
        "UPDATE planning_project_updates SET health = ?2, body = ?3, edited_at_ms = ?4 WHERE id = ?1",
        params![current.id, health.as_str(), body, now_ms],
    )?;
    touch(conn, &current.project_id, now_ms)?;
    update(conn, &current.id)
}

/// Blanks the update and keeps its row; it no longer lists.
pub(crate) fn delete(
    conn: &Connection,
    author: Author<'_>,
    id: &str,
    now_ms: i64,
) -> Result<ProjectUpdate> {
    let current = update(conn, id)?;
    if !wrote(author, &current) {
        return Err(Refusal::NotAuthor.into());
    }
    conn.execute(
        "UPDATE planning_project_updates SET body = '', deleted_at_ms = ?2 WHERE id = ?1",
        params![current.id, now_ms],
    )?;
    touch(conn, &current.project_id, now_ms)?;
    Ok(ProjectUpdate {
        body: None,
        deleted: true,
        ..current
    })
}

pub(crate) fn latest(conn: &Connection, project_id: &str) -> Result<Option<ProjectUpdate>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM planning_project_updates
                 WHERE project_id = ?1 AND deleted_at_ms IS NULL
                 ORDER BY created_at_ms DESC, id DESC LIMIT 1"
            ),
            [project_id],
            read,
        )
        .optional()?)
}

/// Updates newest first; `cursor` is the last update of the page before.
pub(crate) fn page(
    conn: &Connection,
    project_id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<ProjectUpdatePage> {
    let project_id = store::uuid(project_id)?;
    store::project(conn, &project_id)?;
    let limit = limit.clamp(1, MAX_PAGE);
    let after = match cursor {
        Some(cursor) => {
            let found = update(conn, cursor)?;
            if found.project_id != project_id {
                return Err(Refusal::NotFound.into());
            }
            Some((found.created_at_ms, found.id))
        }
        None => None,
    };
    let (at, id) = after.unwrap_or((i64::MAX, String::new()));
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM planning_project_updates
         WHERE project_id = ?1 AND deleted_at_ms IS NULL
           AND (?4 = '' OR created_at_ms < ?2 OR (created_at_ms = ?2 AND id < ?4))
         ORDER BY created_at_ms DESC, id DESC LIMIT ?3"
    ))?;
    let mut updates = statement
        .query_map(params![project_id, at, i64::try_from(limit + 1)?, id], read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if updates.len() > limit {
        updates.truncate(limit);
        updates.last().map(|update| update.id.clone())
    } else {
        None
    };
    Ok(ProjectUpdatePage {
        updates,
        next_cursor,
    })
}

fn moved(conn: &Connection, project_id: &str, since_ms: i64, to: &str) -> Result<u32> {
    Ok(conn.query_row(
        "SELECT count(DISTINCT a.task_id) FROM planning_task_activity a
         JOIN planning_tasks t ON t.id = a.task_id
         WHERE t.project_id = ?1 AND t.archived_at_ms IS NULL AND a.kind = 'status'
           AND a.to_value = ?3 AND a.at_ms > ?2 AND t.status = ?3",
        params![project_id, since_ms, to],
        |row| row.get(0),
    )?)
}

fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// A plain Markdown draft of what changed since the last update, or since
/// the project began: tasks finished, started and added, milestones
/// reached, the one in focus, and progress once it moved past two points.
pub(crate) fn draft(conn: &Connection, project_id: &str) -> Result<String> {
    let project = store::project(conn, &store::uuid(project_id)?)?;
    let last = latest(conn, &project.id)?;
    let since_ms = last
        .as_ref()
        .map_or(project.created_at_ms, |update| update.created_at_ms);
    let done = moved(conn, &project.id, since_ms, "done")?;
    let started = moved(conn, &project.id, since_ms, "in_progress")?;
    let added: u32 = conn.query_row(
        "SELECT count(*) FROM planning_tasks
         WHERE project_id = ?1 AND archived_at_ms IS NULL AND created_at_ms > ?2",
        params![project.id, since_ms],
        |row| row.get(0),
    )?;
    let (milestones, progress) = progress::milestone_progress(conn, &project.id)?;
    let mut text = String::new();
    let tasks = [
        (done, "task finished", "tasks finished"),
        (started, "task started", "tasks started"),
        (added, "task added", "tasks added"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, one, many)| plural(count, one, many))
    .collect::<Vec<_>>();
    if tasks.is_empty() {
        text.push_str("- No task moved");
    } else {
        write!(text, "- {}", tasks.join(", "))?;
    }
    if last.is_some() {
        text.push_str(" since the last update.\n");
    } else {
        text.push_str(" so far.\n");
    }
    for (milestone, counts) in milestones.iter().zip(&progress) {
        let reached = milestone.updated_at_ms > since_ms
            && (milestone.status == TaskStatus::Done
                || (counts.scope > 0 && counts.done == counts.scope));
        if reached {
            writeln!(text, "- Milestone reached: {}.", milestone.title)?;
        }
        if counts.next_open {
            writeln!(
                text,
                "- In focus: {}, {}% complete.",
                milestone.title, counts.percent
            )?;
        }
    }
    let now = progress::percent_now(conn, &project.id)?;
    let before = last.as_ref().map_or(0, |update| update.progress);
    if now.abs_diff(before) > PROGRESS_STEP {
        writeln!(text, "- Progress: {before}% to {now}%.")?;
    }
    Ok(text.trim_end().to_owned())
}
