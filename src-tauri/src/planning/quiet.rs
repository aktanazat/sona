use super::proposal::PlanningSourceRef;
use super::proposal_sources::{self, ResolvedSource};
use super::store::{self, Refusal};
use super::suggestions;
use super::types::EvidenceSourceKind;
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use anyhow::Result;
use chrono::Timelike;
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tauri::{AppHandle, Manager, State};
use tokio::sync::Notify;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
extern "C" {
    fn sona_quiet_review_environment() -> i32;
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn initialize() {
    // SAFETY: Installing process-local sleep/session observers reads no private store and requests no access.
    unsafe { sona_quiet_review_environment() };
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct QuietReviewState {
    pub project_id: String,
    pub enabled: bool,
    pub selected_sources: Vec<PlanningSourceRef>,
    pub last_completed_at_ms: Option<i64>,
    pub last_outcome: Option<String>,
}

struct Grant {
    state: QuietReviewState,
    revision: i64,
    last_input_hash: Option<String>,
}

#[derive(Debug)]
struct QuietError(&'static str);

impl std::fmt::Display for QuietError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "quiet_review:{}", self.0)
    }
}

impl std::error::Error for QuietError {}

fn failure(error: anyhow::Error) -> String {
    if let Some(error) = error.downcast_ref::<QuietError>() {
        return error.to_string();
    }
    match error.downcast_ref::<Refusal>() {
        Some(Refusal::Invalid) => QuietError("invalid_request"),
        Some(Refusal::SourceChanged | Refusal::SourceUnavailable) => QuietError("source_changed"),
        Some(Refusal::StaleRevision | Refusal::NotFound) => QuietError("target_changed"),
        _ => QuietError("unavailable"),
    }
    .to_string()
}

#[derive(Default)]
pub(crate) struct Cancellation {
    cancelled: AtomicBool,
    wake: Notify,
}

impl Cancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) async fn cancelled(&self) {
        if !self.is_cancelled() {
            self.wake.notified().await;
        }
    }
}

struct ActiveReview {
    project_id: String,
    cancellation: Arc<Cancellation>,
}

#[derive(Default)]
struct Runtime {
    active: Option<ActiveReview>,
    foreground_work: usize,
}

static RUNTIME: LazyLock<Mutex<Runtime>> = LazyLock::new(|| Mutex::new(Runtime::default()));

pub(crate) struct ForegroundActivity;

pub(crate) fn foreground_activity() -> ForegroundActivity {
    let mut runtime = RUNTIME.lock().expect("quiet review runtime poisoned");
    runtime.foreground_work += 1;
    if let Some(active) = &runtime.active {
        active.cancellation.cancel();
    }
    ForegroundActivity
}

impl Drop for ForegroundActivity {
    fn drop(&mut self) {
        let mut runtime = RUNTIME.lock().expect("quiet review runtime poisoned");
        runtime.foreground_work -= 1;
    }
}

pub(crate) fn interrupt() {
    let runtime = RUNTIME.lock().expect("quiet review runtime poisoned");
    if let Some(active) = &runtime.active {
        active.cancellation.cancel();
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[no_mangle]
pub extern "C" fn sona_quiet_review_interrupt() {
    interrupt();
}

fn cancel_project(project_id: &str) {
    let runtime = RUNTIME.lock().expect("quiet review runtime poisoned");
    if let Some(active) = runtime
        .active
        .as_ref()
        .filter(|active| active.project_id == project_id)
    {
        active.cancellation.cancel();
    }
}

struct RunGuard {
    cancellation: Arc<Cancellation>,
}

impl RunGuard {
    fn acquire(project_id: &str) -> Result<Self> {
        let mut runtime = RUNTIME.lock().expect("quiet review runtime poisoned");
        if runtime.active.is_some() || runtime.foreground_work != 0 {
            return Err(QuietError("busy").into());
        }
        let cancellation = Arc::new(Cancellation::default());
        runtime.active = Some(ActiveReview {
            project_id: project_id.to_string(),
            cancellation: Arc::clone(&cancellation),
        });
        Ok(Self { cancellation })
    }

    fn check(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            Err(QuietError("cancelled").into())
        } else {
            Ok(())
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let mut runtime = RUNTIME.lock().expect("quiet review runtime poisoned");
        if runtime
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.cancellation, &self.cancellation))
        {
            runtime.active = None;
        }
    }
}

fn environment(app: &AppHandle) -> Result<()> {
    if app
        .try_state::<crate::agent_panel::AgentPanelManager>()
        .is_some_and(|panel| panel.has_active_model_work())
    {
        return Err(QuietError("busy").into());
    }
    if !matches!(
        crate::settings::get_settings(app).meeting_local_engine,
        crate::settings::MeetingLocalEngine::AppleIntelligence
    ) {
        return Err(QuietError("apple_required").into());
    }
    let hour = chrono::Local::now().hour();
    if (6..22).contains(&hour) {
        return Err(QuietError("outside_hours").into());
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        // SAFETY: The process-local bridge reads only session and input-idle state without requesting access.
        match unsafe { sona_quiet_review_environment() } {
            0 => Ok(()),
            1 => Err(QuietError("sleeping").into()),
            2 => Err(QuietError("user_active").into()),
            _ => Err(QuietError("unavailable").into()),
        }
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        Err(QuietError("apple_unavailable").into())
    }
}

const COLUMNS: &str = "project_id, enabled, selected_sources_json, grant_revision, last_input_hash, last_completed_at_ms, last_outcome";

fn read_grant(row: &Row<'_>) -> rusqlite::Result<Grant> {
    let sources: String = row.get(2)?;
    let selected_sources = serde_json::from_str(&sources).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Grant {
        state: QuietReviewState {
            project_id: row.get(0)?,
            enabled: row.get(1)?,
            selected_sources,
            last_completed_at_ms: row.get(5)?,
            last_outcome: row.get(6)?,
        },
        revision: row.get(3)?,
        last_input_hash: row.get(4)?,
    })
}

fn grant(history: &Connection, project_id: &str) -> Result<Grant> {
    history
        .query_row(
            &format!("SELECT {COLUMNS} FROM planning_quiet_review WHERE project_id = ?1"),
            [project_id],
            read_grant,
        )
        .optional()?
        .ok_or_else(|| QuietError("consent_required").into())
}

fn require_grant(history: &Connection, expected: &Grant) -> Result<Grant> {
    let current = grant(history, &expected.state.project_id)?;
    if !current.state.enabled
        || current.revision != expected.revision
        || current.state.selected_sources != expected.state.selected_sources
    {
        return Err(QuietError("cancelled").into());
    }
    Ok(current)
}

fn snapshot<T>(
    history: &HistoryManager,
    meetings: Option<&MeetingStore>,
    work: impl FnOnce(&mut Connection, Option<&Connection>) -> Result<T>,
) -> Result<T> {
    if !history.storage_is_ready() {
        return Err(QuietError("unavailable").into());
    }
    if let Some(meetings) = meetings {
        meetings
            .with_planning_snapshot(|meeting_conn| {
                Ok(history.with_connection(|history_conn| work(history_conn, Some(meeting_conn))))
            })
            .map_err(|_| QuietError("unavailable"))?
    } else {
        history.with_connection(|history_conn| work(history_conn, None))
    }
}

fn resolve_sources(
    history: &Connection,
    meetings: Option<&Connection>,
    grant: &Grant,
) -> Result<Vec<ResolvedSource>> {
    grant
        .state
        .selected_sources
        .iter()
        .map(|source| {
            proposal_sources::resolve(history, meetings, source)
                .map_err(|_| QuietError("source_changed").into())
        })
        .collect()
}

fn input_hash(
    history: &Connection,
    project_id: &str,
    sources: &[ResolvedSource],
) -> Result<(String, String, String)> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct InputReceipt<'a> {
        version: u8,
        project_id: &'a str,
        target_hash: &'a str,
        sources: Vec<&'a super::proposal::PlanningReviewSource>,
    }
    let target_hash = proposal_sources::fingerprint(&store::records(history, project_id)?)?;
    let receipt = serde_json::to_string(&InputReceipt {
        version: 1,
        project_id,
        target_hash: &target_hash,
        sources: sources.iter().map(|source| &source.review).collect(),
    })?;
    let hash = format!("{:x}", Sha256::digest(receipt.as_bytes()));
    Ok((hash, target_hash, receipt))
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_quiet_review_grants(
    history_state: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<QuietReviewState>, String> {
    history_state
        .with_connection(|history| {
            let mut statement = history.prepare(&format!(
                "SELECT {COLUMNS} FROM planning_quiet_review ORDER BY project_id"
            ))?;
            let rows = statement.query_map([], read_grant)?;
            Ok(rows
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(|grant| grant.state)
                .collect())
        })
        .map_err(failure)
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_quiet_review_set(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    project_id: String,
    enabled: bool,
    selected_sources: Vec<PlanningSourceRef>,
) -> Result<QuietReviewState, String> {
    let project_id = store::uuid(&project_id).map_err(failure)?;
    cancel_project(&project_id);
    let selected_sources = if enabled {
        let mut selected = proposal_sources::selected_sources(selected_sources).map_err(failure)?;
        if selected.is_empty() {
            return Err(QuietError("invalid_request").to_string());
        }
        selected.sort_by(|a, b| (a.kind.as_str(), &a.id).cmp(&(b.kind.as_str(), &b.id)));
        Some(selected)
    } else {
        None
    };
    let state = history_state.with_connection(|history| {
        let transaction = history.transaction_with_behavior(TransactionBehavior::Immediate)?;
        store::project(&transaction, &project_id)?;
        let previous = transaction.query_row(
            &format!("SELECT {COLUMNS} FROM planning_quiet_review WHERE project_id = ?1"),
            [&project_id], read_grant,
        ).optional()?;
        let sources = selected_sources.or_else(|| previous.as_ref().map(|grant| grant.state.selected_sources.clone())).unwrap_or_default();
        let revision = previous.as_ref().map_or(Ok(1), |grant| grant.revision.checked_add(1).ok_or(QuietError("failed")))?;
        transaction.execute(
            "INSERT INTO planning_quiet_review (project_id, enabled, selected_sources_json, grant_revision)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(project_id) DO UPDATE SET
             enabled = excluded.enabled, selected_sources_json = excluded.selected_sources_json,
             grant_revision = excluded.grant_revision",
            params![project_id, enabled, serde_json::to_string(&sources)?, revision],
        )?;
        let state = grant(&transaction, &project_id)?.state;
        transaction.commit()?;
        Ok(state)
    }).map_err(failure)?;
    super::changed(&app, project_id);
    Ok(state)
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_quiet_review_cancel(project_id: String) -> Result<(), String> {
    let project_id = store::uuid(&project_id).map_err(failure)?;
    cancel_project(&project_id);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn planning_quiet_review_run(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    project_id: String,
) -> Result<QuietReviewState, String> {
    let project_id = store::uuid(&project_id).map_err(failure)?;
    let run = RunGuard::acquire(&project_id).map_err(failure)?;
    if !history_state.storage_is_ready() {
        return Err(QuietError("unavailable").to_string());
    }
    let expected = history_state
        .with_connection(|history| grant(history, &project_id))
        .map_err(failure)?;
    if !expected.state.enabled {
        return Err(QuietError("consent_required").to_string());
    }
    environment(&app).map_err(failure)?;
    crate::scratchpad::synthesis::require_idle(&app).map_err(|_| QuietError("busy").to_string())?;
    let suggestion_guard =
        suggestions::GenerationGuard::acquire().map_err(|_| QuietError("busy").to_string())?;
    let store = if expected
        .state
        .selected_sources
        .iter()
        .all(|source| source.kind == EvidenceSourceKind::Note)
    {
        None
    } else {
        Some(
            meetings
                .cloud_store()
                .await
                .map_err(|_| QuietError("unavailable").to_string())?,
        )
    };
    let history = Arc::clone(&history_state);
    let app_for_run = app.clone();
    let state = tauri::async_runtime::spawn_blocking(move || {
        let _suggestion_guard = suggestion_guard;
        run.check()?;
        environment(&app_for_run)?;
        let generator = crate::meeting::processing::quiet_review_generator(Arc::clone(&run.cancellation))
            .ok_or(QuietError("apple_unavailable"))?;
        let (targets, sources, hash, target_hash, receipt, previous) = snapshot(&history, store.as_deref(), |history, meetings| {
            let current = require_grant(history, &expected)?;
            let targets = suggestions::targets(history, &project_id)?;
            let sources = resolve_sources(history, meetings, &current)?;
            let (hash, target_hash, receipt) = input_hash(history, &project_id, &sources)?;
            Ok((targets, sources, hash, target_hash, receipt, current))
        })?;
        run.check()?;
        if previous.last_input_hash.as_deref() == Some(hash.as_str()) {
            return Ok(previous.state);
        }
        let candidates = suggestions::generate(generator.as_ref(), &targets, &sources)
            .map_err(|_| if run.cancellation.is_cancelled() { QuietError("cancelled") } else { QuietError("failed") })?;
        run.check()?;
        environment(&app_for_run)?;
        let state = snapshot(&history, store.as_deref(), |history, meetings| {
            let transaction = history.transaction_with_behavior(TransactionBehavior::Immediate)?;
            require_grant(&transaction, &expected)?;
            let current_sources = resolve_sources(&transaction, meetings, &expected)?;
            let (current_hash, current_target_hash, _) = input_hash(&transaction, &project_id, &current_sources)?;
            if current_target_hash != target_hash {
                return Err(QuietError("target_changed").into());
            }
            if current_hash != hash {
                return Err(QuietError("source_changed").into());
            }
            let runtime = RUNTIME.lock().expect("quiet review runtime poisoned");
            run.check()?;
            if runtime.foreground_work != 0 {
                return Err(QuietError("busy").into());
            }
            environment(&app_for_run)?;
            let now_ms = chrono::Utc::now().timestamp_millis();
            let added = suggestions::enqueue_in(&transaction, meetings, &targets, &sources, &candidates, now_ms)?;
            let outcome = if added == 0 { "nothing_new" } else { "suggestions_ready" };
            transaction.execute(
                "UPDATE planning_quiet_review SET last_input_hash = ?2, last_grant_revision = ?3,
                 last_completed_at_ms = ?4, last_outcome = ?5, last_input_json = ?6 WHERE project_id = ?1",
                params![project_id, hash, expected.revision, now_ms, outcome, receipt],
            )?;
            let state = grant(&transaction, &project_id)?.state;
            transaction.commit()?;
            drop(runtime);
            Ok(state)
        })?;
        Ok::<_, anyhow::Error>(state)
    }).await.map_err(|_| QuietError("failed").to_string())?.map_err(failure)?;
    super::changed(&app, state.project_id.clone());
    Ok(state)
}
