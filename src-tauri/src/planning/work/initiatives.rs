//! Initiatives: goals one level above projects. An initiative holds
//! projects, and a project sits in at most one initiative.

use super::super::store::{self, Refusal};
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

/// Where an initiative stands, as Linear names it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InitiativeStatus {
    #[default]
    Planned,
    Active,
    Completed,
}

impl InitiativeStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Active => "active",
            Self::Completed => "completed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "planned" => Some(Self::Planned),
            "active" => Some(Self::Active),
            "completed" => Some(Self::Completed),
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
    /// Its projects outside the archive and the bin, by name.
    pub project_ids: Vec<String>,
    pub progress: InitiativeProgress,
}

/// An initiative's fields to set. A key left out keeps its value; `null`
/// clears the description or the target date, and the owner `nobody`
/// clears the owner.
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
}

const COLUMNS: &str = "id, name, description, owner_person_id, owner_is_self, target_at_ms, status,
    created_at_ms, updated_at_ms FROM planning_initiatives";

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
        project_ids: Vec::new(),
        progress: InitiativeProgress::default(),
    })
}

/// Fills in the initiative's live projects and their progress.
fn attach_projects(conn: &Connection, initiative: &mut Initiative) -> Result<()> {
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
    let id = match current {
        Some(current) => {
            conn.execute(
                "UPDATE planning_initiatives
                    SET name = ?2, description = ?3, owner_person_id = ?4, owner_is_self = ?5,
                        target_at_ms = ?6, status = ?7, updated_at_ms = ?8
                  WHERE id = ?1",
                params![
                    current.id,
                    name,
                    description,
                    owner_person_id,
                    owner_is_self,
                    target,
                    status.as_str(),
                    now_ms
                ],
            )?;
            current.id
        }
        None => {
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO planning_initiatives (id, name, description, owner_person_id,
                    owner_is_self, target_at_ms, status, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    id,
                    name,
                    description,
                    owner_person_id,
                    owner_is_self,
                    target,
                    status.as_str(),
                    now_ms
                ],
            )?;
            id
        }
    };
    initiative(conn, &id)
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

/// [`initiatives`], read for the app.
pub(crate) fn listed(app: &AppHandle) -> std::result::Result<Vec<Initiative>, String> {
    super::super::history(&app.state::<Arc<HistoryManager>>(), |conn| initiatives(conn))
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
