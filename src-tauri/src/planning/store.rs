use super::types::{
    EvidenceLink, EvidenceSourceKind, EvidenceTargetKind, Milestone, MilestonePulse,
    PlanningCalendarItem, PlanningCalendarKind, PlanningTask, Project, ProjectDetail,
    ProjectOwnerCount, ProjectPulse, ProjectStatus, ProjectSummary, TaskStatus,
};
use super::work::items::{self as work_items, NewItem};
use super::work::types::{WorkActor, WorkOwner, WorkStatus};
use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::{
    params, params_from_iter, Connection, OptionalExtension, Row, Transaction, TransactionBehavior,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use uuid::Uuid;

const PROJECT_COLUMNS: &str = "id, title, description, status, revision, created_at_ms, updated_at_ms, lead_person_id, target_at_ms, hue, archived_at_ms";
const MILESTONE_COLUMNS: &str =
    "id, project_id, title, due_at_ms, status, revision, created_at_ms, updated_at_ms, description";
const TASK_COLUMNS: &str = "id, project_id, milestone_id, title, details, due_at_ms, owner_person_id, owner_is_self, status, revision, created_at_ms, updated_at_ms";
const EVIDENCE_COLUMNS: &str = "id, target_kind, target_id, source_kind, source_id, at_ms, revision, created_at_ms, meeting_note_id, end_ms, caption";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Refusal {
    Invalid,
    NotFound,
    StaleRevision,
    OperationConflict,
    InvalidMembership,
    SourceUnavailable,
    OwnerUnavailable,
    ExistingCommitment,
    CaptureActive,
    SourceChanged,
    AlreadyTracked,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid planner input",
            Self::NotFound => "planner record no longer exists",
            Self::StaleRevision => "planner revision changed",
            Self::OperationConflict => "operation id was already used for another change",
            Self::InvalidMembership => "milestone does not belong to this project",
            Self::SourceUnavailable => "source is unavailable",
            Self::OwnerUnavailable => "selected person is unavailable",
            Self::ExistingCommitment => "existing meeting work must be linked, not duplicated",
            Self::CaptureActive => "planning is paused while recording",
            Self::SourceChanged => "source changed during review",
            Self::AlreadyTracked => "loop is already tracked as planner work",
        })
    }
}

impl std::error::Error for Refusal {}

pub(crate) fn uuid(value: &str) -> Result<String> {
    Uuid::parse_str(value)
        .map(|id| id.to_string())
        .map_err(|_| Refusal::Invalid.into())
}

fn title(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Refusal::Invalid.into());
    }
    Ok(value)
}

fn revision(value: i64) -> Result<i64> {
    if value < 1 {
        return Err(Refusal::Invalid.into());
    }
    value.checked_add(1).ok_or_else(|| Refusal::Invalid.into())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredMutation<T> {
    Committed(T),
    Refused(Refusal),
}

impl<T> StoredMutation<T> {
    fn into_result(self) -> Result<T> {
        match self {
            Self::Committed(record) => Ok(record),
            Self::Refused(refusal) => Err(refusal.into()),
        }
    }
}

pub(super) fn mutate<T: DeserializeOwned + Serialize>(
    conn: &mut Connection,
    operation_id: &str,
    command: &str,
    request: &impl Serialize,
    now_ms: i64,
    write: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let operation_id = uuid(operation_id)?;
    let request_json = serde_json::to_string(request)?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let receipt: Option<(String, String, String)> = transaction
        .query_row(
            "SELECT command, request_json, response_json FROM planning_operations WHERE operation_id = ?1",
            [&operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((stored_command, stored_request, response_json)) = receipt {
        if stored_command != command || stored_request != request_json {
            return Err(Refusal::OperationConflict.into());
        }
        let response: StoredMutation<T> = serde_json::from_str(&response_json)?;
        transaction.commit()?;
        return response.into_result();
    }
    transaction.execute_batch("SAVEPOINT planning_write")?;
    let response = match write(&transaction) {
        Ok(record) => {
            transaction.execute_batch("RELEASE planning_write")?;
            StoredMutation::Committed(record)
        }
        Err(error) => {
            let Some(refusal) = error.downcast_ref::<Refusal>().copied() else {
                return Err(error);
            };
            transaction.execute_batch("ROLLBACK TO planning_write; RELEASE planning_write")?;
            StoredMutation::Refused(refusal)
        }
    };
    transaction.execute(
        "INSERT INTO planning_operations (operation_id, command, request_json, response_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![operation_id, command, request_json, serde_json::to_string(&response)?, now_ms],
    )?;
    transaction.commit()?;
    response.into_result()
}

fn read_project(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        revision: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        lead_person_id: row.get(7)?,
        member_person_ids: Vec::new(),
        target_at_ms: row.get(8)?,
        hue: row.get(9)?,
        archived_at_ms: row.get(10)?,
    })
}

fn read_milestone(row: &Row<'_>) -> rusqlite::Result<Milestone> {
    Ok(Milestone {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        due_at_ms: row.get(3)?,
        status: row.get(4)?,
        revision: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
        description: row.get(8)?,
    })
}

fn read_task(row: &Row<'_>) -> rusqlite::Result<PlanningTask> {
    Ok(PlanningTask {
        id: row.get(0)?,
        project_id: row.get(1)?,
        milestone_id: row.get(2)?,
        title: row.get(3)?,
        details: row.get(4)?,
        due_at_ms: row.get(5)?,
        owner_person_id: row.get(6)?,
        owner_is_self: row.get(7)?,
        owner_available: true,
        status: row.get::<_, WorkStatus>(8)?.legacy(),
        revision: row.get(9)?,
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
    })
}

fn read_evidence(row: &Row<'_>) -> rusqlite::Result<EvidenceLink> {
    Ok(EvidenceLink {
        id: row.get(0)?,
        target_kind: row.get(1)?,
        target_id: row.get(2)?,
        source_kind: row.get(3)?,
        source_id: row.get(4)?,
        at_ms: row.get(5)?,
        end_ms: row.get(9)?,
        meeting_note_id: row.get(8)?,
        caption: row.get(10)?,
        revision: row.get(6)?,
        created_at_ms: row.get(7)?,
        available: false,
        title: None,
    })
}

fn attach_members(conn: &Connection, projects: &mut [&mut Project]) -> Result<()> {
    let mut members = conn.prepare(
        "SELECT project_id, person_id FROM planning_project_members ORDER BY project_id, person_id",
    )?;
    let mut by_project: HashMap<String, Vec<String>> = HashMap::new();
    for row in members.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (project_id, person_id) = row?;
        by_project.entry(project_id).or_default().push(person_id);
    }
    for project in projects {
        project.member_person_ids = by_project.remove(&project.id).unwrap_or_default();
    }
    Ok(())
}

pub(crate) fn project(conn: &Connection, id: &str) -> Result<Project> {
    let mut project = conn
        .query_row(
            &format!("SELECT {PROJECT_COLUMNS} FROM planning_projects WHERE id = ?1"),
            [id],
            read_project,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    attach_members(conn, &mut [&mut project])?;
    Ok(project)
}

pub(crate) fn milestone(conn: &Connection, id: &str) -> Result<Milestone> {
    conn.query_row(
        &format!("SELECT {MILESTONE_COLUMNS} FROM planning_milestones WHERE id = ?1"),
        [id],
        read_milestone,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

/// A task as the project screens read it; tracker items outside any project
/// are not tasks there.
pub(crate) fn task(conn: &Connection, id: &str) -> Result<PlanningTask> {
    conn.query_row(
        &format!(
            "SELECT {TASK_COLUMNS} FROM planning_tasks WHERE id = ?1 AND project_id IS NOT NULL"
        ),
        [id],
        read_task,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

pub(crate) fn projects(conn: &Connection) -> Result<Vec<ProjectSummary>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROJECT_COLUMNS},
            (SELECT count(*) FROM planning_tasks t WHERE t.project_id = p.id AND t.archived_at_ms IS NULL
                AND t.status IN ('triage', 'backlog', 'not_started', 'in_progress')),
            (SELECT count(*) FROM planning_tasks t WHERE t.project_id = p.id AND t.archived_at_ms IS NULL
                AND t.status = 'done'),
            (SELECT min(due_at_ms) FROM (
                SELECT due_at_ms FROM planning_tasks WHERE project_id = p.id AND archived_at_ms IS NULL
                    AND status IN ('triage', 'backlog', 'not_started', 'in_progress')
                UNION ALL
                SELECT due_at_ms FROM planning_milestones WHERE project_id = p.id AND status IN ('not_started', 'in_progress')
            ))
         FROM planning_projects p WHERE p.archived_at_ms IS NULL
         ORDER BY p.updated_at_ms DESC, p.id"
    ))?;
    let rows = statement.query_map([], |row| {
        Ok(ProjectSummary {
            project: read_project(row)?,
            open_task_count: row.get(11)?,
            completed_task_count: row.get(12)?,
            next_due_at_ms: row.get(13)?,
        })
    })?;
    let mut summaries = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    attach_members(
        conn,
        &mut summaries
            .iter_mut()
            .map(|summary| &mut summary.project)
            .collect::<Vec<_>>(),
    )?;
    Ok(summaries)
}

/// Every project, put away or not, for the command line.
pub(crate) fn all_projects(conn: &Connection) -> Result<Vec<Project>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROJECT_COLUMNS} FROM planning_projects ORDER BY lower(title), id"
    ))?;
    let mut projects = statement
        .query_map([], read_project)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    attach_members(conn, &mut projects.iter_mut().collect::<Vec<_>>())?;
    Ok(projects)
}

/// Projects still taking work, the one worked in most recently first: a task
/// or milestone that moved counts as much as the project's own row.
pub(crate) fn projects_by_activity(conn: &Connection) -> Result<Vec<Project>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROJECT_COLUMNS} FROM planning_projects p
         WHERE p.status != 'completed' AND p.archived_at_ms IS NULL
         ORDER BY max(
             p.updated_at_ms,
             coalesce((SELECT max(t.updated_at_ms) FROM planning_tasks t WHERE t.project_id = p.id), 0),
             coalesce((SELECT max(m.updated_at_ms) FROM planning_milestones m WHERE m.project_id = p.id), 0)
         ) DESC, p.id"
    ))?;
    let rows = statement.query_map([], read_project)?;
    let mut projects = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    attach_members(conn, &mut projects.iter_mut().collect::<Vec<_>>())?;
    Ok(projects)
}

/// A project's saved rows. Serializes exactly as `ProjectDetail` did before it
/// carried a pulse, so fingerprints the quiet review already stored still match.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectRecords {
    pub(crate) project: Project,
    pub(crate) milestones: Vec<Milestone>,
    pub(crate) tasks: Vec<PlanningTask>,
    pub(crate) evidence: Vec<EvidenceLink>,
}

pub(crate) fn records(conn: &Connection, id: &str) -> Result<ProjectRecords> {
    let project = project(conn, id)?;
    let mut statement = conn.prepare(&format!(
        "SELECT {MILESTONE_COLUMNS} FROM planning_milestones WHERE project_id = ?1
         ORDER BY due_at_ms IS NULL, due_at_ms, created_at_ms, id"
    ))?;
    let milestones = statement
        .query_map([id], read_milestone)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLUMNS} FROM planning_tasks WHERE project_id = ?1 AND archived_at_ms IS NULL
         ORDER BY due_at_ms IS NULL, due_at_ms, created_at_ms, id"
    ))?;
    let tasks = statement
        .query_map([id], read_task)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {EVIDENCE_COLUMNS} FROM planning_evidence_links WHERE
            (target_kind = 'project' AND target_id = ?1) OR
            (target_kind = 'milestone' AND target_id IN (SELECT id FROM planning_milestones WHERE project_id = ?1)) OR
            (target_kind = 'task' AND target_id IN
                (SELECT id FROM planning_tasks WHERE project_id = ?1 AND archived_at_ms IS NULL))
         ORDER BY created_at_ms, id"
    ))?;
    let evidence = statement
        .query_map([id], read_evidence)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ProjectRecords {
        project,
        milestones,
        tasks,
        evidence,
    })
}

/// The sources linked to one task, oldest first.
pub(crate) fn task_evidence(conn: &Connection, task_id: &str) -> Result<Vec<EvidenceLink>> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT {EVIDENCE_COLUMNS} FROM planning_evidence_links
         WHERE target_kind = 'task' AND target_id = ?1
         ORDER BY created_at_ms, id"
    ))?;
    let evidence = statement
        .query_map([task_id], read_evidence)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(evidence)
}

/// The page for one project: rows the caller has resolved, and their pulse as
/// of `now_ms`. Meeting times come from `meetings` in one query; without it the
/// rows must hold no meeting evidence.
pub(crate) fn detail(
    records: ProjectRecords,
    meetings: Option<&Connection>,
    now_ms: i64,
) -> Result<ProjectDetail> {
    let now = Local
        .timestamp_millis_opt(now_ms)
        .single()
        .ok_or_else(|| anyhow::anyhow!("clock is outside the supported calendar"))?;
    let today_ms = crate::analytics::local_days_start_utc_ms(now, 1)?;
    let week_ms = crate::analytics::local_days_start_utc_ms(now, 7)?;
    let pulse = pulse(&records, meetings, today_ms, week_ms)?;
    Ok(ProjectDetail {
        project: records.project,
        milestones: records.milestones,
        tasks: records.tasks,
        evidence: records.evidence,
        pulse,
    })
}

const fn is_open(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::NotStarted | TaskStatus::InProgress)
}

fn pulse(
    records: &ProjectRecords,
    meetings: Option<&Connection>,
    today_ms: i64,
    week_ms: i64,
) -> Result<ProjectPulse> {
    let mut done = 0_usize;
    let mut open = 0_usize;
    let mut overdue = 0_usize;
    let mut owned = HashMap::<&str, usize>::new();
    for task in &records.tasks {
        if task.status == TaskStatus::Done {
            done += 1;
        }
        if !is_open(task.status) {
            continue;
        }
        open += 1;
        if task.due_at_ms.is_some_and(|due| due < today_ms) {
            overdue += 1;
        }
        if let Some(person_id) = task.owner_person_id.as_deref() {
            *owned.entry(person_id).or_default() += 1;
        }
    }
    let next_due_at_ms = records
        .tasks
        .iter()
        .filter(|task| is_open(task.status))
        .filter_map(|task| task.due_at_ms)
        .chain(
            records
                .milestones
                .iter()
                .filter(|milestone| is_open(milestone.status))
                .filter_map(|milestone| milestone.due_at_ms),
        )
        .filter(|due| *due >= today_ms)
        .min();
    let mut owners = owned
        .into_iter()
        .map(|(person_id, open_tasks)| {
            Ok(ProjectOwnerCount {
                person_id: person_id.to_owned(),
                open_tasks: u32::try_from(open_tasks)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    owners.sort_by(|left, right| {
        right
            .open_tasks
            .cmp(&left.open_tasks)
            .then_with(|| left.person_id.cmp(&right.person_id))
    });
    let held = held_meetings(records, meetings)?;
    let mut milestone_meetings = HashMap::<&str, HashMap<&str, i64>>::new();
    for link in &records.evidence {
        if link.target_kind != EvidenceTargetKind::Milestone
            || link.source_kind != EvidenceSourceKind::Meeting
        {
            continue;
        }
        if let Some(&at_ms) = held.get(link.source_id.as_str()) {
            milestone_meetings
                .entry(link.target_id.as_str())
                .or_default()
                .insert(link.source_id.as_str(), at_ms);
        }
    }
    let milestones = records
        .milestones
        .iter()
        .filter_map(|milestone| {
            let linked = milestone_meetings.get(milestone.id.as_str())?;
            let last_meeting_at_ms = linked.values().copied().max()?;
            Some((milestone, linked.len(), last_meeting_at_ms))
        })
        .map(|(milestone, linked, last_meeting_at_ms)| {
            Ok(MilestonePulse {
                milestone_id: milestone.id.clone(),
                meetings: u32::try_from(linked)?,
                last_meeting_at_ms,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let this_week = held.values().filter(|at_ms| **at_ms >= week_ms).count();
    Ok(ProjectPulse {
        done: u32::try_from(done)?,
        open: u32::try_from(open)?,
        overdue: u32::try_from(overdue)?,
        next_due_at_ms,
        meetings_this_week: u32::try_from(this_week)?,
        owners,
        milestones,
    })
}

/// When each meeting cited by the rows started, for meetings that still exist.
fn held_meetings(
    records: &ProjectRecords,
    meetings: Option<&Connection>,
) -> Result<HashMap<String, i64>> {
    let ids = records
        .evidence
        .iter()
        .filter(|link| link.source_kind == EvidenceSourceKind::Meeting)
        .map(|link| link.source_id.as_str())
        .collect::<HashSet<_>>();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let meetings = meetings.ok_or(Refusal::SourceUnavailable)?;
    let mut statement = meetings.prepare(&format!(
        "SELECT id, COALESCE(started_at_utc_ms, created_at_utc_ms) FROM meeting_sessions
         WHERE phase != 'deleting' AND id IN ({})",
        vec!["?"; ids.len()].join(", ")
    ))?;
    let held = statement
        .query_map(params_from_iter(ids), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(held)
}

pub(crate) fn create_project(
    conn: &mut Connection,
    title_text: &str,
    description: Option<&str>,
    operation_id: &str,
    now_ms: i64,
) -> Result<Project> {
    mutate(
        conn,
        operation_id,
        "create_project",
        &(title_text, description),
        now_ms,
        |tx| insert_project(tx, title_text, description, now_ms),
    )
}

pub(super) fn insert_project(
    conn: &Connection,
    title_text: &str,
    description: Option<&str>,
    now_ms: i64,
) -> Result<Project> {
    let title = title(title_text)?;
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_projects (id, title, description, status, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, 'active', 1, ?4, ?4)",
        params![id, title, description, now_ms],
    )?;
    project(conn, &id)
}

pub(crate) fn create_milestone(
    conn: &mut Connection,
    project_id: &str,
    title_text: &str,
    due_at_ms: Option<i64>,
    operation_id: &str,
    now_ms: i64,
) -> Result<Milestone> {
    mutate(
        conn,
        operation_id,
        "create_milestone",
        &(project_id, title_text, due_at_ms),
        now_ms,
        |tx| insert_milestone(tx, project_id, title_text, due_at_ms, now_ms),
    )
}

pub(super) fn insert_milestone(
    conn: &Connection,
    project_id: &str,
    title_text: &str,
    due_at_ms: Option<i64>,
    now_ms: i64,
) -> Result<Milestone> {
    let project_id = uuid(project_id)?;
    project(conn, &project_id)?;
    let title = title(title_text)?;
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_milestones (id, project_id, title, due_at_ms, status, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, 'not_started', 1, ?5, ?5)",
        params![id, project_id, title, due_at_ms, now_ms],
    )?;
    milestone(conn, &id)
}

#[derive(Debug, Serialize)]
pub(crate) struct TaskDraft<'a> {
    pub project_id: &'a str,
    pub milestone_id: Option<&'a str>,
    pub title: &'a str,
    pub details: Option<&'a str>,
    pub due_at_ms: Option<i64>,
    pub owner_person_id: Option<&'a str>,
    pub owner_is_self: bool,
}

pub(crate) fn create_task(
    conn: &mut Connection,
    draft: &TaskDraft<'_>,
    operation_id: &str,
    now_ms: i64,
    validate_owner: impl FnOnce(&str) -> Result<String>,
) -> Result<PlanningTask> {
    mutate(conn, operation_id, "create_task", draft, now_ms, |tx| {
        insert_task(tx, draft, now_ms, validate_owner)
    })
}

pub(super) fn insert_task(
    conn: &Connection,
    draft: &TaskDraft<'_>,
    now_ms: i64,
    validate_owner: impl FnOnce(&str) -> Result<String>,
) -> Result<PlanningTask> {
    if draft.owner_is_self && draft.owner_person_id.is_some() {
        return Err(Refusal::Invalid.into());
    }
    let owner = match draft.owner_person_id {
        Some(id) => WorkOwner::Person {
            person_id: validate_owner(&uuid(id)?)?,
        },
        None if draft.owner_is_self => WorkOwner::Myself,
        None => WorkOwner::Nobody,
    };
    let item = work_items::insert(
        conn,
        &NewItem {
            title: draft.title,
            details: draft.details,
            status: WorkStatus::NotStarted,
            priority: 0,
            project_id: Some(draft.project_id),
            milestone_id: draft.milestone_id,
            owner: &owner,
            due_at_ms: draft.due_at_ms,
            label_ids: &[],
            source_loop_id: None,
            recurrence: None,
        },
        WorkActor::Me,
        now_ms,
    )?;
    task(conn, &item.id)
}

pub(crate) fn update_project(
    conn: &mut Connection,
    id: &str,
    expected_revision: i64,
    status: ProjectStatus,
    operation_id: &str,
    now_ms: i64,
) -> Result<Project> {
    mutate(
        conn,
        operation_id,
        "update_project",
        &(id, expected_revision, status),
        now_ms,
        |tx| {
            let id = uuid(id)?;
            let next_revision = revision(expected_revision)?;
            let current = project(tx, &id)?;
            if current.revision != expected_revision {
                return Err(Refusal::StaleRevision.into());
            }
            tx.execute(
                "UPDATE planning_projects SET status = ?2, revision = ?3, updated_at_ms = ?4
             WHERE id = ?1 AND revision = ?5",
                params![
                    id,
                    status.as_str(),
                    next_revision,
                    now_ms,
                    expected_revision
                ],
            )?;
            project(tx, &id)
        },
    )
}

pub(crate) fn update_milestone(
    conn: &mut Connection,
    id: &str,
    expected_revision: i64,
    status: TaskStatus,
    operation_id: &str,
    now_ms: i64,
) -> Result<Milestone> {
    mutate(
        conn,
        operation_id,
        "update_milestone",
        &(id, expected_revision, status),
        now_ms,
        |tx| {
            let id = uuid(id)?;
            let next_revision = revision(expected_revision)?;
            let current = milestone(tx, &id)?;
            project(tx, &current.project_id)?;
            if current.revision != expected_revision {
                return Err(Refusal::StaleRevision.into());
            }
            tx.execute(
                "UPDATE planning_milestones SET status = ?2, revision = ?3, updated_at_ms = ?4
             WHERE id = ?1 AND revision = ?5",
                params![
                    id,
                    status.as_str(),
                    next_revision,
                    now_ms,
                    expected_revision
                ],
            )?;
            milestone(tx, &id)
        },
    )
}

pub(crate) fn target_project(
    conn: &Connection,
    kind: EvidenceTargetKind,
    id: &str,
) -> Result<String> {
    match kind {
        EvidenceTargetKind::Project => Ok(project(conn, id)?.id),
        EvidenceTargetKind::Milestone => {
            let milestone = milestone(conn, id)?;
            project(conn, &milestone.project_id)?;
            Ok(milestone.project_id)
        }
        EvidenceTargetKind::Task => {
            let task = task(conn, id)?;
            project(conn, &task.project_id)?;
            Ok(task.project_id)
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SourceLinkDraft<'a> {
    pub target_kind: EvidenceTargetKind,
    pub target_id: &'a str,
    pub source_kind: EvidenceSourceKind,
    pub source_id: &'a str,
    pub at_ms: Option<i64>,
    pub meeting_note_id: Option<&'a str>,
}

pub(crate) fn link_source(
    conn: &mut Connection,
    draft: &SourceLinkDraft<'_>,
    operation_id: &str,
    now_ms: i64,
    validate_source: impl FnOnce(&Connection) -> Result<()>,
) -> Result<EvidenceLink> {
    mutate(conn, operation_id, "link_source", draft, now_ms, |tx| {
        let target_id = uuid(draft.target_id)?;
        target_project(tx, draft.target_kind, &target_id)?;
        if draft.at_ms.is_some_and(|offset| offset < 0) {
            return Err(Refusal::Invalid.into());
        }
        validate_source(tx)?;
        insert_link(tx, draft, now_ms)
    })
}

pub(super) fn insert_link(
    conn: &Connection,
    draft: &SourceLinkDraft<'_>,
    now_ms: i64,
) -> Result<EvidenceLink> {
    let target_id = uuid(draft.target_id)?;
    target_project(conn, draft.target_kind, &target_id)?;
    if draft.at_ms.is_some_and(|offset| offset < 0) {
        return Err(Refusal::Invalid.into());
    }
    if let Some(note_id) = draft.meeting_note_id {
        if draft.source_kind != EvidenceSourceKind::Meeting
            || draft.at_ms.is_some()
            || uuid(note_id)? != note_id
        {
            return Err(Refusal::Invalid.into());
        }
    }
    conn.execute(
        "INSERT INTO planning_evidence_links
            (id, target_kind, target_id, source_kind, source_id, at_ms, revision, created_at_ms, meeting_note_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)
         ON CONFLICT DO NOTHING",
        params![
            Uuid::new_v4().to_string(),
            draft.target_kind.as_str(),
            target_id,
            draft.source_kind.as_str(),
            draft.source_id,
            draft.at_ms,
            now_ms,
            draft.meeting_note_id
        ],
    )?;
    Ok(conn.query_row(
        &format!(
            "SELECT {EVIDENCE_COLUMNS} FROM planning_evidence_links
            WHERE target_kind = ?1 AND target_id = ?2 AND source_kind = ?3 AND source_id = ?4
              AND at_ms IS ?5 AND meeting_note_id IS ?6"
        ),
        params![
            draft.target_kind.as_str(),
            target_id,
            draft.source_kind.as_str(),
            draft.source_id,
            draft.at_ms,
            draft.meeting_note_id
        ],
        read_evidence,
    )?)
}

pub(crate) fn calendar(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<PlanningCalendarItem>> {
    if start_ms >= end_ms {
        return Err(Refusal::Invalid.into());
    }
    let mut statement = conn.prepare(
        "SELECT id, 'task', title, project_id, due_at_ms FROM planning_tasks
            WHERE due_at_ms >= ?1 AND due_at_ms < ?2 AND archived_at_ms IS NULL
              AND status IN ('triage', 'backlog', 'not_started', 'in_progress')
         UNION ALL
         SELECT id, 'milestone', title, project_id, due_at_ms FROM planning_milestones
            WHERE due_at_ms >= ?1 AND due_at_ms < ?2 AND status IN ('not_started', 'in_progress')
         ORDER BY 5, 2, 1",
    )?;
    let rows = statement.query_map(params![start_ms, end_ms], |row| {
        let kind: String = row.get(1)?;
        Ok(PlanningCalendarItem {
            id: row.get(0)?,
            kind: if kind == "task" {
                PlanningCalendarKind::Task
            } else {
                PlanningCalendarKind::Milestone
            },
            title: row.get(2)?,
            project_id: row.get(3)?,
            due_at_ms: row.get(4)?,
            meeting_id: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_counts_work_due_today_as_open_not_overdue() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        super::super::schema::apply_all(&conn)?;
        let now = Local::now();
        let today_ms = crate::analytics::local_days_start_utc_ms(now, 1)?;
        let project = insert_project(&conn, "Launch", None, today_ms)?;
        let owner = Uuid::new_v4().to_string();
        let task = |title: &str, due_at_ms: i64, owner_person_id: Option<&str>| {
            insert_task(
                &conn,
                &TaskDraft {
                    project_id: &project.id,
                    milestone_id: None,
                    title,
                    details: None,
                    due_at_ms: Some(due_at_ms),
                    owner_person_id,
                    owner_is_self: false,
                },
                today_ms,
                |id| Ok(id.to_owned()),
            )
        };
        task("Send the brief", today_ms - 3_600_000, Some(&owner))?;
        let due_today = task("Book the room", today_ms + 1, None)?;
        let canceled = task("Order banners", today_ms - 3_600_000, None)?;
        let done = task("Draft the outline", today_ms - 3_600_000, None)?;
        conn.execute(
            "UPDATE planning_tasks SET status = 'canceled' WHERE id = ?1",
            [&canceled.id],
        )?;
        conn.execute(
            "UPDATE planning_tasks SET status = 'done' WHERE id = ?1",
            [&done.id],
        )?;

        let detail = detail(records(&conn, &project.id)?, None, now.timestamp_millis())?;

        assert_eq!(
            detail.pulse,
            ProjectPulse {
                done: 1,
                open: 2,
                overdue: 1,
                next_due_at_ms: due_today.due_at_ms,
                meetings_this_week: 0,
                owners: vec![ProjectOwnerCount {
                    person_id: owner,
                    open_tasks: 1,
                }],
                milestones: Vec::new(),
            }
        );
        Ok(())
    }
}
