use super::{now_ms, pick, refusal, Board, StatusArg, Tracker, WorkAnswer, WriteArgs};
use crate::planning;
use crate::planning::work::loops::{
    self, LoopConditions, LoopDay, LoopEvery, LoopInput, LoopRunStatus, LoopTool, LoopTrigger,
    WorkLoop, WorkLoopDetail, WorkLoopRun,
};
use crate::planning::ProjectStatus;
use crate::query::external::{local_time, ExternalError};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, ValueEnum};
use rusqlite::Transaction;
use serde::Serialize;
use serde_json::Value;
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LoopShowArgs {
    /// The automation, by name or id.
    #[arg(value_name = "AUTOMATION")]
    pub name: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LoopArgs {
    /// The automation, by name or id.
    #[arg(value_name = "AUTOMATION")]
    pub name: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LoopRestoreArgs {
    /// The automation, by name or id.
    #[arg(value_name = "AUTOMATION")]
    pub name: String,
    /// The version to bring back, from automation-show.
    pub version: i64,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum EveryArg {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

impl EveryArg {
    const fn every(self) -> LoopEvery {
        match self {
            Self::Hourly => LoopEvery::Hourly,
            Self::Daily => LoopEvery::Daily,
            Self::Weekly => LoopEvery::Weekly,
            Self::Monthly => LoopEvery::Monthly,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum EventArg {
    TaskCreated,
    StatusChanged,
    LabelAdded,
    Assigned,
    CommentAdded,
    ProjectStatusChanged,
    MeetingEnded,
}

impl EventArg {
    const fn of(trigger: &LoopTrigger) -> Option<Self> {
        match trigger {
            LoopTrigger::Schedule { .. } => None,
            LoopTrigger::TaskCreated => Some(Self::TaskCreated),
            LoopTrigger::StatusChanged { .. } => Some(Self::StatusChanged),
            LoopTrigger::LabelAdded { .. } => Some(Self::LabelAdded),
            LoopTrigger::Assigned => Some(Self::Assigned),
            LoopTrigger::CommentAdded => Some(Self::CommentAdded),
            LoopTrigger::ProjectStatusChanged { .. } => Some(Self::ProjectStatusChanged),
            LoopTrigger::MeetingEnded => Some(Self::MeetingEnded),
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LoopSaveArgs {
    /// The automation's name.
    pub name: String,
    /// The automation to change, by name or id; left out, the one called NAME
    /// or a new one.
    #[arg(long = "automation", value_name = "AUTOMATION")]
    pub target: Option<String>,
    /// What Sona's agent is asked to do on each run.
    #[arg(long)]
    pub prompt: Option<String>,
    /// Run on a schedule.
    #[arg(long, value_enum, conflicts_with = "on")]
    pub schedule: Option<EveryArg>,
    /// The first scheduled run, "YYYY-MM-DD HH:MM" in --timezone. It sets the
    /// time of day, and the day of the month for monthly.
    #[arg(long, value_name = "WHEN")]
    pub first_run: Option<String>,
    /// An IANA time zone such as America/Los_Angeles; left out, this Mac's.
    #[arg(long, value_name = "ZONE")]
    pub timezone: Option<String>,
    /// The days a weekly schedule runs, comma-separated: mon,wed,fri.
    #[arg(long, value_delimiter = ',', value_name = "DAY")]
    pub days: Option<Vec<String>>,
    /// Run when this happens.
    #[arg(long, value_enum, value_name = "EVENT")]
    pub on: Option<EventArg>,
    /// With status-changed, a task status; with project-status-changed, a
    /// project status. "" means any.
    #[arg(long, value_name = "STATUS")]
    pub to: Option<String>,
    /// With label-added, the label by name or id. "" means any.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    /// Run only for tasks with one of these labels; "" clears.
    #[arg(long, value_delimiter = ',', value_name = "LABEL")]
    pub if_label: Option<Vec<String>>,
    /// Run only for tasks in, or projects that are, one of these projects;
    /// "" clears.
    #[arg(long, value_delimiter = ',', value_name = "PROJECT")]
    pub if_project: Option<Vec<String>>,
    /// Run only for tasks in one of these statuses; "" clears.
    #[arg(long, value_delimiter = ',', value_name = "STATUS")]
    pub if_status: Option<Vec<String>>,
    /// The lookups the agent may use, as Sona MCP tool names such as
    /// sona_search; "" clears. automation-list names them.
    #[arg(long, value_delimiter = ',', value_name = "TOOL")]
    pub tools: Option<Vec<String>>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalAgentLoops {
    pub schema_version: u32,
    pub automations: Vec<WorkLoop>,
    pub tools: Vec<LoopTool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalAgentLoop {
    pub schema_version: u32,
    #[serde(flatten)]
    pub detail: WorkLoopDetail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopAction {
    Saved,
    Unchanged,
    Enabled,
    Disabled,
    Restored,
    Deleted,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalAgentLoopChange {
    pub schema_version: u32,
    pub action: LoopAction,
    pub dry_run: bool,
    pub operation_id: String,
    #[serde(rename = "automation")]
    pub item: WorkLoop,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalAgentLoopRun {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    pub run: WorkLoopRun,
}

fn listed(values: &[String]) -> impl Iterator<Item = &str> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

fn day(value: &str) -> Result<LoopDay, ExternalError> {
    let short: String = value.to_lowercase().chars().take(3).collect();
    serde_json::from_value(Value::String(short)).map_err(|_| {
        ExternalError::invalid(format!(
            "{value:?} is not a day. Use mon, tue, wed, thu, fri, sat or sun."
        ))
    })
}

fn task_status(value: &str) -> Result<planning::work::types::WorkStatus, ExternalError> {
    StatusArg::from_str(value, true)
        .map(StatusArg::status)
        .map_err(|_| {
            ExternalError::invalid(format!(
                "{value:?} is not a task status. Use triage, backlog, todo, in-progress, done or canceled."
            ))
        })
}

fn project_status(value: &str) -> Result<ProjectStatus, ExternalError> {
    serde_json::from_value(Value::String(value.to_lowercase())).map_err(|_| {
        ExternalError::invalid(format!(
            "{value:?} is not a project status. Use backlog, planned, active, paused, completed or canceled."
        ))
    })
}

fn picked(value: Option<&String>) -> Option<&str> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

fn event(
    args: &LoopSaveArgs,
    board: &Board,
    on: EventArg,
    old: Option<&LoopTrigger>,
) -> Result<LoopTrigger, ExternalError> {
    if args.to.is_some() && !matches!(on, EventArg::StatusChanged | EventArg::ProjectStatusChanged)
    {
        return Err(ExternalError::invalid(
            "--to goes with --on status-changed or project-status-changed.",
        ));
    }
    if args.label.is_some() && on != EventArg::LabelAdded {
        return Err(ExternalError::invalid(
            "--label goes with --on label-added.",
        ));
    }
    Ok(match on {
        EventArg::TaskCreated => LoopTrigger::TaskCreated,
        EventArg::Assigned => LoopTrigger::Assigned,
        EventArg::CommentAdded => LoopTrigger::CommentAdded,
        EventArg::MeetingEnded => LoopTrigger::MeetingEnded,
        EventArg::StatusChanged => LoopTrigger::StatusChanged {
            to: match (&args.to, old) {
                (Some(_), _) => picked(args.to.as_ref()).map(task_status).transpose()?,
                (None, Some(LoopTrigger::StatusChanged { to })) => *to,
                (None, _) => None,
            },
        },
        EventArg::ProjectStatusChanged => LoopTrigger::ProjectStatusChanged {
            to: match (&args.to, old) {
                (Some(_), _) => picked(args.to.as_ref()).map(project_status).transpose()?,
                (None, Some(LoopTrigger::ProjectStatusChanged { to })) => *to,
                (None, _) => None,
            },
        },
        EventArg::LabelAdded => LoopTrigger::LabelAdded {
            label_id: match (&args.label, old) {
                (Some(_), _) => match picked(args.label.as_ref()) {
                    Some(label) => board.label_ids(&[label.to_owned()])?.into_iter().next(),
                    None => None,
                },
                (None, Some(LoopTrigger::LabelAdded { label_id })) => label_id.clone(),
                (None, _) => None,
            },
        },
    })
}

fn schedule(
    args: &LoopSaveArgs,
    every: LoopEvery,
    old: Option<&LoopTrigger>,
) -> Result<LoopTrigger, ExternalError> {
    if args.to.is_some() || args.label.is_some() {
        return Err(ExternalError::invalid("--to and --label go with --on."));
    }
    let (old_zone, old_first, old_days) = match old {
        Some(LoopTrigger::Schedule {
            timezone,
            first_run_at_ms,
            days,
            ..
        }) => (Some(timezone.clone()), Some(*first_run_at_ms), days.clone()),
        _ => (None, None, Vec::new()),
    };
    let timezone = args
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|zone| !zone.is_empty())
        .map(str::to_owned)
        .or(old_zone)
        .or_else(|| iana_time_zone::get_timezone().ok())
        .ok_or_else(|| ExternalError::invalid("Give the schedule's time zone with --timezone."))?;
    let first_run_at_ms = match &args.first_run {
        Some(local) => loops::first_run_ms(&timezone, local).map_err(ExternalError::invalid)?,
        None => old_first.ok_or_else(|| {
            ExternalError::invalid("A schedule needs --first-run \"YYYY-MM-DD HH:MM\".")
        })?,
    };
    let days = match &args.days {
        Some(days) => listed(days).map(day).collect::<Result<Vec<_>, _>>()?,
        None => old_days,
    };
    Ok(LoopTrigger::Schedule {
        every,
        days,
        timezone,
        first_run_at_ms,
    })
}

fn trigger(
    args: &LoopSaveArgs,
    board: &Board,
    old: Option<&LoopTrigger>,
) -> Result<LoopTrigger, ExternalError> {
    let schedule_touched =
        args.first_run.is_some() || args.timezone.is_some() || args.days.is_some();
    if let Some(on) = args.on {
        if schedule_touched {
            return Err(ExternalError::invalid(
                "--first-run, --timezone and --days go with --schedule.",
            ));
        }
        return event(args, board, on, old);
    }
    if let Some(every) = args.schedule {
        return schedule(args, every.every(), old);
    }
    match old {
        Some(LoopTrigger::Schedule { every, .. })
            if schedule_touched || args.to.is_some() || args.label.is_some() =>
        {
            schedule(args, *every, old)
        }
        Some(old) => match EventArg::of(old) {
            Some(_) if schedule_touched => Err(ExternalError::invalid(
                "--first-run, --timezone and --days go with --schedule.",
            )),
            Some(on) if args.to.is_some() || args.label.is_some() => {
                event(args, board, on, Some(old))
            }
            _ => Ok(old.clone()),
        },
        None => Err(ExternalError::invalid(
            "A new automation needs a trigger: --schedule with --first-run, or --on.",
        )),
    }
}

fn conditions(
    args: &LoopSaveArgs,
    board: &Board,
    old: Option<&LoopConditions>,
) -> Result<LoopConditions, ExternalError> {
    let old = old.cloned().unwrap_or_default();
    Ok(LoopConditions {
        labels: match &args.if_label {
            Some(labels) => board.label_ids(labels)?,
            None => old.labels,
        },
        projects: match &args.if_project {
            Some(projects) => listed(projects)
                .filter_map(|project| board.project(project).transpose())
                .map(|project| project.map(|project| project.id.clone()))
                .collect::<Result<Vec<_>, _>>()?,
            None => old.projects,
        },
        statuses: match &args.if_status {
            Some(statuses) => listed(statuses)
                .map(task_status)
                .collect::<Result<Vec<_>, _>>()?,
            None => old.statuses,
        },
    })
}

impl Tracker<'_> {
    fn all_loops(&self) -> Result<Vec<WorkLoop>, ExternalError> {
        planning::history(self.history, |conn| loops::loops(conn))
            .map_err(|message| refusal(self.history, message))
    }

    fn loop_named<'l>(all: &'l [WorkLoop], value: &str) -> Result<&'l WorkLoop, ExternalError> {
        pick("automation", value, all, |item| &item.id, |item| &item.name)
            .map_err(|refusal| refusal.hint("List them with: sona work automation-list"))
    }

    fn rehearse_loop<T>(
        &self,
        write: impl FnOnce(&Transaction<'_>, i64) -> anyhow::Result<T>,
    ) -> Result<T, ExternalError> {
        self.history
            .with_connection(|conn| {
                let tx = conn.transaction()?;
                write(&tx, now_ms())
            })
            .map_err(|error| refusal(self.history, loops::message(error)))
    }

    pub(super) fn loop_list(&self) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::AgentLoops(ExternalAgentLoops {
            schema_version: QUERY_SCHEMA_VERSION,
            automations: self.all_loops()?,
            tools: loops::tools(),
        }))
    }

    pub(super) fn loop_show(&self, args: &LoopShowArgs) -> Result<WorkAnswer, ExternalError> {
        let all = self.all_loops()?;
        let found = Self::loop_named(&all, &args.name)?;
        let detail = planning::history(self.history, |conn| loops::detail(conn, &found.id))
            .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::AgentLoop(Box::new(ExternalAgentLoop {
            schema_version: QUERY_SCHEMA_VERSION,
            detail,
        })))
    }

    pub(super) async fn loop_save(&self, args: &LoopSaveArgs) -> Result<WorkAnswer, ExternalError> {
        let name = args.name.trim();
        if name.is_empty() {
            return Err(ExternalError::invalid("An automation needs a name."));
        }
        let board = self.board().await?;
        let all = self.all_loops()?;
        let existing = match &args.target {
            Some(value) => Some(Self::loop_named(&all, value)?),
            None => all
                .iter()
                .find(|item| item.name.to_lowercase() == name.to_lowercase()),
        };
        let prompt = match (&args.prompt, existing) {
            (Some(prompt), _) => prompt.clone(),
            (None, Some(existing)) => existing.prompt.clone(),
            (None, None) => return Err(ExternalError::invalid("A new automation needs --prompt.")),
        };
        let input = LoopInput {
            name: name.to_owned(),
            prompt,
            trigger: trigger(args, &board, existing.map(|item| &item.trigger))?,
            conditions: conditions(args, &board, existing.map(|item| &item.conditions))?,
            tools: match &args.tools {
                Some(tools) => listed(tools).map(str::to_owned).collect(),
                None => existing.map(|item| item.tools.clone()).unwrap_or_default(),
            },
        };
        let id = existing.map(|item| item.id.as_str());
        let operation_id = match existing {
            Some(item) => args
                .write
                .operation_id("automation-save", &(&item.id, item.version, &input))?,
            None => args.write.fresh_operation_id(),
        };
        let by = args.write.agent();
        let saved = if args.dry_run {
            self.rehearse_loop(|tx, at| loops::save(tx, id, &input, &by, at))?
        } else {
            self.applies("saving an automation")?;
            loops::save_as(self.app, &operation_id, id, &input, &by)
                .map_err(|message| self.write_refusal(message, &operation_id, "The automation"))?
        };
        let action = match existing {
            Some(item) if item.version == saved.version => LoopAction::Unchanged,
            _ => LoopAction::Saved,
        };
        Ok(self.loop_change(action, args.dry_run, operation_id, saved))
    }

    fn loop_change(
        &self,
        action: LoopAction,
        dry_run: bool,
        operation_id: String,
        item: WorkLoop,
    ) -> WorkAnswer {
        WorkAnswer::AgentLoopChange(Box::new(ExternalAgentLoopChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id,
            item,
        }))
    }

    pub(super) fn loop_run(&self, args: &LoopArgs) -> Result<WorkAnswer, ExternalError> {
        let all = self.all_loops()?;
        let found = Self::loop_named(&all, &args.name)?;
        let operation_id = args.write.fresh_operation_id();
        let run = if args.dry_run {
            self.rehearse_loop(|tx, at| loops::run_now(tx, &found.id, &operation_id, at))?
        } else {
            self.applies("running an automation")?;
            loops::run_now_as(self.app, &operation_id, &found.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The automation"))?
        };
        Ok(WorkAnswer::AgentLoopRun(Box::new(ExternalAgentLoopRun {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id,
            run,
        })))
    }

    pub(super) fn loop_enable(
        &self,
        args: &LoopArgs,
        enabled: bool,
    ) -> Result<WorkAnswer, ExternalError> {
        let all = self.all_loops()?;
        let found = Self::loop_named(&all, &args.name)?;
        let operation_id = args.write.operation_id(
            "automation-enable",
            &(&found.id, found.version, found.updated_at_ms, enabled),
        )?;
        let item = if args.dry_run {
            self.rehearse_loop(|tx, at| loops::enable(tx, &found.id, enabled, at))?
        } else {
            self.applies(if enabled {
                "turning an automation on"
            } else {
                "turning an automation off"
            })?;
            loops::enable_as(self.app, &operation_id, &found.id, enabled)
                .map_err(|message| self.write_refusal(message, &operation_id, "The automation"))?
        };
        let action = if enabled {
            LoopAction::Enabled
        } else {
            LoopAction::Disabled
        };
        Ok(self.loop_change(action, args.dry_run, operation_id, item))
    }

    pub(super) fn loop_restore(&self, args: &LoopRestoreArgs) -> Result<WorkAnswer, ExternalError> {
        let all = self.all_loops()?;
        let found = Self::loop_named(&all, &args.name)?;
        let operation_id = args.write.operation_id(
            "automation-restore-version",
            &(&found.id, found.version, args.version),
        )?;
        let by = args.write.agent();
        let item = if args.dry_run {
            self.rehearse_loop(|tx, at| loops::restore(tx, &found.id, args.version, &by, at))?
        } else {
            self.applies("restoring an automation")?;
            loops::restore_as(self.app, &operation_id, &found.id, args.version, &by)
                .map_err(|message| self.write_refusal(message, &operation_id, "The automation"))?
        };
        let action = if item.version == found.version {
            LoopAction::Unchanged
        } else {
            LoopAction::Restored
        };
        Ok(self.loop_change(action, args.dry_run, operation_id, item))
    }

    pub(super) fn loop_delete(&self, args: &LoopArgs) -> Result<WorkAnswer, ExternalError> {
        let all = self.all_loops()?;
        let found = Self::loop_named(&all, &args.name)?;
        let operation_id = args
            .write
            .operation_id("automation-delete", &(&found.id, found.version))?;
        if !args.dry_run {
            self.applies("deleting an automation")?;
            loops::delete_as(self.app, &operation_id, &found.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The automation"))?;
        }
        Ok(self.loop_change(
            LoopAction::Deleted,
            args.dry_run,
            operation_id,
            found.clone(),
        ))
    }
}

fn day_word(day: LoopDay) -> &'static str {
    match day {
        LoopDay::Mon => "Mon",
        LoopDay::Tue => "Tue",
        LoopDay::Wed => "Wed",
        LoopDay::Thu => "Thu",
        LoopDay::Fri => "Fri",
        LoopDay::Sat => "Sat",
        LoopDay::Sun => "Sun",
    }
}

fn trigger_words(trigger: &LoopTrigger) -> String {
    match trigger {
        LoopTrigger::Schedule {
            every,
            days,
            timezone,
            first_run_at_ms,
        } => {
            let every = match every {
                LoopEvery::Hourly => "Every hour".to_owned(),
                LoopEvery::Daily => "Every day".to_owned(),
                LoopEvery::Weekly => format!(
                    "Every {}",
                    days.iter()
                        .copied()
                        .map(day_word)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                LoopEvery::Monthly => "Every month".to_owned(),
            };
            format!(
                "{every}, from {} ({timezone})",
                local_time(*first_run_at_ms)
            )
        }
        LoopTrigger::TaskCreated => "When a task is created".to_owned(),
        LoopTrigger::StatusChanged { to: Some(to) } => {
            format!("When a task moves to {}", loops::status_words(to.as_str()))
        }
        LoopTrigger::StatusChanged { to: None } => "When a task changes status".to_owned(),
        LoopTrigger::LabelAdded { .. } => "When a task gets a label".to_owned(),
        LoopTrigger::Assigned => "When a task gets an owner or a delegate".to_owned(),
        LoopTrigger::CommentAdded => "When a task gets a comment".to_owned(),
        LoopTrigger::ProjectStatusChanged { to: Some(to) } => {
            format!(
                "When a project moves to {}",
                loops::status_words(to.as_str())
            )
        }
        LoopTrigger::ProjectStatusChanged { to: None } => {
            "When a project changes status".to_owned()
        }
        LoopTrigger::MeetingEnded => "When a meeting ends".to_owned(),
    }
}

const fn status_word(status: LoopRunStatus) -> &'static str {
    match status {
        LoopRunStatus::Queued => "queued",
        LoopRunStatus::Running => "running",
        LoopRunStatus::Done => "done",
        LoopRunStatus::Failed => "failed",
        LoopRunStatus::Skipped => "skipped",
    }
}

fn loop_line(text: &mut String, item: &WorkLoop) {
    let state = if item.enabled { "" } else { " (off)" };
    let _ = writeln!(
        text,
        "{}{state}: {}. Version {}.",
        item.name,
        trigger_words(&item.trigger),
        item.version
    );
    if let Some(next) = item.next_run_at_ms {
        let _ = writeln!(text, "  Next run {}.", local_time(next));
    }
    if let Some(run) = &item.last_run {
        let _ = writeln!(
            text,
            "  Last run {} ({}), {}.",
            status_word(run.status),
            run.cause,
            local_time(run.queued_at_ms)
        );
    }
}

fn run_line(text: &mut String, run: &WorkLoopRun) {
    let _ = writeln!(
        text,
        "- {} {}: {}",
        local_time(run.queued_at_ms),
        status_word(run.status),
        run.cause
    );
    if let Some(said) = run.output.as_deref().or(run.error.as_deref()) {
        let _ = writeln!(text, "  {said}");
    }
    if !run.refused_tools.is_empty() {
        let _ = writeln!(text, "  Refused: {}", run.refused_tools.join(", "));
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::AgentLoops(page) => {
            if page.automations.is_empty() {
                text.push_str("No automations.\n");
            }
            for item in &page.automations {
                loop_line(text, item);
            }
        }
        WorkAnswer::AgentLoop(shown) => {
            let detail = &shown.detail;
            loop_line(text, &detail.item);
            let _ = writeln!(text, "Prompt: {}", detail.item.prompt);
            if !detail.item.tools.is_empty() {
                let _ = writeln!(text, "Tools: {}", detail.item.tools.join(", "));
            }
            text.push_str("Versions:\n");
            for version in &detail.versions {
                let restored = version
                    .restored_from
                    .map(|from| format!(", restored from {from}"))
                    .unwrap_or_default();
                let _ = writeln!(
                    text,
                    "- {} by {}, {}{restored}",
                    version.version,
                    version.saved_by,
                    local_time(version.saved_at_ms)
                );
            }
            if detail.runs.is_empty() {
                text.push_str("No runs yet.\n");
            } else {
                text.push_str("Runs:\n");
            }
            for run in &detail.runs {
                run_line(text, run);
            }
        }
        WorkAnswer::AgentLoopChange(change) => {
            let verb = match (change.action, change.dry_run) {
                (LoopAction::Saved, false) => "Saved",
                (LoopAction::Saved, true) => "Would save",
                (LoopAction::Unchanged, _) => "Unchanged",
                (LoopAction::Enabled, false) => "Turned on",
                (LoopAction::Enabled, true) => "Would turn on",
                (LoopAction::Disabled, false) => "Turned off",
                (LoopAction::Disabled, true) => "Would turn off",
                (LoopAction::Restored, false) => "Restored",
                (LoopAction::Restored, true) => "Would restore",
                (LoopAction::Deleted, false) => "Deleted",
                (LoopAction::Deleted, true) => "Would delete",
            };
            let _ = write!(text, "{verb} ");
            loop_line(text, &change.item);
        }
        WorkAnswer::AgentLoopRun(queued) => {
            let verb = if queued.dry_run {
                "Would queue"
            } else {
                "Queued"
            };
            let _ = writeln!(
                text,
                "{verb} a run. The running Sona app starts it within seconds; see it with: sona work automation-show {:?}",
                queued.run.loop_id
            );
        }
        _ => {}
    }
}
