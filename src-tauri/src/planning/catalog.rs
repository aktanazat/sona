//! Projects and milestones as the command line saves them: the fields the
//! screens show beyond a title and a status.

use super::store::{self, Refusal};
use super::types::{
    DateResolution, Milestone, Project, ProjectActivity, ProjectStatus, ProjectStatusName,
    TaskStatus,
};
use super::work::types::WorkOwner;
use super::work::attachments;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use uuid::Uuid;

/// How long a project stays in the bin before it is gone.
pub(crate) const BIN_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub(crate) const SELF_LEAD_MIGRATION: &str = "ALTER TABLE planning_projects ADD COLUMN lead_is_self INTEGER NOT NULL DEFAULT 0
    CHECK(lead_is_self IN (0, 1) AND (lead_is_self = 0 OR lead_person_id IS NULL));
ALTER TABLE planning_projects ADD COLUMN members_include_self INTEGER NOT NULL DEFAULT 0
    CHECK(members_include_self IN (0, 1));";

/// Summaries, custom status names, label scope and the history of a
/// project's properties. Triggers write the history so every writer of a
/// project or milestone is in it; members are written by `save_project`.
pub(crate) const PROJECT_CORE_MIGRATION: &str = "ALTER TABLE planning_projects ADD COLUMN summary TEXT
    CHECK(summary IS NULL OR length(trim(summary)) > 0);
CREATE TABLE planning_project_status_names (
    id TEXT PRIMARY KEY NOT NULL,
    category TEXT NOT NULL
        CHECK(category IN ('backlog', 'planned', 'active', 'paused', 'completed', 'canceled')),
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    position INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX planning_project_status_names_name
    ON planning_project_status_names(lower(name));
ALTER TABLE planning_projects ADD COLUMN status_name_id TEXT
    REFERENCES planning_project_status_names(id) ON DELETE SET NULL;
CREATE TRIGGER planning_project_status_name_category AFTER UPDATE OF status ON planning_projects
WHEN new.status_name_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM planning_project_status_names n
    WHERE n.id = new.status_name_id AND n.category = new.status)
BEGIN
    UPDATE planning_projects SET status_name_id = NULL WHERE id = new.id;
END;
ALTER TABLE planning_labels ADD COLUMN scope TEXT NOT NULL DEFAULT 'all'
    CHECK(scope IN ('all', 'tasks', 'projects'));
CREATE TABLE planning_project_activity (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES planning_projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('title', 'status', 'statusName', 'lead', 'members',
        'startDate', 'targetDate', 'priority', 'milestoneAdded', 'milestoneRemoved',
        'milestoneTarget')),
    subject TEXT,
    from_value TEXT,
    to_value TEXT,
    at_ms INTEGER NOT NULL
);
CREATE INDEX planning_project_activity_project ON planning_project_activity(project_id, at_ms);
CREATE TRIGGER planning_project_activity_update AFTER UPDATE ON planning_projects BEGIN
    INSERT INTO planning_project_activity (id, project_id, kind, from_value, to_value, at_ms)
    SELECT lower(hex(randomblob(16))), new.id, kind, from_value, to_value, new.updated_at_ms
    FROM (
        SELECT 'title' AS kind, old.title AS from_value, new.title AS to_value
            WHERE old.title IS NOT new.title
        UNION ALL SELECT 'status', old.status, new.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'statusName',
            (SELECT name FROM planning_project_status_names WHERE id = old.status_name_id),
            (SELECT name FROM planning_project_status_names WHERE id = new.status_name_id)
            WHERE old.status_name_id IS NOT new.status_name_id
        UNION ALL SELECT 'lead',
            CASE WHEN old.lead_is_self THEN 'me' ELSE old.lead_person_id END,
            CASE WHEN new.lead_is_self THEN 'me' ELSE new.lead_person_id END
            WHERE old.lead_is_self IS NOT new.lead_is_self
               OR old.lead_person_id IS NOT new.lead_person_id
        UNION ALL SELECT 'startDate', CAST(old.start_at_ms AS TEXT), CAST(new.start_at_ms AS TEXT)
            WHERE old.start_at_ms IS NOT new.start_at_ms
        UNION ALL SELECT 'targetDate', CAST(old.target_at_ms AS TEXT),
            CAST(new.target_at_ms AS TEXT) WHERE old.target_at_ms IS NOT new.target_at_ms
        UNION ALL SELECT 'priority', CAST(old.priority AS TEXT), CAST(new.priority AS TEXT)
            WHERE old.priority IS NOT new.priority
    );
END;
CREATE TRIGGER planning_project_activity_milestone_add AFTER INSERT ON planning_milestones
WHEN EXISTS (SELECT 1 FROM planning_projects WHERE id = new.project_id)
BEGIN
    INSERT INTO planning_project_activity (id, project_id, kind, subject, to_value, at_ms)
    VALUES (lower(hex(randomblob(16))), new.project_id, 'milestoneAdded', new.title,
        CAST(new.due_at_ms AS TEXT), new.created_at_ms);
END;
CREATE TRIGGER planning_project_activity_milestone_remove AFTER DELETE ON planning_milestones
WHEN EXISTS (SELECT 1 FROM planning_projects WHERE id = old.project_id)
BEGIN
    INSERT INTO planning_project_activity (id, project_id, kind, subject, from_value, at_ms)
    VALUES (lower(hex(randomblob(16))), old.project_id, 'milestoneRemoved', old.title,
        CAST(old.due_at_ms AS TEXT),
        CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
CREATE TRIGGER planning_project_activity_milestone_target AFTER UPDATE OF due_at_ms
    ON planning_milestones
WHEN old.due_at_ms IS NOT new.due_at_ms
BEGIN
    INSERT INTO planning_project_activity (id, project_id, kind, subject, from_value, to_value,
        at_ms)
    VALUES (lower(hex(randomblob(16))), new.project_id, 'milestoneTarget', new.title,
        CAST(old.due_at_ms AS TEXT), CAST(new.due_at_ms AS TEXT), new.updated_at_ms);
END;";

/// A link a project keeps among its resources.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinkEdit {
    pub title: Option<String>,
    pub url: String,
}

/// What a project is set to. A field left `None` keeps what the project has;
/// an empty description or icon clears it, and `Some(None)` clears a
/// date or its resolution. Lists replace the project's whole list.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct ProjectEdit {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<ProjectStatus>,
    pub lead: Option<WorkOwner>,
    pub member_person_ids: Option<Vec<String>>,
    pub members_include_self: Option<bool>,
    pub target_at_ms: Option<Option<i64>>,
    pub hue: Option<u16>,
    pub archived: Option<bool>,
    pub priority: Option<u8>,
    pub start_at_ms: Option<Option<i64>>,
    pub start_resolution: Option<Option<DateResolution>>,
    pub target_resolution: Option<Option<DateResolution>>,
    pub icon: Option<String>,
    pub label_ids: Option<Vec<String>>,
    pub links: Option<Vec<LinkEdit>>,
    pub trashed: Option<bool>,
    /// Empty clears it.
    pub summary: Option<String>,
    /// A custom status name, which sets the status to its category;
    /// `Some(None)` clears the name and keeps the status.
    pub status_name_id: Option<Option<String>>,
}

/// A date and how finely it is meant: a date with no resolution reads as a
/// day, and clearing the date clears its resolution.
fn dated(
    at: Option<Option<i64>>,
    resolution: Option<Option<DateResolution>>,
    current: (Option<i64>, Option<DateResolution>),
) -> (Option<i64>, Option<DateResolution>) {
    let at = at.unwrap_or(current.0);
    let resolution = resolution.unwrap_or(current.1);
    match at {
        Some(at) => (Some(at), Some(resolution.unwrap_or(DateResolution::Day))),
        None => (None, None),
    }
}

fn present(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn clean_title(value: &str) -> Result<String> {
    present(value).ok_or_else(|| Refusal::Invalid.into())
}

/// Members as the history writes them: `me` first, then person ids.
fn member_text(include_self: bool, people: &[String]) -> Option<String> {
    let mut names: Vec<&str> = people.iter().map(String::as_str).collect();
    names.sort_unstable();
    if include_self {
        names.insert(0, "me");
    }
    (!names.is_empty()).then(|| names.join(","))
}

/// The status and custom name a save leaves: a name sets its category,
/// and a status alone keeps the name only while it is that name's category.
fn status_named(
    conn: &Connection,
    edit: &ProjectEdit,
    current: Option<&Project>,
) -> Result<(ProjectStatus, Option<String>)> {
    let named = match &edit.status_name_id {
        Some(Some(id)) => Some(Some(status_name(conn, id)?)),
        Some(None) => Some(None),
        None => None,
    };
    let status = match (&named, edit.status) {
        (Some(Some(name)), Some(status)) if status != name.category => {
            return Err(Refusal::Invalid.into())
        }
        (Some(Some(name)), _) => name.category,
        (_, Some(status)) => status,
        (_, None) => current.map_or(ProjectStatus::Active, |c| c.status),
    };
    let name_id = match named {
        Some(name) => name.map(|name| name.id),
        None => match current.and_then(|c| c.status_name_id.as_deref()) {
            Some(id) => {
                let fits: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM planning_project_status_names
                      WHERE id = ?1 AND category = ?2)",
                    params![id, status.as_str()],
                    |row| row.get(0),
                )?;
                fits.then(|| id.to_owned())
            }
            None => None,
        },
    };
    Ok((status, name_id))
}

const STATUS_NAME_COLUMNS: &str = "id, category, name, position";

fn read_status_name(row: &Row<'_>) -> rusqlite::Result<ProjectStatusName> {
    Ok(ProjectStatusName {
        id: row.get(0)?,
        category: row.get(1)?,
        name: row.get(2)?,
        position: row.get(3)?,
    })
}

/// The custom status names, by category in the order the status menu
/// shows them, then by position.
pub(crate) fn status_names(conn: &Connection) -> Result<Vec<ProjectStatusName>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {STATUS_NAME_COLUMNS} FROM planning_project_status_names
         ORDER BY CASE category WHEN 'backlog' THEN 0 WHEN 'planned' THEN 1
             WHEN 'active' THEN 2 WHEN 'paused' THEN 3 WHEN 'completed' THEN 4 ELSE 5 END,
             position, lower(name), id"
    ))?;
    let rows = statement.query_map([], read_status_name)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn status_name(conn: &Connection, id: &str) -> Result<ProjectStatusName> {
    conn.query_row(
        &format!("SELECT {STATUS_NAME_COLUMNS} FROM planning_project_status_names WHERE id = ?1"),
        [id],
        read_status_name,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

/// Makes a custom status name, or renames or moves one. Moving a name to
/// another category moves the projects under it there too; returns those
/// projects with the name.
pub(crate) fn save_status_name(
    conn: &Connection,
    id: Option<&str>,
    category: ProjectStatus,
    name: &str,
    now_ms: i64,
) -> Result<(ProjectStatusName, Vec<String>)> {
    let name = clean_title(name)?;
    let current = id
        .map(|id| status_name(conn, &store::uuid(id)?))
        .transpose()?;
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_project_status_names
          WHERE lower(name) = lower(?1) AND id != ?2)",
        params![name, current.as_ref().map_or("", |c| c.id.as_str())],
        |row| row.get(0),
    )?;
    if taken {
        return Err(Refusal::Invalid.into());
    }
    let next_position = "(SELECT coalesce(max(position), 0) + 1
        FROM planning_project_status_names WHERE category = ?2)";
    let (id, moved) = match current {
        Some(current) => {
            if current.category == category {
                conn.execute(
                    "UPDATE planning_project_status_names SET name = ?3, updated_at_ms = ?4
                     WHERE id = ?1 AND category = ?2",
                    params![current.id, category.as_str(), name, now_ms],
                )?;
                (current.id, Vec::new())
            } else {
                conn.execute(
                    &format!(
                        "UPDATE planning_project_status_names
                            SET name = ?3, category = ?2, position = {next_position},
                                updated_at_ms = ?4
                          WHERE id = ?1"
                    ),
                    params![current.id, category.as_str(), name, now_ms],
                )?;
                let mut statement = conn.prepare(
                    "SELECT id FROM planning_projects WHERE status_name_id = ?1 ORDER BY id",
                )?;
                let moved = statement
                    .query_map([&current.id], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                conn.execute(
                    "UPDATE planning_projects SET status = ?2, revision = revision + 1,
                        updated_at_ms = ?3
                      WHERE status_name_id = ?1",
                    params![current.id, category.as_str(), now_ms],
                )?;
                (current.id, moved)
            }
        }
        None => {
            let id = Uuid::new_v4().to_string();
            conn.execute(
                &format!(
                    "INSERT INTO planning_project_status_names
                        (id, category, name, position, created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, {next_position}, ?4, ?4)"
                ),
                params![id, category.as_str(), name, now_ms],
            )?;
            (id, Vec::new())
        }
    };
    Ok((status_name(conn, &id)?, moved))
}

/// Deletes a custom status name; the projects under it keep their status
/// and lose the name. Returns those projects.
pub(crate) fn delete_status_name(conn: &Connection, id: &str, now_ms: i64) -> Result<Vec<String>> {
    let id = store::uuid(id)?;
    status_name(conn, &id)?;
    let mut statement =
        conn.prepare("SELECT id FROM planning_projects WHERE status_name_id = ?1 ORDER BY id")?;
    let projects = statement
        .query_map([&id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    conn.execute(
        "UPDATE planning_projects SET status_name_id = NULL, revision = revision + 1,
            updated_at_ms = ?2
          WHERE status_name_id = ?1",
        params![id, now_ms],
    )?;
    conn.execute(
        "DELETE FROM planning_project_status_names WHERE id = ?1",
        [&id],
    )?;
    Ok(projects)
}

/// What changed on a project, newest first: its properties, its members
/// and its milestones.
pub(crate) fn history(
    conn: &Connection,
    project_id: &str,
    limit: usize,
) -> Result<Vec<ProjectActivity>> {
    let project_id = store::uuid(project_id)?;
    store::project(conn, &project_id)?;
    let mut statement = conn.prepare(
        "SELECT id, project_id, kind, subject, from_value, to_value, at_ms
           FROM planning_project_activity WHERE project_id = ?1
          ORDER BY at_ms DESC, rowid DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_id, i64::try_from(limit)?], |row| {
        Ok(ProjectActivity {
            id: row.get(0)?,
            project_id: row.get(1)?,
            kind: row.get(2)?,
            subject: row.get(3)?,
            from_value: row.get(4)?,
            to_value: row.get(5)?,
            at_ms: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Makes a project of a milestone: its title, description and due day,
/// with the parent's priority, lead and members. The milestone's tasks
/// move into the new project and the milestone is deleted. Returns the
/// project and the tasks that moved.
pub(crate) fn milestone_to_project(
    conn: &Connection,
    id: &str,
    now_ms: i64,
) -> Result<(Project, Vec<String>)> {
    let milestone = store::milestone(conn, &store::uuid(id)?)?;
    let parent = store::project(conn, &milestone.project_id)?;
    let lead = match (&parent.lead_person_id, parent.lead_is_self) {
        (_, true) => WorkOwner::Myself,
        (Some(person_id), false) => WorkOwner::Person {
            person_id: person_id.clone(),
        },
        (None, false) => WorkOwner::Nobody,
    };
    let status = match milestone.status {
        TaskStatus::NotStarted => ProjectStatus::Planned,
        TaskStatus::InProgress => ProjectStatus::Active,
        TaskStatus::Done => ProjectStatus::Completed,
        TaskStatus::Canceled => ProjectStatus::Canceled,
    };
    let project = save_project(
        conn,
        None,
        &ProjectEdit {
            title: Some(milestone.title.clone()),
            description: milestone.description.clone(),
            status: Some(status),
            lead: Some(lead),
            member_person_ids: Some(parent.member_person_ids.clone()),
            members_include_self: Some(parent.members_include_self),
            target_at_ms: Some(milestone.due_at_ms),
            priority: Some(parent.priority),
            ..ProjectEdit::default()
        },
        now_ms,
    )?;
    let mut statement =
        conn.prepare("SELECT id FROM planning_tasks WHERE milestone_id = ?1 ORDER BY id")?;
    let tasks = statement
        .query_map([&milestone.id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    conn.execute(
        "UPDATE planning_tasks SET project_id = ?2, milestone_id = NULL,
            revision = revision + 1, updated_at_ms = ?3
          WHERE milestone_id = ?1",
        params![milestone.id, project.id, now_ms],
    )?;
    delete_milestone(conn, &milestone.id, now_ms)?;
    Ok((store::project(conn, &project.id)?, tasks))
}

pub(crate) fn save_project(
    conn: &Connection,
    id: Option<&str>,
    edit: &ProjectEdit,
    now_ms: i64,
) -> Result<Project> {
    let current = match id {
        Some(id) => Some(store::project(conn, &store::uuid(id)?)?),
        None => None,
    };
    let title = match (&edit.title, &current) {
        (Some(title), _) => clean_title(title)?,
        (None, Some(current)) => current.title.clone(),
        (None, None) => return Err(Refusal::Invalid.into()),
    };
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_projects WHERE lower(title) = lower(?1) AND id != ?2)",
        params![title, current.as_ref().map_or("", |project| project.id.as_str())],
        |row| row.get(0),
    )?;
    if taken {
        return Err(Refusal::Invalid.into());
    }
    let description = match &edit.description {
        Some(text) => present(text),
        None => current.as_ref().and_then(|c| c.description.clone()),
    };
    let (status, status_name_id) = status_named(conn, edit, current.as_ref())?;
    let summary = match &edit.summary {
        Some(text) => present(&text.split_whitespace().collect::<Vec<_>>().join(" ")),
        None => current.as_ref().and_then(|c| c.summary.clone()),
    };
    let (lead_is_self, lead) = match &edit.lead {
        Some(WorkOwner::Myself) => (true, None),
        Some(WorkOwner::Person { person_id }) => (false, present(person_id)),
        Some(WorkOwner::Nobody) => (false, None),
        None => current.as_ref().map_or((false, None), |c| {
            (c.lead_is_self, c.lead_person_id.clone())
        }),
    };
    let members_include_self = edit
        .members_include_self
        .or_else(|| current.as_ref().map(|c| c.members_include_self))
        .unwrap_or(false);
    let (target, target_resolution) = dated(
        edit.target_at_ms,
        edit.target_resolution,
        current
            .as_ref()
            .map_or((None, None), |c| (c.target_at_ms, c.target_resolution)),
    );
    let (start, start_resolution) = dated(
        edit.start_at_ms,
        edit.start_resolution,
        current
            .as_ref()
            .map_or((None, None), |c| (c.start_at_ms, c.start_resolution)),
    );
    if matches!((start, target), (Some(start), Some(target)) if start > target) {
        return Err(Refusal::Invalid.into());
    }
    let hue = edit.hue.or_else(|| current.as_ref().and_then(|c| c.hue));
    if hue.is_some_and(|hue| hue > 360) {
        return Err(Refusal::Invalid.into());
    }
    let priority = edit
        .priority
        .or_else(|| current.as_ref().map(|c| c.priority))
        .unwrap_or(0);
    if priority > 4 {
        return Err(Refusal::Invalid.into());
    }
    let icon = match &edit.icon {
        Some(icon) => present(icon),
        None => current.as_ref().and_then(|c| c.icon.clone()),
    };
    let trashed_at = match (edit.trashed, current.as_ref().and_then(|c| c.trashed_at_ms)) {
        (Some(true), Some(at)) => Some(at),
        (Some(true), None) => Some(now_ms),
        (Some(false), _) => None,
        (None, at) => at,
    };
    let archived_at = match (
        edit.archived,
        current.as_ref().and_then(|c| c.archived_at_ms),
    ) {
        (Some(true), Some(at)) => Some(at),
        (Some(true), None) => Some(now_ms),
        (Some(false), _) => None,
        (None, at) => at,
    };
    let id = match &current {
        Some(current) => {
            conn.execute(
                "UPDATE planning_projects
                    SET title = ?2, description = ?3, status = ?4, lead_person_id = ?5,
                        target_at_ms = ?6, hue = ?7, archived_at_ms = ?8, priority = ?9,
                        start_at_ms = ?10, start_resolution = ?11, target_resolution = ?12,
                        icon = ?13, trashed_at_ms = ?14, lead_is_self = ?16,
                        members_include_self = ?17, summary = ?18, status_name_id = ?19,
                        revision = revision + 1, updated_at_ms = ?15
                  WHERE id = ?1",
                params![
                    current.id,
                    title,
                    description,
                    status.as_str(),
                    lead,
                    target,
                    hue,
                    archived_at,
                    priority,
                    start,
                    start_resolution.map(DateResolution::as_str),
                    target_resolution.map(DateResolution::as_str),
                    icon,
                    trashed_at,
                    now_ms,
                    lead_is_self,
                    members_include_self,
                    summary,
                    status_name_id
                ],
            )?;
            current.id.clone()
        }
        None => {
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO planning_projects
                    (id, title, description, status, revision, created_at_ms, updated_at_ms,
                     lead_person_id, target_at_ms, hue, archived_at_ms, priority, start_at_ms,
                     start_resolution, target_resolution, icon, trashed_at_ms, lead_is_self,
                     members_include_self, summary, status_name_id)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19)",
                params![
                    id,
                    title,
                    description,
                    status.as_str(),
                    now_ms,
                    lead,
                    target,
                    hue,
                    archived_at,
                    priority,
                    start,
                    start_resolution.map(DateResolution::as_str),
                    target_resolution.map(DateResolution::as_str),
                    icon,
                    trashed_at,
                    lead_is_self,
                    members_include_self,
                    summary,
                    status_name_id
                ],
            )?;
            id
        }
    };
    if let Some(members) = &edit.member_person_ids {
        conn.execute(
            "DELETE FROM planning_project_members WHERE project_id = ?1",
            [&id],
        )?;
        for person_id in members {
            conn.execute(
                "INSERT OR IGNORE INTO planning_project_members (project_id, person_id)
                 VALUES (?1, ?2)",
                params![id, person_id],
            )?;
        }
    }
    if let Some(current) = &current {
        let before = member_text(current.members_include_self, &current.member_person_ids);
        let after = member_text(
            members_include_self,
            edit.member_person_ids
                .as_ref()
                .unwrap_or(&current.member_person_ids),
        );
        if before != after {
            conn.execute(
                "INSERT INTO planning_project_activity (id, project_id, kind, from_value, to_value, at_ms)
                 VALUES (?1, ?2, 'members', ?3, ?4, ?5)",
                params![Uuid::new_v4().to_string(), id, before, after, now_ms],
            )?;
        }
    }
    if let Some(labels) = &edit.label_ids {
        conn.execute(
            "DELETE FROM planning_project_labels WHERE project_id = ?1",
            [&id],
        )?;
        for label_id in labels {
            let scope: Option<String> = conn
                .query_row(
                    "SELECT scope FROM planning_labels WHERE id = ?1",
                    [label_id],
                    |row| row.get(0),
                )
                .optional()?;
            match scope.as_deref() {
                None => return Err(Refusal::NotFound.into()),
                Some("tasks") => return Err(Refusal::LabelScope.into()),
                Some(_) => {}
            }
            conn.execute(
                "INSERT OR IGNORE INTO planning_project_labels (project_id, label_id)
                 VALUES (?1, ?2)",
                params![id, label_id],
            )?;
        }
    }
    if let Some(links) = &edit.links {
        conn.execute(
            "DELETE FROM planning_project_links WHERE project_id = ?1",
            [&id],
        )?;
        for (position, link) in links.iter().enumerate() {
            conn.execute(
                "INSERT INTO planning_project_links (id, project_id, position, title, url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    Uuid::new_v4().to_string(),
                    id,
                    i64::try_from(position)?,
                    link.title.as_deref().and_then(present),
                    attachments::link(&link.url)?
                ],
            )?;
        }
    }
    store::project(conn, &id)
}

/// Deletes for good every project in the bin for 30 days or more; their
/// tasks stay, in no project. Returns the projects purged.
pub(crate) fn purge_bin(conn: &Connection, now_ms: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT id FROM planning_projects WHERE trashed_at_ms IS NOT NULL AND trashed_at_ms <= ?1
         ORDER BY id",
    )?;
    let gone = statement
        .query_map([now_ms - BIN_MS], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    for id in &gone {
        conn.execute("DELETE FROM planning_projects WHERE id = ?1", [id])?;
    }
    Ok(gone)
}

/// Puts a project's milestones in the order given; every one must be named
/// once.
pub(crate) fn reorder_milestones(
    conn: &Connection,
    project_id: &str,
    order: &[String],
    now_ms: i64,
) -> Result<Vec<Milestone>> {
    let project_id = store::uuid(project_id)?;
    store::project(conn, &project_id)?;
    let mut statement =
        conn.prepare("SELECT id FROM planning_milestones WHERE project_id = ?1 ORDER BY id")?;
    let mut have = statement
        .query_map([&project_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let mut wanted = order
        .iter()
        .map(|id| store::uuid(id))
        .collect::<Result<Vec<_>>>()?;
    let named = wanted.clone();
    wanted.sort();
    have.sort();
    if wanted != have {
        return Err(Refusal::InvalidMembership.into());
    }
    for (position, id) in named.iter().enumerate() {
        conn.execute(
            "UPDATE planning_milestones SET position = ?2,
                revision = revision + CASE WHEN position IS ?2 THEN 0 ELSE 1 END,
                updated_at_ms = CASE WHEN position IS ?2 THEN updated_at_ms ELSE ?3 END
             WHERE id = ?1",
            params![id, i64::try_from(position)? + 1, now_ms],
        )?;
    }
    conn.execute(
        "UPDATE planning_projects SET updated_at_ms = ?2 WHERE id = ?1",
        params![project_id, now_ms],
    )?;
    Ok(super::progress::milestone_progress(conn, &project_id)?.0)
}

/// What a milestone is set to; `None` keeps, an empty description clears.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct MilestoneEdit {
    pub title: Option<String>,
    pub description: Option<String>,
    pub due_at_ms: Option<Option<i64>>,
    pub status: Option<TaskStatus>,
}

pub(crate) fn save_milestone(
    conn: &Connection,
    project_id: &str,
    id: Option<&str>,
    edit: &MilestoneEdit,
    now_ms: i64,
) -> Result<Milestone> {
    let project_id = store::uuid(project_id)?;
    store::project(conn, &project_id)?;
    let current = match id {
        Some(id) => {
            let milestone = store::milestone(conn, &store::uuid(id)?)?;
            if milestone.project_id != project_id {
                return Err(Refusal::NotFound.into());
            }
            Some(milestone)
        }
        None => None,
    };
    let title = match (&edit.title, &current) {
        (Some(title), _) => clean_title(title)?,
        (None, Some(current)) => current.title.clone(),
        (None, None) => return Err(Refusal::Invalid.into()),
    };
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_milestones
          WHERE project_id = ?1 AND lower(title) = lower(?2) AND id != ?3)",
        params![
            project_id,
            title,
            current.as_ref().map_or("", |m| m.id.as_str())
        ],
        |row| row.get(0),
    )?;
    if taken {
        return Err(Refusal::Invalid.into());
    }
    let description = match &edit.description {
        Some(text) => present(text),
        None => current.as_ref().and_then(|c| c.description.clone()),
    };
    let due = match edit.due_at_ms {
        Some(due) => due,
        None => current.as_ref().and_then(|c| c.due_at_ms),
    };
    let status = edit
        .status
        .or_else(|| current.as_ref().map(|c| c.status))
        .unwrap_or(TaskStatus::NotStarted);
    let id = match &current {
        Some(current) => {
            conn.execute(
                "UPDATE planning_milestones
                    SET title = ?2, description = ?3, due_at_ms = ?4, status = ?5,
                        revision = revision + 1, updated_at_ms = ?6
                  WHERE id = ?1",
                params![current.id, title, description, due, status.as_str(), now_ms],
            )?;
            current.id.clone()
        }
        None => {
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO planning_milestones
                    (id, project_id, title, due_at_ms, status, revision, created_at_ms,
                     updated_at_ms, description, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6, ?7,
                    (SELECT coalesce(max(position), 0) + 1 FROM planning_milestones WHERE project_id = ?2))",
                params![
                    id,
                    project_id,
                    title,
                    due,
                    status.as_str(),
                    now_ms,
                    description
                ],
            )?;
            id
        }
    };
    conn.execute(
        "UPDATE planning_projects SET updated_at_ms = ?2 WHERE id = ?1",
        params![project_id, now_ms],
    )?;
    store::milestone(conn, &id)
}

/// Deletes a milestone; its tasks stay in the project with no milestone.
/// Returns those tasks' ids.
pub(crate) fn delete_milestone(conn: &Connection, id: &str, now_ms: i64) -> Result<Vec<String>> {
    let id = store::uuid(id)?;
    let milestone = store::milestone(conn, &id)?;
    let mut statement =
        conn.prepare("SELECT id FROM planning_tasks WHERE milestone_id = ?1 ORDER BY id")?;
    let tasks = statement
        .query_map([&id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    conn.execute("DELETE FROM planning_milestones WHERE id = ?1", [&id])?;
    conn.execute(
        "UPDATE planning_projects SET updated_at_ms = ?2 WHERE id = ?1",
        params![milestone.project_id, now_ms],
    )?;
    Ok(tasks)
}

/// Moves everything the tracker holds for one person to another: owned
/// tasks, projects led or joined, talking points and watches. Returns the
/// tasks that changed hands.
pub(crate) fn repoint_person(
    conn: &Connection,
    from: &str,
    to: &str,
    now_ms: i64,
) -> Result<Vec<String>> {
    let mut statement =
        conn.prepare("SELECT id FROM planning_tasks WHERE owner_person_id = ?1 ORDER BY id")?;
    let tasks = statement
        .query_map([from], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    conn.execute(
        "UPDATE planning_tasks SET owner_person_id = ?2, revision = revision + 1, updated_at_ms = ?3
          WHERE owner_person_id = ?1",
        params![from, to, now_ms],
    )?;
    conn.execute(
        "UPDATE planning_projects SET lead_person_id = ?2, revision = revision + 1, updated_at_ms = ?3
          WHERE lead_person_id = ?1",
        params![from, to, now_ms],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO planning_project_members (project_id, person_id)
         SELECT project_id, ?2 FROM planning_project_members WHERE person_id = ?1",
        params![from, to],
    )?;
    conn.execute(
        "DELETE FROM planning_project_members WHERE person_id = ?1",
        [from],
    )?;
    conn.execute(
        "UPDATE planning_talking_points SET person_id = ?2 WHERE person_id = ?1",
        params![from, to],
    )?;
    conn.execute(
        "UPDATE OR IGNORE planning_watches SET person_id = ?2 WHERE person_id = ?1",
        params![from, to],
    )?;
    conn.execute("DELETE FROM planning_watches WHERE person_id = ?1", [from])?;
    conn.execute(
        "UPDATE planning_watch_hits SET person_id = ?2 WHERE person_id = ?1",
        params![from, to],
    )?;
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;
    use crate::planning::work::items::{self, NewItem};
    use crate::planning::work::types::{WorkActor, WorkOwner, WorkStatus};

    fn owned_by(conn: &Connection, person: &str, title: &str) -> String {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Person {
                    person_id: person.to_owned(),
                },
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            0,
        )
        .expect("a filed task")
        .id
    }

    /// Merging people in the meeting store leaves the tracker's own rows
    /// pointing at the person who is gone; they have to follow.
    #[test]
    fn merging_a_person_moves_their_tasks_projects_and_membership() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let (danny, daniel, priya) = ("danny", "daniel", "priya");
        let task = owned_by(&conn, danny, "Draft the harbor map");
        let other = owned_by(&conn, priya, "Survey the dock");
        let project = save_project(
            &conn,
            None,
            &ProjectEdit {
                title: Some("Harbor".to_owned()),
                lead: Some(WorkOwner::Person {
                    person_id: danny.to_owned(),
                }),
                member_person_ids: Some(vec![danny.to_owned(), daniel.to_owned()]),
                ..ProjectEdit::default()
            },
            1,
        )
        .expect("a project");

        let moved = repoint_person(&conn, danny, daniel, 2).expect("the merge");

        assert_eq!(moved, [task.clone()]);
        let owner = |id: &str| -> Option<String> {
            conn.query_row(
                "SELECT owner_person_id FROM planning_tasks WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("a task")
        };
        assert_eq!(owner(&task).as_deref(), Some(daniel));
        assert_eq!(owner(&other).as_deref(), Some(priya));
        let project = store::project(&conn, &project.id).expect("the project");
        assert_eq!(project.lead_person_id.as_deref(), Some(daniel));
        assert_eq!(project.member_person_ids, [daniel]);
    }

    /// The Projects list counts what the tracker says: a task moved into a
    /// project through the command line shows in that project's row at once.
    #[test]
    fn the_project_list_counts_tasks_moved_into_a_project() {
        use crate::planning::work::types::{WorkActivityKind, WorkPatch};
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project = save_project(
            &conn,
            None,
            &ProjectEdit {
                title: Some("Harbor".to_owned()),
                ..ProjectEdit::default()
            },
            1,
        )
        .expect("a project");
        let open = owned_by(&conn, "danny", "Draft the harbor map");
        let finished = owned_by(&conn, "danny", "Survey the dock");
        let move_into = |id: &str, status: Option<WorkStatus>| {
            let patch = WorkPatch {
                project_id: Some(Some(project.id.clone())),
                status,
                ..WorkPatch::default()
            };
            items::apply(
                &conn,
                id,
                &patch,
                WorkActivityKind::Project,
                WorkActor::Me,
                2,
            )
            .expect("the move");
        };
        assert_eq!(counts(&conn), (0, 0));

        move_into(&open, None);
        move_into(&finished, Some(WorkStatus::Done));

        assert_eq!(counts(&conn), (1, 1));
    }

    fn counts(conn: &Connection) -> (i64, i64) {
        let rows = store::projects(conn, false).expect("the project list");
        (rows[0].open_task_count, rows[0].completed_task_count)
    }

    /// A custom status name pins its category: naming one sets the status,
    /// moving the name moves its projects, and a status of another kind
    /// drops the name.
    #[test]
    fn a_status_name_keeps_projects_inside_its_category() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let (review, _) = save_status_name(&conn, None, ProjectStatus::Active, "In review", 1)
            .expect("a status name");
        let project = save_project(
            &conn,
            None,
            &ProjectEdit {
                title: Some("Harbor".to_owned()),
                status_name_id: Some(Some(review.id.clone())),
                ..ProjectEdit::default()
            },
            2,
        )
        .expect("a project");
        assert_eq!(project.status, ProjectStatus::Active);
        assert_eq!(project.status_name_id.as_deref(), Some(review.id.as_str()));

        let (_, moved) = save_status_name(&conn, Some(&review.id), ProjectStatus::Paused, "In review", 3)
            .expect("the name moved");
        assert_eq!(moved, [project.id.clone()]);
        let paused = store::project(&conn, &project.id).expect("the project");
        assert_eq!(paused.status, ProjectStatus::Paused);
        assert_eq!(paused.status_name_id.as_deref(), Some(review.id.as_str()));

        let done = save_project(
            &conn,
            Some(&project.id),
            &ProjectEdit {
                status: Some(ProjectStatus::Completed),
                ..ProjectEdit::default()
            },
            4,
        )
        .expect("the project finished");
        assert_eq!(done.status, ProjectStatus::Completed);
        assert_eq!(done.status_name_id, None);
    }

    /// Turning a milestone into a project carries its name, date, state and
    /// tasks over, and takes the milestone out of its old project.
    #[test]
    fn a_milestone_turned_into_a_project_takes_its_tasks_along() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let parent = save_project(
            &conn,
            None,
            &ProjectEdit {
                title: Some("Harbor".to_owned()),
                lead: Some(WorkOwner::Person {
                    person_id: "danny".to_owned(),
                }),
                priority: Some(2),
                ..ProjectEdit::default()
            },
            1,
        )
        .expect("a project");
        let milestone = save_milestone(
            &conn,
            &parent.id,
            None,
            &MilestoneEdit {
                title: Some("Beta".to_owned()),
                due_at_ms: Some(Some(5_000)),
                status: Some(TaskStatus::InProgress),
                ..MilestoneEdit::default()
            },
            2,
        )
        .expect("a milestone");
        let task = items::insert(
            &conn,
            &NewItem {
                title: "Ship the beta",
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: Some(&parent.id),
                milestone_id: Some(&milestone.id),
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            3,
        )
        .expect("a task")
        .id;

        let (project, moved) = milestone_to_project(&conn, &milestone.id, 4).expect("the turn");

        assert_eq!(moved, [task.clone()]);
        assert_eq!(project.title, "Beta");
        assert_eq!(project.status, ProjectStatus::Active);
        assert_eq!(project.target_at_ms, Some(5_000));
        assert_eq!(project.lead_person_id.as_deref(), Some("danny"));
        assert_eq!(project.priority, 2);
        let (project_id, milestone_id): (String, Option<String>) = conn
            .query_row(
                "SELECT project_id, milestone_id FROM planning_tasks WHERE id = ?1",
                [&task],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the task");
        assert_eq!((project_id, milestone_id), (project.id.clone(), None));
        assert!(store::milestone(&conn, &milestone.id).is_err());
        let removed: Vec<_> = history(&conn, &parent.id, 10)
            .expect("the parent's history")
            .into_iter()
            .filter(|change| change.kind == "milestoneRemoved")
            .map(|change| change.subject)
            .collect();
        assert_eq!(removed, [Some("Beta".to_owned())]);
    }
}
