//! Talk to Sona's side of the tracker: the reads its lookups run and the
//! changes its cards make.
//!
//! What a lookup returns leaves this Mac for the relay, so every read here
//! takes the tasks a kept-local series hides and leaves them out. A change
//! runs only after the owner presses Apply on its card; it goes through the
//! same tracker functions the app's own screens call, filed as an agent's
//! change named [`CHAT_AGENT`] that came through the chat, and each one
//! returns what putting it back takes.

use super::types::{
    WorkActivity, WorkActor, WorkItem, WorkItemInput, WorkPatch, WorkStatus, WorkVia,
};
use super::{agents, bin, cycles, initiatives, items, now_ms};
use crate::managers::history::HistoryManager;
use crate::planning::catalog::ProjectEdit;
use crate::planning::store;
use crate::planning::types::Project;
use crate::scratchpad::{self, Author, ScratchNote};
use anyhow::Result;
use chrono::{Local, NaiveDate, TimeZone};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// The agent a change made from Talk to Sona is filed under.
pub(crate) const CHAT_AGENT: &str = "Talk to Sona";

const SNIPPET_CHARS: usize = 160;
const DETAILS_CHARS: usize = 1_500;
const NOTE_CHARS: usize = 4_000;
const ACTIVITY_ENTRIES: usize = 10;

/// Which tasks a listing keeps by status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StatusFilter {
    /// Triage, backlog, todo and in progress.
    Open,
    Closed,
    All,
    Is(WorkStatus),
}

impl StatusFilter {
    pub(crate) fn named(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "all" => Some(Self::All),
            other => status_named(other).map(Self::Is),
        }
    }

    fn holds(self, status: WorkStatus) -> bool {
        match self {
            Self::Open => !status.is_closed(),
            Self::Closed => status.is_closed(),
            Self::All => true,
            Self::Is(wanted) => status == wanted,
        }
    }
}

/// A status as the chat writes it; `todo` is the tracker's not-started.
pub(crate) fn status_named(value: &str) -> Option<WorkStatus> {
    match value {
        "triage" => Some(WorkStatus::Triage),
        "backlog" => Some(WorkStatus::Backlog),
        "todo" => Some(WorkStatus::NotStarted),
        "in_progress" => Some(WorkStatus::InProgress),
        "done" => Some(WorkStatus::Done),
        "canceled" => Some(WorkStatus::Canceled),
        "duplicate" => Some(WorkStatus::Duplicate),
        _ => None,
    }
}

const fn status_name(status: WorkStatus) -> &'static str {
    match status {
        WorkStatus::Triage => "triage",
        WorkStatus::Backlog => "backlog",
        WorkStatus::NotStarted => "todo",
        WorkStatus::InProgress => "in_progress",
        WorkStatus::Done => "done",
        WorkStatus::Canceled => "canceled",
        WorkStatus::Duplicate => "duplicate",
    }
}

const fn priority_name(priority: u8) -> &'static str {
    match priority {
        1 => "urgent",
        2 => "high",
        3 => "medium",
        4 => "low",
        _ => "none",
    }
}

fn cut(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_owned(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let (cut, more) = cut(&flat, SNIPPET_CHARS);
    if more {
        format!("{cut}…")
    } else {
        cut
    }
}

fn local_day(utc_ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(utc_ms)
        .single()
        .map(|at| at.format("%Y-%m-%d").to_string())
}

/// Local midnight of `day`, the way the tracker stores a due day.
pub(crate) fn day_start(day: NaiveDate) -> Option<i64> {
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|at| at.timestamp_millis())
}

fn handle(number: i64) -> String {
    format!("S-{number}")
}

fn project_names(conn: &Connection) -> Result<HashMap<String, String>> {
    Ok(store::projects(conn, false)?
        .into_iter()
        .map(|summary| (summary.project.id, summary.project.title))
        .collect())
}

fn task_row(item: &WorkItem, projects: &HashMap<String, String>) -> Value {
    json!({
        "task_id": item.id,
        "handle": handle(item.number),
        "title": item.title,
        "status": status_name(item.status),
        "priority": priority_name(item.priority),
        "due": item.due_at_ms.and_then(local_day),
        "project_id": item.project_id,
        "project": item.project_id.as_ref().and_then(|id| projects.get(id)),
        "cycle": item.cycle,
        "link": crate::query::task_link(&item.id),
    })
}

/// Most pressing first: open before closed, then by priority with none
/// last, then the nearest due day, then the oldest.
fn urgency(item: &WorkItem) -> (bool, u8, i64, i64) {
    (
        item.status.is_closed(),
        if item.priority == 0 { 5 } else { item.priority },
        item.due_at_ms.unwrap_or(i64::MAX),
        item.number,
    )
}

/// Tasks outside the archive and the bin that `hidden` does not name.
pub(crate) fn tasks(
    conn: &Connection,
    hidden: &HashSet<String>,
    search: Option<&str>,
    status: StatusFilter,
    project_id: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let projects = project_names(conn)?;
    let search = search
        .map(str::trim)
        .filter(|search| !search.is_empty())
        .map(str::to_lowercase);
    let mut found: Vec<WorkItem> = items::items(conn, false)?
        .into_iter()
        .filter(|item| {
            !hidden.contains(&item.id)
                && status.holds(item.status)
                && project_id.is_none_or(|id| item.project_id.as_deref() == Some(id))
                && search.as_deref().is_none_or(|search| {
                    item.title.to_lowercase().contains(search)
                        || item
                            .details
                            .as_deref()
                            .is_some_and(|details| details.to_lowercase().contains(search))
                        || handle(item.number).eq_ignore_ascii_case(search)
                })
        })
        .collect();
    found.sort_by_key(urgency);
    let total = found.len();
    let rows: Vec<Value> = found
        .iter()
        .take(limit)
        .map(|item| task_row(item, &projects))
        .collect();
    Ok(json!({ "rows": rows, "total": total, "more": total > limit }))
}

/// The task `reference` names, as an id, `S-12` or `12`, when `hidden` does
/// not name it.
pub(crate) fn find_task(
    conn: &Connection,
    hidden: &HashSet<String>,
    reference: &str,
) -> Result<Option<WorkItem>> {
    let reference = reference.trim();
    let number = reference
        .strip_prefix("S-")
        .or_else(|| reference.strip_prefix("s-"))
        .unwrap_or(reference)
        .parse::<i64>()
        .ok();
    let found = items::items(conn, true)?.into_iter().find(|item| {
        item.id.eq_ignore_ascii_case(reference)
            || number.is_some_and(|number| item.number == number)
    });
    Ok(found.filter(|item| !hidden.contains(&item.id)))
}

fn who(entry: &WorkActivity) -> String {
    match entry.actor {
        WorkActor::Me => "you".to_owned(),
        WorkActor::Sona => "Sona".to_owned(),
        WorkActor::Agent => {
            let name = entry.agent.as_deref().unwrap_or(agents::UNNAMED);
            match entry.via {
                Some(via) => format!("{name} via {}", via.text()),
                None => name.to_owned(),
            }
        }
    }
}

/// One task with its description, labels and newest history.
pub(crate) fn task(
    conn: &Connection,
    hidden: &HashSet<String>,
    reference: &str,
) -> Result<Option<Value>> {
    let Some(item) = find_task(conn, hidden, reference)? else {
        return Ok(None);
    };
    let projects = project_names(conn)?;
    let labels: HashMap<String, String> = items::labels(conn)?
        .into_iter()
        .map(|label| (label.id, label.name))
        .collect();
    let mut row = task_row(&item, &projects);
    let (details, details_cut) = cut(item.details.as_deref().unwrap_or_default(), DETAILS_CHARS);
    let activity: Vec<Value> = items::activity(conn, &item.id)?
        .iter()
        .rev()
        .take(ACTIVITY_ENTRIES)
        .map(|entry| {
            json!({
                "when": local_day(entry.at_ms),
                "who": who(entry),
                "kind": entry.kind,
                "from": entry.from,
                "to": entry.to,
            })
        })
        .collect();
    if let Value::Object(fields) = &mut row {
        fields.insert("details".into(), json!(details));
        fields.insert("details_truncated".into(), json!(details_cut));
        fields.insert(
            "labels".into(),
            json!(item
                .label_ids
                .iter()
                .filter_map(|id| labels.get(id))
                .collect::<Vec<_>>()),
        );
        fields.insert("archived".into(), json!(item.archived_at_ms.is_some()));
        fields.insert("activity".into(), json!(activity));
    }
    Ok(Some(row))
}

/// Projects outside the bin, newest activity first as the tracker orders
/// them.
pub(crate) fn projects(conn: &Connection, limit: usize) -> Result<Value> {
    let summaries = store::projects(conn, false)?;
    let total = summaries.len();
    let rows: Vec<Value> = summaries
        .iter()
        .filter(|summary| summary.project.archived_at_ms.is_none())
        .take(limit)
        .map(|summary| {
            let project = &summary.project;
            json!({
                "project_id": project.id,
                "name": project.title,
                "status": project.status,
                "summary": project.summary,
                "description": project.description.as_deref().map(snippet),
                "priority": priority_name(project.priority),
                "target": project.target_at_ms.and_then(local_day),
                "open_tasks": summary.open_task_count,
                "done_tasks": summary.completed_task_count,
            })
        })
        .collect();
    Ok(json!({ "rows": rows, "total": total, "more": total > limit }))
}

pub(crate) fn cycles(conn: &Connection, now_ms: i64) -> Result<Value> {
    let rows: Vec<Value> = cycles::all(conn, now_ms)?
        .iter()
        .map(|cycle| {
            json!({
                "number": cycle.number,
                "name": cycle.name,
                "start": cycle.start_day,
                "end": cycle.end_day,
                "state": cycle.state,
                "scope": cycle.scope,
                "started": cycle.started,
                "done": cycle.done,
            })
        })
        .collect();
    Ok(json!({ "rows": rows }))
}

pub(crate) fn initiatives(conn: &Connection) -> Result<Value> {
    let projects = project_names(conn)?;
    let rows: Vec<Value> = initiatives::initiatives(conn)?
        .iter()
        .map(|initiative| {
            json!({
                "initiative_id": initiative.id,
                "name": initiative.name,
                "status": initiative.status,
                "description": initiative.description.as_deref().map(snippet),
                "target": initiative.target_at_ms.and_then(local_day),
                "projects": initiative
                    .project_ids
                    .iter()
                    .filter_map(|id| projects.get(id))
                    .collect::<Vec<_>>(),
                "progress": initiative.progress,
            })
        })
        .collect();
    Ok(json!({ "rows": rows }))
}

/// The notes Talk to Sona may read: every note in the scratchpad.
pub(crate) fn readable_notes(conn: &Connection, search: &str) -> Result<Vec<ScratchNote>> {
    scratchpad::list_notes_with_connection(conn, search)
}

pub(crate) fn notes(conn: &Connection, search: Option<&str>, limit: usize) -> Result<Value> {
    let found = readable_notes(conn, search.unwrap_or_default())?;
    let total = found.len();
    let rows: Vec<Value> = found
        .iter()
        .take(limit)
        .map(|note| {
            json!({
                "note_id": note.id,
                "title": note.title,
                "updated": local_day(note.updated_at_ms),
                "snippet": snippet(&note.body),
                "link": crate::query::note_link(&note.id),
            })
        })
        .collect();
    Ok(json!({ "rows": rows, "total": total, "more": total > limit }))
}

pub(crate) fn note(conn: &Connection, note_id: &str) -> Result<Option<Value>> {
    let Some(note) = readable_notes(conn, "")?
        .into_iter()
        .find(|note| note.id == note_id)
    else {
        return Ok(None);
    };
    let (body, truncated) = cut(&note.body, NOTE_CHARS);
    Ok(Some(json!({
        "note_id": note.id,
        "title": note.title,
        "updated": local_day(note.updated_at_ms),
        "body": body,
        "truncated": truncated,
        "link": crate::query::note_link(&note.id),
    })))
}

/* ----------------------------------------------------------- card targets */

/// A task as a card names it: its handle, its title and the name of the
/// project it is in. `None` when no task has this id.
pub(crate) fn task_named(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, String, Option<String>)>> {
    let Ok(id) = store::uuid(id) else {
        return Ok(None);
    };
    let Some((number, title, project_id)) = conn
        .query_row(
            "SELECT number, title, project_id FROM planning_tasks WHERE id = ?1",
            [&id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let project = match project_id {
        Some(project_id) => project_named(conn, &project_id)?,
        None => None,
    };
    Ok(Some((handle(number), title, project)))
}

pub(crate) fn project_named(conn: &Connection, id: &str) -> Result<Option<String>> {
    let Ok(id) = store::uuid(id) else {
        return Ok(None);
    };
    Ok(conn
        .query_row(
            "SELECT title FROM planning_projects WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn note_named(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(scratchpad::note_with_connection(conn, id)?.map(|note| note.title))
}

/* ---------------------------------------------------------------- changes */

/// A due day to set, or none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DueEdit {
    Day(NaiveDate),
    Clear,
}

impl DueEdit {
    /// `YYYY-MM-DD`, or `none` to clear it.
    pub(crate) fn named(value: &str) -> Option<Self> {
        if value == "none" {
            return Some(Self::Clear);
        }
        NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .ok()
            .map(Self::Day)
    }

    fn at_ms(self) -> std::result::Result<Option<i64>, String> {
        match self {
            Self::Clear => Ok(None),
            Self::Day(day) => day_start(day)
                .map(Some)
                .ok_or_else(|| "That due day doesn't exist here.".to_owned()),
        }
    }
}

/// The fields a card may set on a task. `None` keeps a field; an empty
/// description clears it, and a project of `None` inside `Some` takes the
/// task out of its project.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TaskEdit {
    pub(crate) title: Option<String>,
    pub(crate) details: Option<String>,
    pub(crate) status: Option<WorkStatus>,
    pub(crate) priority: Option<u8>,
    pub(crate) due: Option<DueEdit>,
    pub(crate) project_id: Option<Option<String>>,
}

/// The fields a card changed on a task: what they held before, for putting
/// them back, or what the card wrote, for checking nobody changed them since.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskFields {
    title: Option<String>,
    details: Option<Option<String>>,
    status: Option<WorkStatus>,
    priority: Option<u8>,
    due_at_ms: Option<Option<i64>>,
    project_id: Option<Option<String>>,
}

impl TaskFields {
    fn written(patch: &WorkPatch) -> Self {
        Self {
            title: patch.title.clone(),
            details: patch.details.clone(),
            status: patch.status,
            priority: patch.priority,
            due_at_ms: patch.due_at_ms,
            project_id: patch.project_id.clone(),
        }
    }

    fn patch(&self) -> WorkPatch {
        WorkPatch {
            title: self.title.clone(),
            details: self.details.clone(),
            status: self.status,
            priority: self.priority,
            due_at_ms: self.due_at_ms,
            project_id: self.project_id.clone(),
            ..WorkPatch::default()
        }
    }

    /// Whether `item` still holds every field named here.
    fn held_by(&self, item: &WorkItem) -> bool {
        self.title.as_ref().is_none_or(|title| *title == item.title)
            && self
                .details
                .as_ref()
                .is_none_or(|details| *details == item.details)
            && self.status.is_none_or(|status| status == item.status)
            && self
                .priority
                .is_none_or(|priority| priority == item.priority)
            && self.due_at_ms.is_none_or(|due| due == item.due_at_ms)
            && self
                .project_id
                .as_ref()
                .is_none_or(|project_id| *project_id == item.project_id)
    }
}

/// Why Undo leaves a task, project or note the chat changed as it is.
const CHANGED_SINCE: &str = "This changed again after the chat's edit, so Undo would overwrite the newer change. Change it by hand instead.";

/// The patch an edit makes to `item`, with what it replaces. Fields the
/// edit sets to what the task already holds are left out of both.
pub(crate) fn task_patch(
    item: &WorkItem,
    edit: &TaskEdit,
) -> std::result::Result<(WorkPatch, TaskFields), String> {
    let mut patch = WorkPatch::default();
    let mut prior = TaskFields {
        title: None,
        details: None,
        status: None,
        priority: None,
        due_at_ms: None,
        project_id: None,
    };
    if let Some(title) = edit.title.as_deref().map(str::trim) {
        if title.is_empty() {
            return Err("A task needs a title.".to_owned());
        }
        if title != item.title {
            patch.title = Some(title.to_owned());
            prior.title = Some(item.title.clone());
        }
    }
    if let Some(details) = &edit.details {
        let details = Some(details.trim().to_owned()).filter(|details| !details.is_empty());
        if details != item.details {
            patch.details = Some(details);
            prior.details = Some(item.details.clone());
        }
    }
    if let Some(status) = edit.status.filter(|status| *status != item.status) {
        patch.status = Some(status);
        prior.status = Some(item.status);
    }
    if let Some(priority) = edit.priority.filter(|priority| *priority != item.priority) {
        if priority > 4 {
            return Err("Priority runs from 0 (none) to 4 (low).".to_owned());
        }
        patch.priority = Some(priority);
        prior.priority = Some(item.priority);
    }
    if let Some(due) = edit.due {
        let due = due.at_ms()?;
        if due != item.due_at_ms {
            patch.due_at_ms = Some(due);
            prior.due_at_ms = Some(item.due_at_ms);
        }
    }
    if let Some(project_id) = edit
        .project_id
        .as_ref()
        .filter(|project_id| **project_id != item.project_id)
    {
        patch.project_id = Some(project_id.clone());
        prior.project_id = Some(item.project_id.clone());
    }
    Ok((patch, prior))
}

/// Names [`CHAT_AGENT`], through the chat, on the history entries the
/// change since `since_ms` wrote for each task.
fn credit(app: &AppHandle, ids: &[String], since_ms: i64) -> std::result::Result<(), String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        let tx = conn.transaction()?;
        for id in ids {
            agents::credit(&tx, id, since_ms, CHAT_AGENT, Some(WorkVia::Chat))?;
        }
        tx.commit()?;
        Ok(())
    })?;
    super::tasks_changed(app, ids.to_vec());
    Ok(())
}

fn read_item(app: &AppHandle, id: &str) -> std::result::Result<WorkItem, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        items::item(conn, &store::uuid(id)?)
    })
}

/// A new task in Triage's place for work the owner asked the chat to add:
/// it lands as Todo, since the owner pressed Apply on it.
pub(crate) async fn create_task(
    app: &AppHandle,
    operation_id: &str,
    title: &str,
    details: Option<&str>,
    project_id: Option<&str>,
    priority: Option<u8>,
    due: Option<DueEdit>,
) -> std::result::Result<WorkItem, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("A task needs a title.".to_owned());
    }
    if priority.is_some_and(|priority| priority > 4) {
        return Err("Priority runs from 0 (none) to 4 (low).".to_owned());
    }
    let input = WorkItemInput {
        title: title.to_owned(),
        details: details
            .map(str::trim)
            .filter(|details| !details.is_empty())
            .map(str::to_owned),
        status: Some(WorkStatus::NotStarted),
        priority,
        project_id: project_id.map(str::to_owned),
        milestone_id: None,
        owner: None,
        due_at_ms: due.map(DueEdit::at_ms).transpose()?.flatten(),
        label_ids: None,
        recurrence: None,
    };
    let since = now_ms();
    let item = super::create_as(app, WorkActor::Agent, operation_id, &input).await?;
    credit(app, std::slice::from_ref(&item.id), since)?;
    Ok(item)
}

/// Moves a task the chat added to the bin, where it can still be restored.
pub(crate) fn remove_created_task(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> std::result::Result<(), String> {
    let since = now_ms();
    bin::set_trashed_as(app, WorkActor::Agent, operation_id, &[id.to_owned()], true)?;
    credit(app, &[id.to_owned()], since)
}

/// Changes one task, and returns what the changed fields held before and
/// what they hold now.
pub(crate) async fn update_task(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    edit: &TaskEdit,
) -> std::result::Result<(TaskFields, TaskFields), String> {
    let item = read_item(app, id)?;
    let (patch, prior) = task_patch(&item, edit)?;
    let written = TaskFields::written(&patch);
    if patch == WorkPatch::default() {
        return Ok((prior, written));
    }
    let since = now_ms();
    super::update_as(
        app,
        WorkActor::Agent,
        operation_id,
        &item.id,
        item.revision,
        &patch,
    )
    .await?;
    credit(app, std::slice::from_ref(&item.id), since)?;
    Ok((prior, written))
}

/// Puts back what [`update_task`] replaced, unless a field it wrote has
/// changed since.
pub(crate) async fn restore_task(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    prior: &TaskFields,
    written: &TaskFields,
) -> std::result::Result<(), String> {
    let patch = prior.patch();
    if patch == WorkPatch::default() {
        return Ok(());
    }
    let item = read_item(app, id)?;
    if !written.held_by(&item) {
        return Err(CHANGED_SINCE.to_owned());
    }
    let since = now_ms();
    super::update_as(
        app,
        WorkActor::Agent,
        operation_id,
        &item.id,
        item.revision,
        &patch,
    )
    .await?;
    credit(app, std::slice::from_ref(&item.id), since)
}

/// A project's name and description: before a card changed them, or as the
/// card left them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectFields {
    name: String,
    description: Option<String>,
}

impl ProjectFields {
    fn of(project: &Project) -> Self {
        Self {
            name: project.title.clone(),
            description: project.description.clone(),
        }
    }
}

fn read_project(app: &AppHandle, id: &str) -> std::result::Result<Project, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        store::project(conn, &store::uuid(id)?)
    })
}

/// Saves a project's name and description, making it when `id` is none.
/// Returns the project, what an existing one held before, and what it holds
/// now.
pub(crate) fn save_project(
    app: &AppHandle,
    operation_id: &str,
    id: Option<&str>,
    name: &str,
    description: Option<&str>,
) -> std::result::Result<(Project, Option<ProjectFields>, ProjectFields), String> {
    let prior = id
        .map(|id| read_project(app, id).map(|project| ProjectFields::of(&project)))
        .transpose()?;
    let edit = ProjectEdit {
        title: Some(name.trim().to_owned()),
        description: description.map(str::to_owned),
        ..ProjectEdit::default()
    };
    let project = super::catalog_project_save(app, operation_id, id.map(str::to_owned), &edit)?;
    let written = ProjectFields::of(&project);
    Ok((project, prior, written))
}

/// Puts a project's name and description back, or moves a project the chat
/// made to the bin, unless its name or description changed since.
pub(crate) fn restore_project(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    prior: Option<&ProjectFields>,
    written: &ProjectFields,
) -> std::result::Result<(), String> {
    if ProjectFields::of(&read_project(app, id)?) != *written {
        return Err(CHANGED_SINCE.to_owned());
    }
    let edit = match prior {
        Some(prior) => ProjectEdit {
            title: Some(prior.name.clone()),
            description: Some(prior.description.clone().unwrap_or_default()),
            ..ProjectEdit::default()
        },
        None => ProjectEdit {
            trashed: Some(true),
            ..ProjectEdit::default()
        },
    };
    super::catalog_project_save(app, operation_id, Some(id.to_owned()), &edit).map(drop)
}

/// Who is replacing a note's text, which decides what must hold first.
#[derive(Clone, Copy, Debug)]
enum NoteWrite<'a> {
    /// A card: the note must be short enough that the chat read all of it.
    Chat,
    /// Undo: the note must still hold what the card wrote.
    Undo { written: &'a str },
}

/// What became of a note replacement.
#[derive(Debug, Eq, PartialEq)]
enum NoteEdit {
    /// Done; the text it held before.
    Replaced(String),
    /// Longer than [`NOTE_CHARS`]: the chat saw only its start, and its text
    /// would drop the rest.
    TooLong,
    /// Changed after the card wrote it.
    ChangedSince,
}

fn replace_note_in(
    tx: &Connection,
    id: &str,
    body: &str,
    write: NoteWrite<'_>,
    at_ms: i64,
) -> std::result::Result<NoteEdit, scratchpad::ScratchpadError> {
    let Some(note) = scratchpad::note_with_connection(tx, id)? else {
        return Err(scratchpad::ScratchpadRefusal::NotFound.into());
    };
    match write {
        NoteWrite::Chat if note.body.chars().count() > NOTE_CHARS => return Ok(NoteEdit::TooLong),
        NoteWrite::Undo { written } if note.body != written => return Ok(NoteEdit::ChangedSince),
        NoteWrite::Chat | NoteWrite::Undo { .. } => {}
    }
    scratchpad::update_body_in(tx, id, body, Author::Agent(Some(CHAT_AGENT)), at_ms)?;
    Ok(NoteEdit::Replaced(note.body))
}

fn replace_note(
    app: &AppHandle,
    id: &str,
    body: &str,
    write: NoteWrite<'_>,
) -> std::result::Result<String, String> {
    let history = app.state::<Arc<HistoryManager>>();
    let edit = scratchpad::immediate(&history, "The note could not be saved.", |tx| {
        replace_note_in(tx, id, body, write, now_ms())
    })?;
    match edit {
        NoteEdit::Replaced(prior) => {
            scratchpad::emit_changed(app, id, false);
            Ok(prior)
        }
        NoteEdit::TooLong => Err("This note is longer than Talk to Sona can read, so it won't replace the text. Edit the note in Notes.".to_owned()),
        NoteEdit::ChangedSince => Err(CHANGED_SINCE.to_owned()),
    }
}

/// Replaces a note's text as a version written by [`CHAT_AGENT`], and
/// returns the text it replaced.
pub(crate) fn edit_note(
    app: &AppHandle,
    id: &str,
    body: &str,
) -> std::result::Result<String, String> {
    replace_note(app, id, body, NoteWrite::Chat)
}

/// Puts back the text [`edit_note`] replaced, unless the note changed since.
pub(crate) fn restore_note(
    app: &AppHandle,
    id: &str,
    prior: &str,
    written: &str,
) -> std::result::Result<(), String> {
    replace_note(app, id, prior, NoteWrite::Undo { written }).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;
    use crate::planning::work::items;
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        schema::apply_all(&conn).expect("planning schema");
        conn
    }

    fn make(
        conn: &Connection,
        title: &str,
        details: Option<&str>,
        priority: u8,
        actor: WorkActor,
        at: i64,
    ) -> WorkItem {
        items::insert(
            conn,
            &items::NewItem {
                title,
                details,
                status: WorkStatus::NotStarted,
                priority,
                project_id: None,
                milestone_id: None,
                owner: &super::super::types::WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            actor,
            at,
        )
        .expect("task")
    }

    #[test]
    fn tasks_read_hides_kept_local_and_orders_by_urgency() {
        let conn = db();
        let low = make(&conn, "Low priority task", None, 4, WorkActor::Me, 1_000);
        let urgent = make(&conn, "Urgent task", None, 1, WorkActor::Me, 2_000);
        let secret = make(
            &conn,
            "Kept-local meeting task",
            None,
            1,
            WorkActor::Me,
            3_000,
        );

        let hidden = HashSet::from([secret.id.clone()]);
        let list = tasks(&conn, &hidden, None, StatusFilter::Open, None, 10).expect("list");
        let rows = list["rows"].as_array().expect("rows");
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|row| row["task_id"].as_str())
            .collect();
        assert_eq!(ids, vec![urgent.id.as_str(), low.id.as_str()]);
        assert!(task(&conn, &hidden, &secret.id)
            .expect("task read")
            .is_none());
        assert!(task(&conn, &hidden, &format!("S-{}", urgent.number))
            .expect("handle read")
            .is_some());
    }

    #[test]
    fn task_patch_captures_prior_and_skips_unchanged_fields() {
        let conn = db();
        let item = make(
            &conn,
            "Original",
            Some("Old details"),
            3,
            WorkActor::Me,
            1_000,
        );

        let edit = TaskEdit {
            title: Some("Original".to_owned()),
            details: None,
            status: Some(WorkStatus::InProgress),
            priority: Some(1),
            due: Some(DueEdit::named("2026-12-01").expect("day")),
            project_id: None,
        };
        let (patch, prior) = task_patch(&item, &edit).expect("patch");
        assert!(patch.title.is_none(), "unchanged title omitted");
        assert_eq!(patch.status, Some(WorkStatus::InProgress));
        assert_eq!(patch.priority, Some(1));
        assert!(patch.due_at_ms.is_some());

        let undo_patch = prior.patch();
        assert!(undo_patch.title.is_none());
        assert_eq!(undo_patch.status, Some(WorkStatus::NotStarted));
        assert_eq!(undo_patch.priority, Some(3));
        assert_eq!(undo_patch.due_at_ms, Some(None));
    }

    #[test]
    fn chat_credit_marks_activity_via_chat() {
        let conn = db();
        let item = make(&conn, "Created from chat", None, 0, WorkActor::Agent, 5_000);
        agents::credit(&conn, &item.id, 5_000, CHAT_AGENT, Some(WorkVia::Chat)).expect("credit");
        let activity = items::activity(&conn, &item.id).expect("activity");
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].actor, WorkActor::Agent);
        assert_eq!(activity[0].agent.as_deref(), Some(CHAT_AGENT));
        assert_eq!(activity[0].via, Some(WorkVia::Chat));
        let detail = task(&conn, &HashSet::new(), &item.id)
            .expect("detail")
            .expect("present");
        assert_eq!(detail["activity"][0]["who"], "Talk to Sona via chat");
    }

    #[test]
    fn undo_leaves_a_task_whose_written_field_changed_again() {
        let conn = db();
        let item = make(&conn, "Draft plan", None, 3, WorkActor::Me, 1_000);
        let edit = TaskEdit {
            status: Some(WorkStatus::InProgress),
            priority: Some(1),
            ..TaskEdit::default()
        };
        let (patch, _) = task_patch(&item, &edit).expect("patch");
        let written = TaskFields::written(&patch);
        let as_written = WorkItem {
            status: WorkStatus::InProgress,
            priority: 1,
            ..item.clone()
        };
        let retitled = WorkItem {
            title: "Owner's own title".to_owned(),
            ..as_written.clone()
        };
        let reprioritized = WorkItem {
            priority: 2,
            ..as_written.clone()
        };

        assert!(written.held_by(&as_written));
        assert!(
            written.held_by(&retitled),
            "a field the card left alone may change"
        );
        assert!(!written.held_by(&reprioritized));
    }

    fn notes_db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        rusqlite_migration::Migrations::new(crate::managers::history::MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("history migrations");
        conn
    }

    #[test]
    fn a_chat_edit_leaves_a_note_longer_than_the_chat_could_read() {
        let conn = notes_db();
        let long = "word ".repeat(NOTE_CHARS / 5 + 1);
        let note = scratchpad::create_note_in(&conn, &long, Author::Me, 1_000).expect("note");
        let short =
            scratchpad::create_note_in(&conn, "Short note", Author::Me, 1_000).expect("note");

        let refused = replace_note_in(&conn, &note.id, "Start only", NoteWrite::Chat, 2_000);
        let replaced = replace_note_in(&conn, &short.id, "New text", NoteWrite::Chat, 2_000);

        assert_eq!(refused.ok(), Some(NoteEdit::TooLong));
        assert_eq!(
            scratchpad::note_with_connection(&conn, &note.id)
                .expect("read")
                .expect("note")
                .body,
            long
        );
        assert_eq!(
            replaced.ok(),
            Some(NoteEdit::Replaced("Short note".to_owned()))
        );
    }

    #[test]
    fn undo_leaves_a_note_changed_after_the_chat_wrote_it() {
        let conn = notes_db();
        let note = scratchpad::create_note_in(&conn, "Before", Author::Me, 1_000).expect("note");
        replace_note_in(&conn, &note.id, "From chat", NoteWrite::Chat, 2_000).expect("chat edit");
        scratchpad::update_body_in(&conn, &note.id, "Owner's later text", Author::Me, 3_000)
            .expect("owner edit");

        let undo = replace_note_in(
            &conn,
            &note.id,
            "Before",
            NoteWrite::Undo {
                written: "From chat",
            },
            4_000,
        );

        assert_eq!(undo.ok(), Some(NoteEdit::ChangedSince));
        assert_eq!(
            scratchpad::note_with_connection(&conn, &note.id)
                .expect("read")
                .expect("note")
                .body,
            "Owner's later text"
        );
    }
}
