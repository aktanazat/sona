//! Agents on the tracker: the agent working an item beside its owner, the
//! changes an agent proposes while Settings > Agents asks to review them,
//! and the prompt that hands an item to an agent.

use super::capture::{self, WorkCaptureSource};
use super::items;
use super::sync::{self, MeetingReader};
use super::types::{
    WorkActivityKind, WorkActor, WorkDelegation, WorkDoer, WorkItem, WorkItemDetail, WorkItemInput,
    WorkOwner, WorkPatch, WorkProposal, WorkProposalField, WorkStatus, WorkSuggestionField,
    WorkSuggestionValue,
};
use super::{
    edit, now_ms, run_history, run_with_meetings, unique, with_meetings, Change, Done, Effects,
};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::planning::sources;
use crate::planning::store::{self, Refusal};
use crate::settings::WorkAutonomy;
use anyhow::Result as StorageResult;
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// What Sona calls an agent that gave no name.
pub(crate) const UNNAMED: &str = "Agent";

/// How a delegation prompt begins, before the item's handle: a watched
/// session whose prompt carries it is working that item.
pub(crate) const PROMPT_MARK: &str = "Sona task";

/// Agents Sona knows, by how their names start once lowercased and cut to
/// letters and digits; the first match wins.
const KNOWN: [(&str, &str); 7] = [
    ("claudeai", "Claude"),
    ("claudedesktop", "Claude"),
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("ohmypi", "OMP"),
    ("omp", "OMP"),
    ("grok", "Grok"),
];

/// Names the operation a watched session's claim runs under, so one session
/// naming one item claims it once.
const FOLLOW_NAMESPACE: Uuid = Uuid::from_u128(0x3c1d_8a5e_7b20_4f6a_9d14_e2b7_5a90_c613);

/// The name an agent goes by: a known agent by its product's name, any
/// other as it called itself, cut to 40 characters.
pub(crate) fn agent_name(given: Option<&str>) -> String {
    let given = given.map(str::trim).unwrap_or_default();
    let key: String = given
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect();
    if let Some((_, name)) = KNOWN.iter().find(|(start, _)| key.starts_with(start)) {
        return (*name).to_owned();
    }
    if given.is_empty() {
        UNNAMED.to_owned()
    } else {
        given.chars().take(40).collect()
    }
}

fn same(one: &str, other: &str) -> bool {
    one.eq_ignore_ascii_case(other)
}

/// Whether agents' changes apply at once or wait for the user.
pub(crate) fn autonomy(app: &AppHandle) -> WorkAutonomy {
    crate::settings::get_settings(app).work_agent_changes
}

/// An agent making a change, and whether its changes apply or wait.
#[derive(Clone, Copy)]
struct Agent<'a> {
    name: &'a str,
    autonomy: WorkAutonomy,
}

fn read_doer(row: &Row<'_>) -> rusqlite::Result<WorkDoer> {
    Ok(WorkDoer {
        agent: row.get(0)?,
        since_ms: row.get(1)?,
    })
}

/// The agent on each item an agent works, by item id.
pub(super) fn doers(conn: &Connection) -> StorageResult<HashMap<String, WorkDoer>> {
    let mut statement = conn.prepare("SELECT agent, since_ms, task_id FROM planning_task_doers")?;
    let doers = statement
        .query_map([], |row| Ok((row.get::<_, String>(2)?, read_doer(row)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(doers)
}

fn doer(conn: &Connection, task_id: &str) -> StorageResult<Option<WorkDoer>> {
    Ok(conn
        .query_row(
            "SELECT agent, since_ms FROM planning_task_doers WHERE task_id = ?1",
            [task_id],
            read_doer,
        )
        .optional()?)
}

/// Puts `agent` on an item, or takes whoever works it off. An agent already
/// on it keeps the time it started.
fn set_doer(
    conn: &Connection,
    task_id: &str,
    agent: Option<&str>,
    now_ms: i64,
) -> StorageResult<Option<WorkDoer>> {
    let Some(agent) = agent else {
        conn.execute(
            "DELETE FROM planning_task_doers WHERE task_id = ?1",
            [task_id],
        )?;
        return Ok(None);
    };
    if let Some(current) = doer(conn, task_id)?.filter(|current| same(&current.agent, agent)) {
        return Ok(Some(current));
    }
    conn.execute(
        "INSERT INTO planning_task_doers (task_id, agent, since_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(task_id) DO UPDATE SET agent = excluded.agent, since_ms = excluded.since_ms",
        params![task_id, agent, now_ms],
    )?;
    Ok(Some(WorkDoer {
        agent: agent.to_owned(),
        since_ms: now_ms,
    }))
}

/// Names `agent` on the history entries agents wrote for an item since
/// `since_ms` that name no agent yet.
fn credit(conn: &Connection, task_id: &str, since_ms: i64, agent: &str) -> StorageResult<()> {
    conn.execute(
        "INSERT INTO planning_activity_agents (activity_id, agent)
         SELECT a.id, ?3 FROM planning_task_activity a
         WHERE a.task_id = ?1 AND a.at_ms >= ?2 AND a.actor = 'agent'
           AND NOT EXISTS (SELECT 1 FROM planning_activity_agents n WHERE n.activity_id = a.id)",
        params![task_id, since_ms, agent],
    )?;
    Ok(())
}

/// [`credit`] for a change an agent made through the tracker's own
/// functions, which write history without knowing which agent it was.
pub(crate) fn credit_since(
    app: &AppHandle,
    ids: &[String],
    since_ms: i64,
    agent: &str,
) -> Result<(), String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        let tx = conn.transaction()?;
        for id in ids {
            credit(&tx, id, since_ms, agent)?;
        }
        tx.commit()?;
        Ok(())
    })?;
    super::tasks_changed(app, ids.to_vec());
    Ok(())
}

const PROPOSAL_COLUMNS: &str = "id, task_id, agent, field, from_value, patch_json, created_at_ms";

/// One stored proposal, and the patch accepting it applies.
fn read_proposal(row: &Row<'_>) -> rusqlite::Result<(WorkProposal, WorkPatch)> {
    let field: WorkProposalField = row.get(3)?;
    let patch_json: String = row.get(5)?;
    let patch: WorkPatch = serde_json::from_str(&patch_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok((
        WorkProposal {
            id: row.get(0)?,
            item_id: row.get(1)?,
            agent: row.get(2)?,
            field,
            from: row.get(4)?,
            to: proposed(field, &patch),
            created_at_ms: row.get(6)?,
        },
        patch,
    ))
}

/// Every item's open proposals, oldest first, by item id.
pub(super) fn open_proposals(
    conn: &Connection,
) -> StorageResult<BTreeMap<String, Vec<WorkProposal>>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROPOSAL_COLUMNS} FROM planning_task_proposals
         WHERE state = 'open' ORDER BY created_at_ms, rowid"
    ))?;
    let mut open: BTreeMap<String, Vec<WorkProposal>> = BTreeMap::new();
    for proposal in statement.query_map([], read_proposal)? {
        let (proposal, _) = proposal?;
        open.entry(proposal.item_id.clone())
            .or_default()
            .push(proposal);
    }
    Ok(open)
}

/// One item's open proposals, oldest first.
pub(super) fn item_proposals(conn: &Connection, task_id: &str) -> StorageResult<Vec<WorkProposal>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROPOSAL_COLUMNS} FROM planning_task_proposals
         WHERE task_id = ?1 AND state = 'open' ORDER BY created_at_ms, rowid"
    ))?;
    let proposals = statement
        .query_map([task_id], |row| {
            read_proposal(row).map(|(proposal, _)| proposal)
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(proposals)
}

/// The value a proposal's patch gives its field, in history form.
fn proposed(field: WorkProposalField, patch: &WorkPatch) -> Option<String> {
    match field {
        WorkProposalField::Title => patch.title.clone(),
        WorkProposalField::Details => patch.details.clone().flatten(),
        WorkProposalField::Status => patch.status.map(|status| status.as_str().to_owned()),
        WorkProposalField::Priority => patch.priority.map(|priority| priority.to_string()),
        WorkProposalField::Owner => patch.owner.as_ref().map(WorkOwner::history_value),
        WorkProposalField::Due => patch.due_at_ms.flatten().map(|due| due.to_string()),
        WorkProposalField::Project => patch.project_id.clone().flatten(),
        WorkProposalField::Milestone => patch.milestone_id.clone().flatten(),
        WorkProposalField::Labels => patch.label_ids.as_ref().map(|ids| ids.join(",")),
        WorkProposalField::Snoozed => patch
            .snoozed_until_ms
            .flatten()
            .map(|until| until.to_string()),
        WorkProposalField::Recurrence => patch.recurrence.clone().flatten(),
        WorkProposalField::Cycle => patch.cycle.flatten().map(|cycle| cycle.to_string()),
    }
}

/// One field a patch would change: the patch for that field alone, and the
/// field's value before it, in history form.
struct FieldChange {
    field: WorkProposalField,
    patch: WorkPatch,
    from: Option<String>,
}

fn ms(value: Option<i64>) -> Option<String> {
    value.map(|value| value.to_string())
}

fn sorted(ids: &[String]) -> Vec<String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    ids
}

/// The fields `patch` would change on `item`, one by one, with the change
/// it makes to the item's repeat. A new project carries the patch's
/// milestone with it, since moving the project clears the milestone.
fn field_changes(
    item: &WorkItem,
    repeat: Option<items::RepeatChange>,
    patch: &WorkPatch,
) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    let mut add = |field, from: Option<String>, patch: WorkPatch| {
        if proposed(field, &patch) != from {
            changes.push(FieldChange { field, patch, from });
        }
    };
    if let Some(title) = &patch.title {
        add(
            WorkProposalField::Title,
            Some(item.title.clone()),
            WorkPatch {
                title: Some(title.trim().to_owned()),
                ..WorkPatch::default()
            },
        );
    }
    if let Some(details) = &patch.details {
        let details = details
            .as_deref()
            .map(str::trim)
            .filter(|details| !details.is_empty())
            .map(str::to_owned);
        add(
            WorkProposalField::Details,
            item.details.clone(),
            WorkPatch {
                details: Some(details),
                ..WorkPatch::default()
            },
        );
    }
    if let Some(status) = patch.status {
        add(
            WorkProposalField::Status,
            Some(item.status.as_str().to_owned()),
            WorkPatch {
                status: Some(status),
                ..WorkPatch::default()
            },
        );
    }
    if let Some(priority) = patch.priority {
        add(
            WorkProposalField::Priority,
            Some(item.priority.to_string()),
            WorkPatch {
                priority: Some(priority),
                ..WorkPatch::default()
            },
        );
    }
    if let Some(owner) = &patch.owner {
        add(
            WorkProposalField::Owner,
            Some(item.owner().history_value()),
            WorkPatch {
                owner: Some(owner.clone()),
                ..WorkPatch::default()
            },
        );
    }
    if let Some(due) = patch.due_at_ms {
        add(
            WorkProposalField::Due,
            ms(item.due_at_ms),
            WorkPatch {
                due_at_ms: Some(due),
                ..WorkPatch::default()
            },
        );
    }
    match (&patch.project_id, &patch.milestone_id) {
        (Some(project), milestone) if *project != item.project_id => add(
            WorkProposalField::Project,
            item.project_id.clone(),
            WorkPatch {
                project_id: Some(project.clone()),
                milestone_id: milestone.clone(),
                ..WorkPatch::default()
            },
        ),
        (_, Some(milestone)) => add(
            WorkProposalField::Milestone,
            item.milestone_id.clone(),
            WorkPatch {
                milestone_id: Some(milestone.clone()),
                ..WorkPatch::default()
            },
        ),
        (_, None) => {}
    }
    if let Some(label_ids) = &patch.label_ids {
        add(
            WorkProposalField::Labels,
            Some(sorted(&item.label_ids).join(",")),
            WorkPatch {
                label_ids: Some(sorted(label_ids)),
                ..WorkPatch::default()
            },
        );
    }
    if let Some(snoozed) = patch.snoozed_until_ms {
        add(
            WorkProposalField::Snoozed,
            ms(item.snoozed_until_ms),
            WorkPatch {
                snoozed_until_ms: Some(snoozed),
                ..WorkPatch::default()
            },
        );
    }
    if let Some(repeat) = repeat {
        add(
            WorkProposalField::Recurrence,
            repeat.from,
            WorkPatch {
                recurrence: Some(repeat.to),
                ..WorkPatch::default()
            },
        );
    }
    if let Some(cycle) = patch.cycle {
        add(
            WorkProposalField::Cycle,
            item.cycle.map(|cycle| cycle.to_string()),
            WorkPatch {
                cycle: Some(cycle),
                ..WorkPatch::default()
            },
        );
    }
    changes
}

/// Leaves one field change waiting for the user, in place of whatever the
/// same agent proposed for that field before.
fn propose(
    conn: &Connection,
    task_id: &str,
    agent: &str,
    change: FieldChange,
    now_ms: i64,
) -> StorageResult<WorkProposal> {
    conn.execute(
        "DELETE FROM planning_task_proposals
         WHERE task_id = ?1 AND field = ?2 AND state = 'open' AND agent = ?3 COLLATE NOCASE",
        params![task_id, change.field.as_str(), agent],
    )?;
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_task_proposals
             (id, task_id, agent, field, from_value, patch_json, state, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', ?7)",
        params![
            id,
            task_id,
            agent,
            change.field.as_str(),
            change.from,
            serde_json::to_string(&change.patch)?,
            now_ms
        ],
    )?;
    Ok(WorkProposal {
        id,
        item_id: task_id.to_owned(),
        agent: agent.to_owned(),
        field: change.field,
        to: proposed(change.field, &change.patch),
        from: change.from,
        created_at_ms: now_ms,
    })
}

/// What an agent's change did: the item after the part that applied, and
/// the fields left waiting for the user.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AgentChange {
    pub(crate) item: WorkItem,
    pub(crate) proposed: Vec<WorkProposal>,
}

/// The statuses an agent moves the item it works between on its own.
const fn working(status: WorkStatus) -> bool {
    matches!(
        status,
        WorkStatus::Backlog | WorkStatus::NotStarted | WorkStatus::InProgress
    )
}

/// One agent's patch to one item. With agents' changes set to apply, all of
/// it applies; set to suggest, every field waits for the user except the
/// status of an item the agent works, moving between Backlog, Todo and In
/// progress.
fn agent_edit(
    tx: &Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    agent: Agent<'_>,
    id: &str,
    patch: &WorkPatch,
    effects: &mut Effects,
) -> StorageResult<AgentChange> {
    let before = items::item(tx, id)?;
    let (direct, waiting) = match agent.autonomy {
        WorkAutonomy::Apply => (patch.clone(), Vec::new()),
        WorkAutonomy::Suggest => {
            let works_it = doer(tx, id)?.is_some_and(|doer| same(&doer.agent, agent.name));
            let mut direct = WorkPatch::default();
            let mut waiting = Vec::new();
            let repeat = items::repeat_change(tx, id, patch, change.at_ms)?;
            for field in field_changes(&before, repeat, patch) {
                match field.patch.status {
                    Some(status) if works_it && working(before.status) && working(status) => {
                        direct.status = Some(status);
                    }
                    _ => waiting.push(field),
                }
            }
            (direct, waiting)
        }
    };
    let item = if direct == WorkPatch::default() {
        before
    } else {
        let item = edit(tx, meetings, change, id, &direct, effects)?;
        credit(tx, id, change.at_ms, agent.name)?;
        item
    };
    let proposed = waiting
        .into_iter()
        .map(|field| propose(tx, id, agent.name, field, change.at_ms))
        .collect::<StorageResult<Vec<_>>>()?;
    if !proposed.is_empty() {
        effects.item(&item);
    }
    Ok(AgentChange { item, proposed })
}

/// One agent's change to one item it read at `expected_revision`, and the
/// page or file it keeps beside the item as evidence of the work.
#[derive(Clone, Copy)]
pub(crate) struct ItemChange<'a> {
    pub id: &'a str,
    pub expected_revision: i64,
    pub patch: &'a WorkPatch,
    pub link: Option<&'a str>,
}

/// The page or file `evidence` names, when it names one: an absolute
/// http, https or file address.
pub(crate) fn evidence_link(evidence: &str) -> Option<String> {
    sources::link_url(evidence).ok()
}

fn update(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    agent: Agent<'_>,
    edit: ItemChange<'_>,
) -> StorageResult<Done<AgentChange>> {
    store::mutate(
        history,
        change.operation_id,
        "work_agent_update",
        &(
            edit.id,
            edit.expected_revision,
            edit.patch,
            edit.link,
            agent.name,
        ),
        change.at_ms,
        |tx| {
            let id = store::uuid(edit.id)?;
            if items::item(tx, &id)?.revision != edit.expected_revision {
                return Err(Refusal::StaleRevision.into());
            }
            let mut effects = Effects::default();
            let value = agent_edit(tx, meetings, change, agent, &id, edit.patch, &mut effects)?;
            if let Some(url) = edit.link {
                let link = WorkCaptureSource::Link {
                    url: url.to_owned(),
                    title: None,
                };
                capture::attach(tx, &id, &link, change.at_ms)?;
                effects.item(&value.item);
            }
            Ok(Done { value, effects })
        },
    )
}

/// An agent's [`work_update`](super::work_update), under Settings > Agents.
pub(crate) async fn update_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    edit: ItemChange<'_>,
) -> Result<AgentChange, String> {
    let agent = Agent {
        name: agent,
        autonomy: autonomy(app),
    };
    let change = Change::now(operation_id, WorkActor::Agent);
    run_with_meetings(app, |history, meetings| {
        update(history, meetings, change, agent, edit)
    })
    .await
}

fn bulk_update(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    agent: Agent<'_>,
    ids: &[String],
    patch: &WorkPatch,
) -> StorageResult<Done<Vec<AgentChange>>> {
    store::mutate(
        history,
        change.operation_id,
        "work_agent_bulk_update",
        &(ids, patch, agent.name),
        change.at_ms,
        |tx| {
            let mut effects = Effects::default();
            let mut value = Vec::new();
            for id in unique(ids)? {
                value.push(agent_edit(
                    tx,
                    meetings,
                    change,
                    agent,
                    &id,
                    patch,
                    &mut effects,
                )?);
            }
            Ok(Done { value, effects })
        },
    )
}

/// An agent's [`work_bulk_update`](super::work_bulk_update), under
/// Settings > Agents: every item takes its part, or none does.
pub(crate) async fn bulk_update_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    ids: &[String],
    patch: &WorkPatch,
) -> Result<Vec<AgentChange>, String> {
    let agent = Agent {
        name: agent,
        autonomy: autonomy(app),
    };
    let change = Change::now(operation_id, WorkActor::Agent);
    run_with_meetings(app, |history, meetings| {
        bulk_update(history, meetings, change, agent, ids, patch)
    })
    .await
}

/// An agent's new item. While agents' changes wait for the user it starts
/// in triage, where the user accepts or declines it.
pub(crate) async fn create_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    input: &WorkItemInput,
) -> Result<WorkItem, String> {
    let item = match autonomy(app) {
        WorkAutonomy::Apply => super::create_as(app, WorkActor::Agent, operation_id, input).await,
        WorkAutonomy::Suggest => {
            let input = WorkItemInput {
                status: Some(WorkStatus::Triage),
                ..input.clone()
            };
            super::create_as(app, WorkActor::Agent, operation_id, &input).await
        }
    }?;
    credit_since(
        app,
        std::slice::from_ref(&item.id),
        item.created_at_ms,
        agent,
    )?;
    Ok(item)
}

/// Puts an agent on an item: a Todo or Backlog item moves to In progress
/// with the agent on it, its owner unchanged. An item the agent already
/// works in progress stays as it is. One another agent works, or one in
/// progress without an agent, is refused.
fn claim(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    agent: &str,
    id: &str,
    expected_revision: Option<i64>,
) -> StorageResult<Done<WorkItem>> {
    store::mutate(
        history,
        change.operation_id,
        "work_agent_claim",
        &(id, expected_revision, agent),
        change.at_ms,
        |tx| {
            let id = store::uuid(id)?;
            let item = items::item(tx, &id)?;
            if expected_revision.is_some_and(|expected| expected != item.revision) {
                return Err(Refusal::StaleRevision.into());
            }
            let current = doer(tx, &id)?;
            let works_it = current
                .as_ref()
                .is_some_and(|doer| same(&doer.agent, agent));
            if current.is_some() && !works_it {
                return Err(Refusal::StaleRevision.into());
            }
            if item.archived_at_ms.is_some() {
                return Err(Refusal::Invalid.into());
            }
            let mut effects = Effects::default();
            let value = match item.status {
                WorkStatus::Backlog | WorkStatus::NotStarted => {
                    set_doer(tx, &id, Some(agent), change.at_ms)?;
                    let started = WorkPatch {
                        status: Some(WorkStatus::InProgress),
                        ..WorkPatch::default()
                    };
                    let item = edit(tx, meetings, change, &id, &started, &mut effects)?;
                    credit(tx, &id, change.at_ms, agent)?;
                    item
                }
                WorkStatus::InProgress if works_it => item,
                _ => return Err(Refusal::Invalid.into()),
            };
            Ok(Done { value, effects })
        },
    )
}

/// An agent's claim on an item; see [`claim`].
pub(crate) async fn claim_as(
    app: &AppHandle,
    agent: &str,
    operation_id: &str,
    id: &str,
    expected_revision: Option<i64>,
) -> Result<WorkItem, String> {
    let change = Change::now(operation_id, WorkActor::Agent);
    run_with_meetings(app, |history, meetings| {
        claim(history, meetings, change, agent, id, expected_revision)
    })
    .await
}

/// The item a watched session named: `S-12`, or an item id.
fn named_item(conn: &Connection, task: &str) -> StorageResult<Option<String>> {
    match task
        .strip_prefix("S-")
        .and_then(|number| number.parse::<i64>().ok())
    {
        Some(number) => Ok(conn
            .query_row(
                "SELECT id FROM planning_tasks WHERE number = ?1",
                [number],
                |row| row.get(0),
            )
            .optional()?),
        None => Ok(Some(store::uuid(task)?)),
    }
}

/// Claims the item a prompt [`work_delegate`] wrote names, for the watched
/// agent session it was given to. A refusal changes nothing and the session
/// goes on either way.
pub(crate) fn follow_session(app: &AppHandle, agent: &str, session_id: &str, task: &str) {
    let app = app.clone();
    let agent = agent_name(Some(agent));
    let operation_id = Uuid::new_v5(
        &FOLLOW_NAMESPACE,
        format!("{session_id}\0{task}").as_bytes(),
    )
    .to_string();
    let task = task.to_owned();
    tauri::async_runtime::spawn(async move {
        let named = crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
            named_item(conn, &task)
        });
        let id = match named {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(message) => {
                log::info!("A watched session's task could not be read: {message}");
                return;
            }
        };
        if let Err(message) = claim_as(&app, &agent, &operation_id, &id, None).await {
            log::info!("A watched session's task was left as it was: {message}");
        }
    });
}

fn priority_name(priority: u8) -> Option<&'static str> {
    match priority {
        1 => Some("Urgent"),
        2 => Some("High"),
        3 => Some("Medium"),
        4 => Some("Low"),
        _ => None,
    }
}

/// The words that hand an item to an agent: what to do, what was said, and
/// the commands that report back. The first line names the item, so Sona
/// can tell which watched session took it.
fn prompt(detail: &WorkItemDetail, agent: &str, autonomy: WorkAutonomy) -> String {
    let row = &detail.item;
    let item = &row.item;
    let handle = format!("S-{}", item.number);
    let mut text = format!("{PROMPT_MARK} {handle}: {}\n", item.title);
    if let Some(details) = item
        .details
        .as_deref()
        .map(str::trim)
        .filter(|details| !details.is_empty())
    {
        let _ = write!(text, "\n{details}\n");
    }
    if let Some(quote) = &row.source_quote {
        let speaker = row
            .source_speaker
            .as_deref()
            .map_or_else(String::new, |speaker| format!("{speaker}: "));
        let meeting = row
            .source_meeting_title
            .as_deref()
            .map_or_else(|| "a meeting".to_owned(), |title| format!("\"{title}\""));
        let _ = write!(text, "\nSaid in {meeting}: {speaker}\"{quote}\"\n");
    }
    let mut facts = Vec::new();
    if let Some(priority) = priority_name(item.priority) {
        facts.push(format!("Priority: {priority}."));
    }
    if let Some(due) = item
        .due_at_ms
        .and_then(|due| Local.timestamp_millis_opt(due).single())
    {
        facts.push(format!("Due {}.", due.format("%A, %B %-d")));
    }
    if let Some(project) = &row.project_title {
        facts.push(format!("Project: {project}."));
    }
    if !facts.is_empty() {
        let _ = write!(text, "\n{}\n", facts.join(" "));
    }
    let (reviews, confirms) = match autonomy {
        WorkAutonomy::Suggest => (" (the user reviews the change)", " (the user confirms it)"),
        WorkAutonomy::Apply => ("", ""),
    };
    let _ = write!(
        text,
        "\nReport back to Sona as you work:\n\
         - When you start: sona work claim {handle} --agent \"{agent}\"\n\
         - To read it again: sona work show {handle}\n\
         - To change it: sona work update {handle} --agent \"{agent}\"{reviews}\n\
         - When it is finished: sona work done {handle} --evidence \"<link or one line>\" --agent \"{agent}\"{confirms}\n\
         Sona's MCP tools do the same: sona_work_claim, sona_work_get, sona_work_update, sona_work_done.\n"
    );
    text
}

/// Hands an item to an agent, or takes it back when `agent` is none: the
/// agent becomes the item's doer, and the prompt to paste into it comes
/// back.
#[tauri::command]
#[specta::specta]
pub async fn work_delegate(
    app: AppHandle,
    operation_id: String,
    id: String,
    agent: Option<String>,
) -> Result<WorkDelegation, String> {
    let agent = agent.map(|agent| agent_name(Some(&agent)));
    let at_ms = now_ms();
    let doer = run_history(&app, |history| {
        store::mutate(
            history,
            &operation_id,
            "work_delegate",
            &(&id, &agent),
            at_ms,
            |tx| {
                let id = store::uuid(&id)?;
                let mut effects = Effects::default();
                effects.item(&items::item(tx, &id)?);
                Ok(Done {
                    value: set_doer(tx, &id, agent.as_deref(), at_ms)?,
                    effects,
                })
            },
        )
    })?;
    let Some(doer) = doer else {
        return Ok(WorkDelegation {
            doer: None,
            prompt: None,
        });
    };
    let history = app.state::<Arc<HistoryManager>>();
    let meetings = app.state::<Arc<MeetingSessionManager>>();
    let user_name = meetings.user_name();
    let detail = with_meetings(&history, &meetings, |history, meeting_conn| {
        super::detail(history, meeting_conn, user_name.as_deref(), &id)
    })
    .await?;
    Ok(WorkDelegation {
        prompt: Some(prompt(&detail, &doer.agent, autonomy(&app))),
        doer: Some(doer),
    })
}

/// The open proposals named, oldest first. One decided or gone refuses the
/// whole decision: the user decided on what they saw.
fn open_named(conn: &Connection, ids: &[String]) -> StorageResult<Vec<(WorkProposal, WorkPatch)>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROPOSAL_COLUMNS} FROM planning_task_proposals WHERE id = ?1 AND state = 'open'"
    ))?;
    let mut open = Vec::with_capacity(ids.len());
    for id in unique(ids)? {
        open.push(
            statement
                .query_row([&id], read_proposal)
                .optional()?
                .ok_or(Refusal::StaleRevision)?,
        );
    }
    open.sort_by_key(|(proposal, _)| proposal.created_at_ms);
    Ok(open)
}

fn decide(conn: &Connection, id: &str, state: &str, at_ms: i64) -> StorageResult<()> {
    conn.execute(
        "UPDATE planning_task_proposals SET state = ?2, decided_at_ms = ?3 WHERE id = ?1",
        params![id, state, at_ms],
    )?;
    Ok(())
}

/// Applies the proposals named, oldest first, each filed under the agent
/// that proposed it. Returns each changed item once, as it ended.
fn accept(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    ids: &[String],
) -> StorageResult<Done<Vec<WorkItem>>> {
    store::mutate(
        history,
        change.operation_id,
        "work_proposals_accept",
        &ids,
        change.at_ms,
        |tx| {
            let mut effects = Effects::default();
            let mut value: Vec<WorkItem> = Vec::new();
            for (proposal, patch) in open_named(tx, ids)? {
                let item = edit(
                    tx,
                    meetings,
                    change,
                    &proposal.item_id,
                    &patch,
                    &mut effects,
                )?;
                credit(tx, &proposal.item_id, change.at_ms, &proposal.agent)?;
                decide(tx, &proposal.id, "accepted", change.at_ms)?;
                value.retain(|other| other.id != item.id);
                value.push(item);
            }
            Ok(Done { value, effects })
        },
    )
}

#[tauri::command]
#[specta::specta]
pub async fn work_proposals_accept(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
) -> Result<Vec<WorkItem>, String> {
    let change = Change::now(&operation_id, WorkActor::Agent);
    run_with_meetings(&app, |history, meetings| {
        accept(history, meetings, change, &ids)
    })
    .await
}

fn reject(
    history: &mut Connection,
    operation_id: &str,
    ids: &[String],
    at_ms: i64,
) -> StorageResult<Done<()>> {
    store::mutate(
        history,
        operation_id,
        "work_proposals_reject",
        &ids,
        at_ms,
        |tx| {
            let mut effects = Effects::default();
            for (proposal, _) in open_named(tx, ids)? {
                decide(tx, &proposal.id, "rejected", at_ms)?;
                if !effects.items.contains(&proposal.item_id) {
                    effects.items.push(proposal.item_id);
                }
            }
            Ok(Done { value: (), effects })
        },
    )
}

#[tauri::command]
#[specta::specta]
pub fn work_proposals_reject(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
) -> Result<(), String> {
    let at_ms = now_ms();
    run_history(&app, |history| reject(history, &operation_id, &ids, at_ms))
}

/// Fills the owner, due day and project Sona read off a meeting into the
/// items it just brought to triage, when Settings > Agents lets Sona's
/// suggestions apply. Each filled field is filed under Sona.
pub(super) fn fill_taken(
    history: &Connection,
    meetings: &Connection,
    user_name: Option<&str>,
    taken: Vec<WorkItem>,
    sona: WorkAutonomy,
    now_ms: i64,
) -> StorageResult<Vec<WorkItem>> {
    if sona == WorkAutonomy::Suggest {
        return Ok(taken);
    }
    let mut reader = MeetingReader::new(Some(meetings), user_name);
    taken
        .into_iter()
        .map(|item| {
            if item.status != WorkStatus::Triage {
                return Ok(item);
            }
            let mut patch = WorkPatch::default();
            for suggestion in sync::fill_suggestions(history, &mut reader, &item)? {
                match (suggestion.field, suggestion.value) {
                    (WorkSuggestionField::Owner, WorkSuggestionValue::Owner(owner)) => {
                        patch.owner = Some(owner);
                    }
                    (WorkSuggestionField::Due, WorkSuggestionValue::Due(due)) => {
                        patch.due_at_ms = Some(Some(due));
                    }
                    (WorkSuggestionField::Project, WorkSuggestionValue::Id(project)) => {
                        patch.project_id = Some(Some(project));
                    }
                    _ => {}
                }
            }
            if patch == WorkPatch::default() {
                return Ok(item);
            }
            Ok(items::apply(
                history,
                &item.id,
                &patch,
                WorkActivityKind::Status,
                WorkActor::Sona,
                now_ms,
            )?
            .after)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;

    const AT: i64 = 1_700_000_000_000;
    const CLAUDE: Agent<'static> = Agent {
        name: "Claude Code",
        autonomy: WorkAutonomy::Suggest,
    };

    fn history() -> Connection {
        // PANIC: a fixture that cannot open its database is a broken test.
        let history = Connection::open_in_memory().expect("an in-memory history database");
        // PANIC: as above, for the planner schema.
        schema::apply_all(&history).expect("the planner schema");
        history
    }

    fn operation() -> String {
        Uuid::new_v4().to_string()
    }

    fn by(actor: WorkActor, operation_id: &str, at_ms: i64) -> Change<'_> {
        Change {
            operation_id,
            actor,
            at_ms,
        }
    }

    fn todo(history: &mut Connection, title: &str) -> WorkItem {
        let input = WorkItemInput {
            title: title.to_owned(),
            details: None,
            status: Some(WorkStatus::NotStarted),
            priority: Some(2),
            project_id: None,
            milestone_id: None,
            owner: None,
            due_at_ms: None,
            label_ids: None,
            recurrence: None,
        };
        // PANIC: a fixture that cannot add its task is a broken test.
        super::super::create(history, None, by(WorkActor::Me, &operation(), AT), &input)
            .expect("a new task")
            .value
    }

    #[test]
    fn an_agents_change_waits_for_the_user_then_lands_in_the_agents_name() {
        let mut history = history();
        let task = todo(&mut history, "Send the pricing deck");
        let urgent = WorkPatch {
            priority: Some(1),
            ..WorkPatch::default()
        };
        let proposed = update(
            &mut history,
            None,
            by(WorkActor::Agent, &operation(), AT + 1_000),
            CLAUDE,
            ItemChange {
                id: &task.id,
                expected_revision: task.revision,
                patch: &urgent,
                link: None,
            },
        )
        .expect("the agent's change")
        .value
        .proposed;
        assert_eq!(items::item(&history, &task.id).expect("the task"), task);
        let [waiting] = proposed.as_slice() else {
            panic!("one proposal, got {proposed:?}");
        };
        assert_eq!(
            (waiting.field, waiting.agent.as_str()),
            (WorkProposalField::Priority, "Claude Code")
        );

        let landed = accept(
            &mut history,
            None,
            by(WorkActor::Agent, &operation(), AT + 2_000),
            std::slice::from_ref(&waiting.id),
        )
        .expect("accepting it")
        .value;
        assert_eq!(
            landed.iter().map(|item| item.priority).collect::<Vec<_>>(),
            [1]
        );
        assert!(item_proposals(&history, &task.id)
            .expect("open proposals")
            .is_empty());
        let newest = items::activity(&history, &task.id)
            .expect("the task's history")
            .pop()
            .expect("a history entry");
        assert_eq!(
            (newest.kind, newest.agent.as_deref()),
            (WorkActivityKind::Priority, Some("Claude Code"))
        );
    }

    #[test]
    fn a_rejected_change_never_lands() {
        let mut history = history();
        let task = todo(&mut history, "Book the offsite venue");
        let retitled = WorkPatch {
            title: Some("Book the bigger venue".to_owned()),
            ..WorkPatch::default()
        };
        let ids: Vec<String> = update(
            &mut history,
            None,
            by(WorkActor::Agent, &operation(), AT + 1_000),
            CLAUDE,
            ItemChange {
                id: &task.id,
                expected_revision: task.revision,
                patch: &retitled,
                link: None,
            },
        )
        .expect("the agent's change")
        .value
        .proposed
        .into_iter()
        .map(|proposal| proposal.id)
        .collect();
        assert_eq!(ids.len(), 1);

        reject(&mut history, &operation(), &ids, AT + 2_000).expect("rejecting it");
        let late = accept(
            &mut history,
            None,
            by(WorkActor::Agent, &operation(), AT + 3_000),
            &ids,
        )
        .map(|done| done.value);
        assert!(late.is_err(), "accepted after rejecting: {late:?}");
        assert_eq!(items::item(&history, &task.id).expect("the task"), task);
        assert!(item_proposals(&history, &task.id)
            .expect("open proposals")
            .is_empty());
    }

    #[test]
    fn a_task_handed_to_one_agent_is_that_agents_to_start() {
        let mut history = history();
        let task = todo(&mut history, "Draft the board update");
        set_doer(&history, &task.id, Some("Claude Code"), AT + 1_000)
            .expect("handing it to Claude Code");

        let taken = claim(
            &mut history,
            None,
            by(WorkActor::Agent, &operation(), AT + 2_000),
            "Codex",
            &task.id,
            None,
        )
        .map(|done| done.value);
        assert!(taken.is_err(), "another agent took it: {taken:?}");
        assert_eq!(items::item(&history, &task.id).expect("the task"), task);
        assert_eq!(
            doer(&history, &task.id)
                .expect("the task's agent")
                .map(|doer| doer.agent),
            Some("Claude Code".to_owned())
        );

        let started = claim(
            &mut history,
            None,
            by(WorkActor::Agent, &operation(), AT + 3_000),
            "Claude Code",
            &task.id,
            None,
        )
        .expect("its own claim")
        .value;
        assert_eq!(started.status, WorkStatus::InProgress);
    }

    #[test]
    fn an_agents_move_to_another_cycle_waits_for_the_user() {
        let mut history = history();
        let task = todo(&mut history, "Migrate the wiki");
        crate::planning::work::cycles::advance(&history, AT).expect("this cycle and the next");
        let next: i64 = history
            .query_row("SELECT max(number) FROM planning_cycles", [], |row| {
                row.get(0)
            })
            .expect("the next cycle");
        let moved = WorkPatch {
            cycle: Some(Some(next)),
            ..WorkPatch::default()
        };
        let proposed = update(
            &mut history,
            None,
            by(WorkActor::Agent, &operation(), AT + 1_000),
            CLAUDE,
            ItemChange {
                id: &task.id,
                expected_revision: task.revision,
                patch: &moved,
                link: None,
            },
        )
        .expect("the agent's change")
        .value
        .proposed;
        let [waiting] = proposed.as_slice() else {
            panic!("one proposal, got {proposed:?}");
        };
        assert_eq!(waiting.field, WorkProposalField::Cycle);

        let landed = accept(
            &mut history,
            None,
            by(WorkActor::Agent, &operation(), AT + 2_000),
            std::slice::from_ref(&waiting.id),
        )
        .expect("accepting it")
        .value;
        assert_eq!(
            landed.iter().map(|item| item.cycle).collect::<Vec<_>>(),
            [Some(next)]
        );
    }

    #[test]
    fn a_repeat_is_kept_in_the_parsers_words_and_stopping_it_keeps_the_due_day() {
        let mut history = history();
        let task = todo(&mut history, "Water the plants");
        let applies = Agent {
            name: "Claude Code",
            autonomy: WorkAutonomy::Apply,
        };
        let due = Some(AT + 86_400_000);
        let set = |words: Option<&str>| WorkPatch {
            recurrence: Some(words.map(str::to_owned)),
            due_at_ms: Some(due),
            ..WorkPatch::default()
        };
        let change = |history: &mut Connection, patch: &WorkPatch, at_ms: i64| {
            let revision = items::item(history, &task.id).expect("the task").revision;
            update(
                history,
                None,
                by(WorkActor::Agent, &operation(), at_ms),
                applies,
                ItemChange {
                    id: &task.id,
                    expected_revision: revision,
                    patch,
                    link: None,
                },
            )
            .map(|done| done.value.item)
        };
        let repeats = |history: &Connection| {
            let newest = items::activity(history, &task.id)
                .expect("the task's history")
                .pop()
                .expect("a history entry");
            (
                items::recurrence(history, &task.id).expect("its repeat"),
                newest.kind,
                newest.from,
                newest.to,
            )
        };

        change(&mut history, &set(Some("Every  Monday")), AT + 1_000).expect("a repeat");
        assert_eq!(
            repeats(&history),
            (
                Some("every monday".to_owned()),
                WorkActivityKind::Recurrence,
                None,
                Some("every monday".to_owned())
            )
        );

        assert!(change(&mut history, &set(Some("sometimes")), AT + 2_000).is_err());
        let stopped = change(
            &mut history,
            &WorkPatch {
                recurrence: Some(None),
                ..WorkPatch::default()
            },
            AT + 3_000,
        )
        .expect("stopping it");
        assert_eq!(stopped.due_at_ms, due);
        assert_eq!(
            repeats(&history),
            (
                None,
                WorkActivityKind::Recurrence,
                Some("every monday".to_owned()),
                None
            )
        );
    }

    #[test]
    fn a_link_given_when_work_is_done_is_kept_once_and_read_without_the_conversations() {
        let mut history = history();
        let task = todo(&mut history, "Ship the release notes");
        let url = "https://github.com/aktan/sona/pull/9";
        let done = WorkPatch {
            status: Some(WorkStatus::Done),
            ..WorkPatch::default()
        };
        for at_ms in [AT + 1_000, AT + 2_000] {
            let revision = items::item(&history, &task.id).expect("the task").revision;
            update(
                &mut history,
                None,
                by(WorkActor::Agent, &operation(), at_ms),
                CLAUDE,
                ItemChange {
                    id: &task.id,
                    expected_revision: revision,
                    patch: &done,
                    link: evidence_link(url).as_deref(),
                },
            )
            .expect("finishing it with a link");
        }

        let evidence = super::super::detail(&history, None, None, &task.id)
            .expect("the task's page")
            .evidence;
        let kept: Vec<_> = evidence
            .iter()
            .map(|link| {
                (
                    link.source_kind,
                    link.source_id.as_str(),
                    link.available,
                    link.title.is_some(),
                )
            })
            .collect();
        assert_eq!(
            kept,
            [(crate::planning::EvidenceSourceKind::Link, url, true, true)]
        );
        assert_eq!(evidence_link("shipped in the morning build"), None);
    }
}
