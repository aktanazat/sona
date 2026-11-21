//! Task templates, a reminder on a task, project, initiative or note, and
//! label groups and retired labels.

use super::agents;
use super::inbox::{self, SubjectKind};
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

/// The reminders on projects, initiatives and notes: one each, posted to
/// the inbox once its time comes.
pub(crate) const SUBJECT_REMINDERS_MIGRATION: &str = "CREATE TABLE planning_subject_reminders (
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('project', 'initiative', 'note')),
    subject_id TEXT NOT NULL,
    remind_at_ms INTEGER NOT NULL,
    set_at_ms INTEGER NOT NULL,
    actor TEXT NOT NULL CHECK(actor IN ('me', 'sona', 'agent')),
    agent TEXT CHECK(agent IS NULL OR length(trim(agent)) > 0),
    notified_at_ms INTEGER,
    PRIMARY KEY(subject_kind, subject_id)
);
CREATE INDEX planning_subject_reminders_due
    ON planning_subject_reminders(notified_at_ms, remind_at_ms);
CREATE TRIGGER planning_subject_reminders_project_delete AFTER DELETE ON planning_projects
BEGIN
    DELETE FROM planning_subject_reminders WHERE subject_kind = 'project'
        AND subject_id = old.id;
END;
CREATE TRIGGER planning_subject_reminders_initiative_delete AFTER DELETE
    ON planning_initiatives
BEGIN
    DELETE FROM planning_subject_reminders WHERE subject_kind = 'initiative'
        AND subject_id = old.id;
END;";

/// A reminder on a project, initiative or note. Due once `remind_at_ms`
/// passes; posted to the inbox once, when `notified_at_ms` is set.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubjectReminder {
    pub(crate) subject_kind: SubjectKind,
    pub(crate) subject_id: String,
    pub(crate) remind_at_ms: i64,
    pub(crate) set_at_ms: i64,
    pub(crate) actor: WorkActor,
    pub(crate) agent: Option<String>,
    pub(crate) notified_at_ms: Option<i64>,
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

const SUBJECT_REMINDER_COLUMNS: &str =
    "subject_kind, subject_id, remind_at_ms, set_at_ms, actor, agent, notified_at_ms";

fn read_subject_reminder(row: &Row<'_>) -> rusqlite::Result<SubjectReminder> {
    Ok(SubjectReminder {
        subject_kind: row.get(0)?,
        subject_id: row.get(1)?,
        remind_at_ms: row.get(2)?,
        set_at_ms: row.get(3)?,
        actor: row.get(4)?,
        agent: row.get(5)?,
        notified_at_ms: row.get(6)?,
    })
}

/// Every reminder on a project, initiative or note, soonest first.
pub(crate) fn subject_reminders(conn: &Connection) -> StorageResult<Vec<SubjectReminder>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {SUBJECT_REMINDER_COLUMNS} FROM planning_subject_reminders
         ORDER BY remind_at_ms, subject_kind, subject_id"
    ))?;
    let rows = statement.query_map([], read_subject_reminder)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Those whose time has come and that were not posted yet, leaving out
/// projects in the bin.
pub(crate) fn due_subject_reminders(
    conn: &Connection,
    now_ms: i64,
) -> StorageResult<Vec<SubjectReminder>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {SUBJECT_REMINDER_COLUMNS} FROM planning_subject_reminders
         WHERE notified_at_ms IS NULL AND remind_at_ms <= ?1
           AND NOT (subject_kind = 'project' AND subject_id IN
                (SELECT id FROM planning_projects WHERE trashed_at_ms IS NOT NULL))
         ORDER BY remind_at_ms, subject_kind, subject_id"
    ))?;
    let rows = statement.query_map([now_ms], read_subject_reminder)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn subject_reminder(
    conn: &Connection,
    kind: SubjectKind,
    id: &str,
) -> StorageResult<Option<SubjectReminder>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {SUBJECT_REMINDER_COLUMNS} FROM planning_subject_reminders
                 WHERE subject_kind = ?1 AND subject_id = ?2"
            ),
            params![kind.as_str(), id],
            read_subject_reminder,
        )
        .optional()?)
}

/// The project, initiative or note, which must exist and not be in the bin.
fn reminded_subject(conn: &Connection, kind: SubjectKind, id: &str) -> StorageResult<String> {
    let id = store::uuid(id)?;
    let found = match kind {
        SubjectKind::Project => store::project(conn, &id)?.trashed_at_ms.is_none(),
        SubjectKind::Initiative => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM planning_initiatives WHERE id = ?1)",
            [&id],
            |row| row.get(0),
        )?,
        SubjectKind::Note => crate::scratchpad::note_with_connection(conn, &id)?.is_some(),
        SubjectKind::View => false,
    };
    if found {
        Ok(id)
    } else {
        Err(Refusal::NotFound.into())
    }
}

/// Sets the one reminder on a project, initiative or note, replacing any it
/// had, or with `None` clears it. Returns the reminder as it now is.
pub(crate) fn set_subject_reminder(
    conn: &Connection,
    kind: SubjectKind,
    id: &str,
    at_ms: Option<i64>,
    actor: WorkActor,
    agent: Option<&str>,
    now_ms: i64,
) -> StorageResult<Option<SubjectReminder>> {
    let id = reminded_subject(conn, kind, id)?;
    match at_ms {
        Some(at_ms) => conn.execute(
            "INSERT INTO planning_subject_reminders
                (subject_kind, subject_id, remind_at_ms, set_at_ms, actor, agent, notified_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
             ON CONFLICT(subject_kind, subject_id) DO UPDATE SET
                remind_at_ms = excluded.remind_at_ms, set_at_ms = excluded.set_at_ms,
                actor = excluded.actor, agent = excluded.agent, notified_at_ms = NULL",
            params![kind.as_str(), id, at_ms, now_ms, actor.as_str(), agent],
        )?,
        None => conn.execute(
            "DELETE FROM planning_subject_reminders WHERE subject_kind = ?1 AND subject_id = ?2",
            params![kind.as_str(), id],
        )?,
    };
    subject_reminder(conn, kind, &id)
}

/// Posts each reminder on a project, initiative or note whose time came to
/// the inbox, and marks it posted. Returns how many it posted.
pub(crate) fn post_subject_reminders(conn: &Connection, now_ms: i64) -> StorageResult<usize> {
    let mut posted = 0;
    for reminder in due_subject_reminders(conn, now_ms)? {
        if inbox::remind_subject(
            conn,
            reminder.subject_kind,
            &reminder.subject_id,
            reminder.remind_at_ms,
            reminder.set_at_ms,
            now_ms,
        )? {
            posted += 1;
        }
        conn.execute(
            "UPDATE planning_subject_reminders SET notified_at_ms = ?3
             WHERE subject_kind = ?1 AND subject_id = ?2",
            params![reminder.subject_kind.as_str(), reminder.subject_id, now_ms],
        )?;
    }
    Ok(posted)
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

/// Label descriptions and multi-select label groups.
pub(crate) const LABEL_EXTRAS_MIGRATION: &str =
    "ALTER TABLE planning_labels ADD COLUMN description TEXT;
     ALTER TABLE planning_labels ADD COLUMN group_multi INTEGER NOT NULL DEFAULT 0
         CHECK(group_multi IN (0, 1));";

/// Puts a label in a group, or with `None` takes it out of its group.
/// When `multi` is given for a group, every label in that group shares it.
/// `multi` false settles the group: each task, project and initiative
/// holding several of its labels keeps the first by name. A move that
/// leaves `multi` out is refused while it would leave something holding
/// two labels of a group that allows one (see [`group_clash`]). Returns
/// the label and what lost a label.
pub(crate) fn group_label(
    conn: &Connection,
    id: &str,
    group: Option<&str>,
    multi: Option<bool>,
    now_ms: i64,
) -> StorageResult<(WorkLabel, Labeled)> {
    let id = store::uuid(id)?;
    label(conn, &id)?;
    if !group_clash(conn, &id, group, multi)?.is_empty() {
        return Err(Refusal::LabelGroup.into());
    }
    let group = trimmed(group);
    let effective_multi = match (&group, multi) {
        (None, _) => false,
        (Some(_), Some(m)) => m,
        (Some(name), None) => allows_several(conn, &id, name)?,
    };
    conn.execute(
        "UPDATE planning_labels SET group_name = ?2, group_multi = ?3,
            revision = revision + 1, updated_at_ms = ?4
         WHERE id = ?1",
        params![id, group, effective_multi, now_ms],
    )?;
    if let (Some(name), Some(m)) = (&group, multi) {
        conn.execute(
            "UPDATE planning_labels SET group_multi = ?2, updated_at_ms = ?3
             WHERE lower(group_name) = lower(?1) AND id != ?4",
            params![name, m, now_ms, id],
        )?;
    }
    let settled = match (&group, multi) {
        (Some(name), Some(false)) => keep_one_from(conn, name)?,
        _ => Labeled::default(),
    };
    Ok((label(conn, &id)?, settled))
}

/// Tasks, projects and initiatives, by id.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Labeled {
    pub(crate) tasks: Vec<String>,
    pub(crate) projects: Vec<String>,
    pub(crate) initiatives: Vec<String>,
}

impl Labeled {
    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty() && self.projects.is_empty() && self.initiatives.is_empty()
    }

    /// Runs `owners` on each table of [`LABELED`]: the ids it returns are
    /// the tasks, the projects and the initiatives.
    fn each(
        mut owners: impl FnMut(&str, &str) -> StorageResult<Vec<String>>,
    ) -> StorageResult<Self> {
        let [(tasks, task), (projects, project), (initiatives, initiative)] = LABELED;
        Ok(Self {
            tasks: owners(tasks, task)?,
            projects: owners(projects, project)?,
            initiatives: owners(initiatives, initiative)?,
        })
    }
}

/// Whether the other labels in `group` let an item carry several of them.
fn allows_several(conn: &Connection, id: &str, group: &str) -> StorageResult<bool> {
    Ok(conn
        .query_row(
            "SELECT group_multi FROM planning_labels
             WHERE lower(group_name) = lower(?1) AND id != ?2 LIMIT 1",
            params![group, id],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// What moving label `id` into `group` would leave holding two labels of a
/// group that allows one: what carries `id` and another of the group's
/// labels. Empty when `multi` is given, since a move that sets it says
/// whether to keep several or settle.
pub(crate) fn group_clash(
    conn: &Connection,
    id: &str,
    group: Option<&str>,
    multi: Option<bool>,
) -> StorageResult<Labeled> {
    let id = store::uuid(id)?;
    let Some(group) = trimmed(group).filter(|_| multi.is_none()) else {
        return Ok(Labeled::default());
    };
    if allows_several(conn, &id, &group)? {
        return Ok(Labeled::default());
    }
    Labeled::each(|table, owner| {
        let mut statement = conn.prepare(&format!(
            "SELECT DISTINCT x.{owner} FROM {table} x
                 JOIN {table} y ON y.{owner} = x.{owner}
                 JOIN planning_labels l ON l.id = y.label_id
             WHERE x.label_id = ?1 AND y.label_id != ?1 AND lower(l.group_name) = lower(?2)
             ORDER BY 1"
        ))?;
        let owners = statement
            .query_map(params![&id, &group], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(owners)
    })
}

/// Leaves each item holding several labels from `group` with the first by
/// name. Returns what lost one.
fn keep_one_from(conn: &Connection, group: &str) -> StorageResult<Labeled> {
    Labeled::each(|table, owner| {
        let crowded = format!(
            "SELECT x.{owner}, x.label_id FROM {table} x
                 JOIN planning_labels l ON l.id = x.label_id
             WHERE lower(l.group_name) = lower(?1)
               AND EXISTS (SELECT 1 FROM {table} y
                   JOIN planning_labels m ON m.id = y.label_id
                   WHERE y.{owner} = x.{owner} AND lower(m.group_name) = lower(?1)
                     AND (lower(m.name), m.id) < (lower(l.name), l.id))"
        );
        let mut statement =
            conn.prepare(&format!("SELECT DISTINCT {owner} FROM ({crowded}) ORDER BY 1"))?;
        let settled = statement
            .query_map([group], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        conn.execute(
            &format!("DELETE FROM {table} WHERE ({owner}, label_id) IN ({crowded})"),
            [group],
        )?;
        Ok(settled)
    })
}

/// The tables that put a label on something, with the column naming it.
const LABELED: [(&str, &str); 3] = [
    ("planning_task_labels", "task_id"),
    ("planning_project_labels", "project_id"),
    ("planning_initiative_labels", "initiative_id"),
];

/// The saved JSON that can name a label: views' filters, automations'
/// triggers and conditions, and task and project templates.
const LABEL_REFERENCES: [(&str, &str); 5] = [
    ("planning_views", "query_json"),
    ("planning_loops", "trigger_json"),
    ("planning_loops", "conditions_json"),
    ("planning_task_templates", "fields"),
    ("planning_project_templates", "fields"),
];

/// Merges `from_id` into `into_id`: whatever carried `from_id` carries
/// `into_id`, saved views, automations and templates that named it name
/// `into_id`, and `from_id` is retired: it carries nothing and stays in the
/// label list for history, where it can be restored. Each item that moves
/// gets a change row and a new revision (see [`record_merge`]), and no
/// automation runs for the merge. Refused when `into_id` may not go where
/// `from_id` is, or when a task, project or initiative would hold two
/// labels from a group that allows one. Returns the surviving label and
/// the tasks that had `from_id`.
pub(crate) fn merge_labels(
    conn: &Connection,
    from_id: &str,
    into_id: &str,
    now_ms: i64,
) -> StorageResult<(WorkLabel, Vec<String>)> {
    let from_id = store::uuid(from_id)?;
    let into_id = store::uuid(into_id)?;
    if from_id == into_id {
        return Err(Refusal::Invalid.into());
    }
    label(conn, &from_id)?;
    let into = label(conn, &into_id)?;
    let mut stmt = conn
        .prepare("SELECT task_id FROM planning_task_labels WHERE label_id = ?1 ORDER BY task_id")?;
    let tasks: Vec<String> = stmt
        .query_map([&from_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let on_projects: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_project_labels WHERE label_id = ?1)
             OR EXISTS(SELECT 1 FROM planning_initiative_labels WHERE label_id = ?1)",
        [&from_id],
        |row| row.get(0),
    )?;
    let leaves_out = match into.scope {
        LabelScope::All => false,
        LabelScope::Tasks => on_projects,
        LabelScope::Projects => !tasks.is_empty(),
    };
    if leaves_out {
        return Err(Refusal::LabelScope.into());
    }
    if let (Some(group), false) = (&into.group, into.group_multi) {
        for (table, owner) in LABELED {
            let clash: bool = conn.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {table} f
                         JOIN {table} o ON o.{owner} = f.{owner}
                         JOIN planning_labels l ON l.id = o.label_id
                     WHERE f.label_id = ?1 AND o.label_id NOT IN (?1, ?2)
                       AND lower(l.group_name) = lower(?3))"
                ),
                params![&from_id, &into_id, group],
                |row| row.get(0),
            )?;
            if clash {
                return Err(Refusal::LabelGroup.into());
            }
        }
    }
    record_merge(conn, &from_id, &into_id, now_ms)?;
    for (table, owner) in LABELED {
        conn.execute(
            &format!(
                "UPDATE {table} SET label_id = ?2 WHERE label_id = ?1
                   AND {owner} NOT IN (SELECT {owner} FROM {table} WHERE label_id = ?2)"
            ),
            params![&from_id, &into_id],
        )?;
        conn.execute(&format!("DELETE FROM {table} WHERE label_id = ?1"), [&from_id])?;
    }
    for (table, column) in LABEL_REFERENCES {
        let mut statement = conn.prepare(&format!(
            "SELECT rowid, {column} FROM {table} WHERE instr({column}, ?1) > 0"
        ))?;
        let saved = statement
            .query_map([&from_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (rowid, json) in saved {
            let mut value: serde_json::Value = serde_json::from_str(&json)?;
            rename_label(&mut value, &from_id, &into_id);
            conn.execute(
                &format!("UPDATE {table} SET {column} = ?2 WHERE rowid = ?1"),
                params![rowid, value.to_string()],
            )?;
        }
    }
    conn.execute(
        "UPDATE planning_labels SET retired_at_ms = COALESCE(retired_at_ms, ?2),
            revision = revision + 1, updated_at_ms = ?2
         WHERE id = ?1",
        params![&from_id, now_ms],
    )?;
    conn.execute(
        "UPDATE planning_labels SET revision = revision + 1, updated_at_ms = ?2 WHERE id = ?1",
        params![&into_id, now_ms],
    )?;
    Ok((label(conn, &into_id)?, tasks))
}

/// Writes the change row and the revision a merge owes each item carrying
/// `from_id`, before it moves. The row holds the labels the item had and,
/// as `merged_label`, the label merged away, which the label-added
/// automation passes over; it is stamped on the clock the change triggers
/// use. A task that already carries `into_id` only loses `from_id`, and the
/// task label table's own trigger records that.
fn record_merge(conn: &Connection, from_id: &str, into_id: &str, now_ms: i64) -> StorageResult<()> {
    conn.execute(
        "INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
         SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', f.task_id,
             f.task_id, 'update', json_object(
                 'labels', json((SELECT json_group_array(label_id) FROM planning_task_labels
                     WHERE task_id = f.task_id)),
                 'merged_label', ?1)
         FROM planning_task_labels f
         WHERE f.label_id = ?1
           AND f.task_id NOT IN (SELECT task_id FROM planning_task_labels WHERE label_id = ?2)",
        params![from_id, into_id],
    )?;
    for (table, owner, entity) in [
        ("planning_project_labels", "project_id", "project"),
        ("planning_initiative_labels", "initiative_id", "initiative"),
    ] {
        conn.execute(
            &format!(
                "INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from)
                 SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), '{entity}',
                     f.{owner}, 'update', json_object(
                         'labels', json((SELECT json_group_array(label_id) FROM {table}
                             WHERE {owner} = f.{owner})),
                         'merged_label', ?1)
                 FROM {table} f WHERE f.label_id = ?1"
            ),
            [from_id],
        )?;
    }
    conn.execute(
        "UPDATE planning_tasks SET revision = revision + 1, updated_at_ms = ?2
         WHERE id IN (SELECT task_id FROM planning_task_labels WHERE label_id = ?1)",
        params![from_id, now_ms],
    )?;
    conn.execute(
        "UPDATE planning_projects SET revision = revision + 1, updated_at_ms = ?2
         WHERE id IN (SELECT project_id FROM planning_project_labels WHERE label_id = ?1)",
        params![from_id, now_ms],
    )?;
    conn.execute(
        "UPDATE planning_initiatives SET updated_at_ms = ?2
         WHERE id IN (SELECT initiative_id FROM planning_initiative_labels WHERE label_id = ?1)",
        params![from_id, now_ms],
    )?;
    Ok(())
}

/// Puts `into` wherever `from` is in saved JSON, keeping one `into` in a
/// list that then names it twice.
fn rename_label(value: &mut serde_json::Value, from: &str, into: &str) {
    match value {
        serde_json::Value::String(text) if text == from => into.clone_into(text),
        serde_json::Value::Array(values) => {
            values.iter_mut().for_each(|value| rename_label(value, from, into));
            let mut seen = false;
            values.retain(|value| {
                let again = seen && value.as_str() == Some(into);
                seen |= value.as_str() == Some(into);
                !again
            });
        }
        serde_json::Value::Object(fields) => {
            fields.values_mut().for_each(|value| rename_label(value, from, into));
        }
        _ => {}
    }
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

/// Every reminder on a project, initiative or note. The app's
/// `work_subject_reminders`.
pub(crate) fn subject_reminders_read(app: &AppHandle) -> Result<Vec<SubjectReminder>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        subject_reminders(conn)
    })
}

/// Sets or with `None` clears the reminder on a project, initiative or
/// note, as `actor`. The app's `work_subject_reminder_set`.
pub(crate) fn subject_reminder_set_as(
    app: &AppHandle,
    actor: WorkActor,
    agent: Option<&str>,
    operation_id: &str,
    kind: SubjectKind,
    id: &str,
    at_ms: Option<i64>,
) -> Result<Option<SubjectReminder>, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_subject_reminder_set",
            &(kind, id, at_ms, actor, agent),
            now,
            |tx| {
                Ok(Done {
                    value: set_subject_reminder(tx, kind, id, at_ms, actor, agent, now)?,
                    effects: Effects::default(),
                })
            },
        )
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
/// Also returns what kept one label from a group that now allows one.
pub(crate) fn label_group_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    group: Option<&str>,
    multi: Option<bool>,
) -> Result<(WorkLabel, Labeled), String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_label_group",
            &(id, group, multi),
            now,
            |tx| {
                let (label, settled) = group_label(tx, id, group, multi, now)?;
                Ok(Done {
                    value: (label, settled.clone()),
                    effects: Effects {
                        items: settled.tasks,
                        projects: settled.projects,
                        ..Effects::default()
                    },
                })
            },
        )
    })
}

/// Merges `from_id` into `into_id` (see [`merge_labels`]).
pub(crate) fn label_merge_as(
    app: &AppHandle,
    operation_id: &str,
    from_id: &str,
    into_id: &str,
) -> Result<(WorkLabel, Vec<String>), String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_label_merge",
            &(from_id, into_id),
            now,
            |tx| {
                let (label, items) = merge_labels(tx, from_id, into_id, now)?;
                Ok(Done {
                    value: (label, items.clone()),
                    effects: Effects {
                        items,
                        ..Effects::default()
                    },
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

#[cfg(test)]
mod label_tests {
    use super::super::super::schema;
    use super::super::items::{self, NewItem};
    use super::super::types::{WorkActivityKind, WorkActor, WorkOwner, WorkPatch, WorkStatus};
    use super::{group_clash, group_label, merge_labels, scope_label, Labeled};
    use crate::planning::store::Refusal;
    use crate::planning::types::LabelScope;
    use rusqlite::Connection;

    const AT: i64 = 1_700_000_000_000;

    fn planner() -> Connection {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        conn
    }

    fn label(conn: &Connection, name: &str) -> String {
        items::save_label(conn, None, name, 0.0, None, AT)
            .expect("a label")
            .id
    }

    fn task(conn: &Connection, labels: &[String]) -> String {
        items::insert(
            conn,
            &NewItem {
                title: "Ship the brochure",
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: labels,
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            AT,
        )
        .expect("a task")
        .id
    }

    fn labelled(conn: &Connection, id: &str, labels: &[String]) -> Vec<String> {
        let patch = WorkPatch {
            label_ids: Some(labels.to_vec()),
            ..WorkPatch::default()
        };
        let changed = items::apply(
            conn,
            id,
            &patch,
            WorkActivityKind::Status,
            WorkActor::Me,
            AT,
        )
        .expect("labelled");
        changed.after.label_ids
    }

    #[test]
    fn a_multi_select_group_keeps_several_of_its_labels_on_a_task() {
        let conn = planner();
        let print = label(&conn, "Print");
        let web = label(&conn, "Web");
        group_label(&conn, &print, Some("Channel"), None, AT).expect("grouped");
        group_label(&conn, &web, Some("Channel"), None, AT).expect("grouped");
        let id = task(&conn, &[]);
        let both = [print.clone(), web.clone()];

        assert_eq!(labelled(&conn, &id, &both).len(), 1);
        group_label(&conn, &web, Some("Channel"), Some(true), AT).expect("multi");

        let mut kept = labelled(&conn, &id, &both);
        kept.sort();
        let mut expected = both.to_vec();
        expected.sort();
        assert_eq!(kept, expected);
    }

    #[test]
    fn a_group_back_to_one_label_leaves_each_item_the_first_by_name() {
        let conn = planner();
        let web = label(&conn, "Web");
        let print = label(&conn, "Print");
        group_label(&conn, &web, Some("Channel"), Some(true), AT).expect("multi");
        group_label(&conn, &print, Some("Channel"), None, AT).expect("grouped");
        let id = task(&conn, &[web.clone(), print.clone()]);
        both_on_a_project_and_an_initiative(&conn, &web, &print);
        assert_eq!(items::item(&conn, &id).expect("task").label_ids.len(), 2);

        let (_, settled) =
            group_label(&conn, &web, Some("Channel"), Some(false), AT).expect("one per task");

        assert_eq!(
            settled,
            Labeled {
                tasks: vec![id.clone()],
                projects: vec!["harbor".to_owned()],
                initiatives: vec!["launch".to_owned()],
            }
        );
        assert_eq!(items::item(&conn, &id).expect("task").label_ids, vec![print]);
    }

    fn both_on_a_project_and_an_initiative(conn: &Connection, one: &str, other: &str) {
        for label in [one, other] {
            conn.execute(
                "INSERT INTO planning_project_labels (project_id, label_id) VALUES ('harbor', ?1)",
                [label],
            )
            .expect("a project label");
            conn.execute(
                "INSERT INTO planning_initiative_labels (initiative_id, label_id)
                 VALUES ('launch', ?1)",
                [label],
            )
            .expect("an initiative label");
        }
    }

    #[test]
    fn moving_a_label_into_a_one_label_group_it_clashes_with_is_refused_until_settled() {
        let conn = planner();
        let web = label(&conn, "Web");
        let print = label(&conn, "Print");
        group_label(&conn, &print, Some("Channel"), None, AT).expect("grouped");
        let id = task(&conn, &[web.clone(), print.clone()]);
        both_on_a_project_and_an_initiative(&conn, &web, &print);

        let clash = group_clash(&conn, &web, Some("Channel"), None).expect("a clash");
        let refused = group_label(&conn, &web, Some("Channel"), None, AT).expect_err("a clash");

        assert_eq!(
            clash,
            Labeled {
                tasks: vec![id.clone()],
                projects: vec!["harbor".to_owned()],
                initiatives: vec!["launch".to_owned()],
            }
        );
        assert!(matches!(refusal(&refused), Some(Refusal::LabelGroup)));
        assert_eq!(items::item(&conn, &id).expect("task").label_ids.len(), 2);
        let web_label = items::labels(&conn).expect("labels").into_iter().find(|l| l.id == web);
        assert_eq!(web_label.expect("web").group, None);

        let (_, settled) =
            group_label(&conn, &web, Some("Channel"), Some(false), AT).expect("settled");

        assert_eq!(settled.tasks, vec![id.clone()]);
        assert_eq!(items::item(&conn, &id).expect("task").label_ids, vec![print]);
    }

    fn retired(conn: &Connection, id: &str) -> bool {
        items::labels(conn)
            .expect("labels")
            .iter()
            .any(|label| label.id == id && label.retired_at_ms.is_some())
    }

    fn refusal(error: &anyhow::Error) -> Option<&Refusal> {
        error.downcast_ref::<Refusal>()
    }

    #[test]
    fn merging_a_label_moves_its_items_and_names_records_each_and_retires_it() {
        let conn = planner();
        let bug = label(&conn, "Bug");
        let defect = label(&conn, "Defect");
        let only_bug = task(&conn, std::slice::from_ref(&bug));
        let both = task(&conn, &[bug.clone(), defect.clone()]);
        let project = crate::planning::store::insert_project(&conn, "Harbor", None, AT)
            .expect("a project")
            .id;
        conn.execute(
            "INSERT INTO planning_project_labels (project_id, label_id) VALUES (?1, ?2)",
            [&project, &bug],
        )
        .expect("a project label");
        conn.execute(
            "INSERT INTO planning_task_templates (id, name, fields, revision, created_at_ms,
                updated_at_ms) VALUES ('t', 'Triage', ?1, 1, 0, 0)",
            [serde_json::json!({ "labelIds": [bug, defect] }).to_string()],
        )
        .expect("a template naming both");
        let changes = |id: &str| -> Vec<serde_json::Value> {
            conn.prepare(
                "SELECT coalesce(updated_from, 'null') FROM planning_changes
                 WHERE entity_id = ?1 ORDER BY seq",
            )
            .expect("a query")
            .query_map([id], |row| row.get::<_, String>(0))
            .expect("changes")
            .map(|row| serde_json::from_str(&row.expect("a change")).expect("json"))
            .collect()
        };
        let revision = |table: &str, id: &str| -> i64 {
            conn.query_row(&format!("SELECT revision FROM {table} WHERE id = ?1"), [id], |row| {
                row.get(0)
            })
            .expect("a revision")
        };
        let before = [&only_bug, &both, &project].map(|id| changes(id).len());
        let revisions = [
            revision("planning_tasks", &only_bug),
            revision("planning_tasks", &both),
            revision("planning_projects", &project),
        ];

        let (kept, moved) = merge_labels(&conn, &bug, &defect, AT + 1).expect("merged");

        assert_eq!(kept.id, defect);
        assert_eq!(moved.len(), 2);
        for id in [&only_bug, &both] {
            assert_eq!(items::item(&conn, id).expect("task").label_ids, vec![defect.clone()]);
        }
        let after = [&only_bug, &both, &project].map(|id| changes(id).len());
        assert_eq!(after, before.map(|n| n + 1));
        let merged = serde_json::json!({ "labels": [bug], "merged_label": bug });
        assert_eq!(changes(&only_bug).last(), Some(&merged));
        assert_eq!(changes(&project).last(), Some(&merged));
        assert_eq!(
            [
                revision("planning_tasks", &only_bug),
                revision("planning_tasks", &both),
                revision("planning_projects", &project),
            ],
            revisions.map(|n| n + 1)
        );
        let fields: String = conn
            .query_row("SELECT fields FROM planning_task_templates", [], |row| row.get(0))
            .expect("the template");
        assert_eq!(fields, serde_json::json!({ "labelIds": [defect] }).to_string());
        assert!(retired(&conn, &bug));
    }

    #[test]
    fn a_merge_that_breaks_scope_or_a_one_label_group_is_refused() {
        let conn = planner();
        let bug = label(&conn, "Bug");
        let defect = label(&conn, "Defect");
        let urgent = label(&conn, "Urgent");
        task(&conn, &[bug.clone(), urgent.clone()]);
        scope_label(&conn, &defect, LabelScope::Projects, AT).expect("projects only");
        let scoped = merge_labels(&conn, &bug, &defect, AT).expect_err("scope");
        assert!(matches!(refusal(&scoped), Some(Refusal::LabelScope)));

        scope_label(&conn, &defect, LabelScope::All, AT).expect("anywhere");
        group_label(&conn, &defect, Some("Kind"), None, AT).expect("grouped");
        group_label(&conn, &urgent, Some("Kind"), None, AT).expect("grouped");
        let crowded = merge_labels(&conn, &bug, &defect, AT).expect_err("group");
        assert!(matches!(refusal(&crowded), Some(Refusal::LabelGroup)));
        assert!(!retired(&conn, &bug));
    }
}
