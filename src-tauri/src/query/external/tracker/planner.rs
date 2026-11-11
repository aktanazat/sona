//! `sona work` verbs for planning: typed words read as fields, an hour
//! booked for a task, a wait asked about, a watched line made a task, the
//! links Sona suggests, the planner calendar, the stats trend, Home's Today
//! strip and a project made from a plan draft.

use super::{
    day, day_start_utc_ms, external_line, now_ms, refusal, task_ref, task_ref_of,
    ExternalWorkBlock, ExternalWorkChange, ExternalWorkLine, ExternalWorkRef, Fence, TaskRef,
    Tracker, WorkAction, WorkAnswer, WriteArgs,
};
use crate::commands::meeting::today_meetings;
use crate::meeting::detection::calendar::{CalendarAccess, CalendarSource};
use crate::meeting::store::today::{HomeToday, TodayCount, TodayMeetings, TodayStoreCounts};
use crate::meeting::types::MeetingSessionId;
use crate::meeting::upcoming::upcoming_window;
use crate::analytics::{DashboardTrendRange, DashboardTrendRequest};
use crate::managers::history::HistoryTrendProjection;
use crate::meeting::types::MeetingTrendProjection;
use crate::planning::suggestions::{
    self, PlanningSuggestion, PlanningSuggestionAvailability, PlanningSuggestionKind,
    PlanningSuggestionState,
};
use crate::planning::work::parse::{self, WorkField, WorkParse};
use crate::planning::work::types::{WorkActor, WorkOwner};
use crate::planning::work::{blocks, follow, watch};
use crate::planning::{self, EvidenceSourceKind, EvidenceTargetKind};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Local, NaiveDate, TimeZone as _};
use clap::{Args, ValueEnum};
use rusqlite::OptionalExtension as _;
use serde::Serialize;
use std::fmt::Write as _;
use std::path::PathBuf;
use tauri::Manager;
use uuid::Uuid;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ParseArgs {
    /// The words, as typed in the task box: "call Dana friday p1 #design".
    #[arg(required = true, num_args = 1.., value_name = "WORDS")]
    pub words: Vec<String>,
    /// Read them as said aloud: no sigils, the owner named first.
    #[arg(long)]
    pub spoken: bool,
    /// Words to keep as plain words, read as nothing. Repeatable.
    #[arg(long, value_name = "WORD")]
    pub plain: Vec<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct BookArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct WatchTaskArgs {
    /// The watched topic or its id, from `sona work watches`.
    #[arg(value_name = "WATCH")]
    pub watch: String,
    /// The heard line's number, as `sona work watches` lists it.
    #[arg(long, value_name = "N")]
    pub line: usize,
    /// Print the line and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LinkSuggestionsArgs {
    /// Also list the ones already accepted or dismissed.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LinkDecideArgs {
    /// The suggestion's id, from `sona work link-suggestions`.
    #[arg(value_name = "ID")]
    pub id: Uuid,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CalendarArgs {
    /// The first day: YYYY-MM-DD, today or tomorrow.
    #[arg(long, value_parser = day, default_value = "today", value_name = "DAY")]
    pub from: NaiveDate,
    /// The last day, included. Defaults to six days after --from.
    #[arg(long, value_parser = day, value_name = "DAY")]
    pub to: Option<NaiveDate>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum TrendRangeArg {
    #[value(name = "7")]
    Days7,
    #[value(name = "30")]
    Days30,
    #[value(name = "180")]
    Days180,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TrendArgs {
    /// How many days back, today included.
    #[arg(long, value_enum, default_value = "30", value_name = "DAYS")]
    pub range: TrendRangeArg,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PlanAcceptArgs {
    /// The plan draft as JSON, as Plan with Sona makes it: a file, or - for
    /// standard input.
    #[arg(long, value_name = "PATH", required_unless_present = "draft", conflicts_with = "draft")]
    pub draft_file: Option<PathBuf>,
    /// The plan draft as JSON, given inline.
    #[arg(long, value_name = "JSON")]
    pub draft: Option<String>,
    /// Check the draft and print what it would make, writing nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

/// The properties a view's rows can show, in the app's words.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum PropertyArg {
    Id,
    Priority,
    Labels,
    Project,
    Owner,
    Due,
    Meeting,
    Updated,
}

impl PropertyArg {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Priority => "priority",
            Self::Labels => "labels",
            Self::Project => "project",
            Self::Owner => "owner",
            Self::Due => "due",
            Self::Meeting => "meeting",
            Self::Updated => "updated",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum PlannerAnswer {
    Parse(ExternalWorkParse),
    Task(ExternalTaskPlan),
    Suggestions(ExternalLinkSuggestions),
    Decision(ExternalLinkDecision),
    Calendar(ExternalPlannerCalendar),
    Trend(ExternalTrend),
    Line(ExternalWatchLine),
    Today(ExternalToday),
    Plan(Box<ExternalPlanAccept>),
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkParse {
    pub schema_version: u32,
    pub text: String,
    pub spoken: bool,
    #[serde(flatten)]
    pub read: WorkParse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    Booked,
    Unbooked,
    Nudged,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTaskPlan {
    pub schema_version: u32,
    pub action: PlanAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    /// True when an earlier run already did it.
    pub replayed: bool,
    pub task: ExternalWorkRef,
    /// The hour booked for it once this ran; none after unbooking.
    pub time_block: Option<ExternalWorkBlock>,
    /// What a dry run would do.
    pub would: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalLinkEvidence {
    pub title: String,
    pub passage_id: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalLinkSuggestion {
    pub id: String,
    pub kind: PlanningSuggestionKind,
    pub state: PlanningSuggestionState,
    pub revision: i64,
    pub target_kind: EvidenceTargetKind,
    pub target_id: String,
    pub target_title: Option<String>,
    pub project_id: Option<String>,
    pub source_kind: EvidenceSourceKind,
    pub source_id: String,
    pub at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub availability: PlanningSuggestionAvailability,
    pub evidence: Option<ExternalLinkEvidence>,
}

fn project_suggestion(suggestion: PlanningSuggestion) -> ExternalLinkSuggestion {
    ExternalLinkSuggestion {
        id: suggestion.id,
        kind: suggestion.kind,
        state: suggestion.state,
        revision: suggestion.revision,
        target_kind: suggestion.target_kind,
        target_id: suggestion.target_id,
        target_title: suggestion.target_title,
        project_id: suggestion.project_id,
        source_kind: suggestion.source.kind,
        source_id: suggestion.source.id,
        at_ms: suggestion.at_ms,
        created_at_ms: suggestion.created_at_ms,
        availability: suggestion.availability,
        evidence: suggestion.evidence.map(|evidence| ExternalLinkEvidence {
            title: evidence.title,
            passage_id: evidence.passage_id,
            text: evidence.text,
        }),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalLinkSuggestions {
    pub schema_version: u32,
    pub suggestions: Vec<ExternalLinkSuggestion>,
    /// False while the conversations are closed: suggestions drawn from
    /// meetings are left out.
    pub conversations_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkAction {
    Accepted,
    Dismissed,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalLinkDecision {
    pub schema_version: u32,
    pub action: LinkAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub replayed: bool,
    pub suggestion: ExternalLinkSuggestion,
    /// The link it made, once accepted.
    pub evidence_link_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCalendarItem {
    pub id: String,
    /// task, milestone or meeting.
    pub kind: String,
    pub title: String,
    pub project_id: Option<String>,
    pub due_at_ms: i64,
    /// YYYY-MM-DD, local.
    pub day: String,
    pub meeting_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPlannerCalendar {
    pub schema_version: u32,
    pub from: String,
    pub to: String,
    pub items: Vec<ExternalCalendarItem>,
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTrend {
    pub schema_version: u32,
    pub dictation: HistoryTrendProjection,
    pub meetings: MeetingTrendProjection,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWatchLine {
    pub schema_version: u32,
    pub dry_run: bool,
    pub watch: String,
    pub line: ExternalWorkLine,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalToday {
    pub schema_version: u32,
    #[serde(flatten)]
    pub today: HomeToday,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPlanAccept {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub title: String,
    pub milestones: usize,
    pub tasks: usize,
    pub linked_commitments: usize,
    /// The project made, with its milestones and tasks. Absent on a dry run.
    pub project: Option<planning::ProjectDetail>,
}

fn planner(answer: PlannerAnswer) -> WorkAnswer {
    WorkAnswer::Planner(Box::new(answer))
}

fn local_date(at_ms: i64) -> String {
    Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map(|at| at.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

impl Tracker<'_> {
    pub(super) async fn parse(&self, args: &ParseArgs) -> Result<WorkAnswer, ExternalError> {
        let text = args.words.join(" ");
        let read = parse::work_parse(
            self.app.state(),
            self.app.state(),
            text.clone(),
            args.plain.clone(),
            args.spoken,
        )
        .await
        .map_err(|message| refusal(self.history, message))?;
        Ok(planner(PlannerAnswer::Parse(ExternalWorkParse {
            schema_version: QUERY_SCHEMA_VERSION,
            text,
            spoken: args.spoken,
            read,
        })))
    }

    pub(super) async fn book(&self, args: &BookArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let now = now_ms();
        let held = row
            .time_block
            .filter(|block| block.ends_at_ms > now)
            .map(|block| ExternalWorkBlock {
                starts_at_ms: block.starts_at_ms,
                ends_at_ms: block.ends_at_ms,
            });
        let task = task_ref_of(row);
        if args.dry_run || held.is_some() {
            let would = held.is_none().then(|| {
                let until = row.item.due_at_ms.filter(|due| *due >= now + 3_600_000);
                match until {
                    Some(due) => format!(
                        "Book the first free working hour before it is due on {} in the \"Sona\" calendar.",
                        local_date(due)
                    ),
                    None => "Book the first free working hour in the coming week in the \"Sona\" calendar.".to_owned(),
                }
            });
            return Ok(planner(PlannerAnswer::Task(ExternalTaskPlan {
                schema_version: QUERY_SCHEMA_VERSION,
                action: PlanAction::Booked,
                dry_run: args.dry_run,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                replayed: held.is_some(),
                task,
                time_block: held,
                would,
            })));
        }
        self.applies("booking time")?;
        let operation_id = args.write.fresh_operation_id();
        let started_ms = now_ms();
        let block = blocks::work_time_block(self.app.clone(), operation_id.clone(), row.item.id.clone())
            .await
            .map_err(|message| self.write_refusal(message, &operation_id, "The task"))?;
        self.credit(&args.write, std::slice::from_ref(&row.item.id), started_ms)?;
        Ok(planner(PlannerAnswer::Task(ExternalTaskPlan {
            schema_version: QUERY_SCHEMA_VERSION,
            action: PlanAction::Booked,
            dry_run: false,
            operation_id: Some(operation_id),
            replayed: false,
            task,
            time_block: Some(ExternalWorkBlock {
                starts_at_ms: block.starts_at_ms,
                ends_at_ms: block.ends_at_ms,
            }),
            would: None,
        })))
    }

    pub(super) async fn unbook(&self, args: &BookArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let held = row.time_block.map(|block| ExternalWorkBlock {
            starts_at_ms: block.starts_at_ms,
            ends_at_ms: block.ends_at_ms,
        });
        let task = task_ref_of(row);
        if args.dry_run || held.is_none() {
            return Ok(planner(PlannerAnswer::Task(ExternalTaskPlan {
                schema_version: QUERY_SCHEMA_VERSION,
                action: PlanAction::Unbooked,
                dry_run: args.dry_run,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                replayed: held.is_none(),
                task,
                time_block: held,
                would: held.map(|_| "Take its booked hour off the calendar.".to_owned()),
            })));
        }
        self.applies("taking a booked hour off the calendar")?;
        let operation_id = args.write.fresh_operation_id();
        let started_ms = now_ms();
        blocks::work_time_block_remove(self.app.clone(), operation_id.clone(), row.item.id.clone())
            .await
            .map_err(|message| self.write_refusal(message, &operation_id, "The task"))?;
        self.credit(&args.write, std::slice::from_ref(&row.item.id), started_ms)?;
        Ok(planner(PlannerAnswer::Task(ExternalTaskPlan {
            schema_version: QUERY_SCHEMA_VERSION,
            action: PlanAction::Unbooked,
            dry_run: false,
            operation_id: Some(operation_id),
            replayed: false,
            task,
            time_block: None,
            would: None,
        })))
    }

    pub(super) async fn nudged(&self, args: &BookArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        if !matches!(row.item.owner(), WorkOwner::Person { .. }) {
            return Err(ExternalError::invalid(format!(
                "{} is not waiting on a person, so there is nobody to have asked.",
                task_ref_of(row).handle
            )));
        }
        let task = task_ref_of(row);
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let nudged_at: Option<i64> = planning::history(self.history, |conn| {
            Ok(conn
                .query_row(
                    "SELECT nudged_at_ms FROM planning_nudges WHERE task_id = ?1",
                    [&row.item.id],
                    |found| found.get(0),
                )
                .optional()?)
        })
        .map_err(|message| refusal(self.history, message))?;
        let replayed = nudged_at.is_some_and(|at| local_date(at) == today);
        let operation_id = args.write.operation_id("nudged", &(&row.item.id, &today))?;
        if args.dry_run || replayed {
            return Ok(planner(PlannerAnswer::Task(ExternalTaskPlan {
                schema_version: QUERY_SCHEMA_VERSION,
                action: PlanAction::Nudged,
                dry_run: args.dry_run,
                operation_id: Some(operation_id),
                replayed,
                task,
                time_block: None,
                would: (!replayed).then(|| {
                    "Mark it asked about today, so its follow-up comes back a week from now."
                        .to_owned()
                }),
            })));
        }
        self.applies("recording a nudge")?;
        follow::work_nudged(self.app.clone(), row.item.id.clone())
            .map_err(|message| self.write_refusal(message, &operation_id, "The task"))?;
        Ok(planner(PlannerAnswer::Task(ExternalTaskPlan {
            schema_version: QUERY_SCHEMA_VERSION,
            action: PlanAction::Nudged,
            dry_run: false,
            operation_id: Some(operation_id),
            replayed: false,
            task,
            time_block: None,
            would: None,
        })))
    }

    pub(super) async fn watch_task(
        &self,
        args: &WatchTaskArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let wanted = args.watch.trim();
        let found = watch::list(self.app)
            .await
            .map_err(|message| refusal(self.history, message))?
            .into_iter()
            .find(|watch| watch.id == wanted || watch.phrase.eq_ignore_ascii_case(wanted))
            .ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    format!("Nothing watches \u{201C}{wanted}\u{201D}."),
                )
                .hint("List them with: sona work watches")
            })?;
        let board = self.board().await?;
        let heard: Vec<_> = found
            .hits
            .iter()
            .filter(|hit| !board.fence.hides_line(&hit.line))
            .collect();
        let hit = args
            .line
            .checked_sub(1)
            .and_then(|index| heard.get(index))
            .ok_or_else(|| {
                ExternalError::invalid(format!(
                    "There is no line {}; \u{201C}{}\u{201D} has {} heard.",
                    args.line,
                    found.phrase,
                    heard.len()
                ))
            })?;
        if let Some(task_id) = &hit.task_id {
            let row = board.row(task_id).ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    "That line's task is not one you can see.",
                )
            })?;
            return Ok(WorkAnswer::Change(ExternalWorkChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: WorkAction::Created,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                tasks: vec![board.project_item(row.item.clone())],
                skipped: Vec::new(),
                proposed: Vec::new(),
                replayed: true,
                evidence_url: None,
            }));
        }
        if args.dry_run {
            return Ok(planner(PlannerAnswer::Line(ExternalWatchLine {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                watch: found.phrase.clone(),
                line: external_line(args.line, &hit.line),
            })));
        }
        let operation_id = args.write.operation_id("watch-task", &hit.id)?;
        let started_ms = now_ms();
        let item = watch::task_from(self.app, WorkActor::Agent, &hit.id)
            .await
            .map_err(|message| self.write_refusal(message, &operation_id, "The line"))?;
        self.credit(&args.write, std::slice::from_ref(&item.id), started_ms)?;
        let replayed = item.created_at_ms < started_ms;
        Ok(WorkAnswer::Change(ExternalWorkChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: WorkAction::Created,
            operation_id: Some(operation_id),
            tasks: vec![board.project_item(item)],
            skipped: Vec::new(),
            proposed: Vec::new(),
            replayed,
            evidence_url: None,
        }))
    }

    /// The link suggestions an outside caller may see: none drawn from a
    /// meeting kept on this Mac, none about a task it cannot see.
    async fn visible_suggestions(&self) -> Result<(Vec<PlanningSuggestion>, bool), ExternalError> {
        let board = self.board().await?;
        let all = suggestions::planning_suggestions(self.app.state(), self.app.state())
            .await
            .map_err(|message| refusal(self.history, message))?;
        let shown = all
            .into_iter()
            .filter(|suggestion| {
                !board
                    .fence
                    .hides_source(suggestion.source.kind, &suggestion.source.id)
                    && (suggestion.target_kind != EvidenceTargetKind::Task
                        || board.row(&suggestion.target_id).is_some())
            })
            .collect();
        Ok((shown, board.fence.is_open()))
    }

    pub(super) async fn link_suggestions(
        &self,
        args: &LinkSuggestionsArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let (mut shown, conversations_available) = self.visible_suggestions().await?;
        if !args.all {
            shown.retain(|suggestion| suggestion.state == PlanningSuggestionState::Pending);
        }
        Ok(planner(PlannerAnswer::Suggestions(ExternalLinkSuggestions {
            schema_version: QUERY_SCHEMA_VERSION,
            suggestions: shown.into_iter().map(project_suggestion).collect(),
            conversations_available,
        })))
    }

    pub(super) async fn link_decide(
        &self,
        args: &LinkDecideArgs,
        action: LinkAction,
    ) -> Result<WorkAnswer, ExternalError> {
        let id = args.id.to_string();
        let (shown, _) = self.visible_suggestions().await?;
        let suggestion = shown
            .into_iter()
            .find(|suggestion| suggestion.id == id)
            .ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::NotFound, format!("No link suggestion {id}."))
                    .hint("List them with: sona work link-suggestions")
            })?;
        let settled = match (action, suggestion.state) {
            (_, PlanningSuggestionState::Pending) => false,
            (LinkAction::Accepted, PlanningSuggestionState::Accepted)
            | (LinkAction::Dismissed, PlanningSuggestionState::Declined) => true,
            (_, state) => {
                return Err(ExternalError::invalid(format!(
                    "That suggestion was already {}.",
                    match state {
                        PlanningSuggestionState::Accepted => "accepted",
                        _ => "dismissed",
                    }
                )))
            }
        };
        if args.dry_run || settled {
            return Ok(planner(PlannerAnswer::Decision(ExternalLinkDecision {
                schema_version: QUERY_SCHEMA_VERSION,
                action,
                dry_run: args.dry_run,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                replayed: settled,
                suggestion: project_suggestion(suggestion),
                evidence_link_id: None,
            })));
        }
        let (verb, what) = match action {
            LinkAction::Accepted => ("link-accept", "accepting a link suggestion"),
            LinkAction::Dismissed => ("link-dismiss", "dismissing a link suggestion"),
        };
        self.applies(what)?;
        let operation_id = args
            .write
            .operation_id(verb, &(&suggestion.id, suggestion.revision))?;
        let decision = match action {
            LinkAction::Accepted => {
                suggestions::planning_accept_suggestion(
                    self.app.clone(),
                    self.app.state(),
                    self.app.state(),
                    suggestion.id.clone(),
                    suggestion.revision,
                    operation_id.clone(),
                )
                .await
            }
            LinkAction::Dismissed => suggestions::planning_decline_suggestion(
                self.app.state(),
                suggestion.id.clone(),
                suggestion.revision,
                operation_id.clone(),
            ),
        }
        .map_err(|message| self.write_refusal(message, &operation_id, "The suggestion"))?;
        let mut changed = project_suggestion(suggestion);
        changed.state = decision.state;
        changed.revision = decision.revision;
        Ok(planner(PlannerAnswer::Decision(ExternalLinkDecision {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run: false,
            operation_id: Some(operation_id),
            replayed: false,
            suggestion: changed,
            evidence_link_id: decision.evidence_link_id,
        })))
    }

    pub(super) async fn calendar(&self, args: &CalendarArgs) -> Result<WorkAnswer, ExternalError> {
        let to = match args.to {
            Some(to) => to,
            None => args
                .from
                .checked_add_days(chrono::Days::new(6))
                .ok_or_else(|| ExternalError::invalid("That day is past the calendar."))?,
        };
        if to < args.from {
            return Err(ExternalError::invalid("--to comes before --from."));
        }
        let after = to
            .succ_opt()
            .ok_or_else(|| ExternalError::invalid("That day is past the calendar."))?;
        let start_ms = day_start_utc_ms(&args.from.format("%Y-%m-%d").to_string())?;
        let end_ms = day_start_utc_ms(&after.format("%Y-%m-%d").to_string())?;
        let board = self.board().await?;
        let page = planning::planning_calendar(self.app.state(), self.app.state(), start_ms, end_ms)
            .await
            .map_err(|message| refusal(self.history, message))?;
        let items = page
            .items
            .into_iter()
            .filter_map(|item| {
                let kind = serde_json::to_value(item.kind)
                    .ok()?
                    .as_str()
                    .map(str::to_owned)?;
                if item
                    .meeting_id
                    .as_deref()
                    .is_some_and(|meeting| board.fence.hides_meeting(meeting))
                    || (kind == "task" && board.row(&item.id).is_none())
                {
                    return None;
                }
                Some(ExternalCalendarItem {
                    day: local_date(item.due_at_ms),
                    id: item.id,
                    kind,
                    title: item.title,
                    project_id: item.project_id,
                    due_at_ms: item.due_at_ms,
                    meeting_id: item.meeting_id,
                })
            })
            .collect();
        Ok(planner(PlannerAnswer::Calendar(ExternalPlannerCalendar {
            schema_version: QUERY_SCHEMA_VERSION,
            from: args.from.format("%Y-%m-%d").to_string(),
            to: to.format("%Y-%m-%d").to_string(),
            items,
            conversations_available: page.conversations_available && board.fence.is_open(),
        })))
    }

    pub(super) async fn trend(&self, args: &TrendArgs) -> Result<WorkAnswer, ExternalError> {
        let request = DashboardTrendRequest {
            range: match args.range {
                TrendRangeArg::Days7 => DashboardTrendRange::Days7,
                TrendRangeArg::Days30 => DashboardTrendRange::Days30,
                TrendRangeArg::Days180 => DashboardTrendRange::Days180,
            },
        };
        let dictation = self
            .history
            .get_history_trend(request)
            .await
            .map_err(|error| refusal(self.history, error.to_string()))?;
        let meetings = self.meetings.trend_projection(request).await;
        Ok(planner(PlannerAnswer::Trend(ExternalTrend {
            schema_version: QUERY_SCHEMA_VERSION,
            dictation,
            meetings,
        })))
    }

    pub(super) async fn today(
        &self,
        calendar: &dyn CalendarSource,
    ) -> Result<WorkAnswer, ExternalError> {
        let fence = Fence::read(self.meetings).await?;
        let hidden = |id: MeetingSessionId| match &fence {
            Fence::Open(kept_local) => kept_local.hides(id),
            Fence::Closed => true,
        };
        let store = super::store(self.meetings).await.ok();
        let meetings = match &store {
            None => Some(TodayCount::Unavailable),
            Some(_) if !crate::settings::get_settings(self.app).detection_calendar_enabled => None,
            Some(store) => {
                let window = upcoming_window(Local::now(), 0);
                let occurrences = calendar.events_between(window.0, window.1);
                match self
                    .meetings
                    .upcoming_events(calendar.access(), window, occurrences)
                    .await
                {
                    Ok(events) if events.access == CalendarAccess::Authorized => {
                        Some(TodayCount::read(
                            "meetings",
                            crate::query::tools::allowed_upcoming(store, events.rows)
                                .map(|rows| today_meetings(&rows)),
                        ))
                    }
                    Ok(_) => None,
                    Err(error) => Some(TodayCount::read("meetings", Err::<TodayMeetings, _>(error))),
                }
            }
        };
        let untracked = TodayCount::read(
            "untracked",
            crate::planning::untracked::tally(self.history, self.meetings, &hidden).await,
        );
        let counts = store.as_ref().map_or(TodayStoreCounts::UNAVAILABLE, |store| {
            store.today_counts(
                Local::now().date_naive(),
                self.meetings.user_name().as_deref(),
                &hidden,
            )
        });
        Ok(planner(PlannerAnswer::Today(ExternalToday {
            schema_version: QUERY_SCHEMA_VERSION,
            today: HomeToday {
                meetings,
                processing: counts.processing,
                unknown_speakers: counts.unknown_speakers,
                untracked,
                owed: counts.owed,
            },
        })))
    }

    pub(super) async fn plan_accept(
        &self,
        args: &PlanAcceptArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let json = match (&args.draft, &args.draft_file) {
            (Some(json), _) => json.clone(),
            (None, Some(path)) if path.as_os_str() == "-" => {
                std::io::read_to_string(std::io::stdin()).map_err(|error| {
                    ExternalError::invalid(format!(
                        "The draft could not be read from standard input: {error}."
                    ))
                })?
            }
            (None, Some(path)) => std::fs::read_to_string(path).map_err(|error| {
                ExternalError::invalid(format!("{} could not be read: {error}.", path.display()))
            })?,
            (None, None) => return Err(ExternalError::invalid("Pass --draft-file or --draft.")),
        };
        let draft: planning::PlanningDraft = serde_json::from_str(&json).map_err(|error| {
            ExternalError::invalid(format!(
                "That is not a plan draft: {error}. A draft is one JSON object with goal, sources, project, milestones, tasks and linkedCommitments, in camelCase."
            ))
        })?;
        let cites_meetings = !draft.linked_commitments.is_empty()
            || draft
                .sources
                .iter()
                .any(|source| source.source.kind != EvidenceSourceKind::Note);
        if cites_meetings {
            let fence = Fence::read(self.meetings).await?;
            if draft
                .sources
                .iter()
                .any(|source| fence.hides_source(source.source.kind, &source.source.id))
                || draft
                    .linked_commitments
                    .iter()
                    .any(|link| fence.hides_source(EvidenceSourceKind::Loop, &link.loop_id))
            {
                return Err(ExternalError::new(
                    ExternalErrorCode::NotFound,
                    "The draft cites a meeting that is not open to agents.",
                ));
            }
        }
        let mut answer = ExternalPlanAccept {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: args.write.operation_id.map(|id| id.to_string()),
            title: draft.project.title.clone(),
            milestones: draft.milestones.len(),
            tasks: draft.tasks.len(),
            linked_commitments: draft.linked_commitments.len(),
            project: None,
        };
        if args.dry_run {
            return Ok(planner(PlannerAnswer::Plan(Box::new(answer))));
        }
        self.applies("making a project from a plan")?;
        let operation_id = args.write.operation_id("plan-accept", &draft)?;
        let started_ms = now_ms();
        let detail = planning::planning_accept(
            self.app.clone(),
            self.app.state(),
            self.app.state(),
            draft,
            operation_id.clone(),
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, "A source"))?;
        let ids = detail
            .tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        self.credit(&args.write, &ids, started_ms)?;
        answer.operation_id = Some(operation_id);
        answer.project = Some(detail);
        Ok(planner(PlannerAnswer::Plan(Box::new(answer))))
    }
}

fn field_text(field: &WorkField) -> String {
    match field {
        WorkField::Due { at, timed } => {
            let when = Local.timestamp_millis_opt(*at).single();
            match (when, timed) {
                (Some(when), true) => format!("due {}", when.format("%Y-%m-%d %H:%M")),
                (Some(when), false) => format!("due {}", when.format("%Y-%m-%d")),
                (None, _) => "due".to_owned(),
            }
        }
        WorkField::Priority { level } => format!(
            "priority {}",
            match level {
                1 => "urgent",
                2 => "high",
                3 => "medium",
                _ => "low",
            }
        ),
        WorkField::Label { id, name } => match id {
            Some(_) => format!("label {name}"),
            None => format!("new label {name}"),
        },
        WorkField::Owner { name, .. } => format!("owner {name}"),
        WorkField::Project { title, .. } => format!("project {title}"),
        WorkField::Repeat { every, .. } => format!("repeats {every}"),
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let WorkAnswer::Planner(answer) = answer else {
        return;
    };
    match answer.as_ref() {
        PlannerAnswer::Parse(read) => {
            let title = if read.read.title.is_empty() {
                "(no title left)"
            } else {
                read.read.title.as_str()
            };
            let _ = writeln!(text, "Title: {title}");
            if read.read.tokens.is_empty() {
                text.push_str("No fields read from these words.\n");
            }
            for token in &read.read.tokens {
                let _ = writeln!(
                    text,
                    "  \u{201C}{}\u{201D} \u{2192} {}",
                    token.text,
                    field_text(&token.field)
                );
            }
        }
        PlannerAnswer::Task(plan) => {
            let name = if plan.task.handle.is_empty() {
                String::new()
            } else {
                format!("{} {}", plan.task.handle, plan.task.title)
            };
            if let Some(would) = &plan.would {
                let _ = writeln!(text, "Would: {would}");
                if !name.is_empty() {
                    let _ = writeln!(text, "  {name}");
                }
                return;
            }
            let verb = match (plan.action, plan.replayed) {
                (PlanAction::Booked, false) => "Booked an hour for",
                (PlanAction::Booked, true) => "Already booked for",
                (PlanAction::Unbooked, false) => "Took the booked hour off",
                (PlanAction::Unbooked, true) => "Nothing booked for",
                (PlanAction::Nudged, false) => "Marked asked about today:",
                (PlanAction::Nudged, true) => "Already asked about today:",
            };
            let _ = write!(text, "{verb} {name}");
            if let Some(block) = plan.time_block {
                let _ = write!(
                    text,
                    " · {} to {}",
                    super::local_time(block.starts_at_ms),
                    super::local_time(block.ends_at_ms)
                );
            }
            text.push('\n');
        }
        PlannerAnswer::Suggestions(page) => {
            if page.suggestions.is_empty() {
                text.push_str("No link suggestions waiting.\n");
            }
            for suggestion in &page.suggestions {
                let _ = writeln!(
                    text,
                    "{}  {} \u{2192} {}",
                    suggestion.id,
                    serde_json::to_value(suggestion.kind)
                        .ok()
                        .and_then(|kind| kind.as_str().map(str::to_owned))
                        .unwrap_or_default()
                        .replace('_', " "),
                    suggestion.target_title.as_deref().unwrap_or("(untitled)")
                );
                if let Some(evidence) = &suggestion.evidence {
                    let _ = writeln!(text, "  {}: {}", evidence.title, evidence.text);
                }
            }
            if !page.conversations_available {
                text.push_str("Suggestions from meetings are left out while the conversations are closed.\n");
            }
        }
        PlannerAnswer::Decision(decision) => {
            let verb = match (decision.action, decision.dry_run, decision.replayed) {
                (LinkAction::Accepted, true, _) => "Would accept",
                (LinkAction::Dismissed, true, _) => "Would dismiss",
                (LinkAction::Accepted, false, true) => "Already accepted",
                (LinkAction::Dismissed, false, true) => "Already dismissed",
                (LinkAction::Accepted, false, false) => "Accepted",
                (LinkAction::Dismissed, false, false) => "Dismissed",
            };
            let _ = writeln!(
                text,
                "{verb} the link to {}.",
                decision
                    .suggestion
                    .target_title
                    .as_deref()
                    .unwrap_or("(untitled)")
            );
        }
        PlannerAnswer::Calendar(calendar) => {
            let _ = writeln!(text, "{} to {}", calendar.from, calendar.to);
            if calendar.items.is_empty() {
                text.push_str("Nothing due or scheduled.\n");
            }
            let mut last = String::new();
            for item in &calendar.items {
                if item.day != last {
                    let _ = writeln!(text, "{}", item.day);
                    last.clone_from(&item.day);
                }
                let _ = writeln!(text, "  {} · {}", item.kind, item.title);
            }
        }
        PlannerAnswer::Line(line) => {
            let _ = writeln!(
                text,
                "Would make a task in Triage from this line heard for \u{201C}{}\u{201D}:",
                line.watch
            );
            super::line_text(text, &line.line);
        }
        PlannerAnswer::Trend(trend) => {
            let dictation = &trend.dictation;
            let _ = writeln!(
                text,
                "{} to {}: {} dictations, {} words, {} active days, a {}-day streak.",
                dictation.range_start_local_date,
                dictation.range_end_local_date,
                dictation.range_total.recordings,
                dictation.range_total.words,
                dictation.active_days,
                dictation.current_streak_days
            );
            match &trend.meetings {
                MeetingTrendProjection::Available { range_total, .. } => {
                    let _ = writeln!(
                        text,
                        "Meetings: {}, {} minutes captured, {} action items.",
                        range_total.meetings,
                        range_total.verified_captured_duration_ms / 60_000,
                        range_total.generated_action_items
                    );
                }
                MeetingTrendProjection::Unavailable { .. } => {
                    text.push_str("Meetings: unavailable while the conversations are closed.\n");
                }
            }
        }
        PlannerAnswer::Today(today) => today_text(&today.today, text),
        PlannerAnswer::Plan(plan) => match &plan.project {
            Some(detail) => {
                let _ = writeln!(
                    text,
                    "Made the project \u{201C}{}\u{201D} with {} milestones and {} tasks.",
                    detail.project.title,
                    detail.milestones.len(),
                    detail.tasks.len()
                );
            }
            None => {
                let _ = writeln!(
                    text,
                    "Would make the project \u{201C}{}\u{201D} with {} milestones, {} tasks and {} linked commitments.",
                    plan.title, plan.milestones, plan.tasks, plan.linked_commitments
                );
            }
        },
    }
}

fn count_text<T>(count: Option<&TodayCount<T>>, show: impl Fn(&T) -> String) -> String {
    match count {
        None => "no calendar connected".to_owned(),
        Some(TodayCount::Unavailable) => "could not be read".to_owned(),
        Some(TodayCount::Available { value }) => show(value),
    }
}

fn today_text(today: &HomeToday, text: &mut String) {
    let _ = writeln!(
        text,
        "Meetings today: {}",
        count_text(today.meetings.as_ref(), |meetings| {
            let names = meetings
                .people
                .iter()
                .map(|person| person.name.as_str())
                .collect::<Vec<_>>();
            if names.is_empty() {
                meetings.count.to_string()
            } else {
                format!("{}, with {}", meetings.count, names.join(", "))
            }
        })
    );
    let _ = writeln!(
        text,
        "Processing: {}",
        count_text(Some(&today.processing), u32::to_string)
    );
    let _ = writeln!(
        text,
        "Speakers nobody named: {}",
        count_text(Some(&today.unknown_speakers), |tally| tally.count.to_string())
    );
    let _ = writeln!(
        text,
        "Promises not tracked: {}",
        count_text(Some(&today.untracked), |tally| tally.count.to_string())
    );
    let _ = writeln!(
        text,
        "You owe: {}",
        count_text(Some(&today.owed), |owed| if owed.late == 0 {
            owed.count.to_string()
        } else {
            format!("{}, {} late", owed.count, owed.late)
        })
    );
}
