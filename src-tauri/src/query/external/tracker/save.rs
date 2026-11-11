use super::{
    cycle_ref, day_or_none, handle, limit, refusal, repeat, snooze_day, task_ref, threads, Fence,
    Fields, Priority, StatusArg, TaskRef, Tracker, UpdateArgs, WorkAnswer, WriteArgs,
};
use crate::planning;
use crate::planning::work::changes::{self, CursorRefusal};
use crate::query::external::ExternalError;
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SaveArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// The changes as a JSON list, applied in order, all or none:
    /// [{"field":"status","value":"in_progress"},
    /// {"field":"labels","op":"add","value":["bug"]},
    /// {"field":"description","op":"append","value":"Done: x"}].
    /// Fields: title, description, status, priority, owner, due, project,
    /// milestone, labels (op set, add or remove), parent, cycle, repeat,
    /// snooze. description takes op replace, append, prepend,
    /// insert_before, insert_after or replace_text, the last three with an
    /// anchor: text already in it. A null value clears the field.
    #[arg(long, value_name = "JSON")]
    pub patch_json: String,
    /// Refuse unless the task is still at this revision.
    #[arg(long)]
    pub expected_revision: Option<i64>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ChangesArgs {
    /// The next_cursor of the last page. Left out: from the oldest change
    /// kept; changes are kept 30 days.
    #[arg(long, value_name = "CURSOR")]
    pub since: Option<i64>,
    /// Changes to print, at most 100.
    #[arg(long, value_parser = limit)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PatchValue {
    Text(String),
    Number(i64),
    List(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchEntry {
    field: String,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    value: Option<PatchValue>,
    #[serde(default)]
    anchor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkChangeEvent {
    pub seq: i64,
    pub at_ms: i64,
    pub entity: String,
    pub entity_id: String,
    pub action: String,
    pub task_id: Option<String>,
    pub task: Option<String>,
    pub updated_from: Option<serde_json::Value>,
    pub data: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkChanges {
    pub schema_version: u32,
    pub changes: Vec<ExternalWorkChangeEvent>,
    pub next_cursor: i64,
    pub has_more: bool,
}

fn bad(at: usize, message: impl std::fmt::Display) -> ExternalError {
    ExternalError::invalid(format!("patch[{at}]: {message}"))
}

fn one(at: usize, value: Option<&PatchValue>, none: &str) -> Result<String, ExternalError> {
    match value {
        None => Ok(none.to_owned()),
        Some(PatchValue::Text(text)) => Ok(text.clone()),
        Some(PatchValue::Number(number)) => Ok(number.to_string()),
        Some(PatchValue::List(_)) => Err(bad(at, "value takes one string here, not a list")),
    }
}

fn many(at: usize, value: Option<&PatchValue>) -> Result<Vec<String>, ExternalError> {
    match value {
        None => Ok(Vec::new()),
        Some(PatchValue::Text(text)) => Ok(vec![text.clone()]),
        Some(PatchValue::List(list)) => Ok(list.clone()),
        Some(PatchValue::Number(_)) => Err(bad(at, "labels take names or ids")),
    }
}

fn parsed<T>(
    at: usize,
    field: &str,
    value: &str,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, ExternalError> {
    parse(value).map_err(|message| bad(at, format!("{field} {value:?}: {message}")))
}

fn enumerated<T: ValueEnum>(at: usize, field: &str, value: &str) -> Result<T, ExternalError> {
    T::from_str(value, true).map_err(|_| {
        let names: Vec<String> = T::value_variants()
            .iter()
            .filter_map(|variant| {
                variant
                    .to_possible_value()
                    .map(|value| value.get_name().to_owned())
            })
            .collect();
        bad(
            at,
            format!("{field} {value:?} is not one of {}", names.join(", ")),
        )
    })
}

fn rewritten(
    at: usize,
    entry: &PatchEntry,
    current: Option<String>,
) -> Result<Option<String>, ExternalError> {
    let op = entry.op.as_deref().unwrap_or("replace");
    if op == "replace" {
        return match entry.value.as_ref() {
            None => Ok(None),
            Some(PatchValue::Text(text)) => Ok(Some(text.clone())),
            Some(_) => Err(bad(at, "description takes text")),
        };
    }
    let Some(PatchValue::Text(text)) = entry.value.as_ref() else {
        return Err(bad(at, format!("{op} needs value: the text")));
    };
    let current = current.unwrap_or_default();
    let joined = |first: &str, second: &str| {
        if first.trim().is_empty() || second.trim().is_empty() {
            format!("{first}{second}")
        } else {
            format!("{first}\n\n{second}")
        }
    };
    let anchored = |current: &str| -> Result<(usize, String), ExternalError> {
        let anchor = entry
            .anchor
            .as_deref()
            .filter(|anchor| !anchor.is_empty())
            .ok_or_else(|| {
                bad(
                    at,
                    format!("{op} needs anchor: text already in the description"),
                )
            })?;
        let start = current
            .find(anchor)
            .ok_or_else(|| bad(at, format!("anchor {anchor:?} is not in the description")))?;
        Ok((start, anchor.to_owned()))
    };
    let next = match op {
        "append" => joined(&current, text),
        "prepend" => joined(text, &current),
        "insert_before" => {
            let (start, _) = anchored(&current)?;
            format!("{}{text}{}", &current[..start], &current[start..])
        }
        "insert_after" => {
            let (start, anchor) = anchored(&current)?;
            let end = start + anchor.len();
            format!("{}{text}{}", &current[..end], &current[end..])
        }
        "replace_text" => {
            let (start, anchor) = anchored(&current)?;
            let end = start + anchor.len();
            format!("{}{text}{}", &current[..start], &current[end..])
        }
        other => {
            return Err(bad(
                at,
                format!("description op {other:?} is not one of replace, append, prepend, insert_before, insert_after, replace_text"),
            ))
        }
    };
    Ok(Some(next))
}

impl Tracker<'_> {
    pub(super) async fn save(&self, args: &SaveArgs) -> Result<WorkAnswer, ExternalError> {
        let entries: Vec<PatchEntry> = serde_json::from_str(&args.patch_json).map_err(|error| {
            ExternalError::invalid(format!(
                "--patch-json is not a list of {{field, op, value, anchor}}: {error}."
            ))
        })?;
        if entries.is_empty() {
            return Err(ExternalError::invalid("--patch-json names no change."));
        }
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let mut update = UpdateArgs {
            tasks: vec![TaskRef::Id(row.item.id.clone())],
            title: None,
            fields: Fields {
                details: None,
                status: None,
                priority: None,
                owner: None,
                due: None,
                repeat: None,
                project: None,
                milestone: None,
                labels: None,
            },
            add_label: Vec::new(),
            remove_label: Vec::new(),
            snooze_until: None,
            cycle: None,
            parent: None,
            expected_revision: args.expected_revision,
            dry_run: args.dry_run,
            write: args.write.clone(),
        };
        let mut details = row.item.details.clone();
        let mut details_touched = false;
        let mut labels = row.item.label_ids.clone();
        let mut labels_touched = false;
        for (at, entry) in entries.iter().enumerate() {
            let field = entry.field.as_str();
            let value = entry.value.as_ref();
            if field != "labels"
                && field != "description"
                && entry.op.as_deref().is_some_and(|op| op != "set")
            {
                return Err(bad(at, format!("{field} takes no op but set")));
            }
            if entry.anchor.is_some() && field != "description" {
                return Err(bad(at, "only description takes an anchor"));
            }
            match field {
                "title" => {
                    let title = one(at, value, "")?;
                    if title.trim().is_empty() {
                        return Err(bad(at, "a title cannot be empty"));
                    }
                    update.title = Some(title);
                }
                "description" | "details" => {
                    details = rewritten(at, entry, details)?;
                    details_touched = true;
                }
                "status" => {
                    update.fields.status = Some(enumerated::<StatusArg>(at, field, &one(at, value, "")?)?);
                }
                "priority" => {
                    update.fields.priority = Some(enumerated::<Priority>(at, field, &one(at, value, "none")?)?);
                }
                "owner" => {
                    let owner = one(at, value, "nobody")?;
                    board.owner(&owner).map_err(|error| bad(at, error.message))?;
                    update.fields.owner = Some(owner);
                }
                "due" => update.fields.due = Some(parsed(at, field, &one(at, value, "none")?, day_or_none)?),
                "project" => {
                    let project = one(at, value, "none")?;
                    board.project(&project).map_err(|error| bad(at, error.message))?;
                    update.fields.project = Some(project);
                }
                "milestone" => update.fields.milestone = Some(one(at, value, "none")?),
                "labels" => {
                    let names = many(at, value)?;
                    let ids = board.label_ids(&names).map_err(|error| bad(at, error.message))?;
                    match entry.op.as_deref().unwrap_or("set") {
                        "set" => labels = ids,
                        "add" => {
                            for id in ids {
                                if !labels.contains(&id) {
                                    labels.push(id);
                                }
                            }
                        }
                        "remove" => labels.retain(|id| !ids.contains(id)),
                        other => return Err(bad(at, format!("labels op {other:?} is not one of set, add, remove"))),
                    }
                    labels_touched = true;
                }
                "parent" => update.parent = Some(parsed(at, field, &one(at, value, "none")?, threads::parent_arg)?),
                "cycle" => update.cycle = Some(parsed(at, field, &one(at, value, "none")?, cycle_ref)?),
                "repeat" => update.fields.repeat = Some(parsed(at, field, &one(at, value, "none")?, repeat)?),
                "snooze" => update.snooze_until = Some(parsed(at, field, &one(at, value, "none")?, snooze_day)?),
                other => {
                    return Err(bad(
                        at,
                        format!("{other:?} is not a field: title, description, status, priority, owner, due, project, milestone, labels, parent, cycle, repeat or snooze"),
                    ))
                }
            }
        }
        if details_touched {
            update.fields.details = Some(details.unwrap_or_default());
        }
        if labels_touched {
            update.fields.labels = Some(labels);
        }
        if let Some(threads::ParentArg::Task(parent)) = &update.parent {
            let parent = board.task(parent)?;
            if board.above(&row.item.id, &parent.item.id) {
                return Err(ExternalError::invalid(format!(
                    "{} cannot go under {}: a task cannot sit under itself or one of its own sub-tasks.",
                    handle(row.item.number),
                    handle(parent.item.number)
                )));
            }
        }
        self.update(&update).await
    }

    pub(super) async fn changes(&self, args: &ChangesArgs) -> Result<WorkAnswer, ExternalError> {
        let limit = args.limit.unwrap_or(changes::PAGE).min(changes::MAX_PAGE);
        let page = match changes::page_read(self.app, args.since, limit)
            .map_err(|message| refusal(self.history, message))?
        {
            Ok(page) => page,
            Err(CursorRefusal::Expired { oldest }) => {
                return Err(ExternalError::invalid(format!(
                    "Cursor {} has expired: changes are kept 30 days and the oldest kept is {oldest}. Read the tasks again (sona work list), then follow changes without --since.",
                    args.since.unwrap_or_default()
                )))
            }
            Err(CursorRefusal::Ahead { head }) => {
                return Err(ExternalError::invalid(format!(
                    "Cursor {} is past the newest change, {head}.",
                    args.since.unwrap_or_default()
                )))
            }
        };
        let fence = Fence::read(self.meetings).await?;
        let hidden = planning::history(self.history, |history| fence.hidden_tasks(history))
            .map_err(|message| refusal(self.history, message))?;
        let numbers: std::collections::HashMap<String, i64> =
            planning::history(self.history, |history| {
                let mut statement = history.prepare("SELECT id, number FROM planning_tasks")?;
                let found = statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok(found)
            })
            .map_err(|message| refusal(self.history, message))?;
        let changes = page
            .changes
            .into_iter()
            .filter(|change| {
                change
                    .item_id
                    .as_ref()
                    .is_none_or(|id| !hidden.contains(id))
            })
            .map(|change| {
                let number = change
                    .item_id
                    .as_ref()
                    .and_then(|id| numbers.get(id).copied())
                    .or_else(|| {
                        change
                            .data
                            .as_ref()
                            .and_then(|data| data.get("number"))
                            .and_then(serde_json::Value::as_i64)
                    });
                ExternalWorkChangeEvent {
                    seq: change.seq,
                    at_ms: change.at_ms,
                    entity: change.entity,
                    entity_id: change.entity_id,
                    action: change.action,
                    task: number.map(handle),
                    task_id: change.item_id,
                    updated_from: change.updated_from,
                    data: change.data,
                }
            })
            .collect();
        Ok(WorkAnswer::Changes(ExternalWorkChanges {
            schema_version: QUERY_SCHEMA_VERSION,
            changes,
            next_cursor: page.next_cursor,
            has_more: page.has_more,
        }))
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let WorkAnswer::Changes(page) = answer else {
        return;
    };
    if page.changes.is_empty() {
        text.push_str("No changes.\n");
    }
    for change in &page.changes {
        let fields = change
            .updated_from
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .map(|from| from.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "{} {} {} {}{}{}",
            change.seq,
            change.action,
            change.entity,
            change.task.as_deref().unwrap_or(&change.entity_id),
            if fields.is_empty() { "" } else { ": " },
            fields
        );
    }
    let _ = writeln!(
        text,
        "{}--since {}",
        if page.has_more { "More: " } else { "Next: " },
        page.next_cursor
    );
}
