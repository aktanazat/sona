//! The tracker: every piece of work in one list, whether a meeting raised it
//! or someone typed it, with triage, history, labels, links and saved views.
//! Project screens read the same rows through `store`.

pub(crate) mod agents;
pub(crate) mod ask;
pub(crate) mod blocks;
pub(crate) mod capture;
pub(crate) mod command;
pub(crate) mod cycles;
pub(crate) mod follow;
mod hygiene;
pub(crate) mod items;
pub(crate) mod live;
pub(crate) mod parse;
pub(crate) mod recur;
pub(crate) mod sync;
#[cfg(test)]
mod tests;
pub mod types;
pub(crate) mod watch;

use self::follow::FollowUps;
use self::items::{Changed, DueSpan, NewItem};
use self::sync::{MeetingReader, People};
pub use self::types::WorkChanged;
use self::types::{
    WorkActivityKind, WorkActor, WorkCounts, WorkDay, WorkDisplay, WorkDoer, WorkItem,
    WorkItemDetail, WorkItemInput, WorkItemRow, WorkLabel, WorkList, WorkOwner, WorkPatch,
    WorkProjectChoice, WorkQuery, WorkRelation, WorkRelationKind, WorkRepeat, WorkSource,
    WorkStatus, WorkTimeBlock, WorkView,
};
use super::sources;
use super::store::{self, Refusal};
use super::types::{PlanningTask, TaskStatus};
use crate::managers::history::HistoryManager;
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use anyhow::Result as StorageResult;
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What one change touched, stored with its response so a replay tells the
/// windows again.
#[derive(Default, Deserialize, Serialize)]
pub(super) struct Effects {
    pub(super) items: Vec<String>,
    pub(super) projects: Vec<String>,
    pub(super) loops: Vec<MeetingLoopId>,
}

impl Effects {
    pub(super) fn item(&mut self, item: &WorkItem) {
        if !self.items.contains(&item.id) {
            self.items.push(item.id.clone());
        }
        if let Some(project_id) = &item.project_id {
            if !self.projects.contains(project_id) {
                self.projects.push(project_id.clone());
            }
        }
    }

    pub(super) fn changed(&mut self, changed: &Changed) {
        self.item(&changed.before);
        self.item(&changed.after);
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct Done<T> {
    pub(super) value: T,
    pub(super) effects: Effects,
}

/// Who made a change, under which operation id, and when.
#[derive(Clone, Copy)]
struct Change<'a> {
    operation_id: &'a str,
    actor: WorkActor,
    at_ms: i64,
}

impl<'a> Change<'a> {
    fn now(operation_id: &'a str, actor: WorkActor) -> Self {
        Self {
            operation_id,
            actor,
            at_ms: now_ms(),
        }
    }
}

fn tell(app: &AppHandle, effects: &Effects) {
    let event = WorkChanged {
        ids: effects.items.clone(),
    };
    if event.emit(app).is_err() {
        log::warn!("Work change event could not be delivered");
    }
    for project_id in &effects.projects {
        super::changed(app, project_id.clone());
    }
}

/// Tells every window what a committed change touched: the tracker, the
/// project pages, and the meetings whose loops moved with it.
pub(super) async fn announce(app: &AppHandle, meetings: &MeetingSessionManager, effects: &Effects) {
    tell(app, effects);
    if !effects.loops.is_empty() {
        meetings.loops_moved(&effects.loops).await;
    }
}

/// Tells the tracker that project-page work changed these tasks.
pub(super) fn tasks_changed(app: &AppHandle, ids: Vec<String>) {
    let event = WorkChanged { ids };
    if event.emit(app).is_err() {
        log::warn!("Work change event could not be delivered");
    }
}

/// Runs `work` inside the meeting database's transaction when the
/// conversations open, so a refusal or failure on either side leaves both as
/// they were, and with no meeting database when they cannot: then only the
/// tracker side changes, and [`catch_up`] moves the commitments once they
/// open. The history side commits first; a crash between the two commits is
/// put right by the same catch-up.
pub(super) async fn with_meetings<T: Send>(
    history: &HistoryManager,
    meetings: &MeetingSessionManager,
    work: impl FnOnce(&mut Connection, Option<&Connection>) -> StorageResult<T> + Send,
) -> Result<T, String> {
    let Ok(store) = meetings.store().await else {
        return super::history(history, |conn| work(conn, None));
    };
    let mut failed = None;
    let result = store.with_planning_snapshot(|meeting_conn| {
        history
            .with_connection(|conn| work(conn, Some(meeting_conn)))
            .map_err(|error| {
                failed = Some(error);
                StoreError::Unavailable
            })
    });
    result.map_err(|_| {
        failed.take().map_or_else(
            || "Conversations could not be read.".to_string(),
            super::failure,
        )
    })
}

/// [`with_meetings`] for work that cannot be done without them.
pub(super) async fn with_both<T: Send>(
    history: &HistoryManager,
    meetings: &MeetingSessionManager,
    work: impl FnOnce(&mut Connection, &Connection) -> StorageResult<T> + Send,
) -> Result<T, String> {
    with_meetings(history, meetings, |history, meetings| {
        work(history, meetings.ok_or(super::ConversationsClosed)?)
    })
    .await
}

/// [`with_both`] for callers already holding the meeting store, with the
/// history side in one transaction of its own.
fn in_both<T>(
    store: &MeetingStore,
    history: &HistoryManager,
    work: impl FnOnce(&Connection, &Connection) -> StorageResult<T>,
) -> StorageResult<T> {
    let mut failed = None;
    let result = store.with_planning_snapshot(|meeting_conn| {
        history
            .with_connection(|conn| {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let value = work(&tx, meeting_conn)?;
                tx.commit()?;
                Ok(value)
            })
            .map_err(|error| {
                failed = Some(error);
                StoreError::Unavailable
            })
    });
    result.map_err(|error| {
        failed
            .take()
            .unwrap_or_else(|| anyhow::anyhow!("conversations could not be read: {error:?}"))
    })
}

/// Runs a tracker change through [`with_meetings`], then tells every window.
async fn run_with_meetings<T: Send>(
    app: &AppHandle,
    work: impl FnOnce(&mut Connection, Option<&Connection>) -> StorageResult<Done<T>> + Send,
) -> Result<T, String> {
    let history = app.state::<Arc<HistoryManager>>();
    let meetings = app.state::<Arc<MeetingSessionManager>>();
    let done = with_meetings(&history, &meetings, work).await?;
    announce(app, &meetings, &done.effects).await;
    Ok(done.value)
}

/// Runs a change that touches only the tracker, then tells every window.
fn run_history<T>(
    app: &AppHandle,
    work: impl FnOnce(&mut Connection) -> StorageResult<Done<T>>,
) -> Result<T, String> {
    let done = super::history(&app.state::<Arc<HistoryManager>>(), work)?;
    tell(app, &done.effects);
    Ok(done.value)
}

/// Brings a just-processed meeting's open commitments into triage.
pub(crate) fn take_in(app: Option<&AppHandle>, store: &MeetingStore, session_id: MeetingSessionId) {
    let Some(app) = app else { return };
    let Some(history) = app.try_state::<Arc<HistoryManager>>() else {
        return;
    };
    let user_name = store.user_name();
    let sona = crate::settings::get_settings(app).work_sona_changes;
    let now = now_ms();
    let taken = in_both(store, &history, |history, meetings| {
        let taken =
            sync::take_in_meeting(history, meetings, session_id, user_name.as_deref(), now)?;
        agents::fill_taken(history, meetings, user_name.as_deref(), taken, sona, now)
    });
    match taken {
        Ok(taken) if !taken.is_empty() => {
            let mut effects = Effects::default();
            for item in &taken {
                effects.item(item);
            }
            tell(app, &effects);
        }
        Ok(_) => {}
        Err(error) => log::warn!("Meeting commitments could not reach triage: {error:#}"),
    }
}

/// Moves the tracker after a loop moved in its meeting, and returns the
/// other loops that moved with it. The meeting's own change already stands,
/// so a failure here is logged and left to the next start's reconcile.
pub(crate) fn follow_loop(
    app: &AppHandle,
    store: &MeetingStore,
    loop_id: &MeetingLoopId,
) -> Vec<MeetingLoopId> {
    let Some(history) = app.try_state::<Arc<HistoryManager>>() else {
        return Vec::new();
    };
    let now = now_ms();
    match in_both(store, &history, |history, meetings| {
        sync::follow_loop_in(history, meetings, loop_id, now)
    }) {
        Ok(followed) => {
            if !followed.items.is_empty() {
                let mut effects = Effects::default();
                for item in &followed.items {
                    effects.item(item);
                }
                tell(app, &effects);
            }
            followed.loops
        }
        Err(error) => {
            log::warn!("The tracker could not follow a meeting loop: {error:#}");
            Vec::new()
        }
    }
}

/// Catches the tracker up with the last month of meetings and puts items
/// and their commitments back in step, moves made while the conversations
/// were closed included, then tells every window, which reads the meetings
/// again. The meeting store starts it once per launch, as it opens: at
/// launch, or when the conversations unlock later.
pub(crate) fn catch_up(app: AppHandle, store: Arc<MeetingStore>) {
    tauri::async_runtime::spawn(async move {
        let Some(history) = app.try_state::<Arc<HistoryManager>>() else {
            log::warn!("Tracker catch-up skipped: history is not ready");
            return;
        };
        let user_name = store.user_name();
        let sona = crate::settings::get_settings(&app).work_sona_changes;
        let now = now_ms();
        let sessions = match in_both(&store, &history, |_, meetings| {
            sync::meetings_since(meetings, now - sync::BACKFILL_MS)
        }) {
            Ok(sessions) => sessions,
            Err(error) => {
                log::warn!("Tracker catch-up could not list meetings: {error:#}");
                return;
            }
        };
        let mut effects = Effects::default();
        for session_id in sessions {
            let taken = in_both(&store, &history, |history, meetings| {
                let taken = sync::take_in_meeting(
                    history,
                    meetings,
                    session_id,
                    user_name.as_deref(),
                    now,
                )?;
                agents::fill_taken(history, meetings, user_name.as_deref(), taken, sona, now)
            });
            match taken {
                Ok(taken) => taken.iter().for_each(|item| effects.item(item)),
                Err(error) => log::warn!("Tracker catch-up skipped a meeting: {error:#}"),
            }
        }
        match in_both(&store, &history, |history, meetings| {
            hygiene::fold_repeats(history, meetings, user_name.as_deref(), now)
        }) {
            Ok(folded) => folded.iter().for_each(|item| effects.item(item)),
            Err(error) => log::warn!("Tracker repeats could not be folded: {error:#}"),
        }
        match in_both(&store, &history, |history, meetings| {
            sync::reconcile(history, meetings, now)
        }) {
            Ok(followed) => {
                followed.items.iter().for_each(|item| effects.item(item));
                effects.loops = followed.loops;
            }
            Err(error) => {
                log::warn!("Tracker items could not be matched to their loops: {error:#}");
            }
        }
        match app.try_state::<Arc<MeetingSessionManager>>() {
            Some(meetings) => announce(&app, &meetings, &effects).await,
            None => tell(&app, &effects),
        }
    });
}

fn project_choices(history: &Connection) -> StorageResult<Vec<WorkProjectChoice>> {
    let mut statement = history.prepare(
        "SELECT id, title, status FROM planning_projects
         ORDER BY status = 'completed', lower(title), id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(WorkProjectChoice {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// What a row shows beside its item, read once per request.
struct RowContext {
    people: People,
    project_titles: HashMap<String, String>,
    blockers: HashMap<String, u32>,
    raised: HashMap<String, u32>,
    sources: HashMap<String, WorkSource>,
    follow_ups: FollowUps,
    doers: HashMap<String, WorkDoer>,
    heard: HashMap<String, live::Heard>,
    risks: std::collections::BTreeSet<String>,
    blocks: HashMap<String, WorkTimeBlock>,
    repeats: HashMap<String, WorkRepeat>,
}

impl RowContext {
    fn read(
        history: &Connection,
        meetings: Option<&Connection>,
        projects: &[WorkProjectChoice],
    ) -> StorageResult<Self> {
        Ok(Self {
            people: meetings.map(People::read).transpose()?.unwrap_or_default(),
            project_titles: projects
                .iter()
                .map(|project| (project.id.clone(), project.title.clone()))
                .collect(),
            blockers: items::open_blockers(history)?,
            raised: items::meetings_raised(history)?,
            sources: follow::item_sources(history)?,
            follow_ups: FollowUps::read(history)?,
            doers: agents::doers(history)?,
            heard: live::heard(history, meetings)?,
            risks: blocks::risks(history)?,
            blocks: blocks::booked(history)?,
            repeats: recur::repeats(history, now_ms())?,
        })
    }

    fn row(&self, reader: &mut MeetingReader<'_>, item: WorkItem) -> StorageResult<WorkItemRow> {
        let source_loop_id = item.source_loop_id.clone();
        let mut row = WorkItemRow {
            project_title: item
                .project_id
                .as_ref()
                .and_then(|id| self.project_titles.get(id))
                .cloned(),
            owner_name: item
                .owner_person_id
                .as_deref()
                .and_then(|id| self.people.name(id))
                .map(str::to_owned),
            doer: self.doers.get(&item.id).cloned(),
            source_meeting_id: None,
            source_meeting_title: None,
            source_quote: None,
            source_speaker: None,
            source_at_ms: None,
            blocked_by_open_count: self.blockers.get(&item.id).copied().unwrap_or(0),
            raised_in: self
                .raised
                .get(item.duplicate_of_id.as_ref().unwrap_or(&item.id))
                .copied()
                .unwrap_or(0),
            source: self
                .sources
                .get(&item.id)
                .copied()
                .unwrap_or(WorkSource::Person),
            follow_up_at_ms: self.follow_ups.at(&item)?,
            at_risk: self.risks.contains(&item.id),
            time_block: self.blocks.get(&item.id).copied(),
            repeat: self.repeats.get(&item.id).cloned(),
            item,
        };
        if let Some(source) = match &source_loop_id {
            Some(loop_id) => reader.source(loop_id)?,
            None => None,
        } {
            row.source_meeting_id = Some(source.meeting.session_id.uuid().to_string());
            row.source_meeting_title = Some(source.meeting.title.clone());
            if source.retained {
                row.source_quote.clone_from(&source.row.quote);
                row.source_speaker.clone_from(&source.row.speaker);
                row.source_at_ms = i64::try_from(source.row.at_ms).ok();
            }
        } else if let Some(heard) = self.heard.get(&row.item.id) {
            row.source_meeting_id = Some(heard.meeting_id.clone());
            row.source_meeting_title.clone_from(&heard.meeting_title);
            row.source_quote.clone_from(&heard.quote);
            row.source_at_ms = heard.at_ms;
        }
        Ok(row)
    }
}

/// The items a meeting raised or is cited by.
fn from_meeting(history: &Connection, meeting_id: &str) -> StorageResult<HashSet<String>> {
    let meeting_id = store::uuid(meeting_id)?;
    let mut statement = history.prepare(
        "SELECT id FROM planning_tasks WHERE source_loop_id LIKE ?1 || ':%'
         UNION
         SELECT target_id FROM planning_evidence_links
         WHERE target_kind = 'task' AND ((source_kind = 'meeting' AND source_id = ?1)
            OR (source_kind = 'loop' AND source_id LIKE ?1 || ':%'))",
    )?;
    let ids = statement.query_map([meeting_id], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

fn any_of<T: PartialEq>(wanted: Option<&Vec<T>>, value: &T) -> bool {
    wanted.is_none_or(|wanted| wanted.contains(value))
}

fn wanted(
    item: &WorkItem,
    query: &WorkQuery,
    owners: Option<&HashSet<&str>>,
    due: Option<&[DueSpan]>,
    now_ms: i64,
) -> bool {
    any_of(query.statuses.as_ref(), &item.status)
        && query
            .direction
            .is_none_or(|direction| direction == item.direction)
        && owners.is_none_or(|owners| {
            item.owner_person_id
                .as_deref()
                .is_some_and(|id| owners.contains(id))
        })
        && query
            .project_ids
            .as_ref()
            .is_none_or(|ids| match &item.project_id {
                Some(id) => ids.contains(id),
                None => ids.iter().any(|id| id == "none"),
            })
        && query
            .label_ids
            .as_ref()
            .is_none_or(|ids| item.label_ids.iter().any(|id| ids.contains(id)))
        && any_of(query.priorities.as_ref(), &item.priority)
        && query
            .due_before_ms
            .is_none_or(|before| item.due_at_ms.is_some_and(|due| due < before))
        && due.is_none_or(|spans| spans.iter().any(|span| span.holds(item)))
        && (query.include_snoozed || !items::snoozed(item, now_ms))
}

fn found(item: &WorkItem, search: &str) -> bool {
    let number = search.strip_prefix('#').unwrap_or(search);
    number
        .parse::<i64>()
        .is_ok_and(|number| number == item.number)
        || item.title.to_lowercase().contains(search)
        || item
            .details
            .as_deref()
            .is_some_and(|details| details.to_lowercase().contains(search))
}

fn list(
    history: &Connection,
    meetings: Option<&Connection>,
    user_name: Option<&str>,
    query: &WorkQuery,
    now_ms: i64,
) -> StorageResult<WorkList> {
    let all = items::items(history, query.include_archived)?;
    let projects = project_choices(history)?;
    let context = RowContext::read(history, meetings, &projects)?;
    let raised = query
        .source_meeting_id
        .as_deref()
        .map(|id| from_meeting(history, id))
        .transpose()?;
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|search| !search.is_empty())
        .map(str::to_lowercase);
    let owners = query
        .owner_person_ids
        .as_deref()
        .map(|ids| context.people.with_merged(ids));
    let due = query
        .due
        .as_deref()
        .map(|due| items::due_spans(due, now_ms))
        .transpose()?;
    let cycle = query
        .cycle
        .map(|cycle| cycles::want(history, cycle, now_ms))
        .transpose()?;
    let mut reader = MeetingReader::new(meetings, user_name);
    let mut rows = Vec::new();
    let mut suggestions = BTreeMap::new();
    for item in &all {
        if !wanted(item, query, owners.as_ref(), due.as_deref(), now_ms)
            || raised.as_ref().is_some_and(|ids| !ids.contains(&item.id))
            || cycle.is_some_and(|cycle| !cycle.holds(item))
            || search.as_deref().is_some_and(|search| !found(item, search))
        {
            continue;
        }
        let suggested = sync::suggestions(history, &mut reader, &all, item)?;
        if !suggested.is_empty() {
            suggestions.insert(item.id.clone(), suggested);
        }
        rows.push(context.row(&mut reader, item.clone())?);
    }
    let listed: HashSet<&str> = rows.iter().map(|row| row.item.id.as_str()).collect();
    let mut proposals = agents::open_proposals(history)?;
    proposals.retain(|id, _| listed.contains(id.as_str()));
    Ok(WorkList {
        items: rows,
        labels: items::labels(history)?,
        projects,
        people: context.people.choices,
        suggestions,
        proposals,
        conversations_available: meetings.is_some(),
        cycles: cycles::all(history, now_ms)?,
    })
}

fn detail(
    history: &Connection,
    meetings: Option<&Connection>,
    user_name: Option<&str>,
    id: &str,
) -> StorageResult<WorkItemDetail> {
    let id = store::uuid(id)?;
    let item = items::item(history, &id)?;
    let all = items::items(history, false)?;
    let projects = project_choices(history)?;
    let context = RowContext::read(history, meetings, &projects)?;
    let mut reader = MeetingReader::new(meetings, user_name);
    let suggestions = sync::suggestions(history, &mut reader, &all, &item)?;
    let mut evidence = store::task_evidence(history, &id)?;
    for link in &mut evidence {
        match meetings {
            Some(meetings) => sources::resolve_link(history, meetings, link)?,
            None if !sources::in_conversations(link.source_kind) => {
                sources::resolve_local(history, link)?;
            }
            None => {}
        }
    }
    Ok(WorkItemDetail {
        item: context.row(&mut reader, item)?,
        recurrence: items::recurrence(history, &id)?,
        evidence,
        relations: items::relations(history, &id)?,
        activity: items::activity(history, &id)?,
        suggestions,
        conversations_available: meetings.is_some(),
        proposals: agents::item_proposals(history, &id)?,
    })
}

/// The ids a change names, each once, in the order given.
fn unique(ids: &[String]) -> StorageResult<Vec<String>> {
    let mut seen = Vec::with_capacity(ids.len());
    for id in ids {
        let id = store::uuid(id)?;
        if !seen.contains(&id) {
            seen.push(id);
        }
    }
    Ok(seen)
}

/// The owner a change names, a person checked against the conversations,
/// which must be open to name one.
fn checked_owner(meetings: Option<&Connection>, owner: &WorkOwner) -> StorageResult<WorkOwner> {
    match owner {
        WorkOwner::Person { person_id } => {
            let meetings = meetings.ok_or(super::ConversationsClosed)?;
            Ok(WorkOwner::Person {
                person_id: sources::require_person(meetings, &store::uuid(person_id)?)?,
            })
        }
        WorkOwner::Myself | WorkOwner::Nobody => Ok(owner.clone()),
    }
}

/// Applies one edit to one item, then moves the commitments its family
/// tracks to match, or leaves them to [`catch_up`] while the conversations
/// are closed.
fn edit(
    history: &Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    id: &str,
    patch: &WorkPatch,
    effects: &mut Effects,
) -> StorageResult<WorkItem> {
    let patch = WorkPatch {
        owner: patch
            .owner
            .as_ref()
            .map(|owner| checked_owner(meetings, owner))
            .transpose()?,
        ..patch.clone()
    };
    let changed = items::apply(
        history,
        id,
        &patch,
        WorkActivityKind::Status,
        change.actor,
        change.at_ms,
    )?;
    effects.changed(&changed);
    if changed.after.status.is_closed() {
        if let Some(next) = recur::next_of(history, id)? {
            effects.item(&items::item(history, &next)?);
        }
    }
    if changed.before.status != changed.after.status {
        effects.loops.extend(sync::settle(
            history,
            meetings,
            id,
            change.actor,
            change.at_ms,
        )?);
    }
    Ok(changed.after)
}

#[tauri::command]
#[specta::specta]
pub async fn work_list(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    query: WorkQuery,
) -> Result<WorkList, String> {
    let user_name = meetings.user_name();
    let now = now_ms();
    with_meetings(&history_state, &meetings, |history, meeting_conn| {
        list(history, meeting_conn, user_name.as_deref(), &query, now)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn work_item(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    id: String,
) -> Result<WorkItemDetail, String> {
    let user_name = meetings.user_name();
    with_meetings(&history_state, &meetings, |history, meeting_conn| {
        detail(history, meeting_conn, user_name.as_deref(), &id)
    })
    .await
}

/// Puts `input` in as a new item, its owner checked first.
fn insert_input(
    history: &Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    input: &WorkItemInput,
) -> StorageResult<WorkItem> {
    let owner = checked_owner(meetings, input.owner.as_ref().unwrap_or(&WorkOwner::Nobody))?;
    items::insert(
        history,
        &NewItem {
            title: &input.title,
            details: input.details.as_deref(),
            status: input.status.unwrap_or(WorkStatus::NotStarted),
            priority: input.priority.unwrap_or(0),
            project_id: input.project_id.as_deref(),
            milestone_id: input.milestone_id.as_deref(),
            owner: &owner,
            due_at_ms: input.due_at_ms,
            label_ids: input.label_ids.as_deref().unwrap_or_default(),
            source_loop_id: None,
            recurrence: input.recurrence.as_deref(),
        },
        change.actor,
        change.at_ms,
    )
}

fn create(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    input: &WorkItemInput,
) -> StorageResult<Done<WorkItem>> {
    store::mutate(
        history,
        change.operation_id,
        "work_create",
        input,
        change.at_ms,
        |tx| {
            let item = insert_input(tx, meetings, change, input)?;
            let mut effects = Effects::default();
            effects.item(&item);
            Ok(Done {
                value: item,
                effects,
            })
        },
    )
}

/// [`work_create`] made by `actor`. Only an item given a person as its
/// owner needs the conversations, where people live.
pub(crate) async fn create_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    input: &WorkItemInput,
) -> Result<WorkItem, String> {
    let change = Change::now(operation_id, actor);
    if matches!(input.owner, Some(WorkOwner::Person { .. })) {
        run_with_meetings(app, |history, meetings| {
            create(history, meetings, change, input)
        })
        .await
    } else {
        run_history(app, |history| create(history, None, change, input))
    }
}

#[tauri::command]
#[specta::specta]
pub async fn work_create(
    app: AppHandle,
    operation_id: String,
    input: WorkItemInput,
) -> Result<WorkItem, String> {
    create_as(&app, WorkActor::Me, &operation_id, &input).await
}

fn update(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    id: &str,
    expected_revision: i64,
    patch: &WorkPatch,
) -> StorageResult<Done<WorkItem>> {
    store::mutate(
        history,
        change.operation_id,
        "work_update",
        &(id, expected_revision, patch),
        change.at_ms,
        |tx| {
            let id = store::uuid(id)?;
            if items::item(tx, &id)?.revision != expected_revision {
                return Err(Refusal::StaleRevision.into());
            }
            let mut effects = Effects::default();
            let value = edit(tx, meetings, change, &id, patch, &mut effects)?;
            Ok(Done { value, effects })
        },
    )
}

/// [`work_update`] made by `actor`.
pub(crate) async fn update_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    id: &str,
    expected_revision: i64,
    patch: &WorkPatch,
) -> Result<WorkItem, String> {
    let change = Change::now(operation_id, actor);
    run_with_meetings(app, |history, meetings| {
        update(history, meetings, change, id, expected_revision, patch)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn work_update(
    app: AppHandle,
    operation_id: String,
    id: String,
    expected_revision: i64,
    patch: WorkPatch,
) -> Result<WorkItem, String> {
    update_as(
        &app,
        WorkActor::Me,
        &operation_id,
        &id,
        expected_revision,
        &patch,
    )
    .await
}

/// One patch to many items: every item takes it, or none does.
fn bulk_update(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    ids: &[String],
    patch: &WorkPatch,
) -> StorageResult<Done<Vec<WorkItem>>> {
    store::mutate(
        history,
        change.operation_id,
        "work_bulk_update",
        &(ids, patch),
        change.at_ms,
        |tx| {
            let mut effects = Effects::default();
            let mut value = Vec::new();
            for id in unique(ids)? {
                value.push(edit(tx, meetings, change, &id, patch, &mut effects)?);
            }
            Ok(Done { value, effects })
        },
    )
}

/// [`work_bulk_update`] made by `actor`.
pub(crate) async fn bulk_update_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    ids: &[String],
    patch: &WorkPatch,
) -> Result<Vec<WorkItem>, String> {
    let change = Change::now(operation_id, actor);
    run_with_meetings(app, |history, meetings| {
        bulk_update(history, meetings, change, ids, patch)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn work_bulk_update(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
    patch: WorkPatch,
) -> Result<Vec<WorkItem>, String> {
    bulk_update_as(&app, WorkActor::Me, &operation_id, &ids, &patch).await
}

fn set_archived(
    history: &mut Connection,
    change: Change<'_>,
    ids: &[String],
    archived: bool,
) -> StorageResult<Done<Vec<WorkItem>>> {
    let command = if archived {
        "work_archive"
    } else {
        "work_restore"
    };
    store::mutate(
        history,
        change.operation_id,
        command,
        &ids,
        change.at_ms,
        |tx| {
            let mut effects = Effects::default();
            let mut value = Vec::new();
            for id in unique(ids)? {
                let changed = items::set_archived(tx, &id, archived, change.actor, change.at_ms)?;
                effects.changed(&changed);
                value.push(changed.after);
            }
            Ok(Done { value, effects })
        },
    )
}

/// [`work_archive`], or [`work_restore`] when `archived` is false, made by
/// `actor`.
pub(crate) fn set_archived_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    ids: &[String],
    archived: bool,
) -> Result<Vec<WorkItem>, String> {
    let change = Change::now(operation_id, actor);
    run_history(app, |history| set_archived(history, change, ids, archived))
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_archive(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
) -> Result<Vec<WorkItem>, String> {
    set_archived_as(&app, WorkActor::Me, &operation_id, &ids, true)
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_restore(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
) -> Result<Vec<WorkItem>, String> {
    set_archived_as(&app, WorkActor::Me, &operation_id, &ids, false)
}

fn accept(
    history: &mut Connection,
    meetings: Option<&Connection>,
    user_name: Option<&str>,
    change: Change<'_>,
    ids: &[String],
    apply_suggestions: bool,
) -> StorageResult<Done<Vec<WorkItem>>> {
    store::mutate(
        history,
        change.operation_id,
        "work_triage_accept",
        &(ids, apply_suggestions),
        change.at_ms,
        |tx| {
            let mut reader = MeetingReader::new(meetings, user_name);
            let mut effects = Effects::default();
            let mut value = Vec::new();
            for id in unique(ids)? {
                let item = items::item(tx, &id)?;
                let patch = sync::acceptance(tx, &mut reader, &item, apply_suggestions, None)?;
                let (after, moved) =
                    sync::accept(tx, meetings, &item, &patch, change.actor, change.at_ms)?;
                effects.item(&item);
                effects.item(&after);
                effects.loops.extend(moved);
                value.push(after);
            }
            Ok(Done { value, effects })
        },
    )
}

/// [`work_triage_accept`] made by `actor`.
pub(crate) async fn triage_accept_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    ids: &[String],
    apply_suggestions: bool,
) -> Result<Vec<WorkItem>, String> {
    let change = Change::now(operation_id, actor);
    let user_name = app.state::<Arc<MeetingSessionManager>>().user_name();
    run_with_meetings(app, |history, meetings| {
        accept(
            history,
            meetings,
            user_name.as_deref(),
            change,
            ids,
            apply_suggestions,
        )
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn work_triage_accept(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
    apply_suggestions: bool,
) -> Result<Vec<WorkItem>, String> {
    triage_accept_as(&app, WorkActor::Me, &operation_id, &ids, apply_suggestions).await
}

fn decline(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    ids: &[String],
) -> StorageResult<Done<Vec<WorkItem>>> {
    store::mutate(
        history,
        change.operation_id,
        "work_triage_decline",
        &ids,
        change.at_ms,
        |tx| {
            let mut effects = Effects::default();
            let mut value = Vec::new();
            for id in unique(ids)? {
                let item = items::item(tx, &id)?;
                let (after, moved) =
                    sync::decline(tx, meetings, &item, change.actor, change.at_ms)?;
                effects.item(&after);
                effects.loops.extend(moved);
                value.push(after);
            }
            Ok(Done { value, effects })
        },
    )
}

/// [`work_triage_decline`] made by `actor`.
pub(crate) async fn triage_decline_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    ids: &[String],
) -> Result<Vec<WorkItem>, String> {
    let change = Change::now(operation_id, actor);
    run_with_meetings(app, |history, meetings| {
        decline(history, meetings, change, ids)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn work_triage_decline(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
) -> Result<Vec<WorkItem>, String> {
    triage_decline_as(&app, WorkActor::Me, &operation_id, &ids).await
}

/// Folds `id` into `of_id`: its sources move to the item it repeats.
fn mark_duplicate(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    id: &str,
    of_id: &str,
) -> StorageResult<Done<WorkItem>> {
    store::mutate(
        history,
        change.operation_id,
        "work_mark_duplicate",
        &(id, of_id),
        change.at_ms,
        |tx| {
            let id = store::uuid(id)?;
            let (root, changed) =
                items::mark_duplicate(tx, &id, &store::uuid(of_id)?, change.actor, change.at_ms)?;
            let mut effects = Effects::default();
            for changed in &changed {
                effects.changed(changed);
            }
            effects.item(&items::item(tx, &root)?);
            effects.loops.extend(sync::settle(
                tx,
                meetings,
                &root,
                change.actor,
                change.at_ms,
            )?);
            Ok(Done {
                value: items::item(tx, &id)?,
                effects,
            })
        },
    )
}

/// [`work_mark_duplicate`] made by `actor`.
pub(crate) async fn mark_duplicate_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    id: &str,
    of_id: &str,
) -> Result<WorkItem, String> {
    let change = Change::now(operation_id, actor);
    run_with_meetings(app, |history, meetings| {
        mark_duplicate(history, meetings, change, id, of_id)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn work_mark_duplicate(
    app: AppHandle,
    operation_id: String,
    id: String,
    of_id: String,
) -> Result<WorkItem, String> {
    mark_duplicate_as(&app, WorkActor::Me, &operation_id, &id, &of_id).await
}

/// [`work_relation_add`] made by `actor`.
pub(crate) fn relation_add_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    from_id: &str,
    to_id: &str,
    kind: WorkRelationKind,
) -> Result<WorkRelation, String> {
    let change = Change::now(operation_id, actor);
    run_history(app, |history| {
        store::mutate(
            history,
            change.operation_id,
            "work_relation_add",
            &(from_id, to_id, kind),
            change.at_ms,
            |tx| {
                let relation =
                    items::add_relation(tx, from_id, to_id, kind, change.actor, change.at_ms)?;
                Ok(Done {
                    effects: Effects {
                        items: vec![relation.from_id.clone(), relation.to_id.clone()],
                        ..Effects::default()
                    },
                    value: relation,
                })
            },
        )
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_relation_add(
    app: AppHandle,
    operation_id: String,
    from_id: String,
    to_id: String,
    kind: WorkRelationKind,
) -> Result<WorkRelation, String> {
    relation_add_as(&app, WorkActor::Me, &operation_id, &from_id, &to_id, kind)
}

/// [`work_relation_remove`] made by `actor`.
pub(crate) fn relation_remove_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    id: &str,
) -> Result<(), String> {
    let change = Change::now(operation_id, actor);
    run_history(app, |history| {
        store::mutate(
            history,
            change.operation_id,
            "work_relation_remove",
            &id,
            change.at_ms,
            |tx| {
                let relation = items::remove_relation(tx, id, change.actor, change.at_ms)?;
                Ok(Done {
                    value: (),
                    effects: Effects {
                        items: vec![relation.from_id, relation.to_id],
                        ..Effects::default()
                    },
                })
            },
        )
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_relation_remove(
    app: AppHandle,
    operation_id: String,
    id: String,
) -> Result<(), String> {
    relation_remove_as(&app, WorkActor::Me, &operation_id, &id)
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_labels(
    history_state: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<WorkLabel>, String> {
    super::history(&history_state, |history| items::labels(history))
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_label_save(
    app: AppHandle,
    operation_id: String,
    id: Option<String>,
    name: String,
    hue: f64,
) -> Result<WorkLabel, String> {
    let now = now_ms();
    run_history(&app, |history| {
        store::mutate(
            history,
            &operation_id,
            "work_label_save",
            &(&id, &name, hue),
            now,
            |tx| {
                Ok(Done {
                    value: items::save_label(tx, id.as_deref(), &name, hue, now)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_label_delete(app: AppHandle, operation_id: String, id: String) -> Result<(), String> {
    let now = now_ms();
    run_history(&app, |history| {
        store::mutate(
            history,
            &operation_id,
            "work_label_delete",
            &id,
            now,
            |tx| {
                Ok(Done {
                    effects: Effects {
                        items: items::delete_label(tx, &id)?,
                        ..Effects::default()
                    },
                    value: (),
                })
            },
        )
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_views(history_state: State<'_, Arc<HistoryManager>>) -> Result<Vec<WorkView>, String> {
    super::history(&history_state, |history| items::views(history))
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_view_save(
    app: AppHandle,
    operation_id: String,
    id: Option<String>,
    name: String,
    query: WorkQuery,
    display: WorkDisplay,
) -> Result<WorkView, String> {
    let now = now_ms();
    run_history(&app, |history| {
        store::mutate(
            history,
            &operation_id,
            "work_view_save",
            &(&id, &name, &query, &display),
            now,
            |tx| {
                Ok(Done {
                    value: items::save_view(tx, id.as_deref(), &name, &query, &display, now)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_view_delete(app: AppHandle, operation_id: String, id: String) -> Result<(), String> {
    let now = now_ms();
    run_history(&app, |history| {
        store::mutate(history, &operation_id, "work_view_delete", &id, now, |tx| {
            items::delete_view(tx, &id)?;
            Ok(Done {
                value: (),
                effects: Effects::default(),
            })
        })
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_counts(history_state: State<'_, Arc<HistoryManager>>) -> Result<WorkCounts, String> {
    super::history(&history_state, |history| {
        let items = items::items(history, false)?;
        let follow_ups = FollowUps::read(history)?;
        let moments = items
            .iter()
            .map(|item| follow_ups.at(item))
            .collect::<StorageResult<Vec<_>>>()?;
        items::counts(items.iter().zip(moments), now_ms())
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_keep(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
) -> Result<Vec<WorkItem>, String> {
    let now = now_ms();
    run_history(&app, |history| {
        store::mutate(history, &operation_id, "work_keep", &ids, now, |tx| {
            let mut effects = Effects::default();
            let mut value = Vec::new();
            for id in unique(&ids)? {
                let changed = items::keep(tx, &id, WorkActor::Me, now)?;
                effects.changed(&changed);
                value.push(changed.after);
            }
            Ok(Done { value, effects })
        })
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_day(
    history_state: State<'_, Arc<HistoryManager>>,
    day: String,
) -> Result<WorkDay, String> {
    super::history(&history_state, |history| items::day(history, &day))
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_day_plan(
    history_state: State<'_, Arc<HistoryManager>>,
    operation_id: String,
    day: String,
    focus_ids: Vec<String>,
) -> Result<WorkDay, String> {
    let now = now_ms();
    super::history(&history_state, |history| {
        store::mutate(
            history,
            &operation_id,
            "work_day_plan",
            &(&day, &focus_ids),
            now,
            |tx| items::plan_day(tx, &day, &focus_ids, now),
        )
    })
}

/// The project screen's change to one task: its status, and whether it is
/// the person's own.
pub(super) struct TaskUpdate<'a> {
    pub(super) id: &'a str,
    pub(super) expected_revision: i64,
    pub(super) status: TaskStatus,
    pub(super) owner_is_self: Option<bool>,
}

/// The project screen's status menu and "mine" toggle, written through the
/// tracker so the item's history and its commitment follow.
pub(super) fn update_task(
    history: &mut Connection,
    meetings: Option<&Connection>,
    update: &TaskUpdate<'_>,
    operation_id: &str,
    now_ms: i64,
) -> StorageResult<Done<PlanningTask>> {
    store::mutate(
        history,
        operation_id,
        "update_task",
        &(
            update.id,
            update.expected_revision,
            update.status,
            update.owner_is_self,
        ),
        now_ms,
        |tx| {
            let id = store::uuid(update.id)?;
            if update.expected_revision < 1 {
                return Err(Refusal::Invalid.into());
            }
            let current = store::task(tx, &id)?;
            store::project(tx, &current.project_id)?;
            if let Some(milestone_id) = &current.milestone_id {
                if store::milestone(tx, milestone_id)?.project_id != current.project_id {
                    return Err(Refusal::InvalidMembership.into());
                }
            }
            if current.revision != update.expected_revision {
                return Err(Refusal::StaleRevision.into());
            }
            let owner = match update.owner_is_self {
                Some(true) if current.owner_person_id.is_some() => {
                    return Err(Refusal::Invalid.into());
                }
                Some(true) => Some(WorkOwner::Myself),
                Some(false) if current.owner_is_self => Some(WorkOwner::Nobody),
                Some(false) | None => None,
            };
            let patch = WorkPatch {
                status: (current.status != update.status).then(|| WorkStatus::from(update.status)),
                owner,
                ..WorkPatch::default()
            };
            let mut effects = Effects::default();
            let change = Change {
                operation_id,
                actor: WorkActor::Me,
                at_ms: now_ms,
            };
            edit(tx, meetings, change, &id, &patch, &mut effects)?;
            Ok(Done {
                value: store::task(tx, &id)?,
                effects,
            })
        },
    )
}
