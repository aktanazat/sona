use super::{
    conversations_closed, now_ms, refusal, task_ref, Board, Fence, TaskRef, Tracker, WorkAnswer,
    WriteArgs,
};
use crate::managers::history::HistoryManager;
use crate::meeting::document_types::{
    Document, DocumentDeleteRequest, DocumentDifference, DocumentId, DocumentIngestRequest,
    DocumentSummary,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{
    MeetingCommandError, MeetingFolderId, MeetingOperationId, MeetingSessionId,
};
use crate::planning::{self, EvidenceLink, EvidenceSourceKind, EvidenceTargetKind};
use crate::query::external::{
    command_error, loop_id, meeting_refusal, missing_meeting, note_id, person_id, session_id,
    store, ExternalError, ExternalErrorCode, ExternalScope, READ_SCOPES, WRITE_SCOPES,
};
use crate::query::{KeptLocal, QueryError, QueryRow, QUERY_SCHEMA_VERSION};
use clap::{ArgGroup, Args, Subcommand};
use rusqlite::OptionalExtension as _;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::Manager;
use uuid::Uuid;

const DELETE_COMMAND: &str = "cli_document_delete";

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum SourcesVerb {
    /// Link a meeting, a note or a meeting loop to a task or a project as its source.
    SourceLink(SourceLinkArgs),
    /// The evidence Sona's Ask hands its model for a question: the matching
    /// lines, quoted with their sona:// links. Answer from it yourself.
    AskPack(AskPackArgs),
    /// Documents added to Sona, newest first: all, one person's, or one
    /// meeting's with what was said that differs from them.
    Docs(DocsArgs),
    /// Add a .txt, .md or .pdf file as a document, on its own or to a meeting.
    DocAdd(DocAddArgs),
    /// Delete a document. Nothing brings it back.
    DocDelete(DocDeleteArgs),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[command(group(ArgGroup::new("source").required(true).args(["meeting", "note", "loop_id"])))]
pub struct SourceLinkArgs {
    /// A task (S-12, 12 or its id), or a project's name or id.
    #[arg(value_name = "TARGET")]
    pub target: String,
    /// A meeting's id.
    #[arg(long, value_name = "MEETING_ID")]
    pub meeting: Option<String>,
    /// A note's id, from `sona --notes`.
    #[arg(long, value_name = "NOTE_ID")]
    pub note: Option<String>,
    /// A meeting loop's id, from `sona --loops`.
    #[arg(long = "loop", value_name = "LOOP_ID")]
    pub loop_id: Option<String>,
    /// With --meeting: the moment to point at, in seconds from the meeting's start.
    #[arg(long, value_name = "SECONDS", requires = "meeting")]
    pub at: Option<u32>,
    /// Print what would be linked and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct AskPackArgs {
    /// The question: "what did Dana say about pricing".
    #[arg(required = true, num_args = 1.., value_name = "QUESTION")]
    pub question: Vec<String>,
    /// Only the meetings filed in this folder: its name or id.
    #[arg(long, value_name = "FOLDER")]
    pub folder: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DocsArgs {
    /// Only documents that name this person: a name they go by, or their id.
    #[arg(long, value_name = "PERSON", conflicts_with = "meeting")]
    pub person: Option<String>,
    /// Only documents added to this meeting, each with what was said that differs from it.
    #[arg(long, value_name = "MEETING_ID")]
    pub meeting: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DocAddArgs {
    /// The file: .txt, .md, .markdown or .pdf.
    #[arg(value_name = "PATH")]
    pub path: PathBuf,
    /// Add it to this meeting, by its id.
    #[arg(long, value_name = "MEETING_ID")]
    pub meeting: Option<String>,
    /// Print what would be added and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DocDeleteArgs {
    /// The document's id, from `sona work docs`.
    #[arg(value_name = "DOCUMENT_ID")]
    pub document: Uuid,
    /// Print what would be deleted and delete nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

impl SourcesVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::AskPack(_)
            | Self::Docs(_)
            | Self::SourceLink(SourceLinkArgs { dry_run: true, .. })
            | Self::DocAdd(DocAddArgs { dry_run: true, .. })
            | Self::DocDelete(DocDeleteArgs { dry_run: true, .. }) => READ_SCOPES,
            Self::SourceLink(_) | Self::DocAdd(_) | Self::DocDelete(_) => WRITE_SCOPES,
        }
    }

    pub(super) fn check(&self) -> Result<(), ExternalError> {
        match self {
            Self::AskPack(args) if args.question.join(" ").trim().is_empty() => {
                Err(ExternalError::invalid("Ask a question."))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalSourcesAnswer {
    Link(ExternalSourceLink),
    Pack(ExternalAskPack),
    Documents(ExternalDocuments),
    Document(ExternalDocumentChange),
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSourceLink {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub replayed: bool,
    pub target_kind: EvidenceTargetKind,
    pub target_id: String,
    pub target: String,
    pub source_kind: EvidenceSourceKind,
    pub source_id: String,
    pub at_ms: Option<i64>,
    pub link: Option<EvidenceLink>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFolder {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalAskPack {
    pub schema_version: u32,
    pub question: String,
    pub folder: Option<ExternalFolder>,
    pub pack: String,
    pub sources: Vec<QueryRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDocument {
    pub id: String,
    pub title: String,
    pub source_name: String,
    pub media_type: String,
    pub created_at_ms: i64,
    pub meeting_id: Option<String>,
    pub content: Option<String>,
    pub differences: Option<Vec<DocumentDifference>>,
}

impl ExternalDocument {
    fn from_summary(
        summary: DocumentSummary,
        meeting_id: Option<String>,
        content: Option<String>,
        differences: Option<Vec<DocumentDifference>>,
    ) -> Self {
        Self {
            id: summary.id.uuid().to_string(),
            title: summary.title,
            source_name: summary.source_name,
            media_type: summary.media_type,
            created_at_ms: summary.created_at_utc_ms,
            meeting_id,
            content,
            differences,
        }
    }

    fn whole(document: Document, meeting_id: Option<String>) -> Self {
        Self::from_summary(document.summary, meeting_id, Some(document.content), None)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDocuments {
    pub schema_version: u32,
    pub entries: Vec<ExternalDocument>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentAction {
    Added,
    Deleted,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDocumentChange {
    pub schema_version: u32,
    pub action: DocumentAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub replayed: bool,
    pub path: Option<PathBuf>,
    pub meeting_id: Option<String>,
    pub document: Option<ExternalDocument>,
}

pub(super) struct LinkedSource {
    kind: EvidenceSourceKind,
    id: String,
}

struct LinkTarget {
    kind: EvidenceTargetKind,
    id: String,
    name: String,
}

pub(super) struct DocumentPlan {
    path: PathBuf,
    session: Option<MeetingSessionId>,
}

async fn open_store(
    meetings: &Arc<MeetingSessionManager>,
) -> Result<(Arc<MeetingStore>, KeptLocal), ExternalError> {
    let Fence::Open(kept_local) = Fence::read(meetings).await? else {
        return Err(conversations_closed());
    };
    Ok((store(meetings).await?, kept_local))
}

fn visible(kept_local: &KeptLocal, session: MeetingSessionId) -> Result<(), ExternalError> {
    if kept_local.hides(session) {
        return Err(missing_meeting(session));
    }
    Ok(())
}

fn command_failed(error: MeetingCommandError) -> ExternalError {
    QueryError::from(error).into()
}

fn missing_document(id: Uuid) -> ExternalError {
    ExternalError::new(ExternalErrorCode::NotFound, format!("No document {id}."))
        .hint("List them with: sona work docs")
}

pub(super) async fn linked_source(
    meetings: &Arc<MeetingSessionManager>,
    args: &SourceLinkArgs,
) -> Result<LinkedSource, ExternalError> {
    match (&args.meeting, &args.note, &args.loop_id) {
        (Some(meeting), _, _) => {
            let session = session_id(meeting)?;
            let (_, kept_local) = open_store(meetings).await?;
            visible(&kept_local, session)?;
            Ok(LinkedSource {
                kind: EvidenceSourceKind::Meeting,
                id: session.uuid().to_string(),
            })
        }
        (None, Some(note), _) => Ok(LinkedSource {
            kind: EvidenceSourceKind::Note,
            id: note_id(note)?,
        }),
        (None, None, Some(value)) => {
            let found = loop_id(value)?;
            let session = found
                .session_id()
                .ok_or_else(|| ExternalError::invalid(format!("{value:?} is not a loop id.")))?;
            let (_, kept_local) = open_store(meetings).await?;
            visible(&kept_local, session)?;
            Ok(LinkedSource {
                kind: EvidenceSourceKind::Loop,
                id: found.0,
            })
        }
        (None, None, None) => Err(ExternalError::invalid(
            "Name a source: --meeting, --note or --loop.",
        )),
    }
}

fn link_target(board: &Board, value: &str) -> Result<LinkTarget, ExternalError> {
    let task = match task_ref(value) {
        Ok(found @ TaskRef::Number(_)) => Some(board.task(&found)?),
        Ok(TaskRef::Id(id)) => board.row(&id),
        Err(_) => None,
    };
    if let Some(row) = task {
        return Ok(LinkTarget {
            kind: EvidenceTargetKind::Task,
            id: row.item.id.clone(),
            name: format!("S-{} {}", row.item.number, row.item.title),
        });
    }
    let project = board
        .project(value)?
        .ok_or_else(|| ExternalError::invalid("Name a task or a project to link to."))?;
    Ok(LinkTarget {
        kind: EvidenceTargetKind::Project,
        id: project.id.clone(),
        name: project.title.clone(),
    })
}

fn folder(store: &MeetingStore, value: &str) -> Result<(MeetingFolderId, ExternalFolder), ExternalError> {
    let folders: Vec<(String, String, MeetingFolderId)> = store
        .meeting_folders()
        .map_err(QueryError::from)?
        .into_iter()
        .map(|folder| {
            (
                folder.folder_id.uuid().to_string(),
                folder.name,
                folder.folder_id,
            )
        })
        .collect();
    let (id, name, folder_id) = super::pick("folder", value, &folders, |f| &f.0, |f| &f.1)?;
    Ok((
        *folder_id,
        ExternalFolder {
            id: id.clone(),
            name: name.clone(),
        },
    ))
}

fn placed(store: &MeetingStore) -> Result<HashMap<String, Option<String>>, ExternalError> {
    store
        .with_planning_snapshot(|connection| {
            let mut statement = connection.prepare("SELECT id, session_id FROM documents")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<Result<HashMap<String, Option<String>>, _>>()?)
        })
        .map_err(|error| QueryError::from(error).into())
}

fn shown(
    kept_local: &KeptLocal,
    placed: &HashMap<String, Option<String>>,
    id: DocumentId,
) -> Option<Option<String>> {
    let meeting = placed.get(&id.uuid().to_string())?;
    match meeting {
        Some(meeting) if kept_local.hides_meeting(meeting) => None,
        meeting => Some(meeting.clone()),
    }
}

pub(super) async fn documents(
    meetings: &Arc<MeetingSessionManager>,
    person: Option<PersonId>,
    meeting: Option<&str>,
) -> Result<ExternalDocuments, ExternalError> {
    let (store, kept_local) = open_store(meetings).await?;
    let entries = if let Some(meeting) = meeting {
        let session = session_id(meeting)?;
        visible(&kept_local, session)?;
        meetings
            .doc_meeting_list(session)
            .await
            .map_err(command_error(session.uuid()))?
            .into_iter()
            .map(|found| {
                ExternalDocument::from_summary(
                    found.document,
                    Some(session.uuid().to_string()),
                    None,
                    found.differences,
                )
            })
            .collect()
    } else {
        let listed = meetings.doc_list(person).await.map_err(command_failed)?;
        let placed = placed(&store)?;
        listed
            .entries
            .into_iter()
            .filter_map(|document| {
                let meeting = shown(&kept_local, &placed, document.summary.id)?;
                Some(ExternalDocument::whole(document, meeting))
            })
            .collect()
    };
    Ok(ExternalDocuments {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
    })
}

pub(super) async fn plan_add(
    meetings: &Arc<MeetingSessionManager>,
    path: &Path,
    meeting: Option<&str>,
) -> Result<DocumentPlan, ExternalError> {
    let path = std::fs::canonicalize(path)
        .ok()
        .filter(|found| found.is_file())
        .ok_or_else(|| ExternalError::invalid(format!("No file at {}.", path.display())))?;
    let readable = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| matches!(extension.as_str(), "txt" | "md" | "markdown" | "pdf"));
    if !readable || path.to_str().is_none() {
        return Err(ExternalError::invalid(
            "Sona adds .txt, .md, .markdown and .pdf files.",
        ));
    }
    let (store, kept_local) = open_store(meetings).await?;
    let session = match meeting {
        Some(meeting) => {
            let session = session_id(meeting)?;
            visible(&kept_local, session)?;
            store
                .session_snapshot(session)
                .map_err(|error| meeting_refusal(session, error))?;
            Some(session)
        }
        None => None,
    };
    Ok(DocumentPlan { path, session })
}

impl DocumentPlan {
    fn change(
        self,
        dry_run: bool,
        operation_id: Option<String>,
        replayed: bool,
        document: Option<ExternalDocument>,
    ) -> ExternalDocumentChange {
        ExternalDocumentChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: DocumentAction::Added,
            dry_run,
            operation_id,
            replayed,
            meeting_id: self.session.map(|session| session.uuid().to_string()),
            path: Some(self.path),
            document,
        }
    }

    pub(super) async fn add(
        self,
        meetings: &Arc<MeetingSessionManager>,
        operation_id: Uuid,
    ) -> Result<ExternalDocumentChange, ExternalError> {
        let started = now_ms();
        let path = self.path.to_str().unwrap_or_default().to_owned();
        let result = meetings
            .doc_ingest(DocumentIngestRequest {
                path,
                operation_id: MeetingOperationId::from_uuid(operation_id),
                session_id: self.session,
            })
            .await
            .map_err(|error| match (error, self.session) {
                (MeetingCommandError::NotFound, Some(session)) => missing_meeting(session),
                (MeetingCommandError::InvalidRequest | MeetingCommandError::ImportUnreadable, _) => {
                    ExternalError::invalid("Sona could not read the file's text.")
                }
                (error, _) => command_failed(error),
            })?;
        let document = result.document.ok_or_else(|| {
            ExternalError::new(ExternalErrorCode::Failed, "The document could not be added.")
        })?;
        let replayed = document.summary.created_at_utc_ms < started;
        let meeting = self.session.map(|session| session.uuid().to_string());
        Ok(self.change(
            false,
            Some(operation_id.to_string()),
            replayed,
            Some(ExternalDocument::whole(document, meeting)),
        ))
    }
}

pub(super) async fn deletable(
    meetings: &Arc<MeetingSessionManager>,
    id: Uuid,
) -> Result<(ExternalDocument, u64), ExternalError> {
    let (store, kept_local) = open_store(meetings).await?;
    let listed = meetings.doc_list(None).await.map_err(command_failed)?;
    let placed = placed(&store)?;
    listed
        .entries
        .into_iter()
        .find(|document| document.summary.id.uuid() == id)
        .and_then(|document| {
            let meeting = shown(&kept_local, &placed, document.summary.id)?;
            Some(ExternalDocument::from_summary(document.summary, meeting, None, None))
        })
        .map(|document| (document, listed.revision))
        .ok_or_else(|| missing_document(id))
}

fn delete_receipt(
    history: &HistoryManager,
    operation_id: &str,
    request: &str,
) -> Result<Option<ExternalDocument>, ExternalError> {
    let found: Option<(String, String, String)> = planning::history(history, |conn| {
        Ok(conn
            .query_row(
                "SELECT command, request_json, response_json FROM planning_operations WHERE operation_id = ?1",
                [operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    })
    .map_err(|message| refusal(history, message))?;
    match found {
        None => Ok(None),
        Some((command, asked, title)) if command == DELETE_COMMAND && asked == request => {
            Ok(Some(ExternalDocument {
                id: request.to_owned(),
                title,
                source_name: String::new(),
                media_type: String::new(),
                created_at_ms: 0,
                meeting_id: None,
                content: None,
                differences: None,
            }))
        }
        Some(_) => Err(ExternalError::invalid(format!(
            "Operation id {operation_id} was already used for a different change. Pass a new --operation-id, or leave it out."
        ))),
    }
}

fn file_delete_receipt(
    history: &HistoryManager,
    operation_id: &str,
    request: &str,
    title: &str,
) -> Result<(), ExternalError> {
    planning::history(history, |conn| {
        conn.execute(
            "INSERT INTO planning_operations (operation_id, command, request_json, response_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![operation_id, DELETE_COMMAND, request, title, now_ms()],
        )?;
        Ok(())
    })
    .map_err(|message| refusal(history, message))
}

impl Tracker<'_> {
    pub(super) async fn sources(&self, verb: &SourcesVerb) -> Result<WorkAnswer, ExternalError> {
        let answer = match verb {
            SourcesVerb::SourceLink(args) => ExternalSourcesAnswer::Link(self.source_link(args).await?),
            SourcesVerb::AskPack(args) => ExternalSourcesAnswer::Pack(self.ask_pack(args).await?),
            SourcesVerb::Docs(args) => {
                let person = match &args.person {
                    Some(value) => Some(person_id(&self.board().await?.person(value)?.id)?),
                    None => None,
                };
                ExternalSourcesAnswer::Documents(
                    documents(self.meetings, person, args.meeting.as_deref()).await?,
                )
            }
            SourcesVerb::DocAdd(args) => ExternalSourcesAnswer::Document(self.doc_add(args).await?),
            SourcesVerb::DocDelete(args) => {
                ExternalSourcesAnswer::Document(self.doc_delete(args).await?)
            }
        };
        Ok(WorkAnswer::Sources(Box::new(answer)))
    }

    pub(super) async fn source_link(
        &self,
        args: &SourceLinkArgs,
    ) -> Result<ExternalSourceLink, ExternalError> {
        let source = linked_source(self.meetings, args).await?;
        let board = self.board().await?;
        let target = link_target(&board, &args.target)?;
        let at_ms = args.at.map(|seconds| i64::from(seconds) * 1_000);
        let mut answer = ExternalSourceLink {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: args.write.operation_id.map(|id| id.to_string()),
            replayed: false,
            target_kind: target.kind,
            target_id: target.id.clone(),
            target: target.name,
            source_kind: source.kind,
            source_id: source.id.clone(),
            at_ms,
            link: None,
        };
        if args.dry_run {
            return Ok(answer);
        }
        self.applies("linking sources")?;
        let operation_id = args.write.fresh_operation_id();
        let started = now_ms();
        let link = planning::planning_link_source(
            self.app.clone(),
            self.app.state(),
            self.app.state(),
            target.kind,
            target.id,
            source.kind,
            source.id,
            at_ms,
            operation_id.clone(),
        )
        .await
        .map_err(|message| refusal(self.history, message))?;
        answer.replayed = link.created_at_ms < started;
        answer.operation_id = Some(operation_id);
        answer.link = Some(link);
        Ok(answer)
    }

    async fn ask_pack(&self, args: &AskPackArgs) -> Result<ExternalAskPack, ExternalError> {
        let (store, _) = open_store(self.meetings).await?;
        let folder = args
            .folder
            .as_deref()
            .map(|value| folder(&store, value))
            .transpose()?;
        let question = args.question.join(" ");
        let history = self.app.state::<Arc<HistoryManager>>();
        let calendar = crate::meeting::detection::calendar::platform_calendar();
        let pack = crate::query::pack::for_question(
            self.meetings,
            &history,
            &calendar,
            &question,
            folder.as_ref().map(|(id, _)| *id),
        )
        .await?;
        Ok(ExternalAskPack {
            schema_version: pack.schema_version,
            question,
            folder: folder.map(|(_, folder)| folder),
            pack: pack.pack,
            sources: pack.sources,
        })
    }

    async fn doc_add(&self, args: &DocAddArgs) -> Result<ExternalDocumentChange, ExternalError> {
        let plan = plan_add(self.meetings, &args.path, args.meeting.as_deref()).await?;
        if args.dry_run {
            return Ok(plan.change(
                true,
                args.write.operation_id.map(|id| id.to_string()),
                false,
                None,
            ));
        }
        self.applies("adding documents")?;
        plan.add(
            self.meetings,
            args.write.operation_id.unwrap_or_else(Uuid::new_v4),
        )
        .await
    }

    async fn doc_delete(&self, args: &DocDeleteArgs) -> Result<ExternalDocumentChange, ExternalError> {
        let operation_id = args.write.fresh_operation_id();
        let request = args.document.to_string();
        let mut change = ExternalDocumentChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: DocumentAction::Deleted,
            dry_run: args.dry_run,
            operation_id: Some(operation_id.clone()),
            replayed: false,
            path: None,
            meeting_id: None,
            document: None,
        };
        if let Some(earlier) = delete_receipt(self.history, &operation_id, &request)? {
            change.replayed = true;
            change.document = Some(earlier);
            return Ok(change);
        }
        let (document, revision) = deletable(self.meetings, args.document).await?;
        change.meeting_id.clone_from(&document.meeting_id);
        if args.dry_run {
            change.operation_id = args.write.operation_id.map(|id| id.to_string());
            change.document = Some(document);
            return Ok(change);
        }
        self.applies("deleting documents")?;
        self.meetings
            .doc_delete(DocumentDeleteRequest {
                document_id: DocumentId(args.document),
                expected_revision: revision,
            })
            .await
            .map_err(|error| match error {
                MeetingCommandError::NotFound => missing_document(args.document),
                error => command_failed(error),
            })?;
        file_delete_receipt(self.history, &operation_id, &request, &document.title)?;
        change.document = Some(document);
        Ok(change)
    }
}

pub(super) fn text(answer: &ExternalSourcesAnswer, text: &mut String) {
    match answer {
        ExternalSourcesAnswer::Link(link) => {
            let verb = if link.dry_run { "Would link" } else { "Linked" };
            let _ = writeln!(
                text,
                "{verb} to {}{}",
                link.target,
                if link.replayed { " (already so)" } else { "" }
            );
            let title = link
                .link
                .as_ref()
                .and_then(|link| link.title.as_deref())
                .unwrap_or("");
            let _ = writeln!(
                text,
                "  {} {title}  {}",
                link.source_kind.as_str(),
                link.source_id
            );
            if let Some(at_ms) = link.at_ms {
                let seconds = at_ms / 1_000;
                let _ = writeln!(text, "  at {}:{:02}", seconds / 60, seconds % 60);
            }
        }
        ExternalSourcesAnswer::Pack(pack) => {
            let _ = writeln!(text, "{}", pack.pack);
        }
        ExternalSourcesAnswer::Documents(page) => {
            if page.entries.is_empty() {
                text.push_str("No documents.\n");
            }
            for document in &page.entries {
                document_line(text, document);
            }
        }
        ExternalSourcesAnswer::Document(change) => {
            let verb = match (change.action, change.dry_run) {
                (DocumentAction::Added, false) => "Added the document",
                (DocumentAction::Added, true) => "Would add the document",
                (DocumentAction::Deleted, false) => "Deleted the document",
                (DocumentAction::Deleted, true) => "Would delete the document",
            };
            let _ = writeln!(
                text,
                "{verb}{}",
                if change.replayed && !change.dry_run {
                    " (already so)"
                } else {
                    ""
                }
            );
            if let Some(document) = &change.document {
                document_line(text, document);
            } else if let Some(path) = &change.path {
                let _ = writeln!(text, "  {}", path.display());
            }
            if change.document.is_none() {
                if let Some(meeting) = &change.meeting_id {
                    let _ = writeln!(text, "  meeting {meeting}");
                }
            }
        }
    }
}

fn document_line(text: &mut String, document: &ExternalDocument) {
    let _ = writeln!(text, "{}  id {}", document.title, document.id);
    if !document.source_name.is_empty() {
        let _ = writeln!(text, "  {}  {}", document.source_name, document.media_type);
    }
    if let Some(meeting) = &document.meeting_id {
        let _ = writeln!(text, "  meeting {meeting}");
    }
    for difference in document.differences.iter().flatten() {
        let page = difference
            .page
            .map_or_else(String::new, |page| format!("page {page}: "));
        let _ = writeln!(
            text,
            "  {page}written {:?}, said {:?}{}",
            difference.written,
            difference.said,
            difference
                .speaker
                .as_deref()
                .map_or_else(String::new, |speaker| format!(" by {speaker}"))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{documents, linked_source, plan_add, SourceLinkArgs};
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::documents::DocumentBody;
    use crate::meeting::store::workflow_core_tests::reviewable_meeting;
    use crate::meeting::store::MeetingStore;
    use crate::meeting::types::{MeetingOperationId, MeetingSessionId};
    use crate::query::external::tracker::WriteArgs;
    use crate::query::external::ExternalErrorCode;
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;

    const NOW: i64 = 1_767_000_000_000;

    struct Corpus {
        directory: TempDir,
        meetings: Arc<MeetingSessionManager>,
        store: Arc<MeetingStore>,
        kept: MeetingSessionId,
        shared: MeetingSessionId,
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
        let kept = reviewable_meeting(&store, "Board prep", NOW);
        let shared = reviewable_meeting(&store, "Pricing sync", NOW + 1);
        store
            .remember_calendar_facts(
                kept,
                &CalendarEventSummary {
                    event_key: format!("board-prep#{NOW}"),
                    series_key: "board-prep".to_owned(),
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
                },
            )
            .unwrap();
        store
            .set_series_remote_opt_out(
                &MeetingSeriesRemoteOptOutSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: "board-prep".to_owned(),
                    remote_intelligence_opt_out: true,
                    expected_revision: 0,
                },
                NOW,
            )
            .unwrap();
        Corpus {
            directory,
            meetings,
            store,
            kept,
            shared,
        }
    }

    fn add(store: &MeetingStore, title: &str, session: Option<MeetingSessionId>) {
        store
            .ingest_document(
                MeetingOperationId::new(),
                title.to_owned(),
                format!("{title}.md"),
                DocumentBody::Text {
                    markdown: true,
                    content: format!("{title} body"),
                },
                session,
                NOW,
            )
            .unwrap();
    }

    fn link_args(meeting: Option<String>, loop_id: Option<String>) -> SourceLinkArgs {
        SourceLinkArgs {
            target: "S-1".to_owned(),
            meeting,
            note: None,
            loop_id,
            at: None,
            dry_run: false,
            write: WriteArgs {
                operation_id: None,
                agent: Some("Organizer".to_owned()),
            },
        }
    }

    #[test]
    fn a_meeting_or_loop_kept_on_this_mac_is_no_source() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let kept = corpus.kept.uuid();
            for args in [
                link_args(Some(kept.to_string()), None),
                link_args(None, Some(format!("{kept}:commitment:abc"))),
            ] {
                let refused = linked_source(&corpus.meetings, &args).await.err().unwrap();
                assert_eq!(refused.error, ExternalErrorCode::NotFound);
            }
            let shared = corpus.shared.uuid().to_string();
            let found = linked_source(&corpus.meetings, &link_args(Some(shared.clone()), None))
                .await
                .unwrap();
            assert_eq!(found.id, shared);
        });
    }

    #[test]
    fn documents_of_a_meeting_kept_on_this_mac_stay_out() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            add(&corpus.store, "Kept contract", Some(corpus.kept));
            add(&corpus.store, "Shared contract", Some(corpus.shared));
            add(&corpus.store, "Handbook", None);

            let listed = documents(&corpus.meetings, None, None).await.unwrap();
            let mut titles: Vec<&str> = listed
                .entries
                .iter()
                .map(|document| document.title.as_str())
                .collect();
            titles.sort_unstable();
            assert_eq!(titles, ["Handbook", "Shared contract"]);

            let refused = documents(&corpus.meetings, None, Some(&corpus.kept.uuid().to_string()))
                .await
                .err()
                .unwrap();
            assert_eq!(refused.error, ExternalErrorCode::NotFound);

            let file = corpus.directory.path().join("notes.md");
            std::fs::write(&file, "terms").unwrap();
            let refused = plan_add(&corpus.meetings, &file, Some(&corpus.kept.uuid().to_string()))
                .await
                .err()
                .unwrap();
            assert_eq!(refused.error, ExternalErrorCode::NotFound);
        });
    }

    #[test]
    fn adding_a_document_twice_under_one_operation_adds_it_once() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let file = corpus.directory.path().join("terms.md");
            std::fs::write(&file, "Payment within sixty days.").unwrap();
            let shared = corpus.shared.uuid().to_string();
            let operation = uuid::Uuid::new_v4();

            let first = plan_add(&corpus.meetings, &file, Some(&shared))
                .await
                .unwrap()
                .add(&corpus.meetings, operation)
                .await
                .unwrap();
            assert!(!first.replayed);
            let again = plan_add(&corpus.meetings, &file, Some(&shared))
                .await
                .unwrap()
                .add(&corpus.meetings, operation)
                .await
                .unwrap();
            assert!(again.replayed);
            assert_eq!(
                again.document.as_ref().map(|document| &document.id),
                first.document.as_ref().map(|document| &document.id)
            );

            let listed = documents(&corpus.meetings, None, None).await.unwrap();
            assert_eq!(listed.entries.len(), 1);
            assert_eq!(listed.entries[0].meeting_id.as_deref(), Some(shared.as_str()));
        });
    }
}
