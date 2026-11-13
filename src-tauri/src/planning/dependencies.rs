//! Projects that wait on other projects. A blocker should end before the
//! project it blocks starts; when its end falls after that start, the
//! dependency is broken.

use super::store::{self, Refusal};
use crate::managers::history::HistoryManager;
use anyhow::Result;
use chrono::{Local, NaiveDate, TimeZone};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// The dependencies between projects. Deleting a project drops its
/// dependencies, and each one added or removed reaches the change stream
/// as a change to `blocked_by` on the blocked project and to `blocking` on
/// the blocker, with the list it had before.
pub(crate) const DEPENDENCIES_MIGRATION: &str = "CREATE TABLE planning_project_dependencies (
    id TEXT PRIMARY KEY NOT NULL,
    blocker_id TEXT NOT NULL,
    blocked_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    CHECK(blocker_id != blocked_id),
    UNIQUE(blocker_id, blocked_id)
);
CREATE INDEX planning_project_dependencies_blocked ON planning_project_dependencies(blocked_id);
CREATE TRIGGER planning_project_dependencies_delete AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_project_dependencies WHERE blocker_id = old.id OR blocked_id = old.id;
END;
CREATE TRIGGER planning_changes_dependency_add AFTER INSERT ON planning_project_dependencies BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', p.id, 'update',
        json_object('blocked_by', json((SELECT json_group_array(blocker_id)
            FROM planning_project_dependencies
            WHERE blocked_id = new.blocked_id AND blocker_id != new.blocker_id))),
        json_object('title', p.title)
    FROM planning_projects p WHERE p.id = new.blocked_id;
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', p.id, 'update',
        json_object('blocking', json((SELECT json_group_array(blocked_id)
            FROM planning_project_dependencies
            WHERE blocker_id = new.blocker_id AND blocked_id != new.blocked_id))),
        json_object('title', p.title)
    FROM planning_projects p WHERE p.id = new.blocker_id;
END;
CREATE TRIGGER planning_changes_dependency_remove AFTER DELETE ON planning_project_dependencies BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', p.id, 'update',
        json_object('blocked_by', json((SELECT json_group_array(id) FROM (
            SELECT blocker_id AS id FROM planning_project_dependencies
            WHERE blocked_id = old.blocked_id
            UNION ALL SELECT old.blocker_id)))),
        json_object('title', p.title)
    FROM planning_projects p WHERE p.id = old.blocked_id;
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', p.id, 'update',
        json_object('blocking', json((SELECT json_group_array(id) FROM (
            SELECT blocked_id AS id FROM planning_project_dependencies
            WHERE blocker_id = old.blocker_id
            UNION ALL SELECT old.blocked_id)))),
        json_object('title', p.title)
    FROM planning_projects p WHERE p.id = old.blocker_id;
END;";

/// One project waiting on another to end before it starts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDependency {
    pub id: String,
    pub blocker_id: String,
    pub blocked_id: String,
    pub created_at_ms: i64,
    /// The blocker is still open and its target day, or its start when it
    /// has no target, falls after the day the blocked project starts, or
    /// its target when it has no start.
    pub broken: bool,
}

const COLUMNS: &str = "d.id, d.blocker_id, d.blocked_id, d.created_at_ms, b.status,
    coalesce(b.target_at_ms, b.start_at_ms), coalesce(k.start_at_ms, k.target_at_ms)
    FROM planning_project_dependencies d
    JOIN planning_projects b ON b.id = d.blocker_id
    JOIN planning_projects k ON k.id = d.blocked_id";

fn local_day(ms: i64) -> Option<NaiveDate> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|moment| moment.date_naive())
}

/// Whether a blocker with `status` ending at `blocker_end_ms` breaks a
/// dependency on a project starting at `blocked_start_ms`, by local day.
fn breaks(status: &str, blocker_end_ms: Option<i64>, blocked_start_ms: Option<i64>) -> bool {
    if matches!(status, "completed" | "canceled") {
        return false;
    }
    match (
        blocker_end_ms.and_then(local_day),
        blocked_start_ms.and_then(local_day),
    ) {
        (Some(end), Some(start)) => end > start,
        _ => false,
    }
}

fn read(row: &Row<'_>) -> rusqlite::Result<ProjectDependency> {
    let status: String = row.get(4)?;
    Ok(ProjectDependency {
        id: row.get(0)?,
        blocker_id: row.get(1)?,
        blocked_id: row.get(2)?,
        created_at_ms: row.get(3)?,
        broken: breaks(&status, row.get(5)?, row.get(6)?),
    })
}

/// Every dependency between projects outside the bin, oldest first.
pub(crate) fn dependencies(conn: &Connection) -> Result<Vec<ProjectDependency>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS}
         WHERE b.trashed_at_ms IS NULL AND k.trashed_at_ms IS NULL
         ORDER BY d.created_at_ms, d.id"
    ))?;
    let rows = statement.query_map([], read)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// [`dependencies`], read for the app.
pub(crate) fn listed(app: &AppHandle) -> std::result::Result<Vec<ProjectDependency>, String> {
    super::history(&app.state::<Arc<HistoryManager>>(), |conn| dependencies(conn))
}

fn dependency(conn: &Connection, id: &str) -> Result<ProjectDependency> {
    conn.query_row(&format!("SELECT {COLUMNS} WHERE d.id = ?1"), [id], read)
        .optional()?
        .ok_or_else(|| Refusal::NotFound.into())
}

fn live_project(conn: &Connection, id: &str) -> Result<String> {
    conn.query_row(
        "SELECT id FROM planning_projects WHERE id = ?1 AND trashed_at_ms IS NULL",
        [store::uuid(id)?],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

/// Makes `blocked_id` wait on `blocker_id`, or hands back the dependency
/// already there. Refused when the blocker already waits on the blocked
/// project, directly or through others, or when both are one project.
pub(crate) fn add(
    conn: &Connection,
    blocker_id: &str,
    blocked_id: &str,
    now_ms: i64,
) -> Result<ProjectDependency> {
    let blocker_id = live_project(conn, blocker_id)?;
    let blocked_id = live_project(conn, blocked_id)?;
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM planning_project_dependencies WHERE blocker_id = ?1 AND blocked_id = ?2",
            params![blocker_id, blocked_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return dependency(conn, &id);
    }
    let loops: bool = conn.query_row(
        "WITH RECURSIVE downstream(id) AS (
             SELECT ?2
             UNION SELECT d.blocked_id FROM planning_project_dependencies d
             JOIN downstream ON d.blocker_id = downstream.id
         )
         SELECT EXISTS (SELECT 1 FROM downstream WHERE id = ?1)",
        params![blocker_id, blocked_id],
        |row| row.get(0),
    )?;
    if loops {
        return Err(Refusal::DependencyLoop.into());
    }
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_project_dependencies (id, blocker_id, blocked_id, created_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![id, blocker_id, blocked_id, now_ms],
    )?;
    dependency(conn, &id)
}

/// Removes a dependency and hands back what it was.
pub(crate) fn remove(conn: &Connection, id: &str) -> Result<ProjectDependency> {
    let found = dependency(conn, &store::uuid(id)?)?;
    conn.execute(
        "DELETE FROM planning_project_dependencies WHERE id = ?1",
        [&found.id],
    )?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;

    fn project(conn: &Connection, title: &str, start: Option<&str>, target: Option<&str>) -> String {
        let id = store::insert_project(conn, title, None, 1).expect("a project").id;
        let ms = |day: Option<&str>| {
            day.map(|day| {
                let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").expect("a day");
                Local
                    .from_local_datetime(&day.and_hms_opt(12, 0, 0).expect("noon"))
                    .single()
                    .expect("one moment")
                    .timestamp_millis()
            })
        };
        conn.execute(
            "UPDATE planning_projects SET start_at_ms = ?2, target_at_ms = ?3 WHERE id = ?1",
            params![id, ms(start), ms(target)],
        )
        .expect("dates");
        id
    }

    fn changes(conn: &Connection) -> Vec<(String, String)> {
        let mut statement = conn
            .prepare("SELECT entity_id, updated_from FROM planning_changes WHERE updated_from LIKE '{\"block%' ORDER BY seq")
            .expect("the stream");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("rows");
        rows.collect::<rusqlite::Result<_>>().expect("changes")
    }

    #[test]
    fn a_blocker_ending_after_the_blocked_start_is_broken_and_loops_are_refused() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let pilot = project(&conn, "Pilot", Some("2026-11-16"), Some("2026-12-18"));
        let harbor = project(&conn, "Harbor", Some("2027-01-04"), Some("2027-03-31"));
        let ferry = project(&conn, "Ferry", Some("2027-01-11"), Some("2027-02-26"));

        let kept = add(&conn, &pilot, &harbor, 10).expect("pilot blocks harbor");
        let broken = add(&conn, &harbor, &ferry, 11).expect("harbor blocks ferry");
        assert!(!kept.broken, "the pilot ends before the harbor starts");
        assert!(broken.broken, "the harbor ends after the ferry starts");
        assert_eq!(add(&conn, &harbor, &ferry, 12).expect("again").id, broken.id);

        for (blocker, blocked) in [(&ferry, &pilot), (&ferry, &harbor), (&pilot, &pilot)] {
            let refused = add(&conn, blocker, blocked, 13).expect_err("a loop");
            assert!(matches!(refused.downcast_ref::<Refusal>(), Some(Refusal::DependencyLoop)));
        }

        conn.execute(
            "UPDATE planning_projects SET status = 'completed' WHERE id = ?1",
            [&harbor],
        )
        .expect("harbor done");
        let listed = dependencies(&conn).expect("the list");
        assert_eq!(
            listed.iter().map(|d| (d.id.as_str(), d.broken)).collect::<Vec<_>>(),
            [(kept.id.as_str(), false), (broken.id.as_str(), false)]
        );

        remove(&conn, &kept.id).expect("removed");
        assert_eq!(
            changes(&conn),
            [
                (harbor.clone(), "{\"blocked_by\":[]}".to_owned()),
                (pilot.clone(), "{\"blocking\":[]}".to_owned()),
                (ferry.clone(), "{\"blocked_by\":[]}".to_owned()),
                (harbor.clone(), "{\"blocking\":[]}".to_owned()),
                (harbor.clone(), format!("{{\"blocked_by\":[\"{pilot}\"]}}")),
                (pilot.clone(), format!("{{\"blocking\":[\"{harbor}\"]}}")),
            ]
        );

        conn.execute("DELETE FROM planning_projects WHERE id = ?1", [&ferry])
            .expect("ferry gone");
        assert!(dependencies(&conn).expect("the list").is_empty());
    }
}
