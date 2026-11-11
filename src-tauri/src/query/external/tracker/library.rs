//! The meeting library verbs: rename, file into folders, delete to the
//! trash, notes and action items, screen pictures, heard plans, searches and
//! exports, each through the same function the app's own screen calls.

use super::{
    conversations_closed, now_ms, refusal, Tracker, WorkAnswer, WriteArgs, READ_SCOPES,
    WRITE_SCOPES,
};
use crate::managers::history::HistoryManager;
use crate::meeting::folder_types::{
    MeetingFolder, MeetingFolderCreateRequest, MeetingFolderDeleteRequest,
    MeetingFolderMembershipRequest, MeetingFolderRenameRequest,
};
use crate::meeting::heard_plan_types::{HeardPlan, HeardPlanEdit};
use crate::meeting::session::{
    MeetingActionItemDoneRequest, MeetingMutationRequest, MeetingNoteCreateRequest,
    MeetingNoteDeleteRequest, MeetingNoteUpdateRequest, MeetingSessionManager,
    MeetingTitleSetRequest, MeetingUserNotesSaveRequest,
};
use crate::meeting::snapshots::{MeetingSnapshotId, MeetingSnapshotSummary, MeetingSnapshotTextMoment};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::{
    HeardPlanId, ManualNoteId, MeetingCommandError, MeetingCommandKind, MeetingDeletionJobId,
    MeetingExportFormat, MeetingExportRequest, MeetingOperationId, MeetingReviewSnapshot,
    MeetingSearchHit, MeetingSearchRequest, MeetingStatusFilter, MeetingTrashEntry,
    OperationReceipt, OperationResult, SaidMoment,
};
use crate::planning;
use crate::query::external::{
    command_error, current_artifacts, kept_local, meeting_refusal, missing_meeting, session_id,
    store, visible_meeting, ExternalError, ExternalErrorCode, ExternalScope,
};
use crate::query::{QueryError, QUERY_SCHEMA_VERSION};
use chrono::{Local, NaiveDateTime, TimeZone};
use clap::{Args, Subcommand, ValueEnum};
use rusqlite::OptionalExtension as _;
use serde::Serialize;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

const DEFAULT_HITS: usize = 10;
const MAX_HITS: usize = 50;
const NS_PER_SECOND: u64 = 1_000_000_000;
const SNAPSHOT_DELETE: &str = "cli_snapshot_delete";
const MEETING_RESTORE: &str = "cli_meeting_restore";
const MEETING_DELETE_FOREVER: &str = "cli_meeting_delete_forever";

pub(super) type Gate<'a> = &'a (dyn Fn(&str) -> Result<(), ExternalError> + Sync);

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum MeetingStatus {
    /// Notes are ready to read.
    Ready,
    /// Recording stopped and the notes are still being written.
    Processing,
    /// The notes could not be written, or the recording needs recovery.
    Failed,
}

impl From<MeetingStatus> for MeetingStatusFilter {
    fn from(status: MeetingStatus) -> Self {
        match status {
            MeetingStatus::Ready => Self::Ready,
            MeetingStatus::Processing => Self::Processing,
            MeetingStatus::Failed => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ExportFormat {
    Markdown,
    Json,
}

impl From<ExportFormat> for MeetingExportFormat {
    fn from(format: ExportFormat) -> Self {
        match format {
            ExportFormat::Markdown => Self::Markdown,
            ExportFormat::Json => Self::Json,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct Change {
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MeetingRef {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct RenameArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The new title.
    #[arg(value_name = "TITLE")]
    pub title: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MeetingChangeArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FolderNewArgs {
    #[arg(value_name = "NAME")]
    pub name: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FolderRenameArgs {
    /// The folder, by name or id, from `folders`.
    #[arg(value_name = "FOLDER")]
    pub folder: String,
    #[arg(value_name = "NAME")]
    pub name: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FolderArgs {
    /// The folder, by name or id, from `folders`.
    #[arg(value_name = "FOLDER")]
    pub folder: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FolderMembershipArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The folder, by name or id, from `folders`.
    #[arg(long, value_name = "FOLDER")]
    pub folder: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NoteAddArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    #[arg(value_name = "TEXT")]
    pub body: String,
    /// Pin the note to this many seconds into the recording.
    #[arg(long, value_name = "SECONDS")]
    pub at: Option<u64>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NoteEditArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The note's id, from `meeting-notes`.
    #[arg(value_name = "NOTE")]
    pub note: Uuid,
    #[arg(value_name = "TEXT")]
    pub body: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NoteArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The note's id, from `meeting-notes`.
    #[arg(value_name = "NOTE")]
    pub note: Uuid,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MyNotesArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// Replace my notes on the meeting with this text.
    #[arg(value_name = "TEXT")]
    pub body: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ActionDoneArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The action item's number, from `meeting-notes`.
    #[arg(value_name = "NUMBER")]
    pub number: u32,
    /// Mark it not done again.
    #[arg(long)]
    pub undo: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SnapshotArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The picture's id, from `snapshots`.
    #[arg(value_name = "SNAPSHOT")]
    pub snapshot: Uuid,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TrashArgs {
    /// The trash entry's id, from `meeting-trash`.
    #[arg(value_name = "ENTRY")]
    pub entry: Uuid,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct HeardPlanArgs {
    /// The plan's id, from `heard-plans`.
    #[arg(value_name = "PLAN")]
    pub plan: Uuid,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct HeardPlanEditArgs {
    /// The plan's id, from `heard-plans`.
    #[arg(value_name = "PLAN")]
    pub plan: Uuid,
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// When it starts: YYYY-MM-DD HH:MM local time, RFC 3339, or epoch milliseconds.
    #[arg(long, value_parser = when, value_name = "WHEN")]
    pub start: Option<i64>,
    /// When it ends, the same ways as --start.
    #[arg(long, value_parser = when, value_name = "WHEN")]
    pub end: Option<i64>,
    /// Everyone in it, comma-separated; "" clears them.
    #[arg(long, value_delimiter = ',', value_name = "NAMES")]
    pub people: Option<Vec<String>>,
    /// Where it is; "" clears it.
    #[arg(long, value_name = "PLACE")]
    pub place: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SearchArgs {
    #[arg(value_name = "QUERY")]
    pub query: String,
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MeetingSearchArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    #[arg(value_name = "QUERY")]
    pub query: String,
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ExportArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    #[arg(long, value_enum, default_value = "markdown")]
    pub format: ExportFormat,
    /// The file to write. The meeting's screen pictures go beside it.
    #[arg(long, value_name = "FILE")]
    pub out: PathBuf,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FileArgs {
    /// Write to this file instead of printing.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LedgerArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    #[command(flatten)]
    pub file: FileArgs,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum LibraryVerb {
    /// Rename a meeting, as its title field does.
    MeetingRename(RenameArgs),
    /// Move a meeting to the trash, where Sona keeps it for 30 days.
    MeetingDelete(MeetingChangeArgs),
    /// The meetings in the trash, kept 30 days.
    MeetingTrash,
    /// Put a meeting back from the trash.
    MeetingRestore(TrashArgs),
    /// Delete a meeting in the trash for good.
    MeetingDeleteForever(TrashArgs),
    /// List the meeting folders.
    Folders,
    /// Make a folder.
    FolderNew(FolderNewArgs),
    /// Rename a folder.
    FolderRename(FolderRenameArgs),
    /// Delete a folder. Its meetings stay, unfiled.
    FolderDelete(FolderArgs),
    /// File a meeting in a folder.
    FolderAdd(FolderMembershipArgs),
    /// Take a meeting out of a folder.
    FolderRemove(FolderMembershipArgs),
    /// A meeting's notes: the pinned notes, my notes, and the action items.
    MeetingNotes(MeetingRef),
    /// Add a note to a meeting.
    MeetingNoteAdd(NoteAddArgs),
    /// Change a meeting note's text.
    MeetingNoteEdit(NoteEditArgs),
    /// Delete a meeting note.
    MeetingNoteDelete(NoteArgs),
    /// Replace my notes on a meeting.
    MyNotes(MyNotesArgs),
    /// Tick an action item off, or back on with --undo.
    ActionDone(ActionDoneArgs),
    /// The screen pictures kept from a meeting.
    Snapshots(MeetingRef),
    /// Delete one screen picture.
    SnapshotDelete(SnapshotArgs),
    /// The plans to meet heard in a meeting.
    HeardPlans(MeetingRef),
    /// Correct a heard plan's title, time, people or place.
    HeardPlanEdit(HeardPlanEditArgs),
    /// Answer "Not now" to a heard plan: it stays and stops asking.
    HeardPlanNotNow(HeardPlanArgs),
    /// Lines anyone said, across meetings.
    Said(SearchArgs),
    /// Words seen on screen, across meetings.
    ScreenText(SearchArgs),
    /// Search inside one meeting.
    MeetingSearch(MeetingSearchArgs),
    /// Export one meeting as Markdown or JSON.
    MeetingExport(ExportArgs),
    /// A meeting's ledger as one HTML page.
    MeetingLedger(LedgerArgs),
    /// Every meeting as one CSV.
    MeetingsCsv(FileArgs),
}

impl LibraryVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::Folders
            | Self::MeetingTrash
            | Self::MeetingNotes(_)
            | Self::Snapshots(_)
            | Self::HeardPlans(_)
            | Self::Said(_)
            | Self::ScreenText(_)
            | Self::MeetingSearch(_)
            | Self::MeetingLedger(_)
            | Self::MeetingsCsv(_) => READ_SCOPES,
            Self::MeetingRename(RenameArgs { change, .. })
            | Self::MeetingDelete(MeetingChangeArgs { change, .. })
            | Self::MeetingRestore(TrashArgs { change, .. })
            | Self::MeetingDeleteForever(TrashArgs { change, .. })
            | Self::FolderNew(FolderNewArgs { change, .. })
            | Self::FolderRename(FolderRenameArgs { change, .. })
            | Self::FolderDelete(FolderArgs { change, .. })
            | Self::FolderAdd(FolderMembershipArgs { change, .. })
            | Self::FolderRemove(FolderMembershipArgs { change, .. })
            | Self::MeetingNoteAdd(NoteAddArgs { change, .. })
            | Self::MeetingNoteEdit(NoteEditArgs { change, .. })
            | Self::MeetingNoteDelete(NoteArgs { change, .. })
            | Self::MyNotes(MyNotesArgs { change, .. })
            | Self::ActionDone(ActionDoneArgs { change, .. })
            | Self::SnapshotDelete(SnapshotArgs { change, .. })
            | Self::HeardPlanEdit(HeardPlanEditArgs { change, .. })
            | Self::HeardPlanNotNow(HeardPlanArgs { change, .. })
            | Self::MeetingExport(ExportArgs { change, .. }) => {
                if change.dry_run {
                    READ_SCOPES
                } else {
                    WRITE_SCOPES
                }
            }
        }
    }

    pub(super) fn check(&self) -> Result<(), ExternalError> {
        let refusal = match self {
            Self::MeetingRename(args) if args.title.trim().is_empty() => "A meeting needs a title.",
            Self::FolderNew(FolderNewArgs { name, .. })
            | Self::FolderRename(FolderRenameArgs { name, .. })
                if name.trim().is_empty() =>
            {
                "A folder needs a name."
            }
            Self::MeetingNoteAdd(NoteAddArgs { body, .. })
            | Self::MeetingNoteEdit(NoteEditArgs { body, .. })
                if body.trim().is_empty() =>
            {
                "A note needs some text."
            }
            Self::ActionDone(args) if args.number == 0 => "Action items count from 1.",
            Self::Said(SearchArgs { query, .. })
            | Self::ScreenText(SearchArgs { query, .. })
            | Self::MeetingSearch(MeetingSearchArgs { query, .. })
                if query.trim().chars().count() < 2 =>
            {
                "Search for at least two characters."
            }
            _ => return Ok(()),
        };
        Err(ExternalError::invalid(refusal))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFolder {
    pub id: Uuid,
    pub name: String,
    pub meeting_count: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFolders {
    pub schema_version: u32,
    pub folders: Vec<ExternalFolder>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMeetingNote {
    pub id: Uuid,
    pub at_seconds: Option<u64>,
    pub body: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalActionItem {
    pub number: u32,
    pub text: String,
    pub owner: Option<String>,
    pub due: Option<String>,
    pub done: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMeetingNotes {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub title: String,
    pub notes: Vec<ExternalMeetingNote>,
    pub my_notes: String,
    pub action_items: Vec<ExternalActionItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSnapshots {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub snapshots: Vec<MeetingSnapshotSummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalHeardPlans {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub plans: Vec<HeardPlan>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSaid {
    pub schema_version: u32,
    pub query: String,
    pub moments: Vec<SaidMoment>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalScreenText {
    pub schema_version: u32,
    pub query: String,
    pub moments: Vec<MeetingSnapshotTextMoment>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMeetingSearch {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub query: String,
    pub hits: Vec<MeetingSearchHit>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTrashEntry {
    pub id: Uuid,
    pub title: String,
    pub deleted_at_ms: i64,
    /// None for a meeting a retention setting removed: it stays until
    /// someone restores or deletes it.
    pub expires_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTrash {
    pub schema_version: u32,
    pub entries: Vec<ExternalTrashEntry>,
}

/// A page or a CSV: printed when no file was named, else where it went.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalDocument {
    pub schema_version: u32,
    pub meeting_id: Option<Uuid>,
    pub path: Option<String>,
    pub contents: Option<String>,
}

/// One change to the library. The ids name what it touched.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalLibraryChange {
    pub schema_version: u32,
    pub summary: String,
    pub dry_run: bool,
    /// True when an earlier run with the same operation id made it, or it
    /// already reads that way.
    pub replayed: bool,
    pub operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meeting_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<Uuid>,
    /// The trash entry a delete made or a restore emptied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trash_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalLibraryAnswer {
    Folders(ExternalFolders),
    Notes(ExternalMeetingNotes),
    Snapshots(ExternalSnapshots),
    HeardPlans(ExternalHeardPlans),
    Said(ExternalSaid),
    ScreenText(ExternalScreenText),
    Search(ExternalMeetingSearch),
    Document(ExternalDocument),
    Change(ExternalLibraryChange),
    Trash(ExternalTrash),
}

fn when(value: &str) -> Result<i64, String> {
    let value = value.trim();
    if let Ok(ms) = value.parse::<i64>() {
        return Ok(ms);
    }
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(at.timestamp_millis());
    }
    ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
        .and_then(|naive| Local.from_local_datetime(&naive).single())
        .map(|at| at.timestamp_millis())
        .ok_or_else(|| format!("{value:?} is not a time: use YYYY-MM-DD HH:MM, RFC 3339, or epoch milliseconds"))
}

pub(super) fn told(dry_run: bool, done: &str, would: &str) -> String {
    (if dry_run { would } else { done }).to_owned()
}

fn change(summary: String, dry_run: bool, operation_id: String) -> ExternalLibraryChange {
    ExternalLibraryChange {
        schema_version: QUERY_SCHEMA_VERSION,
        summary,
        dry_run,
        replayed: false,
        operation_id,
        meeting_id: None,
        folder_id: None,
        note_id: None,
        snapshot_id: None,
        plan_id: None,
        trash_id: None,
        path: None,
    }
}

pub(super) fn operation(
    write: &WriteArgs,
    verb: &str,
    key: &impl Serialize,
) -> Result<(String, MeetingOperationId), ExternalError> {
    let id = write.operation_id(verb, key)?;
    let uuid = Uuid::parse_str(&id).map_err(|_| {
        ExternalError::new(ExternalErrorCode::Failed, "The change could not be named.")
    })?;
    Ok((id, MeetingOperationId::from_uuid(uuid)))
}

async fn library(meetings: &Arc<MeetingSessionManager>) -> Result<Arc<MeetingStore>, ExternalError> {
    store(meetings).await.map_err(|_| conversations_closed())
}

/// The meeting, refused as absent when it is kept on this Mac.
async fn meeting(
    meetings: &Arc<MeetingSessionManager>,
    value: &str,
) -> Result<(Arc<MeetingStore>, MeetingReviewSnapshot), ExternalError> {
    let session = session_id(value)?;
    let store = library(meetings).await?;
    visible_meeting(&store, session)?;
    let review = store
        .review_snapshot(session)
        .map_err(|error| meeting_refusal(session, error))?;
    Ok((store, review))
}

fn folder_error(folder: Uuid) -> impl Fn(MeetingCommandError) -> ExternalError {
    move |error| match error {
        MeetingCommandError::NotFound => ExternalError::new(
            ExternalErrorCode::NotFound,
            format!("No folder {folder}."),
        )
        .hint("List them with: sona work folders"),
        error => QueryError::from(error).into(),
    }
}

/// A folder by its id or its name, ignoring case.
pub(super) async fn folder(
    meetings: &Arc<MeetingSessionManager>,
    value: &str,
) -> Result<MeetingFolder, ExternalError> {
    let value = value.trim();
    let by_id = Uuid::parse_str(value).ok();
    library(meetings).await?;
    meetings
        .folder_list()
        .await
        .map_err(|error| ExternalError::from(QueryError::from(error)))?
        .folders
        .into_iter()
        .find(|folder| match by_id {
            Some(id) => folder.folder_id.uuid() == id,
            None => folder.name.trim().eq_ignore_ascii_case(value),
        })
        .ok_or_else(|| {
            ExternalError::new(ExternalErrorCode::NotFound, format!("No folder {value:?}."))
                .hint("List them with: sona work folders")
        })
}

async fn folder_revision(meetings: &Arc<MeetingSessionManager>) -> Result<u64, ExternalError> {
    Ok(meetings
        .folder_list()
        .await
        .map_err(|error| ExternalError::from(QueryError::from(error)))?
        .revision)
}

fn reused_operation(operation_id: &str) -> ExternalError {
    ExternalError::invalid(format!(
        "Operation id {operation_id} was already used for a different change. Pass a new --operation-id, or leave it out."
    ))
}

/// Whether this operation id already made this change, for the changes
/// whose own store keeps no receipt.
pub(super) fn made_before(
    history: &HistoryManager,
    operation_id: &str,
    command: &str,
    request: &str,
) -> Result<bool, ExternalError> {
    let found: Option<(String, String)> = planning::history(history, |conn| {
        Ok(conn
            .query_row(
                "SELECT command, request_json FROM planning_operations WHERE operation_id = ?1",
                [operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    })
    .map_err(|message| refusal(history, message))?;
    match found {
        None => Ok(false),
        Some((made, asked)) if made == command && asked == request => Ok(true),
        Some(_) => Err(reused_operation(operation_id)),
    }
}

pub(super) fn file_made(
    history: &HistoryManager,
    operation_id: &str,
    command: &str,
    request: &str,
) -> Result<(), ExternalError> {
    planning::history(history, |conn| {
        conn.execute(
            "INSERT INTO planning_operations (operation_id, command, request_json, response_json, created_at_ms) VALUES (?1, ?2, ?3, '', ?4)",
            rusqlite::params![operation_id, command, request, now_ms()],
        )?;
        Ok(())
    })
    .map_err(|message| refusal(history, message))
}

fn trashed_by(receipt: &OperationReceipt) -> Option<MeetingDeletionJobId> {
    receipt
        .effect_ids
        .first()
        .and_then(|value| Uuid::parse_str(value).ok())
        .map(MeetingDeletionJobId::from_uuid)
}

/// The earlier delete this operation id made, answered without reading the
/// meeting it took away.
async fn delete_replay(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingChangeArgs,
) -> Result<Option<ExternalLibraryChange>, ExternalError> {
    let Some(given) = args.change.write.operation_id else {
        return Ok(None);
    };
    let session = session_id(&args.meeting)?;
    let store = library(meetings).await?;
    let Some(receipt) = store
        .operation_receipt(MeetingOperationId::from_uuid(given))
        .map_err(|error| meeting_refusal(session, error))?
    else {
        return Ok(None);
    };
    if receipt.command != MeetingCommandKind::Delete || receipt.session_id != Some(session) {
        return Err(reused_operation(&given.to_string()));
    }
    if receipt.result != OperationResult::Committed {
        return Ok(None);
    }
    let hidden = store
        .kept_local_trash()
        .map_err(|error| ExternalError::from(QueryError::from(error)))?;
    let job = trashed_by(&receipt);
    if job.is_none_or(|job| hidden.contains(&job)) {
        return Err(missing_meeting(session));
    }
    visible_meeting(&store, session)?;
    let mut answer = change(
        "Moved the meeting to the trash".to_owned(),
        args.change.dry_run,
        given.to_string(),
    );
    answer.replayed = true;
    answer.meeting_id = Some(session.uuid());
    answer.trash_id = job.map(|job| job.uuid());
    Ok(Some(answer))
}

fn missing_trash_entry(entry: Uuid) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("No meeting {entry} in the trash."),
    )
    .hint("List them with: sona work meeting-trash")
}

/// The trash an agent may see: none of it kept on this Mac.
async fn visible_trash(
    meetings: &Arc<MeetingSessionManager>,
) -> Result<Vec<MeetingTrashEntry>, ExternalError> {
    let store = library(meetings).await?;
    let hidden = store
        .kept_local_trash()
        .map_err(|error| ExternalError::from(QueryError::from(error)))?;
    Ok(meetings
        .trash_list()
        .await
        .map_err(|error| ExternalError::from(QueryError::from(error)))?
        .into_iter()
        .filter(|entry| !hidden.contains(&entry.job_id))
        .collect())
}

pub(super) async fn trash(
    meetings: &Arc<MeetingSessionManager>,
) -> Result<ExternalTrash, ExternalError> {
    Ok(ExternalTrash {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: visible_trash(meetings)
            .await?
            .into_iter()
            .map(|entry| ExternalTrashEntry {
                id: entry.job_id.uuid(),
                title: entry.title,
                deleted_at_ms: entry.deleted_at_utc_ms,
                expires_at_ms: entry.expires_at_utc_ms,
            })
            .collect(),
    })
}

fn trash_answer(
    args: &TrashArgs,
    restore: bool,
) -> Result<(ExternalLibraryChange, &'static str), ExternalError> {
    let (verb, command) = if restore {
        ("meeting-restore", MEETING_RESTORE)
    } else {
        ("meeting-delete-forever", MEETING_DELETE_FOREVER)
    };
    let id = args.change.write.operation_id(verb, &args.entry)?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        if restore {
            told(
                dry_run,
                "Put the meeting back from the trash",
                "Would put the meeting back from the trash",
            )
        } else {
            told(
                dry_run,
                "Deleted the meeting for good",
                "Would delete the meeting for good",
            )
        },
        dry_run,
        id,
    );
    answer.trash_id = Some(args.entry);
    Ok((answer, command))
}

/// Restore a trashed meeting, or with `restore` false delete it for good.
pub(super) async fn trash_change(
    meetings: &Arc<MeetingSessionManager>,
    args: &TrashArgs,
    restore: bool,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (mut answer, _) = trash_answer(args, restore)?;
    let entry = visible_trash(meetings)
        .await?
        .into_iter()
        .find(|entry| entry.job_id.uuid() == args.entry)
        .ok_or_else(|| missing_trash_entry(args.entry))?;
    if answer.dry_run {
        return Ok(answer);
    }
    let refused = |error: MeetingCommandError| match error {
        MeetingCommandError::NotFound => missing_trash_entry(args.entry),
        error => ExternalError::from(QueryError::from(error)),
    };
    if restore {
        gate("restoring a meeting")?;
        let restored = meetings.trash_restore(entry.job_id).await.map_err(refused)?;
        answer.meeting_id = Some(restored.session_id.uuid());
    } else {
        gate("deleting a meeting for good")?;
        meetings
            .trash_delete_forever(entry.job_id)
            .await
            .map_err(refused)?;
    }
    Ok(answer)
}

/// A trash change, answered from the earlier run when this operation id
/// already made it: the entry is gone from the trash by then.
async fn trash_once(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    args: &TrashArgs,
    restore: bool,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (mut answer, command) = trash_answer(args, restore)?;
    let request = args.entry.to_string();
    if made_before(history, &answer.operation_id, command, &request)? {
        answer.replayed = true;
        return Ok(answer);
    }
    let made = trash_change(meetings, args, restore, gate).await?;
    if !made.dry_run {
        file_made(history, &made.operation_id, command, &request)?;
    }
    Ok(made)
}

pub(super) fn receipt_replayed(
    receipt: &OperationReceipt,
    expected: u64,
) -> Result<bool, ExternalError> {
    if receipt.result != OperationResult::Committed {
        return Err(ExternalError::new(
            ExternalErrorCode::Failed,
            format!(
                "Sona did not make the change ({:?}). Read the meeting again and retry.",
                receipt.reason_codes
            ),
        ));
    }
    Ok(receipt.expected_revision != expected)
}

fn plan_error(plan: Uuid) -> impl Fn(StoreError) -> ExternalError {
    move |error| match error {
        StoreError::NotFound => {
            ExternalError::new(ExternalErrorCode::NotFound, format!("No heard plan {plan}."))
        }
        StoreError::Conflict => ExternalError::invalid(
            "That plan is on the calendar. Take it off the calendar in Sona before changing it.",
        ),
        StoreError::Invalid => ExternalError::invalid(
            "That plan needs a title, a start before its end, and short names and place.",
        ),
        error => QueryError::from(error).into(),
    }
}

/// The plan, refused as absent when its meeting is kept on this Mac.
async fn heard_plan(
    meetings: &Arc<MeetingSessionManager>,
    plan: Uuid,
) -> Result<(Arc<MeetingStore>, HeardPlan), ExternalError> {
    let store = library(meetings).await?;
    let found = store
        .heard_plan(HeardPlanId::from_uuid(plan))
        .map_err(plan_error(plan))?;
    visible_meeting(&store, found.session_id).map_err(|_| plan_error(plan)(StoreError::NotFound))?;
    Ok((store, found))
}

fn hits(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_HITS).clamp(1, MAX_HITS)
}

pub(super) async fn rename(
    meetings: &Arc<MeetingSessionManager>,
    args: &RenameArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let title = args.title.trim().to_owned();
    let (id, operation_id) = operation(
        &args.change.write,
        "meeting-rename",
        &(session.uuid(), revision, &title),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Renamed the meeting", "Would rename the meeting"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    answer.replayed = review.session.title == title;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("renaming a meeting")?;
    let result = meetings
        .title_set(MeetingTitleSetRequest {
            operation_id,
            session_id: session,
            expected_revision: revision,
            title,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

pub(super) async fn delete(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingChangeArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    if let Some(replayed) = delete_replay(meetings, args).await? {
        return Ok(replayed);
    }
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let (id, operation_id) = operation(
        &args.change.write,
        "meeting-delete",
        &(session.uuid(), revision),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(
            dry_run,
            "Moved the meeting to the trash",
            "Would move the meeting to the trash",
        ),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    if dry_run {
        return Ok(answer);
    }
    gate("deleting a meeting")?;
    let result = meetings
        .delete(MeetingMutationRequest {
            operation_id,
            session_id: session,
            expected_revision: revision,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    answer.trash_id = trashed_by(&result.receipt).map(|job| job.uuid());
    Ok(answer)
}

pub(super) async fn folder_new(
    meetings: &Arc<MeetingSessionManager>,
    args: &FolderNewArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    library(meetings).await?;
    let name = args.name.trim().to_owned();
    let revision = folder_revision(meetings).await?;
    let (id, operation_id) = operation(&args.change.write, "folder-new", &(&name, revision))?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Made the folder", "Would make the folder"),
        dry_run,
        id,
    );
    if let Ok(existing) = folder(meetings, &name).await {
        answer.folder_id = Some(existing.folder_id.uuid());
        answer.replayed = true;
        return Ok(answer);
    }
    if dry_run {
        return Ok(answer);
    }
    gate("making a folder")?;
    let result = meetings
        .folder_create(MeetingFolderCreateRequest {
            operation_id,
            name: name.clone(),
            expected_revision: revision,
        })
        .await
        .map_err(|error| ExternalError::from(QueryError::from(error)))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    answer.folder_id = result
        .folders
        .folders
        .iter()
        .find(|folder| folder.name.trim().eq_ignore_ascii_case(&name))
        .map(|folder| folder.folder_id.uuid());
    Ok(answer)
}

pub(super) async fn folder_rename(
    meetings: &Arc<MeetingSessionManager>,
    args: &FolderRenameArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let found = folder(meetings, &args.folder).await?;
    let name = args.name.trim().to_owned();
    let revision = folder_revision(meetings).await?;
    let folder_id = found.folder_id.uuid();
    let (id, operation_id) = operation(
        &args.change.write,
        "folder-rename",
        &(folder_id, &name, revision),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Renamed the folder", "Would rename the folder"),
        dry_run,
        id,
    );
    answer.folder_id = Some(folder_id);
    answer.replayed = found.name == name;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("renaming a folder")?;
    let result = meetings
        .folder_rename(MeetingFolderRenameRequest {
            operation_id,
            folder_id: found.folder_id,
            name,
            expected_revision: revision,
        })
        .await
        .map_err(folder_error(folder_id))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

pub(super) async fn folder_delete(
    meetings: &Arc<MeetingSessionManager>,
    args: &FolderArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let found = folder(meetings, &args.folder).await?;
    let revision = folder_revision(meetings).await?;
    let folder_id = found.folder_id.uuid();
    let (id, operation_id) =
        operation(&args.change.write, "folder-delete", &(folder_id, revision))?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(
            dry_run,
            "Deleted the folder; its meetings stay",
            "Would delete the folder; its meetings would stay",
        ),
        dry_run,
        id,
    );
    answer.folder_id = Some(folder_id);
    if dry_run {
        return Ok(answer);
    }
    gate("deleting a folder")?;
    let result = meetings
        .folder_delete(MeetingFolderDeleteRequest {
            operation_id,
            folder_id: found.folder_id,
            expected_revision: revision,
        })
        .await
        .map_err(folder_error(folder_id))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

pub(super) async fn folder_membership(
    meetings: &Arc<MeetingSessionManager>,
    args: &FolderMembershipArgs,
    file: bool,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let found = folder(meetings, &args.folder).await?;
    let folder_id = found.folder_id.uuid();
    let revision = folder_revision(meetings).await?;
    let filed = meetings
        .folders_for_session(session)
        .await
        .map_err(command_error(session.uuid()))?
        .contains(&found.folder_id);
    let verb = if file { "folder-add" } else { "folder-remove" };
    let (id, operation_id) = operation(
        &args.change.write,
        verb,
        &(session.uuid(), folder_id, revision),
    )?;
    let dry_run = args.change.dry_run;
    let summary = match (file, dry_run) {
        (true, false) => "Filed the meeting in the folder",
        (true, true) => "Would file the meeting in the folder",
        (false, false) => "Took the meeting out of the folder",
        (false, true) => "Would take the meeting out of the folder",
    };
    let mut answer = change(summary.to_owned(), dry_run, id);
    answer.meeting_id = Some(session.uuid());
    answer.folder_id = Some(folder_id);
    answer.replayed = filed == file;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("filing a meeting")?;
    let request = MeetingFolderMembershipRequest {
        operation_id,
        folder_id: found.folder_id,
        session_id: session,
        expected_revision: revision,
    };
    let result = if file {
        meetings.folder_add_meeting(request).await
    } else {
        meetings.folder_remove_meeting(request).await
    }
    .map_err(folder_error(folder_id))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

pub(super) async fn notes(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingRef,
) -> Result<ExternalMeetingNotes, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let mine = meetings
        .user_notes_get(session)
        .await
        .map_err(command_error(session.uuid()))?;
    let done = store
        .action_item_states(session)
        .map_err(QueryError::from)?;
    let current = review
        .artifacts
        .iter()
        .find(|revision| revision.state == crate::meeting::types::MeetingArtifactState::Current)
        .map(|revision| revision.artifact_id);
    let action_items = current_artifacts(&review)
        .map(|content| {
            content
                .action_items
                .iter()
                .zip(0u32..)
                .map(|(item, index)| ExternalActionItem {
                    number: index + 1,
                    text: item.text.text.clone(),
                    owner: item.owner_text.clone(),
                    due: item.due_text.clone(),
                    done: done.iter().any(|state| {
                        Some(state.artifact_id) == current
                            && state.action_index == index
                            && state.done
                    }),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ExternalMeetingNotes {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid(),
        title: review.session.title.clone(),
        notes: review
            .notes
            .iter()
            .map(|note| ExternalMeetingNote {
                id: note.note_id.uuid(),
                at_seconds: note.start_offset_ns.map(|ns| ns / NS_PER_SECOND),
                body: note.body.clone(),
            })
            .collect(),
        my_notes: mine.body,
        action_items,
    })
}

pub(super) async fn note_add(
    meetings: &Arc<MeetingSessionManager>,
    args: &NoteAddArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let body = args.body.trim().to_owned();
    let (id, operation_id) = operation(
        &args.change.write,
        "meeting-note-add",
        &(session.uuid(), revision, &body, args.at),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Added the note", "Would add the note"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    if dry_run {
        return Ok(answer);
    }
    gate("adding a meeting note")?;
    let offset = args.at.map(|seconds| seconds.saturating_mul(NS_PER_SECOND));
    let result = meetings
        .note_create(MeetingNoteCreateRequest {
            operation_id,
            session_id: session,
            expected_revision: revision,
            start_offset_ns: offset,
            end_offset_ns: offset,
            body,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

fn missing_note(note: Uuid) -> ExternalError {
    ExternalError::new(ExternalErrorCode::NotFound, format!("No note {note} in this meeting."))
        .hint("List them with: sona work meeting-notes MEETING")
}

pub(super) async fn note_edit(
    meetings: &Arc<MeetingSessionManager>,
    args: &NoteEditArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let note = review
        .notes
        .iter()
        .find(|note| note.note_id.uuid() == args.note)
        .ok_or_else(|| missing_note(args.note))?;
    let body = args.body.trim().to_owned();
    let (id, operation_id) = operation(
        &args.change.write,
        "meeting-note-edit",
        &(session.uuid(), args.note, note.revision, &body),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Changed the note", "Would change the note"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    answer.note_id = Some(args.note);
    answer.replayed = note.body == body;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("changing a meeting note")?;
    let result = meetings
        .note_update(MeetingNoteUpdateRequest {
            operation_id,
            session_id: session,
            expected_revision: revision,
            note_id: note.note_id,
            expected_note_revision: note.revision,
            start_offset_ns: note.start_offset_ns,
            end_offset_ns: note.end_offset_ns,
            body,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

pub(super) async fn note_delete(
    meetings: &Arc<MeetingSessionManager>,
    args: &NoteArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let note = review
        .notes
        .iter()
        .find(|note| note.note_id.uuid() == args.note)
        .ok_or_else(|| missing_note(args.note))?;
    let (id, operation_id) = operation(
        &args.change.write,
        "meeting-note-delete",
        &(session.uuid(), args.note, note.revision),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Deleted the note", "Would delete the note"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    answer.note_id = Some(args.note);
    if dry_run {
        return Ok(answer);
    }
    gate("deleting a meeting note")?;
    let result = meetings
        .note_delete(MeetingNoteDeleteRequest {
            operation_id,
            session_id: session,
            expected_revision: revision,
            note_id: ManualNoteId::from_uuid(args.note),
            expected_note_revision: note.revision,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

pub(super) async fn my_notes(
    meetings: &Arc<MeetingSessionManager>,
    args: &MyNotesArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let mine = meetings
        .user_notes_get(session)
        .await
        .map_err(command_error(session.uuid()))?;
    let (id, _) = operation(
        &args.change.write,
        "my-notes",
        &(session.uuid(), mine.revision, &args.body),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Saved my notes", "Would save my notes"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    answer.replayed = mine.body == args.body;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("changing my notes on a meeting")?;
    meetings
        .user_notes_save(MeetingUserNotesSaveRequest {
            session_id: session,
            body: args.body.clone(),
            template: mine.template,
            custom_template_id: mine.custom_template_id,
            expected_note_revision: mine.revision,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    Ok(answer)
}

pub(super) async fn action_done(
    meetings: &Arc<MeetingSessionManager>,
    args: &ActionDoneArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let listed = notes(
        meetings,
        &MeetingRef {
            meeting: args.meeting.clone(),
        },
    )
    .await?;
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let item = listed
        .action_items
        .iter()
        .find(|item| item.number == args.number)
        .ok_or_else(|| {
            ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("This meeting has no action item {}.", args.number),
            )
            .hint("List them with: sona work meeting-notes MEETING")
        })?;
    let artifact_id = review
        .artifacts
        .iter()
        .find(|revision| revision.state == crate::meeting::types::MeetingArtifactState::Current)
        .map(|revision| revision.artifact_id)
        .ok_or_else(|| ExternalError::invalid("This meeting has no notes yet."))?;
    let done = !args.undo;
    let (id, _) = operation(
        &args.change.write,
        "action-done",
        &(session.uuid(), artifact_id.uuid(), args.number, done),
    )?;
    let dry_run = args.change.dry_run;
    let summary = match (done, dry_run) {
        (true, false) => "Marked the action item done",
        (true, true) => "Would mark the action item done",
        (false, false) => "Marked the action item not done",
        (false, true) => "Would mark the action item not done",
    };
    let mut answer = change(summary.to_owned(), dry_run, id);
    answer.meeting_id = Some(session.uuid());
    answer.replayed = item.done == done;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("ticking off an action item")?;
    meetings
        .action_item_done_set(MeetingActionItemDoneRequest {
            session_id: session,
            artifact_id,
            action_index: args.number - 1,
            done,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    Ok(answer)
}

pub(super) async fn snapshots(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingRef,
) -> Result<ExternalSnapshots, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    Ok(ExternalSnapshots {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid(),
        snapshots: store
            .meeting_snapshots(session)
            .map_err(|error| meeting_refusal(session, error))?,
    })
}

pub(super) async fn snapshot_delete(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    args: &SnapshotArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let snapshot = MeetingSnapshotId::from_uuid(args.snapshot);
    let (id, _) = operation(
        &args.change.write,
        "snapshot-delete",
        &(session.uuid(), args.snapshot),
    )?;
    let request = format!("{}/{}", session.uuid(), args.snapshot);
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(
            dry_run,
            "Deleted the screen picture",
            "Would delete the screen picture",
        ),
        dry_run,
        id.clone(),
    );
    answer.meeting_id = Some(session.uuid());
    answer.snapshot_id = Some(args.snapshot);
    if made_before(history, &id, SNAPSHOT_DELETE, &request)? {
        answer.replayed = true;
        return Ok(answer);
    }
    let kept = store
        .meeting_snapshots(session)
        .map_err(|error| meeting_refusal(session, error))?
        .iter()
        .any(|summary| summary.snapshot_id == snapshot);
    if !kept {
        return Err(ExternalError::new(
            ExternalErrorCode::NotFound,
            format!("No screen picture {} in this meeting.", args.snapshot),
        )
        .hint("List them with: sona work snapshots MEETING"));
    }
    if dry_run {
        return Ok(answer);
    }
    gate("deleting a screen picture")?;
    store
        .delete_meeting_snapshot(session, snapshot)
        .map_err(|error| meeting_refusal(session, error))?;
    file_made(history, &id, SNAPSHOT_DELETE, &request)?;
    Ok(answer)
}

pub(super) async fn heard_plans(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingRef,
) -> Result<ExternalHeardPlans, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    Ok(ExternalHeardPlans {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid(),
        plans: store
            .heard_plans(session)
            .map_err(|error| meeting_refusal(session, error))?,
    })
}

pub(super) async fn heard_plan_edit(
    meetings: &Arc<MeetingSessionManager>,
    args: &HeardPlanEditArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (store, found) = heard_plan(meetings, args.plan).await?;
    let (start_utc_ms, end_utc_ms) = match (args.start, args.end, found.window) {
        (Some(start), Some(end), _) => (start, end),
        (Some(start), None, Some(window)) => {
            (start, start + (window.end_utc_ms - window.start_utc_ms))
        }
        (None, Some(end), Some(window)) => (window.start_utc_ms, end),
        (None, None, Some(window)) => (window.start_utc_ms, window.end_utc_ms),
        (_, _, None) => {
            return Err(ExternalError::invalid(
                "Nobody said a day this Mac can place for that plan. Give it both --start and --end.",
            ))
        }
    };
    let edit = HeardPlanEdit {
        plan_id: found.plan_id,
        title: args.title.clone().unwrap_or_else(|| found.title.clone()),
        start_utc_ms,
        end_utc_ms,
        people: match &args.people {
            Some(people) => people
                .iter()
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect(),
            None => found.people.clone(),
        },
        place: match &args.place {
            Some(place) if place.trim().is_empty() => None,
            Some(place) => Some(place.clone()),
            None => found.place.clone(),
        },
    };
    let (id, _) = operation(
        &args.change.write,
        "heard-plan-edit",
        &(
            args.plan,
            &edit.title,
            edit.start_utc_ms,
            edit.end_utc_ms,
            &edit.people,
            &edit.place,
        ),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Changed the plan", "Would change the plan"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(found.session_id.uuid());
    answer.plan_id = Some(args.plan);
    answer.replayed = found.title == edit.title
        && found.people == edit.people
        && found.place == edit.place
        && found.window.is_some_and(|window| {
            !window.all_day
                && window.start_utc_ms == edit.start_utc_ms
                && window.end_utc_ms == edit.end_utc_ms
        });
    if found.on_calendar {
        return Err(plan_error(args.plan)(StoreError::Conflict));
    }
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("changing a heard plan")?;
    store.heard_plan_edit(edit).map_err(plan_error(args.plan))?;
    Ok(answer)
}

pub(super) async fn heard_plan_not_now(
    meetings: &Arc<MeetingSessionManager>,
    args: &HeardPlanArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (store, found) = heard_plan(meetings, args.plan).await?;
    let (id, _) = operation(&args.change.write, "heard-plan-not-now", &args.plan)?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(
            dry_run,
            "Answered not now; the plan stays and stops asking",
            "Would answer not now; the plan would stay and stop asking",
        ),
        dry_run,
        id,
    );
    answer.meeting_id = Some(found.session_id.uuid());
    answer.plan_id = Some(args.plan);
    answer.replayed = found.declined;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("answering a heard plan")?;
    store
        .heard_plan_not_now(found.plan_id)
        .map_err(plan_error(args.plan))?;
    Ok(answer)
}

pub(super) async fn said(
    meetings: &Arc<MeetingSessionManager>,
    args: &SearchArgs,
) -> Result<ExternalSaid, ExternalError> {
    let store = library(meetings).await?;
    let hidden = kept_local(&store)?;
    let limit = hits(args.limit);
    let mut moments = store
        .said_search(&args.query, MAX_HITS, meetings.user_name().as_deref())
        .map_err(QueryError::from)?;
    moments.retain(|moment| !hidden.hides(moment.session_id));
    moments.truncate(limit);
    Ok(ExternalSaid {
        schema_version: QUERY_SCHEMA_VERSION,
        query: args.query.clone(),
        moments,
    })
}

pub(super) async fn screen_text(
    meetings: &Arc<MeetingSessionManager>,
    args: &SearchArgs,
) -> Result<ExternalScreenText, ExternalError> {
    let store = library(meetings).await?;
    let hidden = kept_local(&store)?;
    let limit = hits(args.limit);
    let mut moments = store
        .snapshot_text_search(&args.query, MAX_HITS)
        .map_err(QueryError::from)?;
    moments.retain(|moment| !hidden.hides(moment.session_id));
    moments.truncate(limit);
    Ok(ExternalScreenText {
        schema_version: QUERY_SCHEMA_VERSION,
        query: args.query.clone(),
        moments,
    })
}

pub(super) async fn search(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingSearchArgs,
) -> Result<ExternalMeetingSearch, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let result = meetings
        .search(MeetingSearchRequest {
            query: args.query.clone(),
            session_ids: vec![session],
            limit: Some(hits(args.limit)),
        })
        .await
        .map_err(command_error(session.uuid()))?;
    Ok(ExternalMeetingSearch {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid(),
        query: args.query.clone(),
        hits: result.entries,
    })
}

pub(super) async fn export(
    meetings: &Arc<MeetingSessionManager>,
    args: &ExportArgs,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let format = MeetingExportFormat::from(args.format);
    let (id, operation_id) = operation(
        &args.change.write,
        "meeting-export",
        &(session.uuid(), revision, format, &args.out),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Exported the meeting", "Would export the meeting"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    answer.path = Some(args.out.display().to_string());
    if !review.can_export {
        return Err(ExternalError::invalid(
            "This meeting can't be exported until it is ready.",
        ));
    }
    if dry_run {
        return Ok(answer);
    }
    let out = args.out.clone();
    let result = meetings
        .export_to(
            MeetingExportRequest {
                operation_id,
                session_id: session,
                expected_revision: revision,
                format,
            },
            move |_| async move { Ok(out) },
        )
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

fn written(
    meeting_id: Option<Uuid>,
    contents: String,
    out: Option<&PathBuf>,
) -> Result<ExternalDocument, ExternalError> {
    let path = match out {
        Some(path) => {
            crate::meeting::export::write_atomic(path, contents.as_bytes()).map_err(|_| {
                ExternalError::new(
                    ExternalErrorCode::Failed,
                    format!("Could not write {}.", path.display()),
                )
            })?;
            Some(path.display().to_string())
        }
        None => None,
    };
    Ok(ExternalDocument {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id,
        contents: path.is_none().then_some(contents),
        path,
    })
}

pub(super) async fn ledger(
    meetings: &Arc<MeetingSessionManager>,
    args: &LedgerArgs,
) -> Result<ExternalDocument, ExternalError> {
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let html = meetings
        .ledger_html(session)
        .await
        .map_err(command_error(session.uuid()))?;
    written(Some(session.uuid()), html, args.file.out.as_ref())
}

pub(super) async fn csv(
    meetings: &Arc<MeetingSessionManager>,
    args: &FileArgs,
) -> Result<ExternalDocument, ExternalError> {
    let store = library(meetings).await?;
    let hidden = kept_local(&store)?.sessions().clone();
    let csv = meetings
        .export_csv_without(hidden)
        .await
        .map_err(|error| ExternalError::from(QueryError::from(error)))?;
    written(None, csv, args.out.as_ref())
}

impl Tracker<'_> {
    pub(super) async fn library(&self, verb: &LibraryVerb) -> Result<WorkAnswer, ExternalError> {
        let meetings = self.meetings;
        let gate = |what: &str| self.applies(what);
        let answer = match verb {
            LibraryVerb::MeetingRename(args) => {
                ExternalLibraryAnswer::Change(rename(meetings, args, &gate).await?)
            }
            LibraryVerb::MeetingDelete(args) => {
                ExternalLibraryAnswer::Change(delete(meetings, args, &gate).await?)
            }
            LibraryVerb::MeetingTrash => ExternalLibraryAnswer::Trash(trash(meetings).await?),
            LibraryVerb::MeetingRestore(args) => ExternalLibraryAnswer::Change(
                trash_once(meetings, self.history, args, true, &gate).await?,
            ),
            LibraryVerb::MeetingDeleteForever(args) => ExternalLibraryAnswer::Change(
                trash_once(meetings, self.history, args, false, &gate).await?,
            ),
            LibraryVerb::Folders => {
                library(meetings).await?;
                let list = meetings
                    .folder_list()
                    .await
                    .map_err(|error| ExternalError::from(QueryError::from(error)))?;
                ExternalLibraryAnswer::Folders(ExternalFolders {
                    schema_version: QUERY_SCHEMA_VERSION,
                    folders: list
                        .folders
                        .into_iter()
                        .map(|folder| ExternalFolder {
                            id: folder.folder_id.uuid(),
                            name: folder.name,
                            meeting_count: folder.meeting_count,
                        })
                        .collect(),
                })
            }
            LibraryVerb::FolderNew(args) => {
                ExternalLibraryAnswer::Change(folder_new(meetings, args, &gate).await?)
            }
            LibraryVerb::FolderRename(args) => {
                ExternalLibraryAnswer::Change(folder_rename(meetings, args, &gate).await?)
            }
            LibraryVerb::FolderDelete(args) => {
                ExternalLibraryAnswer::Change(folder_delete(meetings, args, &gate).await?)
            }
            LibraryVerb::FolderAdd(args) => {
                ExternalLibraryAnswer::Change(folder_membership(meetings, args, true, &gate).await?)
            }
            LibraryVerb::FolderRemove(args) => ExternalLibraryAnswer::Change(
                folder_membership(meetings, args, false, &gate).await?,
            ),
            LibraryVerb::MeetingNotes(args) => {
                ExternalLibraryAnswer::Notes(notes(meetings, args).await?)
            }
            LibraryVerb::MeetingNoteAdd(args) => {
                ExternalLibraryAnswer::Change(note_add(meetings, args, &gate).await?)
            }
            LibraryVerb::MeetingNoteEdit(args) => {
                ExternalLibraryAnswer::Change(note_edit(meetings, args, &gate).await?)
            }
            LibraryVerb::MeetingNoteDelete(args) => {
                ExternalLibraryAnswer::Change(note_delete(meetings, args, &gate).await?)
            }
            LibraryVerb::MyNotes(args) => {
                ExternalLibraryAnswer::Change(my_notes(meetings, args, &gate).await?)
            }
            LibraryVerb::ActionDone(args) => {
                ExternalLibraryAnswer::Change(action_done(meetings, args, &gate).await?)
            }
            LibraryVerb::Snapshots(args) => {
                ExternalLibraryAnswer::Snapshots(snapshots(meetings, args).await?)
            }
            LibraryVerb::SnapshotDelete(args) => ExternalLibraryAnswer::Change(
                snapshot_delete(meetings, self.history, args, &gate).await?,
            ),
            LibraryVerb::HeardPlans(args) => {
                ExternalLibraryAnswer::HeardPlans(heard_plans(meetings, args).await?)
            }
            LibraryVerb::HeardPlanEdit(args) => {
                ExternalLibraryAnswer::Change(heard_plan_edit(meetings, args, &gate).await?)
            }
            LibraryVerb::HeardPlanNotNow(args) => {
                ExternalLibraryAnswer::Change(heard_plan_not_now(meetings, args, &gate).await?)
            }
            LibraryVerb::Said(args) => ExternalLibraryAnswer::Said(said(meetings, args).await?),
            LibraryVerb::ScreenText(args) => {
                ExternalLibraryAnswer::ScreenText(screen_text(meetings, args).await?)
            }
            LibraryVerb::MeetingSearch(args) => {
                ExternalLibraryAnswer::Search(search(meetings, args).await?)
            }
            LibraryVerb::MeetingExport(args) => {
                ExternalLibraryAnswer::Change(export(meetings, args).await?)
            }
            LibraryVerb::MeetingLedger(args) => {
                ExternalLibraryAnswer::Document(ledger(meetings, args).await?)
            }
            LibraryVerb::MeetingsCsv(args) => {
                ExternalLibraryAnswer::Document(csv(meetings, args).await?)
            }
        };
        Ok(WorkAnswer::Library(Box::new(answer)))
    }
}

fn offset(ns: u64) -> String {
    let seconds = ns / NS_PER_SECOND;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub(super) fn text(answer: &ExternalLibraryAnswer, text: &mut String) {
    match answer {
        ExternalLibraryAnswer::Folders(list) => {
            if list.folders.is_empty() {
                text.push_str("No folders.\n");
            }
            for folder in &list.folders {
                let _ = writeln!(
                    text,
                    "{}  ({} meetings)  {}",
                    folder.name, folder.meeting_count, folder.id
                );
            }
        }
        ExternalLibraryAnswer::Notes(notes) => {
            let _ = writeln!(text, "{}", notes.title);
            if !notes.my_notes.trim().is_empty() {
                let _ = writeln!(text, "My notes:\n{}", notes.my_notes.trim_end());
            }
            if !notes.notes.is_empty() {
                text.push_str("Notes:\n");
                for note in &notes.notes {
                    let at = note
                        .at_seconds
                        .map(|seconds| offset(seconds * NS_PER_SECOND) + "  ")
                        .unwrap_or_default();
                    let _ = writeln!(text, "  {at}{}  {}", note.body, note.id);
                }
            }
            if !notes.action_items.is_empty() {
                text.push_str("Action items:\n");
                for item in &notes.action_items {
                    let _ = writeln!(
                        text,
                        "  {}. [{}] {}{}",
                        item.number,
                        if item.done { "x" } else { " " },
                        item.text,
                        item.owner
                            .as_deref()
                            .map(|owner| format!(" ({owner})"))
                            .unwrap_or_default()
                    );
                }
            }
        }
        ExternalLibraryAnswer::Snapshots(list) => {
            if list.snapshots.is_empty() {
                text.push_str("No screen pictures.\n");
            }
            for snapshot in &list.snapshots {
                let _ = writeln!(
                    text,
                    "{}  {}x{}  {}",
                    offset(snapshot.offset_ns),
                    snapshot.width,
                    snapshot.height,
                    snapshot.snapshot_id.uuid()
                );
            }
        }
        ExternalLibraryAnswer::HeardPlans(list) => {
            if list.plans.is_empty() {
                text.push_str("No plans heard.\n");
            }
            for plan in &list.plans {
                let _ = writeln!(
                    text,
                    "{}  {}{}  {}",
                    plan.title,
                    plan.when_text,
                    if plan.declined { "  (not now)" } else { "" },
                    plan.plan_id.uuid()
                );
            }
        }
        ExternalLibraryAnswer::Said(found) => {
            if found.moments.is_empty() {
                text.push_str("Nobody said that.\n");
            }
            for moment in &found.moments {
                let _ = writeln!(
                    text,
                    "{}  {}  {}: {}",
                    moment.meeting_title,
                    offset(moment.start_offset_ns),
                    moment.speaker_label.as_deref().unwrap_or("Someone"),
                    moment.text
                );
            }
        }
        ExternalLibraryAnswer::ScreenText(found) => {
            if found.moments.is_empty() {
                text.push_str("Nothing on screen matched.\n");
            }
            for moment in &found.moments {
                let _ = writeln!(
                    text,
                    "{}  {}  {}: {}",
                    moment.meeting_title,
                    offset(moment.offset_ns),
                    moment.title,
                    moment.excerpt
                );
            }
        }
        ExternalLibraryAnswer::Search(found) => {
            if found.hits.is_empty() {
                text.push_str("Nothing in this meeting matched.\n");
            }
            for hit in &found.hits {
                let at = hit.start_offset_ns.map(offset).unwrap_or_default();
                let _ = writeln!(text, "{at}  {}", hit.excerpt);
            }
        }
        ExternalLibraryAnswer::Document(document) => match (&document.path, &document.contents) {
            (Some(path), _) => {
                let _ = writeln!(text, "Wrote {path}");
            }
            (None, Some(contents)) => text.push_str(contents),
            (None, None) => {}
        },
        ExternalLibraryAnswer::Trash(trash) => {
            if trash.entries.is_empty() {
                text.push_str("The trash is empty.\n");
            }
            for entry in &trash.entries {
                let title = if entry.title.trim().is_empty() {
                    "Untitled meeting"
                } else {
                    entry.title.as_str()
                };
                let _ = writeln!(text, "{title}  {}", entry.id);
            }
        }
        ExternalLibraryAnswer::Change(change) => {
            let _ = writeln!(
                text,
                "{}{}",
                change.summary,
                if change.replayed && !change.dry_run {
                    " (already so)"
                } else {
                    ""
                }
            );
            if let Some(path) = &change.path {
                let _ = writeln!(text, "  {path}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        csv, delete, folder_membership, folder_new, rename, trash, trash_change, Change,
        FileArgs, FolderMembershipArgs, FolderNewArgs, MeetingChangeArgs, RenameArgs, TrashArgs,
    };
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingMutationRequest, MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::workflow_core_tests::reviewable_meeting;
    use crate::meeting::store::{MeetingStore, StoreMutation, StoreTransition};
    use crate::meeting::types::{
        AllowedMeetingAction, DegradedStartPolicy, MeetingCommandKind, MeetingListFilter,
        MeetingOperationId, MeetingOrigin, MeetingPhase, MeetingPreflightSnapshot,
        MeetingRetentionPolicy, MeetingSessionId, MeetingSourceSnapshot, OperationActor,
        ProcessingDestination, SourceAvailability, SourceHealth, SourceKind, StorageAvailability,
    };
    use crate::query::external::tracker::WriteArgs;
    use crate::query::external::{meeting_summaries, ExternalError, ExternalErrorCode};
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;
    use uuid::Uuid;

    const NOW: i64 = 1_767_000_000_000;

    fn applies(_: &str) -> Result<(), ExternalError> {
        Ok(())
    }

    struct Corpus {
        _directory: TempDir,
        meetings: Arc<MeetingSessionManager>,
        pricing: MeetingSessionId,
        kept: MeetingSessionId,
    }

    async fn corpus() -> Corpus {
        let directory = TempDir::new().unwrap();
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let meetings = Arc::new(
            MeetingSessionManager::with_parts(
                None,
                Some(directory.path().join("meetings")),
                secrets,
                Arc::new(NoCaptureSources),
            )
            .reading_only(),
        );
        let store = meetings.store().await.unwrap();
        let pricing = reviewable_meeting(&store, "Pricing sync", NOW);
        let kept = reviewable_meeting(&store, "Board prep", NOW + 1);
        store.remember_calendar_facts(kept, &board_event()).unwrap();
        store
            .set_series_remote_opt_out(
                &MeetingSeriesRemoteOptOutSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: "board".to_owned(),
                    remote_intelligence_opt_out: true,
                    expected_revision: 0,
                },
                NOW,
            )
            .unwrap();
        Corpus {
            _directory: directory,
            meetings,
            pricing,
            kept,
        }
    }

    fn board_event() -> CalendarEventSummary {
        CalendarEventSummary {
            event_key: format!("board#{NOW}"),
            series_key: "board".to_owned(),
            title: "Board prep".to_owned(),
            attendee_count: 2,
            start_utc_ms: NOW,
            end_utc_ms: NOW + 30 * 60 * 1_000,
            attendees: Vec::new(),
            notes: None,
            calendar_name: None,
            location: None,
            url: None,
            place: None,
        }
    }

    /// A meeting made the way a recording makes one, so deleting it leaves
    /// an entry in the trash that can be restored.
    fn trashable(store: &MeetingStore, title: &str) -> MeetingSessionId {
        let session_id = MeetingSessionId::new();
        let preflight = MeetingPreflightSnapshot {
            session_id,
            revision: 0,
            proposed_title: title.to_owned(),
            origin: MeetingOrigin::Manual,
            sources: SourceKind::ALL
                .into_iter()
                .map(|source_kind| MeetingSourceSnapshot {
                    track_id: None,
                    source_kind,
                    required: true,
                    availability: SourceAvailability::Available,
                    health: SourceHealth::NotStarted,
                    format: None,
                    last_durable_offset_ns: None,
                    gap_count: 0,
                })
                .collect(),
            storage: StorageAvailability::Available,
            local_processing: SourceAvailability::Available,
            destination: ProcessingDestination::Local,
            microphone_device_uid: None,
            frozen_system_audio_application_bundle_ids: Vec::new(),
            accepted_known_missing_sources: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            required_acknowledgements: SourceKind::ALL.to_vec(),
            allowed_actions: vec![AllowedMeetingAction::Start],
        };
        store
            .create_preflight(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    actor: OperationActor::User,
                    requested_at_utc_ms: NOW,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                title.to_owned(),
                MeetingOrigin::Manual,
                preflight,
                MeetingRetentionPolicy::Forever,
            )
            .unwrap();
        let revision = store.session_snapshot(session_id).unwrap().revision;
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::RecoveryFinalize,
                requested_at_utc_ms: NOW,
                session_id,
                expected_revision: revision,
                allowed_from: &[MeetingPhase::Preflight],
                next_phase: MeetingPhase::ReviewReady,
                event_kind: "test_review_ready",
                reason_codes: Vec::new(),
            })
            .unwrap();
        session_id
    }

    async fn trash_meeting(meetings: &MeetingSessionManager, store: &MeetingStore, meeting: MeetingSessionId) {
        let revision = store.session_snapshot(meeting).unwrap().revision;
        meetings
            .delete(MeetingMutationRequest {
                operation_id: MeetingOperationId::new(),
                session_id: meeting,
                expected_revision: revision,
            })
            .await
            .unwrap();
    }

    fn write(dry_run: bool) -> Change {
        Change {
            dry_run,
            write: WriteArgs {
                operation_id: None,
                agent: Some("Organizer".to_owned()),
            },
        }
    }

    fn renaming(meeting: MeetingSessionId, title: &str, dry_run: bool) -> RenameArgs {
        RenameArgs {
            meeting: meeting.uuid().to_string(),
            title: title.to_owned(),
            change: write(dry_run),
        }
    }

    async fn title(corpus: &Corpus, meeting: MeetingSessionId) -> String {
        let store = corpus.meetings.store().await.unwrap();
        store.review_snapshot(meeting).unwrap().session.title
    }

    #[test]
    fn meeting_rename_writes_once_and_a_dry_run_writes_nothing() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let dry = rename(&corpus.meetings, &renaming(corpus.pricing, "Q3 pricing", true), &applies)
                .await
                .unwrap();
            assert!(dry.dry_run && !dry.replayed);
            assert_eq!(title(&corpus, corpus.pricing).await, "Pricing sync");

            let renamed = rename(&corpus.meetings, &renaming(corpus.pricing, "Q3 pricing", false), &applies)
                .await
                .unwrap();
            assert!(!renamed.replayed);
            assert_eq!(title(&corpus, corpus.pricing).await, "Q3 pricing");

            let again = rename(&corpus.meetings, &renaming(corpus.pricing, "Q3 pricing", false), &applies)
                .await
                .unwrap();
            assert!(again.replayed);
        });
    }

    #[test]
    fn a_meeting_kept_on_this_mac_is_never_changed_or_exported() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let refused = rename(&corpus.meetings, &renaming(corpus.kept, "Leaked", false), &applies)
                .await
                .unwrap_err();
            assert_eq!(refused.error, ExternalErrorCode::NotFound);
            assert_eq!(title(&corpus, corpus.kept).await, "Board prep");

            let exported = csv(&corpus.meetings, &FileArgs { out: None }).await.unwrap();
            let contents = exported.contents.unwrap();
            assert!(contents.contains("Pricing sync"));
            assert!(!contents.contains("Board prep"));
        });
    }

    #[test]
    fn filing_a_meeting_narrows_the_list_to_its_folder() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let made = folder_new(
                &corpus.meetings,
                &FolderNewArgs { name: "Q3".to_owned(), change: write(false) },
                &applies,
            )
            .await
            .unwrap();
            let filing = FolderMembershipArgs {
                meeting: corpus.pricing.uuid().to_string(),
                folder: "q3".to_owned(),
                change: write(false),
            };
            let filed = folder_membership(&corpus.meetings, &filing, true, &applies)
                .await
                .unwrap();
            assert!(!filed.replayed);
            assert_eq!(filed.folder_id, made.folder_id);
            assert!(folder_membership(&corpus.meetings, &filing, true, &applies)
                .await
                .unwrap()
                .replayed);

            let store = corpus.meetings.store().await.unwrap();
            let folder_id = corpus
                .meetings
                .folders_for_session(corpus.pricing)
                .await
                .unwrap()
                .into_iter()
                .next();
            let (listed, _) = meeting_summaries(
                &store,
                None,
                None,
                25,
                &MeetingListFilter { folder_id, ..MeetingListFilter::default() },
            )
            .unwrap();
            let ids: Vec<_> = listed.iter().map(|summary| summary.session_id).collect();
            assert_eq!(ids, vec![corpus.pricing]);
        });
    }

    #[test]
    fn meeting_delete_takes_the_meeting_out_and_a_retry_replays() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let mut args = MeetingChangeArgs {
                meeting: corpus.pricing.uuid().to_string(),
                change: write(false),
            };
            args.change.write.operation_id = Some(Uuid::new_v4());
            let first = delete(&corpus.meetings, &args, &applies).await.unwrap();
            assert!(!first.replayed);
            let store = corpus.meetings.store().await.unwrap();
            assert!(store.review_snapshot(corpus.pricing).is_err());
            let refused = rename(&corpus.meetings, &renaming(corpus.pricing, "Back", false), &applies)
                .await
                .unwrap_err();
            assert_eq!(refused.error, ExternalErrorCode::NotFound);

            let again = delete(&corpus.meetings, &args, &applies).await.unwrap();
            assert!(again.replayed);
            assert_eq!(again.operation_id, first.operation_id);
            assert_eq!(again.trash_id, first.trash_id);
        });
    }

    #[test]
    fn a_meeting_kept_on_this_mac_stays_out_of_the_trash_and_kept_after_a_restore() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let store = corpus.meetings.store().await.unwrap();
            let open = trashable(&store, "Roadmap review");
            let kept = trashable(&store, "Board offsite");
            store.remember_calendar_facts(kept, &board_event()).unwrap();
            trash_meeting(&corpus.meetings, &store, open).await;
            trash_meeting(&corpus.meetings, &store, kept).await;

            let listed = trash(&corpus.meetings).await.unwrap();
            let titles: Vec<_> = listed.entries.iter().map(|entry| entry.title.as_str()).collect();
            assert_eq!(titles, vec!["Roadmap review"]);

            let kept_entry = corpus
                .meetings
                .trash_list()
                .await
                .unwrap()
                .into_iter()
                .find(|entry| entry.title == "Board offsite")
                .unwrap()
                .job_id;
            for restore in [true, false] {
                let asked = TrashArgs { entry: kept_entry.uuid(), change: write(false) };
                let refused = trash_change(&corpus.meetings, &asked, restore, &applies)
                    .await
                    .unwrap_err();
                assert_eq!(refused.error, ExternalErrorCode::NotFound);
            }
            assert!(store.review_snapshot(kept).is_err());

            let asked = TrashArgs { entry: listed.entries[0].id, change: write(false) };
            let restored = trash_change(&corpus.meetings, &asked, true, &applies).await.unwrap();
            assert_eq!(restored.meeting_id, Some(open.uuid()));

            corpus.meetings.trash_restore(kept_entry).await.unwrap();
            let refused = rename(&corpus.meetings, &renaming(kept, "Leaked", false), &applies)
                .await
                .unwrap_err();
            assert_eq!(refused.error, ExternalErrorCode::NotFound);
        });
    }
}
