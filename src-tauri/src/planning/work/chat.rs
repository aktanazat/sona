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
use rusqlite::Connection;
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

/// What a task held in the fields a card changed, for putting it back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskPrior {
    title: Option<String>,
    details: Option<Option<String>>,
    status: Option<WorkStatus>,
    priority: Option<u8>,
    due_at_ms: Option<Option<i64>>,
    project_id: Option<Option<String>>,
}

impl TaskPrior {
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
}

/// The patch an edit makes to `item`, with what it replaces. Fields the
/// edit sets to what the task already holds are left out of both.
pub(crate) fn task_patch(
    item: &WorkItem,
    edit: &TaskEdit,
) -> std::result::Result<(WorkPatch, TaskPrior), String> {
    let mut patch = WorkPatch::default();
    let mut prior = TaskPrior {
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

/// Changes one task, and returns what the changed fields held.
pub(crate) async fn update_task(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    edit: &TaskEdit,
) -> std::result::Result<TaskPrior, String> {
    let item = read_item(app, id)?;
    let (patch, prior) = task_patch(&item, edit)?;
    if patch == WorkPatch::default() {
        return Ok(prior);
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
    Ok(prior)
}

/// Puts back what [`update_task`] replaced.
pub(crate) async fn restore_task(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    prior: &TaskPrior,
) -> std::result::Result<(), String> {
    let patch = prior.patch();
    if patch == WorkPatch::default() {
        return Ok(());
    }
    let item = read_item(app, id)?;
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

/// A project's name and description before a card changed them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectPrior {
    name: String,
    description: Option<String>,
}

/// Saves a project's name and description, making it when `id` is none.
/// Returns the project and, for an existing one, what it held.
pub(crate) fn save_project(
    app: &AppHandle,
    operation_id: &str,
    id: Option<&str>,
    name: &str,
    description: Option<&str>,
) -> std::result::Result<(Project, Option<ProjectPrior>), String> {
    let prior = id
        .map(|id| {
            crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
                store::project(conn, &store::uuid(id)?)
            })
            .map(|project| ProjectPrior {
                name: project.title,
                description: project.description,
            })
        })
        .transpose()?;
    let edit = ProjectEdit {
        title: Some(name.trim().to_owned()),
        description: description.map(str::to_owned),
        ..ProjectEdit::default()
    };
    let project = super::catalog_project_save(app, operation_id, id.map(str::to_owned), &edit)?;
    Ok((project, prior))
}

/// Puts a project's name and description back, or moves a project the chat
/// made to the bin.
pub(crate) fn restore_project(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    prior: Option<&ProjectPrior>,
) -> std::result::Result<(), String> {
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

/// Replaces a note's text as a version written by [`CHAT_AGENT`], and
/// returns the text it replaced.
pub(crate) fn edit_note(
    app: &AppHandle,
    id: &str,
    body: &str,
) -> std::result::Result<String, String> {
    let history = app.state::<Arc<HistoryManager>>();
    let prior = scratchpad::immediate(&history, "The note could not be saved.", |tx| {
        let Some(note) = scratchpad::note_with_connection(tx, id)? else {
            return Err(scratchpad::ScratchpadRefusal::NotFound.into());
        };
        scratchpad::update_body_in(tx, id, body, Author::Agent(Some(CHAT_AGENT)), now_ms())?;
        Ok(note.body)
    })?;
    scratchpad::emit_changed(app, id, false);
    Ok(prior)
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
}
