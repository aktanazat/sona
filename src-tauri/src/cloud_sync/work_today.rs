//! The work the paired phone shows: one snapshot of what needs the user
//! today, and the decisions the phone sends back.
//!
//! The snapshot is one object per Mac, written again whenever what it says
//! changes. A decision is one object the phone writes; this Mac applies it
//! once, as the user, and names it in the next snapshot's `handled` list so
//! the phone learns how it went. Tasks drawn from a meeting kept on this Mac,
//! and notices about a personal document, never leave it.

use super::{
    agent_asks::download_empty_object,
    memory_index::phone_sync_directory,
    open_object_revision_payload,
    revision_upload::{self, StagedRevision, UploadRefused},
    sha256_base64url, utc_now_ms, write_staged_file, CloudAccess, CloudRuntimeError,
    CloudSyncRuntime, ObjectPayloadManifest, ObjectRevisionCryptoContext, ObjectRevisionEnvelope,
    MAX_PLAINTEXT_CHUNK_BYTES,
};
use crate::managers::history::HistoryManager;
use crate::planning::work::comments::Author;
use crate::planning::work::inbox::{
    self, rows::InboxRow, InboxFilter, Mark, NotificationKind, Shown,
};
use crate::planning::work::types::{WorkActor, WorkDirection, WorkItem, WorkPatch, WorkStatus};
use crate::planning::work::{digest, items, sessions};
use crate::query::KeptLocal;
use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::Arc,
};
use tauri::{AppHandle, Manager};
use uuid::Uuid;
use zeroize::Zeroizing;

const SOURCE_FORMAT: &str = "sona-work-today-v1";
pub(super) const ACTION_SOURCE_FORMAT: &str = "sona-work-action-v1";
const CACHE_FILE: &str = "work-today.json";
const HANDLED_FILE: &str = "work-actions.json";
const MAX_ROWS: usize = 50;
const MAX_HANDLED: usize = 50;
const HANDLED_LIFETIME_MS: i64 = 2 * 24 * 60 * 60 * 1000;
const MAX_TEXT_BYTES: usize = 8 * 1024;
const MAX_NOTICE_IDS: usize = 200;
/// Names the planner operation a phone's decision is applied as, so the same
/// decision pulled twice changes nothing the second time.
const ACTION_NAMESPACE: Uuid = Uuid::from_u128(0x3c5e_91d2_7a40_4f6b_8e13_d27a_64b9_0c55);

#[derive(Serialize)]
struct WorkToday<'a> {
    format_version: u32,
    kind: &'static str,
    source_device_id: &'a str,
    produced_at_utc_ms: i64,
    #[serde(flatten)]
    rows: TodayRows,
    handled: Vec<Handled>,
}

/// What the phone lists, newest or most urgent first, at most `MAX_ROWS` each.
#[derive(Debug, Default, PartialEq, Serialize)]
pub(super) struct TodayRows {
    /// My open work due today or before.
    due: Vec<TodayTask>,
    triage: Vec<TodayTask>,
    /// Unread inbox rows other than agents' questions.
    inbox: Vec<TodayNotice>,
    /// Agents waiting on an answer in a session on a task.
    questions: Vec<TodayQuestion>,
}

#[derive(Debug, PartialEq, Serialize)]
struct TodayTask {
    id: String,
    number: i64,
    title: String,
    due_at_ms: Option<i64>,
    project: Option<String>,
    revision: i64,
}

#[derive(Debug, PartialEq, Serialize)]
struct TodayNotice {
    id: String,
    notice_ids: Vec<String>,
    task_id: Option<String>,
    task_number: Option<i64>,
    title: String,
    line: String,
    needs_you: bool,
    at_ms: i64,
}

#[derive(Debug, PartialEq, Serialize)]
struct TodayQuestion {
    session_id: String,
    task_id: String,
    task_number: i64,
    task_title: String,
    agent: String,
    question: String,
    options: Vec<TodayOption>,
    asked_at_ms: i64,
}

#[derive(Debug, PartialEq, Serialize)]
struct TodayOption {
    label: String,
    value: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Handled {
    action_id: String,
    outcome: Outcome,
    at_utc_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Applied,
    /// The Mac would not do it: the task changed there, is gone, or the
    /// decision is one this Mac does not know.
    Refused,
}

/// The rows, leaving out what may not leave this Mac. `kept` is `None` when
/// the meetings cannot be read, and then every task drawn from one stays.
pub(super) fn today_rows(
    conn: &Connection,
    kept: Option<&KeptLocal>,
    now_ms: i64,
) -> Result<TodayRows> {
    let hidden = digest::hidden(conn, kept)?;
    let (_, tomorrow_ms) = items::today_bounds(now_ms)?;
    let all = items::items(conn, false)?;
    let tasks: HashMap<&str, &WorkItem> = all.iter().map(|item| (item.id.as_str(), item)).collect();
    let projects: HashMap<String, String> = conn
        .prepare("SELECT id, title FROM planning_projects")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let shown = |item: &&WorkItem| !hidden.contains(&item.id) && !items::snoozed(item, now_ms);
    let task = |item: &WorkItem| TodayTask {
        id: item.id.clone(),
        number: item.number,
        title: item.title.clone(),
        due_at_ms: item.due_at_ms,
        project: item
            .project_id
            .as_ref()
            .and_then(|id| projects.get(id))
            .cloned(),
        revision: item.revision,
    };
    let mut due: Vec<&WorkItem> = all
        .iter()
        .filter(shown)
        .filter(|item| {
            item.direction == WorkDirection::Mine
                && item.status.is_open()
                && item.due_at_ms.is_some_and(|due| due < tomorrow_ms)
        })
        .collect();
    due.sort_by_key(|item| (item.due_at_ms, item.number));
    let mut triage: Vec<&WorkItem> = all
        .iter()
        .filter(shown)
        .filter(|item| item.status == WorkStatus::Triage)
        .collect();
    triage.sort_by_key(|item| (std::cmp::Reverse(item.created_at_ms), item.number));

    let personal = personal_notes(conn)?;
    let inbox = inbox::rows_of(
        conn,
        Shown {
            unread_only: true,
            ..Shown::default()
        },
        &InboxFilter::default(),
        now_ms,
    )?
    .into_iter()
    .filter(|row| {
        row.lead.kind != NotificationKind::Question
            && !row
                .item_id
                .iter()
                .chain(&row.item_ids)
                .any(|id| hidden.contains(id))
            && !row.lead.subject.as_ref().is_some_and(|subject| {
                subject.kind == inbox::SubjectKind::Note && personal.contains(&subject.id)
            })
    })
    .take(MAX_ROWS)
    .map(|row| notice(row, &tasks))
    .collect();

    let questions = sessions::sessions(conn, None, true, now_ms)?
        .into_iter()
        .filter(|session| !session.watched && !hidden.contains(&session.item_id))
        .filter_map(|session| {
            let asked = session.question?;
            let task = tasks.get(session.item_id.as_str())?;
            Some(TodayQuestion {
                session_id: session.id,
                task_id: task.id.clone(),
                task_number: task.number,
                task_title: task.title.clone(),
                agent: session.agent,
                question: asked.body.unwrap_or_default(),
                options: asked
                    .options
                    .into_iter()
                    .map(|option| TodayOption {
                        label: option.label,
                        value: option.value,
                    })
                    .collect(),
                asked_at_ms: asked.at_ms,
            })
        })
        .take(MAX_ROWS)
        .collect();

    Ok(TodayRows {
        due: due.into_iter().take(MAX_ROWS).map(task).collect(),
        triage: triage.into_iter().take(MAX_ROWS).map(task).collect(),
        inbox,
        questions,
    })
}

/// Personal documents, which no notice about may leave this Mac. The notes
/// table lives beside the planner's, so a planner-only database has none.
fn personal_notes(conn: &Connection) -> Result<HashSet<String>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'scratch_notes')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(HashSet::new());
    }
    let mut statement = conn.prepare("SELECT id FROM scratch_notes WHERE personal = 1")?;
    let ids = statement.query_map([], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

fn notice(row: InboxRow, tasks: &HashMap<&str, &WorkItem>) -> TodayNotice {
    let task = row.item_id.as_deref().and_then(|id| tasks.get(id));
    let lead = &row.lead;
    let title = match (task, &lead.subject, &lead.automation) {
        (Some(task), _, _) => task.title.clone(),
        (None, Some(subject), _) => subject.name.clone().unwrap_or_else(|| "Deleted".to_owned()),
        (None, None, Some(automation)) => automation.name.clone(),
        (None, None, None) if lead.digest.is_some() => "Weekly project digest".to_owned(),
        (None, None, None) if lead.kind == NotificationKind::Housekeeping => {
            "Housekeeping".to_owned()
        }
        (None, None, None) => "A task deleted for good".to_owned(),
    };
    let line = if row.summary.is_empty() {
        notice_line(lead)
    } else {
        format!("{}: {}", who(lead), row.summary)
    };
    TodayNotice {
        id: row.id,
        notice_ids: row.notice_ids,
        task_id: task.map(|task| task.id.clone()),
        task_number: task.map(|task| task.number),
        title,
        line,
        needs_you: row.needs.is_some(),
        at_ms: row.created_at_ms,
    }
}

/// Who did it, as the Mac's inbox says it.
fn who(notice: &inbox::Notification) -> String {
    match notice.kind {
        NotificationKind::Reminder
        | NotificationKind::Due
        | NotificationKind::Digest
        | NotificationKind::Housekeeping => "Sona".to_owned(),
        NotificationKind::Automation => match (&notice.item_id, &notice.automation) {
            (Some(_), Some(automation)) => format!("Automation “{}”", automation.name),
            _ => "Automation".to_owned(),
        },
        _ => match notice.agent.as_deref().filter(|agent| !agent.is_empty()) {
            Some(agent) => agent.to_owned(),
            None if notice.actor == WorkActor::Sona => "Sona".to_owned(),
            None => "An agent".to_owned(),
        },
    }
}

/// One notice as the Mac's inbox row reads it: who, what, and the start of
/// the comment it is about.
fn notice_line(notice: &inbox::Notification) -> String {
    let to = notice.to.as_deref();
    let what = match notice.kind {
        NotificationKind::Status => to.and_then(status_label).map_or_else(
            || "changed its status".to_owned(),
            |label| format!("moved it to {label}"),
        ),
        NotificationKind::Comment => "commented".to_owned(),
        NotificationKind::Mention => "mentioned you".to_owned(),
        NotificationKind::Assigned => "assigned it to you".to_owned(),
        NotificationKind::Changed => changed(notice.field.as_deref(), to).to_owned(),
        NotificationKind::Reminder if notice.field.as_deref() == Some("update") => {
            "reminds you an update is due".to_owned()
        }
        NotificationKind::Reminder => "reminds you".to_owned(),
        NotificationKind::Due => "says it's due today".to_owned(),
        NotificationKind::Digest => "sent the weekly project digest".to_owned(),
        NotificationKind::Question => format!("asks: {}", to.unwrap_or_default()),
        NotificationKind::Project => match notice.field.as_deref() {
            Some("update") => "posted an update".to_owned(),
            Some("status") => "changed its status".to_owned(),
            _ => "changed it".to_owned(),
        },
        NotificationKind::View => match notice.field.as_deref() {
            Some("added") => "brought it into a followed view".to_owned(),
            Some("done") => "finished it in a followed view".to_owned(),
            Some("canceled") => "canceled it in a followed view".to_owned(),
            _ => "changed it in a followed view".to_owned(),
        },
        NotificationKind::Note => "edited it".to_owned(),
        NotificationKind::Housekeeping => to.unwrap_or("ran housekeeping").to_owned(),
        NotificationKind::Automation if to == Some("done") => "finished".to_owned(),
        NotificationKind::Automation => {
            let failed = if notice.earlier.is_empty() {
                "failed".to_owned()
            } else {
                format!("failed {} times", notice.earlier.len() + 1)
            };
            match notice
                .automation
                .as_ref()
                .and_then(|automation| automation.error.as_deref())
            {
                Some(error) => format!("{failed}: {error}"),
                None => failed,
            }
        }
        NotificationKind::Unblocked => {
            let verb = if to == Some("canceled") {
                "canceled"
            } else {
                "finished"
            };
            notice.from.as_deref().map_or_else(
                || "unblocked it".to_owned(),
                |from| format!("{verb} “{from}”, which blocked it"),
            )
        }
    };
    let line = format!("{} {what}", who(notice));
    match &notice.excerpt {
        Some(excerpt) => format!("{line}: “{excerpt}”"),
        None => line,
    }
}

fn status_label(status: &str) -> Option<&'static str> {
    Some(match status {
        "triage" => "Triage",
        "backlog" => "Backlog",
        "not_started" => "Todo",
        "in_progress" => "In progress",
        "done" => "Done",
        "canceled" => "Canceled",
        "duplicate" => "Duplicate",
        _ => return None,
    })
}

fn changed(field: Option<&str>, to: Option<&str>) -> &'static str {
    match field {
        Some("priority") => "changed the priority",
        Some("owner") => "reassigned it",
        Some("due") if to.is_none() => "cleared the due date",
        Some("due") => "changed the due date",
        Some("project") => "moved it to another project",
        Some("title") => "renamed it",
        Some("details") => "edited the description",
        Some("labels") => "changed the labels",
        Some("archived") => "archived it",
        Some("restored") => "restored it",
        Some("snoozed") => "snoozed it",
        Some("relation" | "block") => "changed its links",
        Some("recurrence") => "changed how it repeats",
        Some("parent") => "changed its parent",
        Some("attachment") => "changed its attachments",
        Some("reminder") if to.is_none() => "cleared the reminder",
        Some("reminder") => "set a reminder",
        _ => "changed it",
    }
}

/// A decision the phone sealed into the vault. Fields are in key order, so
/// it serializes with sorted keys as the phone writes it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkAction {
    acted_at_utc_ms: i64,
    /// `done`, `accept`, `decline`, `read`, `archive` or `answer`; any other
    /// is refused, so a newer phone learns this Mac cannot do it.
    action: String,
    body: Option<String>,
    device_id: String,
    expected_revision: Option<i64>,
    format_version: u32,
    kind: String,
    notice_ids: Vec<String>,
    session_id: Option<String>,
    target_device_id: String,
    task_id: Option<String>,
    value: Option<String>,
}

impl WorkAction {
    fn validate(
        &self,
        envelope: &ObjectRevisionEnvelope,
        this_device_id: &str,
    ) -> Result<(), CloudRuntimeError> {
        let short = |text: &Option<String>| {
            text.as_ref()
                .is_none_or(|text| text.len() <= MAX_TEXT_BYTES)
        };
        if self.format_version != 1
            || self.kind != "work_action"
            || self.device_id != envelope.writer_device_id
            || envelope.writer_device_id == this_device_id
            || envelope.chunk_count != 1
            || self.acted_at_utc_ms < 0
            || self.notice_ids.len() > MAX_NOTICE_IDS
            || !short(&self.body)
            || !short(&self.value)
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        Ok(())
    }
}

pub(super) fn open_action(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    ciphertext: &[u8],
) -> Result<Option<WorkAction>, CloudRuntimeError> {
    let context = ObjectRevisionCryptoContext {
        source_format: ACTION_SOURCE_FORMAT,
        ..*context
    };
    let Ok(bytes) = open_object_revision_payload(vault_root, &context, ciphertext) else {
        return Ok(None);
    };
    serde_json::from_slice(&Zeroizing::new(bytes))
        .map(Some)
        .map_err(|_| CloudRuntimeError::IntegrityFailure)
}

/// Whether a revision is a snapshot some Mac wrote. Nothing here reads one.
pub(super) fn is_snapshot(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    ciphertext: &[u8],
) -> bool {
    let context = ObjectRevisionCryptoContext {
        source_format: SOURCE_FORMAT,
        ..*context
    };
    open_object_revision_payload(vault_root, &context, ciphertext).is_ok()
}

/// The decisions this Mac applied, newest last, for the snapshot to report
/// until the phone has had time to see them.
#[derive(Default, Serialize, Deserialize)]
struct HandledState {
    vault_id: String,
    device_id: String,
    handled: Vec<Handled>,
}

fn read_handled(
    directory: &Path,
    vault_id: &str,
    device_id: &str,
    now_utc_ms: i64,
) -> Result<HandledState, CloudRuntimeError> {
    let mut state = match fs::read(directory.join(HANDLED_FILE)) {
        Ok(bytes) => serde_json::from_slice::<HandledState>(&bytes)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => HandledState::default(),
        Err(_) => return Err(CloudRuntimeError::File),
    };
    if state.vault_id != vault_id || state.device_id != device_id {
        state = HandledState {
            vault_id: vault_id.to_owned(),
            device_id: device_id.to_owned(),
            handled: Vec::new(),
        };
    }
    state
        .handled
        .retain(|entry| entry.at_utc_ms >= now_utc_ms.saturating_sub(HANDLED_LIFETIME_MS));
    Ok(state)
}

fn ensure_directory(directory: &Path) -> Result<(), CloudRuntimeError> {
    fs::create_dir_all(directory).map_err(|_| CloudRuntimeError::File)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| CloudRuntimeError::File)
}

/// Applies one decision as the user. The planner refuses one that no longer
/// fits (a task moved on, a question already answered), and that is an
/// outcome the phone is told, not an error to retry.
async fn apply(app: &AppHandle, object_id: &str, action: &WorkAction) -> Outcome {
    let operation_id = Uuid::new_v5(&ACTION_NAMESPACE, object_id.as_bytes()).to_string();
    let op = operation_id.as_str();
    let task = action.task_id.as_deref();
    let applied = match (action.action.as_str(), task) {
        ("done", Some(task)) => match action.expected_revision {
            Some(revision) => {
                let patch = WorkPatch {
                    status: Some(WorkStatus::Done),
                    ..WorkPatch::default()
                };
                crate::planning::work::update_as(app, WorkActor::Me, op, task, revision, &patch)
                    .await
                    .is_ok()
            }
            None => false,
        },
        ("accept", Some(task)) => crate::planning::work::triage_accept_as(
            app,
            WorkActor::Me,
            op,
            &[task.to_owned()],
            true,
        )
        .await
        .is_ok(),
        ("decline", Some(task)) => crate::planning::work::triage_decline_as(
            app,
            WorkActor::Me,
            op,
            &[task.to_owned()],
            None,
        )
        .await
        .is_ok(),
        ("read" | "archive", _) if !action.notice_ids.is_empty() => {
            let mark = if action.action == "read" {
                Mark::Read
            } else {
                Mark::Archive
            };
            inbox::mark_as(app, op, &action.notice_ids, mark).is_ok()
        }
        ("answer", _) => match action.session_id.as_deref() {
            Some(session) => sessions::reply_as(
                app,
                Author {
                    actor: WorkActor::Me,
                    agent: None,
                },
                op,
                session,
                action.body.as_deref(),
                action.value.as_deref(),
            )
            .is_ok(),
            None => false,
        },
        _ => false,
    };
    if applied {
        Outcome::Applied
    } else {
        Outcome::Refused
    }
}

impl CloudSyncRuntime {
    pub(super) async fn download_work_action(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        envelope: &ObjectRevisionEnvelope,
        action: WorkAction,
    ) -> Result<WorkAction, CloudRuntimeError> {
        action.validate(envelope, &access.state.device_id)?;
        download_empty_object(
            self,
            access,
            object_id,
            revision_id,
            envelope,
            ACTION_SOURCE_FORMAT,
        )
        .await?;
        Ok(action)
    }

    /// Apply a decision meant for this Mac, note how it went, and mark the
    /// revision seen. One meant for another Mac is only marked seen.
    pub(super) async fn apply_work_action(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
        action: WorkAction,
    ) -> Result<(), CloudRuntimeError> {
        if action.target_device_id == access.state.device_id {
            let directory = phone_sync_directory(&self.app).map_err(|_| CloudRuntimeError::File)?;
            ensure_directory(&directory)?;
            let now = utc_now_ms();
            let mut state = read_handled(
                &directory,
                &access.state.vault_id,
                &access.state.device_id,
                now,
            )?;
            if !state
                .handled
                .iter()
                .any(|entry| entry.action_id == object_id)
            {
                let outcome = apply(&self.app, object_id, &action).await;
                state.handled.push(Handled {
                    action_id: object_id.to_owned(),
                    outcome,
                    at_utc_ms: now,
                });
                let excess = state.handled.len().saturating_sub(MAX_HANDLED);
                state.handled.drain(..excess);
                write_staged_file(
                    &directory,
                    HANDLED_FILE,
                    &serde_json::to_vec(&state).map_err(|_| CloudRuntimeError::File)?,
                )?;
            }
        }
        super::acknowledge_remote_revision(&access.store, object_id, revision_id, sequence)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct TodayState {
    vault_id: String,
    device_id: String,
    completed_digest: Option<String>,
    completed_produced_at_utc_ms: Option<i64>,
    pending: Option<PendingToday>,
}

#[derive(Serialize, Deserialize)]
struct PendingToday {
    digest: String,
    produced_at_utc_ms: i64,
    #[serde(flatten)]
    revision: StagedRevision,
}

fn stage(
    access: &CloudAccess,
    plaintext: &[u8],
    digest: String,
    produced_at_utc_ms: i64,
) -> Result<PendingToday, CloudRuntimeError> {
    let object_id =
        sha256_base64url(format!("{SOURCE_FORMAT}:{}", access.state.device_id).as_bytes());
    let plaintext_chunks: Vec<&[u8]> = plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES).collect();
    let manifest_plaintext = serde_json::to_vec(&ObjectPayloadManifest {
        version: 1,
        kind: "work_today".to_owned(),
        source_format: SOURCE_FORMAT.to_owned(),
        chunk_count: u32::try_from(plaintext_chunks.len())
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        plaintext_bytes: u64::try_from(plaintext.len())
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        plaintext_sha256: digest.clone(),
    })
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    Ok(PendingToday {
        digest,
        produced_at_utc_ms,
        revision: revision_upload::stage(
            access,
            SOURCE_FORMAT,
            object_id,
            &manifest_plaintext,
            &plaintext_chunks,
        )?,
    })
}

fn persist(directory: &Path, state: &TodayState) -> Result<(), CloudRuntimeError> {
    write_staged_file(
        directory,
        CACHE_FILE,
        &serde_json::to_vec(state).map_err(|_| CloudRuntimeError::File)?,
    )
}

/// Publish the snapshot when what it says changed since the last one.
pub(super) async fn sync(
    runtime: &CloudSyncRuntime,
    access: &CloudAccess,
) -> Result<(), CloudRuntimeError> {
    let directory = phone_sync_directory(&runtime.app).map_err(|_| CloudRuntimeError::File)?;
    ensure_directory(&directory)?;
    let mut state = match fs::read(directory.join(CACHE_FILE)) {
        Ok(bytes) => serde_json::from_slice::<TodayState>(&bytes)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => TodayState::default(),
        Err(_) => return Err(CloudRuntimeError::File),
    };
    if state.vault_id != access.state.vault_id || state.device_id != access.state.device_id {
        state = TodayState {
            vault_id: access.state.vault_id.clone(),
            device_id: access.state.device_id.clone(),
            ..TodayState::default()
        };
    }
    if state.pending.is_none() {
        let history = runtime
            .app
            .try_state::<Arc<HistoryManager>>()
            .ok_or(CloudRuntimeError::SetupRequired)?;
        let now = utc_now_ms();
        let kept = KeptLocal::read(&access.store).ok();
        let rows = crate::planning::history(&history, |conn| today_rows(conn, kept.as_ref(), now))
            .map_err(|_| CloudRuntimeError::Storage)?;
        let handled = read_handled(
            &directory,
            &access.state.vault_id,
            &access.state.device_id,
            now,
        )?
        .handled;
        let mut today = WorkToday {
            format_version: 1,
            kind: "work_today",
            source_device_id: &access.state.device_id,
            produced_at_utc_ms: state.completed_produced_at_utc_ms.unwrap_or(0),
            rows,
            handled,
        };
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&today).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        );
        if state.completed_digest.as_deref() == Some(&sha256_base64url(&plaintext)) {
            return Ok(());
        }
        // Compared at the previous timestamp, so an unchanged day is not a new revision.
        today.produced_at_utc_ms = now;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&today).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        );
        let digest = sha256_base64url(&plaintext);
        state.pending = Some(stage(access, &plaintext, digest, now)?);
        persist(&directory, &state)?;
    }
    let pending = state
        .pending
        .as_mut()
        .ok_or(CloudRuntimeError::IntegrityFailure)?;
    match revision_upload::upload(runtime, access, SOURCE_FORMAT, &mut pending.revision).await {
        Ok(()) => {}
        Err(UploadRefused::Restage(error)) => {
            state.pending = None;
            persist(&directory, &state)?;
            return Err(error);
        }
        Err(UploadRefused::Retry(error)) => return Err(error),
    }
    state.completed_digest = Some(pending.digest.clone());
    state.completed_produced_at_utc_ms = Some(pending.produced_at_utc_ms);
    state.pending = None;
    persist(&directory, &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;
    use crate::planning::work::items::{insert, NewItem};
    use crate::planning::work::sessions::{
        ActivityInput, WorkSelectOption, WorkSessionActivityKind,
    };
    use crate::planning::work::types::WorkOwner;

    const AT: i64 = 1_800_000_000_000;
    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn task(
        conn: &Connection,
        title: &str,
        status: WorkStatus,
        owner: &WorkOwner,
        due_at_ms: Option<i64>,
        loop_id: Option<&str>,
    ) -> String {
        insert(
            conn,
            &NewItem {
                title,
                details: None,
                status,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner,
                due_at_ms,
                label_ids: &[],
                source_loop_id: loop_id,
                recurrence: None,
            },
            WorkActor::Me,
            AT,
        )
        .expect("a task")
        .id
    }

    fn ask(conn: &Connection, task_id: &str, question: &str) {
        let session =
            sessions::start(conn, task_id, "Claude Code", false, None, AT).expect("a session");
        sessions::emit(
            conn,
            &session,
            "Claude Code",
            WorkSessionActivityKind::Elicitation,
            &ActivityInput {
                body: Some(question.to_owned()),
                options: vec![WorkSelectOption {
                    label: "Acme".to_owned(),
                    value: "acme".to_owned(),
                }],
                ..ActivityInput::default()
            },
            None,
            AT,
        )
        .expect("a question");
    }

    fn titles(tasks: &[TodayTask]) -> Vec<&str> {
        tasks.iter().map(|task| task.title.as_str()).collect()
    }

    #[test]
    fn the_phone_gets_my_work_due_by_today_and_triage() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let other = WorkOwner::Person {
            person_id: "dana".to_owned(),
        };
        task(
            &conn,
            "Send the deck",
            WorkStatus::NotStarted,
            &WorkOwner::Myself,
            Some(AT - DAY),
            None,
        );
        task(
            &conn,
            "Book the room",
            WorkStatus::InProgress,
            &WorkOwner::Myself,
            Some(AT),
            None,
        );
        task(
            &conn,
            "Plan the offsite",
            WorkStatus::NotStarted,
            &WorkOwner::Myself,
            Some(AT + 2 * DAY),
            None,
        );
        task(
            &conn,
            "Dana's pricing",
            WorkStatus::NotStarted,
            &other,
            Some(AT),
            None,
        );
        task(
            &conn,
            "Already sent",
            WorkStatus::Done,
            &WorkOwner::Myself,
            Some(AT),
            None,
        );
        task(
            &conn,
            "Call the venue",
            WorkStatus::Triage,
            &WorkOwner::Myself,
            None,
            None,
        );

        let rows = today_rows(&conn, None, AT).expect("the rows");

        assert_eq!(titles(&rows.due), ["Send the deck", "Book the room"]);
        assert_eq!(titles(&rows.triage), ["Call the venue"]);
    }

    #[test]
    fn work_drawn_from_a_meeting_kept_on_the_mac_never_reaches_the_phone() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let me = WorkOwner::Myself;
        let open = task(
            &conn,
            "Book the room",
            WorkStatus::NotStarted,
            &me,
            Some(AT),
            None,
        );
        let kept = task(
            &conn,
            "Draft the layoffs memo",
            WorkStatus::NotStarted,
            &me,
            Some(AT),
            Some("memo-loop"),
        );
        task(
            &conn,
            "Tell the team",
            WorkStatus::Triage,
            &me,
            None,
            Some("team-loop"),
        );
        ask(&conn, &open, "Which vendor?");
        ask(&conn, &kept, "Who is on the list?");

        let rows = today_rows(&conn, None, AT).expect("the rows");

        assert_eq!(titles(&rows.due), ["Book the room"]);
        assert!(rows.triage.is_empty());
        assert_eq!(
            rows.questions
                .iter()
                .map(|question| question.question.as_str())
                .collect::<Vec<_>>(),
            ["Which vendor?"]
        );
        assert_eq!(rows.questions[0].options[0].value, "acme");
    }

    const ACTION_FIXTURE: &str =
        include_str!("../../../mobile/SonaTests/Fixtures/work-action-v1.json");
    const TODAY_FIXTURE: &str =
        include_str!("../../../mobile/SonaTests/Fixtures/work-today-v1.json");

    #[test]
    fn the_phone_and_the_mac_read_each_others_bytes() {
        let action: WorkAction = serde_json::from_str(ACTION_FIXTURE).expect("action fixture");
        assert_eq!(action.action, "answer");
        assert_eq!(action.value.as_deref(), Some("acme"));
        assert_eq!(
            serde_json::to_string(&action).expect("action"),
            ACTION_FIXTURE
        );

        let today = WorkToday {
            format_version: 1,
            kind: "work_today",
            source_device_id: "dev_mac_fixture",
            produced_at_utc_ms: 1_763_600_000_000,
            rows: TodayRows {
                due: vec![TodayTask {
                    id: "t1".to_owned(),
                    number: 12,
                    title: "Send the deck".to_owned(),
                    due_at_ms: Some(1_763_550_000_000),
                    project: Some("Launch".to_owned()),
                    revision: 3,
                }],
                triage: vec![TodayTask {
                    id: "t2".to_owned(),
                    number: 14,
                    title: "Call the venue".to_owned(),
                    due_at_ms: None,
                    project: None,
                    revision: 1,
                }],
                inbox: vec![TodayNotice {
                    id: "n1".to_owned(),
                    notice_ids: vec!["n1".to_owned(), "n0".to_owned()],
                    task_id: Some("t1".to_owned()),
                    task_number: Some(12),
                    title: "Send the deck".to_owned(),
                    line: "Claude Code commented: “Draft is in the folder.”".to_owned(),
                    needs_you: false,
                    at_ms: 1_763_590_000_000,
                }],
                questions: vec![TodayQuestion {
                    session_id: "s1".to_owned(),
                    task_id: "t3".to_owned(),
                    task_number: 15,
                    task_title: "Pick a vendor".to_owned(),
                    agent: "Claude Code".to_owned(),
                    question: "Which vendor?".to_owned(),
                    options: vec![TodayOption {
                        label: "Acme".to_owned(),
                        value: "acme".to_owned(),
                    }],
                    asked_at_ms: 1_763_580_000_000,
                }],
            },
            handled: vec![Handled {
                action_id: "a1".to_owned(),
                outcome: Outcome::Applied,
                at_utc_ms: 1_763_595_000_000,
            }],
        };
        assert_eq!(
            serde_json::to_string(&today).expect("snapshot"),
            TODAY_FIXTURE
        );
    }
}
