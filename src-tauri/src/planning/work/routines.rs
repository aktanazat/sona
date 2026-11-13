//! Task templates, a reminder on a task, and label groups and retired
//! labels.

use super::agents;
use super::items::{self, NewItem};
use super::types::{
    WorkActivityKind, WorkActor, WorkItem, WorkLabel, WorkOwner, WorkPatch, WorkStatus,
};
use super::{checked_owner, now_ms, run_history, run_with_meetings, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::store::{self, Refusal};
use crate::planning::types::LabelScope;
use crate::settings::WorkAutonomy;
use anyhow::Result as StorageResult;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// What a task made from a template starts with. The title is the task's
/// unless the caller names another; each sub-task title becomes a sub-task.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkTemplateFields {
    pub title: Option<String>,
    pub details: Option<String>,
    pub status: Option<WorkStatus>,
    pub priority: Option<u8>,
    pub owner: Option<WorkOwner>,
    pub project_id: Option<String>,
    #[serde(default)]
    pub label_ids: Vec<String>,
    /// Due this many days after the day it is made.
    pub due_in_days: Option<u32>,
    pub recurrence: Option<String>,
    #[serde(default)]
    pub subtasks: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkTemplate {
    pub id: String,
    pub name: String,
    pub fields: WorkTemplateFields,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// What the caller sets on a task made from a template in place of the
/// template's own.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkTemplateUse {
    pub title: Option<String>,
    pub due_at_ms: Option<i64>,
    /// For the task and its sub-tasks: triage while agents' changes wait.
    pub status: Option<WorkStatus>,
}

/// A task's reminder. Due once `remind_at_ms` passes; handed to the app to
/// notify once, when `notified_at_ms` is set.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkReminder {
    pub item_id: String,
    pub remind_at_ms: i64,
    pub set_at_ms: i64,
    pub actor: WorkActor,
    pub agent: Option<String>,
    pub notified_at_ms: Option<i64>,
}

fn read_template(row: &Row<'_>) -> rusqlite::Result<(String, String, String, i64, i64, i64)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn template_of(
    (id, name, fields, revision, created_at_ms, updated_at_ms): (
        String,
        String,
        String,
        i64,
        i64,
        i64,
    ),
) -> StorageResult<WorkTemplate> {
    Ok(WorkTemplate {
        id,
        name,
        fields: serde_json::from_str(&fields)?,
        revision,
        created_at_ms,
        updated_at_ms,
    })
}

const TEMPLATE_COLUMNS: &str = "id, name, fields, revision, created_at_ms, updated_at_ms";

pub(crate) fn templates(conn: &Connection) -> StorageResult<Vec<WorkTemplate>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {TEMPLATE_COLUMNS} FROM planning_task_templates ORDER BY lower(name), id"
    ))?;
    let rows = statement
        .query_map([], read_template)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(template_of).collect()
}

pub(crate) fn template(conn: &Connection, id: &str) -> StorageResult<WorkTemplate> {
    let row = conn
        .query_row(
            &format!("SELECT {TEMPLATE_COLUMNS} FROM planning_task_templates WHERE id = ?1"),
            [id],
            read_template,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    template_of(row)
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn cleaned(conn: &Connection, fields: &WorkTemplateFields) -> StorageResult<WorkTemplateFields> {
    if fields.priority.is_some_and(|priority| priority > 4) {
        return Err(Refusal::Invalid.into());
    }
    let subtasks = fields
        .subtasks
        .iter()
        .map(|title| trimmed(Some(title)).ok_or(Refusal::Invalid))
        .collect::<Result<Vec<_>, _>>()?;
    let mut label_ids = Vec::with_capacity(fields.label_ids.len());
    for id in &fields.label_ids {
        let id = store::uuid(id)?;
        if !label_ids.contains(&id) {
            label_ids.push(id);
        }
    }
    let label_ids = items::one_per_group(conn, &label_ids, &[])?;
    Ok(WorkTemplateFields {
        title: trimmed(fields.title.as_deref()),
        details: trimmed(fields.details.as_deref()),
        recurrence: trimmed(fields.recurrence.as_deref()),
        project_id: fields.project_id.as_deref().map(store::uuid).transpose()?,
        label_ids,
        subtasks,
        ..fields.clone()
    })
}

/// Creates a template, or renames and refills one. Names are unique
/// ignoring case.
pub(crate) fn save_template(
    conn: &Connection,
    id: Option<&str>,
    name: &str,
    fields: &WorkTemplateFields,
    now_ms: i64,
) -> StorageResult<WorkTemplate> {
    let name = trimmed(Some(name)).ok_or(Refusal::Invalid)?;
    let fields = cleaned(conn, fields)?;
    let id = match id {
        Some(id) => {
            let id = store::uuid(id)?;
            template(conn, &id)?;
            id
        }
        None => Uuid::new_v4().to_string(),
    };
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_task_templates WHERE lower(name) = lower(?1) AND id != ?2)",
        params![name, id],
        |row| row.get(0),
    )?;
    if taken {
        return Err(Refusal::Invalid.into());
    }
    conn.execute(
        "INSERT INTO planning_task_templates (id, name, fields, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, fields = excluded.fields,
            revision = revision + 1, updated_at_ms = excluded.updated_at_ms",
        params![id, name, serde_json::to_string(&fields)?, now_ms],
    )?;
    template(conn, &id)
}

pub(crate) fn delete_template(conn: &Connection, id: &str) -> StorageResult<WorkTemplate> {
    let found = template(conn, &store::uuid(id)?)?;
    conn.execute(
        "DELETE FROM planning_task_templates WHERE id = ?1",
        [&found.id],
    )?;
    Ok(found)
}

/// The template's labels that still exist and are not retired.
fn usable_labels(conn: &Connection, ids: &[String]) -> StorageResult<Vec<String>> {
    let mut statement = conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM planning_labels WHERE id = ?1 AND retired_at_ms IS NULL)",
    )?;
    let mut usable = Vec::with_capacity(ids.len());
    for id in ids {
        if statement.query_row([id], |row| row.get::<_, bool>(0))? {
            usable.push(id.clone());
        }
    }
    Ok(usable)
}

fn live_project(conn: &Connection, id: Option<&str>) -> StorageResult<Option<String>> {
    let Some(id) = id else { return Ok(None) };
    let live: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_projects WHERE id = ?1 AND trashed_at_ms IS NULL)",
        [id],
        |row| row.get(0),
    )?;
    Ok(live.then(|| id.to_owned()))
}

/// Makes a task from a template, then its sub-tasks under it, which take
/// the task's project, priority and owner. Returns the task first.
fn use_template(
    tx: &Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    template_id: &str,
    using: &WorkTemplateUse,
    effects: &mut Effects,
) -> StorageResult<Vec<WorkItem>> {
    let template = template(tx, &store::uuid(template_id)?)?;
    let fields = &template.fields;
    let title = trimmed(using.title.as_deref())
        .or_else(|| fields.title.clone())
        .unwrap_or_else(|| template.name.clone());
    let owner = checked_owner(
        meetings,
        fields.owner.as_ref().unwrap_or(&WorkOwner::Nobody),
    )?;
    let due_at_ms = match (using.due_at_ms, fields.due_in_days) {
        (Some(due), _) => Some(due),
        (None, Some(days)) => Some(items::day_start_ms(
            items::local_today(change.at_ms)?,
            u64::from(days),
        )?),
        (None, None) => None,
    };
    let project_id = live_project(tx, fields.project_id.as_deref())?;
    let label_ids = usable_labels(tx, &fields.label_ids)?;
    let parent = items::insert(
        tx,
        &NewItem {
            title: &title,
            details: fields.details.as_deref(),
            status: using
                .status
                .or(fields.status)
                .unwrap_or(WorkStatus::NotStarted),
            priority: fields.priority.unwrap_or(0),
            project_id: project_id.as_deref(),
            milestone_id: None,
            owner: &owner,
            due_at_ms,
            label_ids: &label_ids,
            source_loop_id: None,
            recurrence: fields.recurrence.as_deref(),
        },
        change.actor,
        change.at_ms,
    )?;
    effects.item(&parent);
    let mut made = vec![parent.clone()];
    for title in &fields.subtasks {
        let child_owner = items::inherited_owner(tx, &parent)?;
        let child = items::insert(
            tx,
            &NewItem {
                title,
                details: None,
                status: using.status.unwrap_or(WorkStatus::NotStarted),
                priority: parent.priority,
                project_id: parent.project_id.as_deref(),
                milestone_id: None,
                owner: &child_owner,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            change.actor,
            change.at_ms,
        )?;
        let changed = items::apply(
            tx,
            &child.id,
            &WorkPatch {
                parent_id: Some(Some(parent.id.clone())),
                ..WorkPatch::default()
            },
            WorkActivityKind::Status,
            change.actor,
            change.at_ms,
        )?;
        effects.item(&changed.after);
        made.push(changed.after);
    }
    Ok(made)
}

fn read_reminder(row: &Row<'_>) -> rusqlite::Result<WorkReminder> {
    Ok(WorkReminder {
        item_id: row.get(0)?,
        remind_at_ms: row.get(1)?,
        set_at_ms: row.get(2)?,
        actor: row.get(3)?,
        agent: row.get(4)?,
        notified_at_ms: row.get(5)?,
    })
}

const REMINDER_COLUMNS: &str = "task_id, remind_at_ms, set_at_ms, actor, agent, notified_at_ms";

/// Every reminder, soonest first.
pub(crate) fn reminders(conn: &Connection) -> StorageResult<Vec<WorkReminder>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {REMINDER_COLUMNS} FROM planning_task_reminders ORDER BY remind_at_ms, task_id"
    ))?;
    let rows = statement.query_map([], read_reminder)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Reminders whose time has come and that the app has not notified yet,
/// leaving out tasks in the bin.
pub(crate) fn due_reminders(conn: &Connection, now_ms: i64) -> StorageResult<Vec<WorkReminder>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {REMINDER_COLUMNS} FROM planning_task_reminders
         WHERE notified_at_ms IS NULL AND remind_at_ms <= ?1
           AND task_id NOT IN (SELECT id FROM planning_tasks WHERE trashed_at_ms IS NOT NULL)
         ORDER BY remind_at_ms, task_id"
    ))?;
    let rows = statement.query_map([now_ms], read_reminder)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn reminder(conn: &Connection, item_id: &str) -> StorageResult<Option<WorkReminder>> {
    Ok(conn
        .query_row(
            &format!("SELECT {REMINDER_COLUMNS} FROM planning_task_reminders WHERE task_id = ?1"),
            [item_id],
            read_reminder,
        )
        .optional()?)
}

/// Sets a task's one reminder, replacing any it had, or with `None` clears
/// it. Returns the reminder as it now is.
fn set_reminder(
    conn: &Connection,
    item_id: &str,
    at_ms: Option<i64>,
    actor: WorkActor,
    agent: Option<&str>,
    now_ms: i64,
) -> StorageResult<Option<WorkReminder>> {
    let item = items::item(conn, &store::uuid(item_id)?)?;
    let changed = match at_ms {
        Some(at_ms) => conn.execute(
            "INSERT INTO planning_task_reminders (task_id, remind_at_ms, set_at_ms, actor, agent, notified_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)
             ON CONFLICT(task_id) DO UPDATE SET remind_at_ms = excluded.remind_at_ms,
                set_at_ms = excluded.set_at_ms, actor = excluded.actor, agent = excluded.agent,
                notified_at_ms = NULL",
            params![item.id, at_ms, now_ms, actor.as_str(), agent],
        )?,
        None => conn.execute(
            "DELETE FROM planning_task_reminders WHERE task_id = ?1",
            [&item.id],
        )?,
    };
    if changed > 0 && actor == WorkActor::Agent {
        super::inbox::reminder_changed(conn, &item.id, agent, at_ms, now_ms)?;
    }
    reminder(conn, &item.id)
}

fn notified(conn: &Connection, item_ids: &[String], now_ms: i64) -> StorageResult<Vec<String>> {
    let mut statement = conn.prepare_cached(
        "UPDATE planning_task_reminders SET notified_at_ms = ?2
         WHERE task_id = ?1 AND notified_at_ms IS NULL",
    )?;
    let mut marked = Vec::new();
    for id in item_ids {
        if statement.execute(params![id, now_ms])? > 0 {
            marked.push(id.clone());
        }
    }
    Ok(marked)
}

fn label(conn: &Connection, id: &str) -> StorageResult<WorkLabel> {
    conn.query_row(
        &format!(
            "SELECT {} FROM planning_labels WHERE id = ?1",
            items::LABEL_COLUMNS
        ),
        [id],
        items::read_label,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

/// Puts a label in a group, or with `None` takes it out of its group.
fn group_label(
    conn: &Connection,
    id: &str,
    group: Option<&str>,
    now_ms: i64,
) -> StorageResult<WorkLabel> {
    let id = store::uuid(id)?;
    label(conn, &id)?;
    conn.execute(
        "UPDATE planning_labels SET group_name = ?2, revision = revision + 1, updated_at_ms = ?3
         WHERE id = ?1",
        params![id, trimmed(group), now_ms],
    )?;
    label(conn, &id)
}

/// Sets what a label may go on. Narrowing it is refused while it sits on
/// the kind it would leave out.
pub(crate) fn scope_label(
    conn: &Connection,
    id: &str,
    scope: LabelScope,
    now_ms: i64,
) -> StorageResult<WorkLabel> {
    let id = store::uuid(id)?;
    label(conn, &id)?;
    let used_elsewhere = match scope {
        LabelScope::All => None,
        LabelScope::Tasks => Some("planning_project_labels"),
        LabelScope::Projects => Some("planning_task_labels"),
    };
    if let Some(table) = used_elsewhere {
        let used: bool = conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE label_id = ?1)"),
            [&id],
            |row| row.get(0),
        )?;
        if used {
            return Err(Refusal::LabelScope.into());
        }
    }
    conn.execute(
        "UPDATE planning_labels SET scope = ?2,
            revision = revision + CASE WHEN scope IS ?2 THEN 0 ELSE 1 END,
            updated_at_ms = ?3
         WHERE id = ?1",
        params![id, scope.as_str(), now_ms],
    )?;
    label(conn, &id)
}

/// Retires a label, or brings it back. Its tasks keep it either way.
fn retire_label(
    conn: &Connection,
    id: &str,
    retired: bool,
    now_ms: i64,
) -> StorageResult<WorkLabel> {
    let id = store::uuid(id)?;
    label(conn, &id)?;
    conn.execute(
        "UPDATE planning_labels SET retired_at_ms = CASE WHEN ?2 THEN COALESCE(retired_at_ms, ?3) END,
            revision = revision + 1, updated_at_ms = ?3
         WHERE id = ?1",
        params![id, retired, now_ms],
    )?;
    label(conn, &id)
}

/// Every template. The app's `work_templates`.
pub(crate) fn templates_read(app: &AppHandle) -> Result<Vec<WorkTemplate>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| templates(conn))
}

/// Every reminder, or with `due_only` those to notify now. The app's
/// `work_reminders`.
pub(crate) fn reminders_read(app: &AppHandle, due_only: bool) -> Result<Vec<WorkReminder>, String> {
    let now = now_ms();
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        if due_only {
            due_reminders(conn, now)
        } else {
            reminders(conn)
        }
    })
}

/// Saves a template. The app's `work_template_save`.
pub(crate) fn template_save_as(
    app: &AppHandle,
    operation_id: &str,
    id: Option<&str>,
    name: &str,
    fields: &WorkTemplateFields,
) -> Result<WorkTemplate, String> {
    let at_ms = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_template_save",
            &(id, name, fields),
            at_ms,
            |tx| {
                Ok(Done {
                    value: save_template(tx, id, name, fields, at_ms)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Deletes a template; tasks made from it stay. The app's
/// `work_template_delete`.
pub(crate) fn template_delete_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> Result<WorkTemplate, String> {
    let at_ms = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_template_delete",
            &id,
            at_ms,
            |tx| {
                Ok(Done {
                    value: delete_template(tx, id)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Makes a task and its sub-tasks from a template, as `actor`. The app's
/// `work_template_use`.
pub(crate) async fn template_use_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    template_id: &str,
    using: &WorkTemplateUse,
) -> Result<Vec<WorkItem>, String> {
    let change = Change::now(operation_id, actor);
    run_with_meetings(app, |history, meetings| {
        store::mutate(
            history,
            change.operation_id,
            "work_template_use",
            &(template_id, using),
            change.at_ms,
            |tx| {
                let mut effects = Effects::default();
                let value = use_template(tx, meetings, change, template_id, using, &mut effects)?;
                Ok(Done { value, effects })
            },
        )
    })
    .await
}

/// [`template_use_as`] by an agent: while agents' changes wait, the task
/// and its sub-tasks start in Triage. Each is filed under the agent.
pub(crate) async fn agent_template_use_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    template_id: &str,
    using: &WorkTemplateUse,
) -> Result<Vec<WorkItem>, String> {
    let using = match agents::autonomy(app) {
        WorkAutonomy::Apply => using.clone(),
        WorkAutonomy::Suggest => WorkTemplateUse {
            status: Some(WorkStatus::Triage),
            ..using.clone()
        },
    };
    let items = template_use_as(app, WorkActor::Agent, operation_id, template_id, &using).await?;
    let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
    if let Some(since) = items.iter().map(|item| item.created_at_ms).min() {
        agents::credit_since(app, &ids, since, agent)?;
    }
    Ok(items)
}

/// Sets or with `None` clears a task's reminder, as `actor`. The app's
/// `work_reminder_set`.
pub(crate) fn reminder_set_as(
    app: &AppHandle,
    actor: WorkActor,
    agent: Option<&str>,
    operation_id: &str,
    item_id: &str,
    at_ms: Option<i64>,
) -> Result<Option<WorkReminder>, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_reminder_set",
            &(item_id, at_ms, actor, agent),
            now,
            |tx| {
                let value = set_reminder(tx, item_id, at_ms, actor, agent, now)?;
                Ok(Done {
                    value,
                    effects: Effects {
                        items: vec![item_id.to_owned()],
                        ..Effects::default()
                    },
                })
            },
        )
    })
}

/// Marks reminders as notified so the app tells the user once. The app's
/// `work_reminders_notified`; returns the tasks it marked.
pub(crate) fn reminders_notified(
    app: &AppHandle,
    operation_id: &str,
    item_ids: &[String],
) -> Result<Vec<String>, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_reminders_notified",
            &item_ids,
            now,
            |tx| {
                let value = notified(tx, item_ids, now)?;
                Ok(Done {
                    effects: Effects {
                        items: value.clone(),
                        ..Effects::default()
                    },
                    value,
                })
            },
        )
    })
}

/// Puts a label in a group or out of one. The app's `work_label_group`.
pub(crate) fn label_group_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    group: Option<&str>,
) -> Result<WorkLabel, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_label_group",
            &(id, group),
            now,
            |tx| {
                Ok(Done {
                    value: group_label(tx, id, group, now)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Retires a label or restores it. The app's `work_label_retire`.
pub(crate) fn label_retire_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    retired: bool,
) -> Result<WorkLabel, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_label_retire",
            &(id, retired),
            now,
            |tx| {
                Ok(Done {
                    value: retire_label(tx, id, retired, now)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Sets what a label may go on. The app's `work_label_scope`.
pub(crate) fn label_scope_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    scope: LabelScope,
) -> Result<WorkLabel, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_label_scope",
            &(id, scope),
            now,
            |tx| {
                Ok(Done {
                    value: scope_label(tx, id, scope, now)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::bin;
    use super::super::items::{self, NewItem};
    use super::super::types::{WorkActor, WorkOwner, WorkStatus};
    use super::super::{Change, Effects};
    use super::{
        due_reminders, notified, save_template, set_reminder, use_template, WorkTemplateFields,
        WorkTemplateUse,
    };
    use rusqlite::Connection;

    const AT: i64 = 1_700_000_000_000;

    fn planner() -> Connection {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        conn
    }

    fn task(conn: &Connection) -> String {
        items::insert(
            conn,
            &NewItem {
                title: "Call the venue",
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            AT,
        )
        .expect("a task")
        .id
    }

    #[test]
    fn a_template_due_in_three_days_makes_a_task_due_three_days_out() {
        let conn = planner();
        let template = save_template(
            &conn,
            None,
            "Bugfix",
            &WorkTemplateFields {
                due_in_days: Some(3),
                ..WorkTemplateFields::default()
            },
            AT,
        )
        .expect("a template");

        let made = use_template(
            &conn,
            None,
            Change {
                operation_id: "use",
                actor: WorkActor::Me,
                at_ms: AT,
            },
            &template.id,
            &WorkTemplateUse::default(),
            &mut Effects::default(),
        )
        .expect("a task from the template");

        let three_days_out =
            items::day_start_ms(items::local_today(AT).expect("today"), 3).expect("a day");
        assert_eq!(made[0].due_at_ms, Some(three_days_out));
    }

    fn due(conn: &Connection, now_ms: i64) -> Vec<String> {
        due_reminders(conn, now_ms)
            .expect("due reminders")
            .into_iter()
            .map(|reminder| reminder.item_id)
            .collect()
    }

    #[test]
    fn a_reminder_set_again_after_it_was_shown_comes_due_again() {
        let conn = planner();
        let task = task(&conn);

        set_reminder(&conn, &task, Some(AT - 1), WorkActor::Me, None, AT).expect("a reminder");
        assert_eq!(due(&conn, AT), vec![task.clone()]);
        notified(&conn, &[task.clone()], AT).expect("marked shown");
        assert!(due(&conn, AT).is_empty());

        set_reminder(&conn, &task, Some(AT + 60_000), WorkActor::Me, None, AT).expect("set again");
        assert!(due(&conn, AT).is_empty());
        assert_eq!(due(&conn, AT + 60_000), vec![task]);
    }

    #[test]
    fn a_task_in_the_bin_reminds_no_one_until_it_comes_back() {
        let conn = planner();
        let task = task(&conn);
        set_reminder(&conn, &task, Some(AT - 1), WorkActor::Me, None, AT).expect("a reminder");

        bin::trash(&conn, &task, WorkActor::Me, AT).expect("the task in the bin");
        assert!(due(&conn, AT).is_empty());

        bin::untrash(&conn, &task, WorkActor::Me, AT).expect("the task restored");
        assert_eq!(due(&conn, AT), vec![task]);
    }
}
