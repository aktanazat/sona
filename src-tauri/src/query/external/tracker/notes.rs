//! `sona work` verbs that write notes: make one, change its body, pin it,
//! link a meeting, a person or another note to it, or file it as a document
//! of a task, a project or a cycle. Only notes and those links are written.

use super::library::meeting;
use super::transcript::missing_line;
use super::{
    cycle_ref, named_cycle, now_ms, refusal, task_ref, Fence, Tracker, WorkAnswer, WriteArgs,
};
use crate::planning::{self, EvidenceSourceKind, EvidenceTargetKind};
use crate::planning::work::types::WorkCycleRef;
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use crate::scratchpad::documents::{self, ScratchDocumentKind, ScratchDocumentOf};
use crate::scratchpad::links::{self, ScratchSourceKind, ScratchSourceRef};
use crate::scratchpad::{self, passages, Author, ScratchNote, ScratchpadError};
use clap::{ArgGroup, Args};
use rusqlite::{Connection, OptionalExtension as _};
use serde::Serialize;
use std::fmt::Write as _;
use tauri::Manager;
use uuid::Uuid;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NoteNewArgs {
    /// The note's text. Its first words become its title.
    #[arg(required = true, num_args = 1.., value_name = "TEXT")]
    pub body: Vec<String>,
    /// File it as a document of this task: S-12, 12 or its id.
    #[arg(long, value_name = "TASK", conflicts_with_all = ["project", "cycle"])]
    pub task: Option<String>,
    /// File it as a document of this project, by name or id.
    #[arg(long, value_name = "PROJECT", conflicts_with = "cycle")]
    pub project: Option<String>,
    /// File it as a document of this cycle: current, next, or its number.
    #[arg(long, value_parser = cycle_ref, value_name = "CYCLE")]
    pub cycle: Option<WorkCycleRef>,
    /// Print what would be written and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NoteEditArgs {
    /// The note's id, from `sona --notes`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// The whole new body. The old one stays as a version.
    #[arg(long)]
    pub body: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NotePinArgs {
    /// The note's id, from `sona --notes`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// Take the pin off.
    #[arg(long)]
    pub unpin: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[command(group(ArgGroup::new("source").required(true).args(["meeting", "person", "other", "cycle"])))]
pub struct NoteLinkArgs {
    /// The note's id, from `sona --notes`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// A meeting's id.
    #[arg(long, value_name = "MEETING_ID")]
    pub meeting: Option<Uuid>,
    /// A person's id.
    #[arg(long, value_name = "PERSON_ID")]
    pub person: Option<Uuid>,
    /// Another note's id.
    #[arg(long = "note", value_name = "NOTE_ID")]
    pub other: Option<Uuid>,
    /// A cycle the note is a document of: current, next, or its number.
    #[arg(long, value_parser = cycle_ref, value_name = "CYCLE")]
    pub cycle: Option<WorkCycleRef>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

impl NoteLinkArgs {
    /// The source named; `None` when the link is to a cycle.
    fn source(&self) -> Option<ScratchSourceRef> {
        let (kind, id) = match (self.meeting, self.person, self.other) {
            (Some(id), _, _) => (ScratchSourceKind::Meeting, id),
            (None, Some(id), _) => (ScratchSourceKind::Person, id),
            (None, None, Some(id)) => (ScratchSourceKind::Note, id),
            (None, None, None) => return None,
        };
        Some(ScratchSourceRef {
            kind,
            id: id.to_string(),
        })
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NoteDeleteArgs {
    /// The note's id, from `sona --notes`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// Print what would be deleted and delete nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NoteRestoreArgs {
    /// The note's id, from `sona --notes`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// The version's id, from `sona --note-versions NOTE_ID`.
    #[arg(value_name = "VERSION_ID")]
    pub version: i64,
    /// Print the body it would put back and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct NotePassageArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The line's segment_id, from `sona --transcript MEETING --json`.
    #[arg(value_name = "LINE")]
    pub line: Uuid,
    /// The words to keep, as the line reads them, raw or corrected.
    #[arg(long, value_name = "WORDS")]
    pub excerpt: String,
    /// Link the person who said them too, by name or id.
    #[arg(long, value_name = "PERSON")]
    pub person: Option<String>,
    /// The note's text. Left out, the note says where the passage is.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// Print what would be written and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteAction {
    Created,
    Edited,
    Pinned,
    Unpinned,
    Linked,
    Unlinked,
    Deleted,
    Restored,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNote {
    pub id: String,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

fn project_note(note: ScratchNote) -> ExternalNote {
    ExternalNote {
        id: note.id,
        title: note.title,
        body: note.body,
        pinned: note.pinned,
        created_at_ms: note.created_at_ms,
        updated_at_ms: note.updated_at_ms,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteSource {
    pub kind: ScratchSourceKind,
    pub id: String,
    /// Absent on a dry run.
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteChange {
    pub schema_version: u32,
    pub action: NoteAction,
    pub dry_run: bool,
    /// Absent on a dry run that names none.
    pub operation_id: Option<String>,
    /// True when this answered from an earlier run with the same operation
    /// id, or the note already read this way.
    pub replayed: bool,
    /// The note as it is now. Absent on a dry run of a new note.
    pub note: Option<ExternalNote>,
    /// What a dry run of a new note would write.
    pub would_write: Option<String>,
    pub linked: Option<ExternalNoteSource>,
    /// The task, project or cycle the note was filed as a document of.
    pub document_of: Option<ScratchDocumentOf>,
}

/// What the ledger holds for an operation id: nothing yet, so `T` is what
/// the write made; the note an earlier run with the same request named; or
/// a different request under that id.
pub(super) enum Receipt<T> {
    New(T),
    Replay(String),
    Conflict,
}

pub(super) fn receipt_in(
    conn: &Connection,
    operation_id: &str,
    command: &str,
    request: &str,
) -> rusqlite::Result<Receipt<()>> {
    let found: Option<(String, String, String)> = conn
        .query_row(
            "SELECT command, request_json, response_json FROM planning_operations WHERE operation_id = ?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(match found {
        None => Receipt::New(()),
        Some((stored, asked, note_id)) if stored == command && asked == request => {
            Receipt::Replay(note_id)
        }
        Some(_) => Receipt::Conflict,
    })
}

pub(super) fn conflict(operation_id: &str) -> ExternalError {
    ExternalError::invalid(format!(
        "Operation id {operation_id} was already used for a different change. Pass a new --operation-id, or leave it out."
    ))
}

pub(super) fn request_json(request: &impl Serialize) -> Result<String, ExternalError> {
    serde_json::to_string(request).map_err(|_| {
        ExternalError::new(ExternalErrorCode::Failed, "The change could not be named.")
    })
}

impl Tracker<'_> {
    fn note(&self, id: &Uuid) -> Result<ScratchNote, ExternalError> {
        scratchpad::scratchpad_get(self.app.state(), id.to_string())
            .map_err(|message| refusal(self.history, message))?
            .ok_or_else(|| ExternalError::new(ExternalErrorCode::NotFound, format!("No note {id}.")))
    }

    fn replayed(&self, note_id: &str) -> Result<ScratchNote, ExternalError> {
        let id = Uuid::parse_str(note_id).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The earlier run names no note.")
        })?;
        self.note(&id)
    }

    pub(super) fn receipt(
        &self,
        operation_id: &str,
        command: &str,
        request: &str,
    ) -> Result<Receipt<()>, ExternalError> {
        planning::history(self.history, |conn| {
            Ok(receipt_in(conn, operation_id, command, request)?)
        })
        .map_err(|message| refusal(self.history, message))
    }

    /// Runs `write` and files its receipt, naming the note it returns, in
    /// one transaction that takes the write lock first, unless a run with
    /// the same operation id got there before: then that run's receipt
    /// answers and nothing is written.
    fn write_once<T>(
        &self,
        operation_id: &str,
        command: &str,
        request: &str,
        failure: &str,
        write: impl FnOnce(&Connection) -> Result<(String, T), ScratchpadError>,
    ) -> Result<Receipt<T>, ExternalError> {
        scratchpad::immediate(self.history, failure, |tx| {
            match receipt_in(tx, operation_id, command, request)? {
                Receipt::New(()) => {}
                Receipt::Replay(note_id) => return Ok(Receipt::Replay(note_id)),
                Receipt::Conflict => return Ok(Receipt::Conflict),
            }
            let (note_id, value) = write(tx)?;
            tx.execute(
                "INSERT INTO planning_operations (operation_id, command, request_json, response_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![operation_id, command, request, note_id, now_ms()],
            )?;
            Ok(Receipt::New(value))
        })
        .map_err(|message| refusal(self.history, message))
    }

    /// Runs `change` once per operation id: a second run with the same id
    /// and request answers with the note the first one wrote.
    fn once(
        &self,
        write: &WriteArgs,
        command: &str,
        request: &impl Serialize,
        failure: &str,
        change: impl FnOnce(&Connection) -> Result<ScratchNote, ScratchpadError>,
    ) -> Result<(String, ScratchNote, bool), ExternalError> {
        let operation_id = write.fresh_operation_id();
        let request = request_json(request)?;
        match self.receipt(&operation_id, command, &request)? {
            Receipt::New(()) => {}
            Receipt::Replay(note_id) => return Ok((operation_id, self.replayed(&note_id)?, true)),
            Receipt::Conflict => return Err(conflict(&operation_id)),
        }
        self.applies("writing notes")?;
        let written = self.write_once(&operation_id, command, &request, failure, |tx| {
            change(tx).map(|note| (note.id.clone(), note))
        })?;
        match written {
            Receipt::New(note) => {
                scratchpad::emit_changed(self.app, &note.id, false);
                Ok((operation_id, note, false))
            }
            Receipt::Replay(note_id) => Ok((operation_id, self.replayed(&note_id)?, true)),
            Receipt::Conflict => Err(conflict(&operation_id)),
        }
    }

    fn note_change(
        action: NoteAction,
        dry_run: bool,
        operation_id: Option<String>,
        note: Option<ScratchNote>,
        replayed: bool,
    ) -> WorkAnswer {
        WorkAnswer::Note(Box::new(ExternalNoteChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id,
            replayed,
            note: note.map(project_note),
            would_write: None,
            linked: None,
            document_of: None,
        }))
    }

    pub(super) async fn note_new(&self, args: &NoteNewArgs) -> Result<WorkAnswer, ExternalError> {
        let body = args.body.join(" ");
        let document = match args.cycle {
            Some(cycle) => Some(self.cycle_document(cycle)?.1),
            None => self.planning_document(args.task.as_deref(), args.project.as_deref()).await?,
        };
        if args.dry_run {
            let mut answer = Self::note_change(
                NoteAction::Created,
                true,
                args.write.operation_id.map(|id| id.to_string()),
                None,
                false,
            );
            if let WorkAnswer::Note(change) = &mut answer {
                change.would_write = Some(body);
                change.document_of = document;
            }
            return Ok(answer);
        }
        let request = match &document {
            None => serde_json::json!(body),
            Some(document) => serde_json::json!([body, document]),
        };
        let author = Author::Agent(args.write.agent.as_deref());
        let (operation_id, note, replayed) = self.once(
            &args.write,
            "cli_note_create",
            &request,
            "The note could not be saved.",
            |tx| scratchpad::create_note_in(tx, &body, author, now_ms()),
        )?;
        if let Some(document) = &document {
            self.file_document(&note.id, document, &operation_id).await?;
        }
        let mut answer = Self::note_change(
            NoteAction::Created,
            false,
            Some(operation_id),
            Some(note),
            replayed,
        );
        if let WorkAnswer::Note(change) = &mut answer {
            change.document_of = document;
        }
        Ok(answer)
    }

    /// Keeps a passage of a meeting's transcript as a new note linked to the
    /// meeting, the way the transcript's "Save passage" does.
    pub(super) async fn note_passage(
        &self,
        args: &NotePassageArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let (store, review) = meeting(self.meetings, &args.meeting).await?;
        let meeting_id = review.session.session_id.uuid();
        let title = review.session.title.clone();
        let linked = |title| ExternalNoteSource {
            kind: ScratchSourceKind::Meeting,
            id: meeting_id.to_string(),
            title,
        };
        let operation_id = args.write.fresh_operation_id();
        let request = request_json(&(
            meeting_id,
            args.line,
            &args.excerpt,
            &args.person,
            &args.body,
        ))?;
        let created = |note, replayed| {
            let mut answer = Self::note_change(
                NoteAction::Created,
                false,
                Some(operation_id.clone()),
                Some(note),
                replayed,
            );
            if let WorkAnswer::Note(change) = &mut answer {
                change.linked = Some(linked(Some(title.clone())));
            }
            answer
        };
        match self.receipt(&operation_id, "cli_note_passage", &request)? {
            Receipt::New(()) => {}
            Receipt::Replay(note_id) => return Ok(created(self.replayed(&note_id)?, true)),
            Receipt::Conflict => return Err(conflict(&operation_id)),
        }
        let line = review
            .transcript
            .iter()
            .find(|line| line.base.segment_id.uuid() == args.line && !line.removed)
            .ok_or_else(|| missing_line(args.line, &args.meeting))?;
        let corrected = line.replacement_text.as_deref().unwrap_or(&line.base.text);
        if !line.base.text.contains(&args.excerpt) && !corrected.contains(&args.excerpt) {
            return Err(ExternalError::invalid(format!(
                "Line {} does not say {:?}.",
                args.line, args.excerpt
            ))
            .hint(&format!(
                "Copy the words from: sona --transcript {} --json",
                args.meeting
            )));
        }
        let person = match &args.person {
            Some(value) => Some(self.person_named(value).await?.uuid().to_string()),
            None => None,
        };
        let passage = passages::passage(
            self.history,
            store,
            review,
            args.line.to_string(),
            &args.excerpt,
            person,
            args.body.clone().unwrap_or_default(),
        )
        .map_err(|message| refusal(self.history, message))?;
        if args.dry_run {
            let mut answer = Self::note_change(
                NoteAction::Created,
                true,
                args.write.operation_id.map(|id| id.to_string()),
                None,
                false,
            );
            if let WorkAnswer::Note(change) = &mut answer {
                change.would_write = Some(passage.body().to_owned());
                change.linked = Some(linked(None));
            }
            return Ok(answer);
        }
        self.applies("writing notes")?;
        let note = passage
            .save(
                self.app,
                self.history,
                Author::Agent(args.write.agent.as_deref()),
            )
            .map_err(|message| refusal(self.history, message))?;
        self.file_receipt(&operation_id, "cli_note_passage", &request, &note.id)?;
        Ok(created(note, false))
    }

    /// The task `--task` or the project `--project` names.
    async fn planning_document(
        &self,
        task: Option<&str>,
        project: Option<&str>,
    ) -> Result<Option<ScratchDocumentOf>, ExternalError> {
        if let Some(value) = task {
            let board = self.board().await?;
            let row = board.task(&task_ref(value).map_err(ExternalError::invalid)?)?;
            return Ok(Some(ScratchDocumentOf {
                kind: ScratchDocumentKind::Task,
                id: row.item.id.clone(),
                title: row.item.title.clone(),
            }));
        }
        let Some(value) = project else {
            return Ok(None);
        };
        let board = self.board().await?;
        let project = board
            .project(value)?
            .ok_or_else(|| ExternalError::invalid("Name the project the note is about."))?;
        Ok(Some(ScratchDocumentOf {
            kind: ScratchDocumentKind::Project,
            id: project.id.clone(),
            title: project.title.clone(),
        }))
    }

    /// The cycle `--cycle` names: its number, and how it reads as a document.
    fn cycle_document(
        &self,
        cycle: WorkCycleRef,
    ) -> Result<(i64, ScratchDocumentOf), ExternalError> {
        let listing = planning::work::cycles::work_cycles(self.app.clone())
            .map_err(|message| refusal(self.history, message))?;
        let number = named_cycle(&listing.cycles, cycle)?;
        let title = listing
            .cycles
            .iter()
            .find(|found| found.number == number)
            .and_then(|found| found.name.clone())
            .unwrap_or_else(|| format!("Cycle {number}"));
        Ok((
            number,
            ScratchDocumentOf {
                kind: ScratchDocumentKind::Cycle,
                id: number.to_string(),
                title,
            },
        ))
    }

    /// Files note `note_id` as a document of `document`. Filing it again
    /// changes nothing.
    async fn file_document(
        &self,
        note_id: &str,
        document: &ScratchDocumentOf,
        operation_id: &str,
    ) -> Result<(), ExternalError> {
        let target_kind = match document.kind {
            ScratchDocumentKind::Cycle => {
                let number = document.id.parse().map_err(|_| {
                    ExternalError::new(ExternalErrorCode::Failed, "The cycle has no number.")
                })?;
                scratchpad::immediate(
                    self.history,
                    "The note could not be filed under the cycle.",
                    |tx| documents::link_cycle_in(tx, note_id, number, now_ms()),
                )
                .map_err(|message| refusal(self.history, message))?;
                scratchpad::emit_changed(self.app, note_id, false);
                return Ok(());
            }
            ScratchDocumentKind::Project => EvidenceTargetKind::Project,
            ScratchDocumentKind::Milestone => EvidenceTargetKind::Milestone,
            ScratchDocumentKind::Task => EvidenceTargetKind::Task,
        };
        let link_operation =
            Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("{operation_id}/document").as_bytes());
        planning::planning_link_source(
            self.app.clone(),
            self.app.state(),
            self.app.state(),
            target_kind,
            document.id.clone(),
            EvidenceSourceKind::Note,
            note_id.to_string(),
            None,
            link_operation.to_string(),
        )
        .await
        .map_err(|message| refusal(self.history, message))?;
        scratchpad::emit_changed(self.app, note_id, false);
        Ok(())
    }

    /// `note-link --cycle` and `note-unlink --cycle`: files the note under
    /// a cycle or takes it off. Both settle on the same state when run twice.
    fn note_cycle(&self, args: &NoteLinkArgs, link: bool) -> Result<WorkAnswer, ExternalError> {
        let note = self.note(&args.note)?;
        let cycle = args
            .cycle
            .ok_or_else(|| ExternalError::invalid("Name a meeting, a person, a note or a cycle."))?;
        let (number, document) = self.cycle_document(cycle)?;
        let present = planning::history(self.history, |conn| {
            documents::documents_of(conn, &note.id, |_, _| false)
        })
        .map_err(|message| refusal(self.history, message))?
        .iter()
        .any(|found| found.kind == ScratchDocumentKind::Cycle && found.id == document.id);
        let settled = present == link;
        if !args.dry_run && !settled {
            self.applies("writing notes")?;
            scratchpad::immediate(self.history, "The note's cycle could not be changed.", |tx| {
                if link {
                    documents::link_cycle_in(tx, &note.id, number, now_ms())
                } else {
                    documents::unlink_cycle_in(tx, &note.id, number)
                }
            })
            .map_err(|message| refusal(self.history, message))?;
            scratchpad::emit_changed(self.app, &note.id, false);
        }
        let mut answer = Self::note_change(
            if link {
                NoteAction::Linked
            } else {
                NoteAction::Unlinked
            },
            args.dry_run,
            args.write.operation_id.map(|id| id.to_string()),
            Some(note),
            settled,
        );
        if let WorkAnswer::Note(change) = &mut answer {
            change.document_of = Some(document);
        }
        Ok(answer)
    }

    pub(super) fn note_edit(&self, args: &NoteEditArgs) -> Result<WorkAnswer, ExternalError> {
        let note = self.note(&args.note)?;
        let unchanged = note.body == args.body;
        if args.dry_run || unchanged {
            let mut shown = note;
            shown.body.clone_from(&args.body);
            return Ok(Self::note_change(
                NoteAction::Edited,
                args.dry_run,
                args.write.operation_id.map(|id| id.to_string()),
                Some(shown),
                unchanged,
            ));
        }
        let (operation_id, note, replayed) = self.once(
            &args.write,
            "cli_note_edit",
            &(&note.id, &args.body),
            "The note could not be saved.",
            |tx| {
                scratchpad::update_body_in(
                    tx,
                    &note.id,
                    &args.body,
                    Author::Agent(args.write.agent.as_deref()),
                    now_ms(),
                )
            },
        )?;
        Ok(Self::note_change(
            NoteAction::Edited,
            false,
            Some(operation_id),
            Some(note),
            replayed,
        ))
    }

    pub(super) fn note_pin(&self, args: &NotePinArgs) -> Result<WorkAnswer, ExternalError> {
        let note = self.note(&args.note)?;
        let pinned = !args.unpin;
        let action = if pinned {
            NoteAction::Pinned
        } else {
            NoteAction::Unpinned
        };
        let unchanged = note.pinned == pinned;
        if args.dry_run || unchanged {
            let mut shown = note;
            shown.pinned = pinned;
            return Ok(Self::note_change(
                action,
                args.dry_run,
                args.write.operation_id.map(|id| id.to_string()),
                Some(shown),
                unchanged,
            ));
        }
        let (operation_id, note, replayed) = self.once(
            &args.write,
            "cli_note_pin",
            &(&note.id, pinned),
            "The pin could not be changed.",
            |tx| scratchpad::set_pinned_with_connection(tx, &note.id, pinned),
        )?;
        Ok(Self::note_change(
            action,
            false,
            Some(operation_id),
            Some(note),
            replayed,
        ))
    }

    pub(super) async fn note_link(&self, args: &NoteLinkArgs) -> Result<WorkAnswer, ExternalError> {
        let note = self.note(&args.note)?;
        let Some(source) = args.source() else {
            return self.note_cycle(args, true);
        };
        if source.kind == ScratchSourceKind::Note {
            self.note(&args.other.unwrap_or_default())?;
        }
        let memory = links::scratchpad_memory(self.app.state(), self.app.state(), note.id.clone())
            .await
            .map_err(|message| refusal(self.history, message))?
            .ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::NotFound, format!("No note {}.", note.id))
            })?;
        let present = memory
            .sources
            .iter()
            .find(|found| found.kind == source.kind && found.id == source.id)
            .map(|found| found.title.clone());
        let mut linked = ExternalNoteSource {
            kind: source.kind,
            id: source.id.clone(),
            title: present.clone(),
        };
        if args.dry_run || present.is_some() {
            return Ok(WorkAnswer::Note(Box::new(ExternalNoteChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: NoteAction::Linked,
                dry_run: args.dry_run,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                replayed: present.is_some(),
                note: Some(project_note(note)),
                would_write: None,
                linked: Some(linked),
                document_of: None,
            })));
        }
        let operation_id = args.write.fresh_operation_id();
        let request = request_json(&(&note.id, &source))?;
        let mut replayed = match self.receipt(&operation_id, "cli_note_link", &request)? {
            Receipt::New(()) => false,
            Receipt::Replay(_) => true,
            Receipt::Conflict => return Err(conflict(&operation_id)),
        };
        let memory = if replayed {
            memory
        } else {
            self.applies("writing notes")?;
            let (resolved, store) =
                links::resolve_link(self.history, self.meetings, &note.id, &source)
                    .await
                    .map_err(|message| refusal(self.history, message))?;
            let written = self.write_once(
                &operation_id,
                "cli_note_link",
                &request,
                "The source could not be linked.",
                |tx| links::link_in(tx, &note.id, resolved).map(|()| (note.id.clone(), ())),
            )?;
            match written {
                Receipt::New(()) => {
                    scratchpad::emit_changed(self.app, &note.id, false);
                    links::read_memory(self.history, store.as_deref(), &note.id, |_, _| false)
                        .map_err(|message| refusal(self.history, message))?
                        .ok_or_else(|| {
                            ExternalError::new(
                                ExternalErrorCode::NotFound,
                                format!("No note {}.", note.id),
                            )
                        })?
                }
                Receipt::Replay(_) => {
                    replayed = true;
                    memory
                }
                Receipt::Conflict => return Err(conflict(&operation_id)),
            }
        };
        linked.title = memory
            .sources
            .iter()
            .find(|found| found.kind == source.kind && found.id == source.id)
            .map(|found| found.title.clone());
        Ok(WorkAnswer::Note(Box::new(ExternalNoteChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: NoteAction::Linked,
            dry_run: false,
            operation_id: Some(operation_id),
            replayed,
            note: Some(project_note(memory.note)),
            would_write: None,
            linked: Some(linked),
            document_of: None,
        })))
    }

    pub(super) fn file_receipt(
        &self,
        operation_id: &str,
        command: &str,
        request: &str,
        note_id: &str,
    ) -> Result<(), ExternalError> {
        planning::history(self.history, |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO planning_operations (operation_id, command, request_json, response_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![operation_id, command, request, note_id, now_ms()],
            )?;
            Ok(())
        })
        .map_err(|message| refusal(self.history, message))
    }

    pub(super) async fn note_delete(
        &self,
        args: &NoteDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let operation_id = args.write.operation_id("note-delete", &args.note)?;
        let request = request_json(&args.note)?;
        match self.receipt(&operation_id, "cli_note_delete", &request)? {
            Receipt::New(()) => {}
            Receipt::Replay(_) => {
                return Ok(Self::note_change(
                    NoteAction::Deleted,
                    false,
                    Some(operation_id),
                    None,
                    true,
                ))
            }
            Receipt::Conflict => return Err(conflict(&operation_id)),
        }
        let note = self.note(&args.note)?;
        if args.dry_run {
            return Ok(Self::note_change(
                NoteAction::Deleted,
                true,
                Some(operation_id),
                Some(note),
                false,
            ));
        }
        self.applies("deleting notes")?;
        scratchpad::scratchpad_delete(
            self.app.clone(),
            self.app.state(),
            self.app.state(),
            note.id.clone(),
        )
        .await
        .map_err(|message| refusal(self.history, message))?;
        self.file_receipt(&operation_id, "cli_note_delete", &request, &note.id)?;
        Ok(Self::note_change(
            NoteAction::Deleted,
            false,
            Some(operation_id),
            Some(note),
            false,
        ))
    }

    pub(super) fn note_restore(&self, args: &NoteRestoreArgs) -> Result<WorkAnswer, ExternalError> {
        let note = self.note(&args.note)?;
        let versions = scratchpad::scratchpad_versions(self.app.state(), note.id.clone())
            .map_err(|message| refusal(self.history, message))?;
        let body = versions
            .into_iter()
            .find(|version| version.id == args.version)
            .map(|version| version.body)
            .ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    format!("Note {} has no version {}.", note.id, args.version),
                )
                .hint("List them with: sona --note-versions NOTE_ID")
            })?;
        let same = note.body == body;
        if args.dry_run || same {
            return Ok(WorkAnswer::Note(Box::new(ExternalNoteChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: NoteAction::Restored,
                dry_run: args.dry_run,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                replayed: same,
                note: Some(project_note(note)),
                would_write: args.dry_run.then_some(body),
                linked: None,
                document_of: None,
            })));
        }
        let operation_id = args.write.fresh_operation_id();
        let request = request_json(&(&note.id, args.version))?;
        match self.receipt(&operation_id, "cli_note_restore", &request)? {
            Receipt::New(()) => {}
            Receipt::Replay(note_id) => {
                return Ok(Self::note_change(
                    NoteAction::Restored,
                    false,
                    Some(operation_id),
                    Some(self.replayed(&note_id)?),
                    true,
                ))
            }
            Receipt::Conflict => return Err(conflict(&operation_id)),
        }
        self.applies("writing notes")?;
        let restored = scratchpad::restore_version(
            self.app,
            self.history,
            &note.id,
            args.version,
            Author::Agent(args.write.agent.as_deref()),
        )
        .map_err(|message| refusal(self.history, message))?;
        self.file_receipt(&operation_id, "cli_note_restore", &request, &restored.id)?;
        Ok(Self::note_change(
            NoteAction::Restored,
            false,
            Some(operation_id),
            Some(restored),
            false,
        ))
    }

    pub(super) async fn note_unlink(
        &self,
        args: &NoteLinkArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let note = self.note(&args.note)?;
        let Some(source) = args.source() else {
            return self.note_cycle(args, false);
        };
        if source.kind == ScratchSourceKind::Meeting
            && Fence::read(self.meetings).await?.hides_meeting(&source.id)
        {
            return Err(ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No meeting {} is linked to this note.", source.id),
            ));
        }
        let memory = links::scratchpad_memory(self.app.state(), self.app.state(), note.id.clone())
            .await
            .map_err(|message| refusal(self.history, message))?
            .ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::NotFound, format!("No note {}.", note.id))
            })?;
        let present = memory
            .sources
            .iter()
            .find(|found| found.kind == source.kind && found.id == source.id)
            .map(|found| found.title.clone());
        let request = request_json(&(&note.id, &source))?;
        let unlinked = ExternalNoteSource {
            kind: source.kind,
            id: source.id.clone(),
            title: present.clone(),
        };
        let answer = |dry_run: bool,
                      operation_id: Option<String>,
                      note: ScratchNote,
                      replayed: bool,
                      unlinked: ExternalNoteSource| {
            WorkAnswer::Note(Box::new(ExternalNoteChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: NoteAction::Unlinked,
                dry_run,
                operation_id,
                replayed,
                note: Some(project_note(note)),
                would_write: None,
                linked: Some(unlinked),
                document_of: None,
            }))
        };
        if args.dry_run || present.is_none() {
            return Ok(answer(
                args.dry_run,
                args.write.operation_id.map(|id| id.to_string()),
                note,
                present.is_none(),
                unlinked,
            ));
        }
        let operation_id = args.write.fresh_operation_id();
        match self.receipt(&operation_id, "cli_note_unlink", &request)? {
            Receipt::New(()) => {}
            Receipt::Replay(_) => {
                return Ok(answer(false, Some(operation_id), note, true, unlinked))
            }
            Receipt::Conflict => return Err(conflict(&operation_id)),
        }
        self.applies("writing notes")?;
        links::scratchpad_unlink(
            self.app.clone(),
            self.app.state(),
            self.app.state(),
            note.id.clone(),
            source,
        )
        .await
        .map_err(|message| refusal(self.history, message))?;
        self.file_receipt(&operation_id, "cli_note_unlink", &request, &note.id)?;
        let now = self.note(&args.note)?;
        Ok(answer(false, Some(operation_id), now, false, unlinked))
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let WorkAnswer::Note(change) = answer else {
        return;
    };
    let verb = match (change.action, change.dry_run) {
        (NoteAction::Created, false) => "Made a note",
        (NoteAction::Created, true) => "Would make a note",
        (NoteAction::Edited, false) => "Changed the note",
        (NoteAction::Edited, true) => "Would change the note",
        (NoteAction::Pinned, false) => "Pinned the note",
        (NoteAction::Pinned, true) => "Would pin the note",
        (NoteAction::Unpinned, false) => "Unpinned the note",
        (NoteAction::Unpinned, true) => "Would unpin the note",
        (NoteAction::Linked, false) => "Linked to the note",
        (NoteAction::Linked, true) => "Would link to the note",
        (NoteAction::Unlinked, false) => "Took off the note",
        (NoteAction::Unlinked, true) => "Would take off the note",
        (NoteAction::Deleted, false) => "Deleted the note",
        (NoteAction::Deleted, true) => "Would delete the note",
        (NoteAction::Restored, false) => "Put the version back",
        (NoteAction::Restored, true) => "Would put this version back",
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
    if let Some(note) = &change.note {
        let _ = writeln!(
            text,
            "  {}{}  id {}",
            note.title,
            if note.pinned { " (pinned)" } else { "" },
            note.id
        );
    }
    if let Some(body) = &change.would_write {
        let _ = writeln!(text, "  {body}");
    }
    if let Some(linked) = &change.linked {
        let _ = writeln!(
            text,
            "  {} {}  {}",
            linked.kind.as_str(),
            linked.title.as_deref().unwrap_or(""),
            linked.id
        );
    }
    if let Some(document) = &change.document_of {
        let kind = match document.kind {
            ScratchDocumentKind::Project => "project",
            ScratchDocumentKind::Milestone => "milestone",
            ScratchDocumentKind::Task => "task",
            ScratchDocumentKind::Cycle => "cycle",
        };
        let _ = writeln!(text, "  about {kind} {}  {}", document.title, document.id);
    }
}
