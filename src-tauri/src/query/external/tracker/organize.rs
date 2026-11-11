//! `sona work` verbs for handing work to an agent, the day's focus, month-old
//! work kept as it is, meeting briefs and talking points, meeting loops and
//! the actions they left untracked, and renaming an attachment.

use super::attachments::{project_attachment, ExternalWorkAttachment};
use super::{
    conversations_closed, day, now_ms, refusal, task_ref, task_ref_of, ExternalWorkRef, Fence,
    TaskRef, Tracker, WorkAnswer, WriteArgs,
};
use crate::meeting::loop_types::{
    MeetingLoopAssignRequest, MeetingLoopId, MeetingLoopReopenRequest, MeetingLoopRow,
};
use crate::meeting::types::MeetingOperationId;
use crate::planning;
use crate::planning::untracked as untracked_actions;
use crate::planning::work::comments::Author;
use crate::planning::work::types::WorkActor;
use crate::planning::work::{self, agents, follow, items};
use crate::query::external::{
    command_error, kept_local, loop_id, missing_loop, person_id, store, ExternalError,
    ExternalErrorCode,
};
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Local, NaiveDate};
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;
use tauri::Manager;
use uuid::Uuid;

fn loop_ref(value: &str) -> Result<MeetingLoopId, String> {
    loop_id(value).map_err(|error| error.message)
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DelegateArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// The agent to hand it to: "Claude Code", "Codex".
    #[arg(long, value_name = "AGENT", required_unless_present = "take_back")]
    pub to: Option<String>,
    /// Take the task back from the agent working it.
    #[arg(long, conflicts_with = "to")]
    pub take_back: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DayArgs {
    /// YYYY-MM-DD, today or tomorrow.
    #[arg(value_parser = day, default_value = "today", value_name = "DAY")]
    pub day: NaiveDate,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FocusArgs {
    /// Up to three tasks, in order: S-12, 12, or ids. They replace the day's focus.
    #[arg(value_parser = task_ref, value_name = "TASK", required_unless_present = "clear")]
    pub tasks: Vec<TaskRef>,
    /// Empty the day's focus.
    #[arg(long, conflicts_with = "tasks")]
    pub clear: bool,
    /// YYYY-MM-DD, today or tomorrow.
    #[arg(long, value_parser = day, default_value = "today", value_name = "DAY")]
    pub day: NaiveDate,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct BriefArgs {
    /// The person's name or id.
    #[arg(value_name = "PERSON")]
    pub person: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TalkingPointArgs {
    /// The person's name or id.
    #[arg(value_name = "PERSON")]
    pub person: String,
    /// What to raise with them, up to 500 characters.
    #[arg(required = true, num_args = 1.., value_name = "TEXT")]
    pub text: Vec<String>,
    /// Check it and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TalkingPointDoneArgs {
    /// The talking point's id, from `brief`.
    #[arg(value_name = "ID")]
    pub id: Uuid,
    /// Mark it not done again.
    #[arg(long)]
    pub undo: bool,
    /// Check it and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LoopArgs {
    /// The loop's id, from `sona --loops` or `work meeting`.
    #[arg(value_parser = loop_ref, value_name = "LOOP_ID")]
    pub loop_id: MeetingLoopId,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LoopOwnerArgs {
    /// The loop's id, from `sona --loops` or `work meeting`.
    #[arg(value_parser = loop_ref, value_name = "LOOP_ID")]
    pub loop_id: MeetingLoopId,
    /// A person's name or id, or none to clear the owner.
    #[arg(long, value_name = "PERSON")]
    pub owner: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ActionTrackArgs {
    /// The loop's id, from `sona --loops` or `work meeting`.
    #[arg(value_parser = loop_ref, value_name = "LOOP_ID")]
    pub loop_id: MeetingLoopId,
    /// The project to track it in, by name or id.
    #[arg(long)]
    pub project: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct AttachmentRenameArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// The attachment's id, from `attachments`.
    #[arg(value_name = "ID")]
    pub attachment: Uuid,
    /// The new title.
    #[arg(long)]
    pub title: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizeAction {
    Delegated,
    TookBack,
    Kept,
    TalkingPointAdded,
    TalkingPointDone,
    TalkingPointReopened,
    LoopReopened,
    LoopOwnerSet,
    ActionTracked,
    ActionDismissed,
    ActionRestored,
    AttachmentRenamed,
}

impl OrganizeAction {
    const fn text(self, dry_run: bool) -> &'static str {
        match (self, dry_run) {
            (Self::Delegated, false) => "Handed to an agent",
            (Self::Delegated, true) => "Would hand to an agent",
            (Self::TookBack, false) => "Took back",
            (Self::TookBack, true) => "Would take back",
            (Self::Kept, false) => "Kept as it is",
            (Self::Kept, true) => "Would keep as it is",
            (Self::TalkingPointAdded, false) => "Added a talking point",
            (Self::TalkingPointAdded, true) => "Would add a talking point",
            (Self::TalkingPointDone, false) => "Marked the talking point done",
            (Self::TalkingPointDone, true) => "Would mark the talking point done",
            (Self::TalkingPointReopened, false) => "Marked the talking point not done",
            (Self::TalkingPointReopened, true) => "Would mark the talking point not done",
            (Self::LoopReopened, false) => "Reopened the loop",
            (Self::LoopReopened, true) => "Would reopen the loop",
            (Self::LoopOwnerSet, false) => "Set the loop's owner",
            (Self::LoopOwnerSet, true) => "Would set the loop's owner",
            (Self::ActionTracked, false) => "Tracked the action",
            (Self::ActionTracked, true) => "Would track the action",
            (Self::ActionDismissed, false) => "Set the action aside as not work",
            (Self::ActionDismissed, true) => "Would set the action aside as not work",
            (Self::ActionRestored, false) => "Asked about the action again",
            (Self::ActionRestored, true) => "Would ask about the action again",
            (Self::AttachmentRenamed, false) => "Renamed the attachment",
            (Self::AttachmentRenamed, true) => "Would rename the attachment",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDoer {
    pub agent: String,
    pub since_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTalkingPoint {
    pub id: String,
    pub text: String,
    pub created_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalLoopState {
    pub loop_id: String,
    pub text: String,
    pub status: String,
    pub owner_person_id: Option<String>,
    pub owner: Option<String>,
}

fn loop_state(row: &MeetingLoopRow) -> ExternalLoopState {
    ExternalLoopState {
        loop_id: row.loop_id.as_str().to_owned(),
        text: row.text.clone(),
        status: row.status.as_str().to_owned(),
        owner_person_id: row.owner_person_id.map(|id| id.uuid().to_string()),
        owner: row.owner_display_name.clone(),
    }
}

/// One change from these verbs. The fields that do not apply are absent.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalOrganizeChange {
    pub schema_version: u32,
    pub action: OrganizeAction,
    pub dry_run: bool,
    /// Absent on a dry run that names none.
    pub operation_id: Option<String>,
    /// True when this answered from an earlier run with the same operation
    /// id, or nothing was left to change.
    pub replayed: bool,
    pub tasks: Vec<ExternalWorkRef>,
    pub doer: Option<ExternalDoer>,
    /// What to paste into the agent the task went to.
    pub prompt: Option<String>,
    pub person_id: Option<String>,
    pub talking_point: Option<ExternalTalkingPoint>,
    pub meeting_loop: Option<ExternalLoopState>,
    pub attachment: Option<ExternalWorkAttachment>,
}

impl ExternalOrganizeChange {
    fn new(action: OrganizeAction, dry_run: bool, operation_id: Option<String>) -> Self {
        Self {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id,
            replayed: false,
            tasks: Vec::new(),
            doer: None,
            prompt: None,
            person_id: None,
            talking_point: None,
            meeting_loop: None,
            attachment: None,
        }
    }
}

/// A day's focus, in order.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkDay {
    pub schema_version: u32,
    pub day: String,
    pub focus: Vec<ExternalWorkRef>,
    pub planned_at_ms: Option<i64>,
    pub dry_run: bool,
    /// Present on a change.
    pub operation_id: Option<String>,
    pub replayed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalBriefPerson {
    pub person_id: String,
    pub name: String,
    /// My open work from meetings they were in.
    pub you_owe: Vec<ExternalWorkRef>,
    /// Their open work.
    pub they_owe: Vec<ExternalWorkRef>,
    pub talking_points: Vec<ExternalTalkingPoint>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalBriefMeeting {
    pub meeting_id: String,
    pub title: String,
    pub at_ms: i64,
    pub decisions: Vec<String>,
}

/// One screen before talking to someone.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkBrief {
    pub schema_version: u32,
    pub title: String,
    pub people: Vec<ExternalBriefPerson>,
    pub last_time: Option<ExternalBriefMeeting>,
}

fn talking_point(point: follow::WorkTalkingPoint) -> ExternalTalkingPoint {
    ExternalTalkingPoint {
        id: point.id,
        text: point.text,
        created_at_ms: Some(point.created_at_ms),
    }
}

fn today() -> String {
    Local::now().date_naive().format("%Y-%m-%d").to_string()
}

impl Tracker<'_> {
    /// Whether the planning ledger already holds `operation_id`: a write
    /// under it answers from that run.
    pub(super) fn ran(&self, operation_id: &str) -> Result<bool, ExternalError> {
        planning::history(self.history, |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM planning_operations WHERE operation_id = ?1)",
                [operation_id],
                |row| row.get::<_, bool>(0),
            )?)
        })
        .map_err(|message| refusal(self.history, message))
    }

    pub(super) async fn delegate(&self, args: &DelegateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let agent = args.to.as_deref().map(|name| agents::agent_name(Some(name)));
        let action = if agent.is_some() {
            OrganizeAction::Delegated
        } else {
            OrganizeAction::TookBack
        };
        let current = row.doer.as_ref().map(|doer| doer.agent.as_str());
        let unchanged = current == agent.as_deref();
        if args.dry_run {
            let mut change = ExternalOrganizeChange::new(
                action,
                true,
                args.write.operation_id.map(|id| id.to_string()),
            );
            change.replayed = unchanged;
            change.tasks.push(task_ref_of(row));
            change.doer = agent.map(|agent| ExternalDoer {
                agent,
                since_ms: now_ms(),
            });
            return Ok(WorkAnswer::Organize(Box::new(change)));
        }
        let operation_id = args.write.fresh_operation_id();
        let replayed = unchanged || self.ran(&operation_id)?;
        self.applies("handing a task to an agent")?;
        let delegation = agents::work_delegate(
            self.app.clone(),
            operation_id.clone(),
            row.item.id.clone(),
            agent,
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, "The task"))?;
        let mut change = ExternalOrganizeChange::new(action, false, Some(operation_id));
        change.replayed = replayed;
        change.tasks.push(task_ref_of(row));
        change.doer = delegation.doer.map(|doer| ExternalDoer {
            agent: doer.agent,
            since_ms: doer.since_ms,
        });
        change.prompt = delegation.prompt;
        Ok(WorkAnswer::Organize(Box::new(change)))
    }

    pub(super) async fn day(&self, args: &DayArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let day = args.day.format("%Y-%m-%d").to_string();
        let plan = planning::history(self.history, |conn| items::day(conn, &day))
            .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::Day(ExternalWorkDay {
            schema_version: QUERY_SCHEMA_VERSION,
            focus: plan
                .focus_ids
                .iter()
                .filter_map(|id| board.row(id).map(task_ref_of))
                .collect(),
            day: plan.day,
            planned_at_ms: plan.planned_at_ms,
            dry_run: false,
            operation_id: None,
            replayed: false,
        }))
    }

    pub(super) async fn focus(&self, args: &FocusArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let ids: Vec<String> = board
            .tasks(&args.tasks)?
            .into_iter()
            .map(|row| row.item.id.clone())
            .collect();
        let day = args.day.format("%Y-%m-%d").to_string();
        let before = planning::history(self.history, |conn| items::day(conn, &day))
            .map_err(|message| refusal(self.history, message))?;
        let (plan, operation_id, replayed) = if args.dry_run {
            let plan = self.rehearse(|conn| items::plan_day(conn, &day, &ids, now_ms()))?;
            (
                plan,
                args.write.operation_id.map(|id| id.to_string()),
                before.focus_ids == ids,
            )
        } else {
            let operation_id = args.write.fresh_operation_id();
            let replayed = before.focus_ids == ids || self.ran(&operation_id)?;
            self.applies("planning the day")?;
            let plan = work::work_day_plan(
                self.app.state(),
                operation_id.clone(),
                day.clone(),
                ids,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "The day"))?;
            (plan, Some(operation_id), replayed)
        };
        Ok(WorkAnswer::Day(ExternalWorkDay {
            schema_version: QUERY_SCHEMA_VERSION,
            focus: plan
                .focus_ids
                .iter()
                .filter_map(|id| board.row(id).map(task_ref_of))
                .collect(),
            day: plan.day,
            planned_at_ms: plan.planned_at_ms,
            dry_run: args.dry_run,
            operation_id,
            replayed,
        }))
    }

    pub(super) async fn keep(&self, args: &super::TasksArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let rows = board.tasks(&args.tasks)?;
        let ids: Vec<String> = rows.iter().map(|row| row.item.id.clone()).collect();
        let operation_id = args.write.operation_id("keep", &(&ids, today()))?;
        let mut change = ExternalOrganizeChange::new(
            OrganizeAction::Kept,
            args.dry_run,
            Some(operation_id.clone()),
        );
        if args.dry_run {
            for id in &ids {
                self.rehearse(|conn| items::keep(conn, id, WorkActor::Agent, now_ms()))?;
            }
            change.replayed = self.ran(&operation_id)?;
            change.tasks = rows.into_iter().map(task_ref_of).collect();
            return Ok(WorkAnswer::Organize(Box::new(change)));
        }
        change.replayed = self.ran(&operation_id)?;
        self.applies("keeping a task as it is")?;
        let started_ms = now_ms();
        let kept = work::keep_as(self.app, WorkActor::Agent, &operation_id, &ids)
            .map_err(|message| self.write_refusal(message, &operation_id, "One of these tasks"))?;
        self.credit(&args.write, &ids, started_ms)?;
        change.tasks = kept
            .iter()
            .filter_map(|item| board.row(&item.id))
            .map(task_ref_of)
            .collect();
        Ok(WorkAnswer::Organize(Box::new(change)))
    }

    pub(super) async fn brief(&self, args: &BriefArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let person = board.person(&args.person)?;
        let Fence::Open(kept_local) = &board.fence else {
            return Err(conversations_closed());
        };
        let brief = follow::person_brief(
            self.history,
            self.meetings,
            person.id.clone(),
            kept_local.sessions(),
        )
        .await
        .map_err(|message| refusal(self.history, message))?;
        let refs = |ids: Vec<String>| -> Vec<ExternalWorkRef> {
            ids.iter()
                .filter_map(|id| board.row(id).map(task_ref_of))
                .collect()
        };
        Ok(WorkAnswer::Brief(Box::new(ExternalWorkBrief {
            schema_version: QUERY_SCHEMA_VERSION,
            title: brief.title,
            people: brief
                .people
                .into_iter()
                .map(|person| ExternalBriefPerson {
                    person_id: person.person_id,
                    name: person.name,
                    you_owe: refs(person.you_owe),
                    they_owe: refs(person.they_owe),
                    talking_points: person
                        .talking_points
                        .into_iter()
                        .map(talking_point)
                        .collect(),
                })
                .collect(),
            last_time: brief.last_time.map(|meeting| ExternalBriefMeeting {
                meeting_id: meeting.meeting_id,
                title: meeting.title,
                at_ms: meeting.at_ms,
                decisions: meeting
                    .decisions
                    .into_iter()
                    .map(|decision| decision.text)
                    .collect(),
            }),
        })))
    }

    pub(super) async fn talking_point(
        &self,
        args: &TalkingPointArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let person = board.person(&args.person)?;
        let text = args.text.join(" ").trim().to_owned();
        let operation_id = args
            .write
            .operation_id("talking-point", &(&person.id, &text, today()))?;
        let mut change = ExternalOrganizeChange::new(
            OrganizeAction::TalkingPointAdded,
            args.dry_run,
            Some(operation_id.clone()),
        );
        change.person_id = Some(person.id.clone());
        change.replayed = self.ran(&operation_id)?;
        if args.dry_run {
            change.talking_point = Some(ExternalTalkingPoint {
                id: String::new(),
                text,
                created_at_ms: None,
            });
            return Ok(WorkAnswer::Organize(Box::new(change)));
        }
        self.applies("adding a talking point")?;
        let point = follow::work_talking_point_add(
            self.app.clone(),
            self.app.state(),
            self.app.state(),
            operation_id.clone(),
            person.id.clone(),
            text,
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, "The talking point"))?;
        change.talking_point = Some(talking_point(point));
        Ok(WorkAnswer::Organize(Box::new(change)))
    }

    pub(super) fn talking_point_done(
        &self,
        args: &TalkingPointDoneArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let id = args.id.to_string();
        let found: Option<(String, bool)> = planning::history(self.history, |conn| {
            use rusqlite::OptionalExtension as _;
            Ok(conn
                .query_row(
                    "SELECT text, done_at_ms IS NOT NULL FROM planning_talking_points WHERE id = ?1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
        })
        .map_err(|message| refusal(self.history, message))?;
        let (text, was_done) = found.ok_or_else(|| {
            ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No talking point {id}."),
            )
        })?;
        let done = !args.undo;
        let action = if done {
            OrganizeAction::TalkingPointDone
        } else {
            OrganizeAction::TalkingPointReopened
        };
        let operation_id = if args.dry_run {
            args.write.operation_id.map(|id| id.to_string())
        } else {
            Some(args.write.fresh_operation_id())
        };
        let mut change = ExternalOrganizeChange::new(action, args.dry_run, operation_id.clone());
        change.talking_point = Some(ExternalTalkingPoint {
            id: id.clone(),
            text,
            created_at_ms: None,
        });
        change.replayed = was_done == done;
        if let (false, Some(operation_id)) = (args.dry_run, operation_id) {
            change.replayed = change.replayed || self.ran(&operation_id)?;
            self.applies("marking a talking point")?;
            follow::work_talking_point_done(self.app.clone(), operation_id.clone(), id, done)
                .map_err(|message| {
                    self.write_refusal(message, &operation_id, "The talking point")
                })?;
        }
        Ok(WorkAnswer::Organize(Box::new(change)))
    }

    async fn loop_row(&self, id: &MeetingLoopId) -> Result<MeetingLoopRow, ExternalError> {
        let session_id = id
            .session_id()
            .ok_or_else(|| ExternalError::invalid("That loop id names no meeting."))?;
        if kept_local(store(self.meetings).await?.as_ref())?.hides(session_id) {
            return Err(missing_loop(id));
        }
        self.meetings
            .loops_list(session_id)
            .await
            .map_err(command_error(session_id.uuid()))?
            .rows
            .into_iter()
            .find(|row| &row.loop_id == id)
            .ok_or_else(|| missing_loop(id))
    }

    fn meeting_operation(write: &WriteArgs, verb: &str, row: &MeetingLoopRow) -> Uuid {
        write.operation_id.unwrap_or_else(|| {
            let mut name = verb.as_bytes().to_vec();
            name.extend_from_slice(&row.revision.to_be_bytes());
            name.extend_from_slice(row.loop_id.as_str().as_bytes());
            Uuid::new_v5(&super::OPERATION_NAMESPACE, &name)
        })
    }

    pub(super) async fn loop_reopen(&self, args: &LoopArgs) -> Result<WorkAnswer, ExternalError> {
        let row = self.loop_row(&args.loop_id).await?;
        let operation_id = Self::meeting_operation(&args.write, "loop-reopen", &row);
        let mut change = ExternalOrganizeChange::new(
            OrganizeAction::LoopReopened,
            args.dry_run,
            Some(operation_id.to_string()),
        );
        if row.is_open() || args.dry_run {
            change.replayed = row.is_open();
            change.meeting_loop = Some(loop_state(&row));
            return Ok(WorkAnswer::Organize(Box::new(change)));
        }
        self.applies("reopening a meeting loop")?;
        let session_id = row.session_id;
        let result = self
            .meetings
            .loop_reopen(MeetingLoopReopenRequest {
                operation_id: MeetingOperationId::from_uuid(operation_id),
                loop_id: row.loop_id.clone(),
                expected_revision: row.revision,
            })
            .await
            .map_err(command_error(session_id.uuid()))?;
        change.meeting_loop = result
            .loops
            .rows
            .iter()
            .find(|found| found.loop_id == row.loop_id)
            .map(loop_state);
        Ok(WorkAnswer::Organize(Box::new(change)))
    }

    pub(super) async fn loop_owner(
        &self,
        args: &LoopOwnerArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let row = self.loop_row(&args.loop_id).await?;
        let owner = if args.owner.trim().eq_ignore_ascii_case("none") {
            None
        } else {
            let board = self.board().await?;
            Some(person_id(&board.person(&args.owner)?.id)?)
        };
        let operation_id = Self::meeting_operation(&args.write, "loop-owner", &row);
        let mut change = ExternalOrganizeChange::new(
            OrganizeAction::LoopOwnerSet,
            args.dry_run,
            Some(operation_id.to_string()),
        );
        let unchanged = row.owner_person_id == owner;
        if unchanged || args.dry_run {
            change.replayed = unchanged;
            let mut state = loop_state(&row);
            state.owner_person_id = owner.map(|id| id.uuid().to_string());
            change.meeting_loop = Some(state);
            return Ok(WorkAnswer::Organize(Box::new(change)));
        }
        self.applies("setting a meeting loop's owner")?;
        let session_id = row.session_id;
        let result = self
            .meetings
            .loop_assign(MeetingLoopAssignRequest {
                operation_id: MeetingOperationId::from_uuid(operation_id),
                loop_id: row.loop_id.clone(),
                expected_revision: row.revision,
                owner_person_id: owner,
            })
            .await
            .map_err(command_error(session_id.uuid()))?;
        change.meeting_loop = result
            .loops
            .rows
            .iter()
            .find(|found| found.loop_id == row.loop_id)
            .map(loop_state);
        Ok(WorkAnswer::Organize(Box::new(change)))
    }

    pub(super) async fn action_track(
        &self,
        args: &ActionTrackArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let row = self.loop_row(&args.loop_id).await?;
        let board = self.board().await?;
        let project = board
            .project(&args.project)?
            .ok_or_else(|| ExternalError::invalid("Name the project to track it in."))?
            .id
            .clone();
        let mut change = ExternalOrganizeChange::new(
            OrganizeAction::ActionTracked,
            args.dry_run,
            None,
        );
        change.meeting_loop = Some(loop_state(&row));
        if args.dry_run {
            change.operation_id = args.write.operation_id.map(|id| id.to_string());
            return Ok(WorkAnswer::Organize(Box::new(change)));
        }
        let operation_id = args.write.fresh_operation_id();
        change.replayed = self.ran(&operation_id)?;
        self.applies("tracking a meeting action")?;
        let started_ms = now_ms();
        let task = untracked_actions::track_action_as(
            self.app,
            self.history,
            self.meetings,
            WorkActor::Agent,
            &operation_id,
            row.loop_id.as_str(),
            &project,
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, "The action"))?;
        self.credit(&args.write, std::slice::from_ref(&task.id), started_ms)?;
        if let Some(found) = self.board().await?.row(&task.id) {
            change.tasks.push(task_ref_of(found));
        }
        change.operation_id = Some(operation_id);
        Ok(WorkAnswer::Organize(Box::new(change)))
    }

    pub(super) async fn action_set_aside(
        &self,
        args: &LoopArgs,
        dismiss: bool,
    ) -> Result<WorkAnswer, ExternalError> {
        let row = self.loop_row(&args.loop_id).await?;
        let action = if dismiss {
            OrganizeAction::ActionDismissed
        } else {
            OrganizeAction::ActionRestored
        };
        let mut change = ExternalOrganizeChange::new(action, args.dry_run, None);
        change.meeting_loop = Some(loop_state(&row));
        if args.dry_run {
            change.operation_id = args.write.operation_id.map(|id| id.to_string());
            return Ok(WorkAnswer::Organize(Box::new(change)));
        }
        let operation_id = args.write.fresh_operation_id();
        change.replayed = self.ran(&operation_id)?;
        self.applies(if dismiss {
            "setting a meeting action aside"
        } else {
            "asking about a meeting action again"
        })?;
        let started_ms = now_ms();
        let touched = untracked_actions::set_aside_as(
            self.app,
            self.history,
            self.meetings,
            WorkActor::Agent,
            &operation_id,
            row.loop_id.as_str(),
            dismiss,
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, "The action"))?;
        self.credit(&args.write, &touched, started_ms)?;
        change.meeting_loop = Some(loop_state(&self.loop_row(&args.loop_id).await?));
        change.operation_id = Some(operation_id);
        Ok(WorkAnswer::Organize(Box::new(change)))
    }

    pub(super) async fn attachment_rename(
        &self,
        args: &AttachmentRenameArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let id = args.attachment.to_string();
        let found = work::attachments_of(self.app, &row.item.id)
            .map_err(|message| refusal(self.history, message))?
            .into_iter()
            .find(|attachment| attachment.id == id)
            .ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    format!("No attachment {id} on {}.", super::handle(row.item.number)),
                )
            })?;
        let title = args.title.trim().to_owned();
        let mut change = ExternalOrganizeChange::new(
            OrganizeAction::AttachmentRenamed,
            args.dry_run,
            None,
        );
        change.tasks.push(task_ref_of(row));
        let unchanged = found.title == title;
        if args.dry_run || unchanged {
            change.operation_id = args.write.operation_id.map(|id| id.to_string());
            change.replayed = unchanged;
            let mut attachment = project_attachment(found);
            attachment.title = title;
            change.attachment = Some(attachment);
            return Ok(WorkAnswer::Organize(Box::new(change)));
        }
        let operation_id = args.write.fresh_operation_id();
        change.replayed = self.ran(&operation_id)?;
        let renamed = work::rename_attachment_as(
            self.app,
            Author {
                actor: WorkActor::Agent,
                agent: args.write.agent.as_deref(),
            },
            &operation_id,
            &id,
            &title,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The attachment"))?;
        change.attachment = Some(project_attachment(renamed));
        change.operation_id = Some(operation_id);
        Ok(WorkAnswer::Organize(Box::new(change)))
    }
}

fn refs_text(text: &mut String, label: &str, refs: &[ExternalWorkRef]) {
    if refs.is_empty() {
        return;
    }
    let _ = writeln!(text, "  {label}:");
    for found in refs {
        let _ = writeln!(text, "    {} {}", found.handle, found.title);
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Organize(change) => {
            let _ = writeln!(
                text,
                "{}{}",
                change.action.text(change.dry_run),
                if change.replayed && !change.dry_run {
                    " (already so)"
                } else {
                    ""
                }
            );
            for task in &change.tasks {
                let _ = writeln!(text, "  {} {}", task.handle, task.title);
            }
            if let Some(doer) = &change.doer {
                let _ = writeln!(text, "  Agent: {}", doer.agent);
            }
            if let Some(point) = &change.talking_point {
                let _ = writeln!(text, "  {}  {}", point.text, point.id);
            }
            if let Some(found) = &change.meeting_loop {
                let _ = writeln!(
                    text,
                    "  {} ({}{})  {}",
                    found.text,
                    found.status,
                    found
                        .owner
                        .as_deref()
                        .map(|owner| format!(", {owner}"))
                        .unwrap_or_default(),
                    found.loop_id
                );
            }
            if let Some(attachment) = &change.attachment {
                let _ = writeln!(text, "  {}  id {}", attachment.title, attachment.id);
            }
            if let Some(prompt) = &change.prompt {
                let _ = writeln!(text, "\n{prompt}");
            }
        }
        WorkAnswer::Day(plan) => {
            let _ = writeln!(
                text,
                "{}Focus for {}:",
                if plan.dry_run { "Dry run. " } else { "" },
                plan.day
            );
            if plan.focus.is_empty() {
                text.push_str("  Nothing picked.\n");
            }
            for task in &plan.focus {
                let _ = writeln!(text, "  {} {}", task.handle, task.title);
            }
        }
        WorkAnswer::Brief(brief) => {
            let _ = writeln!(text, "{}", brief.title);
            for person in &brief.people {
                let _ = writeln!(text, "{}", person.name);
                refs_text(text, "You owe", &person.you_owe);
                refs_text(text, "They owe", &person.they_owe);
                if !person.talking_points.is_empty() {
                    text.push_str("  Talking points:\n");
                }
                for point in &person.talking_points {
                    let _ = writeln!(text, "    {}  {}", point.text, point.id);
                }
            }
            if let Some(meeting) = &brief.last_time {
                let _ = writeln!(text, "Last time: {}", meeting.title);
                for decision in &meeting.decisions {
                    let _ = writeln!(text, "  {decision}");
                }
            }
        }
        _ => {}
    }
}
