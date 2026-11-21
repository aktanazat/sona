//! Initiatives: goals one level above projects. An initiative holds
//! projects, and a project sits in at most one initiative.

use super::super::store::{self, Refusal};
use super::super::types::{ProjectHealth, ProjectUpdateAuthor};
use super::comments::Author;
use super::types::{nullable, WorkOwner};
use super::{now_ms, run_history, Done, Effects};
use crate::managers::history::HistoryManager;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// Initiatives and the projects they hold. A project's place is keyed by
/// the project, so it sits in one initiative at most; deleting either side
/// drops the place. A project joining, moving or leaving reaches the change
/// stream as a change to `initiative` on the project, with the initiative it
/// had before.
pub(crate) const INITIATIVES_MIGRATION: &str = "CREATE TABLE planning_initiatives (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    description TEXT,
    owner_person_id TEXT,
    owner_is_self INTEGER NOT NULL DEFAULT 0 CHECK(owner_is_self IN (0, 1)),
    target_at_ms INTEGER,
    status TEXT NOT NULL DEFAULT 'planned' CHECK(status IN ('planned', 'active', 'completed')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK(NOT (owner_is_self = 1 AND owner_person_id IS NOT NULL))
);
CREATE UNIQUE INDEX planning_initiatives_name ON planning_initiatives(lower(name));
CREATE TABLE planning_initiative_projects (
    project_id TEXT PRIMARY KEY NOT NULL,
    initiative_id TEXT NOT NULL,
    added_at_ms INTEGER NOT NULL
);
CREATE INDEX planning_initiative_projects_initiative
    ON planning_initiative_projects(initiative_id);
CREATE TRIGGER planning_initiative_projects_project_delete AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_initiative_projects WHERE project_id = old.id;
END;
CREATE TRIGGER planning_initiative_projects_initiative_delete AFTER DELETE ON planning_initiatives
BEGIN
    DELETE FROM planning_initiative_projects WHERE initiative_id = old.id;
END;
CREATE TRIGGER planning_changes_initiative_project_add AFTER INSERT ON planning_initiative_projects
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', p.id, 'update',
        json_object('initiative', NULL), json_object('title', p.title)
    FROM planning_projects p WHERE p.id = new.project_id;
END;
CREATE TRIGGER planning_changes_initiative_project_move AFTER UPDATE ON planning_initiative_projects
WHEN old.initiative_id IS NOT new.initiative_id
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', p.id, 'update',
        json_object('initiative', old.initiative_id), json_object('title', p.title)
    FROM planning_projects p WHERE p.id = new.project_id;
END;
CREATE TRIGGER planning_changes_initiative_project_remove AFTER DELETE ON planning_initiative_projects
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', p.id, 'update',
        json_object('initiative', old.initiative_id), json_object('title', p.title)
    FROM planning_projects p WHERE p.id = old.project_id;
END;";

/// Initiatives nest under a parent, carry a priority and project labels,
/// take Linear's six statuses and keep updates with a health, as projects
/// do. Deleting a parent leaves its children at the top. The table is
/// rebuilt for the wider status, so the triggers on it are made again.
pub(crate) const INITIATIVE_DETAILS_MIGRATION: &str = "PRAGMA legacy_alter_table = ON;
CREATE TABLE planning_initiatives_next (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    description TEXT,
    owner_person_id TEXT,
    owner_is_self INTEGER NOT NULL DEFAULT 0 CHECK(owner_is_self IN (0, 1)),
    target_at_ms INTEGER,
    status TEXT NOT NULL DEFAULT 'planned'
        CHECK(status IN ('backlog', 'planned', 'active', 'paused', 'completed', 'canceled')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    parent_id TEXT CHECK(parent_id IS NULL OR parent_id != id),
    priority INTEGER NOT NULL DEFAULT 0 CHECK(priority BETWEEN 0 AND 4),
    CHECK(NOT (owner_is_self = 1 AND owner_person_id IS NOT NULL))
);
INSERT INTO planning_initiatives_next (id, name, description, owner_person_id, owner_is_self,
        target_at_ms, status, created_at_ms, updated_at_ms)
    SELECT id, name, description, owner_person_id, owner_is_self, target_at_ms, status,
        created_at_ms, updated_at_ms
    FROM planning_initiatives;
DROP TABLE planning_initiatives;
ALTER TABLE planning_initiatives_next RENAME TO planning_initiatives;
PRAGMA legacy_alter_table = OFF;
CREATE UNIQUE INDEX planning_initiatives_name ON planning_initiatives(lower(name));
CREATE INDEX planning_initiatives_parent ON planning_initiatives(parent_id);
CREATE TABLE planning_initiative_labels (
    initiative_id TEXT NOT NULL,
    label_id TEXT NOT NULL,
    PRIMARY KEY (initiative_id, label_id)
);
CREATE INDEX planning_initiative_labels_label ON planning_initiative_labels(label_id);
CREATE TRIGGER planning_initiative_labels_label_delete AFTER DELETE ON planning_labels BEGIN
    DELETE FROM planning_initiative_labels WHERE label_id = old.id;
END;
CREATE TABLE planning_initiative_updates (
    id TEXT PRIMARY KEY NOT NULL,
    initiative_id TEXT NOT NULL,
    health TEXT NOT NULL CHECK(health IN ('onTrack', 'atRisk', 'offTrack')),
    body TEXT NOT NULL,
    progress INTEGER NOT NULL CHECK(progress BETWEEN 0 AND 100),
    author_kind TEXT NOT NULL CHECK(author_kind IN ('me', 'sona', 'agent')),
    author_name TEXT CHECK(author_name IS NULL OR length(trim(author_name)) > 0),
    created_at_ms INTEGER NOT NULL,
    edited_at_ms INTEGER,
    deleted_at_ms INTEGER,
    CHECK(deleted_at_ms IS NOT NULL OR length(trim(body)) > 0)
);
CREATE INDEX planning_initiative_updates_initiative
    ON planning_initiative_updates(initiative_id, created_at_ms, id);
CREATE TRIGGER planning_initiative_projects_initiative_delete AFTER DELETE ON planning_initiatives
BEGIN
    DELETE FROM planning_initiative_projects WHERE initiative_id = old.id;
    DELETE FROM planning_initiative_labels WHERE initiative_id = old.id;
    DELETE FROM planning_initiative_updates WHERE initiative_id = old.id;
    UPDATE planning_initiatives SET parent_id = NULL WHERE parent_id = old.id;
END;
CREATE TRIGGER planning_project_activity_initiative_delete BEFORE DELETE ON planning_initiatives
BEGIN
    INSERT INTO planning_project_activity (id, project_id, kind, from_value, at_ms)
    SELECT lower(hex(randomblob(16))), p.id, 'initiative', old.name,
        CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
    FROM planning_initiative_projects ip JOIN planning_projects p ON p.id = ip.project_id
    WHERE ip.initiative_id = old.id;
END;
CREATE TRIGGER planning_changes_initiative_create AFTER INSERT ON planning_initiatives BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'initiative', new.id,
        'create', json_object('name', new.name, 'status', new.status));
END;
CREATE TRIGGER planning_changes_initiative_remove AFTER DELETE ON planning_initiatives BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'initiative', old.id,
        'remove', json_object('name', old.name));
END;
CREATE TRIGGER planning_changes_initiative_update AFTER UPDATE ON planning_initiatives
WHEN old.name IS NOT new.name OR old.description IS NOT new.description
    OR old.owner_person_id IS NOT new.owner_person_id OR old.owner_is_self IS NOT new.owner_is_self
    OR old.target_at_ms IS NOT new.target_at_ms OR old.status IS NOT new.status
    OR old.parent_id IS NOT new.parent_id OR old.priority IS NOT new.priority
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'initiative', new.id,
        'update', json_group_object(field, value), json_object('name', new.name)
    FROM (
        SELECT 'name' AS field, old.name AS value WHERE old.name IS NOT new.name
        UNION ALL SELECT 'description', old.description
            WHERE old.description IS NOT new.description
        UNION ALL SELECT 'owner', CASE WHEN old.owner_is_self THEN 'me' ELSE old.owner_person_id END
            WHERE old.owner_person_id IS NOT new.owner_person_id
               OR old.owner_is_self IS NOT new.owner_is_self
        UNION ALL SELECT 'target_at_ms', old.target_at_ms
            WHERE old.target_at_ms IS NOT new.target_at_ms
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'parent', old.parent_id WHERE old.parent_id IS NOT new.parent_id
        UNION ALL SELECT 'priority', old.priority WHERE old.priority IS NOT new.priority
    );
END;";

/// Where an initiative stands, as Linear names it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InitiativeStatus {
    Backlog,
    #[default]
    Planned,
    Active,
    Paused,
    Completed,
    Canceled,
}

impl InitiativeStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Planned => "planned",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "backlog" => Some(Self::Backlog),
            "planned" => Some(Self::Planned),
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }
}

/// How far an initiative's projects have come: its projects outside the
/// archive and the bin, how many are completed, and their open and done
/// tasks.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct InitiativeProgress {
    pub projects: u32,
    pub completed_projects: u32,
    pub open_tasks: u32,
    pub done_tasks: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Initiative {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_person_id: Option<String>,
    pub owner_is_self: bool,
    pub target_at_ms: Option<i64>,
    pub status: InitiativeStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// The initiative it sits under, when it is nested.
    pub parent_id: Option<String>,
    /// 0 none, 1 urgent, 2 high, 3 medium, 4 low, as tasks count it.
    pub priority: u8,
    pub label_ids: Vec<String>,
    /// Its projects outside the archive and the bin, by name.
    pub project_ids: Vec<String>,
    /// The initiatives directly under it, by name.
    pub child_ids: Vec<String>,
    pub progress: InitiativeProgress,
    pub latest_update: Option<InitiativeUpdate>,
}

/// A note on how an initiative is going, with its health and the share of
/// its projects' tasks done when it was written.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct InitiativeUpdate {
    pub id: String,
    pub initiative_id: String,
    pub health: ProjectHealth,
    /// Markdown; none once deleted.
    pub body: Option<String>,
    pub progress: u8,
    pub author: ProjectUpdateAuthor,
    pub created_at_ms: i64,
    pub edited_at_ms: Option<i64>,
    pub deleted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct InitiativeUpdatePage {
    pub updates: Vec<InitiativeUpdate>,
    pub next_cursor: Option<String>,
}

/// An initiative's fields to set. A key left out keeps its value; `null`
/// clears the description, the target date or the parent, and the owner
/// `nobody` clears the owner.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct InitiativeSaveInput {
    pub name: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub description: Option<Option<String>>,
    pub owner: Option<WorkOwner>,
    #[serde(deserialize_with = "nullable")]
    pub target_at_ms: Option<Option<i64>>,
    pub status: Option<InitiativeStatus>,
    #[serde(deserialize_with = "nullable")]
    pub parent_id: Option<Option<String>>,
    pub priority: Option<u8>,
    pub label_ids: Option<Vec<String>>,
}

const COLUMNS: &str = "id, name, description, owner_person_id, owner_is_self, target_at_ms, status,
    created_at_ms, updated_at_ms, parent_id, priority FROM planning_initiatives";

fn read(row: &Row<'_>) -> rusqlite::Result<Initiative> {
    let status: String = row.get(6)?;
    Ok(Initiative {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        owner_person_id: row.get(3)?,
        owner_is_self: row.get(4)?,
        target_at_ms: row.get(5)?,
        status: InitiativeStatus::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                format!("unknown initiative status {status}").into(),
            )
        })?,
        created_at_ms: row.get(7)?,
        updated_at_ms: row.get(8)?,
        parent_id: row.get(9)?,
        priority: row.get(10)?,
        label_ids: Vec::new(),
        project_ids: Vec::new(),
        child_ids: Vec::new(),
        progress: InitiativeProgress::default(),
        latest_update: None,
    })
}

/// Fills in the initiative's labels, children, latest update, live
/// projects and their progress.
fn attach_projects(conn: &Connection, initiative: &mut Initiative) -> Result<()> {
    let mut labels = conn.prepare_cached(
        "SELECT il.label_id FROM planning_initiative_labels il
         JOIN planning_labels l ON l.id = il.label_id
         WHERE il.initiative_id = ?1 ORDER BY lower(l.name), l.id",
    )?;
    initiative.label_ids = labels
        .query_map([&initiative.id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut children = conn.prepare_cached(
        "SELECT id FROM planning_initiatives WHERE parent_id = ?1 ORDER BY lower(name), id",
    )?;
    initiative.child_ids = children
        .query_map([&initiative.id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    initiative.latest_update = latest_update(conn, &initiative.id)?;
    let mut statement = conn.prepare(
        "SELECT p.id, p.status = 'completed',
            (SELECT count(*) FROM planning_tasks t WHERE t.project_id = p.id
                AND t.archived_at_ms IS NULL AND t.status IN ('backlog', 'not_started', 'in_progress')),
            (SELECT count(*) FROM planning_tasks t WHERE t.project_id = p.id
                AND t.archived_at_ms IS NULL AND t.status = 'done')
         FROM planning_initiative_projects i JOIN planning_projects p ON p.id = i.project_id
         WHERE i.initiative_id = ?1 AND p.archived_at_ms IS NULL AND p.trashed_at_ms IS NULL
         ORDER BY lower(p.title), p.id",
    )?;
    let rows = statement.query_map([&initiative.id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, bool>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, u32>(3)?,
        ))
    })?;
    let mut progress = InitiativeProgress::default();
    let mut ids = Vec::new();
    for row in rows {
        let (id, completed, open, done) = row?;
        ids.push(id);
        progress.projects += 1;
        progress.completed_projects += u32::from(completed);
        progress.open_tasks += open;
        progress.done_tasks += done;
    }
    initiative.project_ids = ids;
    initiative.progress = progress;
    Ok(())
}

pub(crate) fn initiative(conn: &Connection, id: &str) -> Result<Initiative> {
    let mut initiative = conn
        .query_row(&format!("SELECT {COLUMNS} WHERE id = ?1"), [id], read)
        .optional()?
        .ok_or(Refusal::NotFound)?;
    attach_projects(conn, &mut initiative)?;
    Ok(initiative)
}

/// Every initiative, by name, with its projects and their progress.
pub(crate) fn initiatives(conn: &Connection) -> Result<Vec<Initiative>> {
    let mut statement = conn.prepare(&format!("SELECT {COLUMNS} ORDER BY lower(name), id"))?;
    let mut all = statement
        .query_map([], read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for initiative in &mut all {
        attach_projects(conn, initiative)?;
    }
    Ok(all)
}

/// The initiative a project sits in, archived or binned projects included.
pub(crate) fn initiative_of(conn: &Connection, project_id: &str) -> Result<Option<Initiative>> {
    conn.query_row(
        "SELECT initiative_id FROM planning_initiative_projects WHERE project_id = ?1",
        [project_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|id| initiative(conn, &id))
    .transpose()
}

fn present(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Whether another initiative already goes by `name`, ignoring case.
pub(crate) fn name_taken(conn: &Connection, name: &str, other_than: Option<&str>) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_initiatives WHERE lower(name) = lower(?1) AND id != ?2)",
        params![name.trim(), other_than.unwrap_or("")],
        |row| row.get(0),
    )?)
}

/// Makes an initiative, or changes the one `id` names. Refused without a
/// name, or with a name another initiative has.
pub(crate) fn save(
    conn: &Connection,
    id: Option<&str>,
    input: &InitiativeSaveInput,
    now_ms: i64,
) -> Result<Initiative> {
    let current = id
        .map(|id| initiative(conn, &store::uuid(id)?))
        .transpose()?;
    let name = match (&input.name, &current) {
        (Some(name), _) => present(name).ok_or(Refusal::Invalid)?,
        (None, Some(current)) => current.name.clone(),
        (None, None) => return Err(Refusal::Invalid.into()),
    };
    if name_taken(conn, &name, current.as_ref().map(|c| c.id.as_str()))? {
        return Err(Refusal::Invalid.into());
    }
    let description = match &input.description {
        Some(text) => text.as_deref().and_then(present),
        None => current.as_ref().and_then(|c| c.description.clone()),
    };
    let (owner_is_self, owner_person_id) = match &input.owner {
        Some(WorkOwner::Myself) => (true, None),
        Some(WorkOwner::Person { person_id }) => (false, present(person_id)),
        Some(WorkOwner::Nobody) => (false, None),
        None => current
            .as_ref()
            .map_or((false, None), |c| (c.owner_is_self, c.owner_person_id.clone())),
    };
    let target = input
        .target_at_ms
        .unwrap_or_else(|| current.as_ref().and_then(|c| c.target_at_ms));
    let status = input
        .status
        .or_else(|| current.as_ref().map(|c| c.status))
        .unwrap_or_default();
    let parent_id = match &input.parent_id {
        Some(Some(parent)) => Some(initiative(conn, &store::uuid(parent)?)?.id),
        Some(None) => None,
        None => current.as_ref().and_then(|c| c.parent_id.clone()),
    };
    if let (Some(parent), Some(current)) = (&parent_id, &current) {
        if ancestors(conn, parent)?.contains(&current.id) || *parent == current.id {
            return Err(Refusal::ParentLoop.into());
        }
    }
    let priority = input
        .priority
        .or_else(|| current.as_ref().map(|c| c.priority))
        .unwrap_or(0);
    if priority > 4 {
        return Err(Refusal::Invalid.into());
    }
    let id = match current {
        Some(current) => {
            conn.execute(
                "UPDATE planning_initiatives
                    SET name = ?2, description = ?3, owner_person_id = ?4, owner_is_self = ?5,
                        target_at_ms = ?6, status = ?7, updated_at_ms = ?8, parent_id = ?9,
                        priority = ?10
                  WHERE id = ?1",
                params![
                    current.id,
                    name,
                    description,
                    owner_person_id,
                    owner_is_self,
                    target,
                    status.as_str(),
                    now_ms,
                    parent_id,
                    priority
                ],
            )?;
            current.id
        }
        None => {
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO planning_initiatives (id, name, description, owner_person_id,
                    owner_is_self, target_at_ms, status, created_at_ms, updated_at_ms, parent_id,
                    priority)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10)",
                params![
                    id,
                    name,
                    description,
                    owner_person_id,
                    owner_is_self,
                    target,
                    status.as_str(),
                    now_ms,
                    parent_id,
                    priority
                ],
            )?;
            id
        }
    };
    if let Some(labels) = &input.label_ids {
        write_labels(conn, &id, labels)?;
    }
    initiative(conn, &id)
}

/// The initiatives above `id`, nearest first.
fn ancestors(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare_cached(
        "WITH RECURSIVE up(id, depth) AS (
            SELECT parent_id, 1 FROM planning_initiatives WHERE id = ?1 AND parent_id IS NOT NULL
            UNION ALL
            SELECT i.parent_id, up.depth + 1 FROM planning_initiatives i JOIN up ON i.id = up.id
            WHERE i.parent_id IS NOT NULL AND up.depth < 64
         )
         SELECT id FROM up ORDER BY depth",
    )?;
    let ids = statement
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Sets an initiative's labels. A label kept for tasks only is refused:
/// initiatives take the labels projects take.
fn write_labels(conn: &Connection, id: &str, label_ids: &[String]) -> Result<()> {
    conn.execute(
        "DELETE FROM planning_initiative_labels WHERE initiative_id = ?1",
        [id],
    )?;
    for label_id in label_ids {
        let label_id = store::uuid(label_id)?;
        let scope: Option<String> = conn
            .query_row(
                "SELECT scope FROM planning_labels WHERE id = ?1",
                [&label_id],
                |row| row.get(0),
            )
            .optional()?;
        match scope.as_deref() {
            None => return Err(Refusal::NotFound.into()),
            Some("tasks") => return Err(Refusal::LabelScope.into()),
            Some(_) => {}
        }
        conn.execute(
            "INSERT OR IGNORE INTO planning_initiative_labels (initiative_id, label_id)
             VALUES (?1, ?2)",
            params![id, label_id],
        )?;
    }
    Ok(())
}

/// Deletes an initiative and hands back what it was. Its projects stay,
/// in no initiative.
pub(crate) fn delete(conn: &Connection, id: &str) -> Result<Initiative> {
    let found = initiative(conn, &store::uuid(id)?)?;
    conn.execute("DELETE FROM planning_initiatives WHERE id = ?1", [&found.id])?;
    Ok(found)
}

/// Puts a project in an initiative, moving it out of any other.
pub(crate) fn add_project(
    conn: &Connection,
    initiative_id: &str,
    project_id: &str,
    now_ms: i64,
) -> Result<Initiative> {
    let target = initiative(conn, &store::uuid(initiative_id)?)?;
    let project = store::project(conn, &store::uuid(project_id)?)?;
    if project.trashed_at_ms.is_some() {
        return Err(Refusal::NotFound.into());
    }
    conn.execute(
        "INSERT INTO planning_initiative_projects (project_id, initiative_id, added_at_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(project_id) DO UPDATE SET initiative_id = excluded.initiative_id,
             added_at_ms = excluded.added_at_ms
         WHERE initiative_id != excluded.initiative_id",
        params![project.id, target.id, now_ms],
    )?;
    initiative(conn, &target.id)
}

/// Takes a project out of its initiative and hands back the initiative it
/// left.
pub(crate) fn remove_project(conn: &Connection, project_id: &str) -> Result<Initiative> {
    let project_id = store::uuid(project_id)?;
    let left = initiative_of(conn, &project_id)?.ok_or(Refusal::NotFound)?;
    conn.execute(
        "DELETE FROM planning_initiative_projects WHERE project_id = ?1",
        [&project_id],
    )?;
    initiative(conn, &left.id)
}

pub(crate) const UPDATES_PAGE: usize = 20;
pub(crate) const MAX_UPDATES_PAGE: usize = 100;

const UPDATE_COLUMNS: &str = "id, initiative_id, health, body, progress, author_kind, author_name,
    created_at_ms, edited_at_ms, deleted_at_ms FROM planning_initiative_updates";

fn read_update(row: &Row<'_>) -> rusqlite::Result<InitiativeUpdate> {
    let deleted = row.get::<_, Option<i64>>(9)?.is_some();
    Ok(InitiativeUpdate {
        id: row.get(0)?,
        initiative_id: row.get(1)?,
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

/// One initiative update not deleted.
pub(crate) fn update(conn: &Connection, id: &str) -> Result<InitiativeUpdate> {
    let id = store::uuid(id)?;
    conn.query_row(
        &format!("SELECT {UPDATE_COLUMNS} WHERE id = ?1 AND deleted_at_ms IS NULL"),
        [&id],
        read_update,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

fn latest_update(conn: &Connection, initiative_id: &str) -> Result<Option<InitiativeUpdate>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {UPDATE_COLUMNS} WHERE initiative_id = ?1 AND deleted_at_ms IS NULL
                 ORDER BY created_at_ms DESC, id DESC LIMIT 1"
            ),
            [initiative_id],
            read_update,
        )
        .optional()?)
}

fn wrote(author: Author<'_>, update: &InitiativeUpdate) -> bool {
    update.author.kind == author.actor
        && match (author.name(), update.author.name.as_deref()) {
            (Some(mine), Some(theirs)) => mine.eq_ignore_ascii_case(theirs),
            (None, None) => true,
            _ => false,
        }
}

fn update_body(body: &str) -> Result<String> {
    present(body).ok_or_else(|| Refusal::Invalid.into())
}

fn touch(conn: &Connection, initiative_id: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE planning_initiatives SET updated_at_ms = ?2 WHERE id = ?1",
        params![initiative_id, now_ms],
    )?;
    Ok(())
}

/// The share of an initiative's tasks done, 0 to 100.
pub(crate) fn percent(progress: InitiativeProgress) -> u8 {
    let tasks = progress.open_tasks + progress.done_tasks;
    if tasks == 0 {
        return 0;
    }
    u8::try_from((progress.done_tasks * 100 + tasks / 2) / tasks).unwrap_or(100)
}

/// Posts an update on an initiative, keeping its progress as it stands.
pub(crate) fn add_update(
    conn: &Connection,
    author: Author<'_>,
    initiative_id: &str,
    health: ProjectHealth,
    body: &str,
    now_ms: i64,
) -> Result<InitiativeUpdate> {
    let found = initiative(conn, &store::uuid(initiative_id)?)?;
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_initiative_updates
            (id, initiative_id, health, body, progress, author_kind, author_name, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            found.id,
            health.as_str(),
            update_body(body)?,
            percent(found.progress),
            author.actor.as_str(),
            author.name(),
            now_ms
        ],
    )?;
    touch(conn, &found.id, now_ms)?;
    update(conn, &id)
}

/// Changes an update's health or body, from whoever wrote it.
pub(crate) fn edit_update(
    conn: &Connection,
    author: Author<'_>,
    id: &str,
    health: Option<ProjectHealth>,
    body: Option<&str>,
    now_ms: i64,
) -> Result<InitiativeUpdate> {
    let current = update(conn, id)?;
    if !wrote(author, &current) {
        return Err(Refusal::NotAuthor.into());
    }
    let body = match body {
        Some(body) => update_body(body)?,
        None => current.body.clone().unwrap_or_default(),
    };
    let health = health.unwrap_or(current.health);
    if Some(&body) == current.body.as_ref() && health == current.health {
        return Ok(current);
    }
    conn.execute(
        "UPDATE planning_initiative_updates SET health = ?2, body = ?3, edited_at_ms = ?4
         WHERE id = ?1",
        params![current.id, health.as_str(), body, now_ms],
    )?;
    touch(conn, &current.initiative_id, now_ms)?;
    update(conn, &current.id)
}

/// Blanks an update, from whoever wrote it; it no longer lists.
pub(crate) fn delete_update(
    conn: &Connection,
    author: Author<'_>,
    id: &str,
    now_ms: i64,
) -> Result<InitiativeUpdate> {
    let current = update(conn, id)?;
    if !wrote(author, &current) {
        return Err(Refusal::NotAuthor.into());
    }
    conn.execute(
        "UPDATE planning_initiative_updates SET body = '', deleted_at_ms = ?2 WHERE id = ?1",
        params![current.id, now_ms],
    )?;
    touch(conn, &current.initiative_id, now_ms)?;
    Ok(InitiativeUpdate {
        body: None,
        deleted: true,
        ..current
    })
}

/// An initiative's updates newest first; `cursor` is the last update of
/// the page before.
pub(crate) fn updates_page(
    conn: &Connection,
    initiative_id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<InitiativeUpdatePage> {
    let initiative_id = initiative(conn, &store::uuid(initiative_id)?)?.id;
    let limit = limit.clamp(1, MAX_UPDATES_PAGE);
    let (at, after) = match cursor {
        Some(cursor) => {
            let found = update(conn, cursor)?;
            if found.initiative_id != initiative_id {
                return Err(Refusal::NotFound.into());
            }
            (found.created_at_ms, found.id)
        }
        None => (i64::MAX, String::new()),
    };
    let mut statement = conn.prepare(&format!(
        "SELECT {UPDATE_COLUMNS}
         WHERE initiative_id = ?1 AND deleted_at_ms IS NULL
           AND (?4 = '' OR created_at_ms < ?2 OR (created_at_ms = ?2 AND id < ?4))
         ORDER BY created_at_ms DESC, id DESC LIMIT ?3"
    ))?;
    let mut updates = statement
        .query_map(
            params![initiative_id, at, i64::try_from(limit + 1)?, after],
            read_update,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if updates.len() > limit {
        updates.truncate(limit);
        updates.last().map(|update| update.id.clone())
    } else {
        None
    };
    Ok(InitiativeUpdatePage {
        updates,
        next_cursor,
    })
}

/// [`initiatives`], read for the app.
pub(crate) fn listed(app: &AppHandle) -> std::result::Result<Vec<Initiative>, String> {
    super::super::history(&app.state::<Arc<HistoryManager>>(), |conn| initiatives(conn))
}

/// [`updates_page`], read for the app.
pub(crate) fn updates_listed(
    app: &AppHandle,
    initiative_id: &str,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> std::result::Result<InitiativeUpdatePage, String> {
    let limit = limit.map_or(UPDATES_PAGE, |limit| {
        usize::try_from(limit).unwrap_or(MAX_UPDATES_PAGE)
    });
    super::super::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        updates_page(conn, initiative_id, limit, cursor)
    })
}

pub(crate) fn update_add_written(
    app: &AppHandle,
    author: Author<'_>,
    operation_id: &str,
    initiative_id: &str,
    health: ProjectHealth,
    body: &str,
) -> std::result::Result<InitiativeUpdate, String> {
    write(
        app,
        operation_id,
        "initiative_update_add",
        &(initiative_id, health, body, author.actor, author.agent),
        |tx, now| add_update(tx, author, initiative_id, health, body, now),
    )
}

pub(crate) fn update_edit_written(
    app: &AppHandle,
    author: Author<'_>,
    operation_id: &str,
    id: &str,
    health: Option<ProjectHealth>,
    body: Option<&str>,
) -> std::result::Result<InitiativeUpdate, String> {
    write(
        app,
        operation_id,
        "initiative_update_edit",
        &(id, health, body, author.actor, author.agent),
        |tx, now| edit_update(tx, author, id, health, body, now),
    )
}

pub(crate) fn update_delete_written(
    app: &AppHandle,
    author: Author<'_>,
    operation_id: &str,
    id: &str,
) -> std::result::Result<InitiativeUpdate, String> {
    write(
        app,
        operation_id,
        "initiative_update_delete",
        &(id, author.actor, author.agent),
        |tx, now| delete_update(tx, author, id, now),
    )
}

/// Runs one initiative write under `operation_id` and tells every window.
fn write<T: DeserializeOwned + Serialize>(
    app: &AppHandle,
    operation_id: &str,
    command: &str,
    request: &impl Serialize,
    change: impl FnOnce(&Connection, i64) -> Result<T>,
) -> std::result::Result<T, String> {
    let now = now_ms();
    let value = run_history(app, |history| {
        store::mutate(history, operation_id, command, request, now, |tx| {
            Ok(Done {
                effects: Effects::default(),
                value: change(tx, now)?,
            })
        })
    })?;
    super::super::changed_everywhere(app);
    Ok(value)
}

pub(crate) fn save_written(
    app: &AppHandle,
    operation_id: &str,
    id: Option<&str>,
    input: &InitiativeSaveInput,
) -> std::result::Result<Initiative, String> {
    write(app, operation_id, "initiative_save", &(id, input), |tx, now| {
        save(tx, id, input, now)
    })
}

pub(crate) fn delete_written(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> std::result::Result<Initiative, String> {
    write(app, operation_id, "initiative_delete", &id, |tx, _| delete(tx, id))
}

pub(crate) fn add_project_written(
    app: &AppHandle,
    operation_id: &str,
    initiative_id: &str,
    project_id: &str,
) -> std::result::Result<Initiative, String> {
    write(
        app,
        operation_id,
        "initiative_add_project",
        &(initiative_id, project_id),
        |tx, now| add_project(tx, initiative_id, project_id, now),
    )
}

pub(crate) fn remove_project_written(
    app: &AppHandle,
    operation_id: &str,
    project_id: &str,
) -> std::result::Result<Initiative, String> {
    write(app, operation_id, "initiative_remove_project", &project_id, |tx, _| {
        remove_project(tx, project_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::catalog::{save_project, ProjectEdit};
    use crate::planning::schema;
    use crate::planning::work::changes::page;

    fn project(conn: &Connection, title: &str) -> String {
        save_project(
            conn,
            None,
            &ProjectEdit {
                title: Some(title.to_owned()),
                ..Default::default()
            },
            1,
        )
        .expect("a project")
        .id
    }

    fn named(conn: &Connection, name: &str) -> Initiative {
        save(
            conn,
            None,
            &InitiativeSaveInput {
                name: Some(name.to_owned()),
                ..Default::default()
            },
            2,
        )
        .expect("an initiative")
    }

    #[test]
    fn a_project_sits_in_one_initiative_and_its_moves_are_changes() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let venue = project(&conn, "Book the venue");
        let launch = named(&conn, "Spring launch");
        let hiring = named(&conn, "Hiring");
        let before = page(&conn, None, 50).expect("a read").expect("a page");

        add_project(&conn, &launch.id, &venue, 3).expect("added");
        add_project(&conn, &hiring.id, &venue, 4).expect("moved");
        let moved_from = initiative(&conn, &launch.id).expect("the first one");
        let moved_to = initiative(&conn, &hiring.id).expect("the second one");
        delete(&conn, &hiring.id).expect("deleted");
        let after = page(&conn, Some(before.next_cursor), 50)
            .expect("a read")
            .expect("a page");

        assert_eq!(
            (moved_from.project_ids, moved_to.project_ids),
            (vec![], vec![venue.clone()])
        );
        assert_eq!(initiative_of(&conn, &venue).expect("a read"), None);
        let moves: Vec<_> = after
            .changes
            .iter()
            .filter(|change| change.entity == "project")
            .map(|change| (change.entity_id.as_str(), change.updated_from.clone()))
            .collect();
        assert_eq!(
            moves,
            [
                (venue.as_str(), Some(serde_json::json!({ "initiative": null }))),
                (venue.as_str(), Some(serde_json::json!({ "initiative": launch.id }))),
                (venue.as_str(), Some(serde_json::json!({ "initiative": hiring.id }))),
            ]
        );
    }
}
