use super::attachments::{AttachArgs, AttachTarget};
use super::organize::DelegateArgs;
use super::sources::{linked_source, SourceLinkArgs};
use super::{
    changed, cycle_ref, day_or_none, handle, limit, project_task, refusal, repeat, snooze_day,
    task_ref, threads, Board, CreateArgs, DuplicateArgs, ExternalFieldChange, ExternalWorkPreview,
    Fence, Fields, LinkKind, Priority, RelateArgs, StatusArg, TaskRef, Tracker, UpdateArgs,
    WorkAction, WorkAnswer, WriteArgs,
};
use crate::planning;
use crate::planning::work::agents;
use crate::planning::work::attachments;
use crate::planning::work::types::{WorkItemRow, WorkPatch};
use crate::planning::work::changes::{self, CursorRefusal};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use uuid::Uuid;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SaveArgs {
    /// S-12, 12, or the task's id. Left out, the save makes a new task and
    /// needs a title step.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: Option<TaskRef>,
    /// The changes as a JSON list, applied in order, all or none:
    /// [{"field":"status","value":"in_progress"},
    /// {"field":"labels","op":"add","value":["bug"]},
    /// {"field":"description","op":"append","value":"Done: x"}].
    /// Fields: title, description, status, priority, owner, due, project,
    /// milestone, labels (op set, add or remove), parent, cycle, repeat,
    /// snooze, estimate (0 to 64 points) and for (the person owed it).
    /// description takes op replace, append, prepend,
    /// insert_before, insert_after or replace_text, the last three with an
    /// anchor: text already in it. A null value clears the field.
    /// Also: delegate (an agent, null takes it back), blocks, blocked_by
    /// and related (op add or remove, a task or a list), duplicate_of (a
    /// task), links (a link or a list) and source (a meeting's id). Those
    /// are checked before anything is written, then saved one by one after
    /// the fields.
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
pub(super) enum PatchValue {
    Text(String),
    Number(i64),
    List(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PatchEntry {
    pub(super) field: String,
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

pub(super) fn rewritten(
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

/// Fields a save carries as their own writes, after the field update.
const EXTRA_FIELDS: [&str; 7] = [
    "delegate",
    "blocks",
    "blocked_by",
    "related",
    "duplicate_of",
    "links",
    "source",
];

/// A save step the field update does not carry. Each runs after it as its
/// own write, under an operation id derived from the save's.
enum Extra {
    Delegate(Option<String>),
    Relation {
        link: LinkKind,
        other: TaskRef,
        remove: bool,
    },
    DuplicateOf(TaskRef),
    Link(String),
    Source(String),
}

fn tasks_named(at: usize, value: Option<&PatchValue>) -> Result<Vec<TaskRef>, ExternalError> {
    let names = match value {
        None => Vec::new(),
        Some(PatchValue::Text(text)) => vec![text.clone()],
        Some(PatchValue::Number(number)) => vec![number.to_string()],
        Some(PatchValue::List(list)) => list.clone(),
    };
    if names.is_empty() {
        return Err(bad(at, "needs value: a task or a list of tasks"));
    }
    names
        .iter()
        .map(|name| parsed(at, "task", name, task_ref))
        .collect()
}

const fn relation_text(link: LinkKind) -> &'static str {
    match link {
        LinkKind::Blocks => "blocks",
        LinkKind::BlockedBy => "blocked by",
        LinkKind::Related => "related",
    }
}

fn source_args(target: String, meeting: &str, dry_run: bool, write: WriteArgs) -> SourceLinkArgs {
    SourceLinkArgs {
        target,
        meeting: Some(meeting.to_owned()),
        note: None,
        loop_id: None,
        at: None,
        dry_run,
        write,
    }
}

fn at_step(at: usize, mut error: ExternalError) -> ExternalError {
    error.message = format!("patch[{at}]: {}", error.message);
    error
}

/// The task a save without one makes.
fn new_task(update: &UpdateArgs) -> Result<CreateArgs, ExternalError> {
    if update.parent.is_some()
        || update.cycle.is_some()
        || update.snooze_until.is_some()
        || update.estimate.is_some()
        || update.for_person.is_some()
    {
        return Err(ExternalError::invalid(
            "A new task takes parent, cycle, snooze, estimate and for in a second save on it.",
        ));
    }
    let title = update.title.clone().ok_or_else(|| {
        ExternalError::invalid(
            r#"A save without a task makes one and needs a title step: {"field":"title","value":"..."}."#,
        )
    })?;
    Ok(CreateArgs {
        title,
        fields: update.fields.clone(),
        write: update.write.clone(),
        source_url: None,
        source_title: None,
        dry_run: update.dry_run,
    })
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
        let row = args.task.as_ref().map(|task| board.task(task)).transpose()?;
        if row.is_none() && args.expected_revision.is_some() {
            return Err(ExternalError::invalid(
                "--expected-revision needs a task: a new task has no revision yet.",
            ));
        }
        let mut update = UpdateArgs {
            tasks: row
                .map(|row| vec![TaskRef::Id(row.item.id.clone())])
                .unwrap_or_default(),
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
            estimate: None,
            for_person: None,
            expected_revision: args.expected_revision,
            dry_run: args.dry_run,
            write: args.write.clone(),
        };
        let mut details = row.and_then(|row| row.item.details.clone());
        let mut details_touched = false;
        let mut labels = row.map(|row| row.item.label_ids.clone()).unwrap_or_default();
        let mut labels_touched = false;
        let mut extras = Vec::new();
        for (at, entry) in entries.iter().enumerate() {
            let field = entry.field.as_str();
            let value = entry.value.as_ref();
            let ops: &[&str] = match field {
                "labels" | "description" | "details" => &[],
                "blocks" | "blocked_by" | "related" => &["add", "remove"],
                "links" => &["add"],
                _ => &["set"],
            };
            if entry
                .op
                .as_deref()
                .is_some_and(|op| !ops.is_empty() && !ops.contains(&op))
            {
                return Err(bad(at, format!("{field} takes no op but {}", ops.join(" or "))));
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
                "estimate" => update.estimate = Some(parsed(at, field, &one(at, value, "none")?, super::estimate_arg)?),
                "for" => {
                    let person = one(at, value, "none")?;
                    if !person.trim().eq_ignore_ascii_case("none") {
                        board.person(&person).map_err(|error| bad(at, error.message))?;
                    }
                    update.for_person = Some(person);
                }
                "delegate" => {
                    let agent = one(at, value, "")?;
                    extras.push((at, Extra::Delegate(Some(agent).filter(|agent| !agent.trim().is_empty()))));
                }
                "blocks" | "blocked_by" | "related" => {
                    let link = enumerated::<LinkKind>(at, "link", field)?;
                    let remove = entry.op.as_deref() == Some("remove");
                    for other in tasks_named(at, value)? {
                        extras.push((at, Extra::Relation { link, other, remove }));
                    }
                }
                "duplicate_of" => {
                    let of = one(at, value, "")?;
                    extras.push((at, Extra::DuplicateOf(parsed(at, field, &of, task_ref)?)));
                }
                "links" => {
                    let urls = many(at, value)?;
                    if urls.is_empty() {
                        return Err(bad(at, "needs value: a link or a list of links"));
                    }
                    extras.extend(urls.into_iter().map(|url| (at, Extra::Link(url))));
                }
                "source" => extras.push((at, Extra::Source(one(at, value, "")?))),
                other => {
                    return Err(bad(
                        at,
                        format!("{other:?} is not a field: title, description, status, priority, owner, due, project, milestone, labels, parent, cycle, repeat, snooze, estimate, for, delegate, blocks, blocked_by, related, duplicate_of, links or source"),
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
        if let (Some(row), Some(threads::ParentArg::Task(parent))) = (row, &update.parent) {
            let parent = board.task(parent)?;
            if board.above(&row.item.id, &parent.item.id) {
                return Err(ExternalError::invalid(format!(
                    "{} cannot go under {}: a task cannot sit under itself or one of its own sub-tasks.",
                    handle(row.item.number),
                    handle(parent.item.number)
                )));
            }
        }
        let creating = match row {
            Some(_) => None,
            None => Some(new_task(&update)?),
        };
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
        let base = args.write.operation_id(
            "save",
            &(row.map(|row| &row.item.id), &args.patch_json, &today),
        )?;
        let mut planned = Vec::with_capacity(extras.len());
        for (at, extra) in &extras {
            let change = self
                .rehearse_extra(&board, row, extra, &args.write)
                .await
                .map_err(|error| at_step(*at, error))?;
            planned.push(change);
        }
        let fields_named = entries
            .iter()
            .any(|entry| !EXTRA_FIELDS.contains(&entry.field.as_str()));
        let main = match (&creating, fields_named) {
            (Some(create), _) => Some(self.create(create).await?),
            (None, true) => Some(self.update(&update).await?),
            (None, false) => None,
        };
        if args.dry_run {
            let mut preview = match main {
                Some(WorkAnswer::Preview(preview)) => preview,
                Some(answer) => return Ok(answer),
                None => ExternalWorkPreview {
                    schema_version: QUERY_SCHEMA_VERSION,
                    dry_run: true,
                    action: WorkAction::Updated,
                    operation_id: Some(base),
                    changes: row
                        .map(|row| board.planned(row, &WorkPatch::default(), None))
                        .into_iter()
                        .collect(),
                },
            };
            if let Some(change) = preview.changes.first_mut() {
                if !planned.is_empty() {
                    change.skipped = None;
                }
                change.fields.extend(planned);
            }
            return Ok(WorkAnswer::Preview(preview));
        }
        let (task_id, name) = match (row, &main) {
            (Some(row), _) => (row.item.id.clone(), handle(row.item.number)),
            (None, Some(WorkAnswer::Change(change))) => change
                .tasks
                .first()
                .map(|task| (task.id.clone(), task.handle.clone()))
                .ok_or_else(|| {
                    ExternalError::new(ExternalErrorCode::Failed, "The new task could not be read back.")
                })?,
            (None, _) => {
                return Err(ExternalError::new(
                    ExternalErrorCode::Failed,
                    "The new task could not be read back.",
                ))
            }
        };
        if extras.is_empty() {
            if let Some(answer) = main {
                return Ok(answer);
            }
        }
        let base_id = Uuid::parse_str(&base).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The change could not be named.")
        })?;
        for (index, (at, extra)) in extras.iter().enumerate() {
            let write = WriteArgs {
                operation_id: Some(Uuid::new_v5(&base_id, index.to_string().as_bytes())),
                agent: args.write.agent.clone(),
            };
            if let Err(error) = self.apply_extra(&task_id, extra, write).await {
                let kept = if index == 0 && main.is_none() {
                    "Nothing was saved.".to_owned()
                } else {
                    format!(
                        "{name} kept every step before it; to finish, save only the steps from patch[{at}] on."
                    )
                };
                let mut error = at_step(*at, error);
                error.message = format!("{} {kept}", error.message);
                return Err(error);
            }
        }
        let board = self.board().await?;
        let task = board
            .row(&task_id)
            .map(|row| project_task(row, &board))
            .ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::Failed, "The task could not be read back.")
            })?;
        Ok(match main {
            Some(WorkAnswer::Change(mut change)) => {
                change.tasks = vec![task];
                change.operation_id.get_or_insert(base);
                WorkAnswer::Change(change)
            }
            Some(answer) => answer,
            None => changed(WorkAction::Updated, Some(base), vec![task], Vec::new()),
        })
    }

    /// Checks `extra` without writing, and says what it would change.
    async fn rehearse_extra(
        &self,
        board: &Board,
        row: Option<&WorkItemRow>,
        extra: &Extra,
        write: &WriteArgs,
    ) -> Result<ExternalFieldChange, ExternalError> {
        let task = row.map(|row| TaskRef::Id(row.item.id.clone()));
        Ok(match extra {
            Extra::Delegate(agent) => {
                self.applies("handing a task to an agent")?;
                ExternalFieldChange {
                    field: "worked by",
                    from: row
                        .and_then(|row| row.doer.as_ref())
                        .map_or_else(|| "nobody".to_owned(), |doer| doer.agent.clone()),
                    to: agent.as_deref().map_or_else(
                        || "nobody".to_owned(),
                        |name| agents::agent_name(Some(name)),
                    ),
                }
            }
            Extra::Relation {
                link,
                other,
                remove,
            } => {
                let other = board.task(other)?;
                if let Some(task) = task {
                    let args = RelateArgs {
                        task,
                        link: *link,
                        other: TaskRef::Id(other.item.id.clone()),
                        dry_run: true,
                        write: write.clone(),
                    };
                    if *remove {
                        self.unrelate(&args).await?;
                    } else {
                        self.relate(&args).await?;
                    }
                } else if *remove {
                    return Err(ExternalError::invalid("a new task has no links to remove."));
                }
                let text = board.task_text(&other.item.id);
                let (from, to) = if *remove {
                    (text, String::new())
                } else {
                    (String::new(), text)
                };
                ExternalFieldChange {
                    field: relation_text(*link),
                    from,
                    to,
                }
            }
            Extra::DuplicateOf(of) => {
                let of = board.task(of)?;
                match task {
                    Some(task) => {
                        self.duplicate(&DuplicateArgs {
                            task,
                            of: TaskRef::Id(of.item.id.clone()),
                            dry_run: true,
                            write: write.clone(),
                        })
                        .await?;
                    }
                    None => self.applies("marking a duplicate")?,
                }
                ExternalFieldChange {
                    field: "duplicate of",
                    from: String::new(),
                    to: board.task_text(&of.item.id),
                }
            }
            Extra::Link(url) => ExternalFieldChange {
                field: "link",
                from: String::new(),
                to: attachments::link(url)
                    .map_err(|error| refusal(self.history, planning::failure(error)))?,
            },
            Extra::Source(meeting) => {
                linked_source(
                    self.meetings,
                    &source_args(String::new(), meeting, true, write.clone()),
                )
                .await?;
                self.applies("linking sources")?;
                ExternalFieldChange {
                    field: "source",
                    from: String::new(),
                    to: format!("meeting {}", meeting.trim()),
                }
            }
        })
    }

    async fn apply_extra(
        &self,
        task: &str,
        extra: &Extra,
        write: WriteArgs,
    ) -> Result<(), ExternalError> {
        let target = TaskRef::Id(task.to_owned());
        match extra {
            Extra::Delegate(agent) => {
                self.delegate(&DelegateArgs {
                    task: target,
                    to: agent.clone(),
                    take_back: agent.is_none(),
                    dry_run: false,
                    write,
                })
                .await?;
            }
            Extra::Relation {
                link,
                other,
                remove,
            } => {
                let args = RelateArgs {
                    task: target,
                    link: *link,
                    other: other.clone(),
                    dry_run: false,
                    write,
                };
                if *remove {
                    self.unrelate(&args).await?;
                } else {
                    self.relate(&args).await?;
                }
            }
            Extra::DuplicateOf(of) => {
                self.duplicate(&DuplicateArgs {
                    task: target,
                    of: of.clone(),
                    dry_run: false,
                    write,
                })
                .await?;
            }
            Extra::Link(url) => {
                self.attach(&AttachArgs {
                    task: target,
                    target: AttachTarget::Link(url.clone()),
                    title: None,
                    subtitle: None,
                    meta: Vec::new(),
                    comment: None,
                    dry_run: false,
                    write,
                })
                .await?;
            }
            Extra::Source(meeting) => {
                self.source_link(&source_args(task.to_owned(), meeting, false, write))
                    .await?;
            }
        }
        Ok(())
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
        let existing = if fence.is_open() {
            crate::query::external::store(self.meetings)
                .await?
                .session_ids()
                .map_err(|error| ExternalError::from(crate::query::QueryError::from(error)))?
        } else {
            std::collections::HashSet::new()
        };
        let meetings = MeetingFence { fence, existing };
        let hidden = planning::history(self.history, |history| {
            planning::work::drawing_on(history, |kind, id| meetings.hides(kind, id))
        })
        .map_err(|message| refusal(self.history, message))?;
        let hidden_notes: std::collections::HashSet<String> =
            planning::history(self.history, |history| {
                let mut statement = history.prepare(
                    "SELECT note_id, target_id FROM scratch_note_sources WHERE kind = 'meeting'",
                )?;
                let found = statement
                    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(found
                    .into_iter()
                    .filter(|(_, meeting)| {
                        meetings.hides(planning::EvidenceSourceKind::Meeting, meeting)
                    })
                    .map(|(note, _)| note)
                    .collect())
            })
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
            .filter(|change| !kept_here(change, &meetings, &hidden, &hidden_notes))
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

/// The meetings an outside reader of the stream may not see: those kept on
/// this Mac and, failing closed, any the store no longer holds, since a
/// deleted meeting can no longer say whether it was kept here.
struct MeetingFence {
    fence: Fence,
    existing: std::collections::HashSet<crate::meeting::types::MeetingSessionId>,
}

impl MeetingFence {
    fn hides(&self, kind: planning::EvidenceSourceKind, id: &str) -> bool {
        let session = match kind {
            planning::EvidenceSourceKind::Meeting => Uuid::parse_str(id)
                .ok()
                .map(crate::meeting::types::MeetingSessionId::from_uuid),
            planning::EvidenceSourceKind::Loop => {
                crate::meeting::loop_types::MeetingLoopId(id.to_owned()).session_id()
            }
            planning::EvidenceSourceKind::Note
            | planning::EvidenceSourceKind::Dictation
            | planning::EvidenceSourceKind::Link => return false,
        };
        self.fence.hides_source(kind, id)
            || !session.is_some_and(|session| self.existing.contains(&session))
    }
}

/// Whether a change stays out of an outside reader's stream: it is on a
/// task or a note drawn from a meeting the fence hides, on such a meeting
/// itself, or it was drawn from one when it was written.
fn kept_here(
    change: &changes::WorkChange,
    meetings: &MeetingFence,
    hidden_tasks: &std::collections::HashSet<String>,
    hidden_notes: &std::collections::HashSet<String>,
) -> bool {
    let subject = |field: &str| {
        change
            .data
            .as_ref()
            .and_then(|data| data.get(field))
            .and_then(serde_json::Value::as_str)
    };
    let note = match change.entity.as_str() {
        "note" => Some(change.entity_id.as_str()),
        "comment" if subject("subject_kind") == Some("note") => subject("subject_id"),
        _ => None,
    };
    change.item_id.as_ref().is_some_and(|id| hidden_tasks.contains(id))
        || note.is_some_and(|id| hidden_notes.contains(id))
        || (change.entity == "meeting"
            && meetings.hides(planning::EvidenceSourceKind::Meeting, &change.entity_id))
        || change.sources.iter().any(|(kind, id)| {
            let kind = if kind == "loop" {
                planning::EvidenceSourceKind::Loop
            } else {
                planning::EvidenceSourceKind::Meeting
            };
            meetings.hides(kind, id)
        })
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
        let data = |field: &str| {
            change
                .data
                .as_ref()
                .and_then(|data| data.get(field))
                .and_then(serde_json::Value::as_str)
        };
        let what = match (change.task.as_deref(), data("title").or_else(|| data("name"))) {
            (Some(task), _) => task.to_owned(),
            (None, Some(name)) => format!("“{name}”"),
            (None, None) => match data("subject_kind") {
                Some(kind) => format!("on a {kind}"),
                None => change.entity_id.clone(),
            },
        };
        let _ = writeln!(
            text,
            "{} {} {} {}{}{}",
            change.seq,
            change.action,
            change.entity.replace('_', " "),
            what,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn change(entity: &str, note: Option<&str>, sources: &[(&str, &str)]) -> changes::WorkChange {
        changes::WorkChange {
            seq: 1,
            at_ms: 1,
            entity: entity.to_owned(),
            entity_id: format!("{entity}-1"),
            item_id: None,
            action: "create".to_owned(),
            updated_from: None,
            data: note.map(|id| serde_json::json!({ "subject_kind": "note", "subject_id": id })),
            sources: sources
                .iter()
                .map(|(kind, id)| ((*kind).to_owned(), (*id).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn work_drawn_from_a_meeting_kept_here_or_deleted_stays_out_of_the_stream() {
        let session = |id: &str| {
            crate::meeting::types::MeetingSessionId::from_uuid(
                Uuid::parse_str(id).expect("a meeting id"),
            )
        };
        let private = "11111111-1111-4111-8111-111111111111";
        let shared = "22222222-2222-4222-8222-222222222222";
        let deleted = "33333333-3333-4333-8333-333333333333";
        let meetings = MeetingFence {
            fence: Fence::Open(crate::query::KeptLocal(HashSet::from([session(private)]))),
            existing: HashSet::from([session(private), session(shared)]),
        };
        let hidden_notes = HashSet::from(["note-7".to_owned()]);
        let none = HashSet::new();
        let read = |change: &changes::WorkChange| kept_here(change, &meetings, &none, &hidden_notes);

        let left_out = [
            change("meeting", None, &[("meeting", private)]),
            change("task", None, &[("meeting", deleted)]),
            change("comment", Some("note-7"), &[]),
        ];
        let shown = [
            change("project", None, &[]),
            change("task", None, &[("meeting", shared)]),
            change("comment", Some("note-8"), &[]),
        ];

        assert_eq!(left_out.iter().map(read).collect::<Vec<_>>(), [true; 3]);
        assert_eq!(shown.iter().map(read).collect::<Vec<_>>(), [false; 3]);
    }
}
