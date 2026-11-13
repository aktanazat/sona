use super::comments::Author;
use super::types::{WorkActor, WorkCommentAuthor};
use super::{now_ms, run_history, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::store::{self, Refusal};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub(crate) const SESSIONS_MIGRATION: &str = "CREATE TABLE planning_agent_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    agent TEXT NOT NULL CHECK(length(trim(agent)) > 0),
    created_at_ms INTEGER NOT NULL,
    heartbeat_at_ms INTEGER,
    stop_requested_at_ms INTEGER,
    stop_requested_by TEXT,
    plan_json TEXT NOT NULL DEFAULT '[]',
    ended_at_ms INTEGER,
    end_kind TEXT CHECK(end_kind IN ('response', 'error')),
    CHECK((ended_at_ms IS NULL) = (end_kind IS NULL))
);
CREATE INDEX planning_agent_sessions_task ON planning_agent_sessions(task_id, created_at_ms, id);
CREATE TABLE planning_agent_activities (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('thought', 'action', 'response', 'error', 'elicitation',
        'prompt', 'stop')),
    author_kind TEXT NOT NULL CHECK(author_kind IN ('me', 'sona', 'agent')),
    author_name TEXT CHECK(author_name IS NULL OR length(trim(author_name)) > 0),
    body TEXT,
    action TEXT,
    parameter TEXT,
    result TEXT,
    options_json TEXT,
    answer_value TEXT,
    ephemeral INTEGER NOT NULL DEFAULT 0 CHECK(ephemeral IN (0, 1))
);
CREATE INDEX planning_agent_activities_session ON planning_agent_activities(session_id, at_ms);
CREATE TRIGGER planning_agent_sessions_task_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_agent_activities WHERE session_id IN
        (SELECT id FROM planning_agent_sessions WHERE task_id = old.id);
    DELETE FROM planning_agent_sessions WHERE task_id = old.id;
END;";

pub(crate) const UNRESPONSIVE_AFTER_MS: i64 = 10_000;
pub(crate) const STALE_AFTER_MS: i64 = 30 * 60_000;
pub(crate) const MAX_PLAN_STEPS: usize = 50;
pub(crate) const MAX_OPTIONS: usize = 20;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkSessionState {
    Pending,
    Active,
    AwaitingInput,
    Stale,
    Complete,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkSessionActivityKind {
    Thought,
    Action,
    Response,
    Error,
    Elicitation,
    Prompt,
    Stop,
}

impl WorkSessionActivityKind {
    const fn stored(self) -> &'static str {
        match self {
            Self::Thought => "thought",
            Self::Action => "action",
            Self::Response => "response",
            Self::Error => "error",
            Self::Elicitation => "elicitation",
            Self::Prompt => "prompt",
            Self::Stop => "stop",
        }
    }

    fn read(value: &str) -> Option<Self> {
        [
            Self::Thought,
            Self::Action,
            Self::Response,
            Self::Error,
            Self::Elicitation,
            Self::Prompt,
            Self::Stop,
        ]
        .into_iter()
        .find(|kind| kind.stored() == value)
    }

    pub(crate) const fn from_agent(self) -> bool {
        !matches!(self, Self::Prompt | Self::Stop)
    }

    pub(crate) const fn ends(self) -> bool {
        matches!(self, Self::Response | Self::Error)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPlanStatus {
    Pending,
    InProgress,
    Completed,
    Canceled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkPlanStep {
    pub content: String,
    pub status: WorkPlanStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkSelectOption {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkSessionActivity {
    pub id: String,
    pub kind: WorkSessionActivityKind,
    pub at_ms: i64,
    pub author: WorkCommentAuthor,
    pub body: Option<String>,
    pub action: Option<String>,
    pub parameter: Option<String>,
    pub result: Option<String>,
    pub options: Vec<WorkSelectOption>,
    pub value: Option<String>,
    pub ephemeral: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkSession {
    pub id: String,
    pub item_id: String,
    pub agent: String,
    pub state: WorkSessionState,
    pub unresponsive: bool,
    pub created_at_ms: i64,
    pub last_activity_at_ms: Option<i64>,
    pub heartbeat_at_ms: Option<i64>,
    pub stop_requested_at_ms: Option<i64>,
    pub stop_requested_by: Option<String>,
    pub ended_at_ms: Option<i64>,
    pub plan: Vec<WorkPlanStep>,
    pub question: Option<WorkSessionActivity>,
    pub unread_prompts: Vec<WorkSessionActivity>,
    pub activities: Vec<WorkSessionActivity>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct ActivityInput {
    pub(crate) body: Option<String>,
    pub(crate) action: Option<String>,
    pub(crate) parameter: Option<String>,
    pub(crate) result: Option<String>,
    pub(crate) options: Vec<WorkSelectOption>,
    pub(crate) ephemeral: bool,
}

struct Stored {
    id: String,
    item_id: String,
    agent: String,
    created_at_ms: i64,
    heartbeat_at_ms: Option<i64>,
    stop_requested_at_ms: Option<i64>,
    stop_requested_by: Option<String>,
    plan_json: String,
    ended_at_ms: Option<i64>,
    end_kind: Option<String>,
}

const SESSION_COLUMNS: &str = "id, task_id, agent, created_at_ms, heartbeat_at_ms,
    stop_requested_at_ms, stop_requested_by, plan_json, ended_at_ms, end_kind";

const ACTIVITY_COLUMNS: &str = "id, kind, at_ms, author_kind, author_name, body, action,
    parameter, result, options_json, answer_value, ephemeral";

fn read_stored(row: &Row<'_>) -> rusqlite::Result<Stored> {
    Ok(Stored {
        id: row.get(0)?,
        item_id: row.get(1)?,
        agent: row.get(2)?,
        created_at_ms: row.get(3)?,
        heartbeat_at_ms: row.get(4)?,
        stop_requested_at_ms: row.get(5)?,
        stop_requested_by: row.get(6)?,
        plan_json: row.get(7)?,
        ended_at_ms: row.get(8)?,
        end_kind: row.get(9)?,
    })
}

fn bad_column(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

fn read_activity(row: &Row<'_>) -> rusqlite::Result<WorkSessionActivity> {
    let kind: String = row.get(1)?;
    let kind =
        WorkSessionActivityKind::read(&kind).ok_or_else(|| bad_column(1, std::fmt::Error))?;
    let options: Option<String> = row.get(9)?;
    let options = options
        .map(|json| serde_json::from_str(&json).map_err(|error| bad_column(9, error)))
        .transpose()?
        .unwrap_or_default();
    Ok(WorkSessionActivity {
        id: row.get(0)?,
        kind,
        at_ms: row.get(2)?,
        author: WorkCommentAuthor {
            kind: row.get(3)?,
            name: row.get(4)?,
        },
        body: row.get(5)?,
        action: row.get(6)?,
        parameter: row.get(7)?,
        result: row.get(8)?,
        options,
        value: row.get(10)?,
        ephemeral: row.get(11)?,
    })
}

fn stored(conn: &Connection, id: &str) -> Result<Stored> {
    let id = store::uuid(id)?;
    conn.query_row(
        &format!("SELECT {SESSION_COLUMNS} FROM planning_agent_sessions WHERE id = ?1"),
        [&id],
        read_stored,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

fn activities(conn: &Connection, session_id: &str) -> Result<Vec<WorkSessionActivity>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ACTIVITY_COLUMNS} FROM planning_agent_activities
         WHERE session_id = ?1 ORDER BY rowid"
    ))?;
    let found = statement
        .query_map([session_id], read_activity)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(found)
}

fn view(
    stored: Stored,
    log: Vec<WorkSessionActivity>,
    full: bool,
    now_ms: i64,
) -> Result<WorkSession> {
    let plan: Vec<WorkPlanStep> = serde_json::from_str(&stored.plan_json)?;
    let from_agent: Vec<&WorkSessionActivity> = log
        .iter()
        .filter(|activity| activity.kind.from_agent())
        .collect();
    let last_agent = from_agent.last().copied();
    let last_activity_at_ms = log.last().map(|activity| activity.at_ms);
    let question = view_question(&log).cloned();
    let unread_prompts: Vec<WorkSessionActivity> = log
        .iter()
        .skip(
            log.iter()
                .rposition(|activity| activity.kind.from_agent())
                .map_or(0, |at| at + 1),
        )
        .filter(|activity| activity.kind == WorkSessionActivityKind::Prompt)
        .cloned()
        .collect();
    let heard_ms = last_agent
        .map(|activity| activity.at_ms)
        .into_iter()
        .chain(stored.heartbeat_at_ms)
        .max();
    let state = match stored.end_kind.as_deref() {
        Some("error") => WorkSessionState::Error,
        Some(_) => WorkSessionState::Complete,
        None if question.is_some() => WorkSessionState::AwaitingInput,
        None => match heard_ms {
            None => WorkSessionState::Pending,
            Some(heard) if now_ms - heard > STALE_AFTER_MS => WorkSessionState::Stale,
            Some(_) => WorkSessionState::Active,
        },
    };
    let unresponsive =
        state == WorkSessionState::Pending && now_ms - stored.created_at_ms > UNRESPONSIVE_AFTER_MS;
    Ok(WorkSession {
        id: stored.id,
        item_id: stored.item_id,
        agent: stored.agent,
        state,
        unresponsive,
        created_at_ms: stored.created_at_ms,
        last_activity_at_ms,
        heartbeat_at_ms: stored.heartbeat_at_ms,
        stop_requested_at_ms: stored.stop_requested_at_ms,
        stop_requested_by: stored.stop_requested_by,
        ended_at_ms: stored.ended_at_ms,
        plan,
        question,
        unread_prompts,
        activities: if full { log } else { Vec::new() },
    })
}

pub(crate) fn session(conn: &Connection, id: &str, now_ms: i64) -> Result<WorkSession> {
    let found = stored(conn, id)?;
    let log = activities(conn, &found.id)?;
    view(found, log, true, now_ms)
}

pub(crate) fn sessions(
    conn: &Connection,
    task_id: Option<&str>,
    open: bool,
    now_ms: i64,
) -> Result<Vec<WorkSession>> {
    let task_id = task_id.map(store::uuid).transpose()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {SESSION_COLUMNS} FROM planning_agent_sessions
         WHERE (?1 IS NULL OR task_id = ?1) AND (?2 = 0 OR ended_at_ms IS NULL)
         ORDER BY created_at_ms DESC, id DESC"
    ))?;
    let found: Vec<Stored> = statement
        .query_map(params![task_id, open], read_stored)?
        .collect::<rusqlite::Result<_>>()?;
    found
        .into_iter()
        .map(|stored| {
            let log = activities(conn, &stored.id)?;
            view(stored, log, false, now_ms)
        })
        .collect()
}

fn text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn open(conn: &Connection, id: &str) -> Result<Stored> {
    let found = stored(conn, id)?;
    if found.ended_at_ms.is_some() {
        return Err(Refusal::Invalid.into());
    }
    Ok(found)
}

fn runs_it(found: &Stored, agent: &str) -> Result<()> {
    if found.agent.eq_ignore_ascii_case(agent) {
        Ok(())
    } else {
        Err(Refusal::NotAuthor.into())
    }
}

fn insert_activity(
    conn: &Connection,
    session_id: &str,
    kind: WorkSessionActivityKind,
    author: Author<'_>,
    input: &ActivityInput,
    value: Option<&str>,
    at_ms: i64,
) -> Result<String> {
    conn.execute(
        "DELETE FROM planning_agent_activities WHERE ephemeral = 1 AND rowid =
            (SELECT max(rowid) FROM planning_agent_activities WHERE session_id = ?1)",
        [session_id],
    )?;
    let id = Uuid::new_v4().to_string();
    let options = (!input.options.is_empty())
        .then(|| serde_json::to_string(&input.options))
        .transpose()?;
    conn.execute(
        "INSERT INTO planning_agent_activities (id, session_id, at_ms, kind, author_kind,
            author_name, body, action, parameter, result, options_json, answer_value, ephemeral)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            session_id,
            at_ms,
            kind.stored(),
            author.actor.as_str(),
            author.name(),
            text(input.body.as_deref()),
            text(input.action.as_deref()),
            text(input.parameter.as_deref()),
            text(input.result.as_deref()),
            options,
            value,
            input.ephemeral,
        ],
    )?;
    Ok(id)
}

fn valid_plan(plan: &[WorkPlanStep]) -> Result<()> {
    if plan.len() > MAX_PLAN_STEPS || plan.iter().any(|step| step.content.trim().is_empty()) {
        return Err(Refusal::Invalid.into());
    }
    Ok(())
}

fn valid_activity(kind: WorkSessionActivityKind, input: &ActivityInput) -> Result<()> {
    let has = |value: &Option<String>| {
        value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    };
    let shaped = match kind {
        WorkSessionActivityKind::Action => has(&input.action),
        WorkSessionActivityKind::Prompt | WorkSessionActivityKind::Stop => false,
        _ => has(&input.body),
    };
    let options_fit = if input.options.is_empty() {
        true
    } else {
        kind == WorkSessionActivityKind::Elicitation
            && input.options.len() <= MAX_OPTIONS
            && input
                .options
                .iter()
                .all(|option| !option.label.trim().is_empty() && !option.value.trim().is_empty())
            && input.options.iter().enumerate().all(|(at, option)| {
                input.options[..at]
                    .iter()
                    .all(|before| before.value != option.value)
            })
    };
    let ephemeral_fits = !input.ephemeral
        || matches!(
            kind,
            WorkSessionActivityKind::Thought | WorkSessionActivityKind::Action
        );
    if shaped && options_fit && ephemeral_fits {
        Ok(())
    } else {
        Err(Refusal::Invalid.into())
    }
}

pub(crate) fn start(
    conn: &Connection,
    task_id: &str,
    agent: &str,
    set_doer: bool,
    at_ms: i64,
) -> Result<String> {
    let task = super::items::item(conn, &store::uuid(task_id)?)?;
    if task.archived_at_ms.is_some() {
        return Err(Refusal::Invalid.into());
    }
    let doer: Option<String> = conn
        .query_row(
            "SELECT agent FROM planning_task_doers WHERE task_id = ?1",
            [&task.id],
            |row| row.get(0),
        )
        .optional()?;
    match doer {
        Some(doer) if !doer.eq_ignore_ascii_case(agent) => {
            return Err(Refusal::StaleRevision.into())
        }
        Some(_) => {}
        None if set_doer => {
            conn.execute(
                "INSERT INTO planning_task_doers (task_id, agent, since_ms) VALUES (?1, ?2, ?3)",
                params![task.id, agent, at_ms],
            )?;
        }
        None => {}
    }
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_agent_sessions (id, task_id, agent, created_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![id, task.id, agent, at_ms],
    )?;
    Ok(id)
}

pub(crate) fn emit(
    conn: &Connection,
    session_id: &str,
    agent: &str,
    kind: WorkSessionActivityKind,
    input: &ActivityInput,
    plan: Option<&[WorkPlanStep]>,
    at_ms: i64,
) -> Result<String> {
    let found = open(conn, session_id)?;
    runs_it(&found, agent)?;
    if !kind.from_agent() || (found.stop_requested_at_ms.is_some() && !kind.ends()) {
        return Err(Refusal::Invalid.into());
    }
    valid_activity(kind, input)?;
    if let Some(plan) = plan {
        valid_plan(plan)?;
    }
    let author = Author {
        actor: WorkActor::Agent,
        agent: Some(agent),
    };
    insert_activity(conn, &found.id, kind, author, input, None, at_ms)?;
    if let Some(plan) = plan {
        conn.execute(
            "UPDATE planning_agent_sessions SET plan_json = ?2 WHERE id = ?1",
            params![found.id, serde_json::to_string(plan)?],
        )?;
    }
    if kind.ends() {
        conn.execute(
            "UPDATE planning_agent_sessions SET ended_at_ms = ?2, end_kind = ?3 WHERE id = ?1",
            params![found.id, at_ms, kind.stored()],
        )?;
    }
    Ok(found.id)
}

pub(crate) fn set_plan(
    conn: &Connection,
    session_id: &str,
    agent: &str,
    plan: &[WorkPlanStep],
) -> Result<String> {
    let found = open(conn, session_id)?;
    runs_it(&found, agent)?;
    valid_plan(plan)?;
    conn.execute(
        "UPDATE planning_agent_sessions SET plan_json = ?2 WHERE id = ?1",
        params![found.id, serde_json::to_string(plan)?],
    )?;
    Ok(found.id)
}

pub(crate) fn heartbeat(
    conn: &Connection,
    session_id: &str,
    agent: &str,
    at_ms: i64,
) -> Result<String> {
    let found = open(conn, session_id)?;
    runs_it(&found, agent)?;
    conn.execute(
        "UPDATE planning_agent_sessions SET heartbeat_at_ms = ?2 WHERE id = ?1",
        params![found.id, at_ms],
    )?;
    Ok(found.id)
}

pub(crate) fn request_stop(
    conn: &Connection,
    session_id: &str,
    by: Author<'_>,
    at_ms: i64,
) -> Result<String> {
    let found = open(conn, session_id)?;
    if found.stop_requested_at_ms.is_none() {
        let name = by.name().unwrap_or_else(|| "me".to_owned());
        insert_activity(
            conn,
            &found.id,
            WorkSessionActivityKind::Stop,
            by,
            &ActivityInput::default(),
            None,
            at_ms,
        )?;
        conn.execute(
            "UPDATE planning_agent_sessions SET stop_requested_at_ms = ?2, stop_requested_by = ?3
             WHERE id = ?1",
            params![found.id, at_ms, name],
        )?;
    }
    Ok(found.id)
}

pub(crate) fn reply(
    conn: &Connection,
    session_id: &str,
    by: Author<'_>,
    body: Option<&str>,
    value: Option<&str>,
    at_ms: i64,
) -> Result<String> {
    let found = open(conn, session_id)?;
    if by
        .name()
        .is_some_and(|name| name.eq_ignore_ascii_case(&found.agent))
    {
        return Err(Refusal::Invalid.into());
    }
    let body = text(body);
    let value = text(value);
    if body.is_none() && value.is_none() {
        return Err(Refusal::Invalid.into());
    }
    if let Some(value) = &value {
        let log = activities(conn, &found.id)?;
        let question = view_question(&log).ok_or(Refusal::Invalid)?;
        if !question.options.iter().any(|option| option.value == *value) {
            return Err(Refusal::Invalid.into());
        }
    }
    let input = ActivityInput {
        body,
        ..ActivityInput::default()
    };
    insert_activity(
        conn,
        &found.id,
        WorkSessionActivityKind::Prompt,
        by,
        &input,
        value.as_deref(),
        at_ms,
    )?;
    Ok(found.id)
}

fn view_question(log: &[WorkSessionActivity]) -> Option<&WorkSessionActivity> {
    let at = log
        .iter()
        .rposition(|activity| activity.kind == WorkSessionActivityKind::Elicitation)?;
    let answered = log[at + 1..]
        .iter()
        .any(|later| later.kind == WorkSessionActivityKind::Prompt || later.kind.from_agent());
    (!answered).then(|| &log[at])
}

fn task_of(conn: &Connection, session_id: &str) -> Result<String> {
    Ok(stored(conn, session_id)?.item_id)
}

fn change(
    app: &AppHandle,
    operation_id: &str,
    command: &str,
    request: &impl Serialize,
    write: impl FnOnce(&Connection, i64) -> Result<String>,
) -> Result<WorkSession, String> {
    let at_ms = now_ms();
    let id = run_history(app, |conn| {
        let id = store::mutate(conn, operation_id, command, request, at_ms, |tx| {
            write(tx, at_ms)
        })?;
        let mut effects = Effects::default();
        effects.items.push(task_of(conn, &id)?);
        Ok(Done { value: id, effects })
    })?;
    read(app, &id)
}

fn read(app: &AppHandle, id: &str) -> Result<WorkSession, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        session(conn, id, now_ms())
    })
}

pub(crate) fn start_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    task_id: &str,
    set_doer: bool,
) -> Result<WorkSession, String> {
    change(
        app,
        operation_id,
        "work_session_start",
        &(task_id, agent, set_doer),
        |tx, at_ms| start(tx, task_id, agent, set_doer, at_ms),
    )
}

pub(crate) fn emit_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    session_id: &str,
    kind: WorkSessionActivityKind,
    input: &ActivityInput,
    plan: Option<&[WorkPlanStep]>,
) -> Result<WorkSession, String> {
    change(
        app,
        operation_id,
        "work_session_activity",
        &(session_id, agent, kind, input, plan),
        |tx, at_ms| emit(tx, session_id, agent, kind, input, plan, at_ms),
    )
}

pub(crate) fn plan_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    session_id: &str,
    plan: &[WorkPlanStep],
) -> Result<WorkSession, String> {
    change(
        app,
        operation_id,
        "work_session_plan",
        &(session_id, agent, plan),
        |tx, _| set_plan(tx, session_id, agent, plan),
    )
}

pub(crate) fn heartbeat_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    session_id: &str,
) -> Result<WorkSession, String> {
    change(
        app,
        operation_id,
        "work_session_heartbeat",
        &(session_id, agent),
        |tx, at_ms| heartbeat(tx, session_id, agent, at_ms),
    )
}

pub(crate) fn stop_as(
    app: &AppHandle,
    by: Author<'_>,
    operation_id: &str,
    session_id: &str,
) -> Result<WorkSession, String> {
    change(
        app,
        operation_id,
        "work_session_stop",
        &(session_id, by.actor, by.agent),
        |tx, at_ms| request_stop(tx, session_id, by, at_ms),
    )
}

pub(crate) fn reply_as(
    app: &AppHandle,
    by: Author<'_>,
    operation_id: &str,
    session_id: &str,
    body: Option<&str>,
    value: Option<&str>,
) -> Result<WorkSession, String> {
    change(
        app,
        operation_id,
        "work_session_reply",
        &(session_id, by.actor, by.agent, body, value),
        |tx, at_ms| reply(tx, session_id, by, body, value, at_ms),
    )
}

pub(crate) fn sessions_read(
    app: &AppHandle,
    task_id: Option<&str>,
    open: bool,
) -> Result<Vec<WorkSession>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        sessions(conn, task_id, open, now_ms())
    })
}

pub(crate) fn session_read(app: &AppHandle, id: &str) -> Result<WorkSession, String> {
    read(app, id)
}

const ME: Author<'static> = Author {
    actor: WorkActor::Me,
    agent: None,
};

pub fn work_sessions(
    app: AppHandle,
    item_id: Option<String>,
    open: bool,
) -> Result<Vec<WorkSession>, String> {
    sessions_read(&app, item_id.as_deref(), open)
}

pub fn work_session(app: AppHandle, id: String) -> Result<WorkSession, String> {
    session_read(&app, &id)
}

pub fn work_session_stop(
    app: AppHandle,
    operation_id: String,
    id: String,
) -> Result<WorkSession, String> {
    stop_as(&app, ME, &operation_id, &id)
}

pub fn work_session_reply(
    app: AppHandle,
    operation_id: String,
    id: String,
    body: Option<String>,
    value: Option<String>,
) -> Result<WorkSession, String> {
    reply_as(
        &app,
        ME,
        &operation_id,
        &id,
        body.as_deref(),
        value.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;
    use crate::planning::work::items::{self, NewItem};
    use crate::planning::work::types::{WorkOwner, WorkStatus};

    fn filed(conn: &Connection, title: &str) -> String {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            0,
        )
        .expect("a filed task")
        .id
    }

    #[test]
    fn a_stop_request_leaves_the_agent_only_an_ending_activity() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let task = filed(&conn, "Draft the venue contract");
        let id = start(&conn, &task, "Codex", true, 1).expect("a session");
        let thought = ActivityInput {
            body: Some("Reading the deposit terms".to_owned()),
            ..ActivityInput::default()
        };
        emit(
            &conn,
            &id,
            "Codex",
            WorkSessionActivityKind::Thought,
            &thought,
            None,
            2,
        )
        .expect("a thought before the stop");
        let user = Author {
            actor: WorkActor::Me,
            agent: None,
        };
        request_stop(&conn, &id, user, 3).expect("a stop request");

        let after = emit(
            &conn,
            &id,
            "Codex",
            WorkSessionActivityKind::Thought,
            &thought,
            None,
            4,
        );
        let answer = ActivityInput {
            body: Some("Stopped with the draft half done".to_owned()),
            ..ActivityInput::default()
        };
        emit(
            &conn,
            &id,
            "Codex",
            WorkSessionActivityKind::Response,
            &answer,
            None,
            5,
        )
        .expect("the response that ends it");
        let ended = session(&conn, &id, 6).expect("the session");

        assert!(
            after.is_err(),
            "a thought after the stop request was accepted"
        );
        assert_eq!(ended.state, WorkSessionState::Complete);
        assert_eq!(ended.stop_requested_at_ms, Some(3));
        assert_eq!(
            ended
                .activities
                .iter()
                .map(|activity| activity.kind)
                .collect::<Vec<_>>(),
            [
                WorkSessionActivityKind::Thought,
                WorkSessionActivityKind::Stop,
                WorkSessionActivityKind::Response,
            ]
        );
    }
}
