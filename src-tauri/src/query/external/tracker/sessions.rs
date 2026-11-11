use super::{
    now_ms, refusal, task_ref, task_ref_of, Board, ExternalWorkRef, TaskRef, Tracker, WorkAnswer,
    WriteArgs,
};
use crate::planning::work::agents;
use crate::planning::work::comments::Author;
use crate::planning::work::sessions::{
    self, ActivityInput, WorkPlanStep, WorkSelectOption, WorkSession, WorkSessionActivity,
    WorkSessionActivityKind, WorkSessionState,
};
use crate::planning::work::types::WorkActor;
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use crate::settings::WorkAutonomy;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;
use uuid::Uuid;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SessionStartArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// Print what would start and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ActivityType {
    Thought,
    Action,
    Elicitation,
    Response,
    Error,
}

impl ActivityType {
    const fn kind(self) -> WorkSessionActivityKind {
        match self {
            Self::Thought => WorkSessionActivityKind::Thought,
            Self::Action => WorkSessionActivityKind::Action,
            Self::Elicitation => WorkSessionActivityKind::Elicitation,
            Self::Response => WorkSessionActivityKind::Response,
            Self::Error => WorkSessionActivityKind::Error,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SessionActivityArgs {
    /// The session's id.
    #[arg(value_name = "SESSION")]
    pub session: Uuid,
    /// thought, action, elicitation (a question for the user), response or
    /// error. A response or an error ends the session.
    #[arg(long = "type", value_enum, value_name = "TYPE")]
    pub kind: ActivityType,
    /// What the agent thinks, asks, answers or hit; Markdown.
    #[arg(long)]
    pub body: Option<String>,
    /// For an action: what it does, "Searching", "Editing".
    #[arg(long)]
    pub action: Option<String>,
    /// For an action: what it acts on, a file or a query.
    #[arg(long)]
    pub parameter: Option<String>,
    /// For an action: what came of it.
    #[arg(long)]
    pub result: Option<String>,
    /// For an elicitation: one choice the user can pick; repeat for each.
    #[arg(
        long = "option",
        value_name = "CHOICE",
        conflicts_with = "options_json"
    )]
    pub options: Vec<String>,
    /// For an elicitation: choices as JSON, [{"label":"Ship it","value":"ship"}].
    #[arg(long, value_name = "JSON")]
    pub options_json: Option<String>,
    /// A thought or action the next activity replaces.
    #[arg(long)]
    pub ephemeral: bool,
    /// Replace the session's plan in the same write; see session-plan.
    #[arg(long, value_name = "JSON")]
    pub plan_json: Option<String>,
    /// Print what would be written and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SessionPlanArgs {
    /// The session's id.
    #[arg(value_name = "SESSION")]
    pub session: Uuid,
    /// The whole plan as JSON, replacing the old one:
    /// [{"content":"Read the spec","status":"completed"}]; status is pending,
    /// in_progress, completed or canceled.
    #[arg(long, value_name = "JSON")]
    pub plan_json: String,
    /// Print the plan as it would be and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SessionWriteArgs {
    /// The session's id.
    #[arg(value_name = "SESSION")]
    pub session: Uuid,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SessionReplyArgs {
    /// The session's id.
    #[arg(value_name = "SESSION")]
    pub session: Uuid,
    /// What to tell the agent.
    #[arg(long, required_unless_present = "value")]
    pub body: Option<String>,
    /// The value of one choice the agent's question offered.
    #[arg(long)]
    pub value: Option<String>,
    /// Print what would be written and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SessionShowArgs {
    /// The session's id.
    #[arg(value_name = "SESSION")]
    pub session: Uuid,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SessionsArgs {
    /// One task's sessions: S-12, 12, or its id. Left out: every task's.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: Option<TaskRef>,
    /// Only sessions not yet ended.
    #[arg(long)]
    pub open: bool,
    /// Agent to keep: only this agent's sessions.
    #[arg(long, value_name = "AGENT")]
    pub agent: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAction {
    Started,
    Wrote,
    Planned,
    Heartbeat,
    StopRequested,
    Replied,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSessionAuthor {
    pub kind: WorkActor,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSessionActivity {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: WorkSessionActivityKind,
    pub at_ms: i64,
    pub author: ExternalSessionAuthor,
    pub body: Option<String>,
    pub action: Option<String>,
    pub parameter: Option<String>,
    pub result: Option<String>,
    pub options: Vec<WorkSelectOption>,
    pub value: Option<String>,
    pub ephemeral: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSession {
    pub id: String,
    pub task: ExternalWorkRef,
    pub agent: String,
    pub state: WorkSessionState,
    pub unresponsive: bool,
    pub created_at_ms: i64,
    pub last_activity_at_ms: Option<i64>,
    pub heartbeat_at_ms: Option<i64>,
    pub stop_requested: bool,
    pub stop_requested_at_ms: Option<i64>,
    pub stop_requested_by: Option<String>,
    pub ended_at_ms: Option<i64>,
    pub plan: Vec<WorkPlanStep>,
    pub question: Option<ExternalSessionActivity>,
    pub unread_prompts: Vec<ExternalSessionActivity>,
    pub activities: Vec<ExternalSessionActivity>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSessions {
    pub schema_version: u32,
    pub task: Option<ExternalWorkRef>,
    pub sessions: Vec<ExternalWorkSession>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSessionChange {
    pub schema_version: u32,
    pub action: Option<SessionAction>,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub replayed: bool,
    pub session: ExternalWorkSession,
    pub instruction: Option<String>,
}

fn project_activity(activity: WorkSessionActivity) -> ExternalSessionActivity {
    ExternalSessionActivity {
        id: activity.id,
        kind: activity.kind,
        at_ms: activity.at_ms,
        author: ExternalSessionAuthor {
            kind: activity.author.kind,
            name: activity.author.name,
        },
        body: activity.body,
        action: activity.action,
        parameter: activity.parameter,
        result: activity.result,
        options: activity.options,
        value: activity.value,
        ephemeral: activity.ephemeral,
    }
}

fn project(session: WorkSession, task: ExternalWorkRef) -> ExternalWorkSession {
    ExternalWorkSession {
        id: session.id,
        task,
        agent: session.agent,
        state: session.state,
        unresponsive: session.unresponsive,
        created_at_ms: session.created_at_ms,
        last_activity_at_ms: session.last_activity_at_ms,
        heartbeat_at_ms: session.heartbeat_at_ms,
        stop_requested: session.stop_requested_at_ms.is_some(),
        stop_requested_at_ms: session.stop_requested_at_ms,
        stop_requested_by: session.stop_requested_by,
        ended_at_ms: session.ended_at_ms,
        plan: session.plan,
        question: session.question.map(project_activity),
        unread_prompts: session
            .unread_prompts
            .into_iter()
            .map(project_activity)
            .collect(),
        activities: session
            .activities
            .into_iter()
            .map(project_activity)
            .collect(),
    }
}

fn instruction(session: &ExternalWorkSession) -> Option<String> {
    if session.ended_at_ms.is_some() {
        return None;
    }
    if session.stop_requested {
        return Some(format!(
            "{} asked you to stop. Stop working and end the session: sona work session-activity {} --type response --body \"what was done\".",
            session.stop_requested_by.as_deref().unwrap_or("The user"),
            session.id
        ));
    }
    if !session.unread_prompts.is_empty() {
        return Some("The user wrote to you: read unread_prompts and carry on.".to_owned());
    }
    session
        .question
        .as_ref()
        .map(|_| "Waiting for the user's answer to your question.".to_owned())
}

fn missing_session(id: &Uuid) -> ExternalError {
    ExternalError::new(ExternalErrorCode::NotFound, format!("No session {id}."))
}

fn json_list<T: serde::de::DeserializeOwned>(
    flag: &str,
    json: &str,
) -> Result<Vec<T>, ExternalError> {
    serde_json::from_str(json)
        .map_err(|error| ExternalError::invalid(format!("{flag} is not valid: {error}.")))
}

fn check_plan(plan: &[WorkPlanStep]) -> Result<(), ExternalError> {
    if plan.len() > sessions::MAX_PLAN_STEPS {
        return Err(ExternalError::invalid(format!(
            "A plan holds at most {} steps.",
            sessions::MAX_PLAN_STEPS
        )));
    }
    if plan.iter().any(|step| step.content.trim().is_empty()) {
        return Err(ExternalError::invalid("Every plan step needs content."));
    }
    Ok(())
}

impl Tracker<'_> {
    fn session_named<'b>(
        &self,
        board: &'b Board,
        id: &Uuid,
    ) -> Result<(WorkSession, &'b super::WorkItemRow), ExternalError> {
        let found =
            sessions::session_read(self.app, &id.to_string()).map_err(|_| missing_session(id))?;
        let row = board
            .row(&found.item_id)
            .ok_or_else(|| missing_session(id))?;
        Ok((found, row))
    }

    fn session_answer(
        action: SessionAction,
        dry_run: bool,
        operation_id: Option<String>,
        replayed: bool,
        session: WorkSession,
        row: &super::WorkItemRow,
    ) -> WorkAnswer {
        let session = project(session, task_ref_of(row));
        WorkAnswer::Session(Box::new(ExternalWorkSessionChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: Some(action),
            dry_run,
            operation_id,
            replayed,
            instruction: instruction(&session),
            session,
        }))
    }

    fn agents_session(
        &self,
        board: &Board,
        id: &Uuid,
        write: &WriteArgs,
    ) -> Result<(WorkSession, String), ExternalError> {
        let (found, _) = self.session_named(board, id)?;
        let agent = write.agent();
        if !found.agent.eq_ignore_ascii_case(&agent) {
            return Err(ExternalError::invalid(format!(
                "Session {id} is {}'s; only {} writes to it. Pass --agent \"{}\" if that is you.",
                found.agent, found.agent, found.agent
            )));
        }
        if found.ended_at_ms.is_some() {
            return Err(ExternalError::invalid(format!(
                "Session {id} has ended. Start a new one: sona work session-start {}.",
                board.task_text(&found.item_id)
            )));
        }
        Ok((found, agent))
    }

    fn ended(&self, board: &Board, id: &Uuid) -> Result<WorkSession, ExternalError> {
        let (found, _) = self.session_named(board, id)?;
        if found.ended_at_ms.is_some() {
            return Err(ExternalError::invalid(format!("Session {id} has ended.")));
        }
        Ok(found)
    }

    pub(super) async fn session_start(
        &self,
        args: &SessionStartArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let agent = args.write.agent();
        if let Some(doer) = row
            .doer
            .as_ref()
            .filter(|doer| !doer.agent.eq_ignore_ascii_case(&agent))
        {
            return Err(ExternalError::invalid(format!(
                "{} is delegated to {}. Only {} starts sessions on it until the user hands it over.",
                super::handle(row.item.number),
                doer.agent,
                doer.agent
            )));
        }
        if row.item.archived_at_ms.is_some() {
            return Err(ExternalError::invalid(format!(
                "{} is archived. Restore it first.",
                super::handle(row.item.number)
            )));
        }
        let set_doer = matches!(agents::autonomy(self.app), WorkAutonomy::Apply);
        let operation_id = args.write.fresh_operation_id();
        if args.dry_run {
            let session = self.rehearse(|tx| {
                let id = sessions::start(tx, &row.item.id, &agent, set_doer, now_ms())?;
                sessions::session(tx, &id, now_ms())
            })?;
            return Ok(Self::session_answer(
                SessionAction::Started,
                true,
                args.write.operation_id.map(|id| id.to_string()),
                false,
                session,
                row,
            ));
        }
        let replayed = self.ran(&operation_id)?;
        let session = sessions::start_as(self.app, &agent, &operation_id, &row.item.id, set_doer)
            .map_err(|message| self.write_refusal(message, &operation_id, "The task"))?;
        Ok(Self::session_answer(
            SessionAction::Started,
            false,
            Some(operation_id),
            replayed,
            session,
            row,
        ))
    }

    pub(super) async fn session_activity(
        &self,
        args: &SessionActivityArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (found, agent) = self.agents_session(&board, &args.session, &args.write)?;
        let kind = args.kind.kind();
        if found.stop_requested_at_ms.is_some() && !kind.ends() {
            return Err(ExternalError::invalid(format!(
                "{} asked you to stop. End the session with --type response or --type error.",
                found.stop_requested_by.as_deref().unwrap_or("The user")
            )));
        }
        let options: Vec<WorkSelectOption> = match &args.options_json {
            Some(json) => json_list("--options-json", json)?,
            None => args
                .options
                .iter()
                .map(|choice| WorkSelectOption {
                    label: choice.clone(),
                    value: choice.clone(),
                })
                .collect(),
        };
        if !options.is_empty() && kind != WorkSessionActivityKind::Elicitation {
            return Err(ExternalError::invalid(
                "Choices belong to an elicitation: --type elicitation.",
            ));
        }
        if options.len() > sessions::MAX_OPTIONS {
            return Err(ExternalError::invalid(format!(
                "A question offers at most {} choices.",
                sessions::MAX_OPTIONS
            )));
        }
        if options
            .iter()
            .any(|option| option.label.trim().is_empty() || option.value.trim().is_empty())
        {
            return Err(ExternalError::invalid(
                "Every choice needs a label and a value.",
            ));
        }
        if options.iter().enumerate().any(|(at, option)| {
            options[..at]
                .iter()
                .any(|before| before.value == option.value)
        }) {
            return Err(ExternalError::invalid("Two choices share a value."));
        }
        let filled = |value: &Option<String>| {
            value
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        };
        match kind {
            WorkSessionActivityKind::Action if !filled(&args.action) => {
                return Err(ExternalError::invalid(
                    "An action needs --action: what it does.",
                ));
            }
            WorkSessionActivityKind::Action => {}
            _ if !filled(&args.body) => {
                return Err(ExternalError::invalid("This activity needs --body."));
            }
            _ => {}
        }
        if args.ephemeral
            && !matches!(
                kind,
                WorkSessionActivityKind::Thought | WorkSessionActivityKind::Action
            )
        {
            return Err(ExternalError::invalid(
                "Only a thought or an action can be --ephemeral.",
            ));
        }
        let plan: Option<Vec<WorkPlanStep>> = args
            .plan_json
            .as_deref()
            .map(|json| json_list("--plan-json", json))
            .transpose()?;
        if let Some(plan) = &plan {
            check_plan(plan)?;
        }
        let input = ActivityInput {
            body: args.body.clone(),
            action: args.action.clone(),
            parameter: args.parameter.clone(),
            result: args.result.clone(),
            options,
            ephemeral: args.ephemeral,
        };
        let row = board
            .row(&found.item_id)
            .ok_or_else(|| missing_session(&args.session))?;
        let session_id = args.session.to_string();
        let operation_id = args
            .write
            .operation_id("session-activity", &(&session_id, kind, &input, &plan))?;
        if args.dry_run {
            let session = self.rehearse(|tx| {
                sessions::emit(
                    tx,
                    &session_id,
                    &agent,
                    kind,
                    &input,
                    plan.as_deref(),
                    now_ms(),
                )?;
                sessions::session(tx, &session_id, now_ms())
            })?;
            return Ok(Self::session_answer(
                SessionAction::Wrote,
                true,
                Some(operation_id),
                false,
                session,
                row,
            ));
        }
        let replayed = self.ran(&operation_id)?;
        let session = sessions::emit_as(
            self.app,
            &agent,
            &operation_id,
            &session_id,
            kind,
            &input,
            plan.as_deref(),
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The session"))?;
        Ok(Self::session_answer(
            SessionAction::Wrote,
            false,
            Some(operation_id),
            replayed,
            session,
            row,
        ))
    }

    pub(super) async fn session_plan(
        &self,
        args: &SessionPlanArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (found, agent) = self.agents_session(&board, &args.session, &args.write)?;
        let plan: Vec<WorkPlanStep> = json_list("--plan-json", &args.plan_json)?;
        check_plan(&plan)?;
        let row = board
            .row(&found.item_id)
            .ok_or_else(|| missing_session(&args.session))?;
        let session_id = args.session.to_string();
        let operation_id = args
            .write
            .operation_id("session-plan", &(&session_id, &plan))?;
        if args.dry_run {
            let session = self.rehearse(|tx| {
                sessions::set_plan(tx, &session_id, &agent, &plan)?;
                sessions::session(tx, &session_id, now_ms())
            })?;
            return Ok(Self::session_answer(
                SessionAction::Planned,
                true,
                Some(operation_id),
                false,
                session,
                row,
            ));
        }
        let replayed = self.ran(&operation_id)?;
        let session = sessions::plan_as(self.app, &agent, &operation_id, &session_id, &plan)
            .map_err(|message| self.write_refusal(message, &operation_id, "The session"))?;
        Ok(Self::session_answer(
            SessionAction::Planned,
            false,
            Some(operation_id),
            replayed,
            session,
            row,
        ))
    }

    pub(super) async fn session_heartbeat(
        &self,
        args: &SessionWriteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (found, agent) = self.agents_session(&board, &args.session, &args.write)?;
        let row = board
            .row(&found.item_id)
            .ok_or_else(|| missing_session(&args.session))?;
        if args.dry_run {
            return Ok(Self::session_answer(
                SessionAction::Heartbeat,
                true,
                args.write.operation_id.map(|id| id.to_string()),
                false,
                found,
                row,
            ));
        }
        let operation_id = args.write.fresh_operation_id();
        let replayed = self.ran(&operation_id)?;
        let session =
            sessions::heartbeat_as(self.app, &agent, &operation_id, &args.session.to_string())
                .map_err(|message| self.write_refusal(message, &operation_id, "The session"))?;
        Ok(Self::session_answer(
            SessionAction::Heartbeat,
            false,
            Some(operation_id),
            replayed,
            session,
            row,
        ))
    }

    pub(super) async fn session_stop(
        &self,
        args: &SessionWriteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let found = self.ended(&board, &args.session)?;
        let row = board
            .row(&found.item_id)
            .ok_or_else(|| missing_session(&args.session))?;
        let session_id = args.session.to_string();
        let by = Author {
            actor: WorkActor::Agent,
            agent: args.write.agent.as_deref(),
        };
        let operation_id = args.write.operation_id("session-stop", &session_id)?;
        if args.dry_run {
            let session = self.rehearse(|tx| {
                sessions::request_stop(tx, &session_id, by, now_ms())?;
                sessions::session(tx, &session_id, now_ms())
            })?;
            return Ok(Self::session_answer(
                SessionAction::StopRequested,
                true,
                Some(operation_id),
                false,
                session,
                row,
            ));
        }
        let replayed = found.stop_requested_at_ms.is_some() || self.ran(&operation_id)?;
        let session = sessions::stop_as(self.app, by, &operation_id, &session_id)
            .map_err(|message| self.write_refusal(message, &operation_id, "The session"))?;
        Ok(Self::session_answer(
            SessionAction::StopRequested,
            false,
            Some(operation_id),
            replayed,
            session,
            row,
        ))
    }

    pub(super) async fn session_reply(
        &self,
        args: &SessionReplyArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let found = self.ended(&board, &args.session)?;
        let row = board
            .row(&found.item_id)
            .ok_or_else(|| missing_session(&args.session))?;
        if found.agent.eq_ignore_ascii_case(&args.write.agent()) {
            return Err(ExternalError::invalid(
                "An agent cannot answer its own session.",
            ));
        }
        if let Some(value) = &args.value {
            let Some(question) = &found.question else {
                return Err(ExternalError::invalid(
                    "The agent has no open question with choices to pick from.",
                ));
            };
            if !question.options.iter().any(|option| option.value == *value) {
                let choices: Vec<&str> = question
                    .options
                    .iter()
                    .map(|option| option.value.as_str())
                    .collect();
                return Err(ExternalError::invalid(format!(
                    "{value} is not one of the choices: {}.",
                    choices.join(", ")
                )));
            }
        }
        if args
            .body
            .as_deref()
            .is_some_and(|body| body.trim().is_empty())
            && args.value.is_none()
        {
            return Err(ExternalError::invalid("--body cannot be empty."));
        }
        let session_id = args.session.to_string();
        let by = Author {
            actor: WorkActor::Agent,
            agent: args.write.agent.as_deref(),
        };
        let operation_id = args
            .write
            .operation_id("session-reply", &(&session_id, &args.body, &args.value))?;
        if args.dry_run {
            let session = self.rehearse(|tx| {
                sessions::reply(
                    tx,
                    &session_id,
                    by,
                    args.body.as_deref(),
                    args.value.as_deref(),
                    now_ms(),
                )?;
                sessions::session(tx, &session_id, now_ms())
            })?;
            return Ok(Self::session_answer(
                SessionAction::Replied,
                true,
                Some(operation_id),
                false,
                session,
                row,
            ));
        }
        self.applies("answering an agent's question")?;
        let replayed = self.ran(&operation_id)?;
        let session = sessions::reply_as(
            self.app,
            by,
            &operation_id,
            &session_id,
            args.body.as_deref(),
            args.value.as_deref(),
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The session"))?;
        Ok(Self::session_answer(
            SessionAction::Replied,
            false,
            Some(operation_id),
            replayed,
            session,
            row,
        ))
    }

    pub(super) async fn session_show(
        &self,
        args: &SessionShowArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (found, row) = self.session_named(&board, &args.session)?;
        let session = project(found, task_ref_of(row));
        Ok(WorkAnswer::Session(Box::new(ExternalWorkSessionChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: None,
            dry_run: false,
            operation_id: None,
            replayed: false,
            instruction: instruction(&session),
            session,
        })))
    }

    pub(super) async fn sessions(&self, args: &SessionsArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = args
            .task
            .as_ref()
            .map(|task| board.task(task))
            .transpose()?;
        let found =
            sessions::sessions_read(self.app, row.map(|row| row.item.id.as_str()), args.open)
                .map_err(|message| refusal(self.history, message))?;
        let agent = args
            .agent
            .as_deref()
            .map(|agent| agents::agent_name(Some(agent)));
        let sessions = found
            .into_iter()
            .filter(|session| {
                agent
                    .as_deref()
                    .is_none_or(|agent| session.agent.eq_ignore_ascii_case(agent))
            })
            .filter_map(|session| {
                let row = board.row(&session.item_id)?;
                Some(project(session, task_ref_of(row)))
            })
            .collect();
        Ok(WorkAnswer::Sessions(ExternalWorkSessions {
            schema_version: QUERY_SCHEMA_VERSION,
            task: row.map(task_ref_of),
            sessions,
        }))
    }
}

const fn state_text(state: WorkSessionState) -> &'static str {
    match state {
        WorkSessionState::Pending => "pending",
        WorkSessionState::Active => "active",
        WorkSessionState::AwaitingInput => "awaiting input",
        WorkSessionState::Stale => "stale",
        WorkSessionState::Complete => "complete",
        WorkSessionState::Error => "error",
    }
}

fn session_line(text: &mut String, session: &ExternalWorkSession) {
    let _ = writeln!(
        text,
        "{} {} on {} {}: {}{}{}",
        session.id,
        session.agent,
        session.task.handle,
        session.task.title,
        state_text(session.state),
        if session.unresponsive {
            ", unresponsive"
        } else {
            ""
        },
        if session.stop_requested && session.ended_at_ms.is_none() {
            ", stop requested"
        } else {
            ""
        }
    );
}

fn activity_line(text: &mut String, activity: &ExternalSessionActivity) {
    let who = activity.author.name.as_deref().unwrap_or("You");
    let what = match activity.kind {
        WorkSessionActivityKind::Action => format!(
            "{}{}{}",
            activity.action.as_deref().unwrap_or_default(),
            activity
                .parameter
                .as_deref()
                .map(|value| format!(" {value}"))
                .unwrap_or_default(),
            activity
                .result
                .as_deref()
                .map(|value| format!(" -> {value}"))
                .unwrap_or_default()
        ),
        WorkSessionActivityKind::Stop => "asked to stop".to_owned(),
        _ => activity.body.clone().unwrap_or_default(),
    };
    let kind = serde_json::to_value(activity.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let _ = writeln!(text, "  {kind} ({who}): {what}");
    for option in &activity.options {
        let _ = writeln!(text, "    - {} [{}]", option.label, option.value);
    }
    if let Some(value) = &activity.value {
        let _ = writeln!(text, "    chose {value}");
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Sessions(page) => {
            if page.sessions.is_empty() {
                text.push_str("No agent sessions.\n");
            }
            for session in &page.sessions {
                session_line(text, session);
            }
        }
        WorkAnswer::Session(change) => {
            let verb = match (change.action, change.dry_run) {
                (None, _) => "",
                (Some(SessionAction::Started), false) => "Started ",
                (Some(SessionAction::Started), true) => "Would start ",
                (Some(SessionAction::Wrote), false) => "Wrote to ",
                (Some(SessionAction::Wrote), true) => "Would write to ",
                (Some(SessionAction::Planned), false) => "Planned ",
                (Some(SessionAction::Planned), true) => "Would plan ",
                (Some(SessionAction::Heartbeat), _) => "Heard from ",
                (Some(SessionAction::StopRequested), false) => "Asked to stop ",
                (Some(SessionAction::StopRequested), true) => "Would ask to stop ",
                (Some(SessionAction::Replied), false) => "Answered ",
                (Some(SessionAction::Replied), true) => "Would answer ",
            };
            text.push_str(verb);
            session_line(text, &change.session);
            for step in &change.session.plan {
                let mark = match step.status {
                    sessions::WorkPlanStatus::Completed => "[x]",
                    sessions::WorkPlanStatus::InProgress => "[~]",
                    sessions::WorkPlanStatus::Canceled => "[-]",
                    sessions::WorkPlanStatus::Pending => "[ ]",
                };
                let _ = writeln!(text, "  {mark} {}", step.content);
            }
            for activity in &change.session.activities {
                activity_line(text, activity);
            }
            if let Some(instruction) = &change.instruction {
                let _ = writeln!(text, "{instruction}");
            }
        }
        _ => {}
    }
}
