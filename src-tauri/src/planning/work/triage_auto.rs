//! Triage that sorts itself: routing rules (a source meeting, a meeting
//! folder or words in the task lead to a project, a label or an owner) and,
//! for the fields Settings names, Sona's own suggestions once every similar
//! task agrees. Both run on each new task in Triage, fill only fields it
//! left empty, land as Sona's change, and can be undone.

use super::loops::{message, LoopRefusal};
use super::sync::{self, MeetingReader};
use super::types::{
    WorkActor, WorkFieldSuggestion, WorkItem, WorkOwner, WorkPatch, WorkStatus,
    WorkSuggestionField, WorkSuggestionValue,
};
use super::{edit, items, now_ms, run_with_meetings, triage_hints, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::planning::store;
use anyhow::Result;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub(crate) const TRIAGE_AUTO_MIGRATION: &str = "CREATE TABLE planning_triage_rules (
    id TEXT PRIMARY KEY NOT NULL,
    match_kind TEXT NOT NULL CHECK(match_kind IN ('meeting', 'folder', 'words')),
    match_value TEXT NOT NULL CHECK(length(trim(match_value)) > 0),
    project_id TEXT,
    label_id TEXT,
    owner TEXT,
    created_at_ms INTEGER NOT NULL,
    CHECK(project_id IS NOT NULL OR label_id IS NOT NULL OR owner IS NOT NULL)
);
CREATE TABLE planning_triage_applied (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    field TEXT NOT NULL CHECK(field IN ('project', 'labels', 'owner')),
    value TEXT NOT NULL,
    previous TEXT,
    reason TEXT NOT NULL,
    hint_id TEXT,
    applied_at_ms INTEGER NOT NULL,
    undone_at_ms INTEGER
);
CREATE INDEX planning_triage_applied_task ON planning_triage_applied(task_id);
CREATE TRIGGER planning_triage_applied_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_triage_applied WHERE task_id = old.id;
END;
CREATE TABLE planning_triage_cursor (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    change_cursor INTEGER NOT NULL
);
INSERT INTO planning_triage_cursor (id, change_cursor)
    SELECT 1, COALESCE(MAX(seq), 0) FROM planning_changes;";

/// A field Sona may fill by itself from its own suggestions.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum TriageAutoField {
    Project,
    Labels,
    Owner,
}

crate::planning::types::sql_enum!(TriageAutoField {
    Project => "project",
    Labels => "labels",
    Owner => "owner",
});

impl TriageAutoField {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "project" => Some(Self::Project),
            "labels" | "label" => Some(Self::Labels),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }

    const fn of(field: WorkSuggestionField) -> Option<Self> {
        match field {
            WorkSuggestionField::Project => Some(Self::Project),
            WorkSuggestionField::Label => Some(Self::Labels),
            WorkSuggestionField::Owner => Some(Self::Owner),
            WorkSuggestionField::Due
            | WorkSuggestionField::Duplicate
            | WorkSuggestionField::Related => None,
        }
    }
}

/// What a rule looks at: the title of the meeting a task came from, the
/// folder that meeting is in, or the task's own title and details.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TriageRuleMatch {
    Meeting,
    Folder,
    Words,
}

crate::planning::types::sql_enum!(TriageRuleMatch {
    Meeting => "meeting",
    Folder => "folder",
    Words => "words",
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TriageRuleInput {
    pub matches: TriageRuleMatch,
    pub value: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub label_id: Option<String>,
    #[serde(default)]
    pub owner: Option<WorkOwner>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TriageRule {
    pub id: String,
    pub matches: TriageRuleMatch,
    pub value: String,
    pub project_id: Option<String>,
    pub label_id: Option<String>,
    pub owner: Option<WorkOwner>,
    pub created_at_ms: i64,
}

/// A field Sona filled by itself on a task in Triage, until undone.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkAutoApplied {
    pub id: String,
    pub task_id: String,
    pub field: TriageAutoField,
    /// The project or label id, or "me" or a person id for an owner.
    pub value: String,
    pub reason: String,
    pub applied_at_ms: i64,
}

fn refuse<T>(text: &str) -> Result<T> {
    Err(LoopRefusal(text.to_owned()).into())
}

fn owner_text(owner: &WorkOwner) -> Option<String> {
    match owner {
        WorkOwner::Myself => Some("me".to_owned()),
        WorkOwner::Person { person_id } => Some(person_id.clone()),
        WorkOwner::Nobody => None,
    }
}

fn owner_of(text: Option<&str>) -> WorkOwner {
    match text {
        Some("me") => WorkOwner::Myself,
        Some(person_id) => WorkOwner::Person {
            person_id: person_id.to_owned(),
        },
        None => WorkOwner::Nobody,
    }
}

pub(crate) fn rules(conn: &Connection) -> Result<Vec<TriageRule>> {
    let mut statement = conn.prepare(
        "SELECT id, match_kind, match_value, project_id, label_id, owner, created_at_ms
         FROM planning_triage_rules ORDER BY created_at_ms, rowid",
    )?;
    let rows = statement
        .query_map([], |row| {
            let owner: Option<String> = row.get(5)?;
            Ok(TriageRule {
                id: row.get(0)?,
                matches: row.get(1)?,
                value: row.get(2)?,
                project_id: row.get(3)?,
                label_id: row.get(4)?,
                owner: owner.as_deref().map(|owner| owner_of(Some(owner))),
                created_at_ms: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Saves a rule, or with `id` changes one.
pub(crate) fn save_rule(
    conn: &Connection,
    id: Option<&str>,
    input: &TriageRuleInput,
    now: i64,
) -> Result<TriageRule> {
    let value = input.value.trim();
    if value.is_empty() {
        return refuse("A rule needs a meeting title, a folder or words to look for.");
    }
    let owner = input.owner.as_ref().and_then(owner_text);
    if input.project_id.is_none() && input.label_id.is_none() && owner.is_none() {
        return refuse("A rule needs a project, a label or an owner to set.");
    }
    if let Some(project) = &input.project_id {
        let live: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM planning_projects WHERE id = ?1 AND trashed_at_ms IS NULL)",
            [project],
            |row| row.get(0),
        )?;
        if !live {
            return refuse("That project no longer exists.");
        }
    }
    if let Some(label) = &input.label_id {
        let live: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM planning_labels WHERE id = ?1)",
            [label],
            |row| row.get(0),
        )?;
        if !live {
            return refuse("That label no longer exists.");
        }
    }
    let id = match id {
        Some(id) => {
            let changed = conn.execute(
                "UPDATE planning_triage_rules SET match_kind = ?2, match_value = ?3,
                    project_id = ?4, label_id = ?5, owner = ?6 WHERE id = ?1",
                params![
                    id,
                    input.matches.as_str(),
                    value,
                    input.project_id,
                    input.label_id,
                    owner
                ],
            )?;
            if changed == 0 {
                return refuse("This rule no longer exists.");
            }
            id.to_owned()
        }
        None => {
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO planning_triage_rules
                    (id, match_kind, match_value, project_id, label_id, owner, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    input.matches.as_str(),
                    value,
                    input.project_id,
                    input.label_id,
                    owner,
                    now
                ],
            )?;
            id
        }
    };
    rules(conn)?
        .into_iter()
        .find(|rule| rule.id == id)
        .ok_or_else(|| LoopRefusal("This rule no longer exists.".to_owned()).into())
}

pub(crate) fn delete_rule(conn: &Connection, id: &str) -> Result<TriageRule> {
    let Some(rule) = rules(conn)?.into_iter().find(|rule| rule.id == id) else {
        return refuse("This rule no longer exists.");
    };
    conn.execute("DELETE FROM planning_triage_rules WHERE id = ?1", [id])?;
    Ok(rule)
}

/// Each live task's fields Sona filled and that were not undone, by task.
pub(crate) fn applied(conn: &Connection) -> Result<BTreeMap<String, Vec<WorkAutoApplied>>> {
    let mut statement = conn.prepare(
        "SELECT a.id, a.task_id, a.field, a.value, a.reason, a.applied_at_ms
         FROM planning_triage_applied a JOIN planning_tasks t ON t.id = a.task_id
         WHERE a.undone_at_ms IS NULL AND t.status = 'triage'
         ORDER BY a.applied_at_ms, a.rowid",
    )?;
    let mut by_task: BTreeMap<String, Vec<WorkAutoApplied>> = BTreeMap::new();
    let rows = statement.query_map([], |row| {
        Ok(WorkAutoApplied {
            id: row.get(0)?,
            task_id: row.get(1)?,
            field: row.get(2)?,
            value: row.get(3)?,
            reason: row.get(4)?,
            applied_at_ms: row.get(5)?,
        })
    })?;
    for row in rows {
        let row = row?;
        by_task.entry(row.task_id.clone()).or_default().push(row);
    }
    Ok(by_task)
}

/// The meetings a task came from, with each meeting's title and folders,
/// read from the conversations when they are open.
fn sources(meetings: Option<&Connection>, ids: &[String]) -> Result<Vec<(String, Vec<String>)>> {
    let Some(meetings) = meetings else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for id in ids {
        let title: Option<String> = meetings
            .query_row(
                "SELECT title FROM meeting_sessions WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(title) = title else { continue };
        let mut statement = meetings.prepare_cached(
            "SELECT f.name FROM meeting_folder_members m
             JOIN meeting_folders f ON f.folder_id = m.folder_id WHERE m.session_id = ?1",
        )?;
        let folders = statement
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        found.push((title, folders));
    }
    Ok(found)
}

fn holds(rule: &TriageRule, item: &WorkItem, sources: &[(String, Vec<String>)]) -> bool {
    let wanted = rule.value.to_lowercase();
    let has = |text: &str| text.to_lowercase().contains(&wanted);
    match rule.matches {
        TriageRuleMatch::Words => has(&item.title) || item.details.as_deref().is_some_and(has),
        TriageRuleMatch::Meeting => sources.iter().any(|(title, _)| has(title)),
        TriageRuleMatch::Folder => sources
            .iter()
            .any(|(_, folders)| folders.iter().any(|folder| folder.to_lowercase() == wanted)),
    }
}

struct Filling {
    field: TriageAutoField,
    value: String,
    reason: String,
    hint_id: Option<String>,
}

fn rule_reason(rule: &TriageRule) -> String {
    match rule.matches {
        TriageRuleMatch::Words => format!("The rule for the words “{}”", rule.value),
        TriageRuleMatch::Meeting => format!("The rule for meetings called “{}”", rule.value),
        TriageRuleMatch::Folder => format!("The rule for the folder “{}”", rule.value),
    }
}

/// What rules and sure suggestions fill on `item`, one value per field,
/// rules first; only fields the task leaves empty.
fn fillings(
    item: &WorkItem,
    rules: &[TriageRule],
    sources: &[(String, Vec<String>)],
    suggestions: &[WorkFieldSuggestion],
    fields: &[TriageAutoField],
) -> Vec<Filling> {
    let mut found: Vec<Filling> = Vec::new();
    let empty = |found: &[Filling], field: TriageAutoField| match field {
        TriageAutoField::Project => {
            item.project_id.is_none() && !found.iter().any(|fill| fill.field == field)
        }
        TriageAutoField::Owner => {
            item.owner() == WorkOwner::Nobody && !found.iter().any(|fill| fill.field == field)
        }
        TriageAutoField::Labels => true,
    };
    for rule in rules.iter().filter(|rule| holds(rule, item, sources)) {
        let reason = rule_reason(rule);
        if let Some(project) = &rule.project_id {
            if empty(&found, TriageAutoField::Project) {
                found.push(Filling {
                    field: TriageAutoField::Project,
                    value: project.clone(),
                    reason: reason.clone(),
                    hint_id: None,
                });
            }
        }
        if let Some(label) = &rule.label_id {
            let taken = item.label_ids.contains(label)
                || found
                    .iter()
                    .any(|fill| fill.field == TriageAutoField::Labels && fill.value == *label);
            if !taken {
                found.push(Filling {
                    field: TriageAutoField::Labels,
                    value: label.clone(),
                    reason: reason.clone(),
                    hint_id: None,
                });
            }
        }
        if let Some(owner) = rule.owner.as_ref().and_then(owner_text) {
            if empty(&found, TriageAutoField::Owner) {
                found.push(Filling {
                    field: TriageAutoField::Owner,
                    value: owner,
                    reason: reason.clone(),
                    hint_id: None,
                });
            }
        }
    }
    for suggestion in suggestions.iter().filter(|suggestion| suggestion.sure) {
        let Some(field) = TriageAutoField::of(suggestion.field) else {
            continue;
        };
        if !fields.contains(&field) || !empty(&found, field) {
            continue;
        }
        let value = match &suggestion.value {
            WorkSuggestionValue::Id(id) => id.clone(),
            WorkSuggestionValue::Owner(owner) => match owner_text(owner) {
                Some(owner) => owner,
                None => continue,
            },
            WorkSuggestionValue::Due(_) => continue,
        };
        if field == TriageAutoField::Labels
            && (item.label_ids.contains(&value)
                || found
                    .iter()
                    .any(|fill| fill.field == field && fill.value == value))
        {
            continue;
        }
        found.push(Filling {
            field,
            value,
            reason: suggestion.reason.clone(),
            hint_id: Some(suggestion.id.clone()),
        });
    }
    found
}

/// Sorts each task that reached Triage since the last pass. Returns the
/// tasks it changed.
fn sort_new(
    history: &Connection,
    meetings: Option<&Connection>,
    user_name: Option<&str>,
    fields: &[TriageAutoField],
    effects: &mut Effects,
    now: i64,
) -> Result<Vec<String>> {
    let cursor: i64 = history.query_row(
        "SELECT change_cursor FROM planning_triage_cursor WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let head: i64 = history.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM planning_changes",
        [],
        |row| row.get(0),
    )?;
    if head <= cursor {
        return Ok(Vec::new());
    }
    let created: Vec<String> = {
        let mut statement = history.prepare(
            "SELECT DISTINCT entity_id FROM planning_changes
             WHERE seq > ?1 AND seq <= ?2 AND entity = 'task' AND action = 'create'
             ORDER BY seq",
        )?;
        let rows = statement
            .query_map(params![cursor, head], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    history.execute(
        "UPDATE planning_triage_cursor SET change_cursor = ?1 WHERE id = 1",
        [head],
    )?;
    let rules = rules(history)?;
    if created.is_empty() || (rules.is_empty() && fields.is_empty()) {
        return Ok(Vec::new());
    }
    let all = items::items(history, false)?;
    let task_meetings = triage_hints::task_meetings(history)?;
    let mut reader = MeetingReader::new(meetings, user_name);
    let change = Change::now("", WorkActor::Sona);
    let mut changed = Vec::new();
    for id in created {
        let Some(item) = all.iter().find(|item| item.id == id) else {
            continue;
        };
        if item.status != WorkStatus::Triage {
            continue;
        }
        let from = sources(
            meetings,
            task_meetings.get(&item.id).map_or(&[], Vec::as_slice),
        )?;
        let suggestions = if fields.is_empty() {
            Vec::new()
        } else {
            sync::suggestions(history, &mut reader, &all, item)?
        };
        let fills = fillings(item, &rules, &from, &suggestions, fields);
        if fills.is_empty() {
            continue;
        }
        let mut patch = WorkPatch::default();
        let mut labels = item.label_ids.clone();
        for fill in &fills {
            let previous = match fill.field {
                TriageAutoField::Project => {
                    patch.project_id = Some(Some(fill.value.clone()));
                    item.project_id.clone()
                }
                TriageAutoField::Owner => {
                    patch.owner = Some(owner_of(Some(&fill.value)));
                    owner_text(&item.owner())
                }
                TriageAutoField::Labels => {
                    labels.push(fill.value.clone());
                    None
                }
            };
            history.execute(
                "INSERT INTO planning_triage_applied
                    (id, task_id, field, value, previous, reason, hint_id, applied_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    item.id,
                    fill.field.as_str(),
                    fill.value,
                    previous,
                    fill.reason,
                    fill.hint_id,
                    now
                ],
            )?;
        }
        if labels.len() != item.label_ids.len() {
            patch.label_ids = Some(labels);
        }
        edit(history, meetings, change, &item.id, &patch, effects)?;
        changed.push(item.id.clone());
    }
    Ok(changed)
}

/// A fill as stored: task, field, value, previous value, hint, undone time.
type AppliedRow = (
    String,
    TriageAutoField,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// Puts a field back as it was before Sona filled it, in the user's name,
/// and sets the suggestion behind it aside so it is not filled again.
fn undo(
    history: &Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    id: &str,
    effects: &mut Effects,
) -> Result<WorkItem> {
    let row: Option<AppliedRow> = history
        .query_row(
            "SELECT task_id, field, value, previous, hint_id, undone_at_ms
                 FROM planning_triage_applied WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((task_id, field, value, previous, hint_id, undone)) = row else {
        return refuse("This change is no longer there to undo.");
    };
    if undone.is_some() {
        return refuse("This change was already undone.");
    }
    let item = items::item(history, &task_id)?;
    let still_filled = match field {
        TriageAutoField::Project => item.project_id.as_deref() == Some(value.as_str()),
        TriageAutoField::Owner if value == "me" => item.owner_is_self,
        TriageAutoField::Owner => item.owner_person_id.as_deref() == Some(value.as_str()),
        TriageAutoField::Labels => item.label_ids.contains(&value),
    };
    if !still_filled {
        return refuse("This field was changed after Sona filled it, so there is nothing to undo.");
    }
    let patch = match field {
        TriageAutoField::Project => WorkPatch {
            project_id: Some(previous),
            ..WorkPatch::default()
        },
        TriageAutoField::Owner => WorkPatch {
            owner: Some(owner_of(previous.as_deref())),
            ..WorkPatch::default()
        },
        TriageAutoField::Labels => WorkPatch {
            label_ids: Some(
                item.label_ids
                    .iter()
                    .filter(|label| **label != value)
                    .cloned()
                    .collect(),
            ),
            ..WorkPatch::default()
        },
    };
    let item = edit(history, meetings, change, &task_id, &patch, effects)?;
    history.execute(
        "UPDATE planning_triage_applied SET undone_at_ms = ?2 WHERE id = ?1",
        params![id, change.at_ms],
    )?;
    if let Some(hint_id) = hint_id {
        history.execute(
            "INSERT OR IGNORE INTO planning_dismissed_hints (id, task_id, dismissed_at_ms)
             VALUES (?1, ?2, ?3)",
            params![hint_id, task_id, change.at_ms],
        )?;
    }
    Ok(item)
}

fn pending(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_changes
            WHERE seq > (SELECT change_cursor FROM planning_triage_cursor WHERE id = 1))",
        [],
        |row| row.get(0),
    )?)
}

/// One pass of [`sort_new`] for the running app, with the fields Settings
/// lets Sona fill. Takes the write lock only once something new arrived.
pub(crate) async fn tick(app: &AppHandle) {
    let (Some(meetings), Some(history)) = (
        app.try_state::<Arc<MeetingSessionManager>>(),
        app.try_state::<Arc<HistoryManager>>(),
    ) else {
        return;
    };
    match history.with_connection(|conn| pending(conn)) {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            log::warn!("Triage could not read new tasks: {error:#}");
            return;
        }
    }
    let fields = crate::settings::get_settings(app).work_triage_auto_apply;
    let user_name = meetings.user_name();
    let sorted = run_with_meetings(app, |history, meeting_conn| {
        let tx = history.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut effects = Effects::default();
        let changed = sort_new(
            &tx,
            meeting_conn,
            user_name.as_deref(),
            &fields,
            &mut effects,
            now_ms(),
        )?;
        tx.commit()?;
        Ok(Done {
            value: changed,
            effects,
        })
    })
    .await;
    if let Err(error) = sorted {
        log::warn!("Triage could not sort new tasks: {error}");
    }
}

pub(crate) fn rules_read(app: &AppHandle) -> Result<Vec<TriageRule>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| rules(conn))
}

fn rule_change<T: serde::de::DeserializeOwned + Serialize>(
    app: &AppHandle,
    operation_id: &str,
    command: &str,
    request: &impl Serialize,
    write: impl FnOnce(&Connection, i64) -> Result<T>,
) -> Result<T, String> {
    let now = now_ms();
    app.state::<Arc<HistoryManager>>()
        .with_connection(|conn| {
            store::mutate(conn, operation_id, command, request, now, |tx| {
                write(tx, now)
            })
        })
        .map_err(message)
}

pub(crate) fn rule_save_as(
    app: &AppHandle,
    operation_id: &str,
    id: Option<&str>,
    input: &TriageRuleInput,
) -> Result<TriageRule, String> {
    rule_change(
        app,
        operation_id,
        "work_triage_rule_save",
        &(id, input),
        |tx, now| save_rule(tx, id, input, now),
    )
}

pub(crate) fn rule_delete_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> Result<TriageRule, String> {
    rule_change(
        app,
        operation_id,
        "work_triage_rule_delete",
        &id,
        |tx, _| delete_rule(tx, id),
    )
}

pub(crate) async fn undo_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> Result<WorkItem, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_with_meetings(app, |history, meetings| {
        store::mutate(
            history,
            operation_id,
            "work_triage_undo",
            &id,
            change.at_ms,
            |tx| {
                let mut effects = Effects::default();
                let item = undo(tx, meetings, change, id, &mut effects)?;
                Ok(Done {
                    value: item,
                    effects,
                })
            },
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;
    use crate::planning::work::items::NewItem;
    use crate::planning::work::types::WorkActivityKind;

    #[test]
    fn a_rule_fills_an_empty_project_and_undo_takes_it_back() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project =
            store::create_project(&mut conn, "Billing", None, &Uuid::new_v4().to_string(), 1)
                .expect("a project");
        save_rule(
            &conn,
            None,
            &TriageRuleInput {
                matches: TriageRuleMatch::Words,
                value: "invoice".to_owned(),
                project_id: Some(project.id.clone()),
                label_id: None,
                owner: None,
            },
            2,
        )
        .expect("a saved rule");
        let task = items::insert(
            &conn,
            &NewItem {
                title: "Send Dana the Invoice",
                details: None,
                status: WorkStatus::Triage,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Agent,
            3,
        )
        .expect("a task in triage");

        let sorted = sort_new(&conn, None, None, &[], &mut Effects::default(), 4).expect("a pass");
        let filled = items::item(&conn, &task.id).expect("the task").project_id;
        let change = applied(&conn).expect("the fills")[&task.id][0].id.clone();
        undo(
            &conn,
            None,
            Change {
                operation_id: "undo",
                actor: WorkActor::Me,
                at_ms: 5,
            },
            &change,
            &mut Effects::default(),
        )
        .expect("the fill undone");

        assert_eq!(sorted, [task.id.clone()]);
        assert_eq!(filled, Some(project.id));
        assert_eq!(
            items::item(&conn, &task.id).expect("the task").project_id,
            None
        );
        assert!(applied(&conn).expect("the fills").is_empty());
    }

    #[test]
    fn undo_leaves_a_project_the_user_chose_after_the_fill() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let billing =
            store::create_project(&mut conn, "Billing", None, &Uuid::new_v4().to_string(), 1)
                .expect("a project");
        let harbor =
            store::create_project(&mut conn, "Harbor", None, &Uuid::new_v4().to_string(), 1)
                .expect("another project");
        save_rule(
            &conn,
            None,
            &TriageRuleInput {
                matches: TriageRuleMatch::Words,
                value: "invoice".to_owned(),
                project_id: Some(billing.id.clone()),
                label_id: None,
                owner: None,
            },
            2,
        )
        .expect("a saved rule");
        let task = items::insert(
            &conn,
            &NewItem {
                title: "Send Dana the Invoice",
                details: None,
                status: WorkStatus::Triage,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Agent,
            3,
        )
        .expect("a task in triage");
        sort_new(&conn, None, None, &[], &mut Effects::default(), 4).expect("a pass");
        let change = applied(&conn).expect("the fills")[&task.id][0].id.clone();
        items::apply(
            &conn,
            &task.id,
            &WorkPatch {
                project_id: Some(Some(harbor.id.clone())),
                ..WorkPatch::default()
            },
            WorkActivityKind::Project,
            WorkActor::Me,
            5,
        )
        .expect("the user moves it");

        let undone = undo(
            &conn,
            None,
            Change {
                operation_id: "undo",
                actor: WorkActor::Me,
                at_ms: 6,
            },
            &change,
            &mut Effects::default(),
        );

        assert!(undone.is_err());
        assert_eq!(
            items::item(&conn, &task.id).expect("the task").project_id,
            Some(harbor.id)
        );
    }
}
