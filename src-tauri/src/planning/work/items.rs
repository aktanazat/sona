//! Tracker items in the history database: tasks read as work items, changes
//! written with their history, and the labels, relations, saved views and
//! day plans beside them. Nothing here reads the meeting database; callers
//! check people against it before a change arrives.

use super::super::store::{self, Refusal};
use super::types::{
    WorkActivity, WorkActivityKind, WorkActor, WorkCounts, WorkDay, WorkDisplay, WorkDue, WorkItem,
    WorkLabel, WorkOwner, WorkPatch, WorkQuery, WorkRelation, WorkRelationKind, WorkStatus,
    WorkView,
};
use anyhow::Result;
use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashMap;
use uuid::Uuid;

const ITEM_COLUMNS: &str = "t.id, t.number, t.title, t.details, t.status, t.priority, t.project_id,
    t.milestone_id, t.owner_person_id, t.owner_is_self, t.due_at_ms, t.source_loop_id,
    t.duplicate_of, t.snoozed_until_ms, t.archived_at_ms, t.status_changed_at_ms,
    t.created_at_ms, t.updated_at_ms, t.revision, t.sort_order,
    (SELECT group_concat(label_id) FROM planning_task_labels WHERE task_id = t.id)";

fn read_item(row: &Row<'_>) -> rusqlite::Result<WorkItem> {
    let owner_person_id: Option<String> = row.get(8)?;
    let owner_is_self: bool = row.get(9)?;
    let mut label_ids = row
        .get::<_, Option<String>>(20)?
        .map(|ids| ids.split(',').map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    label_ids.sort();
    Ok(WorkItem {
        id: row.get(0)?,
        number: row.get(1)?,
        title: row.get(2)?,
        details: row.get(3)?,
        status: row.get(4)?,
        priority: row.get(5)?,
        project_id: row.get(6)?,
        milestone_id: row.get(7)?,
        direction: WorkOwner::of(owner_is_self, owner_person_id.as_deref()).direction(),
        owner_person_id,
        owner_is_self,
        due_at_ms: row.get(10)?,
        label_ids,
        source_loop_id: row.get(11)?,
        duplicate_of_id: row.get(12)?,
        snoozed_until_ms: row.get(13)?,
        archived_at_ms: row.get(14)?,
        status_changed_at_ms: row.get(15)?,
        created_at_ms: row.get(16)?,
        updated_at_ms: row.get(17)?,
        revision: row.get(18)?,
        sort_order: row.get(19)?,
    })
}

pub(crate) fn item(conn: &Connection, id: &str) -> Result<WorkItem> {
    conn.query_row(
        &format!("SELECT {ITEM_COLUMNS} FROM planning_tasks t WHERE t.id = ?1"),
        [id],
        read_item,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

/// How often the item comes back, as it was said, when it was.
pub(crate) fn recurrence(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn.query_row(
        "SELECT recurrence FROM planning_tasks WHERE id = ?1",
        [id],
        |row| row.get(0),
    )?)
}

/// Every item, oldest first, with archived ones only when asked.
pub(crate) fn items(conn: &Connection, include_archived: bool) -> Result<Vec<WorkItem>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ITEM_COLUMNS} FROM planning_tasks t
         WHERE ?1 OR t.archived_at_ms IS NULL
         ORDER BY t.number"
    ))?;
    let rows = statement.query_map([include_archived], read_item)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The item that tracks a meeting commitment, archived or not.
pub(crate) fn by_source_loop(conn: &Connection, loop_id: &str) -> Result<Option<WorkItem>> {
    Ok(conn
        .query_row(
            &format!("SELECT {ITEM_COLUMNS} FROM planning_tasks t WHERE t.source_loop_id = ?1"),
            [loop_id],
            read_item,
        )
        .optional()?)
}

/// The item and every item marked a duplicate of it.
pub(crate) fn family(conn: &Connection, root_id: &str) -> Result<Vec<WorkItem>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ITEM_COLUMNS} FROM planning_tasks t
         WHERE t.id = ?1 OR t.duplicate_of = ?1
         ORDER BY t.id != ?1, t.number"
    ))?;
    let rows = statement.query_map([root_id], read_item)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn clean_title(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Refusal::Invalid.into());
    }
    Ok(value)
}

fn clean_details(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|details| !details.is_empty())
        .map(str::to_owned)
}

fn owner_columns(owner: &WorkOwner) -> (bool, Option<&str>) {
    match owner {
        WorkOwner::Myself => (true, None),
        WorkOwner::Person { person_id } => (false, Some(person_id)),
        WorkOwner::Nobody => (false, None),
    }
}

fn existing_project(conn: &Connection, id: &str) -> Result<String> {
    let id = store::uuid(id)?;
    store::project(conn, &id)?;
    Ok(id)
}

fn existing_milestone(conn: &Connection, id: &str, project_id: Option<&str>) -> Result<String> {
    let id = store::uuid(id)?;
    if Some(store::milestone(conn, &id)?.project_id.as_str()) != project_id {
        return Err(Refusal::InvalidMembership.into());
    }
    Ok(id)
}

fn existing_labels(conn: &Connection, ids: &[String]) -> Result<Vec<String>> {
    let mut labels = ids
        .iter()
        .map(|id| store::uuid(id))
        .collect::<Result<Vec<_>>>()?;
    labels.sort();
    labels.dedup();
    let mut statement =
        conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM planning_labels WHERE id = ?1)")?;
    for id in &labels {
        if !statement.query_row([id], |row| row.get::<_, bool>(0))? {
            return Err(Refusal::NotFound.into());
        }
    }
    Ok(labels)
}

fn write_labels(conn: &Connection, task_id: &str, label_ids: &[String]) -> Result<()> {
    conn.execute(
        "DELETE FROM planning_task_labels WHERE task_id = ?1",
        [task_id],
    )?;
    let mut statement = conn
        .prepare_cached("INSERT INTO planning_task_labels (task_id, label_id) VALUES (?1, ?2)")?;
    for label_id in label_ids {
        statement.execute([task_id, label_id])?;
    }
    Ok(())
}

pub(crate) fn record(
    conn: &Connection,
    task_id: &str,
    now_ms: i64,
    actor: WorkActor,
    kind: WorkActivityKind,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_task_activity (id, task_id, at_ms, actor, kind, from_value, to_value)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            Uuid::new_v4().to_string(),
            task_id,
            now_ms,
            actor.as_str(),
            kind.as_str(),
            from,
            to
        ],
    )?;
    Ok(())
}

/// A new item before it has an id or a number. The owner's person, when
/// there is one, was already checked against the meeting database.
pub(crate) struct NewItem<'a> {
    pub title: &'a str,
    pub details: Option<&'a str>,
    pub status: WorkStatus,
    pub priority: u8,
    pub project_id: Option<&'a str>,
    pub milestone_id: Option<&'a str>,
    pub owner: &'a WorkOwner,
    pub due_at_ms: Option<i64>,
    pub label_ids: &'a [String],
    pub source_loop_id: Option<&'a str>,
    pub recurrence: Option<&'a str>,
}

pub(crate) fn insert(
    conn: &Connection,
    new: &NewItem<'_>,
    actor: WorkActor,
    now_ms: i64,
) -> Result<WorkItem> {
    if new.status == WorkStatus::Duplicate || new.priority > 4 {
        return Err(Refusal::Invalid.into());
    }
    let project_id = new
        .project_id
        .map(|id| existing_project(conn, id))
        .transpose()?;
    let milestone_id = new
        .milestone_id
        .map(|id| existing_milestone(conn, id, project_id.as_deref()))
        .transpose()?;
    let label_ids = existing_labels(conn, new.label_ids)?;
    let title = clean_title(new.title)?;
    let number: i64 = conn.query_row(
        "UPDATE planning_work_counter SET next_number = next_number + 1 WHERE id = 1
         RETURNING next_number - 1",
        [],
        |row| row.get(0),
    )?;
    let (owner_is_self, owner_person_id) = owner_columns(new.owner);
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_tasks (id, number, project_id, milestone_id, title, details,
            due_at_ms, owner_person_id, owner_is_self, status, priority, source_loop_id,
            status_changed_at_ms, sort_order, revision, created_at_ms, updated_at_ms, recurrence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, 1, ?13, ?13, ?14)",
        params![
            id,
            number,
            project_id,
            milestone_id,
            title,
            clean_details(new.details),
            new.due_at_ms,
            owner_person_id,
            owner_is_self,
            new.status.as_str(),
            new.priority,
            new.source_loop_id,
            now_ms,
            new.recurrence
                .map(str::trim)
                .filter(|every| !every.is_empty())
        ],
    )?;
    write_labels(conn, &id, &label_ids)?;
    record(
        conn,
        &id,
        now_ms,
        actor,
        WorkActivityKind::Created,
        None,
        Some(new.status.as_str()),
    )?;
    item(conn, &id)
}

/// One item before and after a change; equal when the change asked for
/// nothing new.
pub(crate) struct Changed {
    pub before: WorkItem,
    pub after: WorkItem,
}

fn joined(ids: &[String]) -> String {
    ids.join(",")
}

fn ms(value: Option<i64>) -> Option<String> {
    value.map(|value| value.to_string())
}

/// Applies `patch` to one item and writes a history entry per field it
/// changed. A status change is filed under `status_kind`, so accepting and
/// declining read as themselves. Marking a duplicate has its own path,
/// because it needs the item it duplicates.
pub(crate) fn apply(
    conn: &Connection,
    id: &str,
    patch: &WorkPatch,
    status_kind: WorkActivityKind,
    actor: WorkActor,
    now_ms: i64,
) -> Result<Changed> {
    let before = item(conn, id)?;
    let mut next = before.clone();
    if let Some(title) = &patch.title {
        clean_title(title)?.clone_into(&mut next.title);
    }
    if let Some(details) = &patch.details {
        next.details = clean_details(details.as_deref());
    }
    if let Some(status) = patch.status {
        if status == WorkStatus::Duplicate && before.status != WorkStatus::Duplicate {
            return Err(Refusal::Invalid.into());
        }
        if status != WorkStatus::Duplicate {
            next.duplicate_of_id = None;
        }
        next.status = status;
    }
    if let Some(priority) = patch.priority {
        if priority > 4 {
            return Err(Refusal::Invalid.into());
        }
        next.priority = priority;
    }
    if let Some(project_id) = &patch.project_id {
        next.project_id = project_id
            .as_deref()
            .map(|id| existing_project(conn, id))
            .transpose()?;
        if next.project_id != before.project_id {
            next.milestone_id = None;
        }
    }
    if let Some(milestone_id) = &patch.milestone_id {
        next.milestone_id = milestone_id
            .as_deref()
            .map(|id| existing_milestone(conn, id, next.project_id.as_deref()))
            .transpose()?;
    }
    if let Some(owner) = &patch.owner {
        let (owner_is_self, owner_person_id) = owner_columns(owner);
        next.owner_is_self = owner_is_self;
        next.owner_person_id = owner_person_id.map(str::to_owned);
        next.direction = owner.direction();
    }
    if let Some(due_at_ms) = patch.due_at_ms {
        next.due_at_ms = due_at_ms;
    }
    if let Some(label_ids) = &patch.label_ids {
        next.label_ids = existing_labels(conn, label_ids)?;
    }
    if let Some(snoozed_until_ms) = patch.snoozed_until_ms {
        next.snoozed_until_ms = snoozed_until_ms;
    }
    if next == before {
        return Ok(Changed {
            after: before.clone(),
            before,
        });
    }
    let status_changed_at_ms = if next.status == before.status {
        before.status_changed_at_ms
    } else {
        now_ms
    };
    conn.execute(
        "UPDATE planning_tasks SET title = ?2, details = ?3, status = ?4, priority = ?5,
            project_id = ?6, milestone_id = ?7, owner_person_id = ?8, owner_is_self = ?9,
            due_at_ms = ?10, duplicate_of = ?11, snoozed_until_ms = ?12,
            status_changed_at_ms = ?13, revision = revision + 1, updated_at_ms = ?14
         WHERE id = ?1",
        params![
            id,
            next.title,
            next.details,
            next.status.as_str(),
            next.priority,
            next.project_id,
            next.milestone_id,
            next.owner_person_id,
            next.owner_is_self,
            next.due_at_ms,
            next.duplicate_of_id,
            next.snoozed_until_ms,
            status_changed_at_ms,
            now_ms
        ],
    )?;
    if next.label_ids != before.label_ids {
        write_labels(conn, id, &next.label_ids)?;
    }
    let note = |kind: WorkActivityKind, from: Option<String>, to: Option<String>| {
        record(
            conn,
            id,
            now_ms,
            actor,
            kind,
            from.as_deref(),
            to.as_deref(),
        )
    };
    if next.status != before.status {
        note(
            status_kind,
            Some(before.status.as_str().to_owned()),
            Some(next.status.as_str().to_owned()),
        )?;
    }
    if next.priority != before.priority {
        note(
            WorkActivityKind::Priority,
            Some(before.priority.to_string()),
            Some(next.priority.to_string()),
        )?;
    }
    if next.owner() != before.owner() {
        note(
            WorkActivityKind::Owner,
            Some(before.owner().history_value()),
            Some(next.owner().history_value()),
        )?;
    }
    if next.due_at_ms != before.due_at_ms {
        note(
            WorkActivityKind::Due,
            ms(before.due_at_ms),
            ms(next.due_at_ms),
        )?;
    }
    if next.project_id != before.project_id {
        note(
            WorkActivityKind::Project,
            before.project_id.clone(),
            next.project_id.clone(),
        )?;
    }
    if next.title != before.title {
        note(
            WorkActivityKind::Title,
            Some(before.title.clone()),
            Some(next.title.clone()),
        )?;
    }
    if next.details != before.details {
        note(WorkActivityKind::Details, None, None)?;
    }
    if next.label_ids != before.label_ids {
        note(
            WorkActivityKind::Labels,
            Some(joined(&before.label_ids)),
            Some(joined(&next.label_ids)),
        )?;
    }
    if next.snoozed_until_ms != before.snoozed_until_ms {
        note(
            WorkActivityKind::Snoozed,
            ms(before.snoozed_until_ms),
            ms(next.snoozed_until_ms),
        )?;
    }
    Ok(Changed {
        before,
        after: item(conn, id)?,
    })
}

/// Hides an item from every list, or brings it back. Its status, and the
/// loop it tracks, stay as they were.
pub(crate) fn set_archived(
    conn: &Connection,
    id: &str,
    archived: bool,
    actor: WorkActor,
    now_ms: i64,
) -> Result<Changed> {
    let before = item(conn, id)?;
    if before.archived_at_ms.is_some() == archived {
        return Ok(Changed {
            after: before.clone(),
            before,
        });
    }
    conn.execute(
        "UPDATE planning_tasks SET archived_at_ms = ?2, revision = revision + 1, updated_at_ms = ?3
         WHERE id = ?1",
        params![id, archived.then_some(now_ms), now_ms],
    )?;
    let kind = if archived {
        WorkActivityKind::Archived
    } else {
        WorkActivityKind::Restored
    };
    record(conn, id, now_ms, actor, kind, None, None)?;
    Ok(Changed {
        before,
        after: item(conn, id)?,
    })
}

/// Says an item still stands without changing it: it counts as touched
/// now, so the morning's question about month-old work leaves it alone.
pub(crate) fn keep(conn: &Connection, id: &str, actor: WorkActor, now_ms: i64) -> Result<Changed> {
    let before = item(conn, id)?;
    conn.execute(
        "UPDATE planning_tasks SET revision = revision + 1, updated_at_ms = ?2 WHERE id = ?1",
        params![before.id, now_ms],
    )?;
    record(
        conn,
        &before.id,
        now_ms,
        actor,
        WorkActivityKind::Kept,
        None,
        None,
    )?;
    Ok(Changed {
        after: item(conn, &before.id)?,
        before,
    })
}

/// Marks `id` a duplicate of `of_id`'s original and hands its evidence and
/// its own duplicates to that original, so every duplicate points at one
/// item that is not one. Returns the original's id and every item changed.
pub(crate) fn mark_duplicate(
    conn: &Connection,
    id: &str,
    of_id: &str,
    actor: WorkActor,
    now_ms: i64,
) -> Result<(String, Vec<Changed>)> {
    let before = item(conn, id)?;
    let target = item(conn, of_id)?;
    let root = target.duplicate_of_id.clone().unwrap_or(target.id);
    if root == before.id {
        return Err(Refusal::Invalid.into());
    }
    let mut changed = Vec::new();
    if before.duplicate_of_id.as_deref() != Some(root.as_str()) {
        let status_changed_at_ms = if before.status == WorkStatus::Duplicate {
            before.status_changed_at_ms
        } else {
            now_ms
        };
        conn.execute(
            "UPDATE planning_tasks SET status = 'duplicate', duplicate_of = ?2,
                status_changed_at_ms = ?3, revision = revision + 1, updated_at_ms = ?4
             WHERE id = ?1",
            params![before.id, root, status_changed_at_ms, now_ms],
        )?;
        record(
            conn,
            &before.id,
            now_ms,
            actor,
            WorkActivityKind::Duplicate,
            Some(before.status.as_str()),
            Some(&root),
        )?;
        changed.push(Changed {
            after: item(conn, &before.id)?,
            before,
        });
    }
    let mut statement = conn.prepare(&format!(
        "SELECT {ITEM_COLUMNS} FROM planning_tasks t WHERE t.duplicate_of = ?1 ORDER BY t.number"
    ))?;
    let followers = statement
        .query_map([id], read_item)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for follower in followers {
        conn.execute(
            "UPDATE planning_tasks SET duplicate_of = ?2, revision = revision + 1, updated_at_ms = ?3
             WHERE id = ?1",
            params![follower.id, root, now_ms],
        )?;
        changed.push(Changed {
            after: item(conn, &follower.id)?,
            before: follower,
        });
    }
    conn.execute(
        "UPDATE OR IGNORE planning_evidence_links SET target_id = ?2
         WHERE target_kind = 'task' AND target_id = ?1",
        params![id, root],
    )?;
    Ok((root, changed))
}

/// Moves an item's status for a loop that moved in a meeting, filed as
/// Sona's doing.
pub(crate) fn follow(
    conn: &Connection,
    id: &str,
    status: WorkStatus,
    now_ms: i64,
) -> Result<Changed> {
    apply(
        conn,
        id,
        &WorkPatch {
            status: Some(status),
            ..WorkPatch::default()
        },
        WorkActivityKind::Status,
        WorkActor::Sona,
        now_ms,
    )
}

/// The open status an item held before it was last closed, for a reopen
/// that should put it back where it was.
pub(crate) fn status_before_close(conn: &Connection, id: &str) -> Result<WorkStatus> {
    let from: Option<String> = conn
        .query_row(
            "SELECT from_value FROM planning_task_activity
             WHERE task_id = ?1 AND (kind = 'duplicate' OR (kind IN ('status', 'accepted', 'declined')
               AND to_value IN ('done', 'canceled')))
             ORDER BY at_ms DESC, rowid DESC LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(match from.as_deref() {
        Some("triage") => WorkStatus::Triage,
        Some("backlog") => WorkStatus::Backlog,
        Some("in_progress") => WorkStatus::InProgress,
        _ => WorkStatus::NotStarted,
    })
}

pub(crate) fn activity(conn: &Connection, task_id: &str) -> Result<Vec<WorkActivity>> {
    let mut statement = conn.prepare(
        "SELECT a.id, a.task_id, a.at_ms, a.actor, a.kind, a.from_value, a.to_value, n.agent
         FROM planning_task_activity a
         LEFT JOIN planning_activity_agents n ON n.activity_id = a.id
         WHERE a.task_id = ?1 ORDER BY a.at_ms, a.rowid",
    )?;
    let rows = statement.query_map([task_id], |row| {
        Ok(WorkActivity {
            id: row.get(0)?,
            item_id: row.get(1)?,
            at_ms: row.get(2)?,
            actor: row.get(3)?,
            kind: row.get(4)?,
            from: row.get(5)?,
            to: row.get(6)?,
            agent: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_relation(row: &Row<'_>) -> rusqlite::Result<WorkRelation> {
    Ok(WorkRelation {
        id: row.get(0)?,
        from_id: row.get(1)?,
        to_id: row.get(2)?,
        kind: row.get(3)?,
    })
}

pub(crate) fn relations(conn: &Connection, task_id: &str) -> Result<Vec<WorkRelation>> {
    let mut statement = conn.prepare(
        "SELECT id, from_id, to_id, kind FROM planning_task_relations
         WHERE from_id = ?1 OR to_id = ?1 ORDER BY created_at_ms, id",
    )?;
    let rows = statement.query_map([task_id], read_relation)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// How many unfinished items block each item, by the blocked item's id.
pub(crate) fn open_blockers(conn: &Connection) -> Result<HashMap<String, u32>> {
    let mut statement = conn.prepare(
        "SELECT r.to_id, count(*) FROM planning_task_relations r
         JOIN planning_tasks b ON b.id = r.from_id
         WHERE r.kind = 'blocks' AND b.status NOT IN ('done', 'canceled', 'duplicate')
           AND b.archived_at_ms IS NULL
         GROUP BY r.to_id",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

/// How many meetings raised each item, by the id of the item its family
/// follows: the meetings its commitments were said in and the meetings
/// cited as its evidence, each counted once.
pub(crate) fn meetings_raised(conn: &Connection) -> Result<HashMap<String, u32>> {
    let mut statement = conn.prepare(
        "SELECT root, count(DISTINCT meeting) FROM (
             SELECT COALESCE(t.duplicate_of, t.id) AS root, substr(t.source_loop_id, 1, 36) AS meeting
             FROM planning_tasks t WHERE instr(t.source_loop_id, ':') = 37
             UNION ALL
             SELECT COALESCE(t.duplicate_of, t.id), CASE e.source_kind
                 WHEN 'meeting' THEN e.source_id ELSE substr(e.source_id, 1, 36) END
             FROM planning_evidence_links e JOIN planning_tasks t ON t.id = e.target_id
             WHERE e.target_kind = 'task' AND (e.source_kind = 'meeting'
                 OR (e.source_kind = 'loop' AND instr(e.source_id, ':') = 37))
         ) GROUP BY root",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

fn relation_mark(kind: WorkRelationKind, outgoing: bool, other: &str) -> String {
    let verb = match (kind, outgoing) {
        (WorkRelationKind::Blocks, true) => "blocks",
        (WorkRelationKind::Blocks, false) => "blocked_by",
        (WorkRelationKind::Related, _) => "related",
    };
    format!("{verb}:{other}")
}

/// Links two items, or hands back the link already there: "related" reads
/// the same both ways, so its reverse counts as the same link.
pub(crate) fn add_relation(
    conn: &Connection,
    from_id: &str,
    to_id: &str,
    kind: WorkRelationKind,
    actor: WorkActor,
    now_ms: i64,
) -> Result<WorkRelation> {
    let from_id = item(conn, &store::uuid(from_id)?)?.id;
    let to_id = item(conn, &store::uuid(to_id)?)?.id;
    if from_id == to_id {
        return Err(Refusal::Invalid.into());
    }
    let existing = conn
        .query_row(
            "SELECT id, from_id, to_id, kind FROM planning_task_relations
             WHERE kind = ?3 AND ((from_id = ?1 AND to_id = ?2)
                OR (kind = 'related' AND from_id = ?2 AND to_id = ?1))",
            params![from_id, to_id, kind.as_str()],
            read_relation,
        )
        .optional()?;
    if let Some(relation) = existing {
        return Ok(relation);
    }
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_task_relations (id, from_id, to_id, kind, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, from_id, to_id, kind.as_str(), now_ms],
    )?;
    let outgoing = relation_mark(kind, true, &to_id);
    let incoming = relation_mark(kind, false, &from_id);
    record(
        conn,
        &from_id,
        now_ms,
        actor,
        WorkActivityKind::Relation,
        None,
        Some(&outgoing),
    )?;
    record(
        conn,
        &to_id,
        now_ms,
        actor,
        WorkActivityKind::Relation,
        None,
        Some(&incoming),
    )?;
    Ok(WorkRelation {
        id,
        from_id,
        to_id,
        kind,
    })
}

pub(crate) fn remove_relation(
    conn: &Connection,
    id: &str,
    actor: WorkActor,
    now_ms: i64,
) -> Result<WorkRelation> {
    let relation = conn
        .query_row(
            "SELECT id, from_id, to_id, kind FROM planning_task_relations WHERE id = ?1",
            [store::uuid(id)?],
            read_relation,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    conn.execute(
        "DELETE FROM planning_task_relations WHERE id = ?1",
        [&relation.id],
    )?;
    let outgoing = relation_mark(relation.kind, true, &relation.to_id);
    let incoming = relation_mark(relation.kind, false, &relation.from_id);
    record(
        conn,
        &relation.from_id,
        now_ms,
        actor,
        WorkActivityKind::Relation,
        Some(&outgoing),
        None,
    )?;
    record(
        conn,
        &relation.to_id,
        now_ms,
        actor,
        WorkActivityKind::Relation,
        Some(&incoming),
        None,
    )?;
    Ok(relation)
}

fn read_label(row: &Row<'_>) -> rusqlite::Result<WorkLabel> {
    Ok(WorkLabel {
        id: row.get(0)?,
        name: row.get(1)?,
        hue: row.get(2)?,
        revision: row.get(3)?,
    })
}

pub(crate) fn labels(conn: &Connection) -> Result<Vec<WorkLabel>> {
    let mut statement = conn
        .prepare("SELECT id, name, hue, revision FROM planning_labels ORDER BY lower(name), id")?;
    let rows = statement.query_map([], read_label)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Creates a label, or renames and recolors one. Names are unique ignoring
/// case.
pub(crate) fn save_label(
    conn: &Connection,
    id: Option<&str>,
    name: &str,
    hue: f64,
    now_ms: i64,
) -> Result<WorkLabel> {
    let name = clean_title(name)?;
    if !(0.0..=360.0).contains(&hue) {
        return Err(Refusal::Invalid.into());
    }
    let id = match id {
        Some(id) => store::uuid(id)?,
        None => Uuid::new_v4().to_string(),
    };
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_labels WHERE lower(name) = lower(?1) AND id != ?2)",
        params![name, id],
        |row| row.get(0),
    )?;
    if taken {
        return Err(Refusal::Invalid.into());
    }
    conn.execute(
        "INSERT INTO planning_labels (id, name, hue, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, hue = excluded.hue,
            revision = revision + 1, updated_at_ms = excluded.updated_at_ms",
        params![id, name, hue, now_ms],
    )?;
    Ok(conn.query_row(
        "SELECT id, name, hue, revision FROM planning_labels WHERE id = ?1",
        [&id],
        read_label,
    )?)
}

/// Deletes a label and takes it off every item; returns those items.
pub(crate) fn delete_label(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let id = store::uuid(id)?;
    let mut statement = conn
        .prepare("SELECT task_id FROM planning_task_labels WHERE label_id = ?1 ORDER BY task_id")?;
    let tasks = statement
        .query_map([&id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    conn.execute(
        "DELETE FROM planning_task_labels WHERE label_id = ?1",
        [&id],
    )?;
    if conn.execute("DELETE FROM planning_labels WHERE id = ?1", [&id])? == 0 {
        return Err(Refusal::NotFound.into());
    }
    Ok(tasks)
}

fn read_view(row: &Row<'_>) -> rusqlite::Result<(String, String, String, String, i64)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn view_from(
    (id, name, query, display, revision): (String, String, String, String, i64),
) -> Result<WorkView> {
    Ok(WorkView {
        id,
        name,
        query: serde_json::from_str(&query)?,
        display: serde_json::from_str(&display)?,
        revision,
    })
}

pub(crate) fn views(conn: &Connection) -> Result<Vec<WorkView>> {
    let mut statement = conn.prepare(
        "SELECT id, name, query_json, display_json, revision FROM planning_views
         ORDER BY created_at_ms, id",
    )?;
    let rows = statement
        .query_map([], read_view)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(view_from).collect()
}

pub(crate) fn save_view(
    conn: &Connection,
    id: Option<&str>,
    name: &str,
    query: &WorkQuery,
    display: &WorkDisplay,
    now_ms: i64,
) -> Result<WorkView> {
    let name = clean_title(name)?;
    let id = match id {
        Some(id) => store::uuid(id)?,
        None => Uuid::new_v4().to_string(),
    };
    conn.execute(
        "INSERT INTO planning_views (id, name, query_json, display_json, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, query_json = excluded.query_json,
            display_json = excluded.display_json, revision = revision + 1,
            updated_at_ms = excluded.updated_at_ms",
        params![
            id,
            name,
            serde_json::to_string(query)?,
            serde_json::to_string(display)?,
            now_ms
        ],
    )?;
    view_from(conn.query_row(
        "SELECT id, name, query_json, display_json, revision FROM planning_views WHERE id = ?1",
        [&id],
        read_view,
    )?)
}

pub(crate) fn delete_view(conn: &Connection, id: &str) -> Result<()> {
    if conn.execute(
        "DELETE FROM planning_views WHERE id = ?1",
        [store::uuid(id)?],
    )? == 0
    {
        return Err(Refusal::NotFound.into());
    }
    Ok(())
}

/// The most items one day's plan focuses on.
const FOCUS_LIMIT: usize = 3;

/// A local calendar day as `YYYY-MM-DD`.
fn clean_day(value: &str) -> Result<String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(|day| day.format("%Y-%m-%d").to_string())
        .map_err(|_| Refusal::Invalid.into())
}

/// The plan made for a local day, empty when none was.
pub(crate) fn day(conn: &Connection, day: &str) -> Result<WorkDay> {
    let day = clean_day(day)?;
    let saved: Option<(String, i64)> = conn
        .query_row(
            "SELECT focus_json, planned_at_ms FROM planning_days WHERE day = ?1",
            [&day],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match saved {
        Some((focus, planned_at_ms)) => WorkDay {
            day,
            focus_ids: serde_json::from_str(&focus)?,
            planned_at_ms: Some(planned_at_ms),
        },
        None => WorkDay {
            day,
            focus_ids: Vec::new(),
            planned_at_ms: None,
        },
    })
}

/// Saves a day's plan over any earlier one. Each focus item exists, and
/// is named once.
pub(crate) fn plan_day(
    conn: &Connection,
    day: &str,
    focus_ids: &[String],
    now_ms: i64,
) -> Result<WorkDay> {
    if focus_ids.len() > FOCUS_LIMIT {
        return Err(Refusal::Invalid.into());
    }
    let mut focus = Vec::with_capacity(focus_ids.len());
    for id in focus_ids {
        let id = item(conn, &store::uuid(id)?)?.id;
        if focus.contains(&id) {
            return Err(Refusal::Invalid.into());
        }
        focus.push(id);
    }
    conn.execute(
        "INSERT INTO planning_days (day, focus_json, planned_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(day) DO UPDATE SET focus_json = excluded.focus_json,
             planned_at_ms = excluded.planned_at_ms",
        params![clean_day(day)?, serde_json::to_string(&focus)?, now_ms],
    )?;
    self::day(conn, day)
}

fn local_midnight_ms(day: NaiveDate) -> Result<i64> {
    let midnight = day
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("midnight is outside the calendar"))?;
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|at| at.timestamp_millis())
        .ok_or_else(|| anyhow::anyhow!("local midnight does not exist"))
}

fn local_today(now_ms: i64) -> Result<NaiveDate> {
    Ok(Local
        .timestamp_millis_opt(now_ms)
        .single()
        .ok_or_else(|| anyhow::anyhow!("clock is outside the supported calendar"))?
        .date_naive())
}

/// Where the day `days` after `today` starts on this Mac's clock, in epoch ms.
fn day_start_ms(today: NaiveDate, days: u64) -> Result<i64> {
    let day = today
        .checked_add_days(Days::new(days))
        .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))?;
    local_midnight_ms(day)
}

/// Where today starts and ends on this Mac's clock, in epoch ms.
pub(crate) fn today_bounds(now_ms: i64) -> Result<(i64, i64)> {
    let today = local_today(now_ms)?;
    Ok((day_start_ms(today, 0)?, day_start_ms(today, 1)?))
}

/// A [`WorkDue`] pinned to the day it was asked on.
#[derive(Clone, Copy)]
pub(crate) enum DueSpan {
    /// Accepted, unfinished work due before this time.
    Overdue(i64),
    /// Due at or after the first time and before the second.
    Between(i64, i64),
    NoDate,
}

impl DueSpan {
    pub(crate) fn holds(self, item: &WorkItem) -> bool {
        match (self, item.due_at_ms) {
            (Self::Overdue(before), Some(due)) => due < before && item.status.is_open(),
            (Self::Between(from, until), Some(due)) => (from..until).contains(&due),
            (Self::NoDate, None) => true,
            (Self::Overdue(_) | Self::Between(..), None) | (Self::NoDate, Some(_)) => false,
        }
    }
}

/// Pins each window to the day `now_ms` falls on.
pub(crate) fn due_spans(due: &[WorkDue], now_ms: i64) -> Result<Vec<DueSpan>> {
    let today = local_today(now_ms)?;
    let start = day_start_ms(today, 0)?;
    due.iter()
        .map(|due| {
            Ok(match *due {
                WorkDue::Overdue => DueSpan::Overdue(start),
                WorkDue::Today => DueSpan::Between(start, day_start_ms(today, 1)?),
                WorkDue::ThisWeek => {
                    let monday = 7 - u64::from(today.weekday().num_days_from_monday());
                    DueSpan::Between(start, day_start_ms(today, monday)?)
                }
                WorkDue::NextDays { days } => {
                    DueSpan::Between(start, day_start_ms(today, u64::from(days) + 1)?)
                }
                WorkDue::NoDate => DueSpan::NoDate,
            })
        })
        .collect()
}

/// Whether the item is out of sight until a later time.
pub(crate) fn snoozed(item: &WorkItem, now_ms: i64) -> bool {
    item.snoozed_until_ms.is_some_and(|until| until > now_ms)
}

/// The sidebar's numbers, from each item and its follow-up moment. Archived
/// and snoozed items count nowhere; work is overdue once its due day has
/// ended, and a wait is stale once its follow-up moment has come.
pub(crate) fn counts<'a>(
    items: impl IntoIterator<Item = (&'a WorkItem, Option<i64>)>,
    now_ms: i64,
) -> Result<WorkCounts> {
    let (today_ms, tomorrow_ms) = today_bounds(now_ms)?;
    let mut counts = WorkCounts::default();
    for (item, follow_up_at_ms) in items {
        if item.archived_at_ms.is_some() || snoozed(item, now_ms) {
            continue;
        }
        if item.status == WorkStatus::Triage {
            counts.triage += 1;
            continue;
        }
        if !item.status.is_open() {
            continue;
        }
        match item.owner() {
            WorkOwner::Myself => {
                counts.mine_open += 1;
                match item.due_at_ms {
                    Some(due) if due < today_ms => counts.mine_overdue += 1,
                    Some(due) if due < tomorrow_ms => counts.mine_due_today += 1,
                    _ => {}
                }
            }
            WorkOwner::Person { .. } => {
                counts.waiting_on += 1;
                if follow_up_at_ms.is_some_and(|at| at <= now_ms) {
                    counts.waiting_on_stale += 1;
                }
            }
            WorkOwner::Nobody => {}
        }
    }
    Ok(counts)
}
