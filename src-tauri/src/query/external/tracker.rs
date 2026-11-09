//! `sona work`: the task tracker for agents and scripts.
//!
//! A subcommand rather than more corpus flags. Tracker verbs take a dozen
//! modifiers that would collide with the corpus verbs' own (`--status`,
//! `--project`, `--mine`), and a subcommand binds each modifier to its verb by
//! construction. Text is the default; `--json` prints one value.
//!
//! Every write goes through the tracker's own functions, filed under
//! [`WorkActor::Agent`], and needs both grants, as `--loop-resolve` does. Each
//! carries an operation id: the caller's `--operation-id`, or one derived from
//! the request and the revisions it read, so a retry of a change that landed
//! answers with the first result or finds nothing left to change.
//!
//! Work raised in a meeting kept local is left out of every answer and counted
//! nowhere, as that meeting's loops are. While the conversations can't be
//! opened nothing says which meetings those are, so all work from meetings is
//! left out and the answer says so.

use super::{
    day_start_utc_ms, evidence_kept_local, kept_local, local_day, local_time, project_evidence,
    store, ExternalError, ExternalErrorCode, ExternalPlanningEvidence, ExternalScope,
    DEFAULT_LIMIT, MAX_LIMIT, READ_SCOPES, WRITE_SCOPES,
};
use crate::managers::history::HistoryManager;
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingSessionId;
use crate::planning;
use crate::planning::work::types::{
    WorkActivity, WorkActivityKind, WorkActor, WorkDirection, WorkDisplay, WorkFieldSuggestion,
    WorkGroupBy, WorkItem, WorkItemDetail, WorkItemInput, WorkItemRow, WorkLabel, WorkLayout,
    WorkList, WorkOrderBy, WorkOwner, WorkPatch, WorkPersonChoice, WorkProjectChoice, WorkQuery,
    WorkRelation, WorkRelationKind, WorkSource, WorkStatus, WorkSuggestionField,
    WorkSuggestionValue, WorkView,
};
use crate::planning::work::{self, items};
use crate::query::{loop_link, KeptLocal, QUERY_SCHEMA_VERSION};
use chrono::{Local, NaiveDate};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// Names derived operation ids, so the same request always derives the same
/// id and no other request in Sona can.
const OPERATION_NAMESPACE: Uuid = Uuid::from_u128(0x5f0e_2c71_a8d4_4b39_9e06_d13a_7c52_e8b4);

/// The properties the app's own new views show.
const VIEW_PROPERTIES: [&str; 6] = ["id", "priority", "labels", "project", "owner", "due"];

const OPEN_AND_TRIAGE: [WorkStatus; 4] = [
    WorkStatus::Triage,
    WorkStatus::Backlog,
    WorkStatus::NotStarted,
    WorkStatus::InProgress,
];

/// `sona work <verb>`, and how to print the answer.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct WorkCommand {
    #[command(subcommand)]
    pub verb: WorkVerb,
    /// Print one JSON value instead of text.
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum WorkVerb {
    /// List tasks, most urgent first. Without --status: triage and open work.
    List(ListArgs),
    /// Todo tasks nothing open blocks, not snoozed: what to pick up next.
    Ready(ReadyArgs),
    /// The counts, what you owe, and what others owe you.
    Mine(MineArgs),
    /// Triage, your open work, overdue, due today, and waiting on, as numbers.
    Counts,
    /// One task with its quote, links, evidence, suggestions and history.
    Show(ShowArgs),
    /// Create a task: Todo, no priority and no owner unless told otherwise.
    Create(CreateArgs),
    /// Change tasks. Several tasks take the same change together or not at all.
    Update(UpdateArgs),
    /// Start a Todo or Backlog task: it moves to In progress and keeps its owner.
    Claim(ClaimArgs),
    /// Mark tasks done. A meeting commitment behind one closes with it.
    Done(DoneArgs),
    /// Accept triage items into Todo with Sona's suggested owner, due day and project.
    Accept(AcceptArgs),
    /// Decline triage items: canceled, and their meeting commitments set aside.
    Decline(TasksArgs),
    /// Mark a task a duplicate of another; its quotes and links move there.
    Duplicate(DuplicateArgs),
    /// Hide tasks from every list. Their status stays.
    Archive(TasksArgs),
    /// Bring archived tasks back.
    Restore(TasksArgs),
    /// Link two tasks: S-1 blocks S-2, S-1 blocked-by S-2, or S-1 related S-2.
    Relate(RelateArgs),
    /// Remove a link between two tasks.
    Unrelate(RelateArgs),
    /// List labels.
    Labels,
    /// Create a label, or rename or recolor one.
    LabelSave(LabelSaveArgs),
    /// Delete a label. Its tasks lose only that label.
    LabelDelete(LabelDeleteArgs),
    /// List saved views.
    Views,
    /// Save a view: named filters that the app and `list --view` use.
    ViewSave(ViewSaveArgs),
    /// Delete a saved view.
    ViewDelete(ViewDeleteArgs),
    /// Write matching tasks to a folder as Markdown, one file per task.
    Export(ExportArgs),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ListArgs {
    #[command(flatten)]
    pub filters: Filters,
    /// Start from a saved view's filters (name or id); other flags narrow it.
    #[arg(long)]
    pub view: Option<String>,
    /// Rows to print, at most 100.
    #[arg(long, value_parser = limit)]
    pub limit: Option<usize>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ReadyArgs {
    #[command(flatten)]
    pub scope: Scope,
    /// Rows to print, at most 100.
    #[arg(long, value_parser = limit)]
    pub limit: Option<usize>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MineArgs {
    /// Rows per list, at most 100.
    #[arg(long, value_parser = limit)]
    pub limit: Option<usize>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ShowArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CreateArgs {
    pub title: String,
    #[command(flatten)]
    pub fields: Fields,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct UpdateArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    #[arg(long)]
    pub title: Option<String>,
    #[command(flatten)]
    pub fields: Fields,
    /// Labels to add, by name or id, comma-separated.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "LABEL",
        conflicts_with = "labels"
    )]
    pub add_label: Vec<String>,
    /// Labels to remove, by name or id, comma-separated.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "LABEL",
        conflicts_with = "labels"
    )]
    pub remove_label: Vec<String>,
    /// Hide the task until this day: YYYY-MM-DD, today, tomorrow, or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub snooze_until: Option<DayArg>,
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
pub struct ClaimArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DoneArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// A link or one line showing the work is done, added to the details.
    #[arg(long, value_name = "URL")]
    pub evidence: Option<String>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct AcceptArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// Accept without Sona's suggested owner, due day and project.
    #[arg(long)]
    pub ignore_suggestions: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TasksArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DuplicateArgs {
    /// The duplicate: S-12, 12, or its id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// The task it repeats.
    #[arg(long, value_parser = task_ref, value_name = "TASK")]
    pub of: TaskRef,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct RelateArgs {
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    #[arg(value_enum)]
    pub link: LinkKind,
    #[arg(value_parser = task_ref, value_name = "OTHER")]
    pub other: TaskRef,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LabelSaveArgs {
    /// The label's name. Saving a name that exists changes that label.
    pub name: String,
    /// The label to rename to NAME, by name or id.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    /// Color: degrees on the color wheel, 0 to 360.
    #[arg(long, value_parser = clap::value_parser!(u16).range(0..=360))]
    pub hue: Option<u16>,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LabelDeleteArgs {
    /// The label's name or id.
    pub label: String,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ViewSaveArgs {
    /// The view's name. Saving a name that exists changes that view.
    pub name: String,
    /// The view to rename to NAME, by name or id.
    #[arg(long, value_name = "VIEW")]
    pub view: Option<String>,
    #[command(flatten)]
    pub filters: Filters,
    #[arg(long, value_enum)]
    pub layout: Option<LayoutArg>,
    #[arg(long, value_enum)]
    pub group_by: Option<GroupArg>,
    #[arg(long, value_enum)]
    pub order_by: Option<OrderArg>,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ViewDeleteArgs {
    /// The view's name or id.
    pub view: String,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ExportArgs {
    /// The folder to write S-<number>.md files into.
    #[arg(long, value_name = "DIR")]
    pub md: PathBuf,
    #[command(flatten)]
    pub filters: Filters,
}

/// Which tasks a list keeps, by who and what.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct Scope {
    /// Only work you owe.
    #[arg(long, conflicts_with_all = ["waiting", "unowned"])]
    pub mine: bool,
    /// Only work others owe you.
    #[arg(long, conflicts_with = "unowned")]
    pub waiting: bool,
    /// Only work nobody owns.
    #[arg(long)]
    pub unowned: bool,
    /// Owners by name or id, comma-separated; me or nobody alone.
    #[arg(long, value_delimiter = ',', value_name = "PERSON")]
    pub owner: Vec<String>,
    /// Projects by name or id, comma-separated; none for work in no project.
    #[arg(long, value_delimiter = ',', value_name = "PROJECT")]
    pub project: Vec<String>,
    /// Labels by name or id, comma-separated: work with any of them.
    #[arg(long, value_delimiter = ',', value_name = "LABEL")]
    pub label: Vec<String>,
    /// urgent, high, medium, low or none, comma-separated.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub priority: Vec<Priority>,
    /// Due on or before this day: YYYY-MM-DD, today or tomorrow.
    #[arg(long, value_parser = day, value_name = "DAY")]
    pub due_by: Option<NaiveDate>,
    /// Work raised in this meeting, by its id.
    #[arg(long, value_name = "MEETING_ID")]
    pub meeting: Option<Uuid>,
    /// Words in the title or details, or a task number.
    #[arg(long)]
    pub search: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct Filters {
    #[command(flatten)]
    pub scope: Scope,
    /// triage, backlog, todo, in-progress, done, canceled, duplicate, open,
    /// closed or all, comma-separated.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub status: Vec<StatusFilter>,
    /// Include archived work.
    #[arg(long)]
    pub archived: bool,
    /// Include snoozed work.
    #[arg(long)]
    pub snoozed: bool,
}

/// What a new task starts with, or what an update sets.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct Fields {
    #[arg(long)]
    pub details: Option<String>,
    /// triage, backlog, todo, in-progress, done or canceled.
    #[arg(long, value_enum)]
    pub status: Option<StatusArg>,
    /// urgent, high, medium, low or none (1 to 4, 0 for none).
    #[arg(long, value_enum)]
    pub priority: Option<Priority>,
    /// me, nobody, or a person's name or id.
    #[arg(long)]
    pub owner: Option<String>,
    /// YYYY-MM-DD, today, tomorrow, or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub due: Option<DayArg>,
    /// A project's name or id, or none.
    #[arg(long)]
    pub project: Option<String>,
    /// A milestone id in the task's project, or none.
    #[arg(long)]
    pub milestone: Option<String>,
    /// All of the task's labels, by name or id, comma-separated; "" clears them.
    #[arg(long, value_delimiter = ',', value_name = "LABEL")]
    pub labels: Option<Vec<String>>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct WriteArgs {
    /// A UUID naming this change, so a retry cannot apply it twice. Derived
    /// from the request when left out.
    #[arg(long, value_name = "UUID")]
    pub operation_id: Option<Uuid>,
}

/// A task as a caller names it: its number (`S-12`, `#12`, `12`) or its id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskRef {
    Number(i64),
    Id(String),
}

impl fmt::Display for TaskRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(number) => write!(formatter, "S-{number}"),
            Self::Id(id) => formatter.write_str(id),
        }
    }
}

/// A day to set, or `none` to clear it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DayArg {
    Clear,
    On(NaiveDate),
}

impl DayArg {
    fn ms(self) -> Result<Option<i64>, ExternalError> {
        match self {
            Self::Clear => Ok(None),
            Self::On(day) => midnight(day).map(Some),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum StatusArg {
    Triage,
    Backlog,
    #[value(alias = "not-started", alias = "not_started")]
    Todo,
    #[value(alias = "in_progress")]
    InProgress,
    Done,
    #[value(alias = "cancelled")]
    Canceled,
}

impl StatusArg {
    const fn status(self) -> WorkStatus {
        match self {
            Self::Triage => WorkStatus::Triage,
            Self::Backlog => WorkStatus::Backlog,
            Self::Todo => WorkStatus::NotStarted,
            Self::InProgress => WorkStatus::InProgress,
            Self::Done => WorkStatus::Done,
            Self::Canceled => WorkStatus::Canceled,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum StatusFilter {
    Triage,
    Backlog,
    #[value(alias = "not-started", alias = "not_started")]
    Todo,
    #[value(alias = "in_progress")]
    InProgress,
    Done,
    #[value(alias = "cancelled")]
    Canceled,
    Duplicate,
    Open,
    Closed,
    All,
}

impl StatusFilter {
    const fn statuses(self) -> &'static [WorkStatus] {
        match self {
            Self::Triage => &[WorkStatus::Triage],
            Self::Backlog => &[WorkStatus::Backlog],
            Self::Todo => &[WorkStatus::NotStarted],
            Self::InProgress => &[WorkStatus::InProgress],
            Self::Done => &[WorkStatus::Done],
            Self::Canceled => &[WorkStatus::Canceled],
            Self::Duplicate => &[WorkStatus::Duplicate],
            Self::Open => &[
                WorkStatus::Backlog,
                WorkStatus::NotStarted,
                WorkStatus::InProgress,
            ],
            Self::Closed => &[
                WorkStatus::Done,
                WorkStatus::Canceled,
                WorkStatus::Duplicate,
            ],
            Self::All => &[
                WorkStatus::Triage,
                WorkStatus::Backlog,
                WorkStatus::NotStarted,
                WorkStatus::InProgress,
                WorkStatus::Done,
                WorkStatus::Canceled,
                WorkStatus::Duplicate,
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    #[value(alias = "0")]
    None,
    #[value(alias = "1")]
    Urgent,
    #[value(alias = "2")]
    High,
    #[value(alias = "3")]
    Medium,
    #[value(alias = "4")]
    Low,
}

impl Priority {
    const fn of(value: u8) -> Self {
        match value {
            1 => Self::Urgent,
            2 => Self::High,
            3 => Self::Medium,
            4 => Self::Low,
            _ => Self::None,
        }
    }

    const fn value(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Urgent => 1,
            Self::High => 2,
            Self::Medium => 3,
            Self::Low => 4,
        }
    }

    const fn text(self) -> &'static str {
        match self {
            Self::None => "No priority",
            Self::Urgent => "Urgent",
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
        }
    }

    const fn short(self) -> &'static str {
        match self {
            Self::None => "-",
            other => other.text(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LinkKind {
    Blocks,
    #[value(alias = "blocked_by")]
    BlockedBy,
    #[value(alias = "relates")]
    Related,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LayoutArg {
    List,
    Board,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GroupArg {
    Status,
    Owner,
    Project,
    Meeting,
    Due,
    Priority,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OrderArg {
    Priority,
    Due,
    Updated,
    Created,
    Manual,
}

fn task_ref(value: &str) -> Result<TaskRef, String> {
    let text = value.trim();
    let number = text
        .strip_prefix("S-")
        .or_else(|| text.strip_prefix("s-"))
        .or_else(|| text.strip_prefix('#'))
        .unwrap_or(text);
    if let Ok(number) = number.parse::<i64>() {
        if number > 0 {
            return Ok(TaskRef::Number(number));
        }
    }
    Uuid::parse_str(text)
        .map(|id| TaskRef::Id(id.to_string()))
        .map_err(|_| "expected a task: S-12, 12, or a task id".to_owned())
}

fn limit(value: &str) -> Result<usize, String> {
    match value.trim().parse::<usize>() {
        Ok(0) | Err(_) => Err("expected a number of rows, at least 1".to_owned()),
        Ok(rows) => Ok(rows.min(MAX_LIMIT)),
    }
}

fn day(value: &str) -> Result<NaiveDate, String> {
    let today = Local::now().date_naive();
    match value.trim().to_ascii_lowercase().as_str() {
        "today" => Ok(today),
        "tomorrow" => today
            .succ_opt()
            .ok_or_else(|| "tomorrow is past the calendar".to_owned()),
        other => NaiveDate::parse_from_str(other, "%Y-%m-%d")
            .map_err(|_| "expected YYYY-MM-DD, today or tomorrow".to_owned()),
    }
}

fn day_or_none(value: &str) -> Result<DayArg, String> {
    if value.trim().eq_ignore_ascii_case("none") {
        Ok(DayArg::Clear)
    } else {
        day(value).map(DayArg::On)
    }
}

fn midnight(day: NaiveDate) -> Result<i64, ExternalError> {
    day_start_utc_ms(&day.format("%Y-%m-%d").to_string())
}

impl WorkVerb {
    /// Which grants this verb needs. A dry run only reads.
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::List(_)
            | Self::Ready(_)
            | Self::Mine(_)
            | Self::Counts
            | Self::Show(_)
            | Self::Labels
            | Self::Views
            | Self::Export(_)
            | Self::Update(UpdateArgs { dry_run: true, .. })
            | Self::Done(DoneArgs { dry_run: true, .. })
            | Self::Accept(AcceptArgs { dry_run: true, .. })
            | Self::Decline(TasksArgs { dry_run: true, .. })
            | Self::Archive(TasksArgs { dry_run: true, .. })
            | Self::Restore(TasksArgs { dry_run: true, .. }) => READ_SCOPES,
            Self::Create(_)
            | Self::Update(_)
            | Self::Claim(_)
            | Self::Done(_)
            | Self::Accept(_)
            | Self::Decline(_)
            | Self::Duplicate(_)
            | Self::Archive(_)
            | Self::Restore(_)
            | Self::Relate(_)
            | Self::Unrelate(_)
            | Self::LabelSave(_)
            | Self::LabelDelete(_)
            | Self::ViewSave(_)
            | Self::ViewDelete(_) => WRITE_SCOPES,
        }
    }

    /// What clap cannot check: a flag that needs one task, a change that
    /// changes nothing, a name left empty.
    pub(super) fn check(&self) -> Result<(), ExternalError> {
        let refusal = match self {
            Self::Create(args) if args.title.trim().is_empty() => "A task needs a title.",
            Self::Update(args)
                if args
                    .title
                    .as_deref()
                    .is_some_and(|title| title.trim().is_empty()) =>
            {
                "--title cannot be empty."
            }
            Self::Update(args) if args.expected_revision.is_some() && args.tasks.len() > 1 => {
                "--expected-revision fences one task; name one."
            }
            Self::Update(args) if !args.changes_something() => {
                "Name a change: --title, --details, --status, --priority, --owner, --due, --project, --milestone, --labels, --add-label, --remove-label or --snooze-until."
            }
            Self::Done(args) if args.evidence.is_some() && args.tasks.len() > 1 => {
                "--evidence belongs to one task; name one."
            }
            Self::Done(args)
                if args.evidence.as_deref().is_some_and(|evidence| {
                    evidence.trim().is_empty() || evidence.contains(['\n', '\r'])
                }) =>
            {
                "--evidence takes one line: a link or a short note."
            }
            Self::LabelSave(args) if args.name.trim().is_empty() => "A label needs a name.",
            Self::ViewSave(args) if args.name.trim().is_empty() => "A view needs a name.",
            _ => return Ok(()),
        };
        Err(ExternalError::invalid(refusal))
    }
}

impl UpdateArgs {
    fn changes_something(&self) -> bool {
        let fields = &self.fields;
        self.title.is_some()
            || fields.details.is_some()
            || fields.status.is_some()
            || fields.priority.is_some()
            || fields.owner.is_some()
            || fields.due.is_some()
            || fields.project.is_some()
            || fields.milestone.is_some()
            || fields.labels.is_some()
            || !self.add_label.is_empty()
            || !self.remove_label.is_empty()
            || self.snooze_until.is_some()
    }
}

impl WriteArgs {
    /// The caller's id, or one derived from the verb and what it read: the
    /// same request against the same revisions derives the same id.
    fn operation_id(&self, verb: &str, request: &impl Serialize) -> Result<String, ExternalError> {
        if let Some(id) = self.operation_id {
            return Ok(id.to_string());
        }
        let mut name = verb.as_bytes().to_vec();
        name.push(0);
        serde_json::to_writer(&mut name, request).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The change could not be named.")
        })?;
        Ok(Uuid::new_v5(&OPERATION_NAMESPACE, &name).to_string())
    }

    /// The caller's id, or a new one. For creations a deleted twin could
    /// otherwise replay: a label or view made, deleted, and made again.
    fn fresh_operation_id(&self) -> String {
        self.operation_id.unwrap_or_else(Uuid::new_v4).to_string()
    }
}

/// One `sona work` answer. Serialised untagged, like the corpus answers.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum WorkAnswer {
    Page(ExternalWorkPage),
    Mine(ExternalWorkMine),
    Counts(ExternalWorkCountsPage),
    Detail(Box<ExternalWorkDetail>),
    Change(ExternalWorkChange),
    Preview(ExternalWorkPreview),
    Relation(ExternalWorkRelationChange),
    Labels(ExternalWorkLabels),
    Label(ExternalWorkLabelChange),
    Views(ExternalWorkViews),
    View(Box<ExternalWorkViewChange>),
    Export(ExternalWorkExport),
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkItem {
    pub id: String,
    /// `S-<number>`, accepted anywhere an id is.
    pub handle: String,
    pub number: i64,
    pub title: String,
    pub details: Option<String>,
    pub status: WorkStatus,
    pub priority: Priority,
    pub owner: ExternalWorkOwner,
    pub direction: WorkDirection,
    /// The local day it is due, YYYY-MM-DD.
    pub due_date: Option<String>,
    pub due_at_ms: Option<i64>,
    pub project: Option<ExternalWorkName>,
    pub milestone_id: Option<String>,
    pub labels: Vec<ExternalWorkName>,
    /// The meeting commitment the task came from.
    pub source: Option<ExternalWorkSource>,
    pub duplicate_of_id: Option<String>,
    /// Open tasks that block this one.
    pub blocked_by_open: u32,
    pub snoozed_until_ms: Option<i64>,
    pub archived_at_ms: Option<i64>,
    pub status_changed_at_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalWorkOwner {
    #[serde(rename = "self")]
    Myself,
    Person {
        person_id: String,
        name: Option<String>,
    },
    Nobody,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkName {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSource {
    pub loop_id: String,
    pub link: String,
    pub meeting_id: Option<String>,
    pub meeting_title: Option<String>,
    pub quote: Option<String>,
    pub speaker: Option<String>,
    /// Offset into the meeting where it was said.
    pub at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPage {
    pub schema_version: u32,
    pub entries: Vec<ExternalWorkItem>,
    /// Matching tasks before `--limit`.
    pub total: usize,
    pub has_more: bool,
    /// False while the conversations can't be opened: work from meetings is
    /// left out.
    pub conversations_available: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ExternalWorkCounts {
    pub triage: u32,
    pub mine_open: u32,
    pub mine_overdue: u32,
    pub mine_due_today: u32,
    pub waiting_on: u32,
    /// Waiting-on work whose follow-up moment has come.
    pub waiting_on_stale: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCountsPage {
    pub schema_version: u32,
    #[serde(flatten)]
    pub counts: ExternalWorkCounts,
    /// False while the conversations can't be opened: work from meetings is
    /// not counted.
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMine {
    pub schema_version: u32,
    pub counts: ExternalWorkCounts,
    pub i_owe: Vec<ExternalWorkItem>,
    pub waiting_on: Vec<ExternalWorkItem>,
    pub has_more: bool,
    /// False while the conversations can't be opened: work from meetings is
    /// left out.
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkDetail {
    pub schema_version: u32,
    pub task: ExternalWorkItem,
    pub relations: Vec<ExternalWorkLink>,
    pub evidence: Vec<ExternalPlanningEvidence>,
    /// What Sona suggests for a task still in triage.
    pub suggestions: Vec<ExternalWorkSuggestion>,
    pub activity: Vec<ExternalWorkActivity>,
    /// False while the conversations can't be opened: meeting quotes and
    /// suggestions are left out.
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLink {
    pub id: String,
    /// How the other task stands to this one.
    pub kind: ExternalLinkKind,
    pub task: ExternalWorkRef,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalLinkKind {
    Blocks,
    BlockedBy,
    Related,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkRef {
    pub id: String,
    pub handle: String,
    pub title: String,
    pub status: WorkStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSuggestion {
    pub field: WorkSuggestionField,
    /// The suggestion as a person reads it: a name, a day, a title.
    pub value: String,
    /// The person, project or task suggested, when it is one.
    pub id: Option<String>,
    pub due_at_ms: Option<i64>,
    pub reason: String,
    pub quote: Option<String>,
    pub meeting_id: Option<String>,
    pub at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkActivity {
    pub at_ms: i64,
    pub actor: WorkActor,
    pub kind: WorkActivityKind,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkAction {
    Created,
    Updated,
    Claimed,
    Done,
    Accepted,
    Declined,
    MarkedDuplicate,
    Archived,
    Restored,
}

impl WorkAction {
    const fn verb(self) -> &'static str {
        match self {
            Self::Created => "create",
            Self::Updated => "update",
            Self::Claimed => "claim",
            Self::Done => "done",
            Self::Accepted => "accept",
            Self::Declined => "decline",
            Self::MarkedDuplicate => "duplicate",
            Self::Archived => "archive",
            Self::Restored => "restore",
        }
    }

    const fn heading(self) -> &'static str {
        match self {
            Self::Created => "Created",
            Self::Updated => "Updated",
            Self::Claimed => "Claimed: in progress, the owner unchanged",
            Self::Done => "Done",
            Self::Accepted => "Accepted",
            Self::Declined => "Declined",
            Self::MarkedDuplicate => "Marked duplicate",
            Self::Archived => "Archived",
            Self::Restored => "Restored",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkChange {
    pub schema_version: u32,
    pub action: WorkAction,
    /// Absent when nothing needed changing.
    pub operation_id: Option<String>,
    pub tasks: Vec<ExternalWorkItem>,
    pub skipped: Vec<ExternalWorkSkip>,
    /// True when this create answered from an earlier run of the same
    /// request rather than creating another task.
    pub replayed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSkip {
    pub id: String,
    pub handle: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPreview {
    pub schema_version: u32,
    pub dry_run: bool,
    pub action: WorkAction,
    /// The id the same command would write under, unless the tasks change
    /// first.
    pub operation_id: Option<String>,
    pub changes: Vec<ExternalWorkPlanned>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPlanned {
    pub id: String,
    pub handle: String,
    pub title: String,
    pub fields: Vec<ExternalFieldChange>,
    pub commitment: Option<CommitmentEffect>,
    /// Why this task would be left alone.
    pub skipped: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFieldChange {
    pub field: &'static str,
    pub from: String,
    pub to: String,
}

/// What a change does to the meeting commitment a task came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentEffect {
    Closes,
    SetsAside,
    Reopens,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkRelationChange {
    pub schema_version: u32,
    pub operation_id: String,
    pub removed: bool,
    pub relation: ExternalWorkRelation,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkRelation {
    pub id: String,
    pub kind: WorkRelationKind,
    pub from: ExternalWorkRef,
    pub to: ExternalWorkRef,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLabels {
    pub schema_version: u32,
    pub labels: Vec<ExternalWorkLabel>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLabel {
    pub id: String,
    pub name: String,
    pub hue: f64,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLabelChange {
    pub schema_version: u32,
    /// Absent when the label already read this way.
    pub operation_id: Option<String>,
    pub deleted: bool,
    pub label: ExternalWorkLabel,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkViews {
    pub schema_version: u32,
    pub views: Vec<ExternalWorkView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkView {
    pub id: String,
    pub name: String,
    pub filters: ExternalWorkFilters,
    pub layout: WorkLayout,
    pub group_by: WorkGroupBy,
    pub order_by: WorkOrderBy,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkFilters {
    pub statuses: Option<Vec<WorkStatus>>,
    pub direction: Option<WorkDirection>,
    pub owner_person_ids: Option<Vec<String>>,
    pub project_ids: Option<Vec<String>>,
    pub label_ids: Option<Vec<String>>,
    pub priorities: Option<Vec<Priority>>,
    pub due_before_ms: Option<i64>,
    pub meeting_id: Option<String>,
    pub search: Option<String>,
    pub include_archived: bool,
    pub include_snoozed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkViewChange {
    pub schema_version: u32,
    pub operation_id: String,
    pub deleted: bool,
    pub view: ExternalWorkView,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkExport {
    pub schema_version: u32,
    pub directory: String,
    pub files: Vec<String>,
    /// False while the conversations can't be opened: work from meetings is
    /// not written.
    pub conversations_available: bool,
}

/// Answer one allowed `sona work` verb.
pub(super) async fn answer(
    verb: &WorkVerb,
    app: &AppHandle,
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
) -> Result<WorkAnswer, ExternalError> {
    let tracker = Tracker {
        app,
        meetings,
        history,
    };
    match verb {
        WorkVerb::List(args) => tracker.list(args).await,
        WorkVerb::Ready(args) => tracker.ready(args).await,
        WorkVerb::Mine(args) => tracker.mine(args).await,
        WorkVerb::Counts => tracker.counts().await,
        WorkVerb::Show(args) => tracker.show(args).await,
        WorkVerb::Create(args) => tracker.create(args).await,
        WorkVerb::Update(args) => tracker.update(args).await,
        WorkVerb::Claim(args) => tracker.claim(args).await,
        WorkVerb::Done(args) => tracker.done(args).await,
        WorkVerb::Accept(args) => {
            tracker
                .triage(
                    &args.tasks,
                    Decision::Accept {
                        apply_suggestions: !args.ignore_suggestions,
                    },
                    args.dry_run,
                    &args.write,
                )
                .await
        }
        WorkVerb::Decline(args) => {
            tracker
                .triage(&args.tasks, Decision::Decline, args.dry_run, &args.write)
                .await
        }
        WorkVerb::Duplicate(args) => tracker.duplicate(args).await,
        WorkVerb::Archive(args) => tracker.archive(args, true).await,
        WorkVerb::Restore(args) => tracker.archive(args, false).await,
        WorkVerb::Relate(args) => tracker.relate(args).await,
        WorkVerb::Unrelate(args) => tracker.unrelate(args).await,
        WorkVerb::Labels => tracker.labels(),
        WorkVerb::LabelSave(args) => tracker.label_save(args),
        WorkVerb::LabelDelete(args) => tracker.label_delete(args),
        WorkVerb::Views => tracker.views(),
        WorkVerb::ViewSave(args) => tracker.view_save(args).await,
        WorkVerb::ViewDelete(args) => tracker.view_delete(args),
        WorkVerb::Export(args) => tracker.export(args).await,
    }
}

#[derive(Clone, Copy)]
enum Decision {
    Accept { apply_suggestions: bool },
    Decline,
}

struct Tracker<'a> {
    app: &'a AppHandle,
    meetings: &'a Arc<MeetingSessionManager>,
    history: &'a HistoryManager,
}

/// Every task an outside caller may see, with the names to show beside them.
struct Board {
    rows: Vec<WorkItemRow>,
    labels: Vec<WorkLabel>,
    projects: Vec<WorkProjectChoice>,
    people: Vec<WorkPersonChoice>,
    suggestions: BTreeMap<String, Vec<WorkFieldSuggestion>>,
    fence: Fence,
}

impl Tracker<'_> {
    async fn board(&self) -> Result<Board, ExternalError> {
        let fence = Fence::read(self.meetings).await?;
        let list = self
            .listed(
                &fence,
                WorkQuery {
                    include_archived: true,
                    include_snoozed: true,
                    ..WorkQuery::default()
                },
            )
            .await?;
        Ok(Board {
            rows: list.items,
            labels: list.labels,
            projects: list.projects,
            people: list.people,
            suggestions: list.suggestions,
            fence,
        })
    }

    async fn listed(&self, fence: &Fence, query: WorkQuery) -> Result<WorkList, ExternalError> {
        let mut list = work::work_list(self.app.state(), self.app.state(), query)
            .await
            .map_err(|message| refusal(self.history, message))?;
        list.items.retain(|row| !fence.hides_item(&row.item));
        list.suggestions
            .retain(|id, _| list.items.iter().any(|row| row.item.id == *id));
        for suggestions in list.suggestions.values_mut() {
            suggestions.retain(|suggestion| !fence.hides_suggestion(suggestion));
        }
        Ok(list)
    }

    fn write_refusal(&self, message: String, operation_id: &str, subject: &str) -> ExternalError {
        match message.as_str() {
            "This item changed. Reload and try again." => ExternalError::invalid(format!(
                "{subject} changed while this ran. Read it again and retry."
            )),
            "This change was already used. Reload and try again." => ExternalError::invalid(
                format!(
                    "Operation id {operation_id} was already used for a different change. Pass a new --operation-id, or leave it out."
                ),
            ),
            _ => refusal(self.history, message),
        }
    }

    async fn list(&self, args: &ListArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let base = match &args.view {
            Some(view) => self.view_named(view)?.query,
            None => WorkQuery {
                statuses: Some(OPEN_AND_TRIAGE.to_vec()),
                ..WorkQuery::default()
            },
        };
        let query = args.filters.narrow(&board, base)?;
        let rows = self.listed(&board.fence, query).await?.items;
        Ok(WorkAnswer::Page(page(rows, args.limit, &board)))
    }

    async fn ready(&self, args: &ReadyArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let query = args.scope.narrow(
            &board,
            WorkQuery {
                statuses: Some(vec![WorkStatus::NotStarted]),
                ..WorkQuery::default()
            },
        )?;
        let mut rows = self.listed(&board.fence, query).await?.items;
        rows.retain(|row| row.blocked_by_open_count == 0);
        Ok(WorkAnswer::Page(page(rows, args.limit, &board)))
    }

    async fn mine(&self, args: &MineArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let now = now_ms();
        let counts = board.counts(now)?;
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
        let (i_owe, more_owed) = board.open_for(WorkDirection::Mine, now, limit);
        let (waiting_on, more_waiting) = board.open_for(WorkDirection::WaitingOn, now, limit);
        Ok(WorkAnswer::Mine(ExternalWorkMine {
            schema_version: QUERY_SCHEMA_VERSION,
            counts,
            i_owe,
            waiting_on,
            has_more: more_owed || more_waiting,
            conversations_available: board.fence.is_open(),
        }))
    }

    async fn counts(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        Ok(WorkAnswer::Counts(ExternalWorkCountsPage {
            schema_version: QUERY_SCHEMA_VERSION,
            counts: board.counts(now_ms())?,
            conversations_available: board.fence.is_open(),
        }))
    }

    async fn show(&self, args: &ShowArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let id = board.task(&args.task)?.item.id.clone();
        let detail = work::work_item(self.app.state(), self.app.state(), id)
            .await
            .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::Detail(Box::new(board.detail(detail))))
    }

    async fn create(&self, args: &CreateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let fields = &args.fields;
        let input = WorkItemInput {
            title: args.title.trim().to_owned(),
            details: fields
                .details
                .as_deref()
                .map(str::trim)
                .filter(|details| !details.is_empty())
                .map(str::to_owned),
            status: fields.status.map(StatusArg::status),
            priority: fields.priority.map(Priority::value),
            project_id: fields
                .project
                .as_deref()
                .map(|project| board.project(project))
                .transpose()?
                .flatten()
                .map(|project| project.id.clone()),
            milestone_id: fields
                .milestone
                .as_deref()
                .map(milestone)
                .transpose()?
                .flatten(),
            owner: fields
                .owner
                .as_deref()
                .map(|owner| board.owner(owner))
                .transpose()?,
            due_at_ms: fields.due.map(DayArg::ms).transpose()?.flatten(),
            label_ids: fields
                .labels
                .as_deref()
                .map(|labels| board.label_ids(labels))
                .transpose()?,
        };
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let operation_id = args.write.operation_id("create", &(&input, today))?;
        let started_ms = now_ms();
        let item = work::create_as(self.app, WorkActor::Agent, &operation_id, &input)
            .await
            .map_err(|message| self.write_refusal(message, &operation_id, "The new task"))?;
        let replayed = item.created_at_ms < started_ms;
        Ok(WorkAnswer::Change(ExternalWorkChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: WorkAction::Created,
            operation_id: Some(operation_id),
            tasks: vec![board.project_item(item)],
            skipped: Vec::new(),
            replayed,
        }))
    }

    async fn update(&self, args: &UpdateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let targets = board.tasks(&args.tasks)?;
        if let (Some(expected), [row]) = (args.expected_revision, targets.as_slice()) {
            if row.item.revision != expected {
                return Err(ExternalError::invalid(format!(
                    "{} is at revision {}, not {expected}: it changed since it was read. Read it again and retry.",
                    handle(row.item.number),
                    row.item.revision
                )));
            }
        }
        let base = board.patch(args)?;
        let add = board.label_ids(&args.add_label)?;
        let remove = board.label_ids(&args.remove_label)?;
        let planned = targets
            .into_iter()
            .map(|row| (row, relabeled(&base, row, &add, &remove)))
            .collect();
        self.apply(
            &board,
            WorkAction::Updated,
            args.dry_run,
            &args.write,
            planned,
        )
        .await
    }

    async fn claim(&self, args: &ClaimArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let name = handle(row.item.number);
        if row.item.archived_at_ms.is_some() {
            return Err(ExternalError::invalid(format!(
                "{name} is archived. Restore it first: sona work restore {name}"
            )));
        }
        match row.item.status {
            WorkStatus::Triage => Err(ExternalError::invalid(format!(
                "{name} is still in triage. Accept it first: sona work accept {name}"
            ))),
            WorkStatus::InProgress => Err(ExternalError::invalid(format!(
                "{name} is already in progress. Find another with: sona work ready"
            ))),
            WorkStatus::Done | WorkStatus::Canceled | WorkStatus::Duplicate => {
                Err(ExternalError::invalid(format!(
                    "{name} is {}; there is nothing to claim.",
                    status_text(row.item.status).to_lowercase()
                )))
            }
            WorkStatus::Backlog | WorkStatus::NotStarted => {
                let patch = WorkPatch {
                    status: Some(WorkStatus::InProgress),
                    ..WorkPatch::default()
                };
                let operation_id = args
                    .write
                    .operation_id("claim", &(&row.item.id, row.item.revision))?;
                let item = work::update_as(
                    self.app,
                    WorkActor::Agent,
                    &operation_id,
                    &row.item.id,
                    row.item.revision,
                    &patch,
                )
                .await
                .map_err(|message| self.write_refusal(message, &operation_id, &name))?;
                Ok(WorkAnswer::Change(ExternalWorkChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    action: WorkAction::Claimed,
                    operation_id: Some(operation_id),
                    tasks: vec![board.project_item(item)],
                    skipped: Vec::new(),
                    replayed: false,
                }))
            }
        }
    }

    async fn done(&self, args: &DoneArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let planned = board
            .tasks(&args.tasks)?
            .into_iter()
            .map(|row| {
                let patch = WorkPatch {
                    status: Some(WorkStatus::Done),
                    details: args
                        .evidence
                        .as_deref()
                        .and_then(|evidence| with_evidence(row.item.details.as_deref(), evidence)),
                    ..WorkPatch::default()
                };
                (row, patch)
            })
            .collect();
        self.apply(&board, WorkAction::Done, args.dry_run, &args.write, planned)
            .await
    }

    /// Writes one patch per task: fenced alone, all or none when the patches
    /// agree, and otherwise one task at a time, stopping at the first refusal.
    async fn apply(
        &self,
        board: &Board,
        action: WorkAction,
        dry_run: bool,
        write: &WriteArgs,
        planned: Vec<(&WorkItemRow, WorkPatch)>,
    ) -> Result<WorkAnswer, ExternalError> {
        let mut changing = Vec::new();
        let mut skipped = Vec::new();
        for (row, patch) in planned {
            if board.diff(row, &patch).is_empty() {
                skipped.push(skip(row, unchanged_reason(row, &patch)));
            } else {
                changing.push((row, patch));
            }
        }
        let operation_id = match changing.as_slice() {
            [] => None,
            [(row, patch)] => {
                Some(write.operation_id(action.verb(), &(&row.item.id, row.item.revision, patch))?)
            }
            several => Some(
                write.operation_id(
                    action.verb(),
                    &several
                        .iter()
                        .map(|(row, patch)| (&row.item.id, row.item.revision, patch))
                        .collect::<Vec<_>>(),
                )?,
            ),
        };
        if dry_run {
            let mut changes: Vec<ExternalWorkPlanned> = changing
                .iter()
                .map(|(row, patch)| board.planned(row, patch, None))
                .collect();
            changes.extend(skipped.into_iter().map(|skip| ExternalWorkPlanned {
                id: skip.id,
                handle: skip.handle,
                title: String::new(),
                fields: Vec::new(),
                commitment: None,
                skipped: Some(skip.reason),
            }));
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action,
                operation_id,
                changes,
            }));
        }
        let Some(operation_id) = operation_id else {
            return Ok(changed(action, None, Vec::new(), skipped));
        };
        let tasks = match changing.as_slice() {
            [(row, patch)] => {
                let item = work::update_as(
                    self.app,
                    WorkActor::Agent,
                    &operation_id,
                    &row.item.id,
                    row.item.revision,
                    patch,
                )
                .await
                .map_err(|message| {
                    self.write_refusal(message, &operation_id, &handle(row.item.number))
                })?;
                vec![board.project_item(item)]
            }
            several if several.iter().all(|(_, patch)| *patch == several[0].1) => {
                let ids: Vec<String> = several.iter().map(|(row, _)| row.item.id.clone()).collect();
                let items = work::bulk_update_as(
                    self.app,
                    WorkActor::Agent,
                    &operation_id,
                    &ids,
                    &several[0].1,
                )
                .await
                .map_err(|message| {
                    self.write_refusal(message, &operation_id, "One of these tasks")
                })?;
                items
                    .into_iter()
                    .map(|item| board.project_item(item))
                    .collect()
            }
            several => self.apply_each(board, &operation_id, several).await?,
        };
        Ok(changed(action, Some(operation_id), tasks, skipped))
    }

    async fn apply_each(
        &self,
        board: &Board,
        operation_id: &str,
        several: &[(&WorkItemRow, WorkPatch)],
    ) -> Result<Vec<ExternalWorkItem>, ExternalError> {
        let base = Uuid::parse_str(operation_id).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The change could not be named.")
        })?;
        let mut tasks = Vec::with_capacity(several.len());
        for (row, patch) in several {
            let task_operation = Uuid::new_v5(&base, row.item.id.as_bytes()).to_string();
            match work::update_as(
                self.app,
                WorkActor::Agent,
                &task_operation,
                &row.item.id,
                row.item.revision,
                patch,
            )
            .await
            {
                Ok(item) => tasks.push(board.project_item(item)),
                Err(message) => {
                    let refusal =
                        self.write_refusal(message, &task_operation, &handle(row.item.number));
                    let done: Vec<&str> = tasks.iter().map(|task| task.handle.as_str()).collect();
                    let kept = if done.is_empty() {
                        "Nothing was changed.".to_owned()
                    } else {
                        format!(
                            "Already changed: {}. The same command again changes only what is left.",
                            done.join(", ")
                        )
                    };
                    return Err(ExternalError {
                        message: format!("{} {kept}", refusal.message),
                        ..refusal
                    });
                }
            }
        }
        Ok(tasks)
    }

    async fn triage(
        &self,
        tasks: &[TaskRef],
        decision: Decision,
        dry_run: bool,
        write: &WriteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let action = match decision {
            Decision::Accept { .. } => WorkAction::Accepted,
            Decision::Decline => WorkAction::Declined,
        };
        let mut waiting = Vec::new();
        let mut skipped = Vec::new();
        for row in board.tasks(tasks)? {
            if row.item.status == WorkStatus::Triage && row.item.archived_at_ms.is_none() {
                waiting.push(row);
            } else if row.item.archived_at_ms.is_some() {
                skipped.push(skip(row, "archived".to_owned()));
            } else {
                skipped.push(skip(
                    row,
                    format!(
                        "not in triage; it is {}",
                        status_text(row.item.status).to_lowercase()
                    ),
                ));
            }
        }
        let revisions: Vec<(&str, i64)> = waiting
            .iter()
            .map(|row| (row.item.id.as_str(), row.item.revision))
            .collect();
        let operation_id = if waiting.is_empty() {
            None
        } else {
            Some(match decision {
                Decision::Accept { apply_suggestions } => {
                    write.operation_id("accept", &(&revisions, apply_suggestions))?
                }
                Decision::Decline => write.operation_id("decline", &revisions)?,
            })
        };
        if dry_run {
            let mut changes: Vec<ExternalWorkPlanned> = waiting
                .iter()
                .map(|row| {
                    let (patch, commitment) = match decision {
                        Decision::Accept { apply_suggestions } => {
                            (board.acceptance(row, apply_suggestions), None)
                        }
                        Decision::Decline => (
                            WorkPatch {
                                status: Some(WorkStatus::Canceled),
                                ..WorkPatch::default()
                            },
                            row.item
                                .source_loop_id
                                .is_some()
                                .then_some(CommitmentEffect::SetsAside),
                        ),
                    };
                    board.planned(row, &patch, Some(commitment))
                })
                .collect();
            changes.extend(skipped.into_iter().map(|skip| ExternalWorkPlanned {
                id: skip.id,
                handle: skip.handle,
                title: String::new(),
                fields: Vec::new(),
                commitment: None,
                skipped: Some(skip.reason),
            }));
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action,
                operation_id,
                changes,
            }));
        }
        let Some(operation_id) = operation_id else {
            return Ok(changed(action, None, Vec::new(), skipped));
        };
        let ids: Vec<String> = waiting.iter().map(|row| row.item.id.clone()).collect();
        let items = match decision {
            Decision::Accept { apply_suggestions } => {
                work::triage_accept_as(
                    self.app,
                    WorkActor::Agent,
                    &operation_id,
                    &ids,
                    apply_suggestions,
                )
                .await
            }
            Decision::Decline => {
                work::triage_decline_as(self.app, WorkActor::Agent, &operation_id, &ids).await
            }
        }
        .map_err(|message| self.write_refusal(message, &operation_id, "One of these items"))?;
        let tasks = items
            .into_iter()
            .map(|item| board.project_item(item))
            .collect();
        Ok(changed(action, Some(operation_id), tasks, skipped))
    }

    async fn duplicate(&self, args: &DuplicateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let of = board.task(&args.of)?;
        if row.item.id == of.item.id {
            return Err(ExternalError::invalid("A task cannot duplicate itself."));
        }
        if of.item.duplicate_of_id.as_deref() == Some(row.item.id.as_str()) {
            return Err(ExternalError::invalid(format!(
                "{} is already a duplicate of {}.",
                handle(of.item.number),
                handle(row.item.number)
            )));
        }
        let operation_id = args
            .write
            .operation_id("duplicate", &(&row.item.id, row.item.revision, &of.item.id))?;
        let item = work::mark_duplicate_as(
            self.app,
            WorkActor::Agent,
            &operation_id,
            &row.item.id,
            &of.item.id,
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, &handle(row.item.number)))?;
        Ok(changed(
            WorkAction::MarkedDuplicate,
            Some(operation_id),
            vec![board.project_item(item)],
            Vec::new(),
        ))
    }

    async fn archive(&self, args: &TasksArgs, archived: bool) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let action = if archived {
            WorkAction::Archived
        } else {
            WorkAction::Restored
        };
        let mut changing = Vec::new();
        let mut skipped = Vec::new();
        for row in board.tasks(&args.tasks)? {
            if row.item.archived_at_ms.is_some() == archived {
                let reason = if archived {
                    "already archived"
                } else {
                    "not archived"
                };
                skipped.push(skip(row, reason.to_owned()));
            } else {
                changing.push(row);
            }
        }
        let revisions: Vec<(&str, i64)> = changing
            .iter()
            .map(|row| (row.item.id.as_str(), row.item.revision))
            .collect();
        let operation_id = if changing.is_empty() {
            None
        } else {
            Some(args.write.operation_id(action.verb(), &revisions)?)
        };
        if args.dry_run {
            let (from, to) = if archived {
                ("no", "yes")
            } else {
                ("yes", "no")
            };
            let mut changes: Vec<ExternalWorkPlanned> = changing
                .iter()
                .map(|row| ExternalWorkPlanned {
                    id: row.item.id.clone(),
                    handle: handle(row.item.number),
                    title: row.item.title.clone(),
                    fields: vec![ExternalFieldChange {
                        field: "archived",
                        from: from.to_owned(),
                        to: to.to_owned(),
                    }],
                    commitment: None,
                    skipped: None,
                })
                .collect();
            changes.extend(skipped.into_iter().map(|skip| ExternalWorkPlanned {
                id: skip.id,
                handle: skip.handle,
                title: String::new(),
                fields: Vec::new(),
                commitment: None,
                skipped: Some(skip.reason),
            }));
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action,
                operation_id,
                changes,
            }));
        }
        let Some(operation_id) = operation_id else {
            return Ok(changed(action, None, Vec::new(), skipped));
        };
        let ids: Vec<String> = changing.iter().map(|row| row.item.id.clone()).collect();
        let items =
            work::set_archived_as(self.app, WorkActor::Agent, &operation_id, &ids, archived)
                .map_err(|message| {
                    self.write_refusal(message, &operation_id, "One of these tasks")
                })?;
        let tasks = items
            .into_iter()
            .map(|item| board.project_item(item))
            .collect();
        Ok(changed(action, Some(operation_id), tasks, skipped))
    }

    async fn relate(&self, args: &RelateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (from, to, kind) = board.link(args)?;
        let operation_id = args.write.fresh_operation_id();
        let relation = work::relation_add_as(
            self.app,
            WorkActor::Agent,
            &operation_id,
            &from.item.id,
            &to.item.id,
            kind,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The link"))?;
        Ok(WorkAnswer::Relation(ExternalWorkRelationChange {
            schema_version: QUERY_SCHEMA_VERSION,
            operation_id,
            removed: false,
            relation: relation_between(&relation, from, to),
        }))
    }

    async fn unrelate(&self, args: &RelateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (from, to, kind) = board.link(args)?;
        let relations =
            planning::history(self.history, |conn| items::relations(conn, &from.item.id))
                .map_err(|message| refusal(self.history, message))?;
        let found = relations
            .into_iter()
            .find(|relation| {
                relation.kind == kind
                    && ((relation.from_id == from.item.id && relation.to_id == to.item.id)
                        || (kind == WorkRelationKind::Related
                            && relation.from_id == to.item.id
                            && relation.to_id == from.item.id))
            })
            .ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    format!(
                        "{} and {} have no such link.",
                        handle(from.item.number),
                        handle(to.item.number)
                    ),
                )
            })?;
        let operation_id = args.write.operation_id("unrelate", &found.id)?;
        work::relation_remove_as(self.app, WorkActor::Agent, &operation_id, &found.id)
            .map_err(|message| self.write_refusal(message, &operation_id, "The link"))?;
        Ok(WorkAnswer::Relation(ExternalWorkRelationChange {
            schema_version: QUERY_SCHEMA_VERSION,
            operation_id,
            removed: true,
            relation: relation_between(&found, from, to),
        }))
    }

    fn all_labels(&self) -> Result<Vec<WorkLabel>, ExternalError> {
        work::work_labels(self.app.state()).map_err(|message| refusal(self.history, message))
    }

    fn all_views(&self) -> Result<Vec<WorkView>, ExternalError> {
        work::work_views(self.app.state()).map_err(|message| refusal(self.history, message))
    }

    fn view_named(&self, value: &str) -> Result<WorkView, ExternalError> {
        let views = self.all_views()?;
        pick("view", value, &views, |view| &view.id, |view| &view.name)
            .cloned()
            .map_err(|refusal| refusal.hint("List them with: sona work views"))
    }

    fn labels(&self) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::Labels(ExternalWorkLabels {
            schema_version: QUERY_SCHEMA_VERSION,
            labels: self.all_labels()?.into_iter().map(project_label).collect(),
        }))
    }

    fn label_save(&self, args: &LabelSaveArgs) -> Result<WorkAnswer, ExternalError> {
        let labels = self.all_labels()?;
        let name = args.name.trim();
        let existing = match &args.label {
            Some(label) => Some(
                pick(
                    "label",
                    label,
                    &labels,
                    |label| &label.id,
                    |label| &label.name,
                )
                .map_err(|refusal| refusal.hint("List them with: sona work labels"))?,
            ),
            None => labels
                .iter()
                .find(|label| label.name.to_lowercase() == name.to_lowercase()),
        };
        if let Some(taken) = labels.iter().find(|label| {
            label.name.to_lowercase() == name.to_lowercase()
                && existing.is_none_or(|existing| existing.id != label.id)
        }) {
            return Err(ExternalError::invalid(format!(
                "A label called {:?} already exists.",
                taken.name
            )));
        }
        let (id, hue, operation_id) = match existing {
            Some(label) => {
                let hue = args.hue.map_or(label.hue, f64::from);
                if label.name == name && label.hue.to_bits() == hue.to_bits() {
                    return Ok(WorkAnswer::Label(ExternalWorkLabelChange {
                        schema_version: QUERY_SCHEMA_VERSION,
                        operation_id: None,
                        deleted: false,
                        label: project_label(label.clone()),
                    }));
                }
                let operation_id = args
                    .write
                    .operation_id("label-save", &(&label.id, label.revision, name, hue))?;
                (Some(label.id.clone()), hue, operation_id)
            }
            None => (
                None,
                args.hue.map_or_else(|| hue_for(name), f64::from),
                args.write.fresh_operation_id(),
            ),
        };
        let label = work::work_label_save(
            self.app.clone(),
            operation_id.clone(),
            id,
            name.to_owned(),
            hue,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The label"))?;
        Ok(WorkAnswer::Label(ExternalWorkLabelChange {
            schema_version: QUERY_SCHEMA_VERSION,
            operation_id: Some(operation_id),
            deleted: false,
            label: project_label(label),
        }))
    }

    fn label_delete(&self, args: &LabelDeleteArgs) -> Result<WorkAnswer, ExternalError> {
        let labels = self.all_labels()?;
        let label = pick(
            "label",
            &args.label,
            &labels,
            |label| &label.id,
            |label| &label.name,
        )
        .map_err(|refusal| refusal.hint("List them with: sona work labels"))?
        .clone();
        let operation_id = args.write.operation_id("label-delete", &label.id)?;
        work::work_label_delete(self.app.clone(), operation_id.clone(), label.id.clone())
            .map_err(|message| self.write_refusal(message, &operation_id, "The label"))?;
        Ok(WorkAnswer::Label(ExternalWorkLabelChange {
            schema_version: QUERY_SCHEMA_VERSION,
            operation_id: Some(operation_id),
            deleted: true,
            label: project_label(label),
        }))
    }

    fn views(&self) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::Views(ExternalWorkViews {
            schema_version: QUERY_SCHEMA_VERSION,
            views: self.all_views()?.iter().map(project_view).collect(),
        }))
    }

    async fn view_save(&self, args: &ViewSaveArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let views = self.all_views()?;
        let name = args.name.trim();
        let existing = match &args.view {
            Some(view) => Some(
                pick("view", view, &views, |view| &view.id, |view| &view.name)
                    .map_err(|refusal| refusal.hint("List them with: sona work views"))?,
            ),
            None => views
                .iter()
                .find(|view| view.name.to_lowercase() == name.to_lowercase()),
        };
        let query = args.filters.narrow(
            &board,
            existing.map_or_else(WorkQuery::default, |view| view.query.clone()),
        )?;
        let mut display = existing.map_or_else(
            || WorkDisplay {
                layout: WorkLayout::List,
                group_by: WorkGroupBy::Status,
                order_by: WorkOrderBy::Priority,
                show_empty_groups: false,
                properties: VIEW_PROPERTIES.map(str::to_owned).to_vec(),
            },
            |view| view.display.clone(),
        );
        if let Some(layout) = args.layout {
            display.layout = match layout {
                LayoutArg::List => WorkLayout::List,
                LayoutArg::Board => WorkLayout::Board,
            };
        }
        if let Some(group) = args.group_by {
            display.group_by = match group {
                GroupArg::Status => WorkGroupBy::Status,
                GroupArg::Owner => WorkGroupBy::Owner,
                GroupArg::Project => WorkGroupBy::Project,
                GroupArg::Meeting => WorkGroupBy::Meeting,
                GroupArg::Due => WorkGroupBy::Due,
                GroupArg::Priority => WorkGroupBy::Priority,
                GroupArg::None => WorkGroupBy::None,
            };
        }
        if let Some(order) = args.order_by {
            display.order_by = match order {
                OrderArg::Priority => WorkOrderBy::Priority,
                OrderArg::Due => WorkOrderBy::Due,
                OrderArg::Updated => WorkOrderBy::Updated,
                OrderArg::Created => WorkOrderBy::Created,
                OrderArg::Manual => WorkOrderBy::Manual,
            };
        }
        let (id, operation_id) = match existing {
            Some(view) => (
                Some(view.id.clone()),
                args.write.operation_id(
                    "view-save",
                    &(&view.id, view.revision, name, &query, &display),
                )?,
            ),
            None => (None, args.write.fresh_operation_id()),
        };
        let view = work::work_view_save(
            self.app.clone(),
            operation_id.clone(),
            id,
            name.to_owned(),
            query,
            display,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The view"))?;
        Ok(WorkAnswer::View(Box::new(ExternalWorkViewChange {
            schema_version: QUERY_SCHEMA_VERSION,
            operation_id,
            deleted: false,
            view: project_view(&view),
        })))
    }

    fn view_delete(&self, args: &ViewDeleteArgs) -> Result<WorkAnswer, ExternalError> {
        let view = self.view_named(&args.view)?;
        let operation_id = args.write.operation_id("view-delete", &view.id)?;
        work::work_view_delete(self.app.clone(), operation_id.clone(), view.id.clone())
            .map_err(|message| self.write_refusal(message, &operation_id, "The view"))?;
        Ok(WorkAnswer::View(Box::new(ExternalWorkViewChange {
            schema_version: QUERY_SCHEMA_VERSION,
            operation_id,
            deleted: true,
            view: project_view(&view),
        })))
    }

    async fn export(&self, args: &ExportArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let query = args.filters.narrow(&board, WorkQuery::default())?;
        let mut rows = self.listed(&board.fence, query).await?.items;
        rows.sort_by_key(|row| row.item.number);
        std::fs::create_dir_all(&args.md).map_err(|error| {
            ExternalError::new(
                ExternalErrorCode::Failed,
                format!("{} could not be created: {error}", args.md.display()),
            )
        })?;
        let mut files = Vec::with_capacity(rows.len());
        for row in &rows {
            let task = project_task(row, &board.labels);
            let path = args.md.join(format!("{}.md", task.handle));
            std::fs::write(&path, markdown(&task)).map_err(|error| {
                ExternalError::new(
                    ExternalErrorCode::Failed,
                    format!("{} could not be written: {error}", path.display()),
                )
            })?;
            files.push(path.display().to_string());
        }
        Ok(WorkAnswer::Export(ExternalWorkExport {
            schema_version: QUERY_SCHEMA_VERSION,
            directory: args.md.display().to_string(),
            files,
            conversations_available: board.fence.is_open(),
        }))
    }
}

impl ExternalError {
    fn hint(self, hint: &str) -> Self {
        Self {
            message: format!("{} {hint}", self.message),
            ..self
        }
    }
}

/// A tracker refusal as an outside caller should branch on it.
fn refusal(history: &HistoryManager, message: String) -> ExternalError {
    let code = match message.as_str() {
        "This item no longer exists." => ExternalErrorCode::NotFound,
        planning::CONVERSATIONS_CLOSED
        | planning::PORTABLE_NO_CONVERSATIONS
        | "Planning is paused while recording." => ExternalErrorCode::Unavailable,
        _ if !history.storage_is_ready() => ExternalErrorCode::Unavailable,
        "Plans could not be saved or read. Try again." | "Conversations could not be read." => {
            ExternalErrorCode::Failed
        }
        _ => ExternalErrorCode::InvalidRequest,
    };
    ExternalError::new(code, message)
}

/// Which work from meetings stays out of an answer.
enum Fence {
    /// The conversations are open: work from meetings kept on this Mac.
    Open(KeptLocal),
    /// Nothing says which meetings are kept on this Mac, so all of it.
    Closed,
}

impl Fence {
    async fn read(meetings: &Arc<MeetingSessionManager>) -> Result<Self, ExternalError> {
        match store(meetings).await {
            Ok(store) => kept_local(&store).map(Self::Open),
            Err(_) => Ok(Self::Closed),
        }
    }

    const fn is_open(&self) -> bool {
        matches!(self, Self::Open(_))
    }

    fn hides_item(&self, item: &WorkItem) -> bool {
        let Some(loop_id) = &item.source_loop_id else {
            return false;
        };
        match self {
            Self::Open(kept_local) => MeetingLoopId(loop_id.clone())
                .session_id()
                .is_some_and(|session_id| kept_local.hides(session_id)),
            Self::Closed => true,
        }
    }

    fn hides_suggestion(&self, suggestion: &WorkFieldSuggestion) -> bool {
        suggestion
            .meeting_id
            .as_deref()
            .is_some_and(|meeting_id| match self {
                Self::Open(kept_local) => kept_local.hides_meeting(meeting_id),
                Self::Closed => true,
            })
    }

    fn hides_evidence(&self, evidence: &planning::EvidenceLink) -> bool {
        match self {
            Self::Open(kept_local) => evidence_kept_local(evidence, kept_local),
            Self::Closed => match evidence.source_kind {
                planning::EvidenceSourceKind::Meeting | planning::EvidenceSourceKind::Loop => true,
                planning::EvidenceSourceKind::Note => false,
            },
        }
    }
}

/// The refusal for a request that needs the people or meetings in the
/// conversations while those can't be opened.
fn conversations_closed() -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::Unavailable,
        if crate::portable::is_portable() {
            planning::PORTABLE_NO_CONVERSATIONS
        } else {
            planning::CONVERSATIONS_CLOSED
        },
    )
}

/// What to tell a caller whose answer lacks work from meetings. Portable mode
/// keeps no conversations, so there it lacks nothing.
fn left_out(conversations_available: bool) -> Option<&'static str> {
    (!conversations_available && !crate::portable::is_portable()).then_some(
        "Work and quotes from meetings are left out while conversations can't be opened.",
    )
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn handle(number: i64) -> String {
    format!("S-{number}")
}

/// Priority first with none last, then the soonest due, then the oldest.
fn urgency(row: &WorkItemRow) -> (u8, i64, i64) {
    let priority = match row.item.priority {
        0 => u8::MAX,
        priority => priority,
    };
    (
        priority,
        row.item.due_at_ms.unwrap_or(i64::MAX),
        row.item.number,
    )
}

fn page(mut rows: Vec<WorkItemRow>, limit: Option<usize>, board: &Board) -> ExternalWorkPage {
    rows.sort_by_key(urgency);
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    let total = rows.len();
    ExternalWorkPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: rows
            .iter()
            .take(limit)
            .map(|row| project_task(row, &board.labels))
            .collect(),
        total,
        has_more: total > limit,
        conversations_available: board.fence.is_open(),
    }
}

fn changed(
    action: WorkAction,
    operation_id: Option<String>,
    tasks: Vec<ExternalWorkItem>,
    skipped: Vec<ExternalWorkSkip>,
) -> WorkAnswer {
    WorkAnswer::Change(ExternalWorkChange {
        schema_version: QUERY_SCHEMA_VERSION,
        action,
        operation_id,
        tasks,
        skipped,
        replayed: false,
    })
}

fn skip(row: &WorkItemRow, reason: String) -> ExternalWorkSkip {
    ExternalWorkSkip {
        id: row.item.id.clone(),
        handle: handle(row.item.number),
        reason,
    }
}

fn unchanged_reason(row: &WorkItemRow, patch: &WorkPatch) -> String {
    match patch.status {
        Some(status) if status == row.item.status => {
            format!("already {}", status_text(status).to_lowercase())
        }
        _ => "nothing to change".to_owned(),
    }
}

/// The details with one more line, unless that line is already there.
fn with_evidence(details: Option<&str>, evidence: &str) -> Option<Option<String>> {
    let line = format!("Evidence: {}", evidence.trim());
    match details.map(str::trim).filter(|details| !details.is_empty()) {
        Some(details) if details.lines().any(|existing| existing.trim() == line) => None,
        Some(details) => Some(Some(format!("{details}\n\n{line}"))),
        None => Some(Some(line)),
    }
}

/// The patch with this task's own labels added and removed.
fn relabeled(patch: &WorkPatch, row: &WorkItemRow, add: &[String], remove: &[String]) -> WorkPatch {
    if add.is_empty() && remove.is_empty() {
        return patch.clone();
    }
    let mut label_ids = row.item.label_ids.clone();
    for id in add {
        if !label_ids.contains(id) {
            label_ids.push(id.clone());
        }
    }
    label_ids.retain(|id| !remove.contains(id));
    WorkPatch {
        label_ids: Some(label_ids),
        ..patch.clone()
    }
}

fn milestone(value: &str) -> Result<Option<String>, ExternalError> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    Uuid::parse_str(value)
        .map(|id| Some(id.to_string()))
        .map_err(|_| {
            ExternalError::invalid(format!(
                "{value:?} is not a milestone id. Read a project's milestones with: sona --project PROJECT_ID"
            ))
        })
}

/// Finds one choice by id, then by its exact name, then by a part of its
/// name that only one choice has.
fn pick<'a, T>(
    kind: &str,
    value: &str,
    choices: &'a [T],
    id: impl Fn(&T) -> &String,
    name: impl Fn(&T) -> &String,
) -> Result<&'a T, ExternalError> {
    let wanted = value.trim();
    if let Some(choice) = choices
        .iter()
        .find(|choice| id(choice).eq_ignore_ascii_case(wanted))
    {
        return Ok(choice);
    }
    let lower = wanted.to_lowercase();
    let exact: Vec<&T> = choices
        .iter()
        .filter(|choice| name(choice).to_lowercase() == lower)
        .collect();
    let matches = if exact.is_empty() && !lower.is_empty() {
        choices
            .iter()
            .filter(|choice| name(choice).to_lowercase().contains(&lower))
            .collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [choice] => Ok(*choice),
        [] => Err(ExternalError::invalid(format!(
            "No {kind} is called {wanted:?}."
        ))),
        several => Err(ExternalError::invalid(format!(
            "{wanted:?} matches {} {kind}s: {}. Use the full name or the id.",
            several.len(),
            several
                .iter()
                .take(8)
                .map(|choice| format!("{} ({})", name(choice), id(choice)))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn hue_for(name: &str) -> f64 {
    let hash = name.to_lowercase().bytes().fold(0_u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    f64::from(hash % 360)
}

impl Board {
    fn row(&self, id: &str) -> Option<&WorkItemRow> {
        self.rows.iter().find(|row| row.item.id == id)
    }

    fn task(&self, task: &TaskRef) -> Result<&WorkItemRow, ExternalError> {
        let found = match task {
            TaskRef::Number(number) => self.rows.iter().find(|row| row.item.number == *number),
            TaskRef::Id(id) => self.row(id),
        };
        found.ok_or_else(|| {
            let missing =
                ExternalError::new(ExternalErrorCode::NotFound, format!("No task {task}."));
            match left_out(self.fence.is_open()) {
                Some(note) => missing.hint(note),
                None => missing,
            }
        })
    }

    fn tasks(&self, tasks: &[TaskRef]) -> Result<Vec<&WorkItemRow>, ExternalError> {
        let mut found: Vec<&WorkItemRow> = Vec::with_capacity(tasks.len());
        for task in tasks {
            let row = self.task(task)?;
            if !found.iter().any(|seen| seen.item.id == row.item.id) {
                found.push(row);
            }
        }
        Ok(found)
    }

    fn person(&self, value: &str) -> Result<&WorkPersonChoice, ExternalError> {
        if !self.fence.is_open() {
            return Err(conversations_closed());
        }
        pick(
            "person",
            value,
            &self.people,
            |person| &person.id,
            |person| &person.name,
        )
        .map_err(|refusal| refusal.hint("Look people up with: sona --people NAME"))
    }

    fn owner(&self, value: &str) -> Result<WorkOwner, ExternalError> {
        match value.trim().to_lowercase().as_str() {
            "me" | "self" | "you" | "mine" => Ok(WorkOwner::Myself),
            "nobody" | "none" | "no one" => Ok(WorkOwner::Nobody),
            _ => self.person(value).map(|person| WorkOwner::Person {
                person_id: person.id.clone(),
            }),
        }
    }

    /// `None` for "none": work in no project.
    fn project(&self, value: &str) -> Result<Option<&WorkProjectChoice>, ExternalError> {
        if value.trim().eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        pick(
            "project",
            value,
            &self.projects,
            |project| &project.id,
            |project| &project.title,
        )
        .map(Some)
        .map_err(|refusal| refusal.hint("List them with: sona --projects"))
    }

    fn label_ids(&self, names: &[String]) -> Result<Vec<String>, ExternalError> {
        let mut ids = Vec::with_capacity(names.len());
        for name in names.iter().filter(|name| !name.trim().is_empty()) {
            let label = pick(
                "label",
                name,
                &self.labels,
                |label| &label.id,
                |label| &label.name,
            )
            .map_err(|refusal| {
                refusal.hint(
                    "List them with: sona work labels; make one with: sona work label-save NAME",
                )
            })?;
            if !ids.contains(&label.id) {
                ids.push(label.id.clone());
            }
        }
        Ok(ids)
    }

    fn label_name(&self, id: &str) -> String {
        self.labels
            .iter()
            .find(|label| label.id == id)
            .map_or_else(|| id.to_owned(), |label| label.name.clone())
    }

    fn project_name(&self, id: &str) -> String {
        self.projects
            .iter()
            .find(|project| project.id == id)
            .map_or_else(|| id.to_owned(), |project| project.title.clone())
    }

    fn person_name(&self, id: &str) -> String {
        self.people
            .iter()
            .find(|person| person.id == id)
            .map_or_else(|| id.to_owned(), |person| person.name.clone())
    }

    fn owner_text(&self, owner: &WorkOwner) -> String {
        match owner {
            WorkOwner::Myself => "you".to_owned(),
            WorkOwner::Person { person_id } => self.person_name(person_id),
            WorkOwner::Nobody => "nobody".to_owned(),
        }
    }

    fn task_text(&self, id: &str) -> String {
        self.row(id).map_or_else(
            || id.to_owned(),
            |row| format!("{} {}", handle(row.item.number), row.item.title),
        )
    }

    fn counts(&self, now: i64) -> Result<ExternalWorkCounts, ExternalError> {
        let rows = self.rows.iter().map(|row| (&row.item, row.follow_up_at_ms));
        let counts = items::counts(rows, now).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The counts could not be read.")
        })?;
        Ok(ExternalWorkCounts {
            triage: counts.triage,
            mine_open: counts.mine_open,
            mine_overdue: counts.mine_overdue,
            mine_due_today: counts.mine_due_today,
            waiting_on: counts.waiting_on,
            waiting_on_stale: counts.waiting_on_stale,
        })
    }

    /// Open work on one side, as the counts count it.
    fn open_for(
        &self,
        direction: WorkDirection,
        now: i64,
        limit: usize,
    ) -> (Vec<ExternalWorkItem>, bool) {
        let mut rows: Vec<&WorkItemRow> = self
            .rows
            .iter()
            .filter(|row| {
                row.item.direction == direction
                    && row.item.status.is_open()
                    && row.item.archived_at_ms.is_none()
                    && !items::snoozed(&row.item, now)
            })
            .collect();
        rows.sort_by_key(|row| urgency(row));
        let more = rows.len() > limit;
        let tasks = rows
            .into_iter()
            .take(limit)
            .map(|row| project_task(row, &self.labels))
            .collect();
        (tasks, more)
    }

    fn patch(&self, args: &UpdateArgs) -> Result<WorkPatch, ExternalError> {
        let fields = &args.fields;
        Ok(WorkPatch {
            title: args.title.as_deref().map(|title| title.trim().to_owned()),
            details: fields.details.as_deref().map(|details| {
                Some(details.trim())
                    .filter(|details| !details.is_empty())
                    .map(str::to_owned)
            }),
            status: fields.status.map(StatusArg::status),
            priority: fields.priority.map(Priority::value),
            project_id: fields
                .project
                .as_deref()
                .map(|project| {
                    self.project(project)
                        .map(|project| project.map(|project| project.id.clone()))
                })
                .transpose()?,
            milestone_id: fields.milestone.as_deref().map(milestone).transpose()?,
            owner: fields
                .owner
                .as_deref()
                .map(|owner| self.owner(owner))
                .transpose()?,
            due_at_ms: fields.due.map(DayArg::ms).transpose()?,
            label_ids: fields
                .labels
                .as_deref()
                .map(|labels| self.label_ids(labels))
                .transpose()?,
            snoozed_until_ms: args.snooze_until.map(DayArg::ms).transpose()?,
        })
    }

    /// What accepting this item sets, from the suggestions Sona shows.
    fn acceptance(&self, row: &WorkItemRow, apply_suggestions: bool) -> WorkPatch {
        let mut patch = WorkPatch {
            status: Some(WorkStatus::NotStarted),
            ..WorkPatch::default()
        };
        if !apply_suggestions {
            return patch;
        }
        for suggestion in self.suggestions.get(&row.item.id).into_iter().flatten() {
            match (&suggestion.field, &suggestion.value) {
                (WorkSuggestionField::Owner, WorkSuggestionValue::Owner(owner)) => {
                    patch.owner = Some(owner.clone());
                }
                (WorkSuggestionField::Due, WorkSuggestionValue::Due(due_at_ms)) => {
                    patch.due_at_ms = Some(Some(*due_at_ms));
                }
                (WorkSuggestionField::Project, WorkSuggestionValue::Id(project_id)) => {
                    patch.project_id = Some(Some(project_id.clone()));
                }
                _ => {}
            }
        }
        patch
    }

    /// The fields this patch would change on this task, as a person reads
    /// them. Empty when the patch would write nothing.
    fn diff(&self, row: &WorkItemRow, patch: &WorkPatch) -> Vec<ExternalFieldChange> {
        let item = &row.item;
        let mut changes = Vec::new();
        let mut field = |field: &'static str, from: String, to: String| {
            if from != to {
                changes.push(ExternalFieldChange { field, from, to });
            }
        };
        if let Some(title) = &patch.title {
            field("title", item.title.clone(), title.trim().to_owned());
        }
        if let Some(details) = &patch.details {
            field(
                "details",
                item.details
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
                details.as_deref().unwrap_or_default().trim().to_owned(),
            );
        }
        if let Some(status) = patch.status {
            field(
                "status",
                status_text(item.status).to_owned(),
                status_text(status).to_owned(),
            );
        }
        if let Some(priority) = patch.priority {
            field(
                "priority",
                Priority::of(item.priority).text().to_owned(),
                Priority::of(priority).text().to_owned(),
            );
        }
        if let Some(owner) = &patch.owner {
            field(
                "owner",
                self.owner_text(&item.owner()),
                self.owner_text(owner),
            );
        }
        if let Some(due) = patch.due_at_ms {
            field("due", day_text(item.due_at_ms), day_text(due));
        }
        if let Some(project) = &patch.project_id {
            let name = |id: Option<&String>| {
                id.map_or_else(|| "none".to_owned(), |id| self.project_name(id))
            };
            field(
                "project",
                name(item.project_id.as_ref()),
                name(project.as_ref()),
            );
        }
        if let Some(milestone) = &patch.milestone_id {
            field(
                "milestone",
                item.milestone_id
                    .clone()
                    .unwrap_or_else(|| "none".to_owned()),
                milestone.clone().unwrap_or_else(|| "none".to_owned()),
            );
        }
        if let Some(label_ids) = &patch.label_ids {
            field(
                "labels",
                self.labels_text(&item.label_ids),
                self.labels_text(label_ids),
            );
        }
        if let Some(snoozed) = patch.snoozed_until_ms {
            field(
                "snoozed until",
                day_text(item.snoozed_until_ms),
                day_text(snoozed),
            );
        }
        changes
    }

    fn labels_text(&self, ids: &[String]) -> String {
        let mut names: Vec<String> = ids.iter().map(|id| self.label_name(id)).collect();
        names.sort();
        names.dedup();
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    }

    /// One task's dry-run line. `commitment` overrides what the status change
    /// alone would do to the meeting commitment behind it.
    fn planned(
        &self,
        row: &WorkItemRow,
        patch: &WorkPatch,
        commitment: Option<Option<CommitmentEffect>>,
    ) -> ExternalWorkPlanned {
        let commitment = commitment.unwrap_or_else(|| {
            let from_meeting = self.rows.iter().any(|member| {
                (member.item.id == row.item.id
                    || member.item.duplicate_of_id.as_deref() == Some(row.item.id.as_str()))
                    && member.item.source_loop_id.is_some()
            });
            patch
                .status
                .filter(|_| from_meeting)
                .and_then(|status| commitment_effect(row.item.status, status))
        });
        ExternalWorkPlanned {
            id: row.item.id.clone(),
            handle: handle(row.item.number),
            title: row.item.title.clone(),
            fields: self.diff(row, patch),
            commitment,
            skipped: None,
        }
    }

    /// Picks the two ends of a link in the order the tracker stores them.
    fn link(
        &self,
        args: &RelateArgs,
    ) -> Result<(&WorkItemRow, &WorkItemRow, WorkRelationKind), ExternalError> {
        let task = self.task(&args.task)?;
        let other = self.task(&args.other)?;
        if task.item.id == other.item.id {
            return Err(ExternalError::invalid("A task cannot link to itself."));
        }
        Ok(match args.link {
            LinkKind::Blocks => (task, other, WorkRelationKind::Blocks),
            LinkKind::BlockedBy => (other, task, WorkRelationKind::Blocks),
            LinkKind::Related => (task, other, WorkRelationKind::Related),
        })
    }

    /// The row the write returned, with the names this board read.
    fn project_item(&self, item: WorkItem) -> ExternalWorkItem {
        let before = self.row(&item.id);
        let row = WorkItemRow {
            project_title: item.project_id.as_deref().map(|id| self.project_name(id)),
            owner_name: item
                .owner_person_id
                .as_deref()
                .map(|id| self.person_name(id)),
            source_meeting_id: before.and_then(|row| row.source_meeting_id.clone()),
            source_meeting_title: before.and_then(|row| row.source_meeting_title.clone()),
            source_quote: before.and_then(|row| row.source_quote.clone()),
            source_speaker: before.and_then(|row| row.source_speaker.clone()),
            source_at_ms: before.and_then(|row| row.source_at_ms),
            blocked_by_open_count: before.map_or(0, |row| row.blocked_by_open_count),
            raised_in: before.map_or(0, |row| row.raised_in),
            source: before.map_or(WorkSource::Agent, |row| row.source),
            follow_up_at_ms: before.and_then(|row| row.follow_up_at_ms),
            item,
        };
        project_task(&row, &self.labels)
    }

    fn detail(&self, detail: WorkItemDetail) -> ExternalWorkDetail {
        let id = detail.item.item.id.clone();
        let relations = detail
            .relations
            .iter()
            .filter_map(|relation| {
                let (other, kind) = if relation.from_id == id {
                    (
                        &relation.to_id,
                        match relation.kind {
                            WorkRelationKind::Blocks => ExternalLinkKind::Blocks,
                            WorkRelationKind::Related => ExternalLinkKind::Related,
                        },
                    )
                } else {
                    (
                        &relation.from_id,
                        match relation.kind {
                            WorkRelationKind::Blocks => ExternalLinkKind::BlockedBy,
                            WorkRelationKind::Related => ExternalLinkKind::Related,
                        },
                    )
                };
                self.row(other).map(|row| ExternalWorkLink {
                    id: relation.id.clone(),
                    kind,
                    task: task_ref_of(row),
                })
            })
            .collect();
        let evidence = detail
            .evidence
            .into_iter()
            .filter(|evidence| !self.fence.hides_evidence(evidence))
            .map(project_evidence)
            .collect();
        let suggestions = detail
            .suggestions
            .iter()
            .filter(|suggestion| !self.fence.hides_suggestion(suggestion))
            .map(|suggestion| self.suggestion(suggestion))
            .collect();
        let activity = detail
            .activity
            .iter()
            .map(|entry| self.activity(entry))
            .collect();
        ExternalWorkDetail {
            schema_version: QUERY_SCHEMA_VERSION,
            conversations_available: self.fence.is_open(),
            task: project_task(&detail.item, &self.labels),
            relations,
            evidence,
            suggestions,
            activity,
        }
    }

    fn suggestion(&self, suggestion: &WorkFieldSuggestion) -> ExternalWorkSuggestion {
        let (value, id, due_at_ms) = match (&suggestion.field, &suggestion.value) {
            (_, WorkSuggestionValue::Owner(owner)) => (
                self.owner_text(owner),
                match owner {
                    WorkOwner::Person { person_id } => Some(person_id.clone()),
                    WorkOwner::Myself | WorkOwner::Nobody => None,
                },
                None,
            ),
            (_, WorkSuggestionValue::Due(due_at_ms)) => {
                (day_text(Some(*due_at_ms)), None, Some(*due_at_ms))
            }
            (WorkSuggestionField::Project, WorkSuggestionValue::Id(id)) => {
                (self.project_name(id), Some(id.clone()), None)
            }
            (_, WorkSuggestionValue::Id(id)) => (self.task_text(id), Some(id.clone()), None),
        };
        ExternalWorkSuggestion {
            field: suggestion.field,
            value,
            id,
            due_at_ms,
            reason: suggestion.reason.clone(),
            quote: suggestion.quote.clone(),
            meeting_id: suggestion.meeting_id.clone(),
            at_ms: suggestion.at_ms,
        }
    }

    /// One history entry with ids and stored values read as names and days.
    fn activity(&self, entry: &WorkActivity) -> ExternalWorkActivity {
        let read = |value: &Option<String>| {
            value.as_deref().map(|value| match entry.kind {
                WorkActivityKind::Status
                | WorkActivityKind::Created
                | WorkActivityKind::Accepted
                | WorkActivityKind::Declined => status_named(value)
                    .map_or_else(|| value.to_owned(), |status| status_text(status).to_owned()),
                WorkActivityKind::Duplicate => status_named(value).map_or_else(
                    || self.task_text(value),
                    |status| status_text(status).to_owned(),
                ),
                WorkActivityKind::Priority => value.parse::<u8>().map_or_else(
                    |_| value.to_owned(),
                    |priority| Priority::of(priority).text().to_owned(),
                ),
                WorkActivityKind::Owner => match value {
                    "self" => "you".to_owned(),
                    "nobody" => "nobody".to_owned(),
                    person_id => self.person_name(person_id),
                },
                WorkActivityKind::Due | WorkActivityKind::Snoozed => value
                    .parse::<i64>()
                    .map_or_else(|_| value.to_owned(), |ms| day_text(Some(ms))),
                WorkActivityKind::Project => self.project_name(value),
                WorkActivityKind::Labels => {
                    let ids: Vec<String> = value
                        .split(',')
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned)
                        .collect();
                    self.labels_text(&ids)
                }
                WorkActivityKind::Relation => match value.split_once(':') {
                    Some((kind, id)) => {
                        format!("{} {}", kind.replace('_', " "), self.task_text(id))
                    }
                    None => value.to_owned(),
                },
                WorkActivityKind::Title
                | WorkActivityKind::Details
                | WorkActivityKind::Archived
                | WorkActivityKind::Restored
                | WorkActivityKind::Kept => value.to_owned(),
            })
        };
        ExternalWorkActivity {
            at_ms: entry.at_ms,
            actor: entry.actor,
            kind: entry.kind,
            from: read(&entry.from),
            to: read(&entry.to),
        }
    }
}

/// A link between two tasks the caller named, ends in the stored order.
fn relation_between(
    relation: &WorkRelation,
    one: &WorkItemRow,
    other: &WorkItemRow,
) -> ExternalWorkRelation {
    let end = |id: &str| task_ref_of(if one.item.id == id { one } else { other });
    ExternalWorkRelation {
        id: relation.id.clone(),
        kind: relation.kind,
        from: end(&relation.from_id),
        to: end(&relation.to_id),
    }
}

fn project_view(view: &WorkView) -> ExternalWorkView {
    let query = &view.query;
    ExternalWorkView {
        id: view.id.clone(),
        name: view.name.clone(),
        filters: ExternalWorkFilters {
            statuses: query.statuses.clone(),
            direction: query.direction,
            owner_person_ids: query.owner_person_ids.clone(),
            project_ids: query.project_ids.clone(),
            label_ids: query.label_ids.clone(),
            priorities: query
                .priorities
                .as_ref()
                .map(|priorities| priorities.iter().copied().map(Priority::of).collect()),
            due_before_ms: query.due_before_ms,
            meeting_id: query.source_meeting_id.clone(),
            search: query.search.clone(),
            include_archived: query.include_archived,
            include_snoozed: query.include_snoozed,
        },
        layout: view.display.layout,
        group_by: view.display.group_by,
        order_by: view.display.order_by,
        revision: view.revision,
    }
}

impl Scope {
    fn narrow(&self, board: &Board, mut query: WorkQuery) -> Result<WorkQuery, ExternalError> {
        if self.mine {
            query.direction = Some(WorkDirection::Mine);
        }
        if self.waiting {
            query.direction = Some(WorkDirection::WaitingOn);
        }
        if self.unowned {
            query.direction = Some(WorkDirection::Unattributed);
        }
        if !self.owner.is_empty() {
            let mut people = Vec::new();
            let mut sides = Vec::new();
            for owner in &self.owner {
                match board.owner(owner)? {
                    WorkOwner::Myself => sides.push(WorkDirection::Mine),
                    WorkOwner::Nobody => sides.push(WorkDirection::Unattributed),
                    WorkOwner::Person { person_id } => people.push(person_id),
                }
            }
            match (sides.as_slice(), people.is_empty()) {
                ([], _) => query.owner_person_ids = Some(people),
                ([side], true) if !(self.mine || self.waiting || self.unowned) => {
                    query.direction = Some(*side);
                }
                _ => {
                    return Err(ExternalError::invalid(
                        "--owner me or nobody goes on its own: not with other owners, or with --mine, --waiting or --unowned.",
                    ))
                }
            }
        }
        if !self.project.is_empty() {
            let mut ids = Vec::with_capacity(self.project.len());
            for project in &self.project {
                ids.push(
                    board
                        .project(project)?
                        .map_or_else(|| "none".to_owned(), |project| project.id.clone()),
                );
            }
            query.project_ids = Some(ids);
        }
        if !self.label.is_empty() {
            query.label_ids = Some(board.label_ids(&self.label)?);
        }
        if !self.priority.is_empty() {
            query.priorities = Some(
                self.priority
                    .iter()
                    .map(|priority| priority.value())
                    .collect(),
            );
        }
        if let Some(day) = self.due_by {
            let next = day
                .succ_opt()
                .ok_or_else(|| ExternalError::invalid("--due-by is past the calendar."))?;
            query.due_before_ms = Some(midnight(next)?);
        }
        if let Some(meeting) = self.meeting {
            match &board.fence {
                Fence::Closed => return Err(conversations_closed()),
                Fence::Open(kept_local)
                    if kept_local.hides(MeetingSessionId::from_uuid(meeting)) =>
                {
                    return Err(ExternalError::new(
                        ExternalErrorCode::NotFound,
                        format!("No meeting {meeting}."),
                    ));
                }
                Fence::Open(_) => {}
            }
            query.source_meeting_id = Some(meeting.to_string());
        }
        if let Some(search) = &self.search {
            query.search = Some(match task_ref(search) {
                Ok(TaskRef::Number(number)) => format!("#{number}"),
                _ => search.clone(),
            });
        }
        Ok(query)
    }
}

impl Filters {
    fn narrow(&self, board: &Board, mut query: WorkQuery) -> Result<WorkQuery, ExternalError> {
        if !self.status.is_empty() {
            let mut statuses: Vec<WorkStatus> = Vec::new();
            for status in self.status.iter().flat_map(|filter| filter.statuses()) {
                if !statuses.contains(status) {
                    statuses.push(*status);
                }
            }
            query.statuses = Some(statuses);
        }
        if self.archived {
            query.include_archived = true;
        }
        if self.snoozed {
            query.include_snoozed = true;
        }
        self.scope.narrow(board, query)
    }
}

fn commitment_effect(from: WorkStatus, to: WorkStatus) -> Option<CommitmentEffect> {
    match (from.is_closed(), to) {
        (_, WorkStatus::Done) if from != WorkStatus::Done => Some(CommitmentEffect::Closes),
        (_, WorkStatus::Canceled) if from != WorkStatus::Canceled => {
            Some(CommitmentEffect::SetsAside)
        }
        (
            true,
            WorkStatus::Triage
            | WorkStatus::Backlog
            | WorkStatus::NotStarted
            | WorkStatus::InProgress,
        ) => Some(CommitmentEffect::Reopens),
        _ => None,
    }
}

fn status_named(value: &str) -> Option<WorkStatus> {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
}

const fn status_text(status: WorkStatus) -> &'static str {
    match status {
        WorkStatus::Triage => "Triage",
        WorkStatus::Backlog => "Backlog",
        WorkStatus::NotStarted => "Todo",
        WorkStatus::InProgress => "In progress",
        WorkStatus::Done => "Done",
        WorkStatus::Canceled => "Canceled",
        WorkStatus::Duplicate => "Duplicate",
    }
}

fn iso_day(utc_ms: i64) -> Option<String> {
    local_day(utc_ms).map(|day| day.format("%Y-%m-%d").to_string())
}

fn day_text(utc_ms: Option<i64>) -> String {
    utc_ms
        .and_then(iso_day)
        .unwrap_or_else(|| "none".to_owned())
}

fn task_ref_of(row: &WorkItemRow) -> ExternalWorkRef {
    ExternalWorkRef {
        id: row.item.id.clone(),
        handle: handle(row.item.number),
        title: row.item.title.clone(),
        status: row.item.status,
    }
}

fn project_label(label: WorkLabel) -> ExternalWorkLabel {
    ExternalWorkLabel {
        id: label.id,
        name: label.name,
        hue: label.hue,
        revision: label.revision,
    }
}

fn project_task(row: &WorkItemRow, labels: &[WorkLabel]) -> ExternalWorkItem {
    let item = &row.item;
    ExternalWorkItem {
        id: item.id.clone(),
        handle: handle(item.number),
        number: item.number,
        title: item.title.clone(),
        details: item.details.clone(),
        status: item.status,
        priority: Priority::of(item.priority),
        owner: match item.owner() {
            WorkOwner::Myself => ExternalWorkOwner::Myself,
            WorkOwner::Person { person_id } => ExternalWorkOwner::Person {
                person_id,
                name: row.owner_name.clone(),
            },
            WorkOwner::Nobody => ExternalWorkOwner::Nobody,
        },
        direction: item.direction,
        due_date: item.due_at_ms.and_then(iso_day),
        due_at_ms: item.due_at_ms,
        project: item.project_id.as_ref().map(|id| ExternalWorkName {
            id: id.clone(),
            name: row.project_title.clone().unwrap_or_else(|| id.clone()),
        }),
        milestone_id: item.milestone_id.clone(),
        labels: item
            .label_ids
            .iter()
            .map(|id| ExternalWorkName {
                id: id.clone(),
                name: labels
                    .iter()
                    .find(|label| label.id == *id)
                    .map_or_else(|| id.clone(), |label| label.name.clone()),
            })
            .collect(),
        source: item
            .source_loop_id
            .as_ref()
            .map(|loop_id| ExternalWorkSource {
                loop_id: loop_id.clone(),
                link: loop_link(&MeetingLoopId(loop_id.clone())),
                meeting_id: row.source_meeting_id.clone(),
                meeting_title: row.source_meeting_title.clone(),
                quote: row.source_quote.clone(),
                speaker: row.source_speaker.clone(),
                at_ms: row.source_at_ms,
            }),
        duplicate_of_id: item.duplicate_of_id.clone(),
        blocked_by_open: row.blocked_by_open_count,
        snoozed_until_ms: item.snoozed_until_ms,
        archived_at_ms: item.archived_at_ms,
        status_changed_at_ms: item.status_changed_at_ms,
        created_at_ms: item.created_at_ms,
        updated_at_ms: item.updated_at_ms,
        revision: item.revision,
    }
}

/// One task as a Markdown file: YAML front matter, then the title, details
/// and the words it came from.
fn markdown(task: &ExternalWorkItem) -> String {
    let quoted = |value: &str| serde_json::Value::String(value.to_owned()).to_string();
    let mut text = String::from("---\n");
    let _ = writeln!(text, "id: {}", task.id);
    let _ = writeln!(text, "handle: {}", task.handle);
    let _ = writeln!(text, "title: {}", quoted(&task.title));
    let _ = writeln!(text, "status: {}", status_text(task.status).to_lowercase());
    let _ = writeln!(
        text,
        "priority: {}",
        Priority::text(task.priority).to_lowercase()
    );
    let owner = match &task.owner {
        ExternalWorkOwner::Myself => "me".to_owned(),
        ExternalWorkOwner::Person { name, person_id } => {
            quoted(name.as_deref().unwrap_or(person_id))
        }
        ExternalWorkOwner::Nobody => "nobody".to_owned(),
    };
    let _ = writeln!(text, "owner: {owner}");
    if let Some(due) = &task.due_date {
        let _ = writeln!(text, "due: {due}");
    }
    if let Some(project) = &task.project {
        let _ = writeln!(text, "project: {}", quoted(&project.name));
    }
    let labels: Vec<String> = task
        .labels
        .iter()
        .map(|label| quoted(&label.name))
        .collect();
    let _ = writeln!(text, "labels: [{}]", labels.join(", "));
    if let Some(source) = &task.source {
        let _ = writeln!(text, "source: {}", source.link);
        if let Some(meeting) = &source.meeting_title {
            let _ = writeln!(text, "meeting: {}", quoted(meeting));
        }
    }
    if task.archived_at_ms.is_some() {
        text.push_str("archived: true\n");
    }
    let _ = writeln!(text, "updated: {}", local_time(task.updated_at_ms));
    let _ = writeln!(text, "revision: {}", task.revision);
    let _ = writeln!(text, "---\n\n# {}", task.title);
    if let Some(details) = &task.details {
        let _ = writeln!(text, "\n{details}");
    }
    if let Some(quote) = task
        .source
        .as_ref()
        .and_then(|source| source.quote.as_deref())
    {
        text.push('\n');
        for line in quote.lines() {
            let _ = writeln!(text, "> {line}");
        }
        if let Some(said) = task.source.as_ref().and_then(said_by) {
            let _ = writeln!(text, ">\n> {said}");
        }
    }
    text
}

/// "Dana, Weekly sync at 12:04", as much of it as is known.
fn said_by(source: &ExternalWorkSource) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(speaker) = &source.speaker {
        parts.push(speaker.clone());
    }
    match (&source.meeting_title, source.at_ms) {
        (Some(title), Some(at_ms)) => parts.push(format!("{title} at {}", offset(at_ms))),
        (Some(title), None) => parts.push(title.clone()),
        (None, Some(at_ms)) => parts.push(format!("at {}", offset(at_ms))),
        (None, None) => {}
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn offset(at_ms: i64) -> String {
    let seconds = at_ms.max(0) / 1000;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// The local day and time the text form measures due days and snoozes by.
struct Today {
    day: String,
    now_ms: i64,
}

impl WorkAnswer {
    /// The answer as plain text for a person at a terminal.
    pub(super) fn text(&self) -> String {
        let now_ms = now_ms();
        let today = Today {
            day: iso_day(now_ms).unwrap_or_default(),
            now_ms,
        };
        let mut text = String::new();
        match self {
            Self::Page(page) => {
                if page.entries.is_empty() {
                    text.push_str("No tasks match.\n");
                }
                for task in &page.entries {
                    task_line(&mut text, task, &today);
                }
                if page.has_more {
                    let _ = writeln!(
                        text,
                        "{} of {} shown. Raise --limit or narrow the filters.",
                        page.entries.len(),
                        page.total
                    );
                }
            }
            Self::Mine(mine) => {
                counts_line(&mut text, &mine.counts);
                section(&mut text, "You owe", &mine.i_owe, &today);
                section(&mut text, "Waiting on", &mine.waiting_on, &today);
                if mine.has_more {
                    text.push_str("\nMore not shown. Raise --limit.\n");
                }
            }
            Self::Counts(counts) => counts_line(&mut text, &counts.counts),
            Self::Detail(detail) => detail_text(&mut text, detail, &today),
            Self::Change(change) => change_text(&mut text, change, &today),
            Self::Preview(preview) => preview_text(&mut text, preview),
            Self::Relation(change) => {
                let relation = &change.relation;
                let link = match relation.kind {
                    WorkRelationKind::Blocks => "blocks",
                    WorkRelationKind::Related => "is related to",
                };
                let _ = writeln!(
                    text,
                    "{} {} {link} {} {}.",
                    if change.removed {
                        "Unlinked:"
                    } else {
                        "Linked:"
                    },
                    relation.from.handle,
                    relation.to.handle,
                    relation.to.title
                );
            }
            Self::Labels(labels) => {
                if labels.labels.is_empty() {
                    text.push_str("No labels yet. Make one with: sona work label-save NAME\n");
                }
                for label in &labels.labels {
                    let _ = writeln!(text, "{}  (hue {})", label.name, label.hue);
                }
            }
            Self::Label(change) => {
                let verb = match (change.deleted, &change.operation_id) {
                    (true, _) => "Deleted",
                    (false, None) => "Unchanged",
                    (false, Some(_)) => "Saved",
                };
                let _ = writeln!(
                    text,
                    "{verb} label {} (hue {}).",
                    change.label.name, change.label.hue
                );
            }
            Self::Views(views) => {
                if views.views.is_empty() {
                    text.push_str("No saved views.\n");
                }
                for view in &views.views {
                    let _ = writeln!(text, "{}  · {}", view.name, filters_text(&view.filters));
                }
            }
            Self::View(change) => {
                let _ = writeln!(
                    text,
                    "{} view {}  · {}",
                    if change.deleted { "Deleted" } else { "Saved" },
                    change.view.name,
                    filters_text(&change.view.filters)
                );
            }
            Self::Export(export) => {
                let _ = writeln!(
                    text,
                    "Wrote {} {} to {}.",
                    export.files.len(),
                    if export.files.len() == 1 {
                        "file"
                    } else {
                        "files"
                    },
                    export.directory
                );
            }
        }
        if let Some(note) = left_out(self.conversations_available()) {
            let _ = writeln!(text, "{note}");
        }
        let end = text.trim_end().len();
        text.truncate(end);
        text
    }

    fn conversations_available(&self) -> bool {
        match self {
            Self::Page(page) => page.conversations_available,
            Self::Mine(mine) => mine.conversations_available,
            Self::Counts(counts) => counts.conversations_available,
            Self::Detail(detail) => detail.conversations_available,
            Self::Export(export) => export.conversations_available,
            Self::Change(_)
            | Self::Preview(_)
            | Self::Relation(_)
            | Self::Labels(_)
            | Self::Label(_)
            | Self::Views(_)
            | Self::View(_) => true,
        }
    }
}

fn task_line(text: &mut String, task: &ExternalWorkItem, today: &Today) {
    let _ = write!(
        text,
        "{:<7}{:<13}{:<8}{}",
        task.handle,
        status_text(task.status),
        task.priority.short(),
        task.title
    );
    let mut notes = Vec::new();
    match &task.owner {
        ExternalWorkOwner::Myself => notes.push("you".to_owned()),
        ExternalWorkOwner::Person { name, .. } => {
            notes.push(name.clone().unwrap_or_else(|| "someone".to_owned()));
        }
        ExternalWorkOwner::Nobody => {}
    }
    if let Some(due) = &task.due_date {
        let late = task.status.is_open() && *due < today.day;
        notes.push(format!(
            "due {}{}",
            short_day(due),
            if late { " (overdue)" } else { "" }
        ));
    }
    if let Some(project) = &task.project {
        notes.push(project.name.clone());
    }
    if !task.labels.is_empty() {
        let names: Vec<&str> = task
            .labels
            .iter()
            .map(|label| label.name.as_str())
            .collect();
        notes.push(format!("[{}]", names.join(", ")));
    }
    if task.blocked_by_open > 0 {
        notes.push(format!("blocked by {}", task.blocked_by_open));
    }
    if let Some(until) = task.snoozed_until_ms.filter(|until| *until > today.now_ms) {
        notes.push(format!("snoozed until {}", day_text(Some(until))));
    }
    if task.archived_at_ms.is_some() {
        notes.push("archived".to_owned());
    }
    if task.status == WorkStatus::Triage {
        if let Some(meeting) = task
            .source
            .as_ref()
            .and_then(|source| source.meeting_title.as_deref())
        {
            notes.push(format!("from {meeting}"));
        }
    }
    if !notes.is_empty() {
        text.push_str("  · ");
        text.push_str(&notes.join(" · "));
    }
    text.push('\n');
}

fn short_day(iso: &str) -> String {
    NaiveDate::parse_from_str(iso, "%Y-%m-%d").map_or_else(
        |_| iso.to_owned(),
        |day| day.format("%a %-d %b").to_string(),
    )
}

fn counts_line(text: &mut String, counts: &ExternalWorkCounts) {
    let _ = writeln!(
        text,
        "Triage {} · yours open {} · overdue {} · due today {} · waiting on {} ({} to follow up)",
        counts.triage,
        counts.mine_open,
        counts.mine_overdue,
        counts.mine_due_today,
        counts.waiting_on,
        counts.waiting_on_stale
    );
}

fn section(text: &mut String, heading: &str, tasks: &[ExternalWorkItem], today: &Today) {
    let _ = writeln!(text, "\n{heading}");
    if tasks.is_empty() {
        text.push_str("  nothing\n");
    }
    for task in tasks {
        text.push_str("  ");
        task_line(text, task, today);
    }
}

fn detail_text(text: &mut String, detail: &ExternalWorkDetail, today: &Today) {
    let task = &detail.task;
    task_line(text, task, today);
    if let Some(details) = &task.details {
        for line in details.lines() {
            let _ = writeln!(text, "  {line}");
        }
    }
    if let Some(source) = &task.source {
        let said = said_by(source).unwrap_or_else(|| "a meeting".to_owned());
        match &source.quote {
            Some(quote) => {
                let _ = writeln!(text, "\nFrom {said}: \"{quote}\"");
            }
            None => {
                let _ = writeln!(text, "\nFrom {said}");
            }
        }
        let _ = writeln!(text, "  {}", source.link);
    }
    if !detail.relations.is_empty() {
        text.push('\n');
        for link in &detail.relations {
            let kind = match link.kind {
                ExternalLinkKind::Blocks => "Blocks",
                ExternalLinkKind::BlockedBy => "Blocked by",
                ExternalLinkKind::Related => "Related to",
            };
            let _ = writeln!(
                text,
                "{kind} {} {} ({})",
                link.task.handle,
                link.task.title,
                status_text(link.task.status)
            );
        }
    }
    if !detail.evidence.is_empty() {
        text.push_str("\nEvidence\n");
        for evidence in &detail.evidence {
            let title = evidence.title.as_deref().unwrap_or("unavailable source");
            match &evidence.link {
                Some(link) => {
                    let _ = writeln!(text, "  {title}  {link}");
                }
                None => {
                    let _ = writeln!(text, "  {title}");
                }
            }
        }
    }
    if !detail.suggestions.is_empty() {
        text.push_str("\nSuggested\n");
        for suggestion in &detail.suggestions {
            let field = match suggestion.field {
                WorkSuggestionField::Owner => "owner",
                WorkSuggestionField::Due => "due",
                WorkSuggestionField::Project => "project",
                WorkSuggestionField::Duplicate => "duplicate of",
            };
            let _ = writeln!(
                text,
                "  {field}: {} ({})",
                suggestion.value, suggestion.reason
            );
        }
    }
    if !detail.activity.is_empty() {
        text.push_str("\nHistory\n");
        for entry in &detail.activity {
            let actor = match entry.actor {
                WorkActor::Me => "you",
                WorkActor::Sona => "Sona",
                WorkActor::Agent => "agent",
            };
            let kind = serde_json::to_value(entry.kind)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            let change = match (&entry.from, &entry.to) {
                (Some(from), Some(to)) => format!("{kind}: {from} → {to}"),
                (None, Some(to)) => format!("{kind}: {to}"),
                (Some(from), None) => format!("{kind}: removed {from}"),
                (None, None) => kind,
            };
            let _ = writeln!(text, "  {}  {actor}  {change}", local_time(entry.at_ms));
        }
    }
    let _ = writeln!(text, "\nid {} · revision {}", task.id, task.revision);
}

fn change_text(text: &mut String, change: &ExternalWorkChange, today: &Today) {
    if change.tasks.is_empty() && change.skipped.is_empty() {
        text.push_str("Nothing to change.\n");
    }
    if !change.tasks.is_empty() {
        if change.replayed {
            text.push_str(
                "Already created by this same request earlier today; pass a new --operation-id to create another:\n",
            );
        } else {
            let _ = writeln!(text, "{}:", change.action.heading());
        }
        for task in &change.tasks {
            task_line(text, task, today);
        }
    }
    for skip in &change.skipped {
        let _ = writeln!(text, "Left {} alone: {}.", skip.handle, skip.reason);
    }
}

fn preview_text(text: &mut String, preview: &ExternalWorkPreview) {
    for change in &preview.changes {
        if let Some(reason) = &change.skipped {
            let _ = writeln!(text, "Would leave {} alone: {reason}.", change.handle);
            continue;
        }
        let _ = writeln!(text, "{} {}", change.handle, change.title);
        if change.fields.is_empty() {
            text.push_str("  nothing would change\n");
        }
        for field in &change.fields {
            let _ = writeln!(text, "  {}: {} → {}", field.field, field.from, field.to);
        }
        if let Some(effect) = change.commitment {
            let effect = match effect {
                CommitmentEffect::Closes => "closes the meeting commitment it came from",
                CommitmentEffect::SetsAside => "sets aside the meeting commitment it came from",
                CommitmentEffect::Reopens => "reopens the meeting commitment it came from",
            };
            let _ = writeln!(text, "  {effect}");
        }
    }
    text.push_str("Dry run: nothing was written.");
    if let Some(operation_id) = &preview.operation_id {
        let _ = write!(
            text,
            " The same command without --dry-run writes under operation id {operation_id}."
        );
    }
    text.push('\n');
}

fn filters_text(filters: &ExternalWorkFilters) -> String {
    let mut parts = Vec::new();
    if let Some(statuses) = &filters.statuses {
        let names: Vec<&str> = statuses.iter().map(|status| status_text(*status)).collect();
        parts.push(names.join(", "));
    }
    match filters.direction {
        Some(WorkDirection::Mine) => parts.push("yours".to_owned()),
        Some(WorkDirection::WaitingOn) => parts.push("waiting on others".to_owned()),
        Some(WorkDirection::Unattributed) => parts.push("unowned".to_owned()),
        None => {}
    }
    for (name, values) in [
        ("owners", &filters.owner_person_ids),
        ("projects", &filters.project_ids),
        ("labels", &filters.label_ids),
    ] {
        if let Some(values) = values {
            parts.push(format!("{name} {}", values.len()));
        }
    }
    if let Some(priorities) = &filters.priorities {
        let names: Vec<&str> = priorities.iter().map(|priority| priority.text()).collect();
        parts.push(names.join(", "));
    }
    if let Some(before) = filters.due_before_ms {
        parts.push(format!("due before {}", day_text(Some(before))));
    }
    if let Some(search) = &filters.search {
        parts.push(format!("matching {search:?}"));
    }
    if filters.include_archived {
        parts.push("with archived".to_owned());
    }
    if parts.is_empty() {
        "everything".to_owned()
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests;
