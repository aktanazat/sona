//! Following a project, a saved view or a note: what others do to it
//! reaches the inbox. A followed project tells of updates posted and status
//! changes, a followed view of tasks joining it and of tasks in it done or
//! canceled, a followed note of an agent's edits. As everywhere in the
//! inbox, what the user did themselves notifies no one.

use super::comments::Author;
use super::inbox::{self, Notice, NotificationKind, SubjectKind};
use super::types::{WorkActor, WorkStatus};
use super::{items, run_history, store, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::store::Refusal;
use crate::planning::types::{ProjectHealth, ProjectStatus};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// Edits by one agent to one note inside this long make one notice.
const NOTE_EDITS_MS: i64 = 10 * 60 * 1000;

/// One followed project, view or note.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Follow {
    pub(crate) kind: SubjectKind,
    pub(crate) target_id: String,
    pub(crate) created_at_ms: i64,
}

/// The tasks a followed view held at its last look, with their statuses,
/// and the view's revision then: a view whose filters changed starts over
/// without telling of every task it gained or lost.
#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct Members {
    revision: i64,
    tasks: BTreeMap<String, WorkStatus>,
}

pub(crate) fn following(conn: &Connection, kind: SubjectKind, id: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM planning_follows WHERE kind = ?1 AND target_id = ?2)",
        )?
        .query_row(params![kind.as_str(), id], |row| row.get(0))?)
}

pub(crate) fn follows(conn: &Connection) -> Result<Vec<Follow>> {
    let mut statement = conn.prepare(
        "SELECT kind, target_id, created_at_ms FROM planning_follows
         ORDER BY created_at_ms DESC, kind, target_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Follow {
            kind: row.get(0)?,
            target_id: row.get(1)?,
            created_at_ms: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The view's tasks as it lists them now.
fn members_now(conn: &Connection, view_id: &str, now_ms: i64) -> Result<Members> {
    let view = items::views(conn)?
        .into_iter()
        .find(|view| view.id == view_id)
        .ok_or(Refusal::NotFound)?;
    let listed = super::list(conn, None, None, &view.query, now_ms)?;
    Ok(Members {
        revision: view.revision,
        tasks: listed
            .items
            .into_iter()
            .map(|row| (row.item.id, row.item.status))
            .collect(),
    })
}

/// Remembers when the view was looked at and, given them, the tasks it
/// held then.
fn looked_at(
    conn: &Connection,
    view_id: &str,
    members: Option<&Members>,
    now_ms: i64,
) -> Result<()> {
    match members {
        Some(members) => conn.execute(
            "UPDATE planning_follows SET members = ?2, checked_at_ms = ?3
             WHERE kind = 'view' AND target_id = ?1",
            params![view_id, serde_json::to_string(members)?, now_ms],
        )?,
        None => conn.execute(
            "UPDATE planning_follows SET checked_at_ms = ?2 WHERE kind = 'view' AND target_id = ?1",
            params![view_id, now_ms],
        )?,
    };
    Ok(())
}

/// Follows a project, view or note, or stops. True when it changed. A
/// view followed starts from the tasks it holds now.
pub(crate) fn follow(
    conn: &Connection,
    kind: SubjectKind,
    id: &str,
    on: bool,
    now_ms: i64,
) -> Result<bool> {
    let id = store::uuid(id)?;
    if !on {
        return Ok(conn.execute(
            "DELETE FROM planning_follows WHERE kind = ?1 AND target_id = ?2",
            params![kind.as_str(), id],
        )? > 0);
    }
    match kind {
        SubjectKind::Project => {
            crate::planning::store::project(conn, &id)?;
        }
        SubjectKind::Note => {
            crate::scratchpad::note_with_connection(conn, &id)?.ok_or(Refusal::NotFound)?;
        }
        SubjectKind::View => {}
        SubjectKind::Initiative => return Err(Refusal::Invalid.into()),
    }
    let added = conn.execute(
        "INSERT INTO planning_follows (kind, target_id, created_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(kind, target_id) DO NOTHING",
        params![kind.as_str(), id, now_ms],
    )? > 0;
    if added && kind == SubjectKind::View {
        looked_at(conn, &id, Some(&members_now(conn, &id, now_ms)?), now_ms)?;
    }
    Ok(added)
}

/// Who changed the task last, and when.
fn last_change(
    conn: &Connection,
    task_id: &str,
) -> Result<Option<(WorkActor, Option<String>, i64)>> {
    Ok(conn
        .prepare_cached(
            "SELECT t.actor, a.agent, t.at_ms FROM planning_task_activity t
                LEFT JOIN planning_activity_agents a ON a.activity_id = t.id
             WHERE t.task_id = ?1 ORDER BY t.at_ms DESC, t.rowid DESC LIMIT 1",
        )?
        .query_row([task_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .optional()?)
}

/// Looks at each followed view: a task that joined it, or one in it that
/// was done or canceled since the last look, posts a notice when someone
/// other than the user changed it since then. A task that joined or left
/// as time passed, a due day coming or a snooze ending, posts none. A
/// view that cannot be read is skipped. Returns how many were posted.
pub(crate) fn watch_views(conn: &Connection, now_ms: i64) -> Result<usize> {
    let followed: Vec<(String, Option<String>, Option<i64>)> = conn
        .prepare(
            "SELECT target_id, members, checked_at_ms FROM planning_follows WHERE kind = 'view'",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut posted = 0;
    for (view_id, before, checked_at_ms) in followed {
        match watch_view(conn, &view_id, before.as_deref(), checked_at_ms, now_ms) {
            Ok(count) => posted += count,
            Err(error) => log::warn!("A followed view could not be looked at: {error:#}"),
        }
    }
    Ok(posted)
}

fn watch_view(
    conn: &Connection,
    view_id: &str,
    before: Option<&str>,
    checked_at_ms: Option<i64>,
    now_ms: i64,
) -> Result<usize> {
    let now = match members_now(conn, view_id, now_ms) {
        Ok(now) => now,
        Err(error) if matches!(error.downcast_ref::<Refusal>(), Some(Refusal::NotFound)) => {
            return Ok(0)
        }
        Err(error) => return Err(error),
    };
    let before = before.and_then(|json| match serde_json::from_str::<Members>(json) {
        Ok(members) => Some(members),
        Err(error) => {
            log::warn!("A followed view's last look could not be read, so it starts over: {error}");
            None
        }
    });
    let mut posted = 0;
    if let Some(before) = before
        .as_ref()
        .filter(|before| before.revision == now.revision)
    {
        let mut moved: Vec<(&str, &str)> = now
            .tasks
            .keys()
            .filter(|id| !before.tasks.contains_key(*id))
            .map(|id| (id.as_str(), "added"))
            .collect();
        for (id, was) in &before.tasks {
            if was.is_closed() {
                continue;
            }
            let status = match now.tasks.get(id) {
                Some(status) => Some(*status),
                None => items::item(conn, id).ok().map(|item| item.status),
            };
            match status {
                Some(WorkStatus::Done) => moved.push((id, "done")),
                Some(WorkStatus::Canceled) => moved.push((id, "canceled")),
                _ => {}
            }
        }
        for (task_id, what) in moved {
            let Some((actor, agent, at_ms)) = last_change(conn, task_id)? else {
                continue;
            };
            if actor == WorkActor::Me || checked_at_ms.is_some_and(|checked| at_ms <= checked) {
                continue;
            }
            let notice = Notice {
                task_id: Some(task_id),
                agent: agent.as_deref(),
                field: Some(what),
                ..Notice::about(NotificationKind::View, SubjectKind::View, view_id, actor)
            };
            if inbox::notify(conn, &notice, now_ms)? {
                posted += 1;
            }
        }
    }
    let changed = before.as_ref() != Some(&now);
    looked_at(conn, view_id, changed.then_some(&now), now_ms)?;
    Ok(posted)
}

/// Someone other than the user posted an update on a followed project.
pub(crate) fn project_update_posted(
    conn: &Connection,
    project_id: &str,
    author: Author<'_>,
    health: ProjectHealth,
    now_ms: i64,
) -> Result<()> {
    if author.actor == WorkActor::Me || !following(conn, SubjectKind::Project, project_id)? {
        return Ok(());
    }
    let name = author.name();
    let notice = Notice {
        agent: name.as_deref(),
        field: Some("update"),
        to: Some(health.as_str()),
        ..Notice::about(
            NotificationKind::Project,
            SubjectKind::Project,
            project_id,
            author.actor,
        )
    };
    inbox::notify(conn, &notice, now_ms)?;
    Ok(())
}

/// Someone other than the user moved a followed project to another status.
pub(crate) fn project_status_changed(
    conn: &Connection,
    project_id: &str,
    author: Author<'_>,
    from: ProjectStatus,
    to: ProjectStatus,
    now_ms: i64,
) -> Result<()> {
    if from == to
        || author.actor == WorkActor::Me
        || !following(conn, SubjectKind::Project, project_id)?
    {
        return Ok(());
    }
    let name = author.name();
    let notice = Notice {
        agent: name.as_deref(),
        field: Some("status"),
        from: Some(from.as_str()),
        to: Some(to.as_str()),
        ..Notice::about(
            NotificationKind::Project,
            SubjectKind::Project,
            project_id,
            author.actor,
        )
    };
    inbox::notify(conn, &notice, now_ms)?;
    Ok(())
}

/// An agent edited a followed note. A run of its edits close together
/// makes one notice, while that notice is still unread.
pub(crate) fn note_edited(
    conn: &Connection,
    note_id: &str,
    agent: Option<&str>,
    now_ms: i64,
) -> Result<()> {
    if !following(conn, SubjectKind::Note, note_id)? {
        return Ok(());
    }
    let recent: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_notifications
            WHERE kind = 'note' AND subject_id = ?1 AND agent IS ?2 AND read_at_ms IS NULL
              AND created_at_ms >= ?3)",
        params![note_id, agent, now_ms - NOTE_EDITS_MS],
        |row| row.get(0),
    )?;
    if recent {
        return Ok(());
    }
    let notice = Notice {
        agent,
        ..Notice::about(
            NotificationKind::Note,
            SubjectKind::Note,
            note_id,
            WorkActor::Agent,
        )
    };
    inbox::notify(conn, &notice, now_ms)?;
    Ok(())
}

fn following_change(
    history: &mut Connection,
    change: Change<'_>,
    kind: SubjectKind,
    id: &str,
    on: bool,
) -> Result<Done<bool>> {
    store::mutate(
        history,
        change.operation_id,
        "work_follow",
        &(kind, id, on),
        change.at_ms,
        |tx| {
            Ok(Done {
                value: follow(tx, kind, id, on, change.at_ms)?,
                effects: Effects::default(),
            })
        },
    )
}

/// Follows a project, view or note, or stops. The app's `work_follow`;
/// true when it changed.
pub(crate) fn follow_as(
    app: &AppHandle,
    operation_id: &str,
    kind: SubjectKind,
    id: &str,
    on: bool,
) -> Result<bool, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_history(app, |history| {
        following_change(history, change, kind, id, on)
    })
}

/// Everything followed besides tasks. The app's `work_follows`.
pub(crate) fn work_follows(app: AppHandle) -> Result<Vec<Follow>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| follows(conn))
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::inbox::{self, NotificationKind, Shown, SubjectKind};
    use super::super::items::{self, NewItem};
    use super::super::types::{
        WorkActor, WorkDisplay, WorkDue, WorkGroupBy, WorkLayout, WorkOrderBy, WorkOwner,
        WorkQuery, WorkStatus,
    };
    use super::{follow, watch_views};
    use rusqlite::Connection;

    const AT: i64 = 1_700_000_000_000;
    const MINUTE: i64 = 60 * 1000;
    const DAY: i64 = 24 * 60 * MINUTE;

    fn planner() -> Connection {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        conn
    }

    fn due_today_view(conn: &Connection, name: &str) -> String {
        let display = WorkDisplay {
            layout: WorkLayout::List,
            group_by: WorkGroupBy::None,
            order_by: WorkOrderBy::Due,
            show_empty_groups: false,
            properties: Vec::new(),
            show_sub_tasks: true,
            sub_group_by: WorkGroupBy::None,
            hidden_columns: Vec::new(),
        };
        let query = WorkQuery {
            due: Some(vec![WorkDue::Today]),
            ..WorkQuery::default()
        };
        let view = items::save_view(conn, None, name, &query, &display, AT).expect("a view");
        follow(conn, SubjectKind::View, &view.id, true, AT).expect("followed");
        view.id
    }

    fn agent_task(conn: &Connection, title: &str, due_at_ms: i64, at_ms: i64) {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Myself,
                due_at_ms: Some(due_at_ms),
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Agent,
            at_ms,
        )
        .expect("a task");
    }

    fn view_notices(conn: &Connection, now_ms: i64) -> Vec<String> {
        inbox::notifications(conn, Shown::default(), now_ms)
            .expect("the inbox")
            .into_iter()
            .filter(|notice| notice.kind == NotificationKind::View)
            .filter_map(|notice| notice.item_id)
            .collect()
    }

    #[test]
    fn a_task_that_joins_a_view_as_its_day_comes_is_credited_to_no_one() {
        let conn = planner();
        due_today_view(&conn, "Due today");
        agent_task(&conn, "Book the venue", AT + DAY, AT + MINUTE);
        assert_eq!(watch_views(&conn, AT + 2 * MINUTE).expect("a look"), 0);

        assert_eq!(watch_views(&conn, AT + DAY).expect("the next day"), 0);
        assert!(view_notices(&conn, AT + DAY).is_empty());

        agent_task(&conn, "Print the badges", AT + DAY, AT + DAY + MINUTE);
        assert_eq!(
            watch_views(&conn, AT + DAY + 2 * MINUTE).expect("a look"),
            1
        );
        assert_eq!(view_notices(&conn, AT + DAY + 2 * MINUTE).len(), 1);
    }

    #[test]
    fn a_view_whose_last_look_is_unreadable_starts_over_and_the_others_still_post() {
        let conn = planner();
        let broken = due_today_view(&conn, "Broken");
        due_today_view(&conn, "Due today");
        conn.execute(
            "UPDATE planning_follows SET members = '{\"revision\":1,\"tasks\":{\"x\":\"gone\"}}'
             WHERE target_id = ?1",
            [&broken],
        )
        .expect("an unreadable last look");
        agent_task(&conn, "Book the venue", AT, AT + MINUTE);

        assert_eq!(watch_views(&conn, AT + 2 * MINUTE).expect("a look"), 1);
        assert_eq!(
            watch_views(&conn, AT + 3 * MINUTE).expect("another look"),
            0
        );
    }
}
