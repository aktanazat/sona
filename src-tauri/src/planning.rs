//! Owner-authored local plans and references to their original evidence.

pub(crate) mod person;
mod proposal;
mod proposal_sources;
pub(crate) mod quiet;
mod schema;
mod sources;
pub(crate) mod store;
pub(crate) mod suggestions;
mod types;
pub(crate) mod untracked;
pub(crate) mod work;

pub use proposal::{PlanningDraft, PlanningSourceRef};
pub use schema::{
    DAY_PLAN_MIGRATION, DISMISSED_LOOP_MIGRATION, MEETING_NOTE_LINK_MIGRATION, MIGRATION,
    PROJECT_DELETE_KEEPS_TASKS_MIGRATION, QUIET_REVIEW_MIGRATION, WORK_MIGRATION,
};
pub use types::{
    EvidenceLink, EvidenceSourceKind, EvidenceTargetKind, Milestone, PlanningCalendarPage,
    PlanningTask, Project, ProjectDetail, ProjectStatus, ProjectSummary, TaskStatus,
};

use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use anyhow::Result as StorageResult;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_specta::Event;

#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct PlanningChangedEvent {
    pub project_id: String,
}

fn changed(app: &AppHandle, project_id: String) {
    let event = PlanningChangedEvent { project_id };
    if event.emit(app).is_err() {
        log::warn!("Planning change event could not be delivered");
    }
}

/// What a planner command answers when the conversations, and the people in
/// them, cannot be opened.
pub(crate) const CONVERSATIONS_CLOSED: &str =
    "Conversations and the people in them can't be opened right now.";
/// [`CONVERSATIONS_CLOSED`] in portable mode, which keeps no conversations.
pub(crate) const PORTABLE_NO_CONVERSATIONS: &str =
    "Portable mode keeps no conversations or people.";

/// Work that needs the conversations found them closed. Not a
/// [`store::Refusal`]: it passes once they open, so it is never kept as an
/// operation's answer.
#[derive(Debug)]
pub(crate) struct ConversationsClosed;

impl std::fmt::Display for ConversationsClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the conversations are closed")
    }
}

impl std::error::Error for ConversationsClosed {}

fn conversations_closed() -> String {
    if crate::portable::is_portable() {
        PORTABLE_NO_CONVERSATIONS
    } else {
        CONVERSATIONS_CLOSED
    }
    .to_string()
}

fn failure(error: anyhow::Error) -> String {
    if error.is::<ConversationsClosed>() {
        return conversations_closed();
    }
    match error.downcast_ref::<store::Refusal>() {
        Some(store::Refusal::Invalid) => "Check the title, date, and selected owner.",
        Some(store::Refusal::NotFound) => "This item no longer exists.",
        Some(store::Refusal::StaleRevision) => "This item changed. Reload and try again.",
        Some(store::Refusal::OperationConflict) => {
            "This change was already used. Reload and try again."
        }
        Some(store::Refusal::InvalidMembership) => "Choose a milestone in this project.",
        Some(store::Refusal::SourceUnavailable) => "This source is unavailable.",
        Some(store::Refusal::OwnerUnavailable) => "Choose an available person.",
        Some(store::Refusal::ExistingCommitment) => {
            "Link existing meeting work instead of creating a duplicate task."
        }
        Some(store::Refusal::CaptureActive) => "Planning is paused while recording.",
        Some(store::Refusal::SourceChanged) => "A source changed. Prepare the plan again.",
        Some(store::Refusal::AlreadyTracked) => "This action is already tracked.",
        None => {
            log::error!("Planning database operation failed");
            "Plans could not be saved or read. Try again."
        }
    }
    .to_string()
}

pub(crate) fn history<T>(
    history: &HistoryManager,
    work: impl FnOnce(&mut Connection) -> StorageResult<T>,
) -> Result<T, String> {
    history.with_connection(work).map_err(failure)
}

async fn with_sources<T: Send>(
    history: &HistoryManager,
    meetings: &MeetingSessionManager,
    work: impl FnOnce(&mut Connection, &Connection) -> StorageResult<T> + Send,
) -> Result<T, String> {
    let store = meetings.store().await.map_err(|_| conversations_closed())?;
    store
        .with_planning_snapshot(|meeting_conn| {
            Ok(history.with_connection(|history_conn| work(history_conn, meeting_conn)))
        })
        .map_err(|_| "Conversations could not be read.".to_string())?
        .map_err(failure)
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_projects(
    history_state: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<ProjectSummary>, String> {
    history(&history_state, |conn| store::projects(conn))
}

#[tauri::command]
#[specta::specta]
pub async fn planning_project(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    id: String,
) -> Result<ProjectDetail, String> {
    project_detail_without(&history_state, &meetings, id, |_| false).await
}

/// One project's page for a reader who may not see the evidence `hides`
/// names: it leaves before the pulse counts the meetings behind it.
pub(crate) async fn project_detail_without(
    history_state: &HistoryManager,
    meetings: &MeetingSessionManager,
    id: String,
    hides: impl Fn(&EvidenceLink) -> bool + Send + Sync,
) -> Result<ProjectDetail, String> {
    let id = store::uuid(&id).map_err(failure)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (records, local_only) = history(history_state, |conn| {
        let mut records = store::records(conn, &id)?;
        records.evidence.retain(|link| !hides(link));
        let local_only = records
            .tasks
            .iter()
            .all(|task| task.owner_person_id.is_none())
            && records
                .evidence
                .iter()
                .all(|link| link.source_kind == EvidenceSourceKind::Note);
        if local_only {
            for link in &mut records.evidence {
                sources::resolve_note(conn, link)?;
            }
        }
        Ok((records, local_only))
    })?;
    if local_only {
        return store::detail(records, None, now_ms).map_err(failure);
    }
    with_sources(history_state, meetings, |history_conn, meeting_conn| {
        let mut records = store::records(history_conn, &id)?;
        records.evidence.retain(|link| !hides(link));
        for task in &mut records.tasks {
            sources::resolve_owner(meeting_conn, task)?;
        }
        for link in &mut records.evidence {
            sources::resolve_link(history_conn, meeting_conn, link)?;
        }
        store::detail(records, Some(meeting_conn), now_ms)
    })
    .await
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_create_project(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    title: String,
    description: Option<String>,
    operation_id: String,
) -> Result<Project, String> {
    let project = history(&history_state, |conn| {
        store::create_project(
            conn,
            &title,
            description.as_deref(),
            &operation_id,
            chrono::Utc::now().timestamp_millis(),
        )
    })?;
    changed(&app, project.id.clone());
    Ok(project)
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_create_milestone(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    project_id: String,
    title: String,
    due_at_ms: Option<i64>,
    operation_id: String,
) -> Result<Milestone, String> {
    let milestone = history(&history_state, |conn| {
        store::create_milestone(
            conn,
            &project_id,
            &title,
            due_at_ms,
            &operation_id,
            chrono::Utc::now().timestamp_millis(),
        )
    })?;
    changed(&app, milestone.project_id.clone());
    Ok(milestone)
}

#[derive(Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlanningTaskInput {
    project_id: String,
    milestone_id: Option<String>,
    title: String,
    details: Option<String>,
    due_at_ms: Option<i64>,
    owner_person_id: Option<String>,
    owner_is_self: Option<bool>,
}

#[tauri::command]
#[specta::specta]
pub async fn planning_create_task(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    input: PlanningTaskInput,
    operation_id: String,
) -> Result<PlanningTask, String> {
    let draft = store::TaskDraft {
        project_id: &input.project_id,
        milestone_id: input.milestone_id.as_deref(),
        title: &input.title,
        details: input.details.as_deref(),
        due_at_ms: input.due_at_ms,
        owner_person_id: input.owner_person_id.as_deref(),
        owner_is_self: input.owner_is_self.unwrap_or(false),
    };
    let task = if draft.owner_person_id.is_none() {
        history(&history_state, |conn| {
            store::create_task(
                conn,
                &draft,
                &operation_id,
                chrono::Utc::now().timestamp_millis(),
                |_| Err(store::Refusal::OwnerUnavailable.into()),
            )
        })?
    } else {
        with_sources(&history_state, &meetings, |history_conn, meeting_conn| {
            let mut task = store::create_task(
                history_conn,
                &draft,
                &operation_id,
                chrono::Utc::now().timestamp_millis(),
                |id| sources::require_person(meeting_conn, id),
            )?;
            sources::resolve_owner(meeting_conn, &mut task)?;
            Ok(task)
        })
        .await?
    };
    changed(&app, task.project_id.clone());
    work::tasks_changed(&app, vec![task.id.clone()]);
    Ok(task)
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_update_project(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    id: String,
    expected_revision: i64,
    status: ProjectStatus,
    operation_id: String,
) -> Result<Project, String> {
    let project = history(&history_state, |conn| {
        store::update_project(
            conn,
            &id,
            expected_revision,
            status,
            &operation_id,
            chrono::Utc::now().timestamp_millis(),
        )
    })?;
    changed(&app, project.id.clone());
    Ok(project)
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_update_milestone(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    id: String,
    expected_revision: i64,
    status: TaskStatus,
    operation_id: String,
) -> Result<Milestone, String> {
    let milestone = history(&history_state, |conn| {
        store::update_milestone(
            conn,
            &id,
            expected_revision,
            status,
            &operation_id,
            chrono::Utc::now().timestamp_millis(),
        )
    })?;
    changed(&app, milestone.project_id.clone());
    Ok(milestone)
}

#[tauri::command]
#[specta::specta]
pub async fn planning_update_task(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    id: String,
    expected_revision: i64,
    status: TaskStatus,
    owner_is_self: Option<bool>,
    operation_id: String,
) -> Result<PlanningTask, String> {
    let update = work::TaskUpdate {
        id: &id,
        expected_revision,
        status,
        owner_is_self,
    };
    let done = work::with_meetings(&history_state, &meetings, |history_conn, meeting_conn| {
        work::update_task(
            history_conn,
            meeting_conn,
            &update,
            &operation_id,
            chrono::Utc::now().timestamp_millis(),
        )
    })
    .await?;
    work::announce(&app, &meetings, &done.effects).await;
    Ok(done.value)
}

#[tauri::command]
#[specta::specta]
pub async fn planning_link_source(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    target_kind: EvidenceTargetKind,
    target_id: String,
    source_kind: EvidenceSourceKind,
    source_id: String,
    at_ms: Option<i64>,
    operation_id: String,
) -> Result<EvidenceLink, String> {
    let source_id = sources::canonical_source_id(source_kind, &source_id).map_err(failure)?;
    let target_id = store::uuid(&target_id).map_err(failure)?;
    let draft = store::SourceLinkDraft {
        target_kind,
        target_id: &target_id,
        source_kind,
        source_id: &source_id,
        at_ms,
        meeting_note_id: None,
    };
    let (link, project_id) = if source_kind == EvidenceSourceKind::Note {
        history(&history_state, |history_conn| {
            let mut link = store::link_source(
                history_conn,
                &draft,
                &operation_id,
                chrono::Utc::now().timestamp_millis(),
                |conn| sources::require_note(conn, &source_id, at_ms),
            )?;
            sources::resolve_note(history_conn, &mut link)?;
            let project_id = store::target_project(history_conn, target_kind, &target_id)?;
            Ok((link, project_id))
        })?
    } else {
        with_sources(&history_state, &meetings, |history_conn, meeting_conn| {
            let mut link = store::link_source(
                history_conn,
                &draft,
                &operation_id,
                chrono::Utc::now().timestamp_millis(),
                |conn| sources::require_source(conn, meeting_conn, source_kind, &source_id, at_ms),
            )?;
            sources::resolve_link(history_conn, meeting_conn, &mut link)?;
            let project_id = store::target_project(history_conn, target_kind, &target_id)?;
            Ok((link, project_id))
        })
        .await?
    };
    changed(&app, project_id);
    if target_kind == EvidenceTargetKind::Task {
        work::tasks_changed(&app, vec![target_id]);
    }
    Ok(link)
}

#[tauri::command]
#[specta::specta]
pub async fn planning_calendar(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    start_ms: i64,
    end_ms: i64,
) -> Result<PlanningCalendarPage, String> {
    let store = match meetings.store().await {
        Ok(store) => store,
        Err(_) => {
            let items = history(&history_state, |conn| {
                store::calendar(conn, start_ms, end_ms)
            })?;
            return Ok(PlanningCalendarPage {
                items,
                conversations_available: false,
            });
        }
    };
    store
        .with_planning_snapshot(|meeting_conn| {
            Ok(history_state.with_connection(|history_conn| {
                let mut items = store::calendar(history_conn, start_ms, end_ms)?;
                items.extend(sources::calendar(meeting_conn, start_ms, end_ms)?);
                items.sort_by(|left, right| {
                    left.due_at_ms
                        .cmp(&right.due_at_ms)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Ok(PlanningCalendarPage {
                    items,
                    conversations_available: true,
                })
            }))
        })
        .map_err(|_| "Conversations could not be read.".to_string())?
        .map_err(failure)
}

async fn proposal_context(
    history_state: &HistoryManager,
    meetings: &MeetingSessionManager,
    selected_sources: Vec<PlanningSourceRef>,
) -> Result<Vec<proposal_sources::ResolvedSource>, String> {
    let targets = proposal_sources::selected_sources(selected_sources).map_err(failure)?;
    if targets
        .iter()
        .all(|target| target.kind == EvidenceSourceKind::Note)
    {
        history(history_state, |conn| {
            targets
                .iter()
                .map(|target| proposal_sources::resolve(conn, None, target))
                .collect::<StorageResult<Vec<_>>>()
        })
    } else {
        with_sources(history_state, meetings, |history_conn, meeting_conn| {
            targets
                .iter()
                .map(|target| proposal_sources::resolve(history_conn, Some(meeting_conn), target))
                .collect::<StorageResult<Vec<_>>>()
        })
        .await
    }
}

#[tauri::command]
#[specta::specta]
pub async fn planning_prepare(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    goal: String,
    selected_sources: Vec<PlanningSourceRef>,
) -> Result<PlanningDraft, String> {
    crate::scratchpad::synthesis::require_idle(&app)?;
    let sources = proposal_context(&history_state, &meetings, selected_sources).await?;
    let processing = meetings.processing().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::scratchpad::synthesis::require_idle(&app)?;
        let generator = processing.local_text_generator().ok_or_else(|| {
            "No local model is ready. Choose one in Meeting settings or enable Apple Intelligence.".to_string()
        })?;
        let draft = proposal::generate(generator.as_ref(), goal, sources)?;
        crate::scratchpad::synthesis::require_idle(&app)?;
        Ok(draft)
    }).await.map_err(|_| "The local model could not finish. Try again.".to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn planning_revise(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    draft: PlanningDraft,
    requests: Vec<String>,
) -> Result<PlanningDraft, String> {
    crate::scratchpad::synthesis::require_idle(&app)?;
    let selected_sources = draft
        .sources
        .iter()
        .map(|source| source.source.clone())
        .collect();
    let sources = proposal_context(&history_state, &meetings, selected_sources).await?;
    let processing = meetings.processing().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::scratchpad::synthesis::require_idle(&app)?;
        let generator = processing.local_text_generator().ok_or_else(|| {
            "No local model is ready. Choose one in Meeting settings or enable Apple Intelligence.".to_string()
        })?;
        let revised = proposal::revise(generator.as_ref(), draft, requests, sources)?;
        crate::scratchpad::synthesis::require_idle(&app)?;
        Ok(revised)
    }).await.map_err(|_| "The local model could not finish. Try again.".to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn planning_accept(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    draft: PlanningDraft,
    operation_id: String,
) -> Result<ProjectDetail, String> {
    crate::scratchpad::synthesis::require_idle(&app)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let detail = if draft
        .sources
        .iter()
        .all(|source| source.source.kind == EvidenceSourceKind::Note)
        && draft.linked_commitments.is_empty()
        && draft
            .tasks
            .iter()
            .all(|task| task.owner_person_id.is_none())
    {
        history(&history_state, |history_conn| {
            crate::scratchpad::synthesis::require_idle(&app)
                .map_err(|_| store::Refusal::CaptureActive)?;
            proposal::accept(history_conn, None, &draft, &operation_id, now_ms)
        })?
    } else {
        with_sources(&history_state, &meetings, |history_conn, meeting_conn| {
            crate::scratchpad::synthesis::require_idle(&app)
                .map_err(|_| store::Refusal::CaptureActive)?;
            proposal::accept(
                history_conn,
                Some(meeting_conn),
                &draft,
                &operation_id,
                now_ms,
            )
        })
        .await?
    };
    changed(&app, detail.project.id.clone());
    work::tasks_changed(
        &app,
        detail.tasks.iter().map(|task| task.id.clone()).collect(),
    );
    Ok(detail)
}
