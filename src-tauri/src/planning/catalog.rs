//! Projects and milestones as the command line saves them: the fields the
//! screens show beyond a title and a status.

use super::store::{self, Refusal};
use super::types::{DateResolution, Milestone, Project, ProjectStatus, TaskStatus};
use super::work::attachments;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;
use uuid::Uuid;

/// How long a project stays in the bin before it is gone.
pub(crate) const BIN_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// A link a project keeps among its resources.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinkEdit {
    pub title: Option<String>,
    pub url: String,
}

/// What a project is set to. A field left `None` keeps what the project has;
/// an empty description, lead or icon clears it, and `Some(None)` clears a
/// date or its resolution. Lists replace the project's whole list.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct ProjectEdit {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<ProjectStatus>,
    pub lead_person_id: Option<String>,
    pub member_person_ids: Option<Vec<String>>,
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
    let status = edit
        .status
        .or_else(|| current.as_ref().map(|c| c.status))
        .unwrap_or(ProjectStatus::Active);
    let lead = match &edit.lead_person_id {
        Some(lead) => present(lead),
        None => current.as_ref().and_then(|c| c.lead_person_id.clone()),
    };
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
                        icon = ?13, trashed_at_ms = ?14,
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
                    now_ms
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
                     start_resolution, target_resolution, icon, trashed_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                    trashed_at
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
    if let Some(labels) = &edit.label_ids {
        conn.execute(
            "DELETE FROM planning_project_labels WHERE project_id = ?1",
            [&id],
        )?;
        for label_id in labels {
            let known: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM planning_labels WHERE id = ?1)",
                [label_id],
                |row| row.get(0),
            )?;
            if !known {
                return Err(Refusal::NotFound.into());
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
                lead_person_id: Some(danny.to_owned()),
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
}
