use super::sessions::{self, ActivityInput, WorkSessionActivityKind};
use super::types::{WorkActor, WorkItemInput, WorkPatch, WorkStatus};
use super::{now_ms, tasks_changed};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use crate::meeting::types::MeetingSessionId;
use crate::planning::store::{self, Refusal};
use crate::planning::types::{EvidenceSourceKind, EvidenceTargetKind, ProjectStatus};
use crate::query::KeptLocal;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub(crate) const LOOPS_MIGRATION: &str = "CREATE TABLE planning_loops (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 64),
    prompt TEXT NOT NULL CHECK(length(trim(prompt)) > 0),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
    trigger_json TEXT NOT NULL,
    conditions_json TEXT NOT NULL,
    tools_json TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version > 0),
    next_run_at_ms INTEGER,
    change_cursor INTEGER NOT NULL DEFAULT 0,
    armed_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX planning_loops_name ON planning_loops(lower(name));
CREATE TABLE planning_loop_versions (
    loop_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    name TEXT NOT NULL,
    prompt TEXT NOT NULL,
    trigger_json TEXT NOT NULL,
    conditions_json TEXT NOT NULL,
    tools_json TEXT NOT NULL,
    saved_at_ms INTEGER NOT NULL,
    saved_by TEXT NOT NULL,
    restored_from INTEGER,
    PRIMARY KEY(loop_id, version)
);
CREATE TABLE planning_loop_runs (
    id TEXT PRIMARY KEY NOT NULL,
    loop_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    cause_key TEXT NOT NULL,
    cause TEXT NOT NULL,
    task_id TEXT,
    meeting_id TEXT,
    status TEXT NOT NULL CHECK(status IN ('queued', 'running', 'done', 'failed', 'skipped')),
    queued_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    ended_at_ms INTEGER,
    output TEXT,
    error TEXT,
    refused_json TEXT NOT NULL DEFAULT '[]',
    session_id TEXT,
    UNIQUE(loop_id, cause_key)
);
CREATE INDEX planning_loop_runs_loop ON planning_loop_runs(loop_id, queued_at_ms);
CREATE INDEX planning_loop_runs_waiting ON planning_loop_runs(status, queued_at_ms)
    WHERE status IN ('queued', 'running');";

pub(crate) const LOOP_TOOLS: [(&str, &str, &[&str]); 10] = [
    ("sona_search", "Search", &["search", "dictation"]),
    ("sona_meetings", "List meetings", &["recent"]),
    ("sona_meeting", "Read a meeting", &["meeting"]),
    ("sona_transcript", "Read a transcript", &["transcript"]),
    ("sona_people", "Find people", &["person"]),
    ("sona_action_items", "Action items", &["loops"]),
    ("sona_upcoming", "Upcoming meetings", &["upcoming"]),
    ("sona_stats", "Statistics", &["word_stats", "activity"]),
    (PROPOSE_NEW, "Propose new tasks for Triage", &[]),
    (PROPOSE_CHANGE, "Propose task changes for Triage", &[]),
];

const PROPOSE_NEW: &str = "sona_work_create";
const PROPOSE_CHANGE: &str = "sona_work_update";
const OFFER_FENCE: &str = "```sona-proposals";
const OFFER_LIMIT: usize = 10;
const KEPT_HERE: &str =
    "Skipped: this meeting's series is kept on this Mac, so it isn't sent to the in-app agent.";

pub(crate) const LOOP_AGENT: &str = "Sona automation";
const NAME_LIMIT: usize = 64;
const PROMPT_LIMIT: usize = 4000;
const CONDITION_LIMIT: usize = 8;
const FIRST_RUN_GRACE_MS: i64 = 60_000;
const CHANGE_PAGE: i64 = 200;
const RUN_PAGE: i64 = 50;
const TICK: Duration = Duration::from_secs(10);
const RUN_DEADLINE: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopEvery {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopDay {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl LoopDay {
    const fn weekday(self) -> jiff::civil::Weekday {
        match self {
            Self::Mon => jiff::civil::Weekday::Monday,
            Self::Tue => jiff::civil::Weekday::Tuesday,
            Self::Wed => jiff::civil::Weekday::Wednesday,
            Self::Thu => jiff::civil::Weekday::Thursday,
            Self::Fri => jiff::civil::Weekday::Friday,
            Self::Sat => jiff::civil::Weekday::Saturday,
            Self::Sun => jiff::civil::Weekday::Sunday,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LoopTrigger {
    Schedule {
        every: LoopEvery,
        #[serde(default)]
        days: Vec<LoopDay>,
        timezone: String,
        first_run_at_ms: i64,
    },
    TaskCreated,
    StatusChanged {
        #[serde(default)]
        to: Option<WorkStatus>,
    },
    LabelAdded {
        #[serde(default)]
        label_id: Option<String>,
    },
    Assigned,
    CommentAdded,
    ProjectStatusChanged {
        #[serde(default)]
        to: Option<ProjectStatus>,
    },
    MeetingEnded,
}

impl LoopTrigger {
    const fn reads_changes(&self) -> bool {
        !matches!(self, Self::Schedule { .. } | Self::MeetingEnded)
    }

    const fn on_tasks(&self) -> bool {
        matches!(
            self,
            Self::TaskCreated
                | Self::StatusChanged { .. }
                | Self::LabelAdded { .. }
                | Self::Assigned
                | Self::CommentAdded
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoopConditions {
    pub labels: Vec<String>,
    pub projects: Vec<String>,
    pub statuses: Vec<WorkStatus>,
}

impl LoopConditions {
    fn count(&self) -> usize {
        self.labels.len() + self.projects.len() + self.statuses.len()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopInput {
    pub name: String,
    pub prompt: String,
    pub trigger: LoopTrigger,
    #[serde(default)]
    pub conditions: LoopConditions,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopRunStatus {
    Queued,
    Running,
    Done,
    Failed,
    Skipped,
}

impl LoopRunStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "done" => Self::Done,
            "skipped" => Self::Skipped,
            _ => Self::Failed,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkLoopRun {
    pub id: String,
    #[serde(rename = "automationId")]
    pub loop_id: String,
    pub version: i64,
    pub cause: String,
    pub task_id: Option<String>,
    pub meeting_id: Option<String>,
    pub status: LoopRunStatus,
    pub queued_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub refused_tools: Vec<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkLoop {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub enabled: bool,
    pub trigger: LoopTrigger,
    pub conditions: LoopConditions,
    pub tools: Vec<String>,
    pub version: i64,
    pub next_run_at_ms: Option<i64>,
    pub last_run: Option<WorkLoopRun>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl WorkLoop {
    fn content(&self) -> LoopInput {
        LoopInput {
            name: self.name.clone(),
            prompt: self.prompt.clone(),
            trigger: self.trigger.clone(),
            conditions: self.conditions.clone(),
            tools: self.tools.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkLoopVersion {
    pub version: i64,
    pub name: String,
    pub prompt: String,
    pub trigger: LoopTrigger,
    pub conditions: LoopConditions,
    pub tools: Vec<String>,
    pub saved_at_ms: i64,
    pub saved_by: String,
    pub restored_from: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopTool {
    pub name: &'static str,
    pub title: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkLoops {
    pub loops: Vec<WorkLoop>,
    pub tools: Vec<LoopTool>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkLoopDetail {
    #[serde(rename = "automation")]
    pub item: WorkLoop,
    pub versions: Vec<WorkLoopVersion>,
    pub runs: Vec<WorkLoopRun>,
}

#[derive(Debug)]
pub(crate) struct LoopRefusal(pub(crate) String);

impl std::fmt::Display for LoopRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LoopRefusal {}

fn refuse<T>(message: impl Into<String>) -> Result<T> {
    Err(LoopRefusal(message.into()).into())
}

pub(crate) fn message(error: anyhow::Error) -> String {
    match error.downcast_ref::<LoopRefusal>() {
        Some(refusal) => refusal.0.clone(),
        None => crate::planning::failure(error),
    }
}

pub(crate) fn tools() -> Vec<LoopTool> {
    LOOP_TOOLS
        .iter()
        .map(|(name, title, _)| LoopTool { name, title })
        .collect()
}

pub(crate) fn lookups(tools: &[String]) -> Vec<&'static str> {
    LOOP_TOOLS
        .iter()
        .filter(|(name, _, _)| tools.iter().any(|tool| tool == name))
        .flat_map(|(_, _, lookups)| lookups.iter().copied())
        .collect()
}

fn json<T: DeserializeOwned>(text: &str) -> Result<T> {
    Ok(serde_json::from_str(text)?)
}

const LOOP_COLUMNS: &str = "id, name, prompt, enabled, trigger_json, conditions_json, tools_json, version, next_run_at_ms, created_at_ms, updated_at_ms";
const RUN_COLUMNS: &str = "id, loop_id, version, cause, task_id, meeting_id, status, queued_at_ms, started_at_ms, ended_at_ms, output, error, refused_json, session_id";

type LoopRow = (
    String,
    String,
    String,
    bool,
    String,
    String,
    String,
    i64,
    Option<i64>,
    i64,
    i64,
);

fn loop_row(row: &Row<'_>) -> rusqlite::Result<LoopRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn loop_of(conn: &Connection, row: LoopRow) -> Result<WorkLoop> {
    let (id, name, prompt, enabled, trigger, conditions, tools, version, next, created, updated) =
        row;
    let last_run = runs(conn, &id, 1)?.into_iter().next();
    Ok(WorkLoop {
        name,
        prompt,
        enabled,
        trigger: json(&trigger)?,
        conditions: json(&conditions)?,
        tools: json(&tools)?,
        version,
        next_run_at_ms: next,
        last_run,
        created_at_ms: created,
        updated_at_ms: updated,
        id,
    })
}

fn run_row(row: &Row<'_>) -> rusqlite::Result<(WorkLoopRun, String)> {
    let status: String = row.get(6)?;
    Ok((
        WorkLoopRun {
            id: row.get(0)?,
            loop_id: row.get(1)?,
            version: row.get(2)?,
            cause: row.get(3)?,
            task_id: row.get(4)?,
            meeting_id: row.get(5)?,
            status: LoopRunStatus::parse(&status),
            queued_at_ms: row.get(7)?,
            started_at_ms: row.get(8)?,
            ended_at_ms: row.get(9)?,
            output: row.get(10)?,
            error: row.get(11)?,
            refused_tools: Vec::new(),
            session_id: row.get(13)?,
        },
        row.get(12)?,
    ))
}

fn runs(conn: &Connection, loop_id: &str, limit: i64) -> Result<Vec<WorkLoopRun>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM planning_loop_runs WHERE loop_id = ?1
         ORDER BY queued_at_ms DESC, rowid DESC LIMIT ?2"
    ))?;
    let rows = statement.query_map(params![loop_id, limit], run_row)?;
    rows.map(|row| {
        let (mut run, refused) = row?;
        run.refused_tools = json(&refused)?;
        Ok(run)
    })
    .collect()
}

fn run(conn: &Connection, run_id: &str) -> Result<WorkLoopRun> {
    let (mut run, refused) = conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM planning_loop_runs WHERE id = ?1"),
            [run_id],
            run_row,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    run.refused_tools = json(&refused)?;
    Ok(run)
}

pub(crate) fn loops(conn: &Connection) -> Result<Vec<WorkLoop>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {LOOP_COLUMNS} FROM planning_loops ORDER BY lower(name)"
    ))?;
    let rows = statement
        .query_map([], loop_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(|row| loop_of(conn, row)).collect()
}

pub(crate) fn get(conn: &Connection, id: &str) -> Result<WorkLoop> {
    let row = conn
        .query_row(
            &format!("SELECT {LOOP_COLUMNS} FROM planning_loops WHERE id = ?1"),
            [id],
            loop_row,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    loop_of(conn, row)
}

pub(crate) fn versions(conn: &Connection, id: &str) -> Result<Vec<WorkLoopVersion>> {
    let mut statement = conn.prepare(
        "SELECT version, name, prompt, trigger_json, conditions_json, tools_json, saved_at_ms, saved_by, restored_from
         FROM planning_loop_versions WHERE loop_id = ?1 ORDER BY version DESC",
    )?;
    let rows = statement
        .query_map([id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<i64>>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(
                version,
                name,
                prompt,
                trigger,
                conditions,
                tools,
                saved_at_ms,
                saved_by,
                restored_from,
            )| {
                Ok(WorkLoopVersion {
                    version,
                    name,
                    prompt,
                    trigger: json(&trigger)?,
                    conditions: json(&conditions)?,
                    tools: json(&tools)?,
                    saved_at_ms,
                    saved_by,
                    restored_from,
                })
            },
        )
        .collect()
}

pub(crate) fn detail(conn: &Connection, id: &str) -> Result<WorkLoopDetail> {
    Ok(WorkLoopDetail {
        item: get(conn, id)?,
        versions: versions(conn, id)?,
        runs: runs(conn, id, RUN_PAGE)?,
    })
}

fn change_head(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'planning_changes'), 0)",
        [],
        |row| row.get(0),
    )?)
}

pub(crate) fn first_run_ms(timezone: &str, local: &str) -> Result<i64, String> {
    let zone = jiff::tz::TimeZone::get(timezone)
        .map_err(|_| format!("Sona doesn't know the time zone {timezone:?}."))?;
    let at: jiff::civil::DateTime = local
        .trim()
        .replace(' ', "T")
        .parse()
        .map_err(|_| "Give the first run as YYYY-MM-DD HH:MM.".to_owned())?;
    at.to_zoned(zone)
        .map(|zoned| zoned.timestamp().as_millisecond())
        .map_err(|_| "Give the first run as YYYY-MM-DD HH:MM.".to_owned())
}

pub(crate) fn next_run(trigger: &LoopTrigger, after_ms: i64) -> Option<i64> {
    let LoopTrigger::Schedule {
        every,
        days,
        timezone,
        first_run_at_ms,
    } = trigger
    else {
        return None;
    };
    let first_ms = *first_run_at_ms;
    if *every == LoopEvery::Hourly {
        if first_ms > after_ms {
            return Some(first_ms);
        }
        let hour = 3_600_000;
        return Some(first_ms + ((after_ms - first_ms) / hour + 1) * hour);
    }
    let zone = jiff::tz::TimeZone::get(timezone).ok()?;
    let first = jiff::Timestamp::from_millisecond(first_ms)
        .ok()?
        .to_zoned(zone.clone());
    let after = jiff::Timestamp::from_millisecond(after_ms)
        .ok()?
        .to_zoned(zone.clone());
    let time = first.time();
    let at = |date: jiff::civil::Date| {
        date.to_datetime(time)
            .to_zoned(zone.clone())
            .ok()
            .map(|zoned| zoned.timestamp().as_millisecond())
            .filter(|at| *at > after_ms && *at >= first_ms)
    };
    match every {
        LoopEvery::Hourly => None,
        LoopEvery::Daily | LoopEvery::Weekly => {
            let mut date = first.date().max(after.date().yesterday().ok()?);
            for _ in 0..10 {
                let chosen = *every == LoopEvery::Daily
                    || days.iter().any(|day| day.weekday() == date.weekday());
                if let Some(found) = at(date).filter(|_| chosen) {
                    return Some(found);
                }
                date = date.tomorrow().ok()?;
            }
            None
        }
        LoopEvery::Monthly => {
            let day = first.day();
            let earliest = after
                .date()
                .first_of_month()
                .checked_sub(jiff::Span::new().months(1))
                .ok()?;
            let mut month = first.date().first_of_month().max(earliest);
            for _ in 0..4 {
                let date = jiff::civil::Date::new(
                    month.year(),
                    month.month(),
                    day.min(month.days_in_month()),
                )
                .ok()?;
                if let Some(found) = at(date) {
                    return Some(found);
                }
                month = month.checked_add(jiff::Span::new().months(1)).ok()?;
            }
            None
        }
    }
}

fn exists(conn: &Connection, table: &str, id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE id = ?1"),
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn checked(
    conn: &Connection,
    input: &LoopInput,
    schedule_changed: bool,
    now: i64,
) -> Result<LoopInput> {
    let name = input.name.trim();
    if name.is_empty() {
        return refuse("An automation needs a name.");
    }
    if name.chars().count() > NAME_LIMIT {
        return refuse("An automation's name can be at most 64 characters.");
    }
    let prompt = input.prompt.trim();
    if prompt.is_empty() {
        return refuse("An automation needs a prompt.");
    }
    if prompt.chars().count() > PROMPT_LIMIT {
        return refuse("An automation's prompt can be at most 4,000 characters.");
    }
    let mut trigger = input.trigger.clone();
    match &mut trigger {
        LoopTrigger::Schedule {
            every,
            days,
            timezone,
            first_run_at_ms,
        } => {
            if jiff::tz::TimeZone::get(timezone).is_err() {
                return refuse(format!("Sona doesn't know the time zone {timezone:?}."));
            }
            if *every == LoopEvery::Weekly {
                if days.is_empty() {
                    return refuse("A weekly automation needs at least one day.");
                }
                days.sort_by_key(|day| day.weekday().to_monday_zero_offset());
                days.dedup();
            } else {
                days.clear();
            }
            if schedule_changed && *first_run_at_ms < now - FIRST_RUN_GRACE_MS {
                return refuse("The first run can't be in the past.");
            }
        }
        LoopTrigger::LabelAdded {
            label_id: Some(label),
        } => {
            if !exists(conn, "planning_labels", label)? {
                return refuse("That label no longer exists.");
            }
        }
        _ => {}
    }
    let mut conditions = input.conditions.clone();
    conditions.labels.dedup();
    conditions.projects.dedup();
    conditions.statuses.dedup();
    if conditions.count() > CONDITION_LIMIT {
        return refuse("An automation can have at most 8 conditions.");
    }
    if conditions.count() > 0 && !trigger.on_tasks() {
        let project_only = matches!(trigger, LoopTrigger::ProjectStatusChanged { .. })
            && conditions.labels.is_empty()
            && conditions.statuses.is_empty();
        if !project_only {
            return refuse(match trigger {
                LoopTrigger::ProjectStatusChanged { .. } => {
                    "A project trigger can only check which project it is."
                }
                _ => "Conditions apply only to task and project triggers.",
            });
        }
    }
    for label in &conditions.labels {
        if !exists(conn, "planning_labels", label)? {
            return refuse("A label in the conditions no longer exists.");
        }
    }
    for project in &conditions.projects {
        if !exists(conn, "planning_projects", project)? {
            return refuse("A project in the conditions no longer exists.");
        }
    }
    let mut tools = Vec::new();
    for tool in &input.tools {
        let tool = tool.trim();
        if !LOOP_TOOLS.iter().any(|(name, _, _)| *name == tool) {
            return refuse(format!(
                "The in-app agent can't use {tool}. It can use {}.",
                LOOP_TOOLS
                    .iter()
                    .map(|(name, _, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !tools.iter().any(|kept: &String| kept == tool) {
            tools.push(tool.to_owned());
        }
    }
    Ok(LoopInput {
        name: name.to_owned(),
        prompt: prompt.to_owned(),
        trigger,
        conditions,
        tools,
    })
}

fn name_taken(conn: &Connection, name: &str, id: Option<&str>) -> Result<()> {
    let taken: Option<String> = conn
        .query_row(
            "SELECT name FROM planning_loops WHERE lower(name) = lower(?1) AND id IS NOT ?2",
            params![name, id],
            |row| row.get(0),
        )
        .optional()?;
    match taken {
        Some(taken) => refuse(format!("An automation called {taken:?} already exists.")),
        None => Ok(()),
    }
}

fn add_version(
    conn: &Connection,
    id: &str,
    version: i64,
    input: &LoopInput,
    by: &str,
    restored_from: Option<i64>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_loop_versions
            (loop_id, version, name, prompt, trigger_json, conditions_json, tools_json, saved_at_ms, saved_by, restored_from)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            version,
            input.name,
            input.prompt,
            serde_json::to_string(&input.trigger)?,
            serde_json::to_string(&input.conditions)?,
            serde_json::to_string(&input.tools)?,
            now,
            by,
            restored_from,
        ],
    )?;
    Ok(())
}

fn rewrite(
    conn: &Connection,
    current: &WorkLoop,
    input: &LoopInput,
    by: &str,
    restored_from: Option<i64>,
    now: i64,
) -> Result<()> {
    let version = current.version + 1;
    let rearm = input.trigger != current.trigger;
    let next = if current.enabled && rearm {
        next_run(&input.trigger, now)
    } else {
        current.next_run_at_ms
    };
    conn.execute(
        "UPDATE planning_loops SET name = ?2, prompt = ?3, trigger_json = ?4, conditions_json = ?5,
            tools_json = ?6, version = ?7, next_run_at_ms = ?8, updated_at_ms = ?9,
            change_cursor = CASE WHEN ?10 THEN ?11 ELSE change_cursor END,
            armed_at_ms = CASE WHEN ?10 THEN ?9 ELSE armed_at_ms END
         WHERE id = ?1",
        params![
            current.id,
            input.name,
            input.prompt,
            serde_json::to_string(&input.trigger)?,
            serde_json::to_string(&input.conditions)?,
            serde_json::to_string(&input.tools)?,
            version,
            next,
            now,
            rearm,
            change_head(conn)?,
        ],
    )?;
    add_version(conn, &current.id, version, input, by, restored_from, now)
}

pub(crate) fn save(
    conn: &Connection,
    id: Option<&str>,
    input: &LoopInput,
    by: &str,
    now: i64,
) -> Result<WorkLoop> {
    match id {
        None => {
            let input = checked(conn, input, true, now)?;
            name_taken(conn, &input.name, None)?;
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO planning_loops
                    (id, name, prompt, enabled, trigger_json, conditions_json, tools_json, version,
                     next_run_at_ms, change_cursor, armed_at_ms, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, 1, ?7, ?8, ?9, ?9, ?9)",
                params![
                    id,
                    input.name,
                    input.prompt,
                    serde_json::to_string(&input.trigger)?,
                    serde_json::to_string(&input.conditions)?,
                    serde_json::to_string(&input.tools)?,
                    next_run(&input.trigger, now),
                    change_head(conn)?,
                    now,
                ],
            )?;
            add_version(conn, &id, 1, &input, by, None, now)?;
            get(conn, &id)
        }
        Some(id) => {
            let current = get(conn, id)?;
            let schedule_changed = input.trigger != current.trigger;
            let input = checked(conn, input, schedule_changed, now)?;
            if input == current.content() {
                return Ok(current);
            }
            name_taken(conn, &input.name, Some(id))?;
            rewrite(conn, &current, &input, by, None, now)?;
            get(conn, id)
        }
    }
}

pub(crate) fn restore(
    conn: &Connection,
    id: &str,
    version: i64,
    by: &str,
    now: i64,
) -> Result<WorkLoop> {
    let current = get(conn, id)?;
    let Some(old) = versions(conn, id)?
        .into_iter()
        .find(|kept| kept.version == version)
    else {
        return refuse(format!("This automation has no version {version}."));
    };
    let input = checked(
        conn,
        &LoopInput {
            name: old.name,
            prompt: old.prompt,
            trigger: old.trigger,
            conditions: old.conditions,
            tools: old.tools,
        },
        false,
        now,
    )?;
    if input == current.content() {
        return Ok(current);
    }
    name_taken(conn, &input.name, Some(id))?;
    rewrite(conn, &current, &input, by, Some(version), now)?;
    get(conn, id)
}

pub(crate) fn enable(conn: &Connection, id: &str, enabled: bool, now: i64) -> Result<WorkLoop> {
    let current = get(conn, id)?;
    if current.enabled == enabled {
        return Ok(current);
    }
    conn.execute(
        "UPDATE planning_loops SET enabled = ?2, next_run_at_ms = ?3, updated_at_ms = ?4,
            change_cursor = CASE WHEN ?2 THEN ?5 ELSE change_cursor END,
            armed_at_ms = CASE WHEN ?2 THEN ?4 ELSE armed_at_ms END
         WHERE id = ?1",
        params![
            id,
            enabled,
            enabled.then(|| next_run(&current.trigger, now)).flatten(),
            now,
            change_head(conn)?,
        ],
    )?;
    if !enabled {
        conn.execute(
            "UPDATE planning_loop_runs SET status = 'skipped', error = ?2, started_at_ms = ?3, ended_at_ms = ?3
             WHERE loop_id = ?1 AND status = 'queued'",
            params![id, "Skipped: the automation was turned off.", now],
        )?;
    }
    get(conn, id)
}

pub(crate) fn delete(conn: &Connection, id: &str) -> Result<WorkLoop> {
    let current = get(conn, id)?;
    conn.execute("DELETE FROM planning_loop_runs WHERE loop_id = ?1", [id])?;
    conn.execute(
        "DELETE FROM planning_loop_versions WHERE loop_id = ?1",
        [id],
    )?;
    conn.execute("DELETE FROM planning_loops WHERE id = ?1", [id])?;
    Ok(current)
}

struct Queue<'a> {
    loop_id: &'a str,
    version: i64,
    cause_key: String,
    cause: String,
    task_id: Option<String>,
    meeting_id: Option<String>,
}

fn queue(conn: &Connection, queued: Queue<'_>, now: i64) -> Result<Option<String>> {
    let id = uuid::Uuid::new_v4().to_string();
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO planning_loop_runs
            (id, loop_id, version, cause_key, cause, task_id, meeting_id, status, queued_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8)",
        params![
            id,
            queued.loop_id,
            queued.version,
            queued.cause_key,
            queued.cause,
            queued.task_id,
            queued.meeting_id,
            now,
        ],
    )?;
    Ok((inserted == 1).then_some(id))
}

pub(crate) fn run_now(
    conn: &Connection,
    id: &str,
    operation_id: &str,
    now: i64,
) -> Result<WorkLoopRun> {
    let current = get(conn, id)?;
    let queued = queue(
        conn,
        Queue {
            loop_id: &current.id,
            version: current.version,
            cause_key: format!("manual:{operation_id}"),
            cause: "Run now".to_owned(),
            task_id: None,
            meeting_id: None,
        },
        now,
    )?
    .ok_or(Refusal::OperationConflict)?;
    run(conn, &queued)
}

struct Change {
    seq: i64,
    entity: String,
    entity_id: String,
    task_id: Option<String>,
    action: String,
    updated_from: Value,
}

impl Change {
    fn had(&self, field: &str) -> bool {
        self.updated_from.get(field).is_some()
    }

    fn task_update(&self) -> bool {
        self.entity == "task" && self.action == "update"
    }

    fn task(&self) -> Option<&str> {
        self.task_id
            .as_deref()
            .or((self.entity == "task").then_some(self.entity_id.as_str()))
    }
}

fn changes_after(conn: &Connection, cursor: i64) -> Result<Vec<Change>> {
    let mut statement = conn.prepare(
        "SELECT seq, entity, entity_id, task_id, action, updated_from
         FROM planning_changes WHERE seq > ?1 ORDER BY seq LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![cursor, CHANGE_PAGE], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(seq, entity, entity_id, task_id, action, updated_from)| {
            let updated_from = match updated_from {
                Some(text) => serde_json::from_str(&text)?,
                None => Value::Null,
            };
            Ok(Change {
                seq,
                entity,
                entity_id,
                task_id,
                action,
                updated_from,
            })
        })
        .collect()
}

fn current(conn: &Connection, change: &Change, field: &str) -> Result<Value> {
    let id = change.entity_id.as_str();
    let text = |sql: &str| -> Result<Value> {
        Ok(conn
            .query_row(sql, [id], |row| row.get::<_, Option<String>>(0))
            .optional()?
            .flatten()
            .map_or(Value::Null, Value::String))
    };
    match (change.entity.as_str(), field) {
        ("task", "status") => text("SELECT status FROM planning_tasks WHERE id = ?1"),
        ("task", "owner") => text(
            "SELECT CASE WHEN owner_is_self THEN 'me' ELSE owner_person_id END FROM planning_tasks WHERE id = ?1",
        ),
        ("task", "delegate") => text("SELECT agent FROM planning_task_doers WHERE task_id = ?1"),
        ("task", "labels") => {
            let mut statement = conn
                .prepare("SELECT label_id FROM planning_task_labels WHERE task_id = ?1")?;
            let labels = statement
                .query_map([id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Value::from(labels))
        }
        ("project", "status") => text("SELECT status FROM planning_projects WHERE id = ?1"),
        _ => Ok(Value::Null),
    }
}

fn after(conn: &Connection, change: &Change, field: &str) -> Result<Value> {
    let later: Option<String> = conn
        .query_row(
            "SELECT updated_from FROM planning_changes
             WHERE entity = ?1 AND entity_id = ?2 AND seq > ?3 AND action = 'update'
               AND json_type(updated_from, '$.' || ?4) IS NOT NULL
             ORDER BY seq LIMIT 1",
            params![change.entity, change.entity_id, change.seq, field],
            |row| row.get(0),
        )
        .optional()?;
    match later {
        Some(text) => Ok(serde_json::from_str::<Value>(&text)?
            .get(field)
            .cloned()
            .unwrap_or(Value::Null)),
        None => current(conn, change, field),
    }
}

fn ids(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

pub(crate) fn status_words(status: &str) -> String {
    match status {
        "not_started" => "Todo".to_owned(),
        "in_progress" => "In progress".to_owned(),
        other => {
            let mut words = other.replace('_', " ");
            if let Some(first) = words.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            words
        }
    }
}

fn label_name(conn: &Connection, id: &str) -> Result<String> {
    Ok(conn
        .query_row(
            "SELECT name FROM planning_labels WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "a label".to_owned()))
}

fn what_happened(
    conn: &Connection,
    trigger: &LoopTrigger,
    change: &Change,
) -> Result<Option<String>> {
    Ok(match trigger {
        LoopTrigger::TaskCreated if change.entity == "task" && change.action == "create" => {
            Some("was created".to_owned())
        }
        LoopTrigger::StatusChanged { to } if change.task_update() && change.had("status") => {
            let now = after(conn, change, "status")?;
            let now = now.as_str().unwrap_or_default();
            let wanted = to.is_none_or(|to| to.as_str() == now);
            wanted.then(|| format!("moved to {}", status_words(now)))
        }
        LoopTrigger::LabelAdded { label_id } if change.task_update() && change.had("labels") => {
            let before = change
                .updated_from
                .get("labels")
                .cloned()
                .unwrap_or(Value::Null);
            let now = after(conn, change, "labels")?;
            let before = ids(&before);
            let added: Vec<&str> = ids(&now)
                .into_iter()
                .filter(|id| !before.contains(id))
                .collect();
            let hit = match label_id {
                Some(label) => added.iter().find(|id| **id == label.as_str()).copied(),
                None => added.first().copied(),
            };
            match hit {
                Some(label) => Some(format!("got the label {}", label_name(conn, label)?)),
                None => None,
            }
        }
        LoopTrigger::Assigned if change.task_update() => {
            let delegated = change.had("delegate") && !after(conn, change, "delegate")?.is_null();
            let assigned = change.had("owner") && !after(conn, change, "owner")?.is_null();
            if delegated {
                Some("was delegated".to_owned())
            } else if assigned {
                Some("was assigned".to_owned())
            } else {
                None
            }
        }
        LoopTrigger::CommentAdded
            if change.entity == "comment"
                && change.action == "create"
                && change.task_id.is_some() =>
        {
            Some("got a comment".to_owned())
        }
        LoopTrigger::ProjectStatusChanged { to }
            if change.entity == "project" && change.action == "update" && change.had("status") =>
        {
            let now = after(conn, change, "status")?;
            let now = now.as_str().unwrap_or_default();
            to.is_none_or(|to| to.as_str() == now)
                .then(|| format!("moved to {}", status_words(now)))
        }
        _ => None,
    })
}

fn fired(
    conn: &Connection,
    trigger: &LoopTrigger,
    conditions: &LoopConditions,
    change: &Change,
) -> Result<Option<(String, Option<String>)>> {
    let Some(happened) = what_happened(conn, trigger, change)? else {
        return Ok(None);
    };
    if let LoopTrigger::ProjectStatusChanged { .. } = trigger {
        if !conditions.projects.is_empty() && !conditions.projects.contains(&change.entity_id) {
            return Ok(None);
        }
        let name: Option<String> = conn
            .query_row(
                "SELECT title FROM planning_projects WHERE id = ?1",
                [&change.entity_id],
                |row| row.get(0),
            )
            .optional()?;
        return Ok(name.map(|name| (format!("Project {name} {happened}"), None)));
    }
    let Some(task_id) = change.task() else {
        return Ok(None);
    };
    let task: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT number, status, project_id FROM planning_tasks WHERE id = ?1",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((number, status, project_id)) = task else {
        return Ok(None);
    };
    if !conditions.statuses.is_empty()
        && !conditions
            .statuses
            .iter()
            .any(|wanted| wanted.as_str() == status)
    {
        return Ok(None);
    }
    if !conditions.projects.is_empty()
        && !project_id.is_some_and(|project| conditions.projects.contains(&project))
    {
        return Ok(None);
    }
    if !conditions.labels.is_empty() {
        let labels = current(
            conn,
            &Change {
                seq: 0,
                entity: "task".to_owned(),
                entity_id: task_id.to_owned(),
                task_id: None,
                action: String::new(),
                updated_from: Value::Null,
            },
            "labels",
        )?;
        if !ids(&labels)
            .iter()
            .any(|label| conditions.labels.iter().any(|wanted| wanted == label))
        {
            return Ok(None);
        }
    }
    Ok(Some((
        format!("S-{number} {happened}"),
        Some(task_id.to_owned()),
    )))
}

pub(crate) fn enqueue(conn: &mut Connection, now: i64) -> Result<usize> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let queued = enqueue_in(&tx, now)?;
    tx.commit()?;
    Ok(queued)
}

fn enqueue_in(tx: &Transaction<'_>, now: i64) -> Result<usize> {
    let mut queued = 0;
    let armed = {
        let mut statement = tx.prepare(
            "SELECT id, version, trigger_json, conditions_json, next_run_at_ms, change_cursor
             FROM planning_loops WHERE enabled = 1",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, version, trigger, conditions, next_run_at_ms, cursor) in armed {
        let trigger: LoopTrigger = json(&trigger)?;
        let conditions: LoopConditions = json(&conditions)?;
        if let Some(due) = next_run_at_ms.filter(|due| *due <= now) {
            if queue(
                tx,
                Queue {
                    loop_id: &id,
                    version,
                    cause_key: format!("schedule:{due}"),
                    cause: "Scheduled".to_owned(),
                    task_id: None,
                    meeting_id: None,
                },
                now,
            )?
            .is_some()
            {
                queued += 1;
            }
            tx.execute(
                "UPDATE planning_loops SET next_run_at_ms = ?2 WHERE id = ?1",
                params![id, next_run(&trigger, now)],
            )?;
        }
        if !trigger.reads_changes() {
            continue;
        }
        let mut cursor = cursor;
        loop {
            let page = changes_after(tx, cursor)?;
            for change in &page {
                if let Some((cause, task_id)) = fired(tx, &trigger, &conditions, change)? {
                    if queue(
                        tx,
                        Queue {
                            loop_id: &id,
                            version,
                            cause_key: format!("change:{}", change.seq),
                            cause,
                            task_id,
                            meeting_id: None,
                        },
                        now,
                    )?
                    .is_some()
                    {
                        queued += 1;
                    }
                }
                cursor = change.seq;
            }
            if i64::try_from(page.len()).unwrap_or(i64::MAX) < CHANGE_PAGE {
                break;
            }
        }
        tx.execute(
            "UPDATE planning_loops SET change_cursor = ?2 WHERE id = ?1",
            params![id, cursor],
        )?;
    }
    Ok(queued)
}

pub(crate) fn meeting_ended_in(
    conn: &Connection,
    meeting_id: &str,
    title: &str,
    started_at_ms: Option<i64>,
    now: i64,
) -> Result<usize> {
    let armed = {
        let mut statement = conn.prepare(
            "SELECT id, version, armed_at_ms FROM planning_loops
             WHERE enabled = 1 AND json_extract(trigger_json, '$.kind') = 'meeting_ended'",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut queued = 0;
    for (id, version, armed_at_ms) in armed {
        if started_at_ms.is_some_and(|started| started < armed_at_ms) {
            continue;
        }
        let run = queue(
            conn,
            Queue {
                loop_id: &id,
                version,
                cause_key: format!("meeting:{meeting_id}"),
                cause: format!("Meeting ended: {title}"),
                task_id: None,
                meeting_id: Some(meeting_id.to_owned()),
            },
            now,
        )?;
        if run.is_some() {
            queued += 1;
        }
    }
    Ok(queued)
}

pub(crate) fn meeting_ended(
    app: &AppHandle,
    history: &HistoryManager,
    store: &MeetingStore,
    session_id: MeetingSessionId,
) {
    let Some((title, started_at_ms)) = ended_meeting(store, session_id) else {
        return;
    };
    let meeting_id = session_id.uuid().to_string();
    match history.with_connection(|conn| {
        meeting_ended_in(conn, &meeting_id, &title, started_at_ms, now_ms())
    }) {
        Ok(0) => {}
        Ok(_) => wake(app),
        Err(error) => log::warn!("A meeting's loops could not be queued: {error:#}"),
    }
}

struct Claimed {
    run_id: String,
    loop_name: String,
    prompt: String,
    tools: Vec<String>,
    cause: String,
    task_id: Option<String>,
    meeting_id: Option<String>,
}

/// Whether a meeting is kept on this Mac. A store that cannot say counts as
/// kept: a run's reason and its lookups leave the Mac.
fn kept_here(store: &MeetingStore, session_id: MeetingSessionId) -> bool {
    KeptLocal::read(store).map_or(true, |kept| kept.hides(session_id))
}

/// The title and start of a meeting that has ended, for the automations
/// waiting on one. None for a meeting kept on this Mac.
fn ended_meeting(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Option<(String, Option<i64>)> {
    if kept_here(store, session_id) {
        return None;
    }
    let snapshot = store.session_snapshot(session_id).ok()?;
    Some((snapshot.title, snapshot.started_at_utc_ms))
}

fn claim(conn: &mut Connection, now: i64) -> Result<Option<Claimed>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let next: Option<(
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    )> = tx
        .query_row(
            "SELECT r.id, l.name, l.prompt, l.tools_json, r.cause, r.task_id, r.meeting_id
             FROM planning_loop_runs r JOIN planning_loops l ON l.id = r.loop_id
             WHERE r.status = 'queued' ORDER BY r.queued_at_ms, r.rowid LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((run_id, loop_name, prompt, tools, cause, task_id, meeting_id)) = next else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE planning_loop_runs SET status = 'running', started_at_ms = ?2 WHERE id = ?1",
        params![run_id, now],
    )?;
    if let Some(task) = &task_id {
        if let Ok(session_id) = sessions::start(&tx, task, LOOP_AGENT, false, now) {
            tx.execute(
                "UPDATE planning_loop_runs SET session_id = ?2 WHERE id = ?1",
                params![run_id, session_id],
            )?;
        }
    }
    tx.commit()?;
    Ok(Some(Claimed {
        run_id,
        loop_name,
        prompt,
        tools: json(&tools)?,
        cause,
        task_id,
        meeting_id,
    }))
}

fn skip_waiting(conn: &Connection, reason: &str, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE planning_loop_runs SET status = 'skipped', error = ?1, started_at_ms = ?2, ended_at_ms = ?2
         WHERE status = 'queued'",
        params![reason, now],
    )?)
}

fn skip_run(conn: &Connection, run_id: &str, reason: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE planning_loop_runs SET status = 'skipped', error = ?2, started_at_ms = ?3, ended_at_ms = ?3
         WHERE id = ?1",
        params![run_id, reason, now],
    )?;
    Ok(())
}

fn recover(conn: &mut Connection, now: i64) -> Result<usize> {
    let tx = conn.transaction()?;
    let ids = tx
        .prepare(
            "UPDATE planning_loop_runs SET status = 'failed', error = 'Sona stopped during this run.', ended_at_ms = ?1
             WHERE status = 'running' RETURNING id",
        )?
        .query_map([now], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        super::inbox::automation_ended(&tx, id, now)?;
    }
    tx.commit()?;
    Ok(ids.len())
}

pub(crate) struct LoopOutcome {
    pub(crate) answer: Result<String, String>,
    pub(crate) refused: Vec<String>,
}

fn finish(conn: &mut Connection, run_id: &str, outcome: &LoopOutcome, now: i64) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let (status, output, error) = match &outcome.answer {
        Ok(answer) => (LoopRunStatus::Done, Some(answer.as_str()), None),
        Err(error) => (LoopRunStatus::Failed, None, Some(error.as_str())),
    };
    tx.execute(
        "UPDATE planning_loop_runs SET status = ?2, output = ?3, error = ?4, refused_json = ?5, ended_at_ms = ?6
         WHERE id = ?1",
        params![
            run_id,
            status.as_str(),
            output,
            error,
            serde_json::to_string(&outcome.refused)?,
            now,
        ],
    )?;
    super::inbox::automation_ended(&tx, run_id, now)?;
    let session_id: Option<String> = tx.query_row(
        "SELECT session_id FROM planning_loop_runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    if let Some(session_id) = session_id {
        let (kind, body) = match &outcome.answer {
            Ok(answer) => (WorkSessionActivityKind::Response, answer.clone()),
            Err(error) => (WorkSessionActivityKind::Error, error.clone()),
        };
        let emitted = sessions::emit(
            &tx,
            &session_id,
            LOOP_AGENT,
            kind,
            &ActivityInput {
                body: Some(body),
                ..ActivityInput::default()
            },
            None,
            now,
        );
        if let Err(error) = emitted {
            log::warn!("A loop's answer could not reach its task session: {error:#}");
        }
    }
    tx.commit()?;
    Ok(())
}

fn closed_gate(app: &AppHandle) -> Option<&'static str> {
    let settings = crate::settings::get_settings(app);
    if !settings.external_query_enabled {
        return Some(
            "Skipped: conversations are closed to agents. Turn on Settings > Agents > External access.",
        );
    }
    if !settings.external_mutations_enabled {
        return Some(
            "Skipped: agents' changes are off. Turn on Settings > Agents > External mutations.",
        );
    }
    None
}

fn turn_error(error: crate::agent_panel::ChatTurnError) -> String {
    match error {
        crate::agent_panel::ChatTurnError::Unreachable => {
            "The in-app agent isn't connected. Pair it in Settings > Agents.".to_owned()
        }
        crate::agent_panel::ChatTurnError::TimedOut => {
            "The in-app agent ran out of time.".to_owned()
        }
        crate::agent_panel::ChatTurnError::Failed
        | crate::agent_panel::ChatTurnError::ReplyNotStructured => {
            "The in-app agent didn't give a usable answer.".to_owned()
        }
    }
}

pub(crate) struct LoopWake(Arc<tokio::sync::Notify>);

pub(crate) fn wake(app: &AppHandle) {
    if let Some(wake) = app.try_state::<LoopWake>() {
        wake.0.notify_one();
    }
}

/// Whether the run's meeting is kept on this Mac by now: the series can be
/// kept between the meeting ending and its run starting.
async fn meeting_kept_here(app: &AppHandle, meeting_id: &str) -> bool {
    let Ok(uuid) = uuid::Uuid::parse_str(meeting_id) else {
        return true;
    };
    let Some(meetings) = app.try_state::<Arc<MeetingSessionManager>>() else {
        return true;
    };
    match meetings.store().await {
        Ok(store) => kept_here(&store, MeetingSessionId::from_uuid(uuid)),
        Err(_) => true,
    }
}

fn brief(claimed: &Claimed) -> String {
    let mut message = format!(
        "You are running the saved automation \"{}\".\n\n{}\n\nWhy this ran: {}",
        claimed.loop_name, claimed.prompt, claimed.cause
    );
    if let Some(meeting_id) = &claimed.meeting_id {
        message += &format!("\nThe meeting's id: {meeting_id}");
    }
    let new = claimed.tools.iter().any(|tool| tool == PROPOSE_NEW);
    let change = claimed.tools.iter().any(|tool| tool == PROPOSE_CHANGE);
    if !new && !change {
        return message;
    }
    let mut what = Vec::new();
    let mut example = Vec::new();
    if new {
        what.push("new tasks");
        example.push(NEW_EXAMPLE);
    }
    if change {
        what.push("changes to existing tasks");
        example.push(CHANGE_EXAMPLE);
    }
    message += &format!(
        "\n\nYou can't change Sona's tasks yourself, but you may propose {}. Each waits in Triage until the user accepts or declines it. To propose, end your reply with one block:\n{OFFER_FENCE}\n{{{}}}\n```\nLeave the block out when there is nothing to propose; at most {OFFER_LIMIT} of each. Priority: 0 none, 1 urgent, 2 high, 3 medium, 4 low. Status: backlog, not_started, in_progress, done or canceled. Name a task by its number, such as S-12.",
        what.join(" or "),
        example.join(", ")
    );
    message
}

const NEW_EXAMPLE: &str = r#""newTasks": [{"title": "Send Priya the deck", "details": "She asked for it before Friday.", "priority": 2}]"#;
const CHANGE_EXAMPLE: &str = r#""changes": [{"task": "S-12", "status": "done"}]"#;

/// What a run may leave in Triage: tasks it proposes, and changes to
/// existing tasks that wait for the user field by field.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Offers {
    #[serde(default)]
    new_tasks: Vec<NewTaskOffer>,
    #[serde(default)]
    changes: Vec<ChangeOffer>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NewTaskOffer {
    title: String,
    #[serde(default)]
    details: Option<String>,
    #[serde(default)]
    priority: Option<u8>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ChangeOffer {
    task: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    details: Option<String>,
    #[serde(default)]
    status: Option<WorkStatus>,
    #[serde(default)]
    priority: Option<u8>,
}

/// The reply without its proposals block, and what the block proposed.
fn split_offers(reply: &str) -> Result<(String, Offers), String> {
    let Some(start) = reply.rfind(OFFER_FENCE) else {
        return Ok((reply.trim().to_owned(), Offers::default()));
    };
    let unreadable = |reason: &dyn std::fmt::Display| {
        format!("The agent's proposals could not be read, so none were kept: {reason}.")
    };
    let block = &reply[start + OFFER_FENCE.len()..];
    let end = block.find("```").ok_or_else(|| unreadable(&"the block never ends"))?;
    let offers = serde_json::from_str(block[..end].trim()).map_err(|error| unreadable(&error))?;
    let answer = format!("{}{}", &reply[..start], &block[end + 3..]);
    Ok((answer.trim().to_owned(), offers))
}

/// Files a run's offers in one change: new tasks in Triage with the meeting
/// as their evidence, and changes as proposals, whatever Settings > Agents
/// lets agents apply. Returns a line for each task touched.
fn file_offers(
    tx: &Connection,
    meetings: Option<&Connection>,
    change: super::Change<'_>,
    meeting_id: Option<&str>,
    offers: &Offers,
) -> Result<super::Done<Vec<String>>> {
    let mut effects = super::Effects::default();
    let mut lines = Vec::new();
    for offer in &offers.new_tasks {
        let input = WorkItemInput {
            title: offer.title.clone(),
            details: offer.details.clone(),
            status: Some(WorkStatus::Triage),
            priority: offer.priority,
            project_id: None,
            milestone_id: None,
            owner: None,
            due_at_ms: None,
            label_ids: None,
            recurrence: None,
        };
        let item = super::insert_input(tx, meetings, change, &input)?;
        super::agents::credit(tx, &item.id, change.at_ms, LOOP_AGENT, super::agents::via())?;
        if let Some(session_id) = meeting_id {
            store::insert_link(
                tx,
                &store::SourceLinkDraft {
                    target_kind: EvidenceTargetKind::Task,
                    target_id: &item.id,
                    source_kind: EvidenceSourceKind::Meeting,
                    source_id: session_id,
                    at_ms: None,
                    meeting_note_id: None,
                },
                change.at_ms,
            )?;
        }
        effects.item(&item);
        lines.push(format!("New in Triage: S-{} {}", item.number, item.title));
    }
    for offer in &offers.changes {
        let patch = WorkPatch {
            title: offer.title.clone(),
            details: offer.details.clone().map(Some),
            status: offer.status,
            priority: offer.priority,
            ..WorkPatch::default()
        };
        let changed = super::agents::suggest(
            tx,
            meetings,
            change,
            LOOP_AGENT,
            &offer.task,
            &patch,
            &mut effects,
        )?;
        lines.push(match changed {
            None => format!("{} isn't a task, so nothing was proposed for it.", offer.task),
            Some(changed) if changed.proposed.is_empty() => {
                format!("S-{} already reads as proposed.", changed.item.number)
            }
            Some(changed) => format!(
                "Waiting for you on S-{}: {}",
                changed.item.number,
                changed
                    .proposed
                    .iter()
                    .map(|proposal| proposal.field.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    Ok(super::Done {
        value: lines,
        effects,
    })
}

async fn offer(app: &AppHandle, claimed: &Claimed, offers: &Offers) -> Result<Vec<String>, String> {
    let operation_id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("sona-loop-run:{}", claimed.run_id).as_bytes(),
    )
    .to_string();
    let change = super::Change::now(&operation_id, WorkActor::Agent);
    let meeting_id = claimed.meeting_id.as_deref();
    super::run_with_meetings(app, |history, meetings| {
        store::mutate(
            history,
            change.operation_id,
            "work_loop_offer",
            &(&claimed.run_id, offers),
            change.at_ms,
            |tx| file_offers(tx, meetings, change, meeting_id, offers),
        )
    })
    .await
}

/// The run's answer, with what it proposed filed and listed under it.
/// Proposals the automation's tools do not allow are refused; none are
/// kept once the consent switches closed during the run.
async fn settle_reply(
    app: &AppHandle,
    claimed: &Claimed,
    reply: &str,
    refused: &mut Vec<String>,
) -> Result<String, String> {
    let (answer, mut offers) = split_offers(reply)?;
    let allows = |tool: &str| claimed.tools.iter().any(|name| name == tool);
    if !offers.new_tasks.is_empty() && !allows(PROPOSE_NEW) {
        refused.push(PROPOSE_NEW.to_owned());
        offers.new_tasks.clear();
    }
    if !offers.changes.is_empty() && !allows(PROPOSE_CHANGE) {
        refused.push(PROPOSE_CHANGE.to_owned());
        offers.changes.clear();
    }
    if offers.new_tasks.is_empty() && offers.changes.is_empty() {
        return Ok(answer);
    }
    if offers.new_tasks.len() > OFFER_LIMIT || offers.changes.len() > OFFER_LIMIT {
        return Err(format!(
            "The agent proposed more than {OFFER_LIMIT} new tasks or changes, so none were kept."
        ));
    }
    if closed_gate(app).is_some() {
        return Err(
            "Agents' access was turned off during this run, so nothing it proposed was kept."
                .to_owned(),
        );
    }
    let filed = offer(app, claimed, &offers).await?;
    Ok(format!("{answer}\n\n{}", filed.join("\n")).trim().to_owned())
}

async fn execute(app: &AppHandle, history: &HistoryManager, claimed: Claimed) {
    if let Some(meeting_id) = &claimed.meeting_id {
        if meeting_kept_here(app, meeting_id).await {
            if let Err(error) = history
                .with_connection(|conn| skip_run(conn, &claimed.run_id, KEPT_HERE, now_ms()))
            {
                log::warn!("A loop run could not be recorded: {error:#}");
            }
            tasks_changed(app, Vec::new());
            return;
        }
    }
    let message = brief(&claimed);
    let lookups = lookups(&claimed.tools);
    let turn = crate::agent_panel::run_loop_turn(app, &message, &lookups, RUN_DEADLINE).await;
    let mut refused = turn.refused;
    let answer = match turn.answer {
        Ok(reply) => settle_reply(app, &claimed, &reply, &mut refused).await,
        Err(error) => Err(turn_error(error)),
    };
    let outcome = LoopOutcome { answer, refused };
    if let Err(error) =
        history.with_connection(|conn| finish(conn, &claimed.run_id, &outcome, now_ms()))
    {
        log::warn!("A loop run could not be recorded: {error:#}");
    }
    tasks_changed(app, claimed.task_id.into_iter().collect());
}

pub(crate) fn start(app: AppHandle) {
    let notify = Arc::new(tokio::sync::Notify::new());
    app.manage(LoopWake(Arc::clone(&notify)));
    tauri::async_runtime::spawn(async move {
        let Some(history) = app
            .try_state::<Arc<HistoryManager>>()
            .map(|state| Arc::clone(&state))
        else {
            return;
        };
        if let Err(error) = history.with_connection(|conn| recover(conn, now_ms())) {
            log::warn!("Interrupted loop runs could not be closed: {error:#}");
        }
        loop {
            match history.with_connection(|conn| enqueue(conn, now_ms())) {
                Ok(0) => {}
                Ok(_) => tasks_changed(&app, Vec::new()),
                Err(error) => log::warn!("Loops could not check their triggers: {error:#}"),
            }
            loop {
                if let Some(reason) = closed_gate(&app) {
                    match history.with_connection(|conn| skip_waiting(conn, reason, now_ms())) {
                        Ok(0) => {}
                        Ok(_) => tasks_changed(&app, Vec::new()),
                        Err(error) => log::warn!("Paused loop runs could not be marked: {error:#}"),
                    }
                    break;
                }
                match history.with_connection(|conn| claim(conn, now_ms())) {
                    Ok(Some(claimed)) => {
                        tasks_changed(&app, Vec::new());
                        execute(&app, &history, claimed).await;
                    }
                    Ok(None) => break,
                    Err(error) => {
                        log::warn!("A loop run could not start: {error:#}");
                        break;
                    }
                }
            }
            tokio::select! {
                () = notify.notified() => {}
                () = tokio::time::sleep(TICK) => {}
            }
        }
    });
}

fn change<T: DeserializeOwned + Serialize>(
    app: &AppHandle,
    operation_id: &str,
    command: &str,
    request: &impl Serialize,
    write: impl FnOnce(&Transaction<'_>, i64) -> Result<T>,
) -> Result<T, String> {
    let at_ms = now_ms();
    let value = app
        .state::<Arc<HistoryManager>>()
        .with_connection(|conn| {
            store::mutate(conn, operation_id, command, request, at_ms, |tx| {
                write(tx, at_ms)
            })
        })
        .map_err(message)?;
    tasks_changed(app, Vec::new());
    wake(app);
    Ok(value)
}

pub(crate) fn loops_read(app: &AppHandle) -> Result<WorkLoops, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        Ok(WorkLoops {
            loops: loops(conn)?,
            tools: tools(),
        })
    })
}

pub(crate) fn loop_read(app: &AppHandle, id: &str) -> Result<WorkLoopDetail, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| detail(conn, id))
}

pub(crate) fn save_as(
    app: &AppHandle,
    operation_id: &str,
    id: Option<&str>,
    input: &LoopInput,
    by: &str,
) -> Result<WorkLoop, String> {
    change(
        app,
        operation_id,
        "work_loop_save",
        &(id, input, by),
        |tx, at| save(tx, id, input, by, at),
    )
}

pub(crate) fn run_now_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> Result<WorkLoopRun, String> {
    change(app, operation_id, "work_loop_run", &id, |tx, at| {
        run_now(tx, id, operation_id, at)
    })
}

pub(crate) fn enable_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    enabled: bool,
) -> Result<WorkLoop, String> {
    change(
        app,
        operation_id,
        "work_loop_enable",
        &(id, enabled),
        |tx, at| enable(tx, id, enabled, at),
    )
}

pub(crate) fn restore_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    version: i64,
    by: &str,
) -> Result<WorkLoop, String> {
    change(
        app,
        operation_id,
        "work_loop_restore_version",
        &(id, version, by),
        |tx, at| restore(tx, id, version, by, at),
    )
}

pub(crate) fn delete_as(app: &AppHandle, operation_id: &str, id: &str) -> Result<WorkLoop, String> {
    change(app, operation_id, "work_loop_delete", &id, |tx, _| {
        delete(tx, id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::store::workflow_core_tests::{self, reviewable_meeting};
    use crate::meeting::types::MeetingOperationId;
    use crate::planning::schema;
    use crate::planning::work::inbox;
    use crate::planning::work::items::{self, NewItem};
    use crate::planning::work::types::{WorkActivityKind, WorkActor, WorkOwner, WorkPatch};

    #[test]
    fn a_task_moved_to_done_queues_one_run_however_often_the_stream_is_read() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let task = items::insert(
            &conn,
            &NewItem {
                title: "Survey the dock",
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
        .expect("a filed task");
        let watcher = save(
            &conn,
            None,
            &LoopInput {
                name: "Wrap up".to_owned(),
                prompt: "Say what the finished task delivered.".to_owned(),
                trigger: LoopTrigger::StatusChanged {
                    to: Some(WorkStatus::Done),
                },
                conditions: LoopConditions::default(),
                tools: Vec::new(),
            },
            "you",
            1,
        )
        .expect("a saved loop");

        items::apply(
            &conn,
            &task.id,
            &WorkPatch {
                status: Some(WorkStatus::Done),
                ..WorkPatch::default()
            },
            WorkActivityKind::Status,
            WorkActor::Agent,
            2,
        )
        .expect("the task done");
        let queued = [
            enqueue(&mut conn, 3).expect("a read of the stream"),
            enqueue(&mut conn, 4).expect("a second read of the stream"),
        ];
        let causes: Vec<_> = runs(&conn, &watcher.id, 10)
            .expect("the runs")
            .into_iter()
            .map(|run| (run.cause, run.task_id))
            .collect();

        assert_eq!(queued, [1, 0]);
        assert_eq!(
            causes,
            [(format!("S-{} moved to Done", task.number), Some(task.id))]
        );
    }

    #[test]
    fn a_run_cut_off_by_a_quit_posts_one_inbox_notice_however_often_sona_restarts() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let digest = save(
            &conn,
            None,
            &LoopInput {
                name: "Weekly digest".to_owned(),
                prompt: "Sum up what moved this week.".to_owned(),
                trigger: LoopTrigger::MeetingEnded,
                conditions: LoopConditions::default(),
                tools: Vec::new(),
            },
            "you",
            1,
        )
        .expect("a saved loop");
        run_now(&conn, &digest.id, "op-1", 2).expect("a queued run");
        let claimed = claim(&mut conn, 3).expect("the queue").expect("a run started");

        let closed = [
            recover(&mut conn, 4).expect("the first restart"),
            recover(&mut conn, 5).expect("the second restart"),
        ];
        inbox::automation_ended(&conn, &claimed.run_id, 6).expect("a late report of the end");
        let notices: Vec<_> = inbox::notifications(&conn, inbox::Shown::default(), 7)
            .expect("the inbox")
            .into_iter()
            .map(|notice| {
                (
                    notice.kind,
                    notice.item_id,
                    notice.to,
                    notice.automation.map(|automation| (automation.name, automation.error)),
                )
            })
            .collect();

        assert_eq!(closed, [1, 0]);
        assert_eq!(
            notices,
            [(
                inbox::NotificationKind::Automation,
                None,
                Some("failed".to_owned()),
                Some((
                    "Weekly digest".to_owned(),
                    Some("Sona stopped during this run.".to_owned())
                )),
            )]
        );
    }

    fn ended(conn: &mut Connection, loop_id: &str, answer: Result<&str, &str>, at: i64) {
        run_now(conn, loop_id, &format!("op-{at}"), at).expect("a queued run");
        let claimed = claim(conn, at).expect("the queue").expect("a run started");
        let outcome = LoopOutcome {
            answer: answer.map(str::to_owned).map_err(str::to_owned),
            refused: Vec::new(),
        };
        finish(conn, &claimed.run_id, &outcome, at).expect("the run ended");
    }

    #[test]
    fn an_automation_failing_the_same_way_again_is_one_inbox_notice_until_that_changes() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let digest = save(
            &conn,
            None,
            &LoopInput {
                name: "Weekly digest".to_owned(),
                prompt: "Sum up what moved this week.".to_owned(),
                trigger: LoopTrigger::MeetingEnded,
                conditions: LoopConditions::default(),
                tools: Vec::new(),
            },
            "you",
            1,
        )
        .expect("a saved loop");

        ended(&mut conn, &digest.id, Err("offline"), 10);
        ended(&mut conn, &digest.id, Err("offline"), 20);
        ended(&mut conn, &digest.id, Ok("Summed up."), 30);
        ended(&mut conn, &digest.id, Err("offline"), 40);
        ended(&mut conn, &digest.id, Err("offline"), 50);
        ended(&mut conn, &digest.id, Err("no quota"), 60);
        let notices: Vec<_> = inbox::notifications(&conn, inbox::Shown::default(), 70)
            .expect("the inbox")
            .into_iter()
            .map(|notice| {
                (
                    notice.to,
                    notice.automation.and_then(|automation| automation.error),
                    notice.earlier.len(),
                )
            })
            .collect();
        let unread = inbox::notifications(
            &conn,
            inbox::Shown {
                unread_only: true,
                ..inbox::Shown::default()
            },
            70,
        )
        .expect("the unread")
        .len();

        let failed = |error: &str, earlier| (Some("failed".to_owned()), Some(error.to_owned()), earlier);
        assert_eq!(
            notices,
            [
                failed("no quota", 0),
                failed("offline", 1),
                (Some("done".to_owned()), None, 0),
                failed("offline", 1),
            ]
        );
        assert_eq!(unread, 4);
    }

    #[test]
    fn a_project_that_changes_status_queues_a_run_named_for_it() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let watcher = save(
            &conn,
            None,
            &LoopInput {
                name: "Project moves".to_owned(),
                prompt: "Say what the project's new status means.".to_owned(),
                trigger: LoopTrigger::ProjectStatusChanged { to: None },
                conditions: LoopConditions::default(),
                tools: Vec::new(),
            },
            "you",
            1,
        )
        .expect("a saved loop");
        let project = store::create_project(
            &mut conn,
            "Harbor launch",
            None,
            &uuid::Uuid::new_v4().to_string(),
            2,
        )
        .expect("a project");
        store::update_project(
            &mut conn,
            &project.id,
            project.revision,
            ProjectStatus::Completed,
            &uuid::Uuid::new_v4().to_string(),
            3,
        )
        .expect("the project completed");

        let queued = enqueue(&mut conn, 4).expect("a read of the stream");
        let causes: Vec<_> = runs(&conn, &watcher.id, 10)
            .expect("the runs")
            .into_iter()
            .map(|run| run.cause)
            .collect();

        assert_eq!(queued, 1);
        assert_eq!(causes, ["Project Harbor launch moved to Completed"]);
    }

    #[test]
    fn a_meeting_kept_on_this_mac_is_never_named_to_an_automation() {
        let (_directory, meetings) = workflow_core_tests::store();
        let open = reviewable_meeting(&meetings, "Pricing sync", 1);
        let kept = reviewable_meeting(&meetings, "Board prep", 1);
        meetings
            .remember_calendar_facts(
                kept,
                &CalendarEventSummary {
                    event_key: "board#1".to_owned(),
                    series_key: "board".to_owned(),
                    title: "Board prep".to_owned(),
                    attendee_count: 2,
                    start_utc_ms: 1,
                    end_utc_ms: 2,
                    attendees: Vec::new(),
                    notes: None,
                    calendar_name: None,
                    location: None,
                    url: None,
                    place: None,
                },
            )
            .expect("the meeting's series");
        meetings
            .set_series_remote_opt_out(
                &MeetingSeriesRemoteOptOutSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: "board".to_owned(),
                    remote_intelligence_opt_out: true,
                    expected_revision: 0,
                },
                1,
            )
            .expect("the series kept on this Mac");

        assert_eq!(
            ended_meeting(&meetings, open).map(|(title, _)| title).as_deref(),
            Some("Pricing sync")
        );
        assert_eq!(ended_meeting(&meetings, kept), None);
    }

    #[test]
    fn an_automation_turned_off_runs_none_of_its_waiting_runs() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let nightly = save(
            &conn,
            None,
            &LoopInput {
                name: "Wrap up".to_owned(),
                prompt: "Say what the finished task delivered.".to_owned(),
                trigger: LoopTrigger::StatusChanged {
                    to: Some(WorkStatus::Done),
                },
                conditions: LoopConditions::default(),
                tools: Vec::new(),
            },
            "you",
            1,
        )
        .expect("a saved loop");
        run_now(&conn, &nightly.id, &uuid::Uuid::new_v4().to_string(), 2).expect("a waiting run");
        enable(&conn, &nightly.id, false, 3).expect("the automation turned off");

        let claimed = claim(&mut conn, 4).expect("a look for waiting runs");
        let statuses: Vec<_> = runs(&conn, &nightly.id, 10)
            .expect("the runs")
            .into_iter()
            .map(|run| run.status.as_str())
            .collect();

        assert!(claimed.is_none());
        assert_eq!(statuses, ["skipped"]);
    }

    #[test]
    fn a_run_leaves_new_tasks_in_triage_and_changes_waiting_for_you() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let task = items::insert(
            &conn,
            &NewItem {
                title: "Survey the dock",
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
        .expect("a filed task");
        let reply = format!(
            "Two follow-ups.\n\n{OFFER_FENCE}\n{{\"newTasks\":[{{\"title\":\"Send the map notes\"}}],\"changes\":[{{\"task\":\"S-{}\",\"status\":\"done\"}}]}}\n```",
            task.number
        );
        let (answer, offers) = split_offers(&reply).expect("readable proposals");
        let meeting = uuid::Uuid::new_v4().to_string();
        let operation = uuid::Uuid::new_v4().to_string();

        let tx = conn.transaction().expect("a transaction");
        let filed = file_offers(
            &tx,
            None,
            super::super::Change {
                operation_id: &operation,
                actor: WorkActor::Agent,
                at_ms: 5,
            },
            Some(&meeting),
            &offers,
        )
        .expect("the proposals filed");
        tx.commit().expect("the proposals kept");

        let (new_id, new_status): (String, String) = conn
            .query_row(
                "SELECT id, status FROM planning_tasks WHERE title = 'Send the map notes'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the proposed task");
        let evidence: i64 = conn
            .query_row(
                "SELECT count(*) FROM planning_evidence_links WHERE target_id = ?1 AND source_kind = 'meeting'",
                [&new_id],
                |row| row.get(0),
            )
            .expect("the task's evidence");
        let waiting: Vec<(String, String)> = conn
            .prepare("SELECT field, agent FROM planning_task_proposals WHERE task_id = ?1 AND state = 'open'")
            .expect("a proposal read")
            .query_map([&task.id], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("the proposals")
            .collect::<rusqlite::Result<_>>()
            .expect("the proposals");

        assert_eq!(answer, "Two follow-ups.");
        assert_eq!(new_status, "triage");
        assert_eq!(evidence, 1);
        assert_eq!(items::item(&conn, &task.id).expect("the task").status, WorkStatus::NotStarted);
        assert_eq!(waiting, [("status".to_owned(), LOOP_AGENT.to_owned())]);
        assert_eq!(filed.value.len(), 2);
    }
}
